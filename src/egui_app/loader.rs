//! Loading of the image on screen: one worker thread, and only the newest request counts.
//!
//! Pressing the arrow key ten times quickly decodes the image the user stops on, not the ten
//! in between: a request that arrives while the worker is busy replaces the waiting one.
//! Images already in the RAM cache (preloaded neighbours) come back at once.

use super::Msg;
use crate::cache_manager::PreloadLoader;
use crate::color_manager::ColorManager;
use crate::config::{DEFAULT_FALLBACK_PROFILE, PROFILE_EMBEDDED};
use crate::image_loader::DecodedImage;
use crate::metadata::{extract_metadata, render_histogram_rgba};
use egui::ColorImage;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};

#[derive(Default)]
struct Slot {
    /// The waiting request: (generation, path).
    job: Option<(u64, String)>,
    stop: bool,
}

/// What the info panel shows about an image, prepared on the loading thread (reading the
/// metadata touches the disk).
pub(super) struct ImageInfo {
    pub metadata: Vec<(String, String)>,
    pub histogram: ColorImage,
}

/// Owner of the loading thread; dropping it stops the thread.
pub(super) struct ImageLoader {
    slot: Arc<(Mutex<Slot>, Condvar)>,
}

impl ImageLoader {
    /// Starts the worker. Results go to `tx`, then `ctx` is asked to repaint.
    pub fn start(
        loader: PreloadLoader,
        color: ColorManager,
        tx: Sender<Msg>,
        ctx: egui::Context,
    ) -> Self {
        let slot: Arc<(Mutex<Slot>, Condvar)> = Arc::default();
        let worker_slot = slot.clone();
        let spawned = std::thread::Builder::new()
            .name("egui-load".into())
            .spawn(move || worker(&worker_slot, &loader, &color, &tx, &ctx));
        if let Err(e) = spawned {
            log::error!("Image loading thread not started: {}", e);
        }
        ImageLoader { slot }
    }

    /// Asks for `path`; replaces a request that has not started yet.
    pub fn request(&self, generation: u64, path: String) {
        let (lock, cvar) = &*self.slot;
        lock.lock().unwrap_or_else(|e| e.into_inner()).job = Some((generation, path));
        cvar.notify_one();
    }
}

impl Drop for ImageLoader {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.slot;
        lock.lock().unwrap_or_else(|e| e.into_inner()).stop = true;
        cvar.notify_one();
    }
}

fn worker(
    slot: &(Mutex<Slot>, Condvar),
    loader: &PreloadLoader,
    color: &ColorManager,
    tx: &Sender<Msg>,
    ctx: &egui::Context,
) {
    let (lock, cvar) = slot;
    loop {
        let (generation, path) = {
            let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            let mut s = cvar
                .wait_while(guard, |s| s.job.is_none() && !s.stop)
                .unwrap_or_else(|e| e.into_inner());
            if s.stop {
                return;
            }
            match s.job.take() {
                Some(job) => job,
                None => continue,
            }
        };
        let decoded = loader.get_or_load(&path);
        let msg = match decoded.and_then(|d| render_for_screen(&d, color).map(|r| (d, r))) {
            Ok((decoded, (image, details))) => Msg::Loaded {
                generation,
                path,
                image,
                details,
                info: image_info(&decoded),
            },
            Err(error) => Msg::Failed {
                generation,
                path,
                error,
            },
        };
        if tx.send(msg).is_err() {
            return; // the interface is gone
        }
        ctx.request_repaint();
    }
}

fn image_info(decoded: &DecodedImage) -> ImageInfo {
    let (w, h, rgba) = render_histogram_rgba(&decoded.rgb_pixels);
    ImageInfo {
        metadata: extract_metadata(decoded),
        histogram: ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba),
    }
}

/// Converts a decoded image to the monitor profile, as the Slint interface does. Returns the
/// screen pixels and a short description (format and profile).
fn render_for_screen(
    decoded: &DecodedImage,
    color: &ColorManager,
) -> Result<(ColorImage, String), String> {
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
