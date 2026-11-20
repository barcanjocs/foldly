# Implementation details

Developer reference for how foldly works internally. Read this before changing
code. User-facing behavior (keys, data model, backup) lives in `README.md`;
current state, the backlog, and working conventions live in `DEVELOPMENT.md`.
This document explains *how* and *why*; keep it in sync when architecture
changes.

## Big picture

Single binary, no network. One SQLite file holds the track cache, playlists
and settings; the music folder is only ever read. Two threads touch the
database: the UI thread, and a scanner thread that exists only while a scan
runs. rodio runs its own audio thread, notify its own watcher thread, and
souvlaki (media keys) its own on Linux. On macOS the whole app runs on a
second thread, because the main thread must run the system run loop that
delivers media key commands (see "Media keys and Now Playing").

```
main.rs        args → music root → Player + App → loop: tick → draw → poll(200 ms) → key
  └─ App       all UI state, the queue, a db Connection       (app.rs)
       ├─ db      all SQL; tracks, playlists, settings          (db.rs)
       ├─ scan    background scanner thread → ScanEvent channel (scan.rs)
       ├─ Watch   folder watcher (notify) → "rescan due"        (watch.rs)
       ├─ Filter  fuzzy matcher (nucleo)                        (filter.rs)
       └─ Player  rodio output device + current track           (player.rs)
ui.rs          draw(frame, &mut App): sidebar | songs | queue, transport, status, popups
```

The loop runs at ~5 fps: `event::poll` times out after 200 ms so the progress
bar moves without key presses. Each iteration first calls `App::tick`, which
drains watcher events (starting a rescan once changes settle), auto-advances
when a track ends and drains scan events, then draws, then handles at most one
key. Only key *press* events are handled.

Data flow per frame is **state → pixels**: `ui::draw` reads `App` and never
touches the db. It takes `&mut App` only for widget state (the lists'
`ListState` scroll offsets, clamping the help popup's scroll, and recording
the column rects in `App::panes` for the mouse).

## Module map

- **`main.rs`** — handles `--help`/`--version` before anything else, resolves
  the music root (CLI argument, saved to `settings`, else the saved one), opens
  the audio device *before* entering the TUI so those errors print normally,
  then runs the loop. `handle_key` routes by `Mode`; in normal mode keys go
  through `keys::action_for` to `perform`, an exhaustive `match` on `Action`.
- **`keys.rs`** — the key table. `BINDINGS` maps each `Action` to its keys
  (Ctrl keys work in every mode, the rest in normal mode); `HELP` lists the
  help popup's sections as rows of actions (or notes, for examples and the
  mouse), labelled from `BINDINGS` by `label`. An action can appear in
  several sections with wording for that pane. Three things keep it honest:
  `perform` must handle every `Action` (the compiler), every `Action` must be
  constructed somewhere, i.e. bound (`-D warnings` dead-code), and a test
  checks every bound action is documented and no key is bound twice.
- **`app.rs`** — `App` and its types: `Location`, `Entry`, `Mode`,
  `InputPurpose`, `Repeat`. Listing, filtering, navigation, playlist actions,
  prompts, queue, shuffle, repeat and scan polling. Tests at the bottom.
- **`db.rs`** — schema (`const SCHEMA`, applied with `execute_batch` on every
  open, so opening also migrates), `Track`/`Playlist`/`ScannedFile`, and every
  query. Tests at the bottom.
- **`scan.rs`** — `spawn` starts the scanner thread; `run` walks the folder and
  updates the db; `read_file` reads tags with lofty. Test at the bottom.
- **`watch.rs`** — `Watch`: a notify watcher on the music root, the filter
  for which events matter, and the settle timer. Tests at the bottom.
- **`filter.rs`** — `Filter`, a reusable nucleo `Matcher` with one method,
  `matches(query, haystacks, rank)`. Tests at the bottom.
- **`queue.rs`** — `Queue`: play order, list order, current track and cursor,
  with every edit that keeps them in step, and the session encoding. Tests
  at the bottom.
- **`media.rs`** — `NowPlaying`: souvlaki's `MediaControls`, the channel its
  commands arrive on, and change-only publishing of what's playing.
- **`player.rs`** — `Player` wraps rodio: the device sink plus at most one
  playing rodio `Player`.
- **`ui.rs`** — every draw function and colours (`ACCENT`); `draw_help`
  renders `keys::HELP`.

## Data layer (`db.rs`)

### Schema

Four tables; see README.md for the full DDL.

