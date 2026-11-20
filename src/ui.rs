//! Draws the three columns (sidebar, songs, queue), the transport bar with the current song
//! above it, the status line, and any open prompt.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Details, InputPurpose, Location, Mode, Pane, PaneRects, Repeat, location_name};

const ACCENT: Color = Color::Cyan;

/// Every key, grouped, for the `?` help popup. Keep in step with `handle_key` in main.rs.
const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Browsing",
        &[
            ("↑ ↓  j k", "Move the selection in the focused pane"),
            ("PgUp PgDn", "Move 10 at a time"),
            ("g G  Home End", "Jump to the top / bottom"),
            ("Tab", "Focus the next pane: sidebar → songs → queue"),
            ("Enter  l", "Sidebar: go to its songs. Songs: play, queue the rest"),
            ("h", "Focus the pane to the left"),
            ("Backspace", "Back to the sidebar"),
            ("< >", "Move the divider right of the focused pane"),
            ("Mouse", "Click selects, double-click plays, the wheel scrolls"),
            ("", "Drag a divider to resize; click or drag the bar to seek"),
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
        "Queue (when focused)",
        &[
            ("Enter  l", "Jump to the selected song"),
            ("d", "Remove it from the queue (n skips the one playing)"),
            ("J K", "Play it later / earlier"),
            ("c", "Clear everything after the current song"),
            ("a  A", "Add it / the whole queue to a playlist"),
        ],
    ),
    (
        "Playlists",
        &[
            ("a", "Add the selected track to a playlist"),
            ("A", "Add all tracks shown to a playlist (skips duplicates)"),
            ("P", "Add the song that's playing to a playlist"),
            ("N", "New playlist"),
            ("R", "Rename the playlist being shown"),
            ("d", "Songs: remove from playlist. Sidebar: delete playlist"),
            ("J K", "Move a track down / up in the playlist"),
        ],
    ),
    ("General", &[("?", "This help"), ("Ctrl-L", "Redraw the screen"), ("q  Ctrl-C", "Quit")]),
];

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [main, transport, status] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(3), Constraint::Length(1)]).areas(frame.area());
    let [sidebar, songs, right] = Layout::horizontal([
        Constraint::Percentage(app.sidebar_width),
        Constraint::Fill(1),
        Constraint::Percentage(app.right_width),
    ])
    .areas(main);
    // The right column is info over queue, halved; on small screens the queue gets it all.
    let (info, queue) = if frame.area().width >= INFO_MIN_SCREEN_WIDTH && right.height >= INFO_MIN_COLUMN_HEIGHT {
        let [info, queue] = Layout::vertical([Constraint::Percentage(50), Constraint::Fill(1)]).areas(right);
        (Some(info), queue)
    } else {
        (None, right)
    };
    // Remembered for mouse hit-testing (dividers, clicks, the wheel).
    app.panes = PaneRects { sidebar, songs, right, queue, transport };

    draw_sidebar(frame, app, sidebar);
    draw_songs(frame, app, songs);
    if let Some(info) = info {
        draw_info(frame, app, info);
    }
    draw_queue(frame, app, queue);
    draw_transport(frame, app, transport);
    draw_status(frame, app, status);
    draw_popup(frame, app);
}

/// A focusable pane's frame and selection style. The focused pane is the only one with a
/// heavy, accented border and a bold accented title; every other pane's border is dimmed, so
/// focus is visible even when the pane is empty. Its selection is filled; the other pane keeps a
/// quieter bold selection so you can see both.
fn pane_block(app: &App, pane: Pane, title: String) -> (Block<'static>, Style) {
    if app.focus == pane {
        let block = Block::bordered()
            .border_type(BorderType::Thick)
            .border_style(Style::new().fg(ACCENT))
            .title(Line::from(title).bold().fg(ACCENT));
        (block, Style::new().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
    } else {
        (quiet_block(title), Style::new().add_modifier(Modifier::BOLD))
    }
}

/// The frame of a pane that isn't focused (or can't be): dimmed border, plain title.
fn quiet_block(title: impl Into<String>) -> Block<'static> {
    Block::bordered().border_style(Style::new().dark_gray()).title(Line::from(title.into()).reset())
}

