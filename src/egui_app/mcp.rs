//! MCP server (`horama --mcp`, or `HORAMA_MCP` set): an AI client
//! drives the viewer through JSON-RPC on stdin/stdout ([`crate::mcp_server`]).
//!
//! The server thread reads the requests; a relay thread passes each command to the interface
//! and wakes it, and the answer goes back through the command's own channel.

use super::HoramaApp;
use crate::browse;
use crate::config::{normalize_path, path_key, same_path};
use crate::mcp_server::{McpCommand, RatingFilter, Rotation, ZoomRequest, run_mcp_stdio_server};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::mpsc::{Receiver, channel};

/// Starts the server and returns where its commands arrive; `ctx` is woken for each one.
pub fn start(ctx: egui::Context) -> Receiver<McpCommand> {
    let (server_tx, server_rx) = crossbeam_channel::unbounded::<McpCommand>();
    let (tx, rx) = channel();
    let spawned = std::thread::Builder::new()
        .name("mcp-stdio".into())
        .spawn(move || run_mcp_stdio_server(server_tx))
        .and_then(|_| {
            std::thread::Builder::new()
                .name("mcp-relay".into())
                .spawn(move || {
                    while let Ok(cmd) = server_rx.recv() {
                        if tx.send(cmd).is_err() {
                            return;
                        }
                        ctx.request_repaint();
                    }
                })
        });
    if let Err(e) = spawned {
        log::error!("MCP server not started: {}", e);
    }
    rx
}

/// A folder opened by MCP answers when its scan is done.
pub(super) struct PendingOpen {
    pub generation: u64,
    pub folder: String,
    pub resp: crossbeam_channel::Sender<Result<String, String>>,
}

impl HoramaApp {
    /// Serves the MCP commands received since the last frame.
    pub(super) fn serve_mcp(&mut self) {
        let Some(rx) = &self.mcp else { return };
        let commands: Vec<McpCommand> = rx.try_iter().collect();
        for cmd in commands {
            self.mcp_command(cmd);
        }
    }

    /// Answers a pending "open folder" once its scan arrived.
    pub(super) fn mcp_folder_scanned(&mut self) {
        let done = self
            .mcp_open
            .as_ref()
            .is_some_and(|p| p.generation == self.scan_generation);
        if let (true, Some(p)) = (done, self.mcp_open.take()) {
            let _ = p.resp.send(Ok(format!(
                "Opened folder '{}' with {} visible images",
                p.folder,
                self.visible.len()
            )));
        }
    }

