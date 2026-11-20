//! Watches the music folder and says when a rescan is due.
//!
//! The OS reports changes (FSEvents on macOS, inotify on Linux) through notify. Copying an
//! album produces a burst of events, so a rescan is only due once nothing relevant has
//! happened for `SETTLE`. The scanner is incremental, so that rescan only reads the files that
//! actually changed.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::event::ModifyKind;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::scan;

/// How long the folder must be quiet before rescanning.
pub const SETTLE: Duration = Duration::from_secs(2);

pub struct Watch {
    // Dropping the watcher stops watching.
    _watcher: RecommendedWatcher,
    rx: Receiver<notify::Result<Event>>,
    root: PathBuf,
    last_change: Option<Instant>,
}

impl Watch {
    pub fn start(root: &Path) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(tx).context("start watching the music folder")?;
        watcher.watch(root, RecursiveMode::Recursive).context("watch the music folder")?;
        Ok(Self { _watcher: watcher, rx, root: root.to_path_buf(), last_change: None })
    }

    /// Takes in pending events, noting when the last relevant one arrived.
    pub fn poll(&mut self, now: Instant) {
        while let Ok(event) = self.rx.try_recv() {
            // A watcher error (e.g. inotify's queue overflowing) may mean missed changes.
            if event.map_or(true, |e| is_relevant(&e, &self.root)) {
                self.last_change = Some(now);
            }
        }
    }

    /// True, once, when changes have been quiet for `SETTLE`.
    pub fn take_settled(&mut self, now: Instant) -> bool {
        match self.last_change {
            Some(t) if now.duration_since(t) >= SETTLE => {
                self.last_change = None;
                true
            }
            _ => false,
        }
    }
}

/// Whether an event could change what the scanner finds: audio files, and folders appearing,
/// disappearing or being renamed. Reads, hidden files and other files (cover art, notes) are
/// ignored.
fn is_relevant(event: &Event, root: &Path) -> bool {
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    event.paths.iter().any(|path| {
        if path == root || is_hidden(path, root) {
            return false;
        }
        if scan::is_audio(path) {
            return true;
        }
        if path.is_dir() {
            // Changes inside a folder are reported for the files themselves.
            return !matches!(event.kind, EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_)));
        }
        // Gone: could have been an audio file or a whole folder.
        !path.exists()
    })
}

fn is_hidden(path: &Path, root: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.components().any(|c| c.as_os_str().to_str().is_some_and(|s| s.starts_with('.')))
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, DataChange, RemoveKind};
    use std::fs;

    fn event(kind: EventKind, path: &Path) -> Event {
        Event::new(kind).add_path(path.to_path_buf())
    }

    #[test]
    fn relevance() {
        let root = std::env::temp_dir().join(format!("foldly-watch-rules-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Album")).unwrap();
        let create = EventKind::Create(CreateKind::Any);

        assert!(is_relevant(&event(create, &root.join("song.mp3")), &root));
        assert!(is_relevant(&event(create, &root.join("Album")), &root), "new folder");
        assert!(is_relevant(&event(EventKind::Remove(RemoveKind::Any), &root.join("Gone")), &root), "removed");

        fs::write(root.join("cover.jpg"), b"").unwrap();
        assert!(!is_relevant(&event(create, &root.join("cover.jpg")), &root), "non-audio file");
        assert!(!is_relevant(&event(create, &root.join(".hidden/song.mp3")), &root), "hidden");
        assert!(!is_relevant(&event(EventKind::Access(AccessKind::Any), &root.join("song.mp3")), &root), "read");
        assert!(!is_relevant(&event(create, &root), &root), "the root itself");
        let data = EventKind::Modify(ModifyKind::Data(DataChange::Any));
        assert!(!is_relevant(&event(data, &root.join("Album")), &root), "folder contents changed");

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn settles_once_after_quiet_period() {
        let root = std::env::temp_dir().join(format!("foldly-watch-settle-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        // Canonical, as main.rs passes it; macOS reports /private/var for /var.
        let root = root.canonicalize().unwrap();
        let mut watch = Watch::start(&root).unwrap();

        fs::write(root.join("new.mp3"), b"not really audio").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while watch.last_change.is_none() && Instant::now() < deadline {
            watch.poll(Instant::now());
            std::thread::sleep(Duration::from_millis(50));
        }
        let changed_at = watch.last_change.expect("the OS should report the new file");

        assert!(!watch.take_settled(changed_at + SETTLE / 2), "still settling");
        assert!(watch.take_settled(changed_at + SETTLE), "due after the quiet period");
        assert!(!watch.take_settled(changed_at + SETTLE * 2), "only once");

        drop(watch);
        fs::remove_dir_all(&root).unwrap();
    }
}