/// Why the songs list is empty, and what to do about it.
fn empty_songs_message(app: &App) -> Vec<Line<'static>> {
    let dim = |s: String| Line::from(s).dark_gray();
    if !app.filter.is_empty() {
        return vec![
            Line::from(format!("No songs match \u{201c}{}\u{201d}.", app.filter)),
            dim("Esc clears the filter.".into()),
        ];
    }
    match &app.location {
        Location::Playlist { .. } => vec![
            Line::from("This playlist is empty."),
            dim("To add songs, go to All Tracks (or another playlist) and press a for the selected song, or A for every song shown.".into()),
        ],
        Location::AllTracks if app.scan_status.is_some() => vec![Line::from("Scanning your music folder…")],
        Location::AllTracks => vec![
            Line::from("No music found yet."),
            dim(format!("Add MP3, FLAC, OGG, WAV or M4A files to {}; foldly picks them up automatically.", display_path(&app.root))),
        ],
    }
}

fn draw_sidebar(frame: &mut Frame, app: &mut App, area: Rect) {
    let width = list_row_width(area, true);
    let items: Vec<ListItem> = app
        .sidebar
        .iter()
        .map(|item| {
            let (name, gap, count) = fit_row(location_name(&item.location), &item.count.to_string(), width);
            let name = if item.location == Location::AllTracks { Span::raw(name).bold() } else { Span::raw(name) };
            ListItem::new(Line::from(vec![name, Span::raw(gap), Span::raw(count).dark_gray()]))
        })
        .collect();
    let (block, highlight) = pane_block(app, Pane::Sidebar, " Library ".into());
    let block = block.title_bottom(Line::from(format!(" {} ", display_path(&app.root))).dark_gray());
    let list = List::new(items).block(block).highlight_style(highlight).highlight_symbol("▌");
    frame.render_stateful_widget(list, area, &mut app.sidebar_state);
}

fn draw_songs(frame: &mut Frame, app: &mut App, area: Rect) {
    let playing = app.now_playing.as_ref().map(|t| t.id);
    let width = list_row_width(area, true);
    let items: Vec<ListItem> = (0..app.visible.len())
        .filter_map(|i| app.track(i))
        .map(|t| {
            let style = if t.missing {
                Style::new().dark_gray()
            } else if Some(t.id) == playing {
                Style::new().bold().green()
            } else {
                Style::new()
            };
            ListItem::new(track_row(t, Some(t.id) == playing && !t.missing, style, width))
        })
        .collect();

    let (mut block, highlight) = pane_block(app, Pane::Songs, format!(" {} ", location_name(&app.location)));
    if let Some(scan) = &app.scan_status {
        block = block.title_top(Line::from(format!(" {scan} ")).right_aligned().dark_gray());
    }
    if items.is_empty() {
        let message = Paragraph::new(empty_songs_message(app)).wrap(Wrap { trim: true }).block(block);
        frame.render_widget(message, area);
        return;
    }
    let list = List::new(items).block(block).highlight_style(highlight).highlight_symbol("▌");
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

/// Columns available to a list row: inside the border, minus the highlight symbol if the list
/// has one.
fn list_row_width(area: Rect, highlight_symbol: bool) -> usize {
    usize::from(area.width.saturating_sub(2 + u16::from(highlight_symbol)))
}

/// A song row: `♪` when it's the current track, "Artist — Title", and its length (or
/// "missing") right-aligned and dimmed.
fn track_row(t: &crate::db::Track, current: bool, style: Style, width: usize) -> Line<'static> {
    let right = if t.missing { "missing".to_string() } else { t.duration.map(fmt_time).unwrap_or_default() };
    // Room for the label: the row, minus the `♪ ` column and the length with its gap.
    let room = width.saturating_sub(2 + if right.is_empty() { 0 } else { right.width() + 1 });
    let left = format!("{} {}", if current { '♪' } else { ' ' }, fitted_label(t, room));
    let (left, gap, right) = fit_row(&left, &right, width);
    Line::from(vec![Span::styled(left, style), Span::raw(gap), Span::raw(right).dark_gray()])
}

