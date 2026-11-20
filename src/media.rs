//! Media keys and the OS's "Now Playing" display, via souvlaki: MPRIS on Linux (over D-Bus, with
//! the pure-Rust zbus backend), Now Playing / Control Center on macOS.
//!
//! Commands from the OS arrive on another thread and are forwarded through a channel that
//! `App::tick` drains, like scan events. What foldly publishes is only sent when it changes,
//! plus the position about once a second, so the OS isn't flooded with updates.
//!
//! On macOS the commands are delivered through the main thread's run loop, which is why
//! `main.rs` runs that loop and the app on a second thread there.

use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig};

use crate::db::Track;

/// How often the position is republished while playing.
const POSITION_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayState {
    Stopped,
    Paused,
    Playing,
}

pub struct NowPlaying {
    controls: MediaControls,
    events: Receiver<MediaControlEvent>,
    /// What was last published, to send only changes.
    track: Option<(i64, Option<i64>)>,
    state: Option<PlayState>,
    published_at: Instant,
}

impl NowPlaying {
    /// Registers with the OS. Fails where there's nothing to register with (e.g. Linux without
    /// a D-Bus session, or another foldly already holding the name).
    pub fn start() -> Result<Self> {
        let config = PlatformConfig { dbus_name: "foldly", display_name: "foldly", hwnd: None };
        let mut controls = MediaControls::new(config).map_err(|e| anyhow!("media controls: {e:?}"))?;
        let (tx, events) = mpsc::channel();
        controls
            .attach(move |event| {
                let _ = tx.send(event);
            })
            .map_err(|e| anyhow!("media controls: {e:?}"))?;
        Ok(Self { controls, events, track: None, state: None, published_at: Instant::now() })
    }

    /// Commands received since the last call.
    pub fn commands(&self) -> Vec<MediaControlEvent> {
        self.events.try_iter().collect()
    }

    /// Publishes what's playing. Cheap to call every tick: only changes (and the position
    /// about once a second) are sent.
    pub fn publish(&mut self, track: Option<&Track>, state: PlayState, position: Duration) {
        let key = track.map(|t| (t.id, t.entry_id));
        if key != self.track {
            let title = track.map(Track::display_title);
            let metadata = MediaMetadata {
                title: title.as_deref(),
                artist: track.and_then(|t| t.artist.as_deref()),
                album: track.and_then(|t| t.album.as_deref()),
                duration: track.and_then(|t| t.duration),
                cover_url: None,
            };
            let _ = self.controls.set_metadata(metadata);
            self.track = key;
            self.state = None; // the position restarts with a new track
        }
        let due = state == PlayState::Playing && self.published_at.elapsed() >= POSITION_INTERVAL;
        if self.state != Some(state) || due {
            let progress = Some(MediaPosition(position));
            let playback = match state {
                PlayState::Stopped => MediaPlayback::Stopped,
                PlayState::Paused => MediaPlayback::Paused { progress },
                PlayState::Playing => MediaPlayback::Playing { progress },
            };
            let _ = self.controls.set_playback(playback);
            self.state = Some(state);
            self.published_at = Instant::now();
        }
    }
}
