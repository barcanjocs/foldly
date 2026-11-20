# Development notes

Handoff document for work sessions (human or agent). The repo is the source of
truth for state — this file plus `git log` is all that is needed to resume.

## Session protocol

**Starting a session** (when asked to read this file and continue):

1. Read this file in full, then run `git log --oneline -15`.
2. Check *Now* below for state and next step; confirm the worktree is clean
   (`git status`).
3. Run `./check` to confirm the tree is green before changing anything.
4. State briefly what you found and what you will do, then proceed.

**Finishing a session** (when told the session is ending):

1. Update *Now* below (state, next step, blockers).
2. Run `./check` — must be green.
3. Commit everything, including this file's update.

## Now

- State: first release, v0.1.0. Playback (rodio), background scan into SQLite,
  virtual folders (All Tracks + playlists), playlist editing (add, add all
  shown, remove, reorder, rename, delete), fuzzy filter (nucleo), shuffle,
  repeat, queue panel, `?` help, one-line install from GitHub releases built
  by `.github/workflows/release.yml`. Since v0.1.0: the music folder is
  watched (notify) and rescanned automatically once changes settle (item I);
  audio stream errors no longer print over the TUI, and a lost output device
  is reopened with the track resumed; `Ctrl-L` redraws; the queue panel
  scrolls only when needed; three columns (sidebar of sources | songs |
  queue) with focus switching, resizable (`<`/`>` or mouse drag) and saved
  in `settings`; an info pane above the queue for the selected song (stored
  tags, file, "also in") or the selected source (count, length, missing).
  In progress: step 3, extra tags (year, genre, format, quality) with a
  one-time rescan. Tree green, 10 tests + 2 ignored end-to-end tests
  (need audio; see *Verification*). Reconnecting is tested by injecting
  errors, not by a real device switch. Docs split into README
  (users), this file (state), IMPLEMENTATION.md (internals).
- Next: backlog item A (resume last session).
- Blockers / open questions: never run on a real Linux desktop (item K).

## Backlog

Ranked by value-per-effort as of 2026-11-20. Each item should be startable
from its scope note alone, without re-deriving context.

- [ ] **A. Resume last session** (small). On quit, nothing is remembered but
      the music folder, shuffle and repeat. Save the location, the queue
      (track ids + entry ids in play order, plus `unshuffled`), the queue
      index and the playback position in `settings` (JSON or a small
      `session` table). On start, restore the location and queue paused at
      that position; `Space` resumes. Drop entries whose track is missing.
      Touches: `App::new`, a save on quit in `main.rs::run`, db helpers.
- [ ] **B. Media keys and OS Now Playing** (medium). Use `souvlaki` (MPRIS on
      Linux, MPNowPlayingInfoCenter on macOS) to publish title/artist/album/
      duration/position and receive play/pause/next/previous/seek. Events
      arrive on another thread: forward them through an `mpsc` channel
      drained in `App::tick`, like `ScanEvent`. macOS needs a run loop on
      the main thread for callbacks; check souvlaki's docs for TUI apps.
- [ ] **C. Artists and Albums in the sidebar** (medium). Add sidebar items
      for each artist and album (new `Location` variants, `SELECT DISTINCT`
      over tags), perhaps under collapsible "Artists" / "Albums" headings;
      an artist shows all their songs grouped by album. `A` from the sidebar
      then adds an exact artist/album (unlike filter + `A`). Untagged tracks
      go in "Unknown".
- [ ] **D. Interactive queue panel** (medium). `Tab` focuses the queue; while
      focused, `j`/`k` move, `Enter` jumps to a track, `d` removes it,
      `J`/`K` reorder, `c` clears upcoming. Needs a focus field on `App`,
      a `ListState` for the queue, and edits applied to both `queue` and
      `unshuffled` (see the shuffle invariant in IMPLEMENTATION.md).
