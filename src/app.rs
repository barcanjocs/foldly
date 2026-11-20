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
use crate::media::{NowPlaying, PlayState};
use crate::player::Player;
use crate::queue::{self, Queue, RemoveError};
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
    Queue,
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
    /// The queue's part of the right column (all of it when the info pane is hidden).
    pub queue: Rect,
    /// The progress bar, including its border.
    pub transport: Rect,
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
    pub queue: Queue,
    /// First queue row the last frame showed, for mouse clicks. Set by `ui::draw`.
    pub queue_offset: usize,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub now_playing: Option<Track>,
    pub player: Player,

    pub message: Option<String>,
    pub scan_status: Option<String>,
    scan_rx: Option<Receiver<ScanEvent>>,
    /// Watches the music folder; `None` if watching couldn't start (`u` still rescans).
    watch: Option<Watch>,
    /// Media keys and the OS's Now Playing display; `None` where unavailable.
    os_media: Option<NowPlaying>,
    /// Ask the main loop to repaint the whole screen, wiping anything another library may
    /// have written over it.
    pub needs_full_redraw: bool,
    /// Column widths in percent of the screen; the songs list gets the rest.
    pub sidebar_width: u16,
    pub right_width: u16,
    pub panes: PaneRects,
    dragging: Option<Divider>,
    /// While dragging on the progress bar: where the track will be when the button is released.
    /// The bar shows this instead of the real position; the seek happens once, on release.
    pub scrub: Option<Duration>,
    /// The last row clicked and when, to spot a double-click.
    last_click: Option<(Instant, Pane, usize)>,
    /// When the session was last saved, and the queue as last written (it can be the whole
    /// library, so it's only rewritten when it changes).
    session_saved_at: Instant,
    saved_queue: Option<(String, String)>,
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
            queue: Queue::default(),
            shuffle: false,
            repeat: Repeat::Off,
            queue_offset: 0,
            now_playing: None,
            player,
            message: None,
            scan_status: None,
            scan_rx: None,
            watch: None,
            os_media: None,
            needs_full_redraw: false,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            right_width: DEFAULT_RIGHT_WIDTH,
            panes: PaneRects::default(),
            dragging: None,
            scrub: None,
            last_click: None,
            session_saved_at: Instant::now(),
            saved_queue: None,
            should_quit: false,
        };
        match Watch::start(&app.root) {
            Ok(watch) => app.watch = Some(watch),
            Err(err) => app.message = Some(format!("{err:#}; press u to rescan after adding music")),
        }
        // Optional: where there's no OS service (e.g. Linux without a D-Bus session), the keys and
        // Now Playing simply aren't there.
        app.os_media = NowPlaying::start().ok();
        app.shuffle = db::get_setting(&app.conn, SHUFFLE_SETTING)?.as_deref() == Some("1");
        if let Some(volume) = db::get_setting(&app.conn, VOLUME_SETTING)?.and_then(|v| v.parse().ok()) {
            app.player.set_volume(volume);
        }
        app.repeat = Repeat::parse(db::get_setting(&app.conn, REPEAT_SETTING)?.as_deref().unwrap_or_default());
        let saved_width =
            |key| -> Result<Option<u16>> { Ok(db::get_setting(&app.conn, key)?.and_then(|w| w.parse().ok())) };
        let (sidebar, right) = (saved_width(SIDEBAR_WIDTH_SETTING)?, saved_width(RIGHT_WIDTH_SETTING)?);
        app.sidebar_width = sidebar.unwrap_or(DEFAULT_SIDEBAR_WIDTH);
        app.right_width = right.unwrap_or(DEFAULT_RIGHT_WIDTH);
        app.clamp_widths(Divider::SongsRight);
        app.reload()?;
        app.select(0);
        app.restore_session()?;
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
            Pane::Queue => self.queue.move_cursor(delta),
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
            Pane::Queue => self.move_in(Pane::Queue, isize::MIN / 2),
        }
    }

    pub fn select_last(&mut self) {
        match self.focus {
            Pane::Songs => self.select(usize::MAX),
            Pane::Sidebar => self.select_source(self.sidebar.len() - 1),
            Pane::Queue => self.move_in(Pane::Queue, isize::MAX / 2),
        }
    }

    // --- focus ----------------------------------------------------------------------------

    pub fn focus_pane(&mut self, pane: Pane) {
        self.focus = pane;
    }

    /// Tab: sidebar → songs → queue → sidebar.
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Pane::Sidebar => Pane::Songs,
            Pane::Songs => Pane::Queue,
            Pane::Queue => Pane::Sidebar,
        };
    }

    /// `h`: one pane to the left.
    pub fn focus_left(&mut self) {
        self.focus = match self.focus {
            Pane::Queue => Pane::Songs,
            Pane::Songs | Pane::Sidebar => Pane::Sidebar,
        };
    }

    // --- actions --------------------------------------------------------------------------

    /// Enter: in the sidebar, moves into the songs list; in the songs list, plays the track and
    /// queues the rest of the list.
    pub fn activate_selected(&mut self) {
        match self.focus {
            Pane::Sidebar => {
                self.focus = Pane::Songs;
                return;
            }
            Pane::Queue => {
                // Jump to the selected track; the cursor then follows the current one again.
                if let Some(i) = self.queue.cursor() {
                    self.play_index(i);
                    self.queue.follow_current();
                }
                return;
            }
            Pane::Songs => {}
        }
        let result = match self.selected_track() {
            Some(t) if t.missing => Err(anyhow::anyhow!("{} is missing from the music folder", t.rel_path)),
            Some(chosen) => {
                // Queue everything playable in this (filtered) list, starting at the chosen track.
                let chosen = queue::key(chosen);
                let list = self.visible.iter().map(|&i| &self.tracks[i]).filter(|t| !t.missing).cloned().collect();
                let start = self.queue.start(list, chosen, self.shuffle);
                self.play_index(start);
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

    /// Opens the playlist picker for the selected track (in the songs list or the queue).
    pub fn start_add_to_playlist(&mut self) {
        match (self.focus, self.selected_track().map(|t| t.id), self.queue.cursor_track().map(|t| t.id)) {
            (Pane::Songs, Some(id), _) => self.open_picker(vec![id], false, true),
            // Queued tracks may come from anywhere, so the shown playlist is offered too.
            (Pane::Queue, _, Some(id)) => self.open_picker(vec![id], false, false),
            _ => self.message = Some("select a song to add it to a playlist (A adds the whole list)".into()),
        }
    }

    /// Opens the playlist picker for every track in the songs list (as filtered), or in the
    /// queue when the queue is focused.
    pub fn start_add_all_to_playlist(&mut self) {
        if self.focus == Pane::Queue {
            let ids: Vec<i64> = self.queue.tracks().iter().map(|t| t.id).collect();
            if ids.is_empty() {
                self.message = Some("the queue is empty".into());
            } else {
                self.open_picker(ids, true, false);
            }
            return;
        }
        let ids: Vec<i64> = self.visible.iter().map(|&i| self.tracks[i].id).collect();
        if ids.is_empty() {
            self.message = Some("no tracks here to add".into());
        } else {
            self.open_picker(ids, true, true);
        }
    }

    /// Opens the playlist picker for the song that's playing, wherever you are.
    pub fn start_add_playing_to_playlist(&mut self) {
        match self.now_playing.as_ref().map(|t| t.id) {
            // It may well not be in the playlist being shown, so that one is offered too.
            Some(id) => self.open_picker(vec![id], false, false),
            None => self.message = Some("nothing is playing".into()),
        }
    }

    /// `skip_shown` leaves out the playlist being shown, for tracks picked from it: adding a
    /// playlist's tracks to itself would only duplicate them.
    fn open_picker(&mut self, track_ids: Vec<i64>, bulk: bool, skip_shown: bool) {
        match db::playlists(&self.conn) {
            Ok(mut playlists) => {
                if let (true, Location::Playlist { id, .. }) = (skip_shown, &self.location) {
                    playlists.retain(|p| p.id != *id);
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
        if self.focus == Pane::Queue {
            self.queue_remove();
            return;
        }
        let Location::Playlist { id, name } = &self.location else { return };
        match self.focus {
            Pane::Queue => {}
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
        if self.focus == Pane::Queue {
            self.queue_move(delta);
            return;
        }
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

    // --- queue editing (the rules live in queue.rs) ---------------------------------------

    fn queue_remove(&mut self) {
        if self.queue.remove_selected() == Err(RemoveError::Playing) {
            self.message = Some("that's the song playing; n skips it".into());
        }
    }

    fn queue_move(&mut self, delta: isize) {
        self.queue.move_selected(delta, self.shuffle);
    }

    /// `c`: drops everything queued after the current track.
    pub fn queue_clear_upcoming(&mut self) {
        if self.queue.current().is_none() {
            return;
        }
        self.message = Some(match self.queue.clear_upcoming() {
            0 => "nothing queued after this song".into(),
            1 => "cleared 1 upcoming song".into(),
            n => format!("cleared {n} upcoming songs"),
        });
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
                    self.queue.set_current(Some(index));
                    self.message = None;
                    // A drag on the bar was aimed at the previous track.
                    self.scrub = None;
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
        let Some(i) = self.queue.current() else { return };
        if i + 1 < self.queue.len() || self.repeat != Repeat::All {
            self.play_index(i + 1);
        } else {
            if self.shuffle {
                self.queue.reshuffle();
            }
            self.play_index(0);
        }
    }

    pub fn previous(&mut self) {
        let Some(i) = self.queue.current() else { return };
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
            // The queue has no divider on its right, so it uses the one on its left.
            Pane::Songs | Pane::Queue => {
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
        } else if inside(self.panes.queue) {
            Some(Pane::Queue)
        } else {
            None
        }
    }

    /// A left click at `now`: grabs a divider if it landed on one (either border of the pair),
    /// seeks if it landed on the progress bar, otherwise focuses the pane under the pointer and
    /// selects the clicked row. A second click on the same row within `DOUBLE_CLICK` acts like
    /// Enter: plays a song, or moves from the sidebar into its songs.
    pub fn mouse_down(&mut self, column: u16, row: u16, now: Instant) {
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
        if self.seek_to_column(column, row) {
            return;
        }
        let Some(pane) = self.pane_at(column, row) else { return };
        self.focus = pane;
        let (area, offset) = match pane {
            Pane::Sidebar => (self.panes.sidebar, self.sidebar_state.offset()),
            Pane::Songs => (self.panes.songs, self.list_state.offset()),
            Pane::Queue => (self.panes.queue, self.queue_offset),
        };
        // Rows inside the border; the top border row selects nothing.
        if row > area.y && row + 1 < area.y + area.height {
            let index = offset + usize::from(row - area.y - 1);
            match pane {
                Pane::Sidebar if index < self.sidebar.len() => self.select_source(index),
                Pane::Songs if index < self.visible.len() => self.select(index),
                Pane::Queue if self.queue.select(index) => {}
                _ => return,
            }
            let double = self
                .last_click
                .is_some_and(|(at, p, i)| p == pane && i == index && now.duration_since(at) <= DOUBLE_CLICK);
            if double {
                self.last_click = None;
                self.activate_selected();
            } else {
                self.last_click = Some((now, pane, index));
            }
        }
    }

    /// A press on the progress bar starts scrubbing at that point; returns whether it was on the
    /// bar. The seek itself waits for the release (`mouse_up`), so dragging doesn't stutter.
    fn seek_to_column(&mut self, column: u16, row: u16) -> bool {
        let bar = self.panes.transport;
        let on_bar = row >= bar.y && row < bar.y + bar.height && column > bar.x && column + 1 < bar.x + bar.width;
        if !on_bar {
            return false;
        }
        self.scrub = self.bar_position(column);
        true
    }

    /// The point of the track under `column` of the progress bar; columns past either end of
    /// the bar give its start or end, so a drag can overshoot.
    fn bar_position(&self, column: u16) -> Option<Duration> {
        let duration = self.now_playing.as_ref().and_then(|t| t.duration)?;
        let bar = self.panes.transport;
        // The bar fills the border's inside, columns x+1 ..= x+width-2.
        let inner = f64::from(bar.width.saturating_sub(2).max(1));
        let fraction = (f64::from(column.saturating_sub(bar.x + 1)) / inner).clamp(0.0, 1.0);
        Some(duration.mul_f64(fraction))
    }

    /// Moves the grabbed divider to follow the pointer. Saved when the button is released.
    pub fn mouse_drag(&mut self, column: u16) {
        if self.scrub.is_some() {
            self.scrub = self.bar_position(column).or(self.scrub);
            return;
        }
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
        if let Some(position) = self.scrub.take() {
            self.player.seek_to(position);
        }
    }

    /// The mouse wheel: scrolls the pane under the pointer (the focused one elsewhere).
    pub fn scroll_at(&mut self, column: u16, row: u16, delta: isize) {
        let pane = self.pane_at(column, row).unwrap_or(self.focus);
        self.move_in(pane, delta);
    }

    /// `+` / `-`: changes the volume by `delta` (0.05 is 5%) and remembers it.
    pub fn change_volume(&mut self, delta: f32) {
        self.set_volume(self.player.volume() + delta);
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.player.set_volume(volume);
        let result = db::set_setting(&self.conn, VOLUME_SETTING, &self.player.volume().to_string());
        self.report(result);
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

        self.queue.set_shuffle(self.shuffle);
    }

    pub fn stop(&mut self) {
        self.player.stop();
        self.queue.set_current(None);
        self.now_playing = None;
    }

    /// Called every loop iteration: advances when a track ends, rescans once folder changes
    /// settle, and picks up scan progress.
    pub fn tick(&mut self) {
        let commands = self.os_media.as_ref().map(NowPlaying::commands).unwrap_or_default();
        for command in commands {
            self.handle_media_command(command);
        }
        if let Some(message) = self.player.handle_errors() {
            self.message = Some(message);
            self.needs_full_redraw = true;
        }
        self.poll_watch();
        if self.player.finished() {
            match (self.repeat, self.queue.current()) {
                (Repeat::One, Some(i)) => self.play_index(i),
                _ => self.next(),
            }
        }
        self.poll_scan();
        self.refresh_details();
        self.publish_now_playing();
        // Saved now and then too, so closing the terminal or a crash loses little.
        if self.player.is_active() && self.session_saved_at.elapsed() >= SESSION_SAVE_INTERVAL {
            let result = self.save_session();
            self.report(result);
        }
    }

    /// Recomputes the info pane when the selection, focus or source changed since last time.
    fn refresh_details(&mut self) {
        let track = match self.focus {
            Pane::Queue => self.queue.cursor_track().map(queue::key),
            _ => self.selected_track().map(queue::key),
        };
        let key = DetailsKey { focus: self.focus, source: self.location.clone(), track };
        if self.details_key.as_ref() == Some(&key) {
            return;
        }
        let result = match (self.focus, track) {
            (Pane::Songs | Pane::Queue, Some((id, _))) => self.track_info(id),
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

    // --- media keys and Now Playing -------------------------------------------------------

    /// Acts on a command from media keys, Control Center, or an MPRIS client like `playerctl`.
    pub fn handle_media_command(&mut self, command: souvlaki::MediaControlEvent) {
        use souvlaki::{MediaControlEvent as E, MediaPosition, SeekDirection};
        let playing = self.player.is_active() && !self.player.is_paused();
        let seek = |dir: SeekDirection, by: Duration| -> i64 {
            let secs = by.as_secs().max(1) as i64;
            if dir == SeekDirection::Forward { secs } else { -secs }
        };
        match command {
            E::Play if self.player.is_paused() => self.player.toggle_pause(),
            E::Pause if playing => self.player.toggle_pause(),
            E::Toggle => self.player.toggle_pause(),
            E::Next => self.next(),
            E::Previous => self.previous(),
            E::Stop => self.stop(),
            E::Seek(dir) => self.player.seek_by(seek(dir, Duration::from_secs(5))),
            E::SeekBy(dir, by) => self.player.seek_by(seek(dir, by)),
            E::SetPosition(MediaPosition(position)) => self.player.seek_to(position),
            E::SetVolume(volume) => self.set_volume(volume as f32),
            E::SetShuffle(on) if on != self.shuffle => self.toggle_shuffle(),
            _ => {}
        }
    }

    fn publish_now_playing(&mut self) {
        let Some(media) = &mut self.os_media else { return };
        let state = match (self.player.is_active(), self.player.is_paused()) {
            (false, _) => PlayState::Stopped,
            (true, true) => PlayState::Paused,
            (true, false) => PlayState::Playing,
        };
        media.publish(self.now_playing.as_ref(), state, self.player.position());
    }

    // --- session --------------------------------------------------------------------------

    /// Saves where you are so the next start picks up from here: the source shown, the queue
    /// (in play and original order), the current track and its position. With nothing loaded,
    /// the saved queue is cleared.
    pub fn save_session(&mut self) -> Result<()> {
        let location = match &self.location {
            Location::AllTracks => "all".to_string(),
            Location::Playlist { id, .. } => format!("playlist:{id}"),
        };
        let queue = self.queue.encode();
        let tx = self.conn.unchecked_transaction()?;
        db::set_setting(&tx, SESSION_LOCATION, &location)?;
        match self.queue.current().filter(|_| self.player.is_active()) {
            Some(index) => {
                db::set_setting(&tx, SESSION_INDEX, &index.to_string())?;
                db::set_setting(&tx, SESSION_POSITION_MS, &self.player.position().as_millis().to_string())?;
                if self.saved_queue.as_ref() != Some(&queue) {
                    db::set_setting(&tx, SESSION_QUEUE, &queue.0)?;
                    db::set_setting(&tx, SESSION_UNSHUFFLED, &queue.1)?;
                }
            }
            None => {
                for key in [SESSION_INDEX, SESSION_POSITION_MS, SESSION_QUEUE, SESSION_UNSHUFFLED] {
                    db::delete_setting(&tx, key)?;
                }
            }
        }
        tx.commit()?;
        self.saved_queue = Some(queue);
        self.session_saved_at = Instant::now();
        Ok(())
    }

    /// Brings back the last saved session: shows the same source, rebuilds the queue (dropping
    /// tracks whose files are missing now) and loads the current track paused at its position,
    /// selected in the songs list if it's there.
    fn restore_session(&mut self) -> Result<()> {
        let setting = |key| db::get_setting(&self.conn, key);
        let location = setting(SESSION_LOCATION)?;
        let index: Option<usize> = setting(SESSION_INDEX)?.and_then(|s| s.parse().ok());
        let position = setting(SESSION_POSITION_MS)?.and_then(|s| s.parse().ok()).map(Duration::from_millis);
        let (queue, unshuffled) = (setting(SESSION_QUEUE)?.unwrap_or_default(), setting(SESSION_UNSHUFFLED)?);

        let source = match location.as_deref().and_then(|l| l.strip_prefix("playlist:")) {
            Some(id) => id.parse().ok().and_then(|id| {
                self.sidebar
                    .iter()
                    .position(|item| matches!(item.location, Location::Playlist { id: p, .. } if p == id))
            }),
            None => Some(0),
        };
        self.show_source(source.unwrap_or(0))?;

        let Some(index) = index else { return Ok(()) };
        let tracks = db::tracks_by_id(&self.conn)?;
        let find = |id: i64| tracks.get(&id).filter(|t| !t.missing).cloned();
        let (restored, at) = Queue::restore(&queue, unshuffled.as_deref(), Some(index), find);
        self.queue = restored;
        self.saved_queue = Some(self.queue.encode());
        let Some(at) = at else {
            return Ok(()); // the current track's file is gone; keep the queue, load nothing
        };
        let track = self.queue.tracks()[at].clone();
        match self.player.load_paused(&self.root.join(&track.rel_path), position.unwrap_or_default()) {
            Ok(decoder_duration) => {
                if let Some(row) = self.visible.iter().position(|&i| queue::key(&self.tracks[i]) == queue::key(&track))
                {
                    self.select(row);
                }
                self.now_playing = Some(Track { duration: track.duration.or(decoder_duration), ..track });
                self.queue.set_current(Some(at));
                self.message = Some("picked up where you left off; Space plays".into());
            }
            Err(err) => self.message = Some(format!("{err:#}")),
        }
        Ok(())
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
const VOLUME_SETTING: &str = "volume";
const SIDEBAR_WIDTH_SETTING: &str = "sidebar_width";
const RIGHT_WIDTH_SETTING: &str = "right_width";
const DEFAULT_SIDEBAR_WIDTH: u16 = 20;
const DEFAULT_RIGHT_WIDTH: u16 = 30;
const SIDEBAR_WIDTH_RANGE: std::ops::RangeInclusive<u16> = 10..=40;
const RIGHT_WIDTH_RANGE: std::ops::RangeInclusive<u16> = 15..=50;
const MIN_SONGS_WIDTH: u16 = 25;
const SESSION_LOCATION: &str = "session_location";
const SESSION_QUEUE: &str = "session_queue";
const SESSION_UNSHUFFLED: &str = "session_unshuffled";
const SESSION_INDEX: &str = "session_index";
const SESSION_POSITION_MS: &str = "session_position_ms";
const SESSION_SAVE_INTERVAL: Duration = Duration::from_secs(15);

/// Two clicks on the same row at most this far apart are a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

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

        // A fresh start plays at full volume.
        assert_eq!(app.player.volume(), 1.0);

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
        // An empty, focused songs list says why it's empty, and only it has the heavy border.
        let frame = |app: &mut App| {
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 26)).unwrap();
            terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
            let buffer = terminal.backend().buffer().clone();
            let top: String = (0..120).map(|x| buffer[(x, 0)].symbol().to_string()).collect();
            (format!("{}", terminal.backend()), top)
        };
        let (screen, top) = frame(&mut app);
        println!("{screen}");
        assert!(screen.contains("This playlist is empty."));
        assert_eq!(top.matches('┏').count(), 1, "one heavy border: {top}");
        assert!(top.find('┏').unwrap() > top.find('┌').unwrap(), "the songs pane's, not the sidebar's: {top}");
        app.set_filter("zzz".into());
        assert!(frame(&mut app).0.contains("No songs match"));
        app.set_filter(String::new());
        app.focus_pane(Pane::Sidebar);
        let (_, top) = frame(&mut app);
        assert!(top.starts_with('┏'), "focus moved to the sidebar: {top}");
        app.delete_selected();
        assert!(matches!(app.mode, Mode::ConfirmDelete { .. }));
        app.confirm_delete();
        assert_eq!(app.sidebar.len(), 2);
        assert!(matches!(&app.location, Location::Playlist { name, .. } if name == "Mix 2"));
        app.focus_pane(Pane::Songs);

        app.select(0);
        app.activate_selected();
        assert_eq!(app.now_playing.as_ref().unwrap().title.as_deref(), Some("Tone 1"));

        // P adds the playing song (not the selection), and offers the playlist being shown.
        app.message = None;
        app.select(2);
        app.start_add_playing_to_playlist();
        match &mut app.mode {
            Mode::PickPlaylist { track_ids, playlists, state, .. } => {
                assert_eq!(track_ids, &[app.now_playing.as_ref().unwrap().id]);
                assert!(playlists.iter().any(|p| p.name == "Mix 2"), "the shown playlist is offered");
                state.select(Some(0));
            }
            _ => panic!("picker should be open"),
        }
        app.mode = Mode::Normal;
        app.select(0);
        app.player.seek_by(2);
        for _ in 0..30 {
            app.tick();
            if app.queue.current() == Some(1) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(app.now_playing.as_ref().unwrap().title.as_deref(), Some("Tone 3"));

        // Shuffle on mid-track: the current track stays and plays first; the queue holds the
        // same tracks. Off again: original playlist order, still on the current track.
        let ordered: Vec<_> = app.queue.tracks().iter().map(|t| t.title.clone()).collect();
        app.toggle_shuffle();
        assert_eq!(app.queue.current(), Some(0));
        assert_eq!(app.queue.tracks()[0].title.as_deref(), Some("Tone 3"));
        let mut shuffled: Vec<_> = app.queue.tracks().iter().map(|t| t.title.clone()).collect();
        shuffled.sort();
        let mut expected = ordered.clone();
        expected.sort();
        assert_eq!(shuffled, expected);
        app.toggle_shuffle();
        assert_eq!(app.queue.tracks().iter().map(|t| t.title.clone()).collect::<Vec<_>>(), ordered);
        assert_eq!(app.queue.tracks()[app.queue.current().unwrap()].title.as_deref(), Some("Tone 3"));

        // Repeat one: when Tone 3 ends it plays again.
        app.cycle_repeat();
        app.cycle_repeat();
        assert_eq!(app.repeat, Repeat::One);
        let index = app.queue.current();
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
        assert_eq!(app.queue.current(), index);

        // Repeat all: next past the last track wraps to the first, previous wraps back.
        app.cycle_repeat();
        app.cycle_repeat();
        assert_eq!(app.repeat, Repeat::All);
        let last = app.queue.len() - 1;
        app.play_index(last);
        app.next();
        assert_eq!(app.queue.current(), Some(0));
        app.previous();
        assert_eq!(app.queue.current(), Some(last));

        // Repeat off: next past the end stops.
        app.cycle_repeat();
        app.cycle_repeat();
        assert_eq!(app.repeat, Repeat::Off);
        app.next();
        assert_eq!(app.queue.current(), None);
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
        let (index, before) = (app.queue.current(), app.player.position());
        app.player.toggle_pause();
        app.player.inject_error(rodio::cpal::ErrorKind::DeviceNotAvailable);
        app.tick();
        assert_eq!(app.message.as_deref(), Some("audio output changed; reconnected to the default device"));
        assert!(app.needs_full_redraw);
        assert_eq!(app.queue.current(), index, "same track");
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
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        app.mouse_down(5, 1, at(0));
        assert_eq!((app.focus, app.location.clone()), (Pane::Sidebar, Location::AllTracks));
        app.scroll_at(5, 3, 3);
        assert!(matches!(&app.location, Location::Playlist { name, .. } if name == "Mix 2"), "clamped to the last");
        app.mouse_down(40, 2, at(1000));
        assert_eq!((app.focus, app.list_state.selected()), (Pane::Songs, Some(1)));

        // A slow second click on the same song only selects; a quick one plays it from the start.
        let playing = app.now_playing.as_ref().map(|t| t.id);
        assert!(app.player.position() > Duration::from_millis(800), "Tone 3 has been playing a while");
        app.mouse_down(40, 2, at(2000));
        assert!(app.player.position() > Duration::from_millis(800), "not a double-click");
        app.mouse_down(40, 2, at(2200));
        assert_eq!(app.now_playing.as_ref().map(|t| t.id), playing, "the same song (Tone 3), restarted");
        assert!(app.player.position() < Duration::from_millis(500), "{:?}", app.player.position());

        // Clicking the middle of the progress bar seeks to about half way. At 100×10 the bar's
        // border is rows 6..9 and its inside columns 1..=98.
        assert_eq!(app.panes.transport, Rect::new(0, 6, 100, 3));
        app.mouse_down(50, 7, at(5000));
        app.mouse_up();
        std::thread::sleep(Duration::from_millis(100));
        let position = app.player.position().as_secs_f64();
        assert!((1.3..2.0).contains(&position), "seeked to {position}s of 3s");

        // Dragging along the bar previews the spot (the bar shows it) and seeks once, on release.
        app.mouse_down(25, 7, at(5500));
        let pressed_at = app.player.position();
        app.mouse_drag(75);
        let preview = app.scrub.expect("scrubbing").as_secs_f64();
        assert!((2.1..2.4).contains(&preview), "previewing {preview}s");
        assert!(app.player.position() < pressed_at + Duration::from_millis(300), "no seek while dragging");
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 10)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert!(format!("{}", terminal.backend()).contains("00:02 / 00:03"), "the bar shows the preview");
        app.mouse_drag(200);
        assert_eq!(app.scrub, app.now_playing.as_ref().unwrap().duration, "past the end: the end");
        app.mouse_drag(75);
        app.mouse_up();
        assert!(app.scrub.is_none());
        std::thread::sleep(Duration::from_millis(100));
        let position = app.player.position().as_secs_f64();
        assert!((2.1..2.8).contains(&position), "seeked to {position}s on release");
        app.player.seek_to(Duration::from_millis(1500));
        app.mouse_drag(60);
        assert_eq!(app.sidebar_width, 20, "a click away from the dividers doesn't drag");

        // Drag both dividers; nothing is saved until the button is released.
        app.mouse_down(20, 2, at(6000));
        app.mouse_drag(30);
        assert_eq!(app.sidebar_width, 30);
        assert_eq!(db::get_setting(&app.conn, "sidebar_width").unwrap(), None, "not saved mid-drag");
        app.mouse_up();
        assert_eq!(db::get_setting(&app.conn, "sidebar_width").unwrap().as_deref(), Some("30"));
        app.mouse_down(70, 2, at(7000));
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

        // Media keys / Control Center / MPRIS commands drive playback like the keyboard does.
        {
            use souvlaki::{MediaControlEvent as E, MediaPosition, SeekDirection};
            app.focus_pane(Pane::Songs);
            app.select(0);
            app.activate_selected(); // Tone 1, queue Tone 1, Tone 3, Tone 2
            let title = |app: &App| app.now_playing.as_ref().map(|t| t.display_title());
            app.handle_media_command(E::Toggle);
            assert!(app.player.is_paused());
            app.handle_media_command(E::Pause);
            assert!(app.player.is_paused(), "pause while paused stays paused");
            app.handle_media_command(E::Play);
            assert!(!app.player.is_paused());
            app.handle_media_command(E::Play);
            assert!(!app.player.is_paused(), "play while playing keeps playing");
            app.handle_media_command(E::SetPosition(MediaPosition(Duration::from_millis(1500))));
            std::thread::sleep(Duration::from_millis(100));
            assert!((1.4..2.0).contains(&app.player.position().as_secs_f64()), "{:?}", app.player.position());
            app.handle_media_command(E::SeekBy(SeekDirection::Backward, Duration::from_secs(1)));
            std::thread::sleep(Duration::from_millis(100));
            assert!(app.player.position() < Duration::from_millis(1200), "{:?}", app.player.position());
            app.handle_media_command(E::SetVolume(0.5));
            assert_eq!(app.player.volume(), 0.5);
            app.set_volume(1.0);
            app.handle_media_command(E::Next);
            assert_eq!(title(&app).as_deref(), Some("Tone 3"));
            app.handle_media_command(E::Previous);
            assert_eq!(title(&app).as_deref(), Some("Tone 1"));
            app.handle_media_command(E::SetShuffle(true));
            assert!(app.shuffle);
            app.handle_media_command(E::SetShuffle(true));
            assert!(app.shuffle, "already on: no toggle");
            app.handle_media_command(E::SetShuffle(false));
            app.handle_media_command(E::Stop);
            assert!(app.now_playing.is_none());
            app.tick(); // publishing a stopped player is fine too
        }

        // The queue pane, focused: a cursor, jumping, moving, removing and clearing.
        {
            let titles = |list: &[Track]| list.iter().map(|t| t.display_title()).collect::<Vec<_>>();
            app.focus_pane(Pane::Songs);
            app.select(0);
            app.activate_selected(); // Tone 1; queue Tone 1, Tone 3, Tone 2
            app.toggle_focus();
            assert_eq!((app.focus, app.queue.cursor()), (Pane::Queue, Some(0)), "Tab: songs → queue, on the current");
            app.move_selection(1);
            app.tick();
            assert!(
                matches!(&app.details, Some(Details::Track { details, .. }) if details.title.as_deref() == Some("Tone 3"))
            );

            app.activate_selected();
            assert_eq!((app.queue.current(), app.queue.cursor()), (Some(1), Some(1)), "jumped to Tone 3");
            app.move_entry(1);
            assert_eq!(titles(app.queue.tracks()), ["Tone 1", "Tone 2", "Tone 3"]);
            assert_eq!(titles(app.queue.unshuffled()), titles(app.queue.tracks()), "shuffle off: both orders change");
            assert_eq!((app.queue.current(), app.queue.cursor()), (Some(2), Some(2)), "the playing song moved with it");

            app.move_selection(-2);
            app.delete_selected();
            assert_eq!(
                (titles(app.queue.tracks()), app.queue.current()),
                (vec!["Tone 2".to_string(), "Tone 3".into()], Some(1))
            );
            assert_eq!(titles(app.queue.unshuffled()), ["Tone 2", "Tone 3"]);
            assert_eq!(app.now_playing.as_ref().map(|t| t.display_title()).as_deref(), Some("Tone 3"));
            app.move_selection(1);
            app.delete_selected();
            assert_eq!(app.message.as_deref(), Some("that's the song playing; n skips it"));
            assert_eq!(app.queue.len(), 2);

            app.move_selection(-1);
            app.activate_selected(); // Tone 2, with Tone 3 after it
            app.queue_clear_upcoming();
            assert_eq!(
                (titles(app.queue.tracks()), app.message.as_deref()),
                (vec!["Tone 2".to_string()], Some("cleared 1 upcoming song"))
            );
            app.queue_clear_upcoming();
            assert_eq!(app.message.as_deref(), Some("nothing queued after this song"));

            // With shuffle on, moving only changes the play order; shuffle off restores the list order.
            app.focus_pane(Pane::Songs);
            app.select(0);
            app.activate_selected();
            app.toggle_shuffle();
            app.focus_pane(Pane::Queue);
            app.move_entry(1);
            assert_eq!(app.queue.current(), Some(1), "the playing song moved later");
            assert_eq!(titles(app.queue.unshuffled()), ["Tone 1", "Tone 3", "Tone 2"]);
            app.toggle_shuffle();
            assert_eq!(
                (titles(app.queue.tracks()), app.queue.current()),
                (vec!["Tone 1".to_string(), "Tone 3".into(), "Tone 2".into()], Some(0))
            );

            // The focused queue has the heavy border; a click selects a row, a double-click jumps.
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 26)).unwrap();
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            let row =
                |y: u16| (0..120).map(|x| terminal.backend().buffer()[(x, y)].symbol().to_string()).collect::<String>();
            let q = app.panes.queue;
            assert!(row(q.y).contains('┏') && !row(0).contains('┏'), "{}\n{}", row(0), row(q.y));
            println!("{}", terminal.backend());
            let t0 = Instant::now();
            app.mouse_down(q.x + 5, q.y + 3, t0);
            assert_eq!((app.focus, app.queue.cursor()), (Pane::Queue, Some(2)));
            app.mouse_down(q.x + 5, q.y + 3, t0 + Duration::from_millis(150));
            assert_eq!(app.queue.current(), Some(2), "double-click jumped to Tone 2");
            app.stop();
        }

        // Quitting saves the session; the next start shows the same playlist with the same queue,
        // the song loaded paused at the same spot and selected.
        app.focus_pane(Pane::Songs);
        assert!(matches!(&app.location, Location::Playlist { name, .. } if name == "Mix 2"));
        app.select(1);
        app.activate_selected();
        app.player.seek_by(1);
        std::thread::sleep(Duration::from_millis(200));
        let titles_of = |queue: &[Track]| queue.iter().map(|t| t.display_title()).collect::<Vec<_>>();
        let (queue, before) = (titles_of(app.queue.tracks()), app.player.position());
        app.save_session().unwrap();
        let resumed = App::new(app.root.clone(), db_path.clone(), Player::new().unwrap()).unwrap();
        assert!(matches!(&resumed.location, Location::Playlist { name, .. } if name == "Mix 2"));
        assert_eq!((titles_of(resumed.queue.tracks()), resumed.queue.current()), (queue.clone(), Some(1)));
        assert_eq!(resumed.now_playing.as_ref().map(|t| t.display_title()).as_deref(), Some("Tone 3"));
        assert_eq!(resumed.selected_track().map(|t| t.display_title()).as_deref(), Some("Tone 3"));
        assert!(resumed.player.is_paused(), "loaded paused");
        std::thread::sleep(Duration::from_millis(200));
        let at = resumed.player.position();
        assert!(
            at + Duration::from_millis(300) >= before && at <= before + Duration::from_millis(300),
            "at {at:?}, saved {before:?}"
        );
        assert_eq!(resumed.message.as_deref(), Some("picked up where you left off; Space plays"));
        drop(resumed);

        // The volume is remembered too, and +/- steps stay whole percents.
        app.change_volume(-0.05);
        app.change_volume(-0.05);
        app.change_volume(0.05);
        assert_eq!(app.player.volume(), 0.95);
        let resumed = App::new(app.root.clone(), db_path.clone(), Player::new().unwrap()).unwrap();
        assert_eq!(resumed.player.volume(), 0.95);
        drop(resumed);
        app.set_volume(1.0);

        // A track whose file went missing meanwhile drops out of the restored queue.
        db::set_missing(&app.conn, "tone2.mp3", true).unwrap();
        let resumed = App::new(app.root.clone(), db_path.clone(), Player::new().unwrap()).unwrap();
        assert_eq!(titles_of(resumed.queue.tracks()), ["Tone 1", "Tone 3"]);
        assert_eq!(resumed.now_playing.as_ref().map(|t| t.display_title()).as_deref(), Some("Tone 3"));
        drop(resumed);
        db::set_missing(&app.conn, "tone2.mp3", false).unwrap();

        // After stopping, there's nothing to pick up.
        app.stop();
        app.save_session().unwrap();
        let resumed = App::new(app.root.clone(), db_path.clone(), Player::new().unwrap()).unwrap();
        assert!(resumed.now_playing.is_none() && resumed.queue.len() == 0);
        drop(resumed);
        app.select(1);
        app.activate_selected();

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
