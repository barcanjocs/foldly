//! Draws the three columns (sidebar, songs, queue), the transport bar with the current song
//! above it, the status line, and any open prompt.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Gauge, List, ListItem, Paragraph, Wrap};

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
            ("Tab", "Switch between the sidebar and the songs list"),
            ("Enter  l", "Sidebar: go to its songs. Songs: play, queue the rest"),
            ("Backspace  h", "Back to the sidebar"),
            ("< >", "Move the divider right of the focused pane"),
            ("Mouse", "Click selects, double-click plays, the wheel scrolls"),
            ("", "Drag a divider to resize; click the progress bar to seek"),
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
    // Remembered for mouse hit-testing (dividers, clicks, the wheel).
    app.panes = PaneRects { sidebar, songs, right, transport };

    draw_sidebar(frame, app, sidebar);
    draw_songs(frame, app, songs);
    // The right column is info over queue, halved; on small screens the queue gets it all.
    if frame.area().width >= INFO_MIN_SCREEN_WIDTH && right.height >= INFO_MIN_COLUMN_HEIGHT {
        let [info, queue] = Layout::vertical([Constraint::Percentage(50), Constraint::Fill(1)]).areas(right);
        draw_info(frame, app, info);
        draw_queue(frame, app, queue);
    } else {
        draw_queue(frame, app, right);
    }
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
    // Counts right-aligned: the row is the inner width minus the highlight symbol.
    let row_width = usize::from(area.width.saturating_sub(3));
    let items: Vec<ListItem> = app
        .sidebar
        .iter()
        .map(|item| {
            let count = item.count.to_string();
            let name_width = row_width.saturating_sub(count.chars().count() + 1);
            let name: String = location_name(&item.location).chars().take(name_width).collect();
            let gap = row_width.saturating_sub(name.chars().count() + count.chars().count()).max(1);
            let name = if item.location == Location::AllTracks { Span::raw(name).bold() } else { Span::raw(name) };
            ListItem::new(Line::from(vec![name, Span::raw(" ".repeat(gap)), Span::raw(count).dark_gray()]))
        })
        .collect();
    let (block, highlight) = pane_block(app, Pane::Sidebar, " Library ".into());
    let block = block.title_bottom(Line::from(format!(" {} ", display_path(&app.root))).dark_gray());
    let list = List::new(items).block(block).highlight_style(highlight).highlight_symbol("▌");
    frame.render_stateful_widget(list, area, &mut app.sidebar_state);
}

fn draw_songs(frame: &mut Frame, app: &mut App, area: Rect) {
    let playing = app.now_playing.as_ref().map(|t| t.id);
    let items: Vec<ListItem> = (0..app.visible.len())
        .filter_map(|i| app.track(i))
        .map(|t| {
            let line = if t.missing {
                Line::from(format!("  {}  (missing)", t.label())).dark_gray()
            } else if Some(t.id) == playing {
                Line::from(format!("♪ {}", t.label())).bold().green()
            } else {
                Line::from(format!("  {}", t.label()))
            };
            ListItem::new(line)
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
fn draw_queue(frame: &mut Frame, app: &App, area: Rect) {
    let title = match app.queue_index {
        Some(i) => format!(" Queue {}/{} ", i + 1, app.queue.len()),
        None => " Queue ".to_string(),
    };
    let block = quiet_block(title);
    let Some(current) = app.queue_index else {
        let hint = Line::from("Press Enter on a track to play it and queue the rest of the list.").dark_gray();
        frame.render_widget(Paragraph::new(hint).wrap(ratatui::widgets::Wrap { trim: true }).block(block), area);
        return;
    };

    // Only build the rows that fit; a queue can hold the whole library.
    let rows = area.height.saturating_sub(2) as usize;
    let start = queue_window_start(current, app.queue.len(), rows);
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
