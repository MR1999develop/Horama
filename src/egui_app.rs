//! The egui interface, under construction next to the Slint one.
//!
//! First stage: open a folder (menu, command line or drag and drop), browse its images with
//! the same filtering and selection rules as the Slint interface ([`crate::browse`]), and show
//! the current image color-managed, with fit / 100% / zoom around the cursor / pan
//! ([`crate::view`]). Ratings, editing, metadata and the other tools come in later stages.
//!
//! Only `egui` is used here, not `eframe`: the `horama-egui` program puts [`HoramaApp`] in a
//! native window. Disk reads and decoding run on worker threads; their results come back
//! through a channel read at every frame, and stale results (an older folder or image) are
//! dropped by generation number.

use crate::browse::{self, ClickMods, FileFacts, Filters};
use crate::color_manager::ColorManager;
use crate::config::{
    is_supported_file, normalize_path, scan_folder_and_subfolders, AppSettings,
    DEFAULT_FALLBACK_PROFILE, PROFILE_EMBEDDED,
};
use crate::view;
use egui::{
    Align2, Color32, ColorImage, Key, KeyboardShortcut, Modifiers, Pos2, Rect, Sense,
    TextureHandle, TextureOptions, Vec2,
};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};

/// Neutral grey behind the image, the same as the Slint interface.
const CANVAS: Color32 = Color32::from_rgb(0x12, 0x12, 0x12);

/// Results of the worker threads.
enum Msg {
    Scanned {
        generation: u64,
        files: Vec<String>,
        select: Option<String>,
    },
    Loaded {
        generation: u64,
        path: String,
        image: ColorImage,
        details: String,
    },
    Failed {
        generation: u64,
        path: String,
        error: String,
    },
}

/// The image on screen.
struct Shown {
    path: String,
    texture: TextureHandle,
    size: (u32, u32),
    details: String,
}

/// Zoom and pan of the image view; `zoom` in screen pixels per image pixel (see [`view`]).
struct ViewState {
    zoom: f32,
    pan: Vec2,
    fit: bool,
}

pub struct HoramaApp {
    ctx: egui::Context,
    color: ColorManager,
    settings: AppSettings,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    scan_generation: u64,
    load_generation: u64,

    folder: Option<String>,
    all_files: Vec<String>,
    filters: Filters,
    visible: Vec<String>,
    selected: BTreeSet<usize>,
    anchor: Option<usize>,
    current: Option<usize>,

    shown: Option<Shown>,
    loading: Option<String>,
    view: ViewState,
    status: String,
}

/// What the filters know about a file in the egui interface (for now: ratings only).
struct Facts<'a>(&'a AppSettings);

impl FileFacts for Facts<'_> {
    fn rating(&self, path: &str) -> u8 {
        self.0.get_rating(path)
    }
    fn is_cropped(&self, _path: &str) -> bool {
        false
    }
    fn has_profile_override(&self, _path: &str) -> bool {
        false
    }
    fn aspect_ratio(&self, _path: &str) -> Option<f32> {
        None
    }
    fn color_profile(&self, _path: &str) -> (String, bool) {
        (DEFAULT_FALLBACK_PROFILE.to_string(), false)
    }
}

impl HoramaApp {
    /// Creates the interface; `ctx` is kept so worker threads can ask for a repaint.
    pub fn new(ctx: egui::Context) -> Self {
        ctx.set_visuals(egui::Visuals::dark());
        let (tx, rx) = channel();
        HoramaApp {
            ctx,
            color: ColorManager::new(),
            settings: AppSettings::load(),
            tx,
            rx,
            scan_generation: 0,
            load_generation: 0,
            folder: None,
            all_files: Vec::new(),
            filters: Filters::default(),
            visible: Vec::new(),
            selected: BTreeSet::new(),
            anchor: None,
            current: None,
            shown: None,
            loading: None,
            view: ViewState {
                zoom: 1.0,
                pan: Vec2::ZERO,
                fit: true,
            },
            status: "Open a folder to view images".to_string(),
        }
    }

    /// Opens a folder, or an image in its folder with that image selected.
    pub fn open_path(&mut self, path: &str) {
        let p = Path::new(path);
        if p.is_dir() {
            self.open_folder(&normalize_path(path), None);
        } else if p.is_file() && is_supported_file(p) {
            let file = normalize_path(path);
            let parent = p
                .parent()
                .map(|d| normalize_path(&d.to_string_lossy()))
                .unwrap_or_default();
            self.open_folder(&parent, Some(file));
        } else if p.is_file() {
            self.status = format!(
                "'{}' is not an image format Horama can open.",
                browse::file_name_of(path)
            );
        } else {
            self.status = format!("'{}' does not exist.", path);
        }
    }

