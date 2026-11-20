//! End-to-end tests: a real `App` with a real audio device, on a folder of three short tagged
//! MP3s. They're `#[ignore]`d because they need both; see DEVELOPMENT.md for making the
//! fixtures and running them:
//!
//!     FOLDLY_TEST_DIR=fixtures cargo test -- --ignored
//!
//! Each test gets its own throwaway database through `Harness`, so they run independently.
//! The pure rules (queue bookkeeping, filtering, layout maths) are unit-tested in their own
//! modules; these check that the pieces work together.

use std::ops::{Deref, DerefMut};
use std::path::Path;

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;

/// The fixture folder: `tone1.mp3`..`tone3.mp3`, titled "Tone 1".."Tone 3", artist "Test Artist",
/// album "Album A", year 1997, genre "Test Genre".
fn fixtures() -> PathBuf {
    let dir = std::env::var("FOLDLY_TEST_DIR").expect("set FOLDLY_TEST_DIR to the fixture folder (see DEVELOPMENT.md)");
    PathBuf::from(dir).canonicalize().unwrap()
}

/// A scanned `App` on the fixtures with its own database, removed afterwards.
struct Harness {
    app: App,
    db: PathBuf,
}

impl Harness {
    fn new(name: &str) -> Self {
        Self::on(fixtures(), name)
    }

    fn on(root: PathBuf, name: &str) -> Self {
        let db = std::env::temp_dir().join(format!("foldly-e2e-{name}-{}.db", std::process::id()));
        remove_db(&db);
        let mut app = App::new(root, db.clone(), Player::new().unwrap()).unwrap();
        while app.scan_rx.is_some() {
            app.tick();
            std::thread::sleep(Duration::from_millis(20));
        }
        Harness { app, db }
    }

    /// With a playlist "Mix" of Tone 1, Tone 3, Tone 2 (in that order), shown in the focused
    /// songs list.
    fn with_mix(name: &str) -> Self {
        let mut h = Self::new(name);
        let id = |title: &str| {
            db::all_tracks(&h.conn).unwrap().into_iter().find(|t| t.title.as_deref() == Some(title)).unwrap().id
        };
        let tracks = [id("Tone 1"), id("Tone 3"), id("Tone 2")];
        let mix = db::create_playlist(&h.conn, "Mix").unwrap();
        db::add_tracks(&h.conn, mix, &tracks, false).unwrap();
        h.reload().unwrap();
        h.select_source(1);
        h.focus_pane(Pane::Songs);
        h
    }

    /// A second `App` on the same database, as the next run of foldly would be.
    fn reopen(&self) -> App {
        App::new(self.root.clone(), self.db.clone(), Player::new().unwrap()).unwrap()
    }

    /// Plays the song at `index` of the songs list.
    fn play(&mut self, index: usize) {
        self.focus_pane(Pane::Songs);
        self.select(index);
        self.activate_selected();
    }
}

impl Deref for Harness {
    type Target = App;
    fn deref(&self) -> &App {
        &self.app
    }
}

impl DerefMut for Harness {
    fn deref_mut(&mut self) -> &mut App {
        &mut self.app
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        remove_db(&self.db);
    }
}

fn remove_db(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

/// The songs list's titles, as shown.
fn titles(app: &App) -> Vec<String> {
    (0..app.visible.len()).filter_map(|i| Some(app.track(i)?.display_title())).collect()
}

fn names(list: &[Track]) -> Vec<String> {
    list.iter().map(|t| t.display_title()).collect()
}

fn playing(app: &App) -> Option<String> {
    app.now_playing.as_ref().map(|t| t.display_title())
}

fn is_playlist(location: &Location, expected: &str) -> bool {
    matches!(location, Location::Playlist { name, .. } if name == expected)
}

/// Draws one frame at `width`×`height`.
fn draw(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
    terminal
}

fn screen(app: &mut App, width: u16, height: u16) -> String {
    format!("{}", draw(app, width, height).backend())
}

fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect()
}

