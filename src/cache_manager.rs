//! RAM cache (LRU, memory-budgeted) and background neighbourhood preloader.

use crate::config::CACHE_FREE_RAM_RATIO;
use crate::image_loader::{DecodedImage, load_image};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use sysinfo::System;

fn compute_budget() -> usize {
    let mut sys = System::new();
    sys.refresh_memory();
    let avail = sys.available_memory() as usize;
    let budget = ((avail as f64) * CACHE_FREE_RAM_RATIO) as usize;
    budget.max(256 * 1024 * 1024)
}

/// Least-recently-used cache of decoded images, bounded by a memory budget.
pub struct RamCache {
    entries: HashMap<String, Arc<DecodedImage>>,
    /// Front = least recently used, back = most recently used.
    order: VecDeque<String>,
    used_bytes: usize,
    budget_bytes: usize,
}

impl RamCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            used_bytes: 0,
            budget_bytes: compute_budget(),
        }
    }

    #[cfg(test)]
    fn with_budget(budget_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            used_bytes: 0,
            budget_bytes,
        }
    }

    fn touch(&mut self, path: &str) {
        if let Some(pos) = self.order.iter().position(|p| p == path)
            && let Some(k) = self.order.remove(pos)
        {
            self.order.push_back(k);
        }
    }

    /// Returns a cached image and marks it as recently used.
    pub fn get(&mut self, path: &str) -> Option<Arc<DecodedImage>> {
        let found = self.entries.get(path).cloned();
        if found.is_some() {
            self.touch(path);
        }
        found
    }

    /// Returns a cached image without changing its LRU position.
    pub fn peek(&self, path: &str) -> Option<Arc<DecodedImage>> {
        self.entries.get(path).cloned()
    }

    pub fn contains(&self, path: &str) -> bool {
        self.entries.contains_key(path)
    }

    pub fn remove(&mut self, path: &str) {
        if let Some(old) = self.entries.remove(path) {
            self.used_bytes = self.used_bytes.saturating_sub(old.memory_bytes());
            self.order.retain(|p| p != path);
        }
    }

    pub fn insert(&mut self, path: String, img: Arc<DecodedImage>) {
        let img_bytes = img.memory_bytes();
        self.remove(&path);
        if img_bytes > self.budget_bytes {
            return;
        }
        while self.used_bytes + img_bytes > self.budget_bytes {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.used_bytes = self.used_bytes.saturating_sub(removed.memory_bytes());
            }
        }
        self.used_bytes += img_bytes;
        self.order.push_back(path.clone());
        self.entries.insert(path, img);
    }

    /// Moves an entry to a new key (file renamed) without double-counting memory.
    pub fn rename_key(&mut self, old: &str, new: &str) {
        if old == new {
            return;
        }
        if let Some(img) = self.entries.remove(old) {
            self.used_bytes = self.used_bytes.saturating_sub(img.memory_bytes());
            self.order.retain(|p| p != old);
            let mut updated = (*img).clone();
            updated.path = new.to_string();
            self.insert(new.to_string(), Arc::new(updated));
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.used_bytes = 0;
        self.budget_bytes = compute_budget();
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        (self.entries.len(), self.used_bytes, self.budget_bytes)
    }
}

impl Default for RamCache {
    fn default() -> Self {
        Self::new()
    }
}

type Inflight = Arc<(Mutex<HashSet<String>>, Condvar)>;

/// Loads `path` through the cache. Concurrent requests for the same file wait for the
/// first decode instead of decoding the file twice.
fn load_dedup(
    cache: &Mutex<RamCache>,
    inflight: &Inflight,
    path: &str,
) -> Result<Arc<DecodedImage>, String> {
    let (lock, cvar) = &**inflight;
    loop {
        if let Some(c) = cache.lock().ok().and_then(|mut g| g.get(path)) {
            return Ok(c);
        }
        let mut set = lock.lock().map_err(|_| "cache lock poisoned".to_string())?;
        if set.contains(path) {
            // Another thread is decoding this file: wait for it, then retry the cache.
            let _unused = cvar
                .wait_timeout(set, Duration::from_millis(250))
                .map_err(|_| "cache lock poisoned".to_string())?;
            continue;
        }
        set.insert(path.to_string());
        break;
    }

    struct Done<'a>(&'a Inflight, String);
    impl Drop for Done<'_> {
        fn drop(&mut self) {
            let (lock, cvar) = self.0.as_ref();
            if let Ok(mut set) = lock.lock() {
                set.remove(&self.1);
            }
            cvar.notify_all();
        }
    }
    let _done = Done(inflight, path.to_string());

    let decoded = Arc::new(load_image(Path::new(path))?);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(path.to_string(), decoded.clone());
    }
    Ok(decoded)
}