    /// Opens the folder the user used last, if it still exists.
    pub fn open_last_folder(&mut self) {
        let last = self.settings.last_folder.clone();
        if !last.is_empty() && Path::new(&last).is_dir() {
            self.open_folder(&last, None);
        }
    }

    /// Scans `folder` (with subfolders) on a worker thread; `select` is shown when done.
    fn open_folder(&mut self, folder: &str, select: Option<String>) {
        self.scan_generation += 1;
        let generation = self.scan_generation;
        self.folder = Some(folder.to_string());
        self.filters.tree = browse::TREE_ALL.to_string();
        self.status = format!("Scanning {}…", folder);
        let (tx, ctx, folder) = (self.tx.clone(), self.ctx.clone(), folder.to_string());
        let spawned = std::thread::Builder::new()
            .name("egui-scan".into())
            .spawn(move || {
                let (files, _subfolders) = scan_folder_and_subfolders(&folder);
                let _ = tx.send(Msg::Scanned {
                    generation,
                    files,
                    select,
                });
                ctx.request_repaint();
            });
        if let Err(e) = spawned {
            self.status = format!("Could not scan the folder: {}", e);
        }
    }

    /// Recomputes the visible files, keeping the current file if it is still visible.
    fn refilter(&mut self) {
        let previous = self.current_path();
        let root = self.folder.clone().unwrap_or_default();
        self.visible = browse::filter_files(
            &self.all_files,
            &root,
            &self.filters,
            &self.settings.saved_aspect_ratios,
            &Facts(&self.settings),
        )
        .visible;
        self.selected.clear();
        self.anchor = None;
        self.current = None;
        if !self.visible.is_empty() {
            let i = browse::index_after_refilter(previous.as_deref(), &self.visible);
            self.select_only(i);
        }
    }

    fn current_path(&self) -> Option<String> {
        self.current.and_then(|i| self.visible.get(i).cloned())
    }

    fn select_only(&mut self, i: usize) {
        self.selected.clear();
        self.selected.insert(i);
        self.anchor = Some(i);
        self.show_index(i);
    }

    /// Makes item `i` the current image and starts loading it.
    fn show_index(&mut self, i: usize) {
        let Some(path) = self.visible.get(i).cloned() else {
            return;
        };
        self.current = Some(i);
        if !self.selected.contains(&i) {
            self.selected.clear();
            self.selected.insert(i);
            self.anchor = Some(i);
        }
        if self.shown.as_ref().map(|s| s.path == path).unwrap_or(false) {
            return;
        }
        self.load(path);
    }

    /// Decodes and color-manages `path` on a worker thread; the previous image stays on screen
    /// until the new one is ready.
    fn load(&mut self, path: String) {
        self.load_generation += 1;
        let generation = self.load_generation;
        self.loading = Some(path.clone());
        let (tx, ctx, color) = (self.tx.clone(), self.ctx.clone(), self.color.clone());
        let spawned = std::thread::Builder::new()
            .name("egui-load".into())
            .spawn(move || {
                let msg = match decode_for_screen(&path, &color) {
                    Ok((image, details)) => Msg::Loaded {
                        generation,
                        path,
                        image,
                        details,
                    },
                    Err(error) => Msg::Failed {
                        generation,
                        path,
                        error,
                    },
                };
                let _ = tx.send(msg);
                ctx.request_repaint();
            });
        if let Err(e) = spawned {
            self.loading = None;
            self.status = format!("Could not start loading: {}", e);
        }
    }

