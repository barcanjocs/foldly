//! The play queue: the tracks in play order, the same tracks in their original list order, the
//! current track, and the queue pane's cursor.
//!
//! Invariant: `unshuffled` holds exactly the tracks of `tracks`; with shuffle off, in the same
//! order. Toggling shuffle off restores `tracks` from `unshuffled`, so every edit here keeps the
//! two in step. Playing audio isn't this module's job: `App` asks it what to play and tells it
//! what's playing (`set_current`).

use std::collections::HashSet;

use crate::db::Track;

/// Identifies a queue item. The entry id (set inside a playlist) tells apart repeats of the same
/// track; queues are built from one list, so keys are unique within a queue.
pub type Key = (i64, Option<i64>);

pub fn key(t: &Track) -> Key {
    (t.id, t.entry_id)
}

/// Why the selected track wasn't removed.
#[derive(Debug, PartialEq, Eq)]
pub enum RemoveError {
    /// The current track can't be removed (skip it instead), so `current` always points at what's
    /// playing.
    Playing,
    /// Nothing is loaded, so there's no cursor.
    Nothing,
}

#[derive(Default)]
pub struct Queue {
    tracks: Vec<Track>,
    unshuffled: Vec<Track>,
    current: Option<usize>,
    /// The cursor; `None` means "on the current track", where it goes back to whenever the
    /// queue is rebuilt.
    selected: Option<usize>,
}

