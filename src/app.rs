//! Application state: where you are in the virtual folder tree, the play queue, what's playing,
//! and any prompt or picker that's open.
//!
//! The tree is: the top level holds "All Tracks" plus one folder per playlist, and each of
//! those holds tracks.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use anyhow::Result;
use nucleo_matcher::Utf32String;
use ratatui::layout::Rect;
use ratatui::widgets::ListState;
use rusqlite::Connection;

use crate::db::{self, Playlist, Track};
use crate::filter::Filter;
use crate::player::Player;
use crate::scan::{self, ScanEvent};
use crate::watch::Watch;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    Root,
    AllTracks,
    Playlist { id: i64, name: String },
}

pub enum Entry {
    Parent,
    Folder { location: Location, count: usize },
    Track(Track),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
    Off,
    /// Start the queue again after the last track.
    All,
    /// Replay the current track when it ends (n / p still move on).
    One,
}

impl Repeat {
    fn cycle(self) -> Self {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Repeat::Off => "off",
            Repeat::All => "all",
            Repeat::One => "one",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "all" => Repeat::All,
            "one" => Repeat::One,
            _ => Repeat::Off,
        }
    }
}

pub enum InputPurpose {
    /// Create a playlist, optionally adding a track to it straight away.
    NewPlaylist {
        then_add: Vec<i64>,
    },
    RenamePlaylist(i64),
}

pub enum Mode {
    Normal,
    /// Typing into the filter line; the list narrows as you type.
    Filter,
    Input {
        purpose: InputPurpose,
        text: String,
    },
    /// Choosing a playlist to add tracks to. Item 0 is "New playlist…". `bulk` means the
    /// tracks are a whole filtered view, so ones already in the playlist are skipped.
    PickPlaylist {
        track_ids: Vec<i64>,
        bulk: bool,
        playlists: Vec<Playlist>,
        state: ListState,
    },
    ConfirmDelete {
        playlist_id: i64,
        name: String,
    },
    /// The key reference, scrolled down `scroll` lines.
    Help {
        scroll: u16,
    },
}

pub struct App {
    pub root: PathBuf,
    db_path: PathBuf,
    conn: Connection,

    pub location: Location,
    entries: Vec<Entry>,
    /// The text the filter matches against, one per entry.
    haystacks: Vec<Utf32String>,
    fuzzy: Filter,
    /// Indices into `entries` that match the filter, in display order.
    pub visible: Vec<usize>,
    pub filter: String,
    pub list_state: ListState,
    pub mode: Mode,

    /// What plays, in play order (shuffled when shuffle is on).
    pub queue: Vec<Track>,
    pub queue_index: Option<usize>,
    /// The queue in its original order, restored when shuffle is turned off.
    unshuffled: Vec<Track>,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub now_playing: Option<Track>,
    pub player: Player,

    pub message: Option<String>,
    pub scan_status: Option<String>,
    scan_rx: Option<Receiver<ScanEvent>>,
    /// Watches the music folder; `None` if watching couldn't start (`u` still rescans).
    watch: Option<Watch>,
    /// Ask the main loop to repaint the whole screen, wiping anything another library may
    /// have written over it.
    pub needs_full_redraw: bool,
    /// The browser's share of the width, in percent; the queue gets the rest.
    pub browser_width: u16,
    /// Where the last frame put the two main panels and the divider between them, so mouse
    /// events can be hit-tested. Set by `ui::draw`.
    pub panels: Rect,
    pub divider_x: u16,
    dragging_divider: bool,
    pub should_quit: bool,
}

impl App {
    pub fn new(root: PathBuf, db_path: PathBuf, player: Player) -> Result<Self> {
        let conn = db::open(&db_path)?;
        let mut app = App {
            root,
            db_path,
            conn,
            location: Location::Root,
            entries: Vec::new(),
            haystacks: Vec::new(),
            fuzzy: Filter::new(),
            visible: Vec::new(),
            filter: String::new(),
            list_state: ListState::default(),
            mode: Mode::Normal,
            queue: Vec::new(),
            unshuffled: Vec::new(),
            shuffle: false,
            repeat: Repeat::Off,
            queue_index: None,
            now_playing: None,
            player,
            message: None,
            scan_status: None,
            scan_rx: None,
            watch: None,
            needs_full_redraw: false,
            browser_width: DEFAULT_BROWSER_WIDTH,
            panels: Rect::default(),
            divider_x: 0,
            dragging_divider: false,
            should_quit: false,
        };
        match Watch::start(&app.root) {
            Ok(watch) => app.watch = Some(watch),
            Err(err) => app.message = Some(format!("{err:#}; press u to rescan after adding music")),
        }
        app.shuffle = db::get_setting(&app.conn, SHUFFLE_SETTING)?.as_deref() == Some("1");
        app.repeat = Repeat::parse(db::get_setting(&app.conn, REPEAT_SETTING)?.as_deref().unwrap_or_default());
        if let Some(width) = db::get_setting(&app.conn, BROWSER_WIDTH_SETTING)?.and_then(|w| w.parse().ok()) {
            app.browser_width = clamp_browser_width(width);
        }
        app.reload()?;
        app.rescan();
        Ok(app)
    }