/// "Artist — Title" in at most `room` columns. When it doesn't fit, the artist is shortened
/// first, since the title is what tells songs apart; if not even a few letters of the artist
/// fit, it's dropped and the title alone is cut to fit.
fn fitted_label(t: &crate::db::Track, room: usize) -> String {
    const SEPARATOR: &str = " — ";
    const MIN_ARTIST: usize = 4;
    let title = t.display_title();
    let Some(artist) = &t.artist else { return truncate_to_width(&title, room) };
    let full = format!("{artist}{SEPARATOR}{title}");
    if full.width() <= room {
        return full;
    }
    match room.checked_sub(title.width() + SEPARATOR.width()) {
        Some(artist_room) if artist_room >= MIN_ARTIST => {
            format!("{}{SEPARATOR}{title}", truncate_to_width(artist, artist_room))
        }
        _ => truncate_to_width(&title, room),
    }
}

/// Lays out a row `width` columns wide: `left`, cut with `…` if needed, and `right` against the
/// right edge with at least one space between. Measured in display columns, so wide characters
/// (CJK, emoji) line up. If even a cut-down left side wouldn't leave room, `right` is dropped.
fn fit_row(left: &str, right: &str, width: usize) -> (String, String, String) {
    let right_width = right.width();
    if right.is_empty() || width < right_width + 3 {
        return (truncate_to_width(left, width), String::new(), String::new());
    }
    let left = truncate_to_width(left, width - right_width - 1);
    let gap = width - left.width() - right_width;
    (left, " ".repeat(gap), right.to_string())
}

/// `s` cut to at most `max` display columns, ending in `…` when anything was cut.
fn truncate_to_width(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > max {
            break;
        }
        out.push(c);
        used += w;
    }
    if max > 0 {
        // "Paranoid…", not "Paranoid …".
        out.truncate(out.trim_end().len());
        out.push('…');
    }
    out
}

/// Below these sizes the info pane is hidden and the queue takes the whole right column.
const INFO_MIN_SCREEN_WIDTH: u16 = 100;
const INFO_MIN_COLUMN_HEIGHT: u16 = 16;

/// The selected song's details, or a summary of the selected source.
fn draw_info(frame: &mut Frame, app: &App, area: Rect) {
    let block = quiet_block(" Selected ");
    let dim = |s: String| Line::from(s).dark_gray();
    let lines = match &app.details {
        Some(Details::Track { details: d, also_in }) => {
            let fallback = std::path::Path::new(&d.rel_path).file_stem().map(|s| s.to_string_lossy().into_owned());
            let mut lines = vec![Line::from(d.title.clone().or(fallback).unwrap_or_default()).bold()];
            if let Some(artist) = &d.artist {
                lines.push(Line::from(artist.as_str()).fg(ACCENT));
            }
            if let Some(album) = &d.album {
                lines.push(Line::from(album.as_str()).italic());
            }
            if d.missing {
                lines.push(Line::from("Missing from the music folder").red());
            }
            // Most useful first, no spacer lines: the pane is half a column and the end may be cut.
            let numbers = [
                d.track_no.map(|n| format!("Track {n}")),
                d.disc_no.map(|n| format!("Disc {n}")),
                d.duration.map(fmt_time),
            ];
            let when_what = [d.year.map(|y| y.to_string()), d.genre.clone()];
            for row in [&numbers[..], &when_what[..]] {
                let row: Vec<&str> = row.iter().flatten().map(String::as_str).collect();
                if !row.is_empty() {
                    lines.push(Line::from(row.join(" · ")));
                }
            }
            lines.push(match also_in.as_slice() {
                [] => dim("In no other playlists".into()),
                names => Line::from(vec![Span::raw("Also in: ").dark_gray(), Span::raw(names.join(", "))]),
            });
            let quality: Vec<String> = [
                d.format.clone(),
                d.bitrate_kbps.map(|b| format!("{b} kbps")),
                d.sample_rate.map(fmt_sample_rate),
                d.bit_depth.map(|b| format!("{b}-bit")),
                d.channels.map(|c| match c {
                    1 => "mono".to_string(),
                    2 => "stereo".to_string(),
                    n => format!("{n} channels"),
                }),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !quality.is_empty() {
                lines.push(dim(quality.join(" · ")));
            }
            lines.push(dim(format!("{} · {}", d.rel_path, fmt_size(d.size))));
            lines
        }
        Some(Details::Source { tracks, duration, unknown, missing }) => {
            let mut summary = format!("{tracks} {}", if *tracks == 1 { "track" } else { "tracks" });
            if *tracks > *unknown {
                summary.push_str(&format!(" · {}", fmt_time(*duration)));
            }
            let mut lines = vec![Line::from(location_name(&app.location).to_string()).bold(), Line::from(summary)];
            if *unknown > 0 {
                lines.push(dim(format!("{unknown} without a known length")));
            }
            if *missing > 0 {
                lines.push(Line::from(format!("{missing} missing from the music folder")).red());
            }
            lines
        }
        None => vec![dim("Nothing selected".into())],
    };
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block), area);
}

