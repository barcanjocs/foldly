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
runs. rodio runs its own audio thread.

```
main.rs        args → music root → Player + App → loop: tick → draw → poll(200 ms) → key
  └─ App       all UI state, the queue, a db Connection       (app.rs)
       ├─ db      all SQL; tracks, playlists, settings          (db.rs)
       ├─ scan    background scanner thread → ScanEvent channel (scan.rs)
       ├─ Filter  fuzzy matcher (nucleo)                        (filter.rs)
       └─ Player  rodio output device + current track           (player.rs)
ui.rs          draw(frame, &mut App): browser, queue, transport, status, popups
```

The loop runs at ~5 fps: `event::poll` times out after 200 ms so the progress
bar moves without key presses. Each iteration first calls `App::tick`, which
auto-advances when a track ends and drains scan events, then draws, then
handles at most one key. Only key *press* events are handled.

Data flow per frame is **state → pixels**: `ui::draw` reads `App` and never
touches the db. It takes `&mut App` only for widget state (the browser's
`ListState` scroll offset, and clamping the help popup's scroll).

## Module map

- **`main.rs`** — handles `--help`/`--version` before anything else, resolves
  the music root (CLI argument, saved to `settings`, else the saved one), opens
  the audio device *before* entering the TUI so those errors print normally,
  then runs the loop. Key dispatch lives here: `handle_key` routes by `Mode`,
  `handle_normal_key` holds the browsing/playback/playlist bindings.
- **`app.rs`** — `App` and its types: `Location`, `Entry`, `Mode`,
  `InputPurpose`, `Repeat`. Listing, filtering, navigation, playlist actions,
  prompts, queue, shuffle, repeat and scan polling. Tests at the bottom.
- **`db.rs`** — schema (`const SCHEMA`, applied with `execute_batch` on every
  open, so opening also migrates), `Track`/`Playlist`/`ScannedFile`, and every
  query. Tests at the bottom.
- **`scan.rs`** — `spawn` starts the scanner thread; `run` walks the folder and
  updates the db; `read_file` reads tags with lofty. Test at the bottom.
- **`filter.rs`** — `Filter`, a reusable nucleo `Matcher` with one method,
  `matches(query, haystacks, rank)`. Tests at the bottom.
- **`player.rs`** — `Player` wraps rodio: the device sink plus at most one
  playing rodio `Player`.
- **`ui.rs`** — every draw function, the `HELP` key table, colours (`ACCENT`).

## Data layer (`db.rs`)

### Schema

Four tables; see README.md for the full DDL.

- `tracks` — one row per audio file ever seen: `rel_path` (unique, relative
  to the music root), `size` + `mtime` (the change stamp), tags, `duration_ms`,
  and `missing`.
- `playlists` — `name` is `UNIQUE COLLATE NOCASE`, so "Mix" and "mix" can't
  coexist; `create_playlist`/`rename_playlist` turn the constraint error into
  "a playlist named … already exists".
- `playlist_tracks` — entries with their own `id` (the *entry id*), so the same
  track can appear twice and each occurrence can be removed or moved on its
  own. Order is `position`, then `id`. Both foreign keys cascade
  (`PRAGMA foreign_keys` is on).
- `settings` — key/value: `music_root`, `shuffle` (`"1"`/`"0"`), `repeat`
  (`off`/`all`/`one`).

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
updates `scan_status` (drawn in the browser's top-right border), reloads the
current list after progress, and drops the receiver when the scan ends.

Tag reading (`read_file`) uses the file's primary tag, falling back to the
first tag; empty or whitespace-only values become `None`. A file lofty can't
parse is still recorded, with no tags, so it shows by file name and the
decoder gets a chance at playback.

## App state (`app.rs`)

```rust
pub struct App {
    root, db_path, conn,              // music folder, db path (for the scanner), UI connection
    location: Location,               // Root | AllTracks | Playlist { id, name }
    entries: Vec<Entry>,              // Parent | Folder { location, count } | Track(Track)
    haystacks: Vec<Utf32String>,      // filter text, one per entry
    fuzzy: Filter,
    visible: Vec<usize>,              // indices into entries, in display order
    filter: String,
    list_state: ListState,            // selection is an index into `visible`
    mode: Mode,                       // which prompt/popup is open
    queue, queue_index, unshuffled,   // see "Queue" below
    shuffle, repeat, now_playing, player,
    message, scan_status, scan_rx, should_quit,
}
```

**The UI mirrors the db.** `load_entries(location)` queries the db and
`set_entries` stores the result together with the haystacks; `reload` re-runs
it for the current location after any change (playlist edits, scan progress).
Draw code only reads these fields.

### Virtual folders and navigation

`Location` is the "folder" being shown. `Root` lists an `All Tracks` folder and
one folder per playlist; the others list a `Parent` (`../`) entry followed by
tracks. `navigate(location)` clears the filter, loads the new entries, and
picks the selection: going up to `Root` selects the folder you came from,
going down selects the first track.

Track identity within a list is `(track id, entry id)` (`queue_key`): the entry
id is `None` in All Tracks and the `playlist_tracks.id` inside a playlist,
which is what distinguishes two occurrences of the same song.

### Filtering

`set_entries` precomputes one haystack per entry: `"<label> <album>"` for
tracks (label is "Artist — Title" or the file stem), the name for folders,
empty for `Parent`. `apply_filter` then:

- with an empty filter, shows everything;
- otherwise keeps `Parent` pinned at the top and runs `Filter::matches` over
  the rest, ranked by score in `AllTracks` and in original order elsewhere
  (so a filtered playlist still reads as the playlist).

`set_filter` also selects the first non-`Parent` row, so `Enter` from filter
mode plays the best match (filter mode hands `Enter`/`↑`/`↓` to
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
playlist, single adds (`a`) don't. `open_picker` leaves out the playlist you're
in. Row 0 of the picker is always "New playlist…"; the initial selection is
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

```
unshuffled: the playable tracks of the list Enter was pressed in, in list order
queue:      the play order (== unshuffled, or a shuffle of it)
queue_index: position in `queue` of the current track
```

- **Enter on a track** builds `unshuffled` from the visible, non-missing
  tracks. Without shuffle `queue = unshuffled` and play starts at the chosen
  index; with shuffle `queue = shuffled_from(unshuffled, chosen)` (chosen
  first, the rest `fastrand::shuffle`d) and play starts at 0.
- **Toggling shuffle** finds the current track in `unshuffled` by `queue_key`;
  on, it reshuffles around it (`queue_index = 0`); off, it restores
  `queue = unshuffled` at that position. This is why `unshuffled` must be kept
  in step with any change to the queue's contents.
- **`next`** plays `queue_index + 1`; past the end, repeat-all reshuffles (if
  shuffling) and starts at 0, otherwise `play_index` runs off the end and
  stops. **`previous`** restarts the track after 3 s, else goes back one, and
  wraps to the end under repeat-all.
- **Auto-advance** is `tick`: when `player.finished()`, repeat-one replays
  `queue_index`, everything else calls `next`.
- **`play_index(i)`** tries `queue[i]`, `i+1`, … until one decodes, then sets
  `now_playing` (with the decoder's duration if the db has none) and
  `queue_index`. If none decode, it stops.

The queue is a snapshot: editing a playlist doesn't change what's queued.

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
- volume lives in `Player.volume` (0.0–1.5, default 0.8) and is applied to
  each new rodio player;
- there is no gapless playback (backlog G would append to the same player).

## Rendering (`ui.rs`)

### Layout

`draw` splits the frame into: the main area (browser 62% | queue 38%), the
transport (3 rows), and the status line (1 row). Popups draw last, centred,
over a `Clear`.

- **Browser** — a `List` with `▌` as the highlight symbol. The border title is
  the location (`~`-shortened root at the top level); scan progress goes in a
  right-aligned top title. Rows: dimmed `../`, cyan `Folder/` with a dimmed
  count, `♪` green bold for the playing track, dimmed `(missing)` tracks.
- **Queue** — titled `Queue i/n`. It shows `HISTORY_ROWS` (2) played tracks,
  the current one, then upcoming tracks, and builds **only the rows that fit**:
  the queue can be the whole library and draw runs 5×/sec.
- **Transport** — a `Gauge` whose bordered block's top title is the current
  song (title bold, `— artist` cyan, `· album` italic dimmed), so "now
  playing" costs no extra rows. The label shows state icon, time, volume,
  `shuffle` and `repeat all|one`.
- **Status line** — the filter prompt while typing, else `message`, else an
  active-filter reminder, else per-location key hints ending in `? help`.

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
  `add_tracks` skipping existing, deleting a playlist keeping its tracks.
- *scan.rs* — a real temp folder with dummy files: hidden files/folders and
  non-audio skipped, unchanged files not re-read, a vanished file marked
  missing but kept in its playlist, and restored when it returns.
- *filter.rs* — word order, fuzzy abbreviations, accents, fzf operators,
  unranked order.
- *app.rs* — `shuffled_from` (chosen first, same tracks once each), plus one
  **ignored end-to-end test** that needs an audio device and `FOLDLY_TEST_DIR`
  (three tagged tones, see DEVELOPMENT.md). It drives `App` directly through
  scanning, navigation, filtering, the picker, bulk add, reordering,
  playback and auto-advance, shuffle on/off, every repeat mode, and renders
  frames to a `TestBackend` (printed with `--nocapture`), including the help
  popup at two heights.

Temp paths are `std::env::temp_dir()/foldly-<test>-<pid>` so parallel tests
don't collide. CI (`release.yml`) runs `cargo test` on every target it can
execute; the ignored test only runs locally.

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
per-batch reloads during scans, and the hand-maintained `HELP` table.