    // --- listing --------------------------------------------------------------------------

    pub fn entry(&self, visible_index: usize) -> Option<&Entry> {
        self.visible.get(visible_index).and_then(|&i| self.entries.get(i))
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.list_state.selected().and_then(|i| self.entry(i))
    }

    fn selected_track(&self) -> Option<&Track> {
        match self.selected() {
            Some(Entry::Track(t)) => Some(t),
            _ => None,
        }
    }

    fn load_entries(&self, location: &Location) -> Result<Vec<Entry>> {
        Ok(match location {
            Location::Root => {
                let mut entries =
                    vec![Entry::Folder { location: Location::AllTracks, count: db::track_count(&self.conn)? }];
                entries.extend(db::playlists(&self.conn)?.into_iter().map(|p| Entry::Folder {
                    location: Location::Playlist { id: p.id, name: p.name },
                    count: p.track_count,
                }));
                entries
            }
            Location::AllTracks => {
                let mut entries = vec![Entry::Parent];
                entries.extend(db::all_tracks(&self.conn)?.into_iter().map(Entry::Track));
                entries
            }
            Location::Playlist { id, .. } => {
                let mut entries = vec![Entry::Parent];
                entries.extend(db::playlist_tracks(&self.conn, *id)?.into_iter().map(Entry::Track));
                entries
            }
        })
    }

    /// Re-reads the current location, keeping the selection position where possible.
    pub fn reload(&mut self) -> Result<()> {
        let entries = self.load_entries(&self.location)?;
        self.set_entries(entries);
        self.apply_filter();
        Ok(())
    }

    fn navigate(&mut self, location: Location) -> Result<()> {
        let came_from = std::mem::replace(&mut self.location, location);
        self.filter.clear();
        let entries = self.load_entries(&self.location)?;
        self.set_entries(entries);
        self.apply_filter();
        // Going up selects the folder we left; going down selects the first track.
        let index = if self.location == Location::Root {
            self.visible
                .iter()
                .position(|&i| matches!(&self.entries[i], Entry::Folder { location, .. } if *location == came_from))
        } else {
            self.visible.iter().position(|&i| matches!(self.entries[i], Entry::Track(_)))
        };
        self.select(index.unwrap_or(0));
        Ok(())
    }

    fn set_entries(&mut self, entries: Vec<Entry>) {
        self.haystacks = entries
            .iter()
            .map(|e| match e {
                Entry::Parent => Utf32String::default(),
                Entry::Folder { location, .. } => location_name(location).into(),
                Entry::Track(t) => format!("{} {}", t.label(), t.album.as_deref().unwrap_or_default()).into(),
            })
            .collect();
        self.entries = entries;
    }

    fn apply_filter(&mut self) {
        if self.filter.trim().is_empty() {
            self.visible = (0..self.entries.len()).collect();
        } else {
            // `..` stays on top; in All Tracks the best matches come next, while playlists keep
            // their order so the list still reads (and reorders) as the playlist.
            let parent = matches!(self.entries.first(), Some(Entry::Parent));
            let offset = usize::from(parent);
            let rank = self.location == Location::AllTracks;
            let matched = self.fuzzy.matches(&self.filter, &self.haystacks[offset..], rank);
            self.visible = (0..offset).chain(matched.into_iter().map(|i| i + offset)).collect();
        }
        let index = self.list_state.selected().unwrap_or(0);
        self.select(index);
    }

