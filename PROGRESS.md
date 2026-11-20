# foldly progress

## Done

- [x] Terminal UI (ratatui): browser, now-playing panel, progress bar, context help line
- [x] Playback (rodio): play/pause, next/previous, seek, volume, stop, auto-advance
- [x] Formats: MP3, FLAC, OGG Vorbis, WAV, M4A (AAC)
- [x] Fix: let the OS choose the audio buffer size (some Macs reject rodio's default)
- [x] One music folder, scanned recursively into SQLite in the background
- [x] Incremental rescans (size + modified time); vanished files marked missing, not deleted
- [x] Virtual folders: top level shows `All Tracks/` plus one folder per playlist
- [x] Playlists: create, rename, delete, add track (picker), remove track, reorder (J/K)
- [x] `/` fuzzy filter (nucleo, fzf syntax): words match independently, ranked in All Tracks,
      playlist order kept; Enter plays the top match
- [x] `A` adds every track shown to a playlist, skipping ones already in it
- [x] Shuffle (`z`): chosen track first, rest random; toggling mid-track keeps the current
      track and restores original order when turned off; remembered between runs
- [x] Queue panel on the right (recent history, current, up next); current song shown above
      the progress bar
- [x] `?` help popup listing every key (scrolls on short terminals)
- [x] Repeat (`r`): off / all (restarts the queue, reshuffled if shuffle is on) / one;
      remembered between runs. Rescan moved to `u`
- [x] Music folder remembered between runs
- [x] One-line install (`install.sh`) from GitHub releases built by CI for Linux and macOS,
      x86_64 and arm64; `--version` / `--help`

## Next

- [ ] Remember the last view, track and position between runs
- [ ] Media keys and OS Now Playing via `souvlaki` (MPRIS on Linux, Now Playing on macOS)
- [ ] Gapless playback (append the next track to the rodio player before the current one ends)

## Playlists, more

- [ ] Mark tracks by hand (`m`, `*` marks all shown) and add the marked ones
- [ ] Add the currently playing track to a playlist without finding it in the list
- [ ] Export and import M3U, so playlists work in other players and survive losing the database
- [ ] Smart playlists from rules (e.g. artist is X, recently added, never played)
- [ ] Nested playlist folders (playlists inside playlist folders)
- [ ] Relink missing tracks when a file is renamed (match on tags + duration)
- [ ] Changing the music folder: offer to remap stored paths instead of marking everything missing

## Library

- [ ] Virtual folders by Artist and Album, built from tags; `a` on any folder adds all of it
- [ ] A real-folder view of the music root's subfolders, for when tags are poor
- [ ] Play counts, last played, ratings or favourites
- [ ] Watch the music folder for changes (`notify`) instead of rescanning on start and `r`

## UI and polish

- [ ] Cover art in the now-playing panel (`ratatui-image`)
- [ ] Show track durations in the list
- [ ] Make the queue panel interactive: focus it (Tab), jump to a track, remove, reorder, clear
- [ ] Config file: keybindings, colours, seek step, start view
- [ ] Silence ALSA's stderr warnings on Linux if they garble the screen
- [ ] Try foldly on a real Linux desktop (CI builds and runs the tests, but not audio)
