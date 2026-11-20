# foldly

A terminal music player for Linux and macOS, built for one big music folder. foldly scans that
folder into a small SQLite database and shows it as virtual folders: `All Tracks/`, plus one
folder per playlist. Pressing Enter on a track plays it and queues the rest of that view.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/barcanjocs/foldly/main/install.sh | sh
```

This downloads the latest release for your machine (Linux or macOS, x86_64 or arm64), checks
its SHA-256 checksum and installs `foldly` to `~/.local/bin`. Set `FOLDLY_INSTALL_DIR` to put it
elsewhere, or `FOLDLY_VERSION=v0.1.0` for a specific release. Run the same command to update.
Binaries are also on the [releases page](https://github.com/barcanjocs/foldly/releases).

Then point it at your music once; the folder is remembered:

```sh
foldly ~/Music
```

On Linux, audio goes through ALSA (`libasound2`), which desktop distributions already include.

### From source

```sh
cargo install --git https://github.com/barcanjocs/foldly
```

Linux needs the ALSA headers to build: `sudo apt install libasound2-dev` (Debian/Ubuntu) or
`sudo dnf install alsa-lib-devel` (Fedora).

## Releasing

Bump `version` in `Cargo.toml`, commit, then tag and push:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

The release workflow builds and tests all four targets and publishes the release with checksums.

The database lives in your data directory (`~/Library/Application Support/foldly/foldly.db` on
macOS, `~/.local/share/foldly/foldly.db` on Linux). Set `FOLDLY_DB` to use a different one.
foldly never writes to your music files.

## Keys

| Key | Action |
|---|---|
| `↑`/`↓`, `j`/`k`, `PgUp`/`PgDn`, `g`/`G` | Move |
| `Enter`/`l` | Open folder / play track |
| `Backspace`/`h` | Back to the top level |
| `/` | Fuzzy filter the current view; `Enter` plays the top match, `Esc` clears |
| `Space` | Pause / resume |
| `n` / `p` | Next / previous (previous restarts the track after 3s) |
| `←` / `→` | Seek 5s |
| `+` / `-` | Volume |
| `s` | Stop |
| `z` | Shuffle on/off (remembered); the current track keeps playing |
| `r` | Repeat: off → all → one (remembered) |
| `a` | Add the selected track to a playlist |
| `A` | Add every track shown (e.g. after filtering) to a playlist; tracks already in it are skipped |
| `N` | New playlist |
| `R` | Rename playlist |
| `d` | Remove track from this playlist / delete the selected playlist |
| `J` / `K` | Move a track down / up in a playlist |
| `u` | Rescan (update) the music folder |
| `?` | Help: every key, explained |
| `q` | Quit |

Formats: MP3, FLAC, OGG Vorbis, WAV, M4A (AAC).

## Filtering

`/` filters fzf-style over artist, title and album. Each word matches on its own, in any order,
and fuzzily: `android radio` and `rhpar` both find "Radiohead — Paranoid Android". Accents are
optional (`beyonce` finds "Beyoncé"). In All Tracks the best matches come first; playlists keep
their order. To add everything by an artist, filter with `'artist name` (exact) and press `A`.
fzf's operators also work:

| Query | Meaning |
|---|---|
| `'word` | Exact match |
| `^word` | Starts with |
| `word$` | Ends with |
| `!word` | Exclude |

## Code

| File | What it does |
|---|---|
| `src/main.rs` | Startup, the event loop and key handling |
| `src/app.rs` | State: where you are, the queue, prompts, playlist actions |
| `src/db.rs` | SQLite schema and queries for tracks and playlists |
| `src/scan.rs` | Background scan of the music folder |
| `src/filter.rs` | Fuzzy filter (nucleo) |
| `src/player.rs` | Audio output via rodio |
| `src/ui.rs` | Drawing |

See [PROGRESS.md](PROGRESS.md) for what's done and planned.

## Tests

```sh
cargo test
```

The end-to-end test needs an audio device and a folder of three short tagged MP3s (titles
"Tone 1" to "Tone 3", artist "Test Artist"):

```sh
mkdir -p fixtures
for i in 1 2 3; do ffmpeg -f lavfi -i "sine=frequency=$((300+i*40)):duration=3" \
  -metadata title="Tone $i" -metadata artist="Test Artist" -metadata track=$i fixtures/tone$i.mp3; done
FOLDLY_TEST_DIR=fixtures cargo test -- --ignored
```