    fn select(&mut self, index: usize) {
        if self.visible.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(index.min(self.visible.len() - 1)));
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let current = self.list_state.selected().unwrap_or(0) as isize;
        self.select((current + delta).max(0) as usize);
    }

    pub fn select_first(&mut self) {
        self.select(0);
    }

    pub fn select_last(&mut self) {
        self.select(usize::MAX);
    }

    // --- actions --------------------------------------------------------------------------

    pub fn activate_selected(&mut self) {
        let Some(index) = self.list_state.selected() else { return };
        let result = match self.entry(index) {
            Some(Entry::Parent) => self.navigate(Location::Root),
            Some(Entry::Folder { location, .. }) => self.navigate(location.clone()),
            Some(Entry::Track(t)) if t.missing => {
                Err(anyhow::anyhow!("{} is missing from the music folder", t.rel_path))
            }
            Some(Entry::Track(chosen)) => {
                // Queue everything playable in this (filtered) view, starting at the chosen track.
                let chosen = queue_key(chosen);
                self.unshuffled = self
                    .visible
                    .iter()
                    .filter_map(|&i| match &self.entries[i] {
                        Entry::Track(t) if !t.missing => Some(t.clone()),
                        _ => None,
                    })
                    .collect();
                let start = self.unshuffled.iter().position(|t| queue_key(t) == chosen).unwrap_or(0);
                if self.shuffle {
                    self.queue = shuffled_from(&self.unshuffled, start);
                    self.play_index(0);
                } else {
                    self.queue = self.unshuffled.clone();
                    self.play_index(start);
                }
                Ok(())
            }
            None => Ok(()),
        };
        self.report(result);
    }

    pub fn go_up(&mut self) {
        if self.location != Location::Root {
            let result = self.navigate(Location::Root);
            self.report(result);
        }
    }

    pub fn rescan(&mut self) {
        if self.scan_rx.is_none() {
            self.scan_status = Some("scanning…".into());
            self.scan_rx = Some(scan::spawn(self.db_path.clone(), self.root.clone()));
        }
    }

    /// Opens the playlist picker for the selected track.
    pub fn start_add_to_playlist(&mut self) {
        match self.selected_track().map(|t| t.id) {
            Some(id) => self.open_picker(vec![id], false),
            None => self.message = Some("select a track to add it to a playlist".into()),
        }
    }

    /// Opens the playlist picker for every track in the current (filtered) view.
    pub fn start_add_all_to_playlist(&mut self) {
        let ids: Vec<i64> = self
            .visible
            .iter()
            .filter_map(|&i| match &self.entries[i] {
                Entry::Track(t) => Some(t.id),
                _ => None,
            })
            .collect();
        if ids.is_empty() {
            self.message = Some("no tracks here to add".into());
        } else {
            self.open_picker(ids, true);
        }
    }

    fn open_picker(&mut self, track_ids: Vec<i64>, bulk: bool) {
        match db::playlists(&self.conn) {
            Ok(mut playlists) => {
                // Adding a playlist's tracks to itself would only duplicate them.
                if let Location::Playlist { id, .. } = self.location {
                    playlists.retain(|p| p.id != id);
                }
                let mut state = ListState::default();
                state.select(Some(if playlists.is_empty() { 0 } else { 1 }));
                self.mode = Mode::PickPlaylist { track_ids, bulk, playlists, state };
            }
            Err(err) => self.report(Err(err)),
        }
    }

    pub fn start_new_playlist(&mut self) {
        self.mode = Mode::Input { purpose: InputPurpose::NewPlaylist { then_add: Vec::new() }, text: String::new() };
    }

    /// Rename the selected playlist folder, or the playlist you're inside.
    pub fn start_rename(&mut self) {
        if let Some((id, name)) = self.target_playlist() {
            self.mode = Mode::Input { purpose: InputPurpose::RenamePlaylist(id), text: name };
        }
    }

    /// In a playlist, removes the selected track from it; at the top level, asks to delete
    /// the selected playlist.
    pub fn delete_selected(&mut self) {
        match (&self.location, self.selected()) {
            (Location::Playlist { .. }, Some(Entry::Track(t))) => {
                let result = t.entry_id.map_or(Ok(()), |e| db::remove_entry(&self.conn, e)).and_then(|_| self.reload());
                self.report(result);
            }
            (Location::Root, Some(Entry::Folder { location: Location::Playlist { id, name }, .. })) => {
                self.mode = Mode::ConfirmDelete { playlist_id: *id, name: name.clone() };
            }
            _ => {}
        }
    }

    /// Moves the selected playlist track up (-1) or down (+1).
    pub fn move_entry(&mut self, delta: isize) {
        if !matches!(self.location, Location::Playlist { .. }) {
            return;
        }
        if !self.filter.is_empty() {
            self.message = Some("clear the filter to reorder".into());
            return;
        }
        let Some(from) = self.list_state.selected() else { return };
        let to = from as isize + delta;
        let entry_at = |i: isize| match usize::try_from(i).ok().and_then(|i| self.entry(i)) {
            Some(Entry::Track(t)) => t.entry_id,
            _ => None,
        };
        if let (Some(a), Some(b)) = (entry_at(from as isize), entry_at(to)) {
            let result = db::swap_entries(&mut self.conn, a, b).and_then(|_| self.reload());
            self.report(result);
            self.select(to as usize);
        }
    }

    fn target_playlist(&self) -> Option<(i64, String)> {
        match (&self.location, self.selected()) {
            (Location::Playlist { id, name }, _) => Some((*id, name.clone())),
            (Location::Root, Some(Entry::Folder { location: Location::Playlist { id, name }, .. })) => {
                Some((*id, name.clone()))
            }
            _ => None,
        }
    }

    // --- prompts --------------------------------------------------------------------------

    pub fn confirm_pick(&mut self) {
        let Mode::PickPlaylist { track_ids, bulk, playlists, state } = std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            return;
        };
        match state.selected().unwrap_or(0) {
            0 => {
                self.mode =
                    Mode::Input { purpose: InputPurpose::NewPlaylist { then_add: track_ids }, text: String::new() }
            }
            i => {
                let (id, name) = (playlists[i - 1].id, playlists[i - 1].name.clone());
                let result = db::add_tracks(&self.conn, id, &track_ids, bulk);
                let reloaded = self.reload();
                match result.and_then(|added| reloaded.map(|_| added)) {
                    Ok(added) => self.message = Some(added_message(added, track_ids.len(), &name)),
                    Err(err) => self.report(Err(err)),
                }
            }
        }
    }

    pub fn submit_input(&mut self) {
        let Mode::Input { purpose, text } = std::mem::replace(&mut self.mode, Mode::Normal) else { return };
        let name = text.trim();
        if name.is_empty() {
            return;
        }
        let result = match purpose {
            InputPurpose::NewPlaylist { then_add } => db::create_playlist(&self.conn, name).and_then(|id| {
                let added = db::add_tracks(&self.conn, id, &then_add, false)?;
                if added > 0 {
                    self.message = Some(added_message(added, added, name));
                }
                Ok(())
            }),
            InputPurpose::RenamePlaylist(id) => db::rename_playlist(&self.conn, id, name).map(|_| {
                if let Location::Playlist { id: current, name: current_name } = &mut self.location
                    && *current == id
                {
                    *current_name = name.to_string();
                }
            }),
        };
        let result = result.and_then(|_| self.reload());
        self.report(result);
    }

    pub fn confirm_delete(&mut self) {
        let Mode::ConfirmDelete { playlist_id, .. } = std::mem::replace(&mut self.mode, Mode::Normal) else { return };
        let result = db::delete_playlist(&self.conn, playlist_id).and_then(|_| self.reload());
        self.report(result);
    }

    /// Updates the filter and selects the best match, so Enter plays it.
    pub fn set_filter(&mut self, filter: String) {
        self.filter = filter;
        self.apply_filter();
        let first_match = self.visible.iter().position(|&i| !matches!(self.entries[i], Entry::Parent));
        self.select(first_match.unwrap_or(0));
    }

    // --- playback -------------------------------------------------------------------------

    /// Plays queue[index], skipping forward past files that fail to decode.
    fn play_index(&mut self, mut index: usize) {
        while let Some(track) = self.queue.get(index).cloned() {
            match self.player.play(&self.root.join(&track.rel_path)) {
                Ok(decoder_duration) => {
                    let mut track = track;
                    track.duration = track.duration.or(decoder_duration);
                    self.now_playing = Some(track);
                    self.queue_index = Some(index);
                    self.message = None;
                    return;
                }
                Err(err) => {
                    self.message = Some(format!("{err:#}"));
                    index += 1;
                }
            }
        }
        self.stop();
    }

    /// Skips to the next track. Past the end of the queue, repeat-all starts it again
    /// (reshuffled when shuffle is on); otherwise playback stops.
    pub fn next(&mut self) {
        let Some(i) = self.queue_index else { return };
        if i + 1 < self.queue.len() || self.repeat != Repeat::All {
            self.play_index(i + 1);
        } else {
            if self.shuffle {
                self.queue = shuffled_from(&self.unshuffled, fastrand::usize(..self.unshuffled.len()));
            }
            self.play_index(0);
        }
    }

    pub fn previous(&mut self) {
        let Some(i) = self.queue_index else { return };
        // Like most players: restart the track unless we're near its start.
        if self.player.position() > Duration::from_secs(3) {
            self.play_index(i);
        } else if i > 0 {
            self.play_index(i - 1);
        } else if self.repeat == Repeat::All {
            self.play_index(self.queue.len() - 1);
        } else {
            self.play_index(0);
        }
    }

    // --- panel sizes ----------------------------------------------------------------------

    /// Widens (positive) or narrows (negative) the browser by `delta` percent and saves it.
    pub fn resize_browser(&mut self, delta: i16) {
        self.set_browser_width(self.browser_width.saturating_add_signed(delta), true);
    }

    fn set_browser_width(&mut self, width: u16, save: bool) {
        self.browser_width = clamp_browser_width(width);
        if save {
            let result = db::set_setting(&self.conn, BROWSER_WIDTH_SETTING, &self.browser_width.to_string());
            self.report(result);
        }
    }

    /// A left click: starts dragging if it landed on the divider between the panels.
    pub fn mouse_down(&mut self, column: u16, row: u16) {
        let on_divider = column + 1 >= self.divider_x && column <= self.divider_x;
        let in_panels = row >= self.panels.y && row < self.panels.y + self.panels.height;
        self.dragging_divider = on_divider && in_panels;
    }

    /// Moves the divider to follow the mouse while dragging. Saved when the button is released.
    pub fn mouse_drag(&mut self, column: u16) {
        if self.dragging_divider && self.panels.width > 0 {
            // The queue's left border follows the pointer, so grabbing it doesn't jump.
            let browser_columns = column.saturating_sub(self.panels.x);
            let width =
                (u32::from(browser_columns) * 100 + u32::from(self.panels.width) / 2) / u32::from(self.panels.width);
            self.set_browser_width(width as u16, false);
        }
    }

    pub fn mouse_up(&mut self) {
        if std::mem::take(&mut self.dragging_divider) {
            self.set_browser_width(self.browser_width, true);
        }
    }

    /// Cycles repeat: off → all → one → off.
    pub fn cycle_repeat(&mut self) {
        self.repeat = self.repeat.cycle();
        let result = db::set_setting(&self.conn, REPEAT_SETTING, self.repeat.as_str());
        self.report(result);
    }

    /// Turns shuffle on or off. The current track keeps playing either way: turning it on
    /// shuffles everything else to play after it, turning it off continues in the original
    /// order from the current track.
    pub fn toggle_shuffle(&mut self) {
        self.shuffle = !self.shuffle;
        let result = db::set_setting(&self.conn, SHUFFLE_SETTING, if self.shuffle { "1" } else { "0" });
        self.report(result);

        let Some(current) = self.queue_index.and_then(|i| self.queue.get(i)).map(queue_key) else { return };
        let position = self.unshuffled.iter().position(|t| queue_key(t) == current).unwrap_or(0);
        if self.shuffle {
            self.queue = shuffled_from(&self.unshuffled, position);
            self.queue_index = Some(0);
        } else {
            self.queue = self.unshuffled.clone();
            self.queue_index = Some(position);
        }
    }

    pub fn stop(&mut self) {
        self.player.stop();
        self.queue_index = None;
        self.now_playing = None;
    }

    /// Called every loop iteration: advances when a track ends, rescans once folder changes
    /// settle, and picks up scan progress.
    pub fn tick(&mut self) {
        if let Some(message) = self.player.handle_errors() {
            self.message = Some(message);
            self.needs_full_redraw = true;
        }
        self.poll_watch();
        if self.player.finished() {
            match (self.repeat, self.queue_index) {
                (Repeat::One, Some(i)) => self.play_index(i),
                _ => self.next(),
            }
        }
        self.poll_scan();
    }

    fn poll_watch(&mut self) {
        let Some(watch) = &mut self.watch else { return };
        let now = Instant::now();
        watch.poll(now);
        // Changes made during a scan stay pending until it finishes, then trigger another.
        if self.scan_rx.is_none() && watch.take_settled(now) {
            self.rescan();
        }
    }

    fn poll_scan(&mut self) {
        let Some(rx) = &self.scan_rx else { return };
        let mut done = false;
        let mut refresh = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                ScanEvent::Progress { done: n, total } => {
                    self.scan_status = Some(format!("scanning {n}/{total}"));
                    refresh = true;
                }
                ScanEvent::Finished { changed, missing } => {
                    self.scan_status = None;
                    if changed + missing > 0 {
                        self.message = Some(format!("scan: {changed} new or changed, {missing} missing"));
                    }
                    done = true;
                    refresh = true;
                }
                ScanEvent::Failed(err) => {
                    self.scan_status = None;
                    self.message = Some(err);
                    done = true;
                }
            }
        }
        if done {
            self.scan_rx = None;
        }
        if refresh {
            let result = self.reload();
            self.report(result);
        }
    }

    fn report(&mut self, result: Result<()>) {
        if let Err(err) = result {
            self.message = Some(format!("{err:#}"));
        }
    }
}

