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

- State: v0.3.0 released. Playback (rodio), background scan into SQLite,
  the music folder watched (notify) and rescanned once changes settle, three
  columns (sidebar of sources | songs | info over queue) with focus
  switching, resizable by `<`/`>` or mouse drag and saved in `settings`.
  Playlist editing (add, add all shown, remove, reorder, rename, delete),
  fuzzy filter (nucleo), shuffle, repeat, `?` help, `Ctrl-L` redraw. The info
  pane shows the selected song (tags incl. year/genre, format and quality,
  file, "also in") or summarises the selected source. Audio stream errors are
  captured; a lost output device is reopened with the track resumed. Schema
  versioning via `user_version`; migration 1 (v0.2.0) forces one full
  re-read of the files on upgrade, checked by hand on a database made by the
  v0.1.0 binary. One-line install from GitHub releases built by
  `.github/workflows/release.yml`. Tree green, 12 tests + 2 ignored
  end-to-end tests (need audio; see *Verification*). Confirmed by hand on
  macOS (2026-11-20): dragging the dividers with a real mouse, and switching
  the audio output while playing (no error on screen, playback follows).
  Not yet tried by hand: unplugging the playing device, i.e. the
  reconnect-and-resume path (tested only by injecting errors).
  New in v0.3.0: obvious focus (one heavy border) and empty-list messages,
  double-click to play and click-to-seek (Q), `P` adds the playing song to a
  playlist (H), lengths in lists (L), resume where you left off (A).
- State addendum: v0.4.0 adds media keys and Now Playing (B). v0.5.0 adds
  the interactive queue pane (D), dragging on the progress bar to scrub, and
  a remembered volume starting at 100%.
- Decision (2026-11-20): foldly's volume stays separate from the system
  volume. macOS volume keys never reach apps, and the user prefers an
  app-level volume since macOS has no per-app mixer; don't try to link them.
- Next: backlog item C (Artists and Albums in the sidebar).
- Dependency watch: souvlaki 0.9 pulls in `block` 0.1.6 on macOS, which
  cargo warns "will be rejected by a future version of Rust"; souvlaki has a
  1.0 rewrite in progress. If the warning becomes an error, move to 1.0 or
  patch `block` to `block2`.
- Blockers / open questions: never run on a real Linux desktop (item K).

## Backlog

Ranked by value-per-effort as of 2026-11-20. Each item should be startable
from its scope note alone, without re-deriving context.

- [x] **A. Resume last session** — done 2026-11-20: saved on quit and every
      15 s; restored paused at the saved position. Real quit path checked by
      running the binary in a pty.
- [x] **B. Media keys and OS Now Playing** — done 2026-11-20 (souvlaki,
      `media.rs`; on macOS the app runs off the main thread). Confirmed by
      hand on macOS: media keys and Control Center work. Linux MPRIS is
      still unchecked (`playerctl -p foldly metadata`, `playerctl -p foldly
      play-pause`; part of item K).
- [ ] **C. Artists and Albums in the sidebar** (medium). Add sidebar items
      for each artist and album (new `Location` variants, `SELECT DISTINCT`
      over tags), perhaps under collapsible "Artists" / "Albums" headings;
      an artist shows all their songs grouped by album. `A` from the sidebar
      then adds an exact artist/album (unlike filter + `A`). Untagged tracks
      go in "Unknown".
- [x] **D. Interactive queue panel** — done 2026-11-20: third `Pane`,
      cursor, jump, remove, `J`/`K`, clear upcoming, `a`/`A`.
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
- [x] **H. Add the playing track to a playlist** — done 2026-11-20: `P`.
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
- [x] **L. Track durations in lists** — done 2026-11-20: right-aligned in
      songs and queue rows, display-width aware.
- [ ] **M. Cover art** (medium). Show the selected song's embedded art
      (lofty) or a `cover.jpg` beside it in the info pane, with
      `ratatui-image` (kitty/sixel/iTerm protocols, halfblocks fallback).
      Decode off the UI thread and cache by track id; draw must stay cheap.
- [ ] **N. Play counts, last played, favourites** (small-medium). New columns
      on `tracks`, bumped when a track plays past half way; feeds O.
- [ ] **O. Smart playlists** (medium-large). Saved rules (artist is X,
      added in the last N days, never played) shown as virtual folders.
- [ ] **P. Config file** (medium). Keybindings, colours, seek step, start
      view, in `dirs::config_dir()/foldly/config.toml`.
- [x] **Q. Mouse clicks** — done 2026-11-20: double-click acts like Enter,
      clicking the progress bar seeks.
- [ ] **Nits** (tiny, do whenever):
  - A file that fails to decode sets the message line, but `play_index`
    clears it when the next track starts, so the skip is invisible. Keep the
    message when a skip happened.
  - Repeat-all with shuffle reshuffles from a random first track, which can
    be the track that just finished.
  - The songs list marks every row of the playing track with ♪, so a track
    listed twice in a playlist shows two.
  - `fit_row` treats the highlight symbol's column as always reserved, so
    with nothing selected rows end one column short of the border.
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
- **Schema changes are migrations**: append to `MIGRATIONS` in db.rs, never
  edit `SCHEMA` or a shipped migration. If the new columns come from the
  files, end with `UPDATE tracks SET size = -1` to force one full re-read.
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
"Tone 1" to "Tone 3", artist "Test Artist", year 1997, genre "Test Genre"). Run them after touching
playback, the queue, scanning, watching or the UI:

```sh
mkdir -p fixtures
for i in 1 2 3; do ffmpeg -f lavfi -i "sine=frequency=$((300+i*40)):duration=3" \
  -metadata title="Tone $i" -metadata artist="Test Artist" -metadata album="Album A" \
  -metadata track=$i -metadata date=1997 -metadata genre="Test Genre" fixtures/tone$i.mp3; done
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
- `src/media.rs` — media keys and Now Playing (souvlaki): command channel,
  change-only publishing.
- `src/player.rs` — audio output: one rodio `Player` per track.
- `src/ui.rs` — all drawing: sidebar, songs, queue, transport, status line, popups,
  and the `HELP` key table.
- `install.sh` — the curl installer (POSIX sh).
- `.github/workflows/release.yml` — release builds on `v*` tags.
- `check` — the verification script.