- [ ] **E. M3U export / import** (small-medium). `E` on a playlist writes
      `<name>.m3u` with paths relative to the music root (into the root, or
      a chosen folder); import reads `.m3u` files found during the scan, or
      via a key, matching entries by relative path. Gives a backup that other
      players can read.
- [ ] **F. Mark tracks** (small). `m` toggles a mark on the selected track,
      `*` marks everything shown, `M` clears marks; `a` adds the marked
      tracks when any are marked. Marks are a `HashSet<(track id, entry id)>`
      cleared on navigation; draw a marker column in the songs list.
- [ ] **G. Gapless playback** (small-medium). Near the end of a track, append
      the next decoder to the same rodio `Player` instead of replacing the
      player; advance `queue_index` when `Player::len()` drops. Interacts with
      repeat-one and with `n`/`p` (which must clear the preloaded source).
- [ ] **H. Add the playing track to a playlist** (tiny). A key (e.g. `P`)
      that opens the picker for `now_playing` instead of the selection.
- [x] **I. Watch the music folder** — done 2026-11-20: `watch.rs`, rescans
      2 s after relevant changes settle; `u` kept as a manual rescan.
- [ ] **J. Relink after renames and root changes** (medium). Renamed files
      currently become a missing track plus a new one, dropping playlist
      entries. On scan, match new files to missing tracks by tags + duration
      + size and move `rel_path` instead. Same idea when the music root
      changes and relative paths no longer line up.
- [ ] **K. Linux desktop check** (small). CI builds and runs the tests on
      Linux, but no one has played audio there. Try it under PipeWire and
      plain ALSA, including unplugging the output device. rodio's errors are
      captured, but ALSA's C library can still print to stderr; `Ctrl-L`
      repairs the screen. If it happens often, redirect stderr (fd 2) to a
      log file while the TUI is up.
- [ ] **L. Track durations in lists** (tiny). Right-align `mm:ss` in the
      songs and queue rows; durations are already in `Track::duration`.
- [ ] **M. Cover art** (medium). Show embedded art (lofty) or `cover.jpg` in a
      corner of the queue panel with `ratatui-image` (kitty/sixel/iTerm
      protocols, halfblocks fallback).
- [ ] **N. Play counts, last played, favourites** (small-medium). New columns
      on `tracks`, bumped when a track plays past half way; feeds O.
- [ ] **O. Smart playlists** (medium-large). Saved rules (artist is X,
      added in the last N days, never played) shown as virtual folders.
- [ ] **P. Config file** (medium). Keybindings, colours, seek step, start
      view, in `dirs::config_dir()/foldly/config.toml`.
- [ ] **Q. Mouse clicks** (small). Mouse capture is on for divider dragging;
      clicking already focuses and selects; double-click could play, and
      clicking the progress bar could seek. Hit-test with the rects
      `ui::draw` already records (extend `App::panes` with the transport).
- [ ] **Nits** (tiny, do whenever):
  - A file that fails to decode sets the message line, but `play_index`
    clears it when the next track starts, so the skip is invisible. Keep the
    message when a skip happened.
  - Repeat-all with shuffle reshuffles from a random first track, which can
    be the track that just finished.
  - The songs list marks every row of the playing track with ♪, so a track
    listed twice in a playlist shows two.
  - Progress events trigger a full `reload` of the current list every 200
    files; fine so far, but rebuilds every filter haystack each time.
  - The `HELP` table in ui.rs is maintained by hand alongside
    `handle_normal_key`; a test could check every bound key appears.

## Conventions (don't drift)

- **foldly never writes to the music folder.** Reading tags and audio only.
- **Tracks are never deleted** by the scanner; vanished files get
  `missing = 1`, so playlist entries survive. Paths are stored relative to
  the music root, valid UTF-8 only (others are skipped at scan time).
- **All SQL lives in db.rs.** Other modules call its functions.
- **Errors:** user-visible failures go through `App::report` (or set
  `message`) and show on the bottom line; only startup failures (music
  folder, database, audio device) exit, before the TUI starts.
