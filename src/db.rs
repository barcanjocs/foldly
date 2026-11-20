//! The SQLite database: a cache of the music folder's tracks and their tags, plus playlists.
//!
//! Tracks are stored by their path relative to the music root and are marked missing rather
//! than deleted when their file disappears, so playlist entries survive a file coming back.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS tracks (
        id          INTEGER PRIMARY KEY,
        rel_path    TEXT NOT NULL UNIQUE,
        size        INTEGER NOT NULL,
        mtime       INTEGER NOT NULL,
        title       TEXT,
        artist      TEXT,
        album       TEXT,
        track_no    INTEGER,
        disc_no     INTEGER,
        duration_ms INTEGER,
        missing     INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS playlists (
        id   INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE COLLATE NOCASE
    );
    CREATE TABLE IF NOT EXISTS playlist_tracks (
        id          INTEGER PRIMARY KEY,
        playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
        track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
        position    INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS playlist_tracks_order ON playlist_tracks(playlist_id, position);
";

#[derive(Clone)]
pub struct Track {
    pub id: i64,
    pub rel_path: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<Duration>,
    pub missing: bool,
    /// The playlist_tracks row this track came from, when listed inside a playlist.
    pub entry_id: Option<i64>,
}

impl Track {
    /// "Artist — Title", falling back to the file name when tags are missing.
    pub fn label(&self) -> String {
        let stem = Path::new(&self.rel_path).file_stem().map(|s| s.to_string_lossy().into_owned());
        let title = self.title.clone().or(stem).unwrap_or_else(|| self.rel_path.clone());
        match &self.artist {
            Some(artist) => format!("{artist} — {title}"),
            None => title,
        }
    }
}

pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub track_count: usize,
}

/// What the scanner learned about one file.
pub struct ScannedFile {
    pub rel_path: String,
    pub size: i64,
    pub mtime: i64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
    pub duration_ms: Option<i64>,
}

pub fn default_path() -> Result<PathBuf> {
    let dir = dirs::data_dir().context("no data directory on this system")?.join("foldly");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir.join("foldly.db"))
}

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).with_context(|| format!("open database {}", path.display()))?;
    // WAL lets the scanner thread write while the UI reads.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

// --- tracks ---------------------------------------------------------------------------------

const TRACK_COLUMNS: &str = "t.id, t.rel_path, t.title, t.artist, t.album, t.duration_ms, t.missing";

fn track_from_row(row: &rusqlite::Row, entry_id: Option<i64>) -> rusqlite::Result<Track> {
    Ok(Track {
        id: row.get(0)?,
        rel_path: row.get(1)?,
        title: row.get(2)?,
        artist: row.get(3)?,
        album: row.get(4)?,
        duration: row.get::<_, Option<i64>>(5)?.map(|ms| Duration::from_millis(ms as u64)),
        missing: row.get(6)?,
        entry_id,
    })
}