    fn mcp_command(&mut self, cmd: McpCommand) {
        match cmd {
            McpCommand::OpenFolder { path, resp } => {
                if !Path::new(&path).exists() {
                    let _ = resp.send(Err(format!("'{}' does not exist", path)));
                    return;
                }
                let before = self.scan_generation;
                self.open_path(&path);
                if self.scan_generation == before {
                    let _ = resp.send(Err(format!("'{}' is not a folder or an image", path)));
                    return;
                }
                self.mcp_open = Some(PendingOpen {
                    generation: self.scan_generation,
                    folder: path,
                    resp,
                });
            }
            McpCommand::SelectImage { filename, resp } => {
                let wanted = filename.trim();
                let low = wanted.to_lowercase();
                let suffix = format!("{}{}", std::path::MAIN_SEPARATOR, normalize_path(wanted))
                    .to_lowercase();
                let found = self
                    .visible
                    .iter()
                    .position(|f| browse::file_name_of(f).to_lowercase() == low)
                    .or_else(|| self.visible.iter().position(|f| same_path(f, wanted)))
                    .or_else(|| {
                        self.visible
                            .iter()
                            .position(|f| f.to_lowercase().ends_with(&suffix))
                    });
                let _ = resp.send(match found {
                    Some(i) => {
                        self.select_only(i);
                        Ok(format!("Selected image: {}", self.visible[i]))
                    }
                    None => Err(format!("Image '{}' not found in current folder", filename)),
                });
            }
            McpCommand::RotateImage { rotation, resp } => {
                let Some(path) = self.current_path() else {
                    let _ = resp.send(Err("No image selected".to_string()));
                    return;
                };
                let turns = match rotation {
                    Rotation::Clockwise => 1,
                    Rotation::HalfTurn => 2,
                    Rotation::CounterClockwise => 3,
                };
                let how = if crate::disk_ops::rotation_format_supported(Path::new(&path)) {
                    "the file is being updated losslessly"
                } else {
                    "shown on screen only (RAW and unsupported formats are never modified)"
                };
                if turns == 3 {
                    self.rotate(false);
                } else {
                    for _ in 0..turns {
                        self.rotate(true);
                    }
                }
                let _ = resp.send(Ok(format!("Image rotated {}°; {}", turns * 90, how)));
            }
            McpCommand::SetZoom { zoom, resp } => {
                if self.shown.is_none() {
                    let _ = resp.send(Err("No image loaded".to_string()));
                    return;
                }
                let _ = resp.send(Ok(match zoom {
                    ZoomRequest::Fit => {
                        self.view.fit = true;
                        self.view.pan = egui::Vec2::ZERO;
                        "Zoom set to fit window".to_string()
                    }
                    ZoomRequest::ActualSize => {
                        self.set_zoom(1.0);
                        self.view.pan = egui::Vec2::ZERO;
                        "Zoom set to 100%".to_string()
                    }
                    ZoomRequest::Factor(f) => {
                        self.set_zoom(f);
                        format!("Zoom set to {:.0}%", self.view.zoom * 100.0)
                    }
                }));
            }
            McpCommand::SetRating { stars, resp } => {
                let _ = resp.send(match self.current_path() {
                    Some(path) => {
                        let stars = stars.min(5);
                        let keep = std::mem::take(&mut self.selected);
                        self.set_rating(stars);
                        self.selected = keep;
                        Ok(format!("Rating set to {} stars for {}", stars, path))
                    }
                    None => Err("No active image to rate".to_string()),
                });
            }
            McpCommand::FilterImages {
                query,
                ratings,
                format,
                resp,
            } => {
                if let Some(q) = query {
                    self.filters.search = q;
                }
                // Bit k of the mask shows the images with k stars (0 = unrated); no bit = all.
                if let Some(r) = ratings {
                    self.filters.ratings_mask = match r {
                        RatingFilter::All => 0,
                        RatingFilter::UnratedOnly => 1,
                        RatingFilter::AtLeast(min) => (min..=5).fold(0u8, |m, k| m | (1 << k)),
                    };
                }
                if let Some(f) = format {
                    self.filters.format = f;
                }
                self.refilter();
                let _ = resp.send(Ok(format!(
                    "Filters applied: {} images matching criteria",
                    self.visible.len()
                )));
            }
            McpCommand::GetCurrentImageInfo { resp } => {
                let Some(path) = self.current_path() else {
                    let _ = resp.send(Err("No image currently loaded".to_string()));
                    return;
                };
                let shown = self.shown.as_ref().filter(|s| s.path == path);
                let (w, h) = shown.map(|s| s.size).unwrap_or((0, 0));
                let metadata: serde_json::Map<String, Value> = shown
                    .map(|s| {
                        s.metadata
                            .iter()
                            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                            .collect()
                    })
                    .unwrap_or_default();
                let _ = resp.send(Ok(json!({
                    "path": path,
                    "filename": browse::file_name_of(&path),
                    "width": w,
                    "height": h,
                    "loading": shown.is_none(),
                    "rating": self.settings.get_rating(&path),
                    "profile": self.profile_of(&path),
                    "unsaved_crop": self.edited.contains_key(&path_key(&path)),
                    "metadata": metadata,
                })));
            }
            McpCommand::ListImages { resp } => {
                let _ = resp.send(Ok(self.visible.clone()));
            }
        }
    }
}
