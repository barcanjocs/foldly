//! Brings the database up to date with the music folder, on a background thread.
//!
//! Files whose size and modified time haven't changed are skipped, so rescans are cheap.
//! Files that disappeared are marked missing rather than removed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use lofty::prelude::*;
use walkdir::WalkDir;

use crate::db::{self, ScannedFile};

pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "ogg", "wav", "m4a"];

pub enum ScanEvent {
    Progress { done: usize, total: usize },
    Finished { changed: usize, missing: usize },
    Failed(String),
}

pub fn spawn(db_path: PathBuf, root: PathBuf) -> Receiver<ScanEvent> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        if let Err(err) = run(&db_path, &root, &tx) {
            let _ = tx.send(ScanEvent::Failed(format!("scan failed: {err:#}")));
        }
    });
    rx
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn run(db_path: &Path, root: &Path, tx: &Sender<ScanEvent>) -> Result<()> {
    let mut conn = db::open(db_path)?;

    // (relative path, size, mtime) for every audio file, skipping hidden files and folders.
    let mut files = Vec::new();
    let walker = WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_str().is_some_and(|n| n.starts_with('.')));
    for entry in walker.filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() || !is_audio(entry.path()) {
            continue;
        }
        // Paths that aren't valid UTF-8 can't round-trip through the database; skip them.
        let Some(rel) = entry.path().strip_prefix(root).ok().and_then(|p| p.to_str()) else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
        files.push((rel.to_string(), meta.len() as i64, mtime as i64));
    }

    let known = db::file_stamps(&conn)?;
    let total = files.len();
    let mut changed = 0;
    let mut seen = HashSet::with_capacity(total);

    // Commit in batches so the UI sees new tracks during a long first scan.
    for (batch_index, batch) in files.chunks(200).enumerate() {
        let tx_db = conn.transaction()?;
        for (rel, size, mtime) in batch {
            seen.insert(rel.as_str());
            match known.get(rel) {
                Some(&(s, m, missing)) if s == *size && m == *mtime => {
                    if missing {
                        db::set_missing(&tx_db, rel, false)?;
                    }
                }
                _ => {
                    db::upsert_track(&tx_db, &read_file(root, rel, *size, *mtime))?;
                    changed += 1;
                }
            }
        }
        tx_db.commit()?;
        let _ = tx.send(ScanEvent::Progress { done: (batch_index * 200 + batch.len()).min(total), total });
    }

    let mut missing = 0;
    let tx_db = conn.transaction()?;
    for (rel, &(_, _, was_missing)) in &known {
        if !was_missing && !seen.contains(rel.as_str()) {
            db::set_missing(&tx_db, rel, true)?;
            missing += 1;
        }
    }
    tx_db.commit()?;

    let _ = tx.send(ScanEvent::Finished { changed, missing });
    Ok(())
}

fn read_file(root: &Path, rel: &str, size: i64, mtime: i64) -> ScannedFile {
    let mut file = ScannedFile {
        rel_path: rel.to_string(),
        size,
        mtime,
        title: None,
        artist: None,
        album: None,
        track_no: None,
        disc_no: None,
        duration_ms: None,
    };
    let Ok(tagged) = lofty::read_from_path(root.join(rel)) else { return file };

    let duration = tagged.properties().duration();
    if !duration.is_zero() {
        file.duration_ms = Some(duration.as_millis() as i64);
    }
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        let clean = |s: Option<std::borrow::Cow<str>>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        file.title = clean(tag.title());
        file.artist = clean(tag.artist());
        file.album = clean(tag.album());
        file.track_no = tag.track();
        file.disc_no = tag.disk();
    }
    file
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("foldly-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn scan(db_path: &Path, root: &Path) -> (usize, usize) {
        let (tx, rx) = mpsc::channel();
        run(db_path, root, &tx).unwrap();
        rx.try_iter()
            .find_map(|e| match e {
                ScanEvent::Finished { changed, missing } => Some((changed, missing)),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn rescans_incrementally_and_keeps_playlist_entries_for_missing_files() {
        let root = temp_dir("scan");
        let db_path = root.join("test.db");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::create_dir_all(root.join(".hidden")).unwrap();
        for f in ["a.mp3", "sub/b.FLAC", ".hidden/c.mp3", ".d.mp3", "notes.txt"] {
            fs::write(root.join(f), b"not really audio").unwrap();
        }

        assert_eq!(scan(&db_path, &root), (2, 0));
        let conn = db::open(&db_path).unwrap();
        let paths: Vec<_> = db::all_tracks(&conn).unwrap().into_iter().map(|t| t.rel_path).collect();
        assert_eq!(paths, ["a.mp3", "sub/b.FLAC"]);

        // Unchanged files aren't re-read.
        assert_eq!(scan(&db_path, &root), (0, 0));

        let playlist = db::create_playlist(&conn, "Mix").unwrap();
        let a = db::all_tracks(&conn).unwrap()[0].id;
        db::add_to_playlist(&conn, playlist, a).unwrap();

        // A vanished file is hidden from All Tracks but stays in the playlist, marked missing.
        fs::rename(root.join("a.mp3"), root.join("a.bak")).unwrap();
        assert_eq!(scan(&db_path, &root), (0, 1));
        assert_eq!(db::track_count(&conn).unwrap(), 1);
        let entries = db::playlist_tracks(&conn, playlist).unwrap();
        assert!(entries.len() == 1 && entries[0].missing);

        // And it comes back when the file does.
        fs::rename(root.join("a.bak"), root.join("a.mp3")).unwrap();
        scan(&db_path, &root);
        assert!(!db::playlist_tracks(&conn, playlist).unwrap()[0].missing);

        fs::remove_dir_all(&root).unwrap();
    }
}
