# foldly

A terminal music player for one big music folder.

foldly plays the music files you already own, all kept in a single folder. It scans that folder
into a small local database and shows it in three columns: a sidebar with `All Tracks` and your
playlists, the songs of whichever one is selected, and on the right, details of the selected song
above the play queue. It never writes to your
music files, and nothing leaves your machine.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/barcanjocs/foldly/main/install.sh | sh
```

This downloads the latest release for your machine (Linux or macOS, x86_64 or arm64), checks its
SHA-256 checksum and installs `foldly` to `~/.local/bin`. Run the same command to update.

| Variable | Effect |
|----------|--------|
| `FOLDLY_INSTALL_DIR` | Install somewhere other than `~/.local/bin` |
| `FOLDLY_VERSION` | Install a specific release, e.g. `v0.1.0` |

Binaries are also on the [releases page](https://github.com/barcanjocs/foldly/releases).

On Linux, audio goes through ALSA (`libasound2`), which desktop distributions already include.

### Building from source

```sh
cargo install --git https://github.com/barcanjocs/foldly
```

Requires Rust stable. SQLite is bundled. Linux also needs the ALSA headers:
`sudo apt install libasound2-dev` (Debian/Ubuntu) or `sudo dnf install alsa-lib-devel` (Fedora).

## Running

Point foldly at your music folder once; it is remembered after that:

```sh
foldly ~/Music     # first run, or to switch folders
foldly             # every run after that
```

On first run, a database is created at:

- **macOS:** `~/Library/Application Support/foldly/foldly.db`
- **Linux:** `~/.local/share/foldly/foldly.db`

Set `FOLDLY_DB` to use a different database file. `foldly --help` and `foldly --version` work as
usual. Inside foldly, press `?` for every key.

## Usage

| Key | Action |
|-----|--------|
| `↑`/`↓`, `j`/`k` | Move the selection in the focused pane (`PgUp`/`PgDn` by 10, `g`/`G` to top/bottom) |
| `Tab` | Switch focus between the sidebar and the songs list |
| `Enter`/`l` | Sidebar: go to its songs. Songs: play the track and queue the rest of the list |
| `Backspace`/`h` | Back to the sidebar |
| `<` / `>` | Move the divider to the right of the focused pane (or drag dividers with the mouse) |
| `/` | Fuzzy filter the songs list; `Enter` plays the top match, `Esc` clears |
| `Space` | Pause / resume |
| `n` / `p` | Next / previous track |
| `←` / `→` | Seek 5 seconds |
| `+` / `-` | Volume |
| `z` | Shuffle on / off |
| `r` | Repeat: off → all → one |
| `s` | Stop |
| `a` | Add the selected track to a playlist |
| `A` | Add every track shown to a playlist |
| `N` | New playlist |
| `R` | Rename the playlist being shown |
| `d` | Songs: remove the track from this playlist. Sidebar: delete the playlist |
| `J` / `K` | Move a track down / up in a playlist |
| `u` | Rescan now (changes are picked up automatically) |
| `?` | Help: every key, explained |
| `Ctrl-L` | Redraw the screen |
| `q` | Quit |

### Sidebar and songs

The sidebar lists `All Tracks` and every playlist, each with its track count. Whatever is
selected in the sidebar is shown in the songs list, so moving through the sidebar previews each
playlist as you go. Focus starts in the songs list; `Tab`, `h`/`l`, `Enter` or a mouse click
move it, and the focused pane has a coloured border.

`All Tracks` lists everything as "Artist — Title", ordered by artist, album, disc and track
number; files without tags show their file name. Playlists list their tracks in playlist order.

Subfolders inside your music folder are scanned too, but foldly doesn't show the real folder
tree: playlists are how you organise.

### The info pane

Above the queue, the info pane describes the song selected in the songs list (not the one
playing, which is shown above the progress bar): title, artist, album, track and disc number,
length, its file and size, and which other playlists it's in. With the sidebar focused it
summarises the selected source instead: how many tracks, their total length, and how many are
missing. On screens narrower than 100 columns, or quite short, the info pane is hidden and the
queue takes the whole right column.

### Scanning

foldly scans the music folder in the background when it starts, and watches it while it runs:
add, remove or rename music (files or whole folders) and foldly rescans two seconds after the
changes stop, so copying in an album triggers one scan. The songs list's border shows progress, and
the bottom line reports what changed. `u` rescans by hand, e.g. if watching couldn't start (foldly
says so on startup).

The first scan reads every file's tags; later scans skip files whose size and modification time
haven't changed, so they are quick. MP3, FLAC, OGG Vorbis, WAV and M4A (AAC) files are picked
up; hidden files and folders are skipped. Changes to other files, like cover art, are ignored.

### Playlists

- `a` opens a picker for the selected track: choose a playlist, or "New playlist…" to create one
  with the track already in it.
- `A` does the same for every track in the current list, which is usually a filter result. The
  picker title shows how many tracks will be added, and tracks already in the playlist are
  skipped, so re-running "add everything by this artist" only adds new songs.
- In a playlist's songs, `d` removes the selected track and `J`/`K` move it. In the sidebar,
  `d` deletes the selected playlist after a `y`/`n` confirmation. Your files are never touched.
- `N` creates an empty playlist and shows it; `R` renames the playlist being shown.

A playlist can hold the same track more than once (with `a`).

### Filtering

`/` filters the current list as you type, matching artist, title and album. Each word matches on
its own, in any order, and fuzzily: `android radio` and `rhpar` both find "Radiohead — Paranoid
Android". Accents are optional (`beyonce` finds "Beyoncé"). The best match is selected as you
type, so `/` + a few letters + `Enter` plays it.

In `All Tracks` the best matches come first; playlists keep their order. For an exact match, or
to narrow loose results, use fzf's operators:

| Query | Meaning |
|-------|---------|
| `'word` | Exact match |
| `^word` | Starts with |
| `word$` | Ends with |
| `!word` | Exclude |

