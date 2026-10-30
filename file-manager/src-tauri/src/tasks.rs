//! Long file operations (copy, move, delete, compress, extract) as tasks the
//! window follows with a progress bar. Each counts what it has done against
//! a total measured first (bytes, the files of the trees it walks, and the
//! items it was given), names the entry it is on, and stops at the next
//! chunk or entry once cancelled. The window polls `Tasks::snapshots` and
//! cancels by the id it gave the task when it started it.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

/// The error a cancelled task ends with.
pub const CANCELLED: &str = "Cancelled.";

/// One running operation's progress, shared by the thread doing it and the window polling it.
#[derive(Default)]
pub struct Task {
    cancelled: AtomicBool,
    measured: AtomicBool,
    bytes_done: AtomicU64,
    bytes_total: AtomicU64,
    files_done: AtomicU64,
    files_total: AtomicU64,
    items_done: AtomicU64,
    items_total: AtomicU64,
    current: Mutex<Option<PathBuf>>,
}

/// What the window draws of a task: nothing known yet while `preparing`;
/// the bar follows the bytes, else the files, else the items.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Snapshot {
    pub preparing: bool,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
    pub items_done: u64,
    pub items_total: u64,
    /// The entry it is on.
    pub current: Option<String>,
}

impl Task {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// An error once cancelled: the operation stops where it is.
    pub fn check(&self) -> io::Result<()> {
        if self.is_cancelled() {
            Err(io::Error::new(io::ErrorKind::Interrupted, CANCELLED))
        } else {
            Ok(())
        }
    }

    /// `what` for an error, or `CANCELLED` when the error came from a cancel.
    pub fn explain(&self, e: io::Error, what: impl FnOnce(io::Error) -> String) -> String {
        if self.is_cancelled() {
            CANCELLED.into()
        } else {
            what(e)
        }
    }

    /// The totals are known: the bar can move.
    pub fn set_totals(&self, bytes: u64, files: u64, items: u64) {
        self.bytes_total.store(bytes, Ordering::SeqCst);
        self.files_total.store(files, Ordering::SeqCst);
        self.items_total.store(items, Ordering::SeqCst);
        self.measured.store(true, Ordering::SeqCst);
    }

    /// More to do than measured at first (a move that turned out to cross volumes).
    pub fn add_totals(&self, bytes: u64, files: u64) {
        self.bytes_total.fetch_add(bytes, Ordering::SeqCst);
        self.files_total.fetch_add(files, Ordering::SeqCst);
    }

    pub fn add_bytes(&self, n: u64) {
        self.bytes_done.fetch_add(n, Ordering::SeqCst);
    }

    pub fn file_done(&self) {
        self.files_done.fetch_add(1, Ordering::SeqCst);
    }

    pub fn item_done(&self) {
        self.items_done.fetch_add(1, Ordering::SeqCst);
    }

    /// The entry the task is on now.
    pub fn on(&self, path: &Path) {
        if let Ok(mut cur) = self.current.lock() {
            *cur = Some(path.to_path_buf());
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            preparing: !self.measured.load(Ordering::SeqCst),
            bytes_done: self.bytes_done.load(Ordering::SeqCst),
            bytes_total: self.bytes_total.load(Ordering::SeqCst),
            files_done: self.files_done.load(Ordering::SeqCst),
            files_total: self.files_total.load(Ordering::SeqCst),
            items_done: self.items_done.load(Ordering::SeqCst),
            items_total: self.items_total.load(Ordering::SeqCst),
            current: self.current.lock().ok().and_then(|c| c.as_ref().map(|p| p.display().to_string())),
        }
    }
}

/// The bytes (of regular files) and the entries (folders and links
/// included, each tree's top too) under `paths`, without following links.
/// What can't be read counts as nothing: the operation reports it later.
pub fn measure(paths: &[PathBuf], task: &Task) -> io::Result<(u64, u64)> {
    let mut bytes = 0;
    let mut files = 0;
    let mut stack: Vec<PathBuf> = paths.to_vec();
    while let Some(path) = stack.pop() {
        task.check()?;
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        files += 1;
        if meta.is_dir() {
            if let Ok(entries) = fs::read_dir(&path) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        } else if meta.is_file() {
            bytes += meta.len();
        }
    }
    Ok((bytes, files))
}