impl Queue {
    /// The tracks in play order.
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// The tracks in the order of the list they came from.
    #[cfg(test)]
    pub fn unshuffled(&self) -> &[Track] {
        &self.unshuffled
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn get(&self, index: usize) -> Option<&Track> {
        self.tracks.get(index)
    }

    /// Where the playing (or loaded) track is in play order.
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// Set by `App` once a track actually starts (or `None` when stopped).
    pub fn set_current(&mut self, index: Option<usize>) {
        self.current = index.filter(|&i| i < self.tracks.len());
    }

    /// Replaces the queue with `list`, to play the track with key `chosen`. Shuffled, `chosen`
    /// comes first and the rest follow in random order. Returns the index to play.
    pub fn start(&mut self, list: Vec<Track>, chosen: Key, shuffle: bool) -> usize {
        let start = list.iter().position(|t| key(t) == chosen).unwrap_or(0);
        self.tracks = if shuffle { shuffled_from(&list, start) } else { list.clone() };
        self.unshuffled = list;
        self.current = None;
        self.selected = None;
        if shuffle { 0 } else { start }
    }

    /// Shuffle was switched on or off. The current track stays current: on, everything else is
    /// shuffled to play after it; off, the list order comes back, continuing from it.
    pub fn set_shuffle(&mut self, on: bool) {
        self.selected = None;
        let Some(current) = self.current_key() else { return };
        let position = self.unshuffled.iter().position(|t| key(t) == current).unwrap_or(0);
        if on {
            self.tracks = shuffled_from(&self.unshuffled, position);
            self.current = Some(0);
        } else {
            self.tracks = self.unshuffled.clone();
            self.current = Some(position);
        }
    }

    /// Repeat-all with shuffle: a fresh random order for the next time round.
    pub fn reshuffle(&mut self) {
        if !self.unshuffled.is_empty() {
            self.tracks = shuffled_from(&self.unshuffled, fastrand::usize(..self.unshuffled.len()));
            self.selected = None;
        }
    }

    fn current_key(&self) -> Option<Key> {
        self.current.and_then(|i| self.tracks.get(i)).map(key)
    }

    // --- the cursor ---------------------------------------------------------------------------

    /// The queue pane's selected row: the chosen one, else the current track. `None` when
    /// nothing is loaded.
    pub fn cursor(&self) -> Option<usize> {
        self.current?;
        self.selected.filter(|&i| i < self.tracks.len()).or(self.current)
    }

    pub fn cursor_track(&self) -> Option<&Track> {
        self.cursor().and_then(|i| self.tracks.get(i))
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if let Some(cursor) = self.cursor() {
            let last = self.tracks.len() as isize - 1;
            self.selected = Some((cursor as isize).saturating_add(delta).clamp(0, last) as usize);
        }
    }

    /// Puts the cursor on `index` (a click); ignored with nothing loaded or out of range.
    pub fn select(&mut self, index: usize) -> bool {
        let ok = self.current.is_some() && index < self.tracks.len();
        if ok {
            self.selected = Some(index);
        }
        ok
    }

    /// The cursor goes back to following the current track.
    pub fn follow_current(&mut self) {
        self.selected = None;
    }

    // --- editing ------------------------------------------------------------------------------

    /// Removes the track under the cursor from both orders.
    pub fn remove_selected(&mut self) -> Result<(), RemoveError> {
        let (Some(i), Some(current)) = (self.cursor(), self.current) else { return Err(RemoveError::Nothing) };
        if i == current {
            return Err(RemoveError::Playing);
        }
        let removed = key(&self.tracks.remove(i));
        self.unshuffled.retain(|t| key(t) != removed);
        if i < current {
            self.current = Some(current - 1);
        }
        self.selected = Some(i.min(self.tracks.len() - 1));
        Ok(())
    }

    /// Moves the track under the cursor later (+1) or earlier (-1); the cursor moves with it.
    /// With shuffle off both orders change; with it on only the play order does, so turning
    /// shuffle off still gives back the list's order.
    pub fn move_selected(&mut self, delta: isize, shuffle: bool) {
        let (Some(i), Some(current)) = (self.cursor(), self.current) else { return };
        let Some(j) = i.checked_add_signed(delta).filter(|&j| j < self.tracks.len()) else { return };
        self.tracks.swap(i, j);
        if !shuffle {
            self.unshuffled.swap(i, j);
        }
        self.current = Some(if current == i {
            j
        } else if current == j {
            i
        } else {
            current
        });
        self.selected = Some(j);
    }

    /// Drops everything after the current track; returns how many were dropped.
    pub fn clear_upcoming(&mut self) -> usize {
        let Some(current) = self.current else { return 0 };
        let dropped = self.tracks.len() - current - 1;
        self.tracks.truncate(current + 1);
        let kept: HashSet<Key> = self.tracks.iter().map(key).collect();
        self.unshuffled.retain(|t| kept.contains(&key(t)));
        self.selected = None;
        dropped
    }

    // --- saving -------------------------------------------------------------------------------

    /// Both orders as `encode_keys` strings, play order first.
    pub fn encode(&self) -> (String, String) {
        (encode_keys(&self.tracks), encode_keys(&self.unshuffled))
    }

    /// Rebuilds a saved queue from `encode` strings, looking tracks up with `find` (which
    /// should return `None` for tracks that are gone or missing; they're dropped). Returns the
    /// queue and where the track saved as current (at `current` in the saved play order) ended
    /// up, if it survived. The returned queue has no current track yet: `App` sets it once the
    /// track has loaded.
    pub fn restore(
        order: &str,
        unshuffled: Option<&str>,
        current: Option<usize>,
        find: impl Fn(i64) -> Option<Track>,
    ) -> (Queue, Option<usize>) {
        let rebuild = |keys: &str| -> Vec<Track> {
            decode_keys(keys).into_iter().filter_map(|(id, entry_id)| Some(Track { entry_id, ..find(id)? })).collect()
        };
        let tracks = rebuild(order);
        let unshuffled = unshuffled.map_or_else(|| tracks.clone(), rebuild);
        let saved_current = current.and_then(|i| decode_keys(order).get(i).copied());
        let at = saved_current.and_then(|k| tracks.iter().position(|t| key(t) == k));
        (Queue { tracks, unshuffled, current: None, selected: None }, at)
    }
}

/// `tracks[first]` followed by all the other tracks in random order.
fn shuffled_from(tracks: &[Track], first: usize) -> Vec<Track> {
    let mut rest: Vec<Track> = tracks.iter().enumerate().filter(|&(i, _)| i != first).map(|(_, t)| t.clone()).collect();
    fastrand::shuffle(&mut rest);
    tracks.get(first).cloned().into_iter().chain(rest).collect()
}

/// Queue items as `id` or `id:entry` (inside a playlist), comma-separated.
fn encode_keys(tracks: &[Track]) -> String {
    let key = |t: &Track| match t.entry_id {
        Some(entry) => format!("{}:{entry}", t.id),
        None => t.id.to_string(),
    };
    tracks.iter().map(key).collect::<Vec<_>>().join(",")
}

/// The reverse of `encode_keys`, skipping anything malformed.
fn decode_keys(keys: &str) -> Vec<Key> {
    keys.split(',')
        .filter_map(|key| match key.split_once(':') {
            Some((id, entry)) => Some((id.parse().ok()?, Some(entry.parse().ok()?))),
            None => Some((key.parse().ok()?, None)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: i64) -> Track {
        Track {
            id,
            rel_path: format!("{id}.mp3"),
            title: None,
            artist: None,
            album: None,
            duration: None,
            missing: false,
            entry_id: None,
        }
    }

    fn ids(list: &[Track]) -> Vec<i64> {
        list.iter().map(|t| t.id).collect()
    }

    /// A queue of 1..=n in list order, playing `playing` (an index).
    fn playing(n: i64, playing: usize, shuffle: bool) -> Queue {
        let mut q = Queue::default();
        let list: Vec<Track> = (1..=n).map(track).collect();
        let chosen = key(&list[playing]);
        let at = q.start(list, chosen, shuffle);
        q.set_current(Some(at));
        q
    }

    #[test]
    fn start_plays_the_chosen_track_first_when_shuffled() {
        let q = playing(5, 2, false);
        assert_eq!((ids(q.tracks()), q.current()), (vec![1, 2, 3, 4, 5], Some(2)));
        for _ in 0..20 {
            let q = playing(50, 7, true);
            assert_eq!((q.tracks()[0].id, q.current()), (8, Some(0)));
            let mut sorted = ids(q.tracks());
            sorted.sort();
            assert_eq!(sorted, (1..=50).collect::<Vec<_>>(), "same tracks, each once");
            assert_eq!(ids(q.unshuffled()), (1..=50).collect::<Vec<_>>());
        }
        assert_ne!(ids(playing(50, 0, true).tracks()), (1..=50).collect::<Vec<_>>(), "actually shuffled");
    }

    #[test]
    fn toggling_shuffle_keeps_the_current_track() {
        let mut q = playing(6, 3, false);
        q.set_shuffle(true);
        assert_eq!((q.tracks()[0].id, q.current()), (4, Some(0)));
        q.set_shuffle(false);
        assert_eq!((ids(q.tracks()), q.current()), (vec![1, 2, 3, 4, 5, 6], Some(3)));
    }

    #[test]
    fn the_cursor_follows_the_current_track_until_moved() {
        let mut q = playing(5, 1, false);
        assert_eq!(q.cursor(), Some(1));
        q.move_cursor(2);
        assert_eq!(q.cursor(), Some(3));
        q.move_cursor(isize::MAX);
        assert_eq!(q.cursor(), Some(4), "clamped");
        q.set_shuffle(true);
        assert_eq!(q.cursor(), q.current(), "a rebuild resets it");
        assert!(!q.select(99));
        assert_eq!(Queue::default().cursor(), None, "nothing loaded, no cursor");
    }

    #[test]
    fn removing_keeps_the_current_track_playing() {
        let mut q = playing(5, 2, false);
        q.move_cursor(-2);
        assert_eq!(q.remove_selected(), Ok(()));
        assert_eq!((ids(q.tracks()), q.current()), (vec![2, 3, 4, 5], Some(1)), "index follows track 3");
        assert_eq!(ids(q.unshuffled()), vec![2, 3, 4, 5]);
        q.follow_current();
        assert_eq!(q.remove_selected(), Err(RemoveError::Playing));
        q.move_cursor(3);
        assert_eq!(q.remove_selected(), Ok(()));
        assert_eq!((ids(q.tracks()), q.cursor()), (vec![2, 3, 4], Some(2)), "cursor stays in range");
        assert_eq!(Queue::default().remove_selected(), Err(RemoveError::Nothing));
    }

    #[test]
    fn moving_with_shuffle_off_changes_both_orders() {
        let mut q = playing(4, 0, false);
        q.move_selected(1, false);
        assert_eq!((ids(q.tracks()), q.current(), q.cursor()), (vec![2, 1, 3, 4], Some(1), Some(1)));
        assert_eq!(ids(q.unshuffled()), ids(q.tracks()));
        q.move_cursor(1);
        q.move_selected(-1, false);
        assert_eq!((ids(q.tracks()), q.current()), (vec![2, 3, 1, 4], Some(2)), "swapped past the current");
        q.move_cursor(isize::MAX);
        q.move_selected(1, false);
        assert_eq!(ids(q.tracks()), vec![2, 3, 1, 4], "can't move past the end");
    }

    #[test]
    fn moving_with_shuffle_on_only_changes_the_play_order() {
        let mut q = playing(5, 0, true);
        let order = ids(q.tracks());
        q.move_selected(1, true);
        assert_eq!(q.current(), Some(1));
        assert_eq!(ids(q.unshuffled()), vec![1, 2, 3, 4, 5]);
        assert_eq!((ids(q.tracks())[0], ids(q.tracks())[1]), (order[1], order[0]));
        q.set_shuffle(false);
        assert_eq!((ids(q.tracks()), q.current()), (vec![1, 2, 3, 4, 5], Some(0)));
    }

    #[test]
    fn clearing_upcoming_keeps_history_and_current() {
        // Shuffled, the chosen track plays first; two tracks later, it's history.
        let mut q = playing(5, 0, true);
        q.set_current(Some(2));
        let kept = ids(&q.tracks()[..3]);
        assert_eq!(q.clear_upcoming(), 2);
        assert_eq!((ids(q.tracks()), q.current()), (kept.clone(), Some(2)));
        let mut unshuffled = ids(q.unshuffled());
        unshuffled.sort();
        let mut expected = kept;
        expected.sort();
        assert_eq!(unshuffled, expected, "the same tracks in both orders");
        assert_eq!(q.clear_upcoming(), 0);
    }

    #[test]
    fn keys_round_trip() {
        let tracks = vec![track(3), Track { entry_id: Some(17), ..track(5) }, track(9)];
        assert_eq!(encode_keys(&tracks), "3,5:17,9");
        assert_eq!(decode_keys("3,5:17,9"), [(3, None), (5, Some(17)), (9, None)]);
        assert_eq!(decode_keys(""), []);
        assert_eq!(decode_keys("4,x,5:y,6:7"), [(4, None), (6, Some(7))], "malformed keys skipped");
    }

    #[test]
    fn restore_drops_lost_tracks_and_finds_the_current_one() {
        let q = playing(4, 2, true);
        let (order, unshuffled) = q.encode();
        let current = q.current();
        // Track 2 has gone missing since.
        let find = |id: i64| (id != 2).then(|| track(id));
        let (restored, at) = Queue::restore(&order, Some(&unshuffled), current, find);
        assert_eq!(ids(restored.unshuffled()), vec![1, 3, 4]);
        assert_eq!(restored.tracks().len(), 3);
        assert_eq!(at.map(|i| restored.tracks()[i].id), Some(3), "the saved current track, wherever it is now");
        assert_eq!(restored.current(), None, "set once it has loaded");

        let (_, at) = Queue::restore(&order, Some(&unshuffled), current, |id| (id != 3).then(|| track(id)));
        assert_eq!(at, None, "the current track itself is gone");
        let (old, _) = Queue::restore("1,2", None, Some(0), |id| Some(track(id)));
        assert_eq!(ids(old.unshuffled()), vec![1, 2], "no saved list order: the play order");
    }
}