- `tracks` — one row per audio file ever seen: `rel_path` (unique, relative
  to the music root), `size` + `mtime` (the change stamp), tags (title,
  artist, album, track/disc number, year, genre), audio properties
  (`duration_ms`, `format`, `bitrate_kbps`, `sample_rate`, `bit_depth`,
  `channels`), and `missing`.
- `playlists` — `name` is `UNIQUE COLLATE NOCASE`, so "Mix" and "mix" can't
  coexist; `create_playlist`/`rename_playlist` turn the constraint error into
  "a playlist named … already exists".
- `playlist_tracks` — entries with their own `id` (the *entry id*), so the same
  track can appear twice and each occurrence can be removed or moved on its
  own. Order is `position`, then `id`. Both foreign keys cascade
  (`PRAGMA foreign_keys` is on).
- `settings` — key/value: `music_root`, `shuffle` (`"1"`/`"0"`), `repeat`
  (`off`/`all`/`one`), `volume` (0.0–1.5), `sidebar_width` and `right_width`
  (percent), and the
  saved session (`session_location`, `session_queue`, `session_unshuffled`,
  `session_index`, `session_position_ms`; see "Resuming").

### Migrations

`SCHEMA` is the original (v0.1.0) layout, applied with `CREATE … IF NOT
EXISTS`; `MIGRATIONS` is an append-only list of SQL batches on top of it.
SQLite's `user_version` counts how many have run. `migrate` runs each pending
one in an `IMMEDIATE` transaction that re-reads `user_version` after taking
the write lock, so the UI and scanner connections opening together can't both
apply one. A fresh database goes through the same path (schema, then every
migration), so there is only one way to reach the current layout.

A migration that adds columns filled from the files ends with
`UPDATE tracks SET size = -1`: no file has that size, so the next scan sees
every file as changed and re-reads it (migration 1 does this for year, genre
and the audio properties). Never edit a migration that has shipped; add one.

### Missing, not deleted

The scanner never deletes rows. A file that disappears gets `missing = 1`: it
drops out of `all_tracks` and `track_count` (both filter `missing = 0`) but
still joins into `playlist_tracks`, where the UI greys it out. When a file
reappears at the same `rel_path`, the scanner flips `missing` back. This is
why playlists survive unplugged drives and moved music roots (paths are
relative, so a moved root matches the same rows).

### Playlist positions

`add_to_playlist` appends at `max(position) + 1` in a single
`INSERT … SELECT`. `remove_entry` leaves gaps, which is fine because only
order matters. `swap_entries` swaps two entries' positions in a transaction;
that's all `J`/`K` needs. `add_tracks` wraps many appends in one transaction
and optionally skips tracks already present (`skip_existing`, used by `A`).

### Two connections

The UI thread and the scanner thread each open their own `Connection` to the
same file. `db::open` sets `journal_mode = WAL` (readers don't block the
writer) and a 5 s `busy_timeout` (the UI's playlist writes wait out a scan
batch instead of failing).

### Data directory

`dirs::data_dir()/foldly/foldly.db`, created on first use. `FOLDLY_DB`
overrides the path; tests pass a temp path (or `:memory:`) directly to
`db::open`.

## Scanning (`scan.rs`)

`App::rescan` calls `scan::spawn(db_path, root)`, which returns a
`Receiver<ScanEvent>`; a second rescan is ignored while one runs. The thread:

1. Walks the root with `walkdir` (following symlinks, skipping any entry whose
   name starts with `.` below the root), keeping files with an extension in
   `AUDIO_EXTENSIONS`. Paths that aren't valid UTF-8 are skipped, because
   `rel_path` must round-trip through SQLite text.
2. Loads every known `(rel_path → size, mtime, missing)` with `file_stamps`.
3. Processes files in batches of 200, each in its own transaction, sending
   `Progress` after each commit so the UI can show new tracks during a long
   first scan. A file whose stamp matches is skipped (or un-missed); anything
   else is read with lofty and upserted.
4. Marks every known, not-yet-missing path that wasn't seen as missing.
5. Sends `Finished { changed, missing }`, or `Failed` on any error.