To add everything by an artist to a playlist: `/`, `'artist name`, `Enter`, `A`.

### Queue, shuffle and repeat

Playing a track queues the rest of the list it came from (the filtered list, if filtering). The
right-hand panel shows the queue: tracks already played (dimmed), the current one (green), then
what's next. It scrolls only once the current track nears the bottom, keeping a few upcoming
tracks in view. The song playing is shown above the progress bar.

- **Shuffle** (`z`): the chosen track plays first, then the rest in random order. Turning it on
  mid-song keeps the song playing and shuffles the rest; turning it off returns to the original
  order from the current song.
- **Repeat** (`r`): *all* starts the queue again after the last track (reshuffled when shuffle is
  on), and `p` on the first track wraps to the last. *One* loops the current track; `n` and `p`
  still move on.
- `p` restarts the current track if more than 3 seconds have played; otherwise it goes back one.

Shuffle and repeat are remembered between runs.

### Missing files

When a file disappears from the music folder, its track drops out of `All Tracks` but stays in
any playlists, greyed out and marked `(missing)`, so moving files around or unplugging a drive
doesn't wipe playlists. When the file comes back at the same path, the track is restored.
Missing tracks are skipped when queueing.

### Messages

The bottom line shows the outcome of the last action (e.g. "added 3 tracks to Mix (20 already
there)", or an error such as a file that couldn't be decoded), replacing the key hints until the
next key press. If the music folder or database can't be opened, foldly prints the error to the
terminal and exits.

### Panel sizes and the mouse

Drag either border between the three columns with the mouse, or press `<` and `>` to move the
border on the right of the focused pane. The songs list always keeps at least a quarter of the
width. Sizes are saved and restored next time.

Clicking a pane focuses it and selects the row under the pointer; the wheel scrolls the pane it's
over.

Because foldly uses the mouse, selecting text on screen needs a modifier in most terminals:
Shift-drag (or Option-drag in macOS Terminal and iTerm2).

### Changing audio output

Switching the system's output (to headphones, or a different speaker) is followed automatically.
If the output device disappears, e.g. it's unplugged, foldly reconnects to the new default and
carries on from the same spot, saying so on the bottom line. If the screen ever looks garbled,
`Ctrl-L` redraws it.

## Data

Everything foldly knows lives in one SQLite file. Your music files are only read. The schema:

```sql
CREATE TABLE tracks (
    id          INTEGER PRIMARY KEY,
    rel_path    TEXT NOT NULL UNIQUE,  -- relative to the music folder
    size        INTEGER NOT NULL,      -- bytes; with mtime, decides whether to re-read tags
    mtime       INTEGER NOT NULL,      -- seconds since the Unix epoch
    title       TEXT,
    artist      TEXT,
    album       TEXT,
    track_no    INTEGER,
    disc_no     INTEGER,
    duration_ms INTEGER,
    missing     INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE playlists (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE
);

CREATE TABLE playlist_tracks (
    id          INTEGER PRIMARY KEY,
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL
);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,  -- music_root, shuffle, repeat, sidebar_width, right_width
    value TEXT NOT NULL
);
```

To inspect your data directly:

```sh
sqlite3 ~/Library/Application\ Support/foldly/foldly.db \
  "SELECT p.name, t.artist, t.title FROM playlist_tracks pt
   JOIN playlists p ON p.id = pt.playlist_id JOIN tracks t ON t.id = pt.track_id
   ORDER BY p.name, pt.position;"
```

## Backing up

Copy the `.db` file. That's every playlist and setting; the tracks table is rebuilt by scanning.
Playlists refer to files by their path inside the music folder, so they keep working if you move
the whole folder (start foldly with the new location), but not if you rename files inside it.

## Design principles

- **The music folder is the library; foldly only reads it.** No tag editing, no reorganising,
  no files written next to your music.
- **Playlists are the folders.** Organising happens in foldly, not in the file system.
- **Nothing is deleted by accident.** Vanished files are marked missing, not removed, and
  deleting a playlist asks first.
- **Keyboard-first and quick.** Every action is a key or two; the filter finds anything in a
  large library in a few keystrokes.
