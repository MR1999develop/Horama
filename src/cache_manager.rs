//! Multithreaded RAM cache and background thumbnail/image preloader.

use crate::config::CACHE_FREE_RAM_RATIO;
use crate::image_loader::{load_image, DecodedImage};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use sysinfo::System;

pub struct RamCache {
    entries: HashMap<String, Arc<DecodedImage>>,
    order: VecDeque<String>,
    used_bytes: usize,
    budget_bytes: usize,
}

impl RamCache {
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_memory();
        let avail = sys.available_memory() as usize;
        let budget = ((avail as f64) * CACHE_FREE_RAM_RATIO) as usize;
        let budget_bytes = budget.max(256 * 1024 * 1024);

        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            used_bytes: 0,
            budget_bytes,
        }
    }

    pub fn get(&self, path: &str) -> Option<Arc<DecodedImage>> {
        self.entries.get(path).cloned()
    }

    pub fn remove(&mut self, path: &str) {
        if let Some(old) = self.entries.remove(path) {
            self.used_bytes = self.used_bytes.saturating_sub(old.memory_bytes());
            self.order.retain(|p| p != path);
        }
    }

    pub fn insert(&mut self, path: String, img: Arc<DecodedImage>) {
        let img_bytes = img.memory_bytes();
        if img_bytes > self.budget_bytes {
            return;
        }
        if let Some(old) = self.entries.remove(&path) {
            self.used_bytes = self.used_bytes.saturating_sub(old.memory_bytes());
            self.order.retain(|p| p != &path);
        }
        while self.used_bytes + img_bytes > self.budget_bytes {
            if let Some(oldest) = self.order.pop_front() {
                if let Some(removed) = self.entries.remove(&oldest) {
                    self.used_bytes = self.used_bytes.saturating_sub(removed.memory_bytes());
                }
            } else {
                break;
            }
        }
        self.used_bytes += img_bytes;
        self.order.push_back(path.clone());
        self.entries.insert(path, img);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.used_bytes = 0;
        let mut sys = System::new();
        sys.refresh_memory();
        let avail = sys.available_memory() as usize;
        let budget = ((avail as f64) * CACHE_FREE_RAM_RATIO) as usize;
        self.budget_bytes = budget.max(256 * 1024 * 1024);
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        (self.entries.len(), self.used_bytes, self.budget_bytes)
    }
}

pub struct PreloadController {
    pub cache: Arc<Mutex<RamCache>>,
    pub generation: Arc<AtomicU64>,
}

impl PreloadController {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(Mutex::new(RamCache::new())),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn get_or_load(&self, path: &str) -> Result<Arc<DecodedImage>, String> {
        if let Ok(guard) = self.cache.lock() {
            if let Some(cached) = guard.get(path) {
                return Ok(cached);
            }
        }
        let decoded = Arc::new(load_image(Path::new(path))?);
        if let Ok(mut guard) = self.cache.lock() {
            guard.insert(path.to_string(), decoded.clone());
        }
        Ok(decoded)
    }

    pub fn cancel_and_next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Preloads immediate neighbors (current_idx +/- 1, 2, 3, 4, 5, 6) in a background thread.
    /// This gives 0ms latency when navigating forward and backward.
    pub fn preload_neighborhood<F>(&self, current_idx: usize, files: &[String], on_cached: F)
    where
        F: Fn() + Send + 'static,
    {
        let gen = self.cancel_and_next_generation();
        let gen_arc = self.generation.clone();
        let cache_arc = self.cache.clone();

        let total = files.len();
        if total == 0 {
            return;
        }

        let mut queue = Vec::new();
        // Priority sequence: +1, -1, +2, -2, +3, -3, +4, -4, +5, -5, +6, -6
        for offset in 1..=6 {
            if current_idx + offset < total {
                queue.push(files[current_idx + offset].clone());
            }
            if current_idx >= offset {
                queue.push(files[current_idx - offset].clone());
            }
        }

        std::thread::spawn(move || {
            for path in queue {
                if gen_arc.load(Ordering::SeqCst) != gen {
                    return;
                }
                let already_cached = cache_arc
                    .lock()
                    .map(|g| g.get(&path).is_some())
                    .unwrap_or(false);
                if !already_cached {
                    if let Ok(decoded) = load_image(Path::new(&path)) {
                        if gen_arc.load(Ordering::SeqCst) != gen {
                            return;
                        }
                        if let Ok(mut guard) = cache_arc.lock() {
                            guard.insert(path, Arc::new(decoded));
                        }
                        on_cached();
                    }
                }
            }
        });
    }
}