/// 44100 → "44.1 kHz", 48000 → "48 kHz".
fn fmt_sample_rate(hz: u32) -> String {
    let khz = format!("{:.1}", f64::from(hz) / 1000.0);
    format!("{} kHz", khz.strip_suffix(".0").unwrap_or(&khz))
}

fn fmt_size(bytes: i64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 1.0 { format!("{mb:.1} MB") } else { format!("{} KB", (bytes + 999) / 1000) }
}

/// Most upcoming tracks kept visible below the current one once the queue scrolls.
const MAX_LOOKAHEAD: usize = 5;

/// First queue row to show: from the top until the current track nears the bottom, then
/// scrolled just enough to keep a few upcoming tracks in view, never past a full last page.
pub fn queue_window_start(current: usize, len: usize, rows: usize) -> usize {
    let lookahead = (rows / 3).min(MAX_LOOKAHEAD);
    (current + lookahead + 1).saturating_sub(rows).min(len.saturating_sub(rows))
}

/// The play queue: played tracks dimmed, the current track highlighted, then what's next.
fn draw_queue(frame: &mut Frame, app: &mut App, area: Rect) {
    let title = match app.queue.current() {
        Some(i) => format!(" Queue {}/{} ", i + 1, app.queue.len()),
        None => " Queue ".to_string(),
    };
    let (block, highlight) = pane_block(app, Pane::Queue, title);
    let focused = app.focus == Pane::Queue;
    let Some(current) = app.queue.current() else {
        let hint = Line::from("Press Enter on a track to play it and queue the rest of the list.").dark_gray();
        frame.render_widget(Paragraph::new(hint).wrap(ratatui::widgets::Wrap { trim: true }).block(block), area);
        return;
    };

    // Only build the rows that fit; a queue can hold the whole library.
    let rows = area.height.saturating_sub(2) as usize;
    let mut start = queue_window_start(current, app.queue.len(), rows);
    // When focused, the selection may wander off the automatic window: scroll to keep it in view.
    let cursor = app.queue.cursor().filter(|_| focused);
    if let Some(cursor) = cursor {
        if cursor < start {
            start = cursor;
        } else if rows > 0 && cursor >= start + rows {
            start = cursor + 1 - rows;
        }
    }
    app.queue_offset = start;
    let width = list_row_width(area, focused);
    let items: Vec<ListItem> = app.queue.tracks()[start..(start + rows).min(app.queue.len())]
        .iter()
        .enumerate()
        .map(|(offset, track)| {
            let (style, current) = match (start + offset).cmp(&current) {
                std::cmp::Ordering::Less => (Style::new().dark_gray(), false),
                std::cmp::Ordering::Equal => (Style::new().bold().green(), true),
                std::cmp::Ordering::Greater => (Style::new(), false),
            };
            ListItem::new(track_row(track, current, style, width))
        })
        .collect();
    let list = List::new(items).block(block);
    match cursor {
        Some(cursor) => {
            // The rows are already the visible slice, so the selection is relative to it.
            let mut state = ListState::default().with_selected(Some(cursor - start));
            frame.render_stateful_widget(list.highlight_style(highlight).highlight_symbol("▌"), area, &mut state);
        }
        None => frame.render_widget(list, area),
    }
}

