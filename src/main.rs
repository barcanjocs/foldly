//! foldly: a terminal music player for one music folder, browsed through playlists that act
//! as virtual folders.

mod app;
mod db;
mod filter;
mod player;
mod scan;
mod ui;
mod watch;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use app::{App, Mode};
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
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
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
        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            handle_key(app, key);
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => app.should_quit = true,
            KeyCode::Char('l') => app.needs_full_redraw = true,
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

fn handle_normal_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('?') => app.mode = Mode::Help { scroll: 0 },
        KeyCode::Esc if !app.filter.is_empty() => app.set_filter(String::new()),

        KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
        KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
        KeyCode::PageDown => app.move_selection(10),
        KeyCode::PageUp => app.move_selection(-10),
        KeyCode::Home | KeyCode::Char('g') => app.select_first(),
        KeyCode::End | KeyCode::Char('G') => app.select_last(),
        KeyCode::Enter | KeyCode::Char('l') => app.activate_selected(),
        KeyCode::Backspace | KeyCode::Char('h') => app.go_up(),
        KeyCode::Char('/') => app.mode = Mode::Filter,

        KeyCode::Char(' ') => app.player.toggle_pause(),
        KeyCode::Char('n') => app.next(),
        KeyCode::Char('p') => app.previous(),
        KeyCode::Char('s') => app.stop(),
        KeyCode::Char('z') => app.toggle_shuffle(),
        KeyCode::Right => app.player.seek_by(5),
        KeyCode::Left => app.player.seek_by(-5),
        KeyCode::Char('+') | KeyCode::Char('=') => app.player.change_volume(0.05),
        KeyCode::Char('-') => app.player.change_volume(-0.05),

        KeyCode::Char('a') => app.start_add_to_playlist(),
        KeyCode::Char('A') => app.start_add_all_to_playlist(),
        KeyCode::Char('N') => app.start_new_playlist(),
        KeyCode::Char('R') => app.start_rename(),
        KeyCode::Char('d') => app.delete_selected(),
        KeyCode::Char('K') => app.move_entry(-1),
        KeyCode::Char('J') => app.move_entry(1),
        KeyCode::Char('r') => app.cycle_repeat(),
        KeyCode::Char('u') => app.rescan(),
        _ => {}
    }
}