const SHUFFLE_SETTING: &str = "shuffle";
const REPEAT_SETTING: &str = "repeat";
const BROWSER_WIDTH_SETTING: &str = "browser_width";
const DEFAULT_BROWSER_WIDTH: u16 = 62;
/// Neither panel can be squeezed below a fifth of the width.
const BROWSER_WIDTH_RANGE: std::ops::RangeInclusive<u16> = 20..=80;

fn clamp_browser_width(width: u16) -> u16 {
    width.clamp(*BROWSER_WIDTH_RANGE.start(), *BROWSER_WIDTH_RANGE.end())
}

/// Identifies a queue item; the entry id tells apart repeats of a track within a playlist.
fn queue_key(t: &Track) -> (i64, Option<i64>) {
    (t.id, t.entry_id)
}

/// `tracks[first]` followed by all the other tracks in random order.
fn shuffled_from(tracks: &[Track], first: usize) -> Vec<Track> {
    let mut rest: Vec<Track> = tracks.iter().enumerate().filter(|&(i, _)| i != first).map(|(_, t)| t.clone()).collect();
    fastrand::shuffle(&mut rest);
    tracks.get(first).cloned().into_iter().chain(rest).collect()
}

fn added_message(added: usize, requested: usize, playlist: &str) -> String {
    let tracks = if added == 1 { "track" } else { "tracks" };
    match requested - added {
        0 => format!("added {added} {tracks} to {playlist}"),
        skipped => format!("added {added} {tracks} to {playlist} ({skipped} already there)"),
    }
}