fn draw_transport(frame: &mut Frame, app: &App, area: Rect) {
    let player = &app.player;
    // While scrubbing, the bar shows where the drag would land.
    let position = app.scrub.unwrap_or_else(|| player.position());
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
        let help = match (app.focus, &app.location) {
            (Pane::Sidebar, Location::AllTracks) => {
                " j/k choose · enter songs · N new playlist · A add all to playlist · u rescan · ? help · q quit"
            }
            (Pane::Sidebar, Location::Playlist { .. }) => {
                " j/k choose · enter songs · N new · R rename · d delete playlist · A add all · ? help · q quit"
            }
            (Pane::Songs, Location::AllTracks) => {
                " enter play · / filter · a/A add to playlist · tab sidebar · space pause · n/p · z/r · ? help"
            }
            (Pane::Queue, _) => {
                " enter jump to · d remove · J/K move · c clear upcoming · a/A add to playlist · tab sidebar · ? help"
            }
            (Pane::Songs, Location::Playlist { .. }) => {
                " enter play · / filter · a/A add · d remove · J/K move · tab sidebar · space · z/r · ? help"
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

#[cfg(test)]
mod tests {
    use super::queue_window_start;

    #[test]
    fn labels_shorten_the_artist_before_the_title() {
        use super::fitted_label;
        let t = crate::db::Track {
            id: 1,
            rel_path: "x.mp3".into(),
            title: Some("Paranoid Android".into()),
            artist: Some("Radiohead".into()),
            album: None,
            duration: None,
            missing: false,
            entry_id: None,
        };
        assert_eq!(fitted_label(&t, 40), "Radiohead — Paranoid Android");
        assert_eq!(fitted_label(&t, 25), "Radio… — Paranoid Android", "artist cut first");
        assert_eq!(fitted_label(&t, 20), "Paranoid Android", "no room for 4 artist columns: title alone");
        assert_eq!(fitted_label(&t, 10), "Paranoid…");
        let untagged = crate::db::Track { title: None, artist: None, rel_path: "dir/My Song.mp3".into(), ..t };
        assert_eq!(fitted_label(&untagged, 40), "My Song");
    }

    #[test]
    fn rows_fit_their_width() {
        use super::{fit_row, truncate_to_width};
        use unicode_width::UnicodeWidthStr;
        let joined = |(l, g, r): (String, String, String)| format!("{l}{g}{r}");

        assert_eq!(joined(fit_row("Radiohead — Airbag", "4:44", 30)), "Radiohead — Airbag        4:44");
        assert_eq!(joined(fit_row("Radiohead — Paranoid Android", "06:23", 20)), "Radiohead — P… 06:23");
        // Wide characters take two columns each and still line up.
        let row = joined(fit_row("坂本龍一 — Merry Christmas", "05:12", 20));
        assert_eq!((row.width(), row.as_str()), (20, "坂本龍一 — Me… 05:12"));
        // Too narrow for the right side: just the cut left side.
        assert_eq!(joined(fit_row("Radiohead", "06:23", 6)), "Radio…");
        assert_eq!(joined(fit_row("Radiohead", "", 20)), "Radiohead");
        // A wide character never gets split.
        assert_eq!(truncate_to_width("日本語", 4), "日…");
        assert_eq!(truncate_to_width("abc", 0), "");
    }

    #[test]
    fn sample_rates() {
        assert_eq!(super::fmt_sample_rate(44_100), "44.1 kHz");
        assert_eq!(super::fmt_sample_rate(48_000), "48 kHz");
        assert_eq!(super::fmt_sample_rate(96_000), "96 kHz");
        assert_eq!(super::fmt_sample_rate(22_050), "22.1 kHz");
    }

    #[test]
    fn queue_scrolls_only_when_needed() {
        // 15 visible rows: lookahead 5. Nothing scrolls while current + 5 fits.
        assert_eq!(queue_window_start(0, 100, 15), 0);
        assert_eq!(queue_window_start(9, 100, 15), 0, "rows 10..14 still show what's next");
        assert_eq!(queue_window_start(10, 100, 15), 1, "then scroll one row at a time");
        assert_eq!(queue_window_start(50, 100, 15), 41);
        // Near the end the last page stays full instead of leaving empty rows.
        assert_eq!(queue_window_start(98, 100, 15), 85);
        assert_eq!(queue_window_start(99, 100, 15), 85);
        // A queue shorter than the panel never scrolls.
        assert_eq!(queue_window_start(7, 8, 15), 0);
        // Tiny panels still keep the current track visible.
        assert_eq!(queue_window_start(5, 100, 1), 5);
        assert_eq!(queue_window_start(5, 100, 0), 6);
    }
}
