//! Filmstrip thumbnails, made by a few worker threads in priority order (closest to the
//! current image first). A new request replaces the queue, so scrolling or changing folder
//! never piles up work; results carry the request generation and old ones are dropped.

use crate::color_manager::ColorManager;
use crate::image_loader::load_thumbnail_rgba;
use egui::ColorImage;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};

/// Thumbnails are decoded by this many threads: enough to hide disk latency, few enough to
/// leave the CPU to the image on screen.
const WORKERS: usize = 3;
/// Thumbnails are made up to this many files away from the centre of interest.
pub const RADIUS: usize = 300;
/// Above this many textures (about 50 KB each) those beyond [`RADIUS`] are dropped.
pub const MAX_KEPT: usize = 2 * RADIUS + 100;
/// The centre moves when the filmstrip is scrolled more than this many cells away.
pub const RECENTER_AFTER: usize = 8;

/// A finished thumbnail (or `None` if the file could not be read).
pub struct ThumbResult {
    pub generation: u64,
    pub path: String,
    pub image: Option<ColorImage>,
}

#[derive(Default)]
struct Queue {
    generation: u64,
    paths: VecDeque<String>,
    stop: bool,
}

/// Owner of the thumbnail workers; dropping it stops them.
pub struct ThumbnailLoader {
    queue: Arc<(Mutex<Queue>, Condvar)>,
}

impl ThumbnailLoader {
    /// Starts the workers. Every result is sent on `tx`, then `on_result` is called (to ask
    /// for a repaint).
    pub fn start(
        color: ColorManager,
        tx: Sender<ThumbResult>,
        on_result: impl Fn() + Send + Sync + Clone + 'static,
    ) -> Self {
        let queue: Arc<(Mutex<Queue>, Condvar)> = Arc::default();
        for n in 0..WORKERS {
            let (queue, color, tx, on_result) =
                (queue.clone(), color.clone(), tx.clone(), on_result.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("egui-thumb-{}", n))
                .spawn(move || worker(&queue, &color, &tx, &on_result));
            if let Err(e) = spawned {
                log::error!("Thumbnail worker not started: {}", e);
            }
        }
        ThumbnailLoader { queue }
    }

    /// Replaces the pending work with `paths` (in priority order) and returns the generation
    /// that the results of this request will carry.
    pub fn request(&self, paths: Vec<String>) -> u64 {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.generation += 1;
        q.paths = paths.into();
        cvar.notify_all();
        q.generation
    }
}

impl Drop for ThumbnailLoader {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.queue;
        lock.lock().unwrap_or_else(|e| e.into_inner()).stop = true;
        cvar.notify_all();
    }
}

fn worker(
    queue: &(Mutex<Queue>, Condvar),
    color: &ColorManager,
    tx: &Sender<ThumbResult>,
    on_result: &impl Fn(),
) {
    let (lock, cvar) = queue;
    loop {
        let job = {
            let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            let mut q = cvar
                .wait_while(guard, |q| q.paths.is_empty() && !q.stop)
                .unwrap_or_else(|e| e.into_inner());
            if q.stop {
                return;
            }
            let generation = q.generation;
            q.paths.pop_front().map(|p| (generation, p))
        };
        let Some((generation, path)) = job else {
            continue;
        };
        let image = load_thumbnail_rgba(Path::new(&path), Some(color)).map(|(w, h, rgba)| {
            ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba)
        });
        if tx
            .send(ThumbResult {
                generation,
                path,
                image,
            })
            .is_err()
        {
            return; // the interface is gone
        }
        on_result();
    }
}

/// Files still without a thumbnail, at most `radius` away from `current`, closest first
/// (alternating after and before it, the next one first).
pub fn priority_order(
    visible: &[String],
    current: Option<usize>,
    radius: usize,
    has_thumb: impl Fn(&str) -> bool,
) -> Vec<String> {
    let n = visible.len();
    let cur = current.unwrap_or(0).min(n.saturating_sub(1));
    let mut out = Vec::new();
    for d in 0..n.min(radius.saturating_add(1)) {
        for i in [cur.checked_add(d), cur.checked_sub(d).filter(|_| d > 0)]
            .into_iter()
            .flatten()
        {
            if i < n && !has_thumb(&visible[i]) {
                out.push(visible[i].clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closest_files_come_first_and_done_ones_are_skipped() {
        let v: Vec<String> = (0..6).map(|i| i.to_string()).collect();
        let order = priority_order(&v, Some(2), 10, |p| p == "1");
        assert_eq!(order, ["2", "3", "4", "0", "5"]);
        assert_eq!(
            priority_order(&v, None, 10, |_| false),
            ["0", "1", "2", "3", "4", "5"]
        );
        assert!(priority_order(&[], Some(3), 10, |_| false).is_empty());
        // A stale index past the end starts from the last file.
        assert_eq!(priority_order(&v, Some(99), 10, |_| false)[0], "5");
        // Files beyond the radius wait until the centre gets closer.
        assert_eq!(priority_order(&v, Some(2), 1, |_| false), ["2", "3", "1"]);
    }
}
