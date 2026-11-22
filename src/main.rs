//! foldly: a terminal music player for one music folder, browsed through playlists that act
//! as virtual folders.

mod app;
mod db;
mod filter;
mod keys;
mod media;
mod player;
mod queue;
mod scan;
mod ui;
mod watch;

use std::io::stdout;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;

use app::{App, Mode, Pane};
use keys::Action;
use player::Player;

const ROOT_SETTING: &str = "music_root";

const USAGE: &str = "\
foldly: a terminal music player for one music folder

Usage:
  foldly <MUSIC_FOLDER>   Scan and play this folder (remembered for next time)
  foldly                  Use the folder from last time

Options:
  -h, --help       Show this message
  -V, --version    Show the version

Press ? inside foldly to see every key.";

fn main() -> Result<()> {
    #[cfg(target_os = "macos")]
    return run_off_the_main_thread(run_app);
    #[cfg(not(target_os = "macos"))]
    run_app()
}

/// On macOS, media key and Control Center commands are delivered through the main thread's run
/// loop, so the main thread runs that loop while the app (terminal UI, audio, everything) runs
/// on a second thread. Panics in the app are passed on, so they behave as they would on the
/// main thread.
#[cfg(target_os = "macos")]
fn run_off_the_main_thread(app: fn() -> Result<()>) -> Result<()> {
    use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
    let thread = std::thread::Builder::new().name("foldly".into()).spawn(app)?;
    while !thread.is_finished() {
        // Wakes for each delivered command, else every 100 ms to check the app is still going.
        // SAFETY: reading an immutable CoreFoundation constant.
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, Duration::from_millis(100), true);
    }
    thread.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn run_app() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("-V" | "--version") => {
            println!("foldly {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return Ok(());
        }
        _ => {}
    }

    // FOLDLY_DB lets you keep a separate database, e.g. for testing.
    let db_path = match std::env::var_os("FOLDLY_DB") {
        Some(path) => PathBuf::from(path),
        None => db::default_path()?,
    };
    let root = music_root(&db_path)?;

    // Open audio before taking over the terminal so errors print normally.
    let player = Player::new()?;
    let mut app = App::new(root, db_path, player)?;

    let mut terminal = ratatui::init();
    // Mouse capture lets the divider be dragged. ratatui's panic hook restores the terminal
    // but doesn't know about mouse capture, so turn it off first on a panic too.
    execute!(stdout(), EnableMouseCapture)?;
    let restore_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableMouseCapture);
        restore_hook(info);
    }));
    let result = run(&mut terminal, &mut app);
    let saved = app.save_session();
    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    if let Err(err) = saved {
        eprintln!("foldly: couldn't save where you left off: {err:#}");
    }
    result
}

/// The music folder from the command line (remembered for next time), or the remembered one.
fn music_root(db_path: &std::path::Path) -> Result<PathBuf> {
    let conn = db::open(db_path)?;
    let root = match std::env::args_os().nth(1) {
        Some(arg) => {
            let path = PathBuf::from(&arg);
            let root = path.canonicalize().with_context(|| format!("can't open {}", path.display()))?;
            if !root.is_dir() {
                bail!("{} is not a folder", root.display());
            }
            let as_str = root.to_str().context("the music folder's path must be valid UTF-8")?;
            db::set_setting(&conn, ROOT_SETTING, as_str)?;
            root
        }
        None => match db::get_setting(&conn, ROOT_SETTING)? {
            Some(saved) => PathBuf::from(saved),
            None => bail!("tell foldly where your music is, e.g. `foldly ~/Music` (it's remembered after that)"),
        },
    };
    if !root.is_dir() {
        bail!("music folder {} isn't available; pass a new one, e.g. `foldly ~/Music`", root.display());
    }
    Ok(root)
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.should_quit {
        app.tick();
        if std::mem::take(&mut app.needs_full_redraw) {
            terminal.clear()?;
        }
        terminal.draw(|frame| ui::draw(frame, app))?;

        // Wake at least a few times a second so the progress bar moves.
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => handle_key(app, key),
                Event::Mouse(mouse) => handle_mouse(app, mouse),
                _ => {}
            }
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) {
    // Quit and redraw work everywhere, even in prompts and popups; other Ctrl keys only in
    // normal mode.
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match keys::action_for(key.code, key.modifiers) {
            Some(action @ (Action::Quit | Action::Redraw)) => perform(app, action),
            Some(action) if matches!(app.mode, Mode::Normal) => perform(app, action),
            _ => {}
        }
        return;
    }
    // Any key press dismisses the last message.
    app.message = None;

    match &mut app.mode {
        Mode::Normal => handle_normal_key(app, key.code),
        Mode::Filter => match key.code {
            KeyCode::Esc => {
                app.mode = Mode::Normal;
                app.set_filter(String::new());
            }
            KeyCode::Enter | KeyCode::Down | KeyCode::Up => {
                app.mode = Mode::Normal;
                handle_normal_key(app, key.code);
            }
            KeyCode::Backspace => {
                let mut filter = app.filter.clone();
                filter.pop();
                app.set_filter(filter);
            }
            KeyCode::Char(c) => {
                let filter = format!("{}{c}", app.filter);
                app.set_filter(filter);
            }
            _ => {}
        },
        Mode::Input { text, .. } => match key.code {
            KeyCode::Esc => app.mode = Mode::Normal,
            KeyCode::Enter => app.submit_input(),
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char(c) => text.push(c),
            _ => {}
        },
        Mode::PickPlaylist { playlists, state, .. } => {
            let last = playlists.len(); // item 0 is "New playlist…"
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => app.mode = Mode::Normal,
                KeyCode::Down | KeyCode::Char('j') => state.select(Some((state.selected().unwrap_or(0) + 1).min(last))),
                KeyCode::Up | KeyCode::Char('k') => state.select(Some(state.selected().unwrap_or(0).saturating_sub(1))),
                KeyCode::Enter => app.confirm_pick(),
                _ => {}
            }
        }
        Mode::ConfirmDelete { .. } => match key.code {
            KeyCode::Char('y') => app.confirm_delete(),
            _ => app.mode = Mode::Normal,
        },
        Mode::Help { scroll } => match key.code {
            KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
            KeyCode::PageDown => *scroll = scroll.saturating_add(10),
            KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
            _ => app.mode = Mode::Normal,
        },
    }
}

fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => app.mouse_down(mouse.column, mouse.row, std::time::Instant::now()),
        MouseEventKind::Drag(MouseButton::Left) => app.mouse_drag(mouse.column),
        MouseEventKind::Up(MouseButton::Left) => app.mouse_up(),
        // Capturing the mouse stops terminals turning the wheel into arrow keys, so do it here.
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            let down = mouse.kind == MouseEventKind::ScrollDown;
            match &mut app.mode {
                Mode::Normal | Mode::Filter => app.scroll_at(mouse.column, mouse.row, if down { 3 } else { -3 }),
                Mode::Help { scroll } => {
                    *scroll = if down { scroll.saturating_add(3) } else { scroll.saturating_sub(3) }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

fn handle_normal_key(app: &mut App, code: KeyCode) {
    if let Some(action) = keys::action_for(code, KeyModifiers::NONE) {
        perform(app, action);
    }
}

/// What each key does; which key is which lives in keys.rs.
fn perform(app: &mut App, action: Action) {
    match action {
        Action::Quit => app.should_quit = true,
        Action::Redraw => app.needs_full_redraw = true,
        Action::Help => app.mode = Mode::Help { scroll: 0 },
        Action::Down => app.move_selection(1),
        Action::Up => app.move_selection(-1),
        Action::PageDown => app.move_selection(10),
        Action::PageUp => app.move_selection(-10),
        Action::First => app.select_first(),
        Action::Last => app.select_last(),
        Action::Activate => app.activate_selected(),
        Action::NextPane => app.toggle_focus(),
        Action::PaneLeft => app.focus_left(),
        Action::PaneRight => app.focus_right(),
        Action::ToSidebar => app.focus_pane(Pane::Sidebar),
        Action::DividerLeft => app.resize_focused(-2),
        Action::DividerRight => app.resize_focused(2),
        Action::Rescan => app.rescan(),
        Action::Filter => {
            // The filter always works on the songs list.
            app.focus_pane(Pane::Songs);
            app.mode = Mode::Filter;
        }
        Action::ClearFilter => {
            if !app.filter.is_empty() {
                app.set_filter(String::new());
            }
        }
        Action::PlayPause => app.player.toggle_pause(),
        Action::Next => app.next(),
        Action::Previous => app.previous(),
        Action::SeekBack => app.player.seek_by(-5),
        Action::SeekForward => app.player.seek_by(5),
        Action::VolumeUp => app.change_volume(0.05),
        Action::VolumeDown => app.change_volume(-0.05),
        Action::Shuffle => app.toggle_shuffle(),
        Action::Repeat => app.cycle_repeat(),
        Action::Stop => app.stop(),
        Action::AddToPlaylist => app.start_add_to_playlist(),
        Action::AddAllToPlaylist => app.start_add_all_to_playlist(),
        Action::AddPlayingToPlaylist => app.start_add_playing_to_playlist(),
        Action::NewPlaylist => app.start_new_playlist(),
        Action::Rename => app.start_rename(),
        Action::Delete => app.delete_selected(),
        Action::MoveDown => app.move_entry(1),
        Action::MoveUp => app.move_entry(-1),
        Action::ClearUpcoming => {
            if app.focus == Pane::Queue {
                app.queue_clear_upcoming();
            }
        }
    }
}
