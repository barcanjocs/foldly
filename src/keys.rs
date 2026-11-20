//! Every key in normal mode, defined once: `BINDINGS` says which keys trigger which `Action` (for
//! `main.rs` to perform), and `HELP` describes the actions for the `?` popup, taking each row's
//! key labels from `BINDINGS`. So a key can't be bound without being documented, or documented
//! as something it isn't.
//!
//! The same action can appear in several help sections with wording for that pane (e.g. `d` in
//! Playlists and in Queue); its keys are still the bound ones. Prompts and popups (the filter
//! line, the picker, the help itself) handle their own keys in `main.rs`.

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Quit,
    Redraw,
    Help,
    Down,
    Up,
    PageDown,
    PageUp,
    First,
    Last,
    Activate,
    NextPane,
    PaneLeft,
    ToSidebar,
    DividerLeft,
    DividerRight,
    Rescan,
    Filter,
    ClearFilter,
    PlayPause,
    Next,
    Previous,
    SeekBack,
    SeekForward,
    VolumeUp,
    VolumeDown,
    Shuffle,
    Repeat,
    Stop,
    AddToPlaylist,
    AddAllToPlaylist,
    AddPlayingToPlaylist,
    NewPlaylist,
    Rename,
    Delete,
    MoveDown,
    MoveUp,
    ClearUpcoming,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    code: KeyCode,
    ctrl: bool,
}

const fn key(code: KeyCode) -> Key {
    Key { code, ctrl: false }
}

const fn ch(c: char) -> Key {
    key(KeyCode::Char(c))
}

const fn ctrl(c: char) -> Key {
    Key { code: KeyCode::Char(c), ctrl: true }
}

use Action as A;

/// Which keys do what. Ctrl keys work in every mode; the others in normal mode.
pub const BINDINGS: &[(Action, &[Key])] = &[
    (A::Quit, &[ch('q'), ctrl('c')]),
    (A::Redraw, &[ctrl('l')]),
    (A::Help, &[ch('?')]),
    (A::Up, &[key(KeyCode::Up), ch('k')]),
    (A::Down, &[key(KeyCode::Down), ch('j')]),
    (A::PageUp, &[key(KeyCode::PageUp)]),
    (A::PageDown, &[key(KeyCode::PageDown)]),
    (A::First, &[key(KeyCode::Home), ch('g')]),
    (A::Last, &[key(KeyCode::End), ch('G')]),
    (A::Activate, &[key(KeyCode::Enter), ch('l')]),
    (A::NextPane, &[key(KeyCode::Tab)]),
    (A::PaneLeft, &[ch('h')]),
    (A::ToSidebar, &[key(KeyCode::Backspace)]),
    (A::DividerLeft, &[ch('<')]),
    (A::DividerRight, &[ch('>')]),
    (A::Rescan, &[ch('u')]),
    (A::Filter, &[ch('/')]),
    (A::ClearFilter, &[key(KeyCode::Esc)]),
    (A::PlayPause, &[ch(' ')]),
    (A::Next, &[ch('n')]),
    (A::Previous, &[ch('p')]),
    (A::SeekBack, &[key(KeyCode::Left)]),
    (A::SeekForward, &[key(KeyCode::Right)]),
    (A::VolumeUp, &[ch('+'), ch('=')]),
    (A::VolumeDown, &[ch('-')]),
    (A::Shuffle, &[ch('z')]),
    (A::Repeat, &[ch('r')]),
    (A::Stop, &[ch('s')]),
    (A::AddToPlaylist, &[ch('a')]),
    (A::AddAllToPlaylist, &[ch('A')]),
    (A::AddPlayingToPlaylist, &[ch('P')]),
    (A::NewPlaylist, &[ch('N')]),
    (A::Rename, &[ch('R')]),
    (A::Delete, &[ch('d')]),
    (A::MoveDown, &[ch('J')]),
    (A::MoveUp, &[ch('K')]),
    (A::ClearUpcoming, &[ch('c')]),
];

/// The action bound to a key press, if any.
pub fn action_for(code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
    let pressed = Key { code, ctrl: modifiers.contains(KeyModifiers::CONTROL) };
    BINDINGS.iter().find(|(_, keys)| keys.contains(&pressed)).map(|&(action, _)| action)
}

/// One line of the help popup.
pub enum Row {
    /// What these actions do; the keys are taken from `BINDINGS`.
    Keys(&'static [Action], &'static str),
    /// A line that isn't a key: an example, or the mouse.
    Note(&'static str, &'static str),
}

pub struct Section {
    pub title: &'static str,
    pub rows: &'static [Row],
}

use Row::{Keys, Note};