pub fn location_name(location: &Location) -> &str {
    match location {
        Location::Root => "foldly",
        Location::AllTracks => "All Tracks",
        Location::Playlist { name, .. } => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: i64) -> Track {
        Track {
            id,
            rel_path: format!("{id}.mp3"),
            title: None,
            artist: None,
            album: None,
            duration: None,
            missing: false,
            entry_id: None,
        }
    }

    #[test]
    fn shuffled_from_keeps_the_chosen_track_first() {
        let tracks: Vec<Track> = (0..50).map(track).collect();
        for first in [0, 7, 49] {
            let queue = shuffled_from(&tracks, first);
            assert_eq!(queue[0].id, first as i64);
            let mut ids: Vec<i64> = queue.iter().map(|t| t.id).collect();
            assert_ne!(ids[1..], (0..50).filter(|&i| i != first as i64).collect::<Vec<_>>()[..], "should be shuffled");
            ids.sort();
            assert_eq!(ids, (0..50).collect::<Vec<_>>(), "same tracks, each once");
        }
    }

    /// Needs an audio device and the same fixtures as `playlists_as_virtual_folders`
    /// (copied, not modified); run with `FOLDLY_TEST_DIR=<dir> cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn picks_up_music_added_while_running() {
        let fixtures = PathBuf::from(std::env::var("FOLDLY_TEST_DIR").unwrap());
        let base = std::env::temp_dir().join(format!("foldly-e2e-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (root, outside) = (base.join("music"), base.join("outside"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(outside.join("New Album")).unwrap();
        for i in 1..=3 {
            std::fs::copy(fixtures.join(format!("tone{i}.mp3")), root.join(format!("tone{i}.mp3"))).unwrap();
        }
        let root = root.canonicalize().unwrap();
        let mut app = App::new(root.clone(), base.join("test.db"), Player::new().unwrap()).unwrap();
        let count = |app: &mut App, expected: usize| {
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            loop {
                app.tick();
                let n = db::track_count(&app.conn).unwrap();
                if (n == expected && app.scan_rx.is_none()) || std::time::Instant::now() > deadline {
                    return n;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        assert_eq!(count(&mut app, 3), 3);

        // A file dropped into the folder.
        std::fs::copy(fixtures.join("tone1.mp3"), root.join("dropped.mp3")).unwrap();
        assert_eq!(count(&mut app, 4), 4);

        // A whole album folder moved in: only the folder's rename is reported.
        std::fs::copy(fixtures.join("tone2.mp3"), outside.join("New Album/01.mp3")).unwrap();
        std::fs::copy(fixtures.join("tone3.mp3"), outside.join("New Album/02.mp3")).unwrap();
        std::fs::rename(outside.join("New Album"), root.join("New Album")).unwrap();
        assert_eq!(count(&mut app, 6), 6);

        // A deleted file becomes missing.
        std::fs::remove_file(root.join("dropped.mp3")).unwrap();
        assert_eq!(count(&mut app, 5), 5);

        drop(app);
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// Needs an audio device and a folder of three short tagged MP3s ("Tone 1".."Tone 3",
    /// artist "Test Artist"); run with `FOLDLY_TEST_DIR=<dir> cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn playlists_as_virtual_folders() {
        let root = PathBuf::from(std::env::var("FOLDLY_TEST_DIR").unwrap());
        let db_path = std::env::temp_dir().join(format!("foldly-e2e-{}.db", std::process::id()));
        let mut app = App::new(root, db_path.clone(), Player::new().unwrap()).unwrap();
        while app.scan_rx.is_some() {
            app.tick();
            std::thread::sleep(Duration::from_millis(20));
        }

        // Top level: All Tracks with 3 tracks, no playlists yet.
        assert!(matches!(app.entry(0), Some(Entry::Folder { location: Location::AllTracks, count: 3 })));
        assert_eq!(app.visible.len(), 1);

        // Into All Tracks, filter to Tone 3, add it to a new playlist.
        app.activate_selected();
        assert_eq!(app.location, Location::AllTracks);
        app.set_filter("tone 3".into());
        assert_eq!(app.visible.len(), 2, "parent + one match");
        app.select(1);
        app.start_add_to_playlist();
        app.confirm_pick(); // item 0: new playlist
        app.mode = Mode::Input {
            purpose: InputPurpose::NewPlaylist { then_add: vec![app.selected_track().unwrap().id] },
            text: "Mix".into(),
        };
        app.submit_input();
        app.set_filter(String::new());

        // Add Tone 1 too, via the picker's existing playlist row.
        app.select(1);
        app.start_add_to_playlist();
        if let Mode::PickPlaylist { state, .. } = &mut app.mode {
            state.select(Some(1));
        }
        app.confirm_pick();
        assert_eq!(app.message.as_deref(), Some("added 1 track to Mix"));

        // Bulk add: filter to the artist and add all results to the playlist. Tone 1 and 3
        // are already there, so only Tone 2 is added.
        app.set_filter("'test artist".into());
        app.start_add_all_to_playlist();
        match &mut app.mode {
            Mode::PickPlaylist { track_ids, state, .. } => {
                assert_eq!(track_ids.len(), 3);
                state.select(Some(1));
            }
            _ => panic!("picker should be open"),
        }
        app.confirm_pick();
        assert_eq!(app.message.as_deref(), Some("added 1 track to Mix (2 already there)"));
        app.set_filter(String::new());

        // Back up: the playlist is a folder, and going up reselects All Tracks.
        app.go_up();
        assert_eq!(app.list_state.selected(), Some(0));
        assert!(matches!(app.entry(1), Some(Entry::Folder { location: Location::Playlist { .. }, count: 3 })));

        // Open it, move Tone 1 above Tone 3, and play from the top.
        app.select(1);
        app.activate_selected();
        app.select(2);
        app.move_entry(-1);
        let titles: Vec<_> = (1..3)
            .filter_map(|i| match app.entry(i) {
                Some(Entry::Track(t)) => t.title.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(titles, ["Tone 1", "Tone 3"]);

        app.select(1);
        app.activate_selected();
        assert_eq!(app.now_playing.as_ref().unwrap().title.as_deref(), Some("Tone 1"));
        app.player.seek_by(2);
        for _ in 0..30 {
            app.tick();
            if app.queue_index == Some(1) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(app.now_playing.as_ref().unwrap().title.as_deref(), Some("Tone 3"));

        // Shuffle on mid-track: the current track stays and plays first; the queue holds the
        // same tracks. Off again: original playlist order, still on the current track.
        let ordered: Vec<_> = app.queue.iter().map(|t| t.title.clone()).collect();
        app.toggle_shuffle();
        assert_eq!(app.queue_index, Some(0));
        assert_eq!(app.queue[0].title.as_deref(), Some("Tone 3"));
        let mut shuffled: Vec<_> = app.queue.iter().map(|t| t.title.clone()).collect();
        shuffled.sort();
        let mut expected = ordered.clone();
        expected.sort();
        assert_eq!(shuffled, expected);
        app.toggle_shuffle();
        assert_eq!(app.queue.iter().map(|t| t.title.clone()).collect::<Vec<_>>(), ordered);
        assert_eq!(app.queue[app.queue_index.unwrap()].title.as_deref(), Some("Tone 3"));

        // Repeat one: when Tone 3 ends it plays again.
        app.cycle_repeat();
        app.cycle_repeat();
        assert_eq!(app.repeat, Repeat::One);
        let index = app.queue_index;
        app.player.seek_by(2);
        std::thread::sleep(Duration::from_millis(100));
        let mut replayed = false;
        for _ in 0..30 {
            let before = app.player.position();
            app.tick();
            if app.player.position() < before {
                replayed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(replayed, "track should restart");
        assert_eq!(app.queue_index, index);

        // Repeat all: next past the last track wraps to the first, previous wraps back.
        app.cycle_repeat();
        app.cycle_repeat();
        assert_eq!(app.repeat, Repeat::All);
        let last = app.queue.len() - 1;
        app.play_index(last);
        app.next();
        assert_eq!(app.queue_index, Some(0));
        app.previous();
        assert_eq!(app.queue_index, Some(last));

        // Repeat off: next past the end stops.
        app.cycle_repeat();
        app.cycle_repeat();
        assert_eq!(app.repeat, Repeat::Off);
        app.next();
        assert_eq!(app.queue_index, None);
        app.play_index(1);

        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 10)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        println!("{}", terminal.backend());

        // Audio errors: a rerouted stream is ignored; a dead device is reopened and the track
        // resumes at the same position, paused if it was paused.
        app.play_index(1);
        app.player.seek_by(1);
        std::thread::sleep(Duration::from_millis(200));
        app.message = None;
        app.player.inject_error(rodio::cpal::ErrorKind::DeviceChanged);
        app.tick();
        assert!(app.message.is_none() && !app.needs_full_redraw);
        let (index, before) = (app.queue_index, app.player.position());
        app.player.toggle_pause();
        app.player.inject_error(rodio::cpal::ErrorKind::DeviceNotAvailable);
        app.tick();
        assert_eq!(app.message.as_deref(), Some("audio output changed; reconnected to the default device"));
        assert!(app.needs_full_redraw);
        assert_eq!(app.queue_index, index, "same track");
        assert!(app.player.is_paused(), "still paused");
        let after = app.player.position();
        assert!(after + Duration::from_millis(300) >= before, "resumed at {after:?}, was {before:?}");
        app.player.toggle_pause();
        std::thread::sleep(Duration::from_millis(300));
        assert!(app.player.position() > after, "playing again on the new device");

        // Resizing: drag the divider (saved on release), use < and >, clamp, and reload.
        let render = |app: &mut App| {
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 10)).unwrap();
            terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
            format!("{}", terminal.backend())
        };
        render(&mut app);
        assert_eq!((app.browser_width, app.divider_x), (62, 62));
        app.mouse_down(30, 2);
        app.mouse_drag(50);
        assert_eq!(app.browser_width, 62, "a click away from the divider doesn't drag");
        app.mouse_down(62, 2);
        app.mouse_drag(40);
        assert_eq!(app.browser_width, 40);
        assert_eq!(db::get_setting(&app.conn, "browser_width").unwrap(), None, "not saved mid-drag");
        app.mouse_up();
        assert_eq!(db::get_setting(&app.conn, "browser_width").unwrap().as_deref(), Some("40"));
        render(&mut app);
        assert_eq!(app.divider_x, 40, "the queue now starts where it was dropped");
        app.resize_browser(2);
        assert_eq!(app.browser_width, 42);
        app.resize_browser(-100);
        assert_eq!(app.browser_width, 20, "clamped");
        app.resize_browser(10);
        let reopened = App::new(app.root.clone(), db_path.clone(), Player::new().unwrap()).unwrap();
        assert_eq!(reopened.browser_width, 30, "remembered");
        drop(reopened);
        println!("{}", render(&mut app));

        // Help fits a normal terminal; on a short one it scrolls, stopping at the last line.
        app.mode = Mode::Help { scroll: 0 };
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        println!("{}", terminal.backend());
        app.mode = Mode::Help { scroll: 999 };
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 12)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        println!("{}", terminal.backend());
        assert!(format!("{}", terminal.backend()).contains("Quit"));
        assert!(matches!(app.mode, Mode::Help { scroll } if scroll < 999));

        let _ = std::fs::remove_file(&db_path);
    }
}
