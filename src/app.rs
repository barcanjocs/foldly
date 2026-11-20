//! Application state: the sidebar of sources (All Tracks and the playlists), the songs of the
//! selected source, the play queue, what's playing, and any prompt or popup that's open.
//!
//! Two panes take keyboard focus: the sidebar and the songs list. The sidebar's selection is
//! always the source the songs list shows, so moving through the sidebar previews each one.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use anyhow::Result;
use nucleo_matcher::Utf32String;
use ratatui::layout::Rect;
use ratatui::widgets::ListState;
use rusqlite::Connection;

use crate::db::{self, Playlist, Track, TrackDetails};
use crate::filter::Filter;
use crate::player::Player;
use crate::scan::{self, ScanEvent};
use crate::watch::Watch;

/// A source of songs shown in the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    AllTracks,
    Playlist { id: i64, name: String },
}

impl Location {
    /// Same source, even if a playlist was renamed since.
    fn same_source(&self, other: &Location) -> bool {
        match (self, other) {
            (Location::AllTracks, Location::AllTracks) => true,
            (Location::Playlist { id: a, .. }, Location::Playlist { id: b, .. }) => a == b,
            _ => false,
        }
    }
}

pub struct SidebarItem {
    pub location: Location,
    pub count: usize,
}

/// The panes that take keyboard focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Sidebar,
    Songs,
}

/// The two draggable borders between the three columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Divider {
    SidebarSongs,
    SongsRight,
}

/// Where the last frame put each column, for mouse hit-testing. Set by `ui::draw`.
#[derive(Clone, Copy, Debug, Default)]
pub struct PaneRects {
    pub sidebar: Rect,
    pub songs: Rect,
    pub right: Rect,
}

/// What the info pane shows: the selected song, or a summary of the selected source when the
/// sidebar has focus (or the songs list is empty).
pub enum Details {
    Track {
        details: TrackDetails,
        /// Other playlists holding this track (not the one being shown).
        also_in: Vec<String>,
    },
    Source {
        tracks: usize,
        /// Sum of the known lengths; `unknown` tracks have none.
        duration: Duration,
        unknown: usize,
        missing: usize,
    },
}

/// What `details` was computed for; when this changes, it is recomputed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DetailsKey {
    focus: Pane,
    source: Location,
    track: Option<(i64, Option<i64>)>,
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
    /// Create a playlist, optionally adding tracks to it straight away.
    NewPlaylist {
        then_add: Vec<i64>,
    },
    RenamePlaylist(i64),
}

pub enum Mode {
    Normal,
    /// Typing into the filter line; the songs list narrows as you type.
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

    pub focus: Pane,
    /// All Tracks, then one item per playlist. Never empty.
    pub sidebar: Vec<SidebarItem>,
    pub sidebar_state: ListState,
    /// The source the songs list shows: always the sidebar's selection.
    pub location: Location,
    tracks: Vec<Track>,
    /// The text the filter matches against, one per track.
    haystacks: Vec<Utf32String>,
    fuzzy: Filter,
    /// Indices into `tracks` that match the filter, in display order.
    pub visible: Vec<usize>,
    pub filter: String,
    pub list_state: ListState,
    pub mode: Mode,
    /// The info pane's contents, kept up to date by `tick` (draw never queries the db).
    pub details: Option<Details>,
    details_key: Option<DetailsKey>,

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
    /// Column widths in percent of the screen; the songs list gets the rest.
    pub sidebar_width: u16,
    pub right_width: u16,
    pub panes: PaneRects,
    dragging: Option<Divider>,
    pub should_quit: bool,
}