/// The tasks running now, by the id the window gave each.
#[derive(Default)]
pub struct Tasks {
    running: Mutex<HashMap<u64, Arc<Task>>>,
    /// Cancelled before their command got to start them.
    early: Mutex<HashSet<u64>>,
}

/// A task registered under its id for as long as this lives.
pub struct Running {
    tasks: Arc<Tasks>,
    id: u64,
    task: Arc<Task>,
}

impl Deref for Running {
    type Target = Task;
    fn deref(&self) -> &Task {
        &self.task
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Ok(mut map) = self.tasks.running.lock() {
            map.remove(&self.id);
        }
    }
}

impl Tasks {
    pub fn start(self: &Arc<Self>, id: u64) -> Running {
        let task = Arc::new(Task::default());
        if self.early.lock().is_ok_and(|mut early| early.remove(&id)) {
            task.cancel();
        }
        if let Ok(mut map) = self.running.lock() {
            map.insert(id, task.clone());
        }
        Running { tasks: self.clone(), id, task }
    }

    /// Cancels task `id`, also when its command has not started it yet.
    pub fn cancel(&self, id: u64) {
        match self.running.lock().ok().and_then(|map| map.get(&id).cloned()) {
            Some(task) => task.cancel(),
            None => {
                if let Ok(mut early) = self.early.lock() {
                    early.insert(id);
                }
            }
        }
    }

    /// Cancels task `id` only while it runs (a late cancel leaves no trace).
    pub fn cancel_if_running(&self, id: u64) {
        if let Some(task) = self.running.lock().ok().and_then(|map| map.get(&id).cloned()) {
            task.cancel();
        }
    }

    pub fn snapshots(&self) -> HashMap<u64, Snapshot> {
        self.running.lock().map(|map| map.iter().map(|(id, t)| (*id, t.snapshot())).collect()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_is_measured_without_following_links() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        fs::create_dir_all(d.join("a/b")).unwrap();
        fs::write(d.join("a/one"), [0u8; 10]).unwrap();
        fs::write(d.join("a/b/two"), [0u8; 32]).unwrap();
        fs::write(d.join("big"), [0u8; 100]).unwrap();
        std::os::unix::fs::symlink(d.join("big"), d.join("a/link")).unwrap();
        let (bytes, files) = measure(&[d.join("a")], &Task::default()).unwrap();
        assert_eq!(bytes, 42, "the link's target is not counted");
        assert_eq!(files, 5, "a, b, one, two and the link");
        let cancelled = Task::default();
        cancelled.cancel();
        assert!(measure(&[d.join("a")], &cancelled).is_err());
    }

    #[test]
    fn tasks_are_listed_while_they_run_and_cancelled_by_id() {
        let tasks = Arc::new(Tasks::default());
        let running = tasks.start(7);
        running.set_totals(100, 3, 1);
        running.add_bytes(40);
        running.file_done();
        running.on(Path::new("/x/y"));
        let snap = &tasks.snapshots()[&7];
        assert_eq!((snap.preparing, snap.bytes_done, snap.bytes_total, snap.files_done), (false, 40, 100, 1));
        assert_eq!(snap.current.as_deref(), Some("/x/y"));
        tasks.cancel(7);
        assert!(running.check().is_err());
        assert_eq!(running.explain(io::Error::other("disk full"), |e| e.to_string()), CANCELLED);
        drop(running);
        assert!(tasks.snapshots().is_empty(), "gone once it ends");
    }

    #[test]
    fn a_cancel_that_comes_first_still_counts() {
        let tasks = Arc::new(Tasks::default());
        tasks.cancel(3);
        assert!(tasks.start(3).is_cancelled());
        let four = tasks.start(4);
        assert!(!four.is_cancelled());
        assert!(tasks.snapshots()[&4].preparing, "nothing measured yet");
    }
}