/// Ticks until `done` or `seconds` pass; returns whether it got done.
fn tick_until(app: &mut App, seconds: u64, mut done: impl FnMut(&App) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        app.tick();
        if done(app) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
#[ignore]
fn browsing_filtering_and_editing_playlists() {
    let mut h = Harness::new("browsing");

    // A fresh start: full volume, the sidebar with just All Tracks, shown in the focused songs list.
    assert_eq!(h.player.volume(), 1.0);
    assert_eq!(h.sidebar.len(), 1);
    assert_eq!((h.location.clone(), h.sidebar[0].count, h.visible.len()), (Location::AllTracks, 3, 3));
    assert_eq!(h.focus, Pane::Songs);

    // Filter to Tone 3 and add it to a new playlist; the songs list stays on All Tracks.
    h.set_filter("tone 3".into());
    assert_eq!(h.visible.len(), 1);
    h.start_add_to_playlist();
    h.confirm_pick(); // item 0: new playlist
    let tone3 = h.selected_track().unwrap().id;
    h.mode = Mode::Input { purpose: InputPurpose::NewPlaylist { then_add: vec![tone3] }, text: "Mix".into() };
    h.submit_input();
    assert_eq!(h.message.as_deref(), Some("added 1 track to Mix"));
    assert_eq!((h.sidebar.len(), h.location.clone()), (2, Location::AllTracks));
    h.set_filter(String::new());

    // Add Tone 1 through the picker's existing playlist row.
    h.select(0);
    h.start_add_to_playlist();
    if let Mode::PickPlaylist { state, .. } = &mut h.mode {
        state.select(Some(1));
    }
    h.confirm_pick();
    assert_eq!(h.message.as_deref(), Some("added 1 track to Mix"));

    // Bulk add everything by the artist: Tone 1 and 3 are already there, so only Tone 2 is added.
    h.set_filter("'test artist".into());
    h.start_add_all_to_playlist();
    match &mut h.mode {
        Mode::PickPlaylist { track_ids, state, .. } => {
            assert_eq!(track_ids.len(), 3);
            state.select(Some(1));
        }
        _ => panic!("picker should be open"),
    }
    h.confirm_pick();
    assert_eq!(h.message.as_deref(), Some("added 1 track to Mix (2 already there)"));
    h.set_filter(String::new());

    // Moving through the sidebar previews each source; Enter moves into its songs.
    h.focus_pane(Pane::Sidebar);
    h.move_selection(1);
    assert!(is_playlist(&h.location, "Mix"));
    assert_eq!(h.sidebar[1].count, 3);
    assert_eq!(titles(&h), ["Tone 3", "Tone 1", "Tone 2"], "playlist order");
    h.move_selection(-1);
    assert_eq!(h.location, Location::AllTracks);
    h.move_selection(1);
    h.activate_selected();
    assert_eq!(h.focus, Pane::Songs);

    // Reorder, then rename from the songs list; the sidebar follows.
    h.select(1);
    h.move_entry(-1);
    assert_eq!(titles(&h), ["Tone 1", "Tone 3", "Tone 2"]);
    h.start_rename();
    if let Mode::Input { text, .. } = &mut h.mode {
        *text = "Mix 2".into();
    }
    h.submit_input();
    assert!(is_playlist(&h.location, "Mix 2") && is_playlist(&h.sidebar[1].location, "Mix 2"));

    // A new empty playlist is shown straight away, says why it's empty, and has the only heavy
    // border; deleting it from the sidebar falls back to the playlist now in its place.
    h.start_new_playlist();
    if let Mode::Input { text, .. } = &mut h.mode {
        *text = "Empty".into();
    }
    h.submit_input();
    assert!(is_playlist(&h.location, "Empty"));
    assert_eq!((h.sidebar.len(), h.visible.len()), (3, 0));
    let frame = draw(&mut h, 120, 26);
    let top = row(&frame, 0);
    assert!(format!("{}", frame.backend()).contains("This playlist is empty."));
    assert_eq!(top.matches('┏').count(), 1, "one heavy border: {top}");
    assert!(top.find('┏').unwrap() > top.find('┌').unwrap(), "the songs pane's, not the sidebar's: {top}");
    h.set_filter("zzz".into());
    assert!(screen(&mut h, 120, 26).contains("No songs match"));
    h.set_filter(String::new());
    h.focus_pane(Pane::Sidebar);
    assert!(row(&draw(&mut h, 120, 26), 0).starts_with('┏'), "focus moved to the sidebar");
    h.delete_selected();
    assert!(matches!(h.mode, Mode::ConfirmDelete { .. }));
    h.confirm_delete();
    assert_eq!(h.sidebar.len(), 2);
    assert!(is_playlist(&h.location, "Mix 2"));

    // P adds the playing song (not the selection), and offers the playlist being shown.
    h.play(0);
    h.select(2);
    h.start_add_playing_to_playlist();
    match &h.mode {
        Mode::PickPlaylist { track_ids, playlists, .. } => {
            assert_eq!(track_ids, &[h.now_playing.as_ref().unwrap().id]);
            assert!(playlists.iter().any(|p| p.name == "Mix 2"), "the shown playlist is offered");
        }
        _ => panic!("picker should be open"),
    }
}

#[test]
#[ignore]
fn the_info_pane_describes_the_selection() {
    let mut h = Harness::with_mix("info");

    // In All Tracks: the selected song's tags, quality and the playlists it's in.
    h.focus_pane(Pane::Sidebar);
    h.select_source(0);
    h.focus_pane(Pane::Songs);
    h.select(2);
    h.tick();
    match &h.details {
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
    let shown = screen(&mut h, 120, 26);
    println!("{shown}");
    assert!(shown.contains("Selected") && shown.contains("Also in: Mix") && shown.contains("Track 3"));
    assert!(shown.contains("1997 · Test Genre") && shown.contains("MP3 ·") && shown.contains("44.1 kHz"));
    assert!(!screen(&mut h, 90, 26).contains("Selected"), "hidden on narrow screens");

    // With the sidebar focused it summarises the source.
    h.focus_pane(Pane::Sidebar);
    h.select_source(1);
    h.tick();
    match &h.details {
        Some(Details::Source { tracks, duration, unknown, missing }) => {
            assert_eq!((*tracks, *unknown, *missing), (3, 0, 0));
            assert!(duration.as_secs_f64() > 8.5, "{duration:?}");
        }
        _ => panic!("expected a source summary"),
    }

    // Inside Mix, a song isn't in any *other* playlist.
    h.focus_pane(Pane::Songs);
    h.tick();
    assert!(matches!(&h.details, Some(Details::Track { also_in, .. }) if also_in.is_empty()));
}

#[test]
#[ignore]
fn playback_shuffle_and_repeat() {
    let mut h = Harness::with_mix("playback");

    // Playing queues the rest of the list, and the next song starts when one ends.
    h.play(0);
    assert_eq!(playing(&h).as_deref(), Some("Tone 1"));
    h.player.seek_by(2);
    assert!(tick_until(&mut h, 5, |app| app.queue.current() == Some(1)), "auto-advanced");
    assert_eq!(playing(&h).as_deref(), Some("Tone 3"));

    // Shuffle on mid-track keeps the current song first; off restores the list order.
    let ordered = names(h.queue.tracks());
    h.toggle_shuffle();
    assert_eq!((h.queue.current(), names(&h.queue.tracks()[..1])), (Some(0), vec!["Tone 3".to_string()]));
    h.toggle_shuffle();
    assert_eq!(names(h.queue.tracks()), ordered);
    assert_eq!(names(&h.queue.tracks()[h.queue.current().unwrap()..][..1]), ["Tone 3"]);

    // Repeat one: when the song ends it plays again.
    h.cycle_repeat();
    h.cycle_repeat();
    assert_eq!(h.repeat, Repeat::One);
    let index = h.queue.current();
    h.player.seek_by(2);
    std::thread::sleep(Duration::from_millis(100));
    let mut previous = h.player.position();
    let restarted = tick_until(&mut h, 3, |app| {
        let now = app.player.position();
        let back = now < previous;
        previous = now;
        back
    });
    assert!(restarted, "the song should restart");
    assert_eq!(h.queue.current(), index);

    // Repeat all: next past the last song wraps to the first, previous wraps back.
    h.cycle_repeat();
    h.cycle_repeat();
    assert_eq!(h.repeat, Repeat::All);
    let last = h.queue.len() - 1;
    h.play_index(last);
    h.next();
    assert_eq!(h.queue.current(), Some(0));
    h.previous();
    assert_eq!(h.queue.current(), Some(last));

    // Repeat off: next past the end stops.
    h.cycle_repeat();
    h.cycle_repeat();
    assert_eq!(h.repeat, Repeat::Off);
    h.next();
    assert_eq!(h.queue.current(), None);
}

#[test]
#[ignore]
fn audio_errors_are_handled() {
    let mut h = Harness::with_mix("audio-errors");
    h.play(1);
    h.player.seek_by(1);
    std::thread::sleep(Duration::from_millis(200));
    h.message = None;

    // A rerouted stream (the system output changed) is ignored.
    h.player.inject_error(rodio::cpal::ErrorKind::DeviceChanged);
    h.tick();
    assert!(h.message.is_none() && !h.needs_full_redraw);

    // A dead device is reopened and the song resumes where it was, paused if it was paused.
    let (index, before) = (h.queue.current(), h.player.position());
    h.player.toggle_pause();
    h.player.inject_error(rodio::cpal::ErrorKind::DeviceNotAvailable);
    h.tick();
    assert_eq!(h.message.as_deref(), Some("audio output changed; reconnected to the default device"));
    assert!(h.needs_full_redraw);
    assert_eq!(h.queue.current(), index, "same song");
    assert!(h.player.is_paused(), "still paused");
    let after = h.player.position();
    assert!(after + Duration::from_millis(300) >= before, "resumed at {after:?}, was {before:?}");
    h.player.toggle_pause();
    std::thread::sleep(Duration::from_millis(300));
    assert!(h.player.position() > after, "playing again on the new device");
}

#[test]
#[ignore]
fn mouse_clicks_seeking_and_resizing() {
    let mut h = Harness::with_mix("mouse");
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    h.play(1); // Tone 3
    h.player.seek_by(1);
    std::thread::sleep(Duration::from_millis(300));

    // At 100 columns: sidebar 20%, songs the rest, right column 30%.
    draw(&mut h, 100, 10);
    assert_eq!((h.panes.sidebar.width, h.panes.songs.x, h.panes.right.x), (20, 20, 70));

    // Clicking a sidebar row focuses it and shows that source; the wheel scrolls the pane under
    // the pointer; clicking a song selects it.
    h.mouse_down(5, 1, at(0));
    assert_eq!((h.focus, h.location.clone()), (Pane::Sidebar, Location::AllTracks));
    h.scroll_at(5, 3, 3);
    assert!(is_playlist(&h.location, "Mix"), "clamped to the last");
    h.mouse_down(40, 2, at(1000));
    assert_eq!((h.focus, h.list_state.selected()), (Pane::Songs, Some(1)));

    // A slow second click on the same song only selects; a quick one plays it from the start.
    h.mouse_down(40, 2, at(2000));
    assert!(h.player.position() > Duration::from_millis(800), "not a double-click");
    h.mouse_down(40, 2, at(2200));
    assert_eq!(playing(&h).as_deref(), Some("Tone 3"));
    assert!(h.player.position() < Duration::from_millis(500), "restarted: {:?}", h.player.position());

    // Clicking the middle of the progress bar seeks about half way. At 100×10 the bar's border
    // is rows 6..9 and its inside columns 1..=98.
    assert_eq!(h.panes.transport, Rect::new(0, 6, 100, 3));
    h.mouse_down(50, 7, at(5000));
    h.mouse_up();
    std::thread::sleep(Duration::from_millis(100));
    let position = h.player.position().as_secs_f64();
    assert!((1.3..2.0).contains(&position), "seeked to {position}s of 3s");

    // Dragging along the bar previews the spot (the bar shows it) and seeks once, on release.
    h.mouse_down(25, 7, at(5500));
    h.mouse_drag(75);
    let preview = h.scrub.expect("scrubbing").as_secs_f64();
    assert!((2.1..2.4).contains(&preview), "previewing {preview}s");
    assert!(screen(&mut h, 100, 10).contains("00:02 / 00:03"), "the bar shows the preview");
    h.mouse_drag(200);
    assert_eq!(h.scrub, h.now_playing.as_ref().unwrap().duration, "past the end: the end");
    h.mouse_drag(75);
    h.mouse_up();
    assert!(h.scrub.is_none());
    std::thread::sleep(Duration::from_millis(100));
    let position = h.player.position().as_secs_f64();
    assert!((2.1..2.8).contains(&position), "seeked to {position}s on release");
    h.mouse_drag(60);
    assert_eq!(h.sidebar_width, 20, "a drag without a grabbed divider changes nothing");

    // Drag both dividers; nothing is saved until the button is released.
    h.mouse_down(20, 2, at(6000));
    h.mouse_drag(30);
    assert_eq!(h.sidebar_width, 30);
    assert_eq!(db::get_setting(&h.conn, "sidebar_width").unwrap(), None, "not saved mid-drag");
    h.mouse_up();
    assert_eq!(db::get_setting(&h.conn, "sidebar_width").unwrap().as_deref(), Some("30"));
    h.mouse_down(70, 2, at(7000));
    h.mouse_drag(60);
    h.mouse_up();
    assert_eq!(h.right_width, 40);
    draw(&mut h, 100, 10);
    assert_eq!((h.panes.songs.x, h.panes.right.x), (30, 60), "columns follow the drops");

    // < and > move the divider right of the focused pane; the songs list keeps its minimum.
    h.focus_pane(Pane::Songs);
    h.resize_focused(2);
    assert_eq!(h.right_width, 38);
    h.focus_pane(Pane::Sidebar);
    h.resize_focused(100);
    assert_eq!(h.sidebar_width, 37, "clamped so the songs list keeps 25%");
    let reopened = h.reopen();
    assert_eq!((reopened.sidebar_width, reopened.right_width), (37, 38), "remembered");
}

#[test]
#[ignore]
fn media_commands_drive_playback() {
    use souvlaki::{MediaControlEvent as E, MediaPosition, SeekDirection};
    let mut h = Harness::with_mix("media");
    h.play(0); // Tone 1; queue Tone 1, Tone 3, Tone 2

    h.handle_media_command(E::Toggle);
    assert!(h.player.is_paused());
    h.handle_media_command(E::Pause);
    assert!(h.player.is_paused(), "pause while paused stays paused");
    h.handle_media_command(E::Play);
    assert!(!h.player.is_paused());
    h.handle_media_command(E::Play);
    assert!(!h.player.is_paused(), "play while playing keeps playing");

    h.handle_media_command(E::SetPosition(MediaPosition(Duration::from_millis(1500))));
    std::thread::sleep(Duration::from_millis(100));
    assert!((1.4..2.0).contains(&h.player.position().as_secs_f64()), "{:?}", h.player.position());
    h.handle_media_command(E::SeekBy(SeekDirection::Backward, Duration::from_secs(1)));
    std::thread::sleep(Duration::from_millis(100));
    assert!(h.player.position() < Duration::from_millis(1200), "{:?}", h.player.position());

    h.handle_media_command(E::SetVolume(0.5));
    assert_eq!(h.player.volume(), 0.5);
    h.handle_media_command(E::Next);
    assert_eq!(playing(&h).as_deref(), Some("Tone 3"));
    h.handle_media_command(E::Previous);
    assert_eq!(playing(&h).as_deref(), Some("Tone 1"));
    h.handle_media_command(E::SetShuffle(true));
    assert!(h.shuffle);
    h.handle_media_command(E::SetShuffle(true));
    assert!(h.shuffle, "already on: no toggle");
    h.handle_media_command(E::SetShuffle(false));
    h.handle_media_command(E::Stop);
    assert!(h.now_playing.is_none());
    h.tick(); // publishing a stopped player is fine too
}

#[test]
#[ignore]
fn the_queue_pane_edits_the_queue() {
    let mut h = Harness::with_mix("queue");
    h.play(0); // Tone 1; queue Tone 1, Tone 3, Tone 2

    // Tab: songs → queue, with the cursor on the current song; the info pane follows the cursor.
    h.toggle_focus();
    assert_eq!((h.focus, h.queue.cursor()), (Pane::Queue, Some(0)));
    h.move_selection(1);
    h.tick();
    assert!(matches!(&h.details, Some(Details::Track { details, .. }) if details.title.as_deref() == Some("Tone 3")));

    // Jump, then move the playing song later: with shuffle off both orders change.
    h.activate_selected();
    assert_eq!((h.queue.current(), h.queue.cursor()), (Some(1), Some(1)), "jumped to Tone 3");
    h.move_entry(1);
    assert_eq!(names(h.queue.tracks()), ["Tone 1", "Tone 2", "Tone 3"]);
    assert_eq!(names(h.queue.unshuffled()), names(h.queue.tracks()));
    assert_eq!((h.queue.current(), h.queue.cursor()), (Some(2), Some(2)), "the playing song moved with it");

    // Remove a song before the current one; the playing one can't be removed.
    h.move_selection(-2);
    h.delete_selected();
    assert_eq!(
        (names(h.queue.tracks()), h.queue.current()),
        (vec!["Tone 2".to_string(), "Tone 3".to_string()], Some(1))
    );
    assert_eq!(playing(&h).as_deref(), Some("Tone 3"));
    h.move_selection(1);
    h.delete_selected();
    assert_eq!(h.message.as_deref(), Some("that's the song playing; n skips it"));
    assert_eq!(h.queue.len(), 2);

    // Clear what's upcoming.
    h.move_selection(-1);
    h.activate_selected(); // Tone 2, with Tone 3 after it
    h.queue_clear_upcoming();
    assert_eq!(
        (names(h.queue.tracks()), h.message.as_deref()),
        (vec!["Tone 2".to_string()], Some("cleared 1 upcoming song"))
    );
    h.queue_clear_upcoming();
    assert_eq!(h.message.as_deref(), Some("nothing queued after this song"));

    // The focused queue has the heavy border; a click selects a row, a double-click jumps.
    h.play(0);
    let frame = draw(&mut h, 120, 26);
    let q = h.panes.queue;
    h.focus_pane(Pane::Queue);
    let frame_focused = draw(&mut h, 120, 26);
    assert!(!row(&frame, q.y).contains('┏') && row(&frame_focused, q.y).contains('┏'));
    assert!(!row(&frame_focused, 0).contains('┏'), "only the queue's");
    println!("{}", frame_focused.backend());
    let t0 = Instant::now();
    h.mouse_down(q.x + 5, q.y + 3, t0);
    assert_eq!((h.focus, h.queue.cursor()), (Pane::Queue, Some(2)));
    h.mouse_down(q.x + 5, q.y + 3, t0 + Duration::from_millis(150));
    assert_eq!(h.queue.current(), Some(2), "double-click jumped to Tone 2");
}

#[test]
#[ignore]
fn resuming_where_you_left_off() {
    let mut h = Harness::with_mix("resume");

    // Quitting saves the session; the next start shows the same playlist with the same queue,
    // the song loaded paused at the same spot and selected.
    h.play(1); // Tone 3
    h.player.seek_by(1);
    std::thread::sleep(Duration::from_millis(200));
    let (queue, before) = (names(h.queue.tracks()), h.player.position());
    h.save_session().unwrap();
    let resumed = h.reopen();
    assert!(is_playlist(&resumed.location, "Mix"));
    assert_eq!((names(resumed.queue.tracks()), resumed.queue.current()), (queue.clone(), Some(1)));
    assert_eq!(playing(&resumed).as_deref(), Some("Tone 3"));
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

    // The volume is remembered, in whole percents.
    h.change_volume(-0.05);
    h.change_volume(-0.05);
    h.change_volume(0.05);
    assert_eq!(h.player.volume(), 0.95);
    assert_eq!(h.reopen().player.volume(), 0.95);

    // A song whose file went missing meanwhile drops out of the restored queue.
    db::set_missing(&h.conn, "tone2.mp3", true).unwrap();
    let resumed = h.reopen();
    assert_eq!(names(resumed.queue.tracks()), ["Tone 1", "Tone 3"]);
    assert_eq!(playing(&resumed).as_deref(), Some("Tone 3"));
    drop(resumed);
    db::set_missing(&h.conn, "tone2.mp3", false).unwrap();

    // After stopping, there's nothing to pick up.
    h.stop();
    h.save_session().unwrap();
    let resumed = h.reopen();
    assert!(resumed.now_playing.is_none() && resumed.queue.len() == 0);
}

#[test]
#[ignore]
fn the_help_popup_fits_or_scrolls() {
    let mut h = Harness::new("help");
    // On a 120×40 terminal it fits in two columns, so there's nothing to scroll.
    h.mode = Mode::Help { scroll: 0 };
    let wide = screen(&mut h, 120, 40);
    println!("{wide}");
    assert!(wide.contains("any key closes") && !wide.contains("j/k scroll"), "fits without scrolling");
    assert!(wide.contains("Browsing") && wide.contains("General") && wide.contains("Ctrl-C"));
    // Narrower, one column.
    println!("{}", screen(&mut h, 100, 40));
    h.mode = Mode::Help { scroll: 999 };
    let short = screen(&mut h, 100, 12);
    println!("{short}");
    assert!(short.contains("Quit"), "scrolled to the end");
    assert!(matches!(h.mode, Mode::Help { scroll } if scroll < 999), "clamped");
}

#[test]
#[ignore]
fn picks_up_music_added_while_running() {
    // On a copy of the fixtures, so files can be added and removed.
    let base = std::env::temp_dir().join(format!("foldly-e2e-watch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (root, outside) = (base.join("music"), base.join("outside"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(outside.join("New Album")).unwrap();
    for i in 1..=3 {
        std::fs::copy(fixtures().join(format!("tone{i}.mp3")), root.join(format!("tone{i}.mp3"))).unwrap();
    }
    let mut h = Harness::on(root.canonicalize().unwrap(), "watch");
    let tracks_once_settled = |h: &mut Harness, expected: usize| {
        tick_until(h, 15, |app| app.scan_rx.is_none() && db::track_count(&app.conn).unwrap() == expected);
        db::track_count(&h.conn).unwrap()
    };
    assert_eq!(tracks_once_settled(&mut h, 3), 3);

    // A file dropped into the folder.
    std::fs::copy(fixtures().join("tone1.mp3"), root.join("dropped.mp3")).unwrap();
    assert_eq!(tracks_once_settled(&mut h, 4), 4);

    // A whole album folder moved in: only the folder's rename is reported.
    std::fs::copy(fixtures().join("tone2.mp3"), outside.join("New Album/01.mp3")).unwrap();
    std::fs::copy(fixtures().join("tone3.mp3"), outside.join("New Album/02.mp3")).unwrap();
    std::fs::rename(outside.join("New Album"), root.join("New Album")).unwrap();
    assert_eq!(tracks_once_settled(&mut h, 6), 6);

    // A deleted file becomes missing.
    std::fs::remove_file(root.join("dropped.mp3")).unwrap();
    assert_eq!(tracks_once_settled(&mut h, 5), 5);

    drop(h);
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
#[ignore]
fn key_presses_go_through_the_key_table() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut h = Harness::with_mix("keys");
    let press = |h: &mut Harness, code: KeyCode| crate::handle_key(h, KeyEvent::new(code, KeyModifiers::NONE));
    let typed = |h: &mut Harness, text: &str| text.chars().for_each(|c| press(h, KeyCode::Char(c)));

    // Filter, Enter plays the top match; Space pauses; Tab cycles focus; ? opens help, which any
    // other key closes; Esc clears a filter; Ctrl-L asks for a redraw; q quits.
    press(&mut h, KeyCode::Char('/'));
    typed(&mut h, "tone 2");
    press(&mut h, KeyCode::Enter);
    assert_eq!(playing(&h).as_deref(), Some("Tone 2"));
    press(&mut h, KeyCode::Char(' '));
    assert!(h.player.is_paused());
    press(&mut h, KeyCode::Tab);
    assert_eq!(h.focus, Pane::Queue);
    press(&mut h, KeyCode::Char('h'));
    assert_eq!(h.focus, Pane::Songs);
    press(&mut h, KeyCode::Char('?'));
    assert!(matches!(h.mode, Mode::Help { .. }));
    press(&mut h, KeyCode::Char('x'));
    assert!(matches!(h.mode, Mode::Normal));
    press(&mut h, KeyCode::Esc);
    assert!(h.filter.is_empty());
    press(&mut h, KeyCode::Char('+'));
    assert_eq!(h.player.volume(), 1.05);
    crate::handle_key(&mut h, KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    assert!(h.needs_full_redraw);
    press(&mut h, KeyCode::Char('q'));
    assert!(h.should_quit);
}
