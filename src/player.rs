//! Thin wrapper around rodio: one output device, one track at a time.
//!
//! Audio stream errors arrive on rodio's audio thread. Instead of letting rodio print them to
//! stderr (which scribbles over the TUI), they are sent to a channel that `handle_errors`
//! drains from the UI loop. If the output device dies (e.g. it was unplugged), the default
//! device is reopened and the current track resumes where it was.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rodio::cpal::{BufferSize, Error as CpalError, ErrorKind};
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player as RodioPlayer, Source};

/// Minimum time between attempts to reopen a failed output device.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(2);

pub struct Player {
    // Must stay alive for the whole run, or audio silently stops.
    device: MixerDeviceSink,
    current: Option<RodioPlayer>,
    current_path: Option<PathBuf>,
    volume: f32,
    errors_tx: Sender<CpalError>,
    errors_rx: Receiver<CpalError>,
    /// The device stopped working and reopening it failed; retried on the next `play`.
    broken: bool,
    last_reconnect: Option<Instant>,
}

impl Player {
    pub fn new() -> Result<Self> {
        let (errors_tx, errors_rx) = mpsc::channel();
        let device = open_device(&errors_tx)?;
        Ok(Self {
            device,
            current: None,
            current_path: None,
            volume: 0.8,
            errors_tx,
            errors_rx,
            broken: false,
            last_reconnect: None,
        })
    }

    /// Starts playing `path`, replacing whatever was playing. Returns the
    /// decoder's duration if it knows one.
    pub fn play(&mut self, path: &Path) -> Result<Option<Duration>> {
        self.start(path, false)
    }

    fn start(&mut self, path: &Path, paused: bool) -> Result<Option<Duration>> {
        if self.broken {
            self.reopen_device()?;
        }
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let source = Decoder::try_from(file).with_context(|| format!("decode {}", path.display()))?;
        let duration = source.total_duration();

        // A fresh rodio player per track; dropping the old one stops it.
        let player = RodioPlayer::connect_new(self.device.mixer());
        player.set_volume(self.volume);
        if paused {
            // Before the source is added, so not a sample is heard.
            player.pause();
        }
        player.append(source);
        self.current = Some(player);
        self.current_path = Some(path.to_path_buf());
        Ok(duration)
    }

    /// Loads `path` paused at `position`, ready for `toggle_pause` to start it: how a saved
    /// session comes back. The player is paused before the track is added, so nothing is heard;
    /// the seek is still carried out by the audio thread while paused.
    pub fn load_paused(&mut self, path: &Path, position: Duration) -> Result<Option<Duration>> {
        let duration = self.start(path, true)?;
        if let Some(p) = &self.current {
            let _ = p.try_seek(position);
        }
        Ok(duration)
    }

    /// Deals with audio stream errors reported since the last call. Returns a message for
    /// the user when something they'd notice happened.
    pub fn handle_errors(&mut self) -> Option<String> {
        let mut fatal = None;
        while let Ok(err) = self.errors_rx.try_recv() {
            match err.kind() {
                // The stream already followed the new default device, or is still running.
                ErrorKind::DeviceChanged | ErrorKind::Xrun | ErrorKind::RealtimeDenied => {}
                _ => fatal = Some(err),
            }
        }
        let err = fatal?;
        if self.last_reconnect.is_some_and(|t| t.elapsed() < RECONNECT_INTERVAL) {
            // Just reconnected and it failed again; try again when the next track starts.
            self.broken = true;
            return None;
        }
        Some(match self.reconnect() {
            Ok(()) => "audio output changed; reconnected to the default device".to_string(),
            Err(reopen) => {
                self.broken = true;
                format!("audio output lost ({err}); retrying on the next track: {reopen:#}")
            }
        })
    }

    /// Reopens the default device and resumes the current track at the same position.
    fn reconnect(&mut self) -> Result<()> {
        self.last_reconnect = Some(Instant::now());
        // Where we were, unless the track had already finished (then `finished()` stays
        // true and the app moves on to the next track on the new device).
        let resume = match (&self.current, &self.current_path) {
            (Some(p), Some(path)) if !p.empty() => Some((path.clone(), p.get_pos(), p.is_paused())),
            _ => None,
        };
        self.reopen_device()?;
        if let Some((path, position, paused)) = resume {
            self.start(&path, paused)?;
            if let Some(p) = &self.current {
                let _ = p.try_seek(position);
            }
        }
        Ok(())
    }

    fn reopen_device(&mut self) -> Result<()> {
        let device = open_device(&self.errors_tx)?;
        // Players are tied to the old device's mixer; drop them before the device.
        if self.current.as_ref().is_some_and(|p| !p.empty()) {
            self.current = None;
        }
        self.device = device;
        self.broken = false;
        Ok(())
    }

    pub fn stop(&mut self) {
        self.current = None;
        self.current_path = None;
    }

    pub fn toggle_pause(&self) {
        if let Some(p) = &self.current {
            if p.is_paused() { p.play() } else { p.pause() }
        }
    }

    pub fn is_paused(&self) -> bool {
        self.current.as_ref().is_some_and(|p| p.is_paused())
    }

    /// True once a started track has played to the end.
    pub fn finished(&self) -> bool {
        self.current.as_ref().is_some_and(|p| p.empty())
    }

    pub fn is_active(&self) -> bool {
        self.current.is_some()
    }

    pub fn position(&self) -> Duration {
        self.current.as_ref().map_or(Duration::ZERO, |p| p.get_pos())
    }

    pub fn seek_to(&self, position: Duration) {
        if let Some(p) = &self.current {
            let _ = p.try_seek(position);
        }
    }

    pub fn seek_by(&self, delta_secs: i64) {
        if let Some(p) = &self.current {
            let pos = p.get_pos().as_secs_f64() + delta_secs as f64;
            let _ = p.try_seek(Duration::from_secs_f64(pos.max(0.0)));
        }
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn change_volume(&mut self, delta: f32) {
        self.volume = (self.volume + delta).clamp(0.0, 1.5);
        if let Some(p) = &self.current {
            p.set_volume(self.volume);
        }
    }

    /// Feeds an error in as if the audio thread had reported it.
    #[cfg(test)]
    pub fn inject_error(&self, kind: ErrorKind) {
        let _ = self.errors_tx.send(CpalError::new(kind));
    }
}

/// Opens the default output device, sending stream errors to `errors`.
fn open_device(errors: &Sender<CpalError>) -> Result<MixerDeviceSink> {
    let tx = errors.clone();
    let on_error = move |err: CpalError| {
        let _ = tx.send(err);
    };
    // rodio picks a fixed ~50ms buffer that some devices reject (e.g. Macs capped at 960
    // frames), so let the OS choose first, then fall back to rodio's choice and other configs.
    let mut device = DeviceSinkBuilder::from_default_device()
        .and_then(|b| b.with_buffer_size(BufferSize::Default).with_error_callback(on_error.clone()).open_stream())
        .or_else(|_| DeviceSinkBuilder::from_default_device()?.with_error_callback(on_error).open_sink_or_fallback())
        .context("open audio output")?;
    device.log_on_drop(false);
    Ok(device)
}