impl App {
    pub fn new(root: PathBuf, db_path: PathBuf, player: Player) -> Result<Self> {
        let conn = db::open(&db_path)?;
        let mut app = App {
            root,
            db_path,
            conn,
            focus: Pane::Songs,
            sidebar: Vec::new(),
            sidebar_state: ListState::default(),
            location: Location::AllTracks,
            tracks: Vec::new(),
            haystacks: Vec::new(),
            fuzzy: Filter::new(),
            visible: Vec::new(),
            filter: String::new(),
            list_state: ListState::default(),
            mode: Mode::Normal,
            details: None,
            details_key: None,
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
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            right_width: DEFAULT_RIGHT_WIDTH,
            panes: PaneRects::default(),
            dragging: None,
            should_quit: false,
        };
        match Watch::start(&app.root) {
            Ok(watch) => app.watch = Some(watch),
            Err(err) => app.message = Some(format!("{err:#}; press u to rescan after adding music")),
        }
        app.shuffle = db::get_setting(&app.conn, SHUFFLE_SETTING)?.as_deref() == Some("1");
        app.repeat = Repeat::parse(db::get_setting(&app.conn, REPEAT_SETTING)?.as_deref().unwrap_or_default());
        let saved_width =
            |key| -> Result<Option<u16>> { Ok(db::get_setting(&app.conn, key)?.and_then(|w| w.parse().ok())) };
        let (sidebar, right) = (saved_width(SIDEBAR_WIDTH_SETTING)?, saved_width(RIGHT_WIDTH_SETTING)?);
        app.sidebar_width = sidebar.unwrap_or(DEFAULT_SIDEBAR_WIDTH);
        app.right_width = right.unwrap_or(DEFAULT_RIGHT_WIDTH);
        app.clamp_widths(Divider::SongsRight);
        app.reload()?;
        app.select(0);
        app.rescan();
        Ok(app)
    }

    // --- listing --------------------------------------------------------------------------

    pub fn track(&self, visible_index: usize) -> Option<&Track> {
        self.visible.get(visible_index).and_then(|&i| self.tracks.get(i))
    }

    pub fn selected_track(&self) -> Option<&Track> {
        self.list_state.selected().and_then(|i| self.track(i))
    }

    fn load_sidebar(&self) -> Result<Vec<SidebarItem>> {
        let mut items = vec![SidebarItem { location: Location::AllTracks, count: db::track_count(&self.conn)? }];
        items.extend(
            db::playlists(&self.conn)?
                .into_iter()
                .map(|p| SidebarItem { location: Location::Playlist { id: p.id, name: p.name }, count: p.track_count }),
        );
        Ok(items)
    }

    fn load_tracks(&self, location: &Location) -> Result<Vec<Track>> {
        match location {
            Location::AllTracks => db::all_tracks(&self.conn),
            Location::Playlist { id, .. } => db::playlist_tracks(&self.conn, *id),
        }
    }

    /// Re-reads the sidebar and the songs list, keeping both selections where possible. If
    /// the shown playlist was deleted, the source now at its position is shown instead.
    pub fn reload(&mut self) -> Result<()> {
        // Playlist membership, counts or tags may have changed.
        self.details_key = None;
        self.sidebar = self.load_sidebar()?;
        match self.sidebar.iter().position(|item| item.location.same_source(&self.location)) {
            Some(i) => {
                self.sidebar_state.select(Some(i));
                self.location = self.sidebar[i].location.clone(); // picks up renames
                self.reload_tracks()
            }
            None => {
                let i = self.sidebar_state.selected().unwrap_or(0).min(self.sidebar.len() - 1);
                self.show_source(i)
            }
        }
    }

    fn reload_tracks(&mut self) -> Result<()> {
        let tracks = self.load_tracks(&self.location)?;
        self.set_tracks(tracks);
        self.apply_filter();
        Ok(())
    }

    /// Shows the sidebar item at `index` in the songs list, from the top, unfiltered.
    fn show_source(&mut self, index: usize) -> Result<()> {
        self.sidebar_state.select(Some(index));
        self.location = self.sidebar[index].location.clone();
        self.filter.clear();
        self.reload_tracks()?;
        self.select(0);
        Ok(())
    }

    fn set_tracks(&mut self, tracks: Vec<Track>) {
        self.haystacks =
            tracks.iter().map(|t| format!("{} {}", t.label(), t.album.as_deref().unwrap_or_default()).into()).collect();
        self.tracks = tracks;
    }