- **`draw` never touches the db** (it runs ~5×/sec). Everything it needs is a
  field on `App`; it may only update widget state (list offsets, help
  scroll clamping).
- **Shuffle invariant:** `unshuffled` holds the queue in original order; any
  change to the queue's contents must update both `queue` and `unshuffled`.
- **Keys:** a new key needs an entry in `handle_normal_key` (main.rs), the
  `HELP` table and status hints (ui.rs), and README's key table.
- **Tests:** temp paths use a `foldly-` prefix plus a per-test name and the
  pid, so parallel tests don't share files. Tests needing an audio device or
  fixtures are `#[ignore]` and read `FOLDLY_TEST_DIR`.
- **Commits:** small, lowercase imperative, one concern each; keep the
  worktree clean so `git diff` always shows exactly the current task.
- User-facing docs (keys, behaviour, data model, backup) live in README.md —
  keep it in sync when behavior changes. Internals live in IMPLEMENTATION.md.

## Verification

```sh
./check   # fmt --check, clippy -D warnings, build, test — must be green before committing
```

The end-to-end tests play real audio, so it needs an audio device and a
folder of three short tagged MP3s named `tone1.mp3`..`tone3.mp3` (titles
"Tone 1" to "Tone 3", artist "Test Artist"). Run them after touching
playback, the queue, scanning, watching or the UI:

```sh
mkdir -p fixtures
for i in 1 2 3; do ffmpeg -f lavfi -i "sine=frequency=$((300+i*40)):duration=3" \
  -metadata title="Tone $i" -metadata artist="Test Artist" -metadata album="Album A" \
  -metadata track=$i fixtures/tone$i.mp3; done
FOLDLY_TEST_DIR=fixtures cargo test -- --ignored --nocapture
```

`--nocapture` prints rendered frames, useful for eyeballing layout changes.

Requires Rust stable (edition 2024). Linux needs `libasound2-dev`.

## Releasing

1. Bump `version` in `Cargo.toml`, run `./check`, commit.
2. Tag and push: `git tag vX.Y.Z && git push origin main vX.Y.Z`.
3. `.github/workflows/release.yml` builds and tests all four targets
   (Linux x86_64/arm64, macOS arm64/x86_64), refuses a tag that doesn't
   match `Cargo.toml`, and publishes the release with a `.sha256` per asset.
4. Check the install: `curl -fsSL https://raw.githubusercontent.com/barcanjocs/foldly/main/install.sh | sh`.

## File map

- `IMPLEMENTATION.md` — developer-facing implementation details:
  architecture, data layer, scanning, app state and modes, queue, playback,
  rendering, testing. Keep in sync when architecture changes.
- `src/main.rs` — entry point: `--help`/`--version`, music root resolution,
  audio + db open, 200 ms event loop, key dispatch per `Mode`
  (`handle_key`, `handle_normal_key`).
- `src/app.rs` — `App` state: `Location` / `Entry` (virtual folders),
  `Mode` (prompts), listing and filtering, playlist actions, queue,
  shuffle, repeat, scan polling. Unit + end-to-end tests at the bottom.
- `src/db.rs` — schema and every query (tracks, playlists, settings).
  Unit tests at the bottom.
- `src/scan.rs` — background scanner thread and tag reading (lofty).
  Unit test at the bottom.
- `src/watch.rs` — music folder watcher (notify), event relevance rules,
  settle timer. Unit tests at the bottom.
- `src/filter.rs` — fuzzy filter over nucleo. Unit tests at the bottom.
- `src/player.rs` — audio output: one rodio `Player` per track.
- `src/ui.rs` — all drawing: sidebar, songs, queue, transport, status line, popups,
  and the `HELP` key table.
- `install.sh` — the curl installer (POSIX sh).
- `.github/workflows/release.yml` — release builds on `v*` tags.
- `check` — the verification script.