struct PreloadJob {
    request: u64,
    queue: Vec<String>,
    on_cached: Box<dyn Fn() + Send + 'static>,
}

pub struct PreloadController {
    pub cache: Arc<Mutex<RamCache>>,
    pub generation: Arc<AtomicU64>,
    inflight: Inflight,
    jobs: Sender<PreloadJob>,
}

impl PreloadController {
    pub fn new() -> Self {
        let cache = Arc::new(Mutex::new(RamCache::new()));
        let generation = Arc::new(AtomicU64::new(0));
        let inflight: Inflight = Arc::new((Mutex::new(HashSet::new()), Condvar::new()));
        let (tx, rx) = channel::<PreloadJob>();
        {
            let cache = cache.clone();
            let generation = generation.clone();
            let inflight = inflight.clone();
            std::thread::Builder::new()
                .name("preloader".into())
                .spawn(move || preload_worker(rx, cache, generation, inflight))
                .expect("failed to start preloader thread");
        }
        Self {
            cache,
            generation,
            inflight,
            jobs: tx,
        }
    }

    pub fn loader(&self) -> PreloadLoader {
        PreloadLoader {
            cache: self.cache.clone(),
            inflight: self.inflight.clone(),
        }
    }

    pub fn cancel_and_next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Preloads immediate neighbours (±1 … ±6) on a single background thread. A newer request
    /// replaces any queued one, so fast navigation never piles up concurrent decodes.
    pub fn preload_neighborhood<F>(&self, current_idx: usize, files: &[String], on_cached: F)
    where
        F: Fn() + Send + 'static,
    {
        let request = self.cancel_and_next_generation();
        let total = files.len();
        if total == 0 {
            return;
        }
        let mut queue = Vec::new();
        for offset in 1..=6 {
            if current_idx + offset < total {
                queue.push(files[current_idx + offset].clone());
            }
            if current_idx >= offset {
                queue.push(files[current_idx - offset].clone());
            }
        }
        let _ = self.jobs.send(PreloadJob {
            request,
            queue,
            on_cached: Box::new(on_cached),
        });
    }
}

impl Default for PreloadController {
    fn default() -> Self {
        Self::new()
    }
}

/// Clonable, thread-safe handle that loads images through the shared cache.
#[derive(Clone)]
pub struct PreloadLoader {
    cache: Arc<Mutex<RamCache>>,
    inflight: Inflight,
}

impl PreloadLoader {
    pub fn get_or_load(&self, path: &str) -> Result<Arc<DecodedImage>, String> {
        load_dedup(&self.cache, &self.inflight, path)
    }
}

fn preload_worker(
    rx: Receiver<PreloadJob>,
    cache: Arc<Mutex<RamCache>>,
    generation: Arc<AtomicU64>,
    inflight: Inflight,
) {
    while let Ok(mut job) = rx.recv() {
        // Only the most recent request matters.
        while let Ok(newer) = rx.try_recv() {
            job = newer;
        }
        for path in &job.queue {
            if generation.load(Ordering::SeqCst) != job.request {
                break;
            }
            let cached = cache.lock().map(|g| g.contains(path)).unwrap_or(false);
            if !cached && load_dedup(&cache, &inflight, path).is_ok() {
                if generation.load(Ordering::SeqCst) != job.request {
                    break;
                }
                (job.on_cached)();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(path: &str, bytes: usize) -> Arc<DecodedImage> {
        Arc::new(DecodedImage {
            path: path.into(),
            width: 1,
            height: 1,
            rgb_pixels: vec![0; bytes],
            embedded_icc: None,
            embedded_icc_name: None,
            ignored_icc_note: None,
            format_name: String::new(),
            bit_depth: 8,
            is_raw: false,
            file_size: 0,
        })
    }

    #[test]
    fn lru_evicts_least_recently_used() {
        let unit = img("x", 1000).memory_bytes();
        let mut c = RamCache::with_budget(unit * 3);
        c.insert("a".into(), img("a", 1000));
        c.insert("b".into(), img("b", 1000));
        c.insert("c".into(), img("c", 1000));
        assert!(c.get("a").is_some()); // a becomes most recent
        c.insert("d".into(), img("d", 1000));
        assert!(c.contains("a"));
        assert!(!c.contains("b"), "b was least recently used");
        assert_eq!(c.stats().1, unit * 3);
    }

    #[test]
    fn rename_does_not_double_count() {
        let unit = img("x", 1000).memory_bytes();
        let mut c = RamCache::with_budget(unit * 10);
        c.insert("a".into(), img("a", 1000));
        c.rename_key("a", "z");
        assert!(!c.contains("a"));
        assert_eq!(c.peek("z").unwrap().path, "z");
        assert_eq!(c.stats(), (1, unit, unit * 10));
    }
}
