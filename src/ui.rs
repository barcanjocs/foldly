//! Draws the browser, the queue, the transport bar (with the current song above it) and any
//! open prompt.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Gauge, List, ListItem, Paragraph};

use crate::app::{App, Entry, InputPurpose, Location, Mode, Repeat, location_name};

const ACCENT: Color = Color::Cyan;

/// Every key, grouped, for the `?` help popup. Keep in step with `handle_key` in main.rs.
const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Browsing",
        &[
            ("↑ ↓  j k", "Move the selection"),
            ("PgUp PgDn", "Move 10 at a time"),
            ("g G  Home End", "Jump to the top / bottom"),
            ("Enter  l", "Open a folder, or play a track and queue the rest of the list"),
            ("Backspace  h", "Back to the top level"),
            ("u", "Rescan now (changes are picked up automatically)"),
        ],
    ),
    (
        "Filter",
        &[
            ("/", "Filter the list as you type; Enter plays the top match"),
            ("Esc", "Clear the filter"),
            ("android radio", "Words match in any order"),
            ("rhpar", "Letters match fuzzily (Radiohead — Paranoid Android)"),
            ("'word", "Exact match"),
            ("^word  word$", "Starts with / ends with"),
            ("!word", "Exclude matches"),
        ],
    ),
    (
        "Playback",
        &[
            ("Space", "Pause / resume"),
            ("n  p", "Next / previous (previous restarts the track after 3s)"),
            ("← →", "Seek 5 seconds"),
            ("+ -", "Volume"),
            ("z", "Shuffle on / off (the current track keeps playing)"),
            ("r", "Repeat: off → all (restart the queue) → one (loop this track)"),
            ("s", "Stop"),
        ],
    ),
    (
        "Playlists",
        &[
            ("a", "Add the selected track to a playlist"),
            ("A", "Add all tracks shown to a playlist (skips duplicates)"),
            ("N", "New playlist"),
            ("R", "Rename the selected playlist, or the one you're in"),
            ("d", "Remove track from playlist / delete selected playlist"),
            ("J K", "Move a track down / up in the playlist"),
        ],
    ),
    ("General", &[("?", "This help"), ("q  Ctrl-C", "Quit")]),
];

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [main, transport, status] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(3), Constraint::Length(1)]).areas(frame.area());
    let [browser, queue] = Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(main);

    draw_browser(frame, app, browser);
    draw_queue(frame, app, queue);
    draw_transport(frame, app, transport);
    draw_status(frame, app, status);
    draw_popup(frame, app);
}