    /// Applies the results the workers sent since the last frame.
    fn receive(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Scanned {
                    generation,
                    files,
                    select,
                } if generation == self.scan_generation => {
                    self.all_files = files;
                    self.current = None;
                    self.refilter();
                    if let Some(file) = select {
                        match self
                            .visible
                            .iter()
                            .position(|f| crate::config::same_path(f, &file))
                        {
                            Some(i) => self.select_only(i),
                            None => {
                                self.status = format!(
                                    "'{}' was not found in its folder.",
                                    browse::file_name_of(&file)
                                )
                            }
                        }
                    }
                    if self.visible.is_empty() {
                        self.shown = None;
                        self.status = "No supported images in this folder.".to_string();
                    }
                }
                Msg::Loaded {
                    generation,
                    path,
                    image,
                    details,
                } if generation == self.load_generation => {
                    let size = (image.size[0] as u32, image.size[1] as u32);
                    let options = TextureOptions {
                        // Mipmaps: zoomed-out views are filtered instead of aliased.
                        mipmap_mode: Some(egui::TextureFilter::Linear),
                        ..TextureOptions::LINEAR
                    };
                    let texture = self.ctx.load_texture(path.clone(), image, options);
                    self.shown = Some(Shown {
                        path,
                        texture,
                        size,
                        details,
                    });
                    self.loading = None;
                    self.view.fit = true;
                    self.view.pan = Vec2::ZERO;
                }
                Msg::Failed {
                    generation,
                    path,
                    error,
                } if generation == self.load_generation => {
                    self.loading = None;
                    self.status =
                        format!("Cannot display {}: {}", browse::file_name_of(&path), error);
                }
                _ => {} // result of an older request
            }
        }
    }

    /// Draws the whole interface. Call once per frame with the window's root `Ui`.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.receive();
        self.handle_keys(ui);
        self.handle_dropped_files(ui);

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.label(self.status_text());
        });
        egui::Panel::left("files")
            .resizable(true)
            .default_size(300.0)
            .size_range(200.0..=600.0)
            .show(ui, |ui| self.file_list(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(CANVAS))
            .show(ui, |ui| self.image_view(ui));
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui
                    .add(egui::Button::new("Open Folder…").shortcut_text("Ctrl+O"))
                    .clicked()
                {
                    self.pick_folder();
                }
                ui.separator();
                if ui.button("Exit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("View", |ui| {
                let has_image = self.shown.is_some();
                if ui
                    .add_enabled(
                        has_image,
                        egui::Button::new("Fit to Window").shortcut_text("F"),
                    )
                    .clicked()
                {
                    self.view.fit = true;
                    self.view.pan = Vec2::ZERO;
                }
                if ui
                    .add_enabled(
                        has_image,
                        egui::Button::new("Actual Size (100%)").shortcut_text("1"),
                    )
                    .clicked()
                {
                    self.set_zoom(1.0);
                }
                if ui
                    .add_enabled(has_image, egui::Button::new("Zoom In").shortcut_text("+"))
                    .clicked()
                {
                    self.set_zoom(self.view.zoom * view::ZOOM_STEP);
                }
                if ui
                    .add_enabled(has_image, egui::Button::new("Zoom Out").shortcut_text("−"))
                    .clicked()
                {
                    self.set_zoom(self.view.zoom / view::ZOOM_STEP);
                }
            });
        });
    }

    fn pick_folder(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("Open Folder");
        if let Some(f) = &self.folder {
            dialog = dialog.set_directory(f);
        }
        if let Some(dir) = dialog.pick_folder() {
            self.open_folder(&normalize_path(&dir.to_string_lossy()), None);
        }
    }

    fn set_zoom(&mut self, zoom: f32) {
        self.view.zoom = view::clamp_zoom(zoom);
        self.view.fit = false;
    }

    fn handle_keys(&mut self, ui: &mut egui::Ui) {
        // Keys go to text fields when one has the focus (the search box).
        if ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        let open = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
        let (open, next, prev, fit, actual, zoom_in, zoom_out) = ui.input_mut(|i| {
            (
                i.consume_shortcut(&open),
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::F),
                i.key_pressed(Key::Num1),
                i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals),
                i.key_pressed(Key::Minus),
            )
        });
        if open {
            self.pick_folder();
        }
        if !self.visible.is_empty() {
            let last = self.visible.len() - 1;
            let cur = self.current.unwrap_or(0);
            if next && cur < last {
                self.select_only(cur + 1);
            }
            if prev && cur > 0 {
                self.select_only(cur - 1);
            }
        }
        if self.shown.is_some() {
            if fit {
                self.view.fit = true;
                self.view.pan = Vec2::ZERO;
            }
            if actual {
                self.set_zoom(1.0);
            }
            if zoom_in {
                self.set_zoom(self.view.zoom * view::ZOOM_STEP);
            }
            if zoom_out {
                self.set_zoom(self.view.zoom / view::ZOOM_STEP);
            }
        }
    }

    /// Files or folders dragged from the file manager: the first one is opened.
    fn handle_dropped_files(&mut self, ui: &egui::Ui) {
        let dropped = ui.input(|i| i.raw.dropped_files.clone());
        if let Some(first) = dropped.first() {
            let path = first.path().to_string_lossy().to_string();
            if !path.is_empty() {
                self.open_path(&path);
            }
        }
    }

    fn file_list(&mut self, ui: &mut egui::Ui) {
        match &self.folder {
            Some(f) => {
                ui.label(egui::RichText::new(browse::file_name_of(f)).strong())
                    .on_hover_text(f.as_str());
            }
            None => {
                ui.label("No folder open");
            }
        }
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.filters.search)
                .hint_text("Search file names")
                .desired_width(f32::INFINITY),
        );
        if search.changed() {
            self.refilter();
        }
        ui.label(format!("{} images", self.visible.len()));
        ui.separator();

        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        let len = self.visible.len();
        let mut clicked: Option<(usize, ClickMods)> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, row_height, len, |ui, rows| {
                for i in rows {
                    let name = browse::file_name_of(&self.visible[i]);
                    let selected = self.selected.contains(&i);
                    let r = ui.add_sized(
                        [ui.available_width(), row_height],
                        egui::Button::selectable(selected, name),
                    );
                    if r.clicked() {
                        let m = ui.input(|inp| inp.modifiers);
                        clicked = Some((
                            i,
                            ClickMods {
                                toggle: m.command,
                                range: m.shift,
                            },
                        ));
                    }
                }
            });
        // Actions are applied after the list is drawn, never while iterating it.
        if let Some((i, mods)) = clicked {
            let show = browse::apply_click(
                &mut self.selected,
                &mut self.anchor,
                self.current,
                i,
                len,
                mods,
            );
            if let Some(i) = show {
                self.show_index(i);
            }
        }
    }

    fn image_view(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let Some(shown) = &self.shown else {
            let text = if self.loading.is_some() {
                "Loading…"
            } else {
                "Open a folder (Ctrl+O) or drop an image here"
            };
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(14.0),
                Color32::from_gray(0x88),
            );
            return;
        };
        let scale = ui.ctx().pixels_per_point();
        let view_size = (rect.width(), rect.height());
        if self.view.fit {
            self.view.zoom =
                view::fit_zoom(view_size.0, view_size.1, shown.size.0, shown.size.1, scale);
        }

        // Wheel: zoom around the cursor. Drag: pan. Double click: fit.
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                if let Some(pos) = response.hover_pos() {
                    // One wheel notch (about 50 points) = one WHEEL_STEP.
                    let factor = view::WHEEL_STEP.powf(scroll / 50.0);
                    let cursor = (pos.x - rect.min.x, pos.y - rect.min.y);
                    let (zoom, pan) = view::zoom_at_cursor(
                        self.view.zoom,
                        (self.view.pan.x, self.view.pan.y),
                        factor,
                        cursor,
                        view_size,
                    );
                    self.view.zoom = zoom;
                    self.view.pan = Vec2::new(pan.0, pan.1);
                    self.view.fit = false;
                }
            }
        }
        if response.dragged() {
            self.view.pan += response.drag_delta();
            self.view.fit = false;
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        if response.double_clicked() {
            self.view.fit = true;
            self.view.pan = Vec2::ZERO;
        }

        let lz = view::logical_zoom(self.view.zoom, scale);
        let size = Vec2::new(shown.size.0 as f32 * lz, shown.size.1 as f32 * lz);
        let center = rect.center() + self.view.pan;
        let image_rect = Rect::from_center_size(center, size);
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        ui.painter_at(rect)
            .image(shown.texture.id(), image_rect, uv, Color32::WHITE);
    }

    fn status_text(&self) -> String {
        let Some(shown) = &self.shown else {
            return self.status.clone();
        };
        let position = match self.current {
            Some(i) => format!("[{}/{}] ", i + 1, self.visible.len()),
            None => String::new(),
        };
        let loading = match &self.loading {
            Some(p) => format!(" — loading {}…", browse::file_name_of(p)),
            None => String::new(),
        };
        format!(
            "{}{} — {}×{} px — {} — {:.0}%{}",
            position,
            browse::file_name_of(&shown.path),
            shown.size.0,
            shown.size.1,
            shown.details,
            self.view.zoom * 100.0,
            loading
        )
    }
}

/// Decodes `path` and converts it to the monitor profile, as the Slint interface does.
/// Returns the screen pixels and a short description (format and profile).
fn decode_for_screen(path: &str, color: &ColorManager) -> Result<(ColorImage, String), String> {
    let decoded = crate::image_loader::load_image(Path::new(path))?;
    let profile = if decoded.embedded_icc_name.is_some() {
        PROFILE_EMBEDDED
    } else {
        DEFAULT_FALLBACK_PROFILE
    };
    let rendered = color.render(
        &decoded.rgb_pixels,
        profile,
        decoded.embedded_icc.as_deref(),
    );
    let size = [decoded.width as usize, decoded.height as usize];
    if rendered.display.len() != size[0] * size[1] * 3 {
        return Err("unexpected size of the rendered pixels".to_string());
    }
    let image = ColorImage::from_rgb(size, &rendered.display);
    let profile_name = decoded
        .embedded_icc_name
        .clone()
        .unwrap_or_else(|| DEFAULT_FALLBACK_PROFILE.to_string());
    Ok((image, format!("{} — {}", decoded.format_name, profile_name)))
}