/// Every track whose file is present, ordered like a record shelf.
pub fn all_tracks(conn: &Connection) -> Result<Vec<Track>> {
    let sql = format!(
        "SELECT {TRACK_COLUMNS} FROM tracks t WHERE t.missing = 0
         ORDER BY coalesce(t.artist, '') COLLATE NOCASE, coalesce(t.album, '') COLLATE NOCASE,
                  coalesce(t.disc_no, 0), coalesce(t.track_no, 0), t.rel_path COLLATE NOCASE"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| track_from_row(r, None))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn track_count(conn: &Connection) -> Result<usize> {
    Ok(conn.query_row("SELECT count(*) FROM tracks WHERE missing = 0", [], |r| r.get::<_, i64>(0))? as usize)
}

/// (size, mtime) for every known file, so the scanner can skip unchanged ones.
pub fn file_stamps(conn: &Connection) -> Result<std::collections::HashMap<String, (i64, i64, bool)>> {
    let mut stmt = conn.prepare("SELECT rel_path, size, mtime, missing FROM tracks")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn upsert_track(conn: &Connection, f: &ScannedFile) -> Result<()> {
    conn.execute(
        "INSERT INTO tracks (rel_path, size, mtime, title, artist, album, track_no, disc_no, duration_ms, missing)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)
         ON CONFLICT(rel_path) DO UPDATE SET
             size = excluded.size, mtime = excluded.mtime, title = excluded.title, artist = excluded.artist,
             album = excluded.album, track_no = excluded.track_no, disc_no = excluded.disc_no,
             duration_ms = excluded.duration_ms, missing = 0",
        params![f.rel_path, f.size, f.mtime, f.title, f.artist, f.album, f.track_no, f.disc_no, f.duration_ms],
    )?;
    Ok(())
}

pub fn set_missing(conn: &Connection, rel_path: &str, missing: bool) -> Result<()> {
    conn.execute("UPDATE tracks SET missing = ?2 WHERE rel_path = ?1", params![rel_path, missing])?;
    Ok(())
}

// --- playlists ------------------------------------------------------------------------------

pub fn playlists(conn: &Connection) -> Result<Vec<Playlist>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.name, count(pt.id) FROM playlists p
         LEFT JOIN playlist_tracks pt ON pt.playlist_id = p.id
         GROUP BY p.id ORDER BY p.name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Playlist { id: r.get(0)?, name: r.get(1)?, track_count: r.get::<_, i64>(2)? as usize })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn playlist_tracks(conn: &Connection, playlist_id: i64) -> Result<Vec<Track>> {
    let sql = format!(
        "SELECT {TRACK_COLUMNS}, pt.id FROM playlist_tracks pt JOIN tracks t ON t.id = pt.track_id
         WHERE pt.playlist_id = ?1 ORDER BY pt.position, pt.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([playlist_id], |r| {
        let entry_id = r.get(7)?;
        track_from_row(r, Some(entry_id))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn create_playlist(conn: &Connection, name: &str) -> Result<i64> {
    conn.execute("INSERT INTO playlists (name) VALUES (?1)", [name])
        .with_context(|| format!("a playlist named \"{name}\" already exists"))?;
    Ok(conn.last_insert_rowid())
}

pub fn rename_playlist(conn: &Connection, id: i64, name: &str) -> Result<()> {
    conn.execute("UPDATE playlists SET name = ?2 WHERE id = ?1", params![id, name])
        .with_context(|| format!("a playlist named \"{name}\" already exists"))?;
    Ok(())
}

pub fn delete_playlist(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM playlists WHERE id = ?1", [id])?;
    Ok(())
}

pub fn add_to_playlist(conn: &Connection, playlist_id: i64, track_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO playlist_tracks (playlist_id, track_id, position)
         SELECT ?1, ?2, coalesce(max(position), -1) + 1 FROM playlist_tracks WHERE playlist_id = ?1",
        params![playlist_id, track_id],
    )?;
    Ok(())
}

/// Appends `track_ids` in order, in one transaction. With `skip_existing`, tracks already in
/// the playlist are left out. Returns how many were added.
pub fn add_tracks(conn: &Connection, playlist_id: i64, track_ids: &[i64], skip_existing: bool) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let mut added = 0;
    {
        let mut exists = tx.prepare("SELECT 1 FROM playlist_tracks WHERE playlist_id = ?1 AND track_id = ?2")?;
        for &track_id in track_ids {
            if skip_existing && exists.exists(params![playlist_id, track_id])? {
                continue;
            }
            add_to_playlist(&tx, playlist_id, track_id)?;
            added += 1;
        }
    }
    tx.commit()?;
    Ok(added)
}

pub fn remove_entry(conn: &Connection, entry_id: i64) -> Result<()> {
    conn.execute("DELETE FROM playlist_tracks WHERE id = ?1", [entry_id])?;
    Ok(())
}

/// Swaps the positions of two playlist entries (used to move a track up or down).
pub fn swap_entries(conn: &mut Connection, a: i64, b: i64) -> Result<()> {
    let tx = conn.transaction()?;
    let pos =
        |id: i64| tx.query_row("SELECT position FROM playlist_tracks WHERE id = ?1", [id], |r| r.get::<_, i64>(0));
    let (pa, pb) = (pos(a)?, pos(b)?);
    tx.execute("UPDATE playlist_tracks SET position = ?2 WHERE id = ?1", params![a, pb])?;
    tx.execute("UPDATE playlist_tracks SET position = ?2 WHERE id = ?1", params![b, pa])?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(conn: &Connection, rel_path: &str) -> i64 {
        let f = ScannedFile {
            rel_path: rel_path.into(),
            size: 1,
            mtime: 1,
            title: None,
            artist: None,
            album: None,
            track_no: None,
            disc_no: None,
            duration_ms: None,
        };
        upsert_track(conn, &f).unwrap();
        conn.query_row("SELECT id FROM tracks WHERE rel_path = ?1", [rel_path], |r| r.get(0)).unwrap()
    }

    fn order(conn: &Connection, playlist: i64) -> Vec<String> {
        playlist_tracks(conn, playlist).unwrap().into_iter().map(|t| t.rel_path).collect()
    }

    #[test]
    fn playlist_editing() {
        let mut conn = open(Path::new(":memory:")).unwrap();
        let (a, b, c) = (track(&conn, "a.mp3"), track(&conn, "b.mp3"), track(&conn, "c.mp3"));

        let p = create_playlist(&conn, "Road trip").unwrap();
        assert!(create_playlist(&conn, "road TRIP").is_err(), "names are unique ignoring case");
        for t in [a, b, c, a] {
            add_to_playlist(&conn, p, t).unwrap();
        }
        assert_eq!(order(&conn, p), ["a.mp3", "b.mp3", "c.mp3", "a.mp3"]);

        let entries: Vec<_> = playlist_tracks(&conn, p).unwrap().into_iter().map(|t| t.entry_id.unwrap()).collect();
        swap_entries(&mut conn, entries[0], entries[1]).unwrap();
        assert_eq!(order(&conn, p), ["b.mp3", "a.mp3", "c.mp3", "a.mp3"]);

        remove_entry(&conn, entries[3]).unwrap();
        assert_eq!(order(&conn, p), ["b.mp3", "a.mp3", "c.mp3"]);

        // Bulk add skips tracks already present (only b is new here); single add doesn't.
        let q = create_playlist(&conn, "Bulk").unwrap();
        add_to_playlist(&conn, q, a).unwrap();
        assert_eq!(add_tracks(&conn, q, &[a, b, a], true).unwrap(), 1);
        assert_eq!(order(&conn, q), ["a.mp3", "b.mp3"]);
        assert_eq!(add_tracks(&conn, q, &[a], false).unwrap(), 1);
        assert_eq!(order(&conn, q), ["a.mp3", "b.mp3", "a.mp3"]);
        delete_playlist(&conn, q).unwrap();

        rename_playlist(&conn, p, "Commute").unwrap();
        let lists = playlists(&conn).unwrap();
        assert_eq!((lists[0].name.as_str(), lists[0].track_count), ("Commute", 3));

        delete_playlist(&conn, p).unwrap();
        assert!(playlists(&conn).unwrap().is_empty());
        assert_eq!(track_count(&conn).unwrap(), 3, "deleting a playlist keeps the tracks");
    }
}
