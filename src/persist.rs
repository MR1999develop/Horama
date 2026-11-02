//! Background writer for small files of the program (settings, ratings).
//!
//! The interface thread never touches the disk for these: it queues a job and goes on. One
//! thread does the writes in order; when several full rewrites of the same file are queued,
//! only the newest is written. Call [`flush`] before the program ends.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

enum Job {
    /// Replaces the whole file (temp file + rename, so a crash never leaves it truncated).
    Replace { path: PathBuf, bytes: Vec<u8> },
    /// Adds bytes at the end of the file, creating it if needed.
    Append { path: PathBuf, bytes: Vec<u8> },
    /// Answers when every job queued before it is done.
    Flush(Sender<()>),
}

fn queue() -> &'static Mutex<Sender<Job>> {
    static QUEUE: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = channel();
        let spawned = std::thread::Builder::new()
            .name("persist".into())
            .spawn(move || worker(rx));
        if let Err(e) = spawned {
            // Without the thread the jobs stay queued; flush() will report it.
            log::error!("Writer thread not started: {}", e);
        }
        Mutex::new(tx)
    })
}

fn send(job: Job) -> bool {
    let tx = queue().lock().unwrap_or_else(|e| e.into_inner());
    tx.send(job).is_ok()
}

/// Queues a full rewrite of `path`.
pub fn replace(path: PathBuf, bytes: Vec<u8>) {
    if !send(Job::Replace { path, bytes }) {
        log::error!("Writer thread gone: file not saved");
    }
}

/// Queues `bytes` to be added at the end of `path`.
pub fn append(path: PathBuf, bytes: Vec<u8>) {
    if !send(Job::Append { path, bytes }) {
        log::error!("Writer thread gone: file not updated");
    }
}

/// Waits until every queued write is done; false if `timeout` passed first.
pub fn flush(timeout: Duration) -> bool {
    let (tx, rx) = channel();
    send(Job::Flush(tx)) && rx.recv_timeout(timeout).is_ok()
}

fn worker(rx: Receiver<Job>) {
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        batch.extend(rx.try_iter());
        // Only the newest full rewrite of each file matters.
        let mut newest: HashSet<PathBuf> = HashSet::new();
        let mut keep = vec![true; batch.len()];
        for (i, job) in batch.iter().enumerate().rev() {
            if let Job::Replace { path, .. } = job
                && !newest.insert(path.clone())
            {
                keep[i] = false;
            }
        }
        for (job, keep) in batch.into_iter().zip(keep) {
            match job {
                Job::Replace { path, bytes } if keep => {
                    if let Err(e) = write_atomic(&path, &bytes) {
                        log::error!("{} not saved: {}", path.display(), e);
                    }
                }
                Job::Replace { .. } => {}
                Job::Append { path, bytes } => {
                    if let Err(e) = append_now(&path, &bytes) {
                        log::error!("{} not updated: {}", path.display(), e);
                    }
                }
                Job::Flush(done) => {
                    let _ = done.send(());
                }
            }
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = tmp_path(path);
    let written = fs::write(&tmp, bytes).and_then(|_| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// `settings.json` → `settings.json.tmp`.
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

fn append_now(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_happen_in_order_and_flush_waits_for_them() {
        let dir = std::env::temp_dir().join(format!("horama-persist-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let full = dir.join("a.json");
        let log = dir.join("b.log");
        for i in 0..20 {
            replace(full.clone(), format!("{}", i).into_bytes());
        }
        append(log.clone(), b"one\n".to_vec());
        append(log.clone(), b"two\n".to_vec());
        assert!(flush(Duration::from_secs(10)));
        assert_eq!(fs::read_to_string(&full).unwrap(), "19");
        assert_eq!(fs::read_to_string(&log).unwrap(), "one\ntwo\n");
        assert!(!tmp_path(&full).exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
