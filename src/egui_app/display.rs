//! Monitor profile: when the window moves to another screen, the profile of that monitor is
//! read (off the interface thread) and, if it differs, the image, the thumbnails and the
//! comparison are made again for it.

use super::i18n::trf;
use super::thumbs::ThumbnailLoader;
use super::{HoramaApp, Msg};
use crate::color_manager::{detect_display_profile, DisplayProfile};
use std::sync::mpsc::channel;

/// The window centre must move at least this far (physical pixels) to look again.
const MOVE_THRESHOLD: f32 = 32.0;

impl HoramaApp {
    /// Called every frame: starts a monitor query when the window centre moved.
    pub(super) fn watch_display(&mut self, ctx: &egui::Context) {
        if self.display_probe_running {
            return;
        }
        let center = ctx.input(|i| {
            let v = i.viewport();
            let ppp = v.native_pixels_per_point.unwrap_or(1.0);
            v.outer_rect.map(|r| r.center().to_vec2() * ppp)
        });
        let Some(center) = center else { return };
        if let Some(last) = self.display_center
            && (center - last).length() < MOVE_THRESHOLD {
                return;
            }
        self.display_center = Some(center);
        self.display_probe_running = true;
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        let point = (center.x.round() as i32, center.y.round() as i32);
        let spawned = std::thread::Builder::new()
            .name("egui-display".into())
            .spawn(move || {
                let _ = tx.send(Msg::Display(detect_display_profile(Some(point))));
                ctx.request_repaint();
            });
        if spawned.is_err() {
            self.display_probe_running = false;
        }
    }

    /// Uses the monitor profile just read; everything on screen is made again if it changed.
    pub(super) fn display_detected(&mut self, profile: DisplayProfile) {
        self.display_probe_running = false;
        if !self.color.set_display_profile(profile) {
            return;
        }
        let name = self.color.display_profile_name().to_string();
        log::info!("Display profile changed: {}", name);
        self.notify(trf("Display profile: {}", &[&name]), false);
        self.restart_thumbnails();
        self.reload_current();
    }

    /// New thumbnail workers with the current color manager; the old ones stop when their
    /// channel closes.
    fn restart_thumbnails(&mut self) {
        let (tx, rx) = channel();
        let repaint = self.ctx.clone();
        self.thumb_loader =
            ThumbnailLoader::start(self.color.clone(), tx, move || repaint.request_repaint());
        self.thumb_rx = rx;
        self.thumb_folder_generation = 0;
        self.thumbs.clear();
        self.request_thumbs();
    }
}