`App::poll_scan` (called from `tick`) drains the channel without blocking,
updates `scan_status` (drawn in the songs list's top-right border), reloads the
current list after progress, and drops the receiver when the scan ends.

### Watching (`watch.rs`)

`App::new` starts a `Watch` on the root (`notify::recommended_watcher`:
FSEvents on macOS, inotify on Linux, recursive). If that fails (e.g. Linux's
inotify watch limit) the error goes to the message line and `u` remains the
only way to rescan.

The watcher sends events to an `mpsc` channel. `App::poll_watch`, from `tick`,
calls `Watch::poll`, which drains the channel and records when the last
*relevant* event arrived; then, if no scan is running, `take_settled` returns
`true` once that is `SETTLE` (2 s) in the past and `App` starts a rescan.
Changes during a scan stay pending until it ends, then trigger another scan,
so nothing is lost. Recording the drain time rather than the event's time is
fine at 5 ticks/sec.

`is_relevant` ignores reads, the root itself, anything under a hidden name,
and existing non-audio files (cover art, notes). It accepts audio files,
folders being created, removed or renamed (a folder moved in is reported only
as a rename), and paths that no longer exist (a removed file or folder, whose
type can't be checked any more). Data/metadata changes on folders are
ignored: the files inside report themselves. Watcher errors (such as an
inotify queue overflow) count as relevant, since changes may have been missed.
Because rescans are incremental and run off-thread, a rare unnecessary one
costs a directory walk and nothing else.

Tag reading (`read_file`) uses the file's primary tag, falling back to the
first tag; empty or whitespace-only values become `None`. The year comes
from lofty's `date()` (recording date, else year). Audio properties come from
`properties()`; `format_name` maps lofty's `FileType` to familiar names
(`Mpeg` → MP3). A file lofty can't
parse is still recorded, with no tags, so it shows by file name and the
decoder gets a chance at playback.

## App state (`app.rs`)

```rust
pub struct App {
    root, db_path, conn,              // music folder, db path (for the scanner), UI connection
    focus: Pane,                      // Sidebar | Songs: which pane keys act on
    sidebar: Vec<SidebarItem>,        // All Tracks, then playlists, each with a count
    sidebar_state: ListState,
    location: Location,               // AllTracks | Playlist { id, name }: the sidebar's selection
    tracks: Vec<Track>,               // the songs of `location`
    haystacks: Vec<Utf32String>,      // filter text, one per track
    fuzzy: Filter,
    visible: Vec<usize>,              // indices into tracks, in display order
    filter: String,
    list_state: ListState,            // songs selection, an index into `visible`
    mode: Mode,                       // which prompt/popup is open
    sidebar_width, right_width,       // column widths in percent; songs get the rest
    panes: PaneRects, dragging,       // last frame's column rects; the divider being dragged
    queue: Queue, queue_offset,       // see "Queue" below; first queue row drawn, for clicks
    shuffle, repeat, now_playing, player,
    message, scan_status, scan_rx, should_quit,
}
```

**The UI mirrors the db.** `load_sidebar` and `load_tracks(location)` query
the db; `set_tracks` stores the songs together with their haystacks. `reload`
re-reads both after any change (playlist edits, scan progress) and keeps both
selections: the sidebar finds the shown source again by identity
(`Location::same_source`, which ignores a playlist's name so renames are
picked up), and if it was deleted, shows the source now at its position. Draw
code only reads these fields.

### Sidebar, songs and focus

**The sidebar's selection is always the source the songs list shows.** Moving
in the sidebar calls `select_source`, which (when the index changes) runs
`show_source`: set `location`, clear the filter, load its tracks, select the
first. That's the instant preview; it's a single indexed query, cheap even for
All Tracks.

`focus: Pane` decides what keys act on. `move_selection`, `select_first`/`last`
dispatch on it (`move_in` takes the pane explicitly, for the mouse wheel).
`Enter` in the sidebar only moves focus to the songs list; in the songs list it
plays. Pane-dependent actions read `focus`: `d` deletes the playlist from the
sidebar and removes an entry from the songs list; `a` and `J`/`K` need the
songs list. `R` and `A` work from either, since both panes refer to the same
`location`. `/` always focuses the songs list. A new empty playlist (`N`) is
shown immediately by setting `location` before `reload`.

Track identity within a list is `(track id, entry id)` (`queue::key`): the entry
id is `None` in All Tracks and the `playlist_tracks.id` inside a playlist,
which is what distinguishes two occurrences of the same song.

### The info pane

`App::details: Option<Details>` is either `Details::Track { details, also_in }`
or `Details::Source { tracks, duration, unknown, missing }`. Draw only reads
it; `refresh_details`, called at the end of every `tick`, recomputes it when
its `DetailsKey` (focus, source, selected `(track id, entry id)`) differs from
the last one, and `reload` clears the key, since playlist membership, counts
and tags may have changed. So details cost two small queries per selection
change, never per frame.

- Songs list focused with a selection: `db::track_details` (one row) plus
  `db::playlists_containing` (distinct, alphabetical), minus the playlist
  being shown, so "Also in" means *other* playlists.
- Otherwise (sidebar focused, or nothing selected): `source_summary`, built
  from the already-loaded `tracks` with no query. Durations are summed over
  tracks that have one; the rest are counted as `unknown`.

### Filtering

`set_tracks` precomputes one haystack per track: `"<label> <album>"` (label
is "Artist — Title" or the file stem). `apply_filter` then shows everything
for an empty filter, else runs `Filter::matches`, ranked by score in
`AllTracks` and in original order in playlists (so a filtered playlist still
reads as the playlist). The filter only ever applies to the songs list.

`set_filter` also selects the first row, so `Enter` from filter mode plays the
best match (filter mode hands `Enter`/`↑`/`↓` to
`handle_normal_key` after leaving the mode). `move_entry` refuses to reorder
while filtering, since neighbours in the filtered view aren't neighbours in the
playlist.

`Filter::matches` parses the query with `Pattern::parse(…, CaseMatching::Ignore,
Normalization::Smart)`: whitespace splits atoms, every atom must match, atoms
are fuzzy unless prefixed (`'`, `^`, `$`, `!`). `Matcher` is created once and
reused; on 30k tracks a keystroke costs 1–40 ms in release builds.

### Modes

```rust
enum Mode {
    Normal,
    Filter,                                          // typing into the filter line
    Input { purpose: InputPurpose, text: String },   // new / rename playlist
    PickPlaylist { track_ids, bulk, playlists, state },
    ConfirmDelete { playlist_id, name },
    Help { scroll },
}
```

**Prompt state lives inside the `Mode` variant**: opening a prompt builds it,
closing sets `Mode::Normal` and the state is dropped. `InputPurpose::NewPlaylist
{ then_add }` carries the tracks to add once the playlist exists, which is how
the picker's "New playlist…" row chains into the name prompt.

`PickPlaylist.bulk` is set by `A`: bulk adds skip tracks already in the
playlist, single adds (`a`, `P`) don't. `open_picker` leaves out the playlist
being shown for tracks picked from it (`a`, `A`), but not for `P`, whose
playing song may well not be in it. Row 0 of the picker is always "New playlist…"; the initial selection is
row 1 when playlists exist.

### Key dispatch and borrows

`handle_key` (main.rs) does three things before dispatching: `Ctrl-C` quits
from any mode, only presses are handled, and the message line is cleared.
Then it matches `&mut app.mode`. Arms that edit prompt state do it through the
binding (`text.push(c)`, `*scroll += 1`, `state.select(…)`); arms that need
`&mut App` methods either bind nothing (`Mode::Filter =>`) or use the binding
only before the call, so the borrow has ended. Methods that consume prompt
state (`confirm_pick`, `submit_input`, `confirm_delete`) take it out with
`std::mem::replace(&mut self.mode, Mode::Normal)` and then use owned values,
which avoids holding a borrow of `self.mode` across `&mut self` calls.

### Errors and messages

`message: Option<String>` is the bottom line's content when set. `report`
turns an `Err` into a message; successful actions may set one ("added 3
tracks to Mix"). Any key press clears it. Startup is the exception: a missing
music folder, an unopenable db or audio device exits with the error printed,
before the TUI starts.

## Queue, shuffle, repeat

### The queue (`queue.rs`)

`Queue` holds:

```
tracks:     the play order
unshuffled: the same tracks in the order of the list Enter was pressed in
current:    where the playing (or loaded) track is in `tracks`
selected:   the queue pane's cursor; None = "on the current track"
```

**Invariant:** `unshuffled` holds exactly the tracks of `tracks`; with shuffle
off, in the same order. Turning shuffle off restores `tracks` from
`unshuffled`, so every operation keeps the two in step, and they all live in
`queue.rs` where they're unit-tested without an audio device. Tracks are
identified by `queue::key`, `(track id, entry id)`: unique within a queue,
since one is built from one list.

- **`start(list, chosen, shuffle)`** (Enter on a song) replaces both orders.
  Shuffled, `chosen` comes first and the rest are `fastrand::shuffle`d.
  Returns the index to play.
- **`set_shuffle(on)`** keeps the current track current: on, it reshuffles
  around it (current becomes 0); off, it restores the list order at its
  position. **`reshuffle`** is repeat-all's fresh order for the next round.
- **The cursor**: `cursor()` is `selected` if in range, else `current`.
  Every rebuild (`start`, `set_shuffle`, `reshuffle`, `restore`) clears
  `selected`, so the cursor goes back to following the current track;
  `follow_current` does the same after a jump.
- **`remove_selected`** takes the track out of both orders (by index, and by
  key in `unshuffled`), moving `current` down if the row was before it.
  Removing the current track is refused (`RemoveError::Playing`), so
  `current` always points at what's playing.
- **`move_selected(delta, shuffle)`** swaps with the neighbour in `tracks`,
  and in `unshuffled` too when shuffle is off (same order, same indices); with
  shuffle on only the play order changes. `current` follows the current track
  if it was one of the pair.
- **`clear_upcoming`** truncates after the current track and keeps only the
  remaining keys in `unshuffled`.
- **`encode` / `restore`** turn both orders into `id` or `id:entry`
  comma-separated strings and back (for "Resuming"); `restore` drops tracks
  its `find` can't supply and reports where the saved current track ended up,
  leaving `current` unset until `App` has loaded it.

### Playing it (`app.rs`)

`Queue` doesn't touch audio; `App` asks it what to play and tells it what's
playing with `set_current`.

- **`next`** plays `current + 1`; past the end, repeat-all reshuffles (if
  shuffling) and starts at 0, otherwise `play_index` runs off the end and
  stops. **`previous`** restarts the track after 3 s, else goes back one, and
  wraps to the end under repeat-all.
- **Auto-advance** is `tick`: when `player.finished()`, repeat-one replays
  the current index, everything else calls `next`.
- **`play_index(i)`** tries track `i`, `i+1`, … until one decodes, then sets
  `now_playing` (with the decoder's duration if the db has none) and the
  queue's current index. If none decode, it stops (current becomes `None`;
  the tracks stay).

The queue is a snapshot: editing a playlist doesn't change what's queued.

### Editing the queue (`Pane::Queue`)

The queue is the third focusable pane (`Tab` cycles sidebar → songs → queue,
`h` is `focus_left`). Keys and clicks map onto the `Queue` operations above:
`j`/`k` move the cursor, `Enter`/double-click jump (`play_index(cursor)` then
`follow_current`), `d` removes, `J`/`K` move, `c` clears upcoming.

The pane-dependent actions route on `focus`: `activate_selected`,
`delete_selected`, `move_entry`, `start_add_to_playlist` (queue: the cursor's
track, offering the shown playlist too) and `start_add_all_to_playlist`
(queue: the whole queue). `refresh_details` describes the cursor's track
while the queue is focused. `<`/`>` from the queue move the songs|right
divider, the one on its left.

### Resuming

`App::save_session` writes, in one transaction: `session_location` (`all` or
`playlist:<id>`), and, if a track is loaded, `session_index`,
`session_position_ms`, and the queue in play and original order as
`Queue::encode` strings (`id` or `id:entry`, comma-separated). The queue can be
the whole library, so it's only rewritten when its encoding differs from
`saved_queue`, the last one written; comparing every save is cheaper than
tracking every place the queue changes. With nothing loaded (stopped), the
queue keys are deleted. It runs from `main.rs` after the loop ends (errors
printed after the terminal is restored) and from `tick` every
`SESSION_SAVE_INTERVAL` (15 s) while a track is loaded, so a closed terminal
or a crash loses little.

`restore_session`, at the end of `App::new` (before the first rescan), shows
the saved source (All Tracks if the playlist is gone), rebuilds the queue
with `Queue::restore` from `db::tracks_by_id` (every track, present or
missing), dropping tracks that are gone or missing and restoring entry ids,
and gets back where the saved current track is in it. If it's there, it
is loaded with `Player::load_paused` at the saved position and selected in
the songs list; if not, the queue is kept with nothing loaded.

## Media keys and Now Playing (`media.rs`)

souvlaki provides both directions: MPRIS (D-Bus) on Linux, using the
pure-Rust `zbus` backend so there's no libdbus build dependency, and
MPNowPlayingInfoCenter / MPRemoteCommandCenter on macOS. `NowPlaying::start`
registers as `foldly` and attaches a handler that sends each
`MediaControlEvent` into an `mpsc` channel. `App::new` keeps it if it starts
and silently does without otherwise (no D-Bus session, the name already taken
by another foldly).

`App::tick` drains the channel first and routes each command through
`handle_media_command`, which maps it onto the same actions as the keyboard:
play/pause/toggle (Play only resumes a paused track, Pause only pauses a
playing one), next/previous/stop, seek by a direction (5 s) or an amount, set
position (Control Center's scrubber, MPRIS `SetPosition`), volume (MPRIS),
and shuffle (macOS). At the end of `tick`, `publish_now_playing` hands the
current track, state and position to `NowPlaying::publish`, which sends
metadata only when the track changes and playback state only when it changes
or, while playing, once per `POSITION_INTERVAL` (1 s).

**macOS threads.** MPRemoteCommandCenter delivers commands as blocks on the
main dispatch queue, which only runs while the main thread runs its run loop.
foldly's loop blocks in `event::poll`, so on macOS `main` spawns the app
(`run_app`: argument parsing, terminal, audio, everything) on a thread named
`foldly` and runs `CFRunLoop::run_in_mode` in 100 ms slices until that thread
finishes; a panic on it is re-raised with `resume_unwind`. ratatui,
crossterm, rodio and SQLite don't care which thread they run on. Publishing
(MPNowPlayingInfoCenter) works from any thread.

## Playback (`player.rs`)

`Player::new` opens the default output device and keeps the `MixerDeviceSink`
for the whole run; dropping it silences everything.

**Buffer size.** rodio's default builder asks for a fixed ~50 ms buffer (2048
frames at 48 kHz), which some Macs reject ("Buffer size 2048 is not in the
supported range 15..=960"). `Player::new` first opens the default device with
`BufferSize::Default` (the OS chooses), and falls back to rodio's own
`open_default_sink` search only if that fails.

**One rodio `Player` per track.** `play` builds a fresh `rodio::Player`
connected to the mixer and appends the decoder; assigning it to `current`
drops the previous one, which stops it. This avoids `Player::clear`, which
blocks on the audio thread and pauses the player. Consequences:

- "finished" is `current.empty()` (the decoder ran out), checked from `tick`;
- position is `get_pos()`, seek is `try_seek()` (works because
  `Decoder::try_from(File)` records the byte length);
- volume lives in `Player.volume` (0.0–1.5, default 1.0, rounded to whole
  percents so `+`/`-` steps don't drift; `App::set_volume` also saves it to
  `settings` as `volume`) and is applied to
  each new rodio player;
- there is no gapless playback (backlog G would append to the same player);
- `load_paused` (resuming a session) and `reconnect` of a paused track go
  through `start(path, paused: true)`, which pauses the new rodio player
  *before* appending the source, so not a sample is heard; the following
  `try_seek` is still applied, because rodio handles seeks in the source's
  periodic callback, which keeps running while paused.

**Stream errors.** rodio's default error callback `eprintln!`s, which writes
over the TUI; ratatui only repaints cells it changes, so the text lingered.
`open_device` installs a callback that sends each `cpal::Error` to a channel
instead, and `App::tick` calls `Player::handle_errors`:

- `DeviceChanged` (cpal already rerouted a default-device stream), `Xrun` and
  `RealtimeDenied` are ignored: the stream still works.
- Anything else counts as a dead device. `reconnect` notes the current path,
  position and paused state (unless the track had finished), reopens the
  default device, and replays the track, seeking back and re-pausing. Players
  belong to the old device's mixer, so they are dropped before the device.
- Reconnects are throttled to one per `RECONNECT_INTERVAL` (2 s). If reopening
  fails, or another error lands inside the interval, `broken` is set and the
  next `play` reopens first.

A handled error returns a message and sets `App::needs_full_redraw`, which
makes the main loop `terminal.clear()` before drawing, wiping anything another
library may have printed. `Ctrl-L` sets the same flag.

## Rendering (`ui.rs`)

### Layout

`draw` splits the frame into: the main area (sidebar `sidebar_width`% |
songs `Fill` | right column `right_width`%), the transport (3 rows), and the
status line (1 row), and records the three column rects in `App::panes` for
mouse hit-testing. Popups draw last, centred, over a `Clear`.

`pane_block` frames the two focusable panes. The focused one gets a
`BorderType::Thick` accent border and a bold accent title, and a filled
selection; every other pane, focusable or not (info, queue), uses
`quiet_block`, a dimmed border with a plain title. So exactly one heavy border
is on screen, which shows focus even when the pane is empty. The unfocused
focusable pane keeps a bold-only selection so both selections stay visible.

When the songs list has no rows, `draw_songs` renders `empty_songs_message`
in the block instead of an empty `List`: no filter matches (Esc clears), an
empty playlist (how to add songs), scanning, or no music found (where to put
files).

- **Sidebar** — titled ` Library `, with the `~`-shortened music root in the
  bottom border. Rows are the source name (All Tracks bold) with its count
  right-aligned (the row width is the inner width minus the highlight symbol).
- **Songs** — a `List` with `▌` as the highlight symbol, titled with the
  source's name; scan progress goes in a right-aligned top title. `♪` green
  bold marks the playing track; missing tracks are dimmed.

**Row layout.** Song rows (songs list and queue) come from `track_row`: the
`♪` column, `fitted_label`, and the length (or `missing`) right-aligned and
dimmed; sidebar rows use the same `fit_row` for their counts. Everything is
measured in display columns (`unicode-width`), so wide characters line up,
and `truncate_to_width` never splits one and ends cuts in `…` (trailing spaces
trimmed first). `fitted_label` shortens the artist before the title, since
the title tells songs apart; with under 4 columns left for the artist it shows
the title alone. `list_row_width` is the inner width minus the highlight
symbol for lists that have one.
- **Right column** — when the screen is at least `INFO_MIN_SCREEN_WIDTH` (100)
  wide and the column at least `INFO_MIN_COLUMN_HEIGHT` (16) tall, it is
  split in half: the info pane (`draw_info`, a wrapping `Paragraph` titled
  ` Selected `) over the queue. Otherwise the queue takes it all. Info lines
  are ordered by usefulness with no spacer lines (title, artist, album, a
  missing warning, track/disc/length, year/genre, "also in", quality, file),
  because half a column can cut the end off.
- **Queue** — titled `Queue i/n`, framed by `pane_block` like the other
  focusable panes; played tracks dimmed, the current one green. When
  focused, the window is nudged to keep the cursor in view, the cursor row
  gets the `▌` highlight (a fresh `ListState` over the visible slice), and
  the first shown row is stored in `App::queue_offset` for clicks.
  `queue_window_start` picks the first row: 0 until the current track nears
  the bottom, then scrolled to keep `rows / 3` (at most `MAX_LOOKAHEAD`, 5)
  upcoming tracks visible, and never past a full last page. Only **the rows
  that fit** are built: the queue can be the whole library and draw runs
  5×/sec.
- **Transport** — a `Gauge` whose bordered block's top title is the current
  song (title bold, `— artist` cyan, `· album` italic dimmed), so "now
  playing" costs no extra rows. The label shows state icon, time, volume,
  `shuffle` and `repeat all|one`.
- **Status line** — the filter prompt while typing, else `message`, else an
  active-filter reminder, else key hints for the focused pane and source,
  ending in `? help`.

### Resizing and the mouse

`sidebar_width` (default 20, range 10–40) and `right_width` (default 30, range
15–50) are loaded from `settings` in `App::new`. `clamp_widths(moved)` applies
the ranges and keeps the songs list at least `MIN_SONGS_WIDTH` (25%): when the
side columns don't fit, the one beside the divider that moved gives way. `<`/`>`
call `resize_focused(∓2)`, which moves the divider on the right of the focused
pane (sidebar|songs, or songs|right) and saves immediately.

`main.rs` enables crossterm mouse capture after `ratatui::init` and disables
it before `ratatui::restore`; a panic hook wrapping ratatui's own does the
same, since ratatui's hook doesn't know about mouse capture. `handle_mouse`
routes left down/drag/up to `App::mouse_down`/`mouse_drag`/`mouse_up`:

- down on a divider (either border of the pair, at `panes.songs.x` or
  `panes.right.x`) grabs it as `dragging`; on the progress bar
  (`panes.transport`) it starts *scrubbing*: `App::scrub` holds the target
  position (`bar_position`: the column's fraction of the bar's inside,
  clamped so a drag can overshoot either end), drags update it, the
  transport draws it instead of the real position, and `mouse_up` performs a
  single `seek_to`. Seeking only on release avoids restarting the decoder on
  every mouse move; a new track starting clears a pending scrub; anywhere else in the
  sidebar or songs it focuses that pane and selects the clicked row (`offset`
  from the list's `ListState`, minus the top border). A second click on the
  same pane and row within `DOUBLE_CLICK` (400 ms, tracked in `last_click`)
  calls `activate_selected`, i.e. acts like `Enter`. `mouse_down` takes the
  click's `Instant` from `main.rs`, so tests pass their own times;
- drag sets the grabbed column's width so the border sits under the pointer
  (rounded to the nearest percent), without saving;
- up saves, so a drag costs one db write.

Mouse capture stops terminals from translating the wheel into arrow keys in
the alternate screen, so the wheel is handled explicitly: `scroll_at` moves
the selection by 3 in the pane under the pointer (or scrolls the help popup).

### Popups

`centered(area, w, h)` centres a fixed size with `Flex::Center`, shrinking to
fit. Input prompts and the delete confirmation are 50×3; the picker grows with
its rows up to 20. The help popup is 80 wide and as tall as its content or the
screen; `draw_help` clamps `scroll` so the last line can't scroll past the
bottom, and shows a scroll hint only when needed. Help descriptions must fit
61 columns (80 minus borders and the 17-column key column).

## Testing

Unit tests sit at the bottom of their module; there is no `tests/` directory.

- *db.rs* — playlist editing on an in-memory db: unique names ignoring case,
  duplicates allowed by `add_to_playlist`, `swap_entries`, `remove_entry`,
  `add_tracks` skipping existing, deleting a playlist keeping its tracks,
  `track_details` and `playlists_containing` (distinct, alphabetical), and
  upgrading an original-layout database file (version, `size` reset,
  playlists kept, nothing re-run on the next open).
- *ui.rs* — sample-rate formatting, the queue scrolling rules, row fitting
  (`fit_row`, `truncate_to_width`, wide characters) and `fitted_label`
  (artist shortened before the title).
- *scan.rs* — a real temp folder with dummy files: hidden files/folders and
  non-audio skipped, unchanged files not re-read, a vanished file marked
  missing but kept in its playlist, and restored when it returns.
- *watch.rs* — `is_relevant` rules on hand-built events, and a real watcher
  on a temp folder: a new audio file is reported and `take_settled` fires once,
  only after `SETTLE`.
- *filter.rs* — word order, fuzzy abbreviations, accents, fzf operators,
  unranked order.
- *queue.rs* — every queue rule without audio: `start` with and without
  shuffle, `set_shuffle` keeping the current track, the cursor following the
  current track until moved, removal (index bookkeeping, refusing the current
  track), moves with shuffle off and on, clearing upcoming, key encoding, and
  `restore` dropping lost tracks.
- *src/app/tests.rs* — **ignored end-to-end tests** that need an audio
  device and `FOLDLY_TEST_DIR` (three tagged tones, see DEVELOPMENT.md). Each
  drives a real `App` through one area: browsing, filtering and editing
  playlists; the info pane; playback, shuffle and repeat; audio errors
  (injected with `Player::inject_error`); mouse clicks, seeking, scrubbing and
  resizing; media commands; the queue pane; resuming; the help popup; and
  picking up files added while running (on a temp copy of the fixtures).
  Frames are rendered to a `TestBackend` and printed with `--nocapture`.

`Harness` gives each test a scanned `App` on its own throwaway database
(`foldly-e2e-<name>-<pid>.db`, removed on drop) and derefs to it;
`Harness::with_mix` adds a playlist "Mix" of Tone 1, Tone 3, Tone 2 straight
through `db`, so tests start from a known state in milliseconds instead of
replaying UI steps. `reopen` builds a second `App` on the same database, as
the next run would. Because every test has its own database they run in
parallel (about seven seconds), and none depends on another's leftovers.

Temp paths are `std::env::temp_dir()/foldly-<test>-<pid>` so parallel tests
don't collide. CI (`release.yml`) runs `cargo test` on every target it can
execute; the ignored tests only run locally.

## Design decisions, in brief

- **The db is a cache of the folder plus the user's own data.** Tracks can
  always be rebuilt by scanning; playlists and settings are the only things
  worth backing up.
- **Relative paths and soft deletes** keep playlists alive across moved roots
  and temporarily missing files.
- **Virtual folders, not the file tree.** One flat folder works; organising is
  done with playlists.
- **Background scan, foreground everything else.** The scan is the only slow
  operation, so it is the only thing off the UI thread; all other db calls are
  small and synchronous.
- **One rodio player per track** trades gapless playback for simple, reliable
  skip/stop semantics.

## Known nits

Tracked in DEVELOPMENT.md's backlog: decode-skip messages being cleared, the
repeat-all reshuffle possibly repeating the last track, duplicate `♪` markers,
and per-batch reloads during scans.