    fn apply_filter(&mut self) {
        if self.filter.trim().is_empty() {
            self.visible = (0..self.tracks.len()).collect();
        } else {
            // In All Tracks the best matches come first; playlists keep their order so the
            // list still reads (and reorders) as the playlist.
            let rank = self.location == Location::AllTracks;
            self.visible = self.fuzzy.matches(&self.filter, &self.haystacks, rank);
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

    /// Moves the selection in the focused pane. In the sidebar that also changes the source.
    pub fn move_selection(&mut self, delta: isize) {
        self.move_in(self.focus, delta);
    }

    fn move_in(&mut self, pane: Pane, delta: isize) {
        match pane {
            Pane::Songs => {
                let current = self.list_state.selected().unwrap_or(0) as isize;
                self.select((current + delta).max(0) as usize);
            }
            Pane::Sidebar => {
                let current = self.sidebar_state.selected().unwrap_or(0);
                let last = self.sidebar.len() - 1;
                self.select_source((current as isize + delta).clamp(0, last as isize) as usize);
            }
        }
    }

    fn select_source(&mut self, index: usize) {
        if Some(index) != self.sidebar_state.selected() {
            let result = self.show_source(index);
            self.report(result);
        }
    }

    pub fn select_first(&mut self) {
        match self.focus {
            Pane::Songs => self.select(0),
            Pane::Sidebar => self.select_source(0),
        }
    }

    pub fn select_last(&mut self) {
        match self.focus {
            Pane::Songs => self.select(usize::MAX),
            Pane::Sidebar => self.select_source(self.sidebar.len() - 1),
        }
    }

    // --- focus ----------------------------------------------------------------------------

    pub fn focus_pane(&mut self, pane: Pane) {
        self.focus = pane;
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Pane::Sidebar => Pane::Songs,
            Pane::Songs => Pane::Sidebar,
        };
    }

    // --- actions --------------------------------------------------------------------------

    /// Enter: in the sidebar, moves into the songs list; in the songs list, plays the track and
    /// queues the rest of the list.
    pub fn activate_selected(&mut self) {
        if self.focus == Pane::Sidebar {
            self.focus = Pane::Songs;
            return;
        }
        let result = match self.selected_track() {
            Some(t) if t.missing => Err(anyhow::anyhow!("{} is missing from the music folder", t.rel_path)),
            Some(chosen) => {
                // Queue everything playable in this (filtered) list, starting at the chosen track.
                let chosen = queue_key(chosen);
                self.unshuffled =
                    self.visible.iter().map(|&i| &self.tracks[i]).filter(|t| !t.missing).cloned().collect();
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

    pub fn rescan(&mut self) {
        if self.scan_rx.is_none() {
            self.scan_status = Some("scanning…".into());
            self.scan_rx = Some(scan::spawn(self.db_path.clone(), self.root.clone()));
        }
    }

    /// Opens the playlist picker for the selected track.
    pub fn start_add_to_playlist(&mut self) {
        match (self.focus, self.selected_track().map(|t| t.id)) {
            (Pane::Songs, Some(id)) => self.open_picker(vec![id], false),
            _ => self.message = Some("select a song to add it to a playlist (A adds the whole list)".into()),
        }
    }

    /// Opens the playlist picker for every track in the songs list (as filtered).
    pub fn start_add_all_to_playlist(&mut self) {
        let ids: Vec<i64> = self.visible.iter().map(|&i| self.tracks[i].id).collect();
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

    /// Renames the playlist being shown (the sidebar's selection).
    pub fn start_rename(&mut self) {
        if let Location::Playlist { id, name } = &self.location {
            self.mode = Mode::Input { purpose: InputPurpose::RenamePlaylist(*id), text: name.clone() };
        }
    }

    /// In the songs list of a playlist, removes the selected track from it; in the sidebar,
    /// asks to delete the selected playlist.
    pub fn delete_selected(&mut self) {
        let Location::Playlist { id, name } = &self.location else { return };
        match self.focus {
            Pane::Sidebar => self.mode = Mode::ConfirmDelete { playlist_id: *id, name: name.clone() },
            Pane::Songs => {
                let Some(entry) = self.selected_track().and_then(|t| t.entry_id) else { return };
                let result = db::remove_entry(&self.conn, entry).and_then(|_| self.reload());
                self.report(result);
            }
        }
    }

    /// Moves the selected playlist track up (-1) or down (+1).
    pub fn move_entry(&mut self, delta: isize) {
        if self.focus != Pane::Songs || !matches!(self.location, Location::Playlist { .. }) {
            return;
        }
        if !self.filter.is_empty() {
            self.message = Some("clear the filter to reorder".into());
            return;
        }
        let Some(from) = self.list_state.selected() else { return };
        let to = from as isize + delta;
        let entry_at = |i: isize| usize::try_from(i).ok().and_then(|i| self.track(i)).and_then(|t| t.entry_id);
        if let (Some(a), Some(b)) = (entry_at(from as isize), entry_at(to)) {
            let result = db::swap_entries(&mut self.conn, a, b).and_then(|_| self.reload());
            self.report(result);
            self.select(to as usize);
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
                if then_add.is_empty() {
                    // A new empty playlist: show it, ready to fill.
                    self.location = Location::Playlist { id, name: name.to_string() };
                    self.filter.clear();
                } else {
                    let added = db::add_tracks(&self.conn, id, &then_add, false)?;
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
        self.select(0);
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

    // --- panel sizes and the mouse --------------------------------------------------------

    /// `<` / `>`: moves the divider on the right of the focused pane by `delta` percent
    /// (positive is rightwards) and saves it.
    pub fn resize_focused(&mut self, delta: i16) {
        match self.focus {
            Pane::Sidebar => {
                self.sidebar_width = self.sidebar_width.saturating_add_signed(delta);
                self.clamp_widths(Divider::SidebarSongs);
            }
            Pane::Songs => {
                self.right_width = self.right_width.saturating_add_signed(-delta);
                self.clamp_widths(Divider::SongsRight);
            }
        }
        self.save_widths();
    }

    /// Keeps each column within its limits and the songs list at least `MIN_SONGS_WIDTH`;
    /// when the two side columns don't fit, the one beside `moved` gives way.
    fn clamp_widths(&mut self, moved: Divider) {
        self.sidebar_width = self.sidebar_width.clamp(*SIDEBAR_WIDTH_RANGE.start(), *SIDEBAR_WIDTH_RANGE.end());
        self.right_width = self.right_width.clamp(*RIGHT_WIDTH_RANGE.start(), *RIGHT_WIDTH_RANGE.end());
        let room = 100 - MIN_SONGS_WIDTH;
        if self.sidebar_width + self.right_width > room {
            match moved {
                Divider::SidebarSongs => self.sidebar_width = room - self.right_width,
                Divider::SongsRight => self.right_width = room - self.sidebar_width,
            }
        }
    }

    fn save_widths(&mut self) {
        let result = db::set_setting(&self.conn, SIDEBAR_WIDTH_SETTING, &self.sidebar_width.to_string())
            .and_then(|_| db::set_setting(&self.conn, RIGHT_WIDTH_SETTING, &self.right_width.to_string()));
        self.report(result);
    }

    fn pane_at(&self, column: u16, row: u16) -> Option<Pane> {
        let inside = |r: Rect| column >= r.x && column < r.x + r.width && row >= r.y && row < r.y + r.height;
        if inside(self.panes.sidebar) {
            Some(Pane::Sidebar)
        } else if inside(self.panes.songs) {
            Some(Pane::Songs)
        } else {
            None
        }
    }

    /// A left click: grabs a divider if it landed on one (either border of the pair), otherwise
    /// focuses the pane under the pointer and selects the clicked row.
    pub fn mouse_down(&mut self, column: u16, row: u16) {
        let (songs, right) = (self.panes.songs, self.panes.right);
        let in_rows = row >= songs.y && row < songs.y + songs.height;
        let on = |x: u16| in_rows && column + 1 >= x && column <= x;
        self.dragging = if on(songs.x) {
            Some(Divider::SidebarSongs)
        } else if on(right.x) {
            Some(Divider::SongsRight)
        } else {
            None
        };
        if self.dragging.is_some() {
            return;
        }
        let Some(pane) = self.pane_at(column, row) else { return };
        self.focus = pane;
        let (area, offset) = match pane {
            Pane::Sidebar => (self.panes.sidebar, self.sidebar_state.offset()),
            Pane::Songs => (self.panes.songs, self.list_state.offset()),
        };
        // Rows inside the border; the top border row selects nothing.
        if row > area.y && row + 1 < area.y + area.height {
            let index = offset + usize::from(row - area.y - 1);
            match pane {
                Pane::Sidebar if index < self.sidebar.len() => self.select_source(index),
                Pane::Songs if index < self.visible.len() => self.select(index),
                _ => {}
            }
        }
    }

    /// Moves the grabbed divider to follow the pointer. Saved when the button is released.
    pub fn mouse_drag(&mut self, column: u16) {
        let total = self.panes.sidebar.width + self.panes.songs.width + self.panes.right.width;
        let Some(divider) = self.dragging else { return };
        if total == 0 {
            return;
        }
        let x = self.panes.sidebar.x;
        let percent = |columns: u16| ((u32::from(columns) * 100 + u32::from(total) / 2) / u32::from(total)) as u16;
        match divider {
            // The border being dragged follows the pointer, so grabbing it doesn't jump.
            Divider::SidebarSongs => self.sidebar_width = percent(column.saturating_sub(x)),
            Divider::SongsRight => self.right_width = percent((x + total).saturating_sub(column)),
        }
        self.clamp_widths(divider);
    }

    pub fn mouse_up(&mut self) {
        if self.dragging.take().is_some() {
            self.save_widths();
        }
    }

    /// The mouse wheel: scrolls the pane under the pointer (the focused one elsewhere).
    pub fn scroll_at(&mut self, column: u16, row: u16, delta: isize) {
        let pane = self.pane_at(column, row).unwrap_or(self.focus);
        self.move_in(pane, delta);
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
        self.refresh_details();
    }

    /// Recomputes the info pane when the selection, focus or source changed since last time.
    fn refresh_details(&mut self) {
        let track = self.selected_track().map(queue_key);
        let key = DetailsKey { focus: self.focus, source: self.location.clone(), track };
        if self.details_key.as_ref() == Some(&key) {
            return;
        }
        let result = match (self.focus, track) {
            (Pane::Songs, Some((id, _))) => self.track_info(id),
            _ => Ok(Some(self.source_summary())),
        };
        match result {
            Ok(details) => self.details = details,
            Err(err) => self.report(Err(err)),
        }
        self.details_key = Some(key);
    }

    fn track_info(&self, track_id: i64) -> Result<Option<Details>> {
        let Some(details) = db::track_details(&self.conn, track_id)? else { return Ok(None) };
        let shown = match self.location {
            Location::Playlist { id, .. } => Some(id),
            Location::AllTracks => None,
        };
        let also_in = db::playlists_containing(&self.conn, track_id)?
            .into_iter()
            .filter(|(id, _)| Some(*id) != shown)
            .map(|(_, name)| name)
            .collect();
        Ok(Some(Details::Track { details, also_in }))
    }

    /// Built from the songs already loaded for the source; no query needed.
    fn source_summary(&self) -> Details {
        let known: Vec<Duration> = self.tracks.iter().filter_map(|t| t.duration).collect();
        Details::Source {
            tracks: self.tracks.len(),
            duration: known.iter().sum(),
            unknown: self.tracks.len() - known.len(),
            missing: self.tracks.iter().filter(|t| t.missing).count(),
        }
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
const SIDEBAR_WIDTH_SETTING: &str = "sidebar_width";
const RIGHT_WIDTH_SETTING: &str = "right_width";
const DEFAULT_SIDEBAR_WIDTH: u16 = 20;
const DEFAULT_RIGHT_WIDTH: u16 = 30;
const SIDEBAR_WIDTH_RANGE: std::ops::RangeInclusive<u16> = 10..=40;
const RIGHT_WIDTH_RANGE: std::ops::RangeInclusive<u16> = 15..=50;
const MIN_SONGS_WIDTH: u16 = 25;

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

        // The sidebar starts with just All Tracks, shown in the songs list, which has focus.
        assert_eq!(app.sidebar.len(), 1);
        assert_eq!((app.location.clone(), app.sidebar[0].count, app.visible.len()), (Location::AllTracks, 3, 3));
        assert_eq!(app.focus, Pane::Songs);

        // Filter to Tone 3 and add it to a new playlist; the songs list stays on All Tracks.
        app.set_filter("tone 3".into());
        assert_eq!(app.visible.len(), 1);
        app.start_add_to_playlist();
        app.confirm_pick(); // item 0: new playlist
        app.mode = Mode::Input {
            purpose: InputPurpose::NewPlaylist { then_add: vec![app.selected_track().unwrap().id] },
            text: "Mix".into(),
        };
        app.submit_input();
        assert_eq!(app.message.as_deref(), Some("added 1 track to Mix"));
        assert_eq!((app.sidebar.len(), app.location.clone()), (2, Location::AllTracks));
        app.set_filter(String::new());

        // Add Tone 1 too, via the picker's existing playlist row.
        app.select(0);
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

        // The info pane: the selected song, with the playlists it's in.
        app.select(2);
        app.tick();
        match &app.details {
            Some(Details::Track { details, also_in }) => {
                assert_eq!((details.title.as_deref(), details.track_no), (Some("Tone 3"), Some(3)));
                assert_eq!((details.year, details.genre.as_deref()), (Some(1997), Some("Test Genre")));
                assert_eq!(
                    (details.format.as_deref(), details.sample_rate, details.channels),
                    (Some("MP3"), Some(44_100), Some(1))
                );
                assert!(details.bitrate_kbps.is_some_and(|b| b > 0));
                assert_eq!(also_in, &["Mix"]);
            }
            _ => panic!("expected track details"),
        }
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 26)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let screen = format!("{}", terminal.backend());
        println!("{screen}");
        assert!(screen.contains("Selected") && screen.contains("Also in: Mix") && screen.contains("Track 3"));
        assert!(screen.contains("1997 · Test Genre") && screen.contains("MP3 ·") && screen.contains("44.1 kHz"));
        let mut small = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 26)).unwrap();
        small.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert!(!format!("{}", small.backend()).contains("Selected"), "hidden on narrow screens");

        // Moving through the sidebar previews each source; Enter moves into its songs.
        let titles =
            |app: &App| -> Vec<String> { (0..app.visible.len()).filter_map(|i| app.track(i)?.title.clone()).collect() };
        app.focus_pane(Pane::Sidebar);
        app.move_selection(1);
        assert!(matches!(&app.location, Location::Playlist { name, .. } if name == "Mix"));
        assert_eq!(app.sidebar[1].count, 3);
        assert_eq!(titles(&app), ["Tone 3", "Tone 1", "Tone 2"], "playlist order");
        app.move_selection(-1);
        assert_eq!(app.location, Location::AllTracks);
        app.move_selection(1);
        // With the sidebar focused, the info pane summarises the source.
        app.tick();
        match &app.details {
            Some(Details::Source { tracks, duration, unknown, missing }) => {
                assert_eq!((*tracks, *unknown, *missing), (3, 0, 0));
                assert!(duration.as_secs_f64() > 8.5, "{duration:?}");
            }
            _ => panic!("expected a source summary"),
        }
        app.activate_selected();
        assert_eq!(app.focus, Pane::Songs);
        // Inside Mix, Tone 3 isn't in any *other* playlist.
        app.tick();
        assert!(matches!(&app.details, Some(Details::Track { also_in, .. }) if also_in.is_empty()));

        // Move Tone 1 above Tone 3.
        app.select(1);
        app.move_entry(-1);
        assert_eq!(titles(&app), ["Tone 1", "Tone 3", "Tone 2"]);

        // Rename the playlist from the songs list; the sidebar and title follow.
        app.start_rename();
        if let Mode::Input { text, .. } = &mut app.mode {
            *text = "Mix 2".into();
        }
        app.submit_input();
        assert!(matches!(&app.location, Location::Playlist { name, .. } if name == "Mix 2"));
        assert!(matches!(&app.sidebar[1].location, Location::Playlist { name, .. } if name == "Mix 2"));

        // A new empty playlist is shown straight away; deleting it from the sidebar falls back to
        // the playlist now in its place.
        app.start_new_playlist();
        if let Mode::Input { text, .. } = &mut app.mode {
            *text = "Empty".into();
        }
        app.submit_input();
        assert!(matches!(&app.location, Location::Playlist { name, .. } if name == "Empty"));
        assert_eq!((app.sidebar.len(), app.visible.len()), (3, 0));
        app.focus_pane(Pane::Sidebar);
        app.delete_selected();
        assert!(matches!(app.mode, Mode::ConfirmDelete { .. }));
        app.confirm_delete();
        assert_eq!(app.sidebar.len(), 2);
        assert!(matches!(&app.location, Location::Playlist { name, .. } if name == "Mix 2"));
        app.focus_pane(Pane::Songs);

        app.select(0);
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

        // Mouse and resizing at 100 columns: sidebar 20%, songs the rest, right column 30%.
        let render = |app: &mut App| {
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 10)).unwrap();
            terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
            format!("{}", terminal.backend())
        };
        render(&mut app);
        assert_eq!((app.panes.sidebar.width, app.panes.songs.x, app.panes.right.x), (20, 20, 70));

        // Clicking a sidebar row focuses the sidebar and shows that source; the wheel scrolls
        // the pane under the pointer; clicking a song selects it.
        app.mouse_down(5, 1);
        assert_eq!((app.focus, app.location.clone()), (Pane::Sidebar, Location::AllTracks));
        app.scroll_at(5, 3, 3);
        assert!(matches!(&app.location, Location::Playlist { name, .. } if name == "Mix 2"), "clamped to the last");
        app.mouse_down(40, 2);
        assert_eq!((app.focus, app.list_state.selected()), (Pane::Songs, Some(1)));
        app.mouse_drag(60);
        assert_eq!(app.sidebar_width, 20, "a click away from the dividers doesn't drag");

        // Drag both dividers; nothing is saved until the button is released.
        app.mouse_down(20, 2);
        app.mouse_drag(30);
        assert_eq!(app.sidebar_width, 30);
        assert_eq!(db::get_setting(&app.conn, "sidebar_width").unwrap(), None, "not saved mid-drag");
        app.mouse_up();
        assert_eq!(db::get_setting(&app.conn, "sidebar_width").unwrap().as_deref(), Some("30"));
        app.mouse_down(70, 2);
        app.mouse_drag(60);
        app.mouse_up();
        assert_eq!(app.right_width, 40);
        render(&mut app);
        assert_eq!((app.panes.songs.x, app.panes.right.x), (30, 60), "columns follow the drops");

        // < and > move the divider right of the focused pane; the songs list keeps its minimum.
        app.focus_pane(Pane::Songs);
        app.resize_focused(2);
        assert_eq!(app.right_width, 38);
        app.focus_pane(Pane::Sidebar);
        app.resize_focused(100);
        assert_eq!(app.sidebar_width, 37, "clamped so the songs list keeps 25%");
        let reopened = App::new(app.root.clone(), db_path.clone(), Player::new().unwrap()).unwrap();
        assert_eq!((reopened.sidebar_width, reopened.right_width), (37, 38), "remembered");
        drop(reopened);
        app.focus_pane(Pane::Songs);
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