pub const HELP: &[Section] = &[
    Section {
        title: "Browsing",
        rows: &[
            Keys(&[A::Up, A::Down], "Move the selection in the focused pane"),
            Keys(&[A::PageUp, A::PageDown], "Move 10 at a time"),
            Keys(&[A::First, A::Last], "Jump to the top / bottom"),
            Keys(&[A::NextPane], "Focus the next pane: sidebar → songs → queue"),
            Keys(&[A::Activate], "Sidebar: go to its songs. Songs: play, queue the rest"),
            Keys(&[A::PaneLeft], "Focus the pane to the left"),
            Keys(&[A::ToSidebar], "Back to the sidebar"),
            Keys(&[A::DividerLeft, A::DividerRight], "Move the divider right of the focused pane"),
            Keys(&[A::Rescan], "Rescan now (changes are picked up automatically)"),
            Note("Mouse", "Click selects, double-click plays, the wheel scrolls"),
            Note("", "Drag a divider to resize; click or drag the bar to seek"),
        ],
    },
    Section {
        title: "Filter",
        rows: &[
            Keys(&[A::Filter], "Filter the list as you type; Enter plays the top match"),
            Keys(&[A::ClearFilter], "Clear the filter"),
            Note("android radio", "Words match in any order"),
            Note("rhpar", "Letters match fuzzily (Radiohead — Paranoid Android)"),
            Note("'word", "Exact match"),
            Note("^word  word$", "Starts with / ends with"),
            Note("!word", "Exclude matches"),
        ],
    },
    Section {
        title: "Playback",
        rows: &[
            Keys(&[A::PlayPause], "Pause / resume"),
            Keys(&[A::Next, A::Previous], "Next / previous (previous restarts after 3 s)"),
            Keys(&[A::SeekBack, A::SeekForward], "Seek 5 seconds"),
            Keys(&[A::VolumeUp, A::VolumeDown], "Volume (foldly's own; remembered)"),
            Keys(&[A::Shuffle], "Shuffle on / off (the current track keeps playing)"),
            Keys(&[A::Repeat], "Repeat: off → all → one (loop this track)"),
            Keys(&[A::Stop], "Stop"),
        ],
    },
    Section {
        title: "Playlists",
        rows: &[
            Keys(&[A::AddToPlaylist], "Add the selected track to a playlist"),
            Keys(&[A::AddAllToPlaylist], "Add all tracks shown to a playlist (skips duplicates)"),
            Keys(&[A::AddPlayingToPlaylist], "Add the song that's playing to a playlist"),
            Keys(&[A::NewPlaylist], "New playlist"),
            Keys(&[A::Rename], "Rename the playlist being shown"),
            Keys(&[A::Delete], "Songs: remove from playlist. Sidebar: delete playlist"),
            Keys(&[A::MoveDown, A::MoveUp], "Move a track down / up in the playlist"),
        ],
    },
    Section {
        title: "Queue (when focused)",
        rows: &[
            Keys(&[A::Activate], "Jump to the selected song"),
            Keys(&[A::Delete], "Remove it from the queue (n skips the one playing)"),
            Keys(&[A::MoveDown, A::MoveUp], "Play it later / earlier"),
            Keys(&[A::ClearUpcoming], "Clear everything after the current song"),
            Keys(&[A::AddToPlaylist, A::AddAllToPlaylist], "Add it / the whole queue to a playlist"),
        ],
    },
    Section {
        title: "General",
        rows: &[Keys(&[A::Help], "This help"), Keys(&[A::Redraw], "Redraw the screen"), Keys(&[A::Quit], "Quit")],
    },
];

/// The keys of `actions` as shown in the help: an action's keys joined by spaces, actions by two.
pub fn label(actions: &[Action]) -> String {
    actions
        .iter()
        .map(|action| {
            let keys = BINDINGS.iter().find(|(a, _)| a == action).map_or(&[][..], |(_, keys)| keys);
            keys.iter().map(|&k| key_name(k)).collect::<Vec<_>>().join(" ")
        })
        .collect::<Vec<_>>()
        .join("  ")
}

fn key_name(key: Key) -> String {
    let name = match key.code {
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(c) if key.ctrl => c.to_ascii_uppercase().to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Up => "↑".into(),
        KeyCode::Down => "↓".into(),
        KeyCode::Left => "←".into(),
        KeyCode::Right => "→".into(),
        KeyCode::PageUp => "PgUp".into(),
        KeyCode::PageDown => "PgDn".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::Backspace => "Backspace".into(),
        KeyCode::Esc => "Esc".into(),
        other => format!("{other:?}"),
    };
    if key.ctrl { format!("Ctrl-{name}") } else { name }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn no_key_is_bound_twice() {
        let mut seen = HashMap::new();
        for (action, keys) in BINDINGS {
            for key in *keys {
                if let Some(other) = seen.insert(*key, *action) {
                    panic!("{} is bound to both {other:?} and {action:?}", key_name(*key));
                }
            }
        }
    }

    #[test]
    fn every_bound_action_is_in_the_help() {
        let documented: HashSet<Action> = HELP
            .iter()
            .flat_map(|s| s.rows.iter())
            .filter_map(|row| match row {
                Keys(actions, _) => Some(actions.iter().copied()),
                Note(..) => None,
            })
            .flatten()
            .collect();
        for (action, _) in BINDINGS {
            assert!(documented.contains(action), "{action:?} isn't in the help");
        }
        for row in HELP.iter().flat_map(|s| s.rows.iter()) {
            if let Keys(actions, text) = row {
                assert!(!label(actions).is_empty(), "{text:?} names an action with no keys");
            }
        }
    }

    #[test]
    fn keys_are_found_and_labelled() {
        assert_eq!(action_for(KeyCode::Char('j'), KeyModifiers::NONE), Some(Action::Down));
        assert_eq!(action_for(KeyCode::Char('c'), KeyModifiers::CONTROL), Some(Action::Quit));
        assert_eq!(action_for(KeyCode::Char('c'), KeyModifiers::NONE), Some(Action::ClearUpcoming));
        assert_eq!(action_for(KeyCode::Char('x'), KeyModifiers::NONE), None);
        assert_eq!(label(&[Action::Up, Action::Down]), "↑ k  ↓ j");
        assert_eq!(label(&[Action::Quit]), "q Ctrl-C");
        assert_eq!(label(&[Action::PlayPause]), "Space");
    }
}