fn draw_browser(frame: &mut Frame, app: &mut App, area: Rect) {
    let playing = app.now_playing.as_ref().map(|t| t.id);
    let items: Vec<ListItem> = (0..app.visible.len())
        .filter_map(|i| app.entry(i))
        .map(|entry| {
            let line = match entry {
                Entry::Parent => Line::from("../").dark_gray(),
                Entry::Folder { location, count } => Line::from(vec![
                    Span::raw(format!("{}/", location_name(location))).fg(ACCENT),
                    Span::raw(format!("  {count}")).dark_gray(),
                ]),
                Entry::Track(t) if t.missing => Line::from(format!("  {}  (missing)", t.label())).dark_gray(),
                Entry::Track(t) if Some(t.id) == playing => Line::from(format!("♪ {}", t.label())).bold().green(),
                Entry::Track(t) => Line::from(format!("  {}", t.label())),
            };
            ListItem::new(line)
        })
        .collect();

    let title = match &app.location {
        Location::Root => format!(" {} ", display_path(&app.root)),
        other => format!(" {} ", location_name(other)),
    };
    let mut block = Block::bordered().title(title);
    if let Some(scan) = &app.scan_status {
        block = block.title_top(Line::from(format!(" {scan} ")).right_aligned().dark_gray());
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▌");
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

/// How many already-played tracks stay visible above the current one.
const HISTORY_ROWS: usize = 2;

/// The play queue: recent history dimmed, the current track highlighted, then what's next.
fn draw_queue(frame: &mut Frame, app: &App, area: Rect) {
    let title = match app.queue_index {
        Some(i) => format!(" Queue {}/{} ", i + 1, app.queue.len()),
        None => " Queue ".to_string(),
    };
    let block = Block::bordered().title(title);
    let Some(current) = app.queue_index else {
        let hint = Line::from("Press Enter on a track to play it and queue the rest of the list.").dark_gray();
        frame.render_widget(Paragraph::new(hint).wrap(ratatui::widgets::Wrap { trim: true }).block(block), area);
        return;
    };

    // Only build the rows that fit; a queue can hold the whole library.
    let rows = area.height.saturating_sub(2) as usize;
    let start = current.saturating_sub(HISTORY_ROWS);
    let items: Vec<ListItem> = app.queue[start..(start + rows).min(app.queue.len())]
        .iter()
        .enumerate()
        .map(|(offset, track)| {
            let line = match (start + offset).cmp(&current) {
                std::cmp::Ordering::Less => Line::from(format!("  {}", track.label())).dark_gray(),
                std::cmp::Ordering::Equal => Line::from(format!("♪ {}", track.label())).bold().green(),
                std::cmp::Ordering::Greater => Line::from(format!("  {}", track.label())),
            };
            ListItem::new(line)
        })
        .collect();
    frame.render_widget(List::new(items).block(block), area);
}

fn draw_transport(frame: &mut Frame, app: &App, area: Rect) {
    let player = &app.player;
    let position = player.position();
    let duration = app.now_playing.as_ref().and_then(|t| t.duration);

    let icon = if !player.is_active() {
        "■"
    } else if player.is_paused() {
        "⏸"
    } else {
        "▶"
    };
    let total = duration.map_or_else(|| "--:--".to_string(), fmt_time);
    let shuffle = if app.shuffle { "   shuffle" } else { "" };
    let repeat = match app.repeat {
        Repeat::Off => "",
        Repeat::All => "   repeat all",
        Repeat::One => "   repeat one",
    };
    let label =
        format!("{icon} {} / {total}   vol {:.0}%{shuffle}{repeat}", fmt_time(position), player.volume() * 100.0);
    let ratio = match duration {
        Some(d) if !d.is_zero() => (position.as_secs_f64() / d.as_secs_f64()).clamp(0.0, 1.0),
        _ => 0.0,
    };

    // The current song sits in the bar's top border: title, artist, album.
    let now_playing = match &app.now_playing {
        Some(t) => {
            let mut spans = vec![Span::raw(" "), Span::raw(t.title.clone().unwrap_or_else(|| t.label())).bold()];
            if let Some(artist) = &t.artist {
                spans.push(Span::raw(format!(" — {artist}")).fg(ACCENT));
            }
            if let Some(album) = &t.album {
                spans.push(Span::raw(format!(" · {album}")).italic().dark_gray());
            }
            spans.push(Span::raw(" "));
            Line::from(spans)
        }
        None => Line::from(" Nothing playing ").dark_gray(),
    };

    let gauge = Gauge::default()
        .block(Block::bordered().title(now_playing))
        .gauge_style(Style::new().fg(ACCENT).bg(Color::Black))
        .ratio(ratio)
        .label(label);
    frame.render_widget(gauge, area);
}

/// The bottom line: the filter being typed, the latest message, or help for where you are.
fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let line = if matches!(app.mode, Mode::Filter) {
        Line::from(vec![Span::raw("/").fg(ACCENT), Span::raw(app.filter.as_str()), Span::raw("▏")])
    } else if let Some(msg) = &app.message {
        Line::from(msg.as_str()).yellow()
    } else if !app.filter.is_empty() {
        Line::from(format!(" filter: {}  (esc clears)", app.filter)).fg(ACCENT)
    } else {
        let help = match app.location {
            Location::Root => " enter open · N new playlist · R rename · d delete · u rescan · ? help · q quit",
            Location::AllTracks => {
                " enter play · / filter · a add to playlist · A add all · space pause · n/p · z shuffle · r repeat · ? help"
            }
            Location::Playlist { .. } => {
                " enter play · / filter · a/A add · d remove · J/K move · space pause · z/r shuffle/repeat · ? help"
            }
        };
        Line::from(help).dark_gray()
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_popup(frame: &mut Frame, app: &mut App) {
    match &mut app.mode {
        Mode::Normal | Mode::Filter => {}
        Mode::Help { scroll } => draw_help(frame, scroll),
        Mode::Input { purpose, text } => {
            let title = match purpose {
                InputPurpose::NewPlaylist { .. } => " New playlist ",
                InputPurpose::RenamePlaylist(_) => " Rename playlist ",
            };
            let area = centered(frame.area(), 50, 3);
            frame.render_widget(Clear, area);
            let input = Paragraph::new(Line::from(vec![Span::raw(text.as_str()), Span::raw("▏").fg(ACCENT)])).block(
                Block::bordered().title(title).title_bottom(Line::from(" enter save · esc cancel ").dark_gray()),
            );
            frame.render_widget(input, area);
        }
        Mode::PickPlaylist { playlists, state, track_ids, .. } => {
            let mut items = vec![ListItem::new(Line::from("+ New playlist…").fg(ACCENT))];
            items.extend(playlists.iter().map(|p| ListItem::new(format!("{}  ({})", p.name, p.track_count))));
            let area = centered(frame.area(), 50, (items.len() as u16 + 2).min(20));
            frame.render_widget(Clear, area);
            let list = List::new(items)
                .block(
                    Block::bordered()
                        .title(match track_ids.len() {
                            1 => " Add to playlist ".to_string(),
                            n => format!(" Add {n} tracks to playlist "),
                        })
                        .title_bottom(Line::from(" enter add · esc cancel ").dark_gray()),
                )
                .highlight_style(Style::new().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
                .highlight_symbol("▌");
            frame.render_stateful_widget(list, area, state);
        }
        Mode::ConfirmDelete { name, .. } => {
            let area = centered(frame.area(), 50, 3);
            frame.render_widget(Clear, area);
            let text = Line::from(format!("Delete playlist \"{name}\"? Your files are untouched.  y/n"));
            frame.render_widget(Paragraph::new(text).block(Block::bordered().title(" Delete ")), area);
        }
    }
}

fn draw_help(frame: &mut Frame, scroll: &mut u16) {
    const KEY_WIDTH: usize = 15;
    let mut lines = Vec::new();
    for (i, (section, keys)) in HELP.iter().enumerate() {
        if i > 0 {
            lines.push(Line::default());
        }
        lines.push(Line::from(*section).bold().fg(ACCENT));
        for (key, action) in keys.iter() {
            lines.push(Line::from(vec![Span::raw(format!("  {key:<KEY_WIDTH$}")).bold(), Span::raw(*action)]));
        }
    }

    let screen = frame.area();
    let height = (lines.len() as u16 + 2).min(screen.height);
    let area = centered(screen, 80, height);
    // Stop scrolling once the last line is in view.
    let max_scroll = (lines.len() as u16).saturating_sub(height.saturating_sub(2));
    *scroll = (*scroll).min(max_scroll);
    let hint = if max_scroll > 0 { " j/k scroll · any other key closes " } else { " any key closes " };

    frame.render_widget(Clear, area);
    let help = Paragraph::new(lines)
        .scroll((*scroll, 0))
        .block(Block::bordered().title(" Keys ").title_bottom(Line::from(hint).dark_gray()));
    frame.render_widget(help, area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height)]).flex(Flex::Center).areas(area);
    let [cell] = Layout::horizontal([Constraint::Length(width.min(area.width))]).flex(Flex::Center).areas(row);
    cell
}

fn fmt_time(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// Shows the home directory as `~` to keep the title short.
fn display_path(path: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from)
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.display()) };
    }
    path.display().to_string()
}
