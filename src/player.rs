//! Thin wrapper around rodio: one output device, one track at a time.

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player as RodioPlayer, Source};

pub struct Player {
    // Must stay alive for the whole run, or audio silently stops.
    device: MixerDeviceSink,
    current: Option<RodioPlayer>,
    volume: f32,
}

impl Player {
    pub fn new() -> Result<Self> {
        // rodio picks a fixed ~50ms buffer that some devices reject (e.g. Macs
        // capped at 960 frames), so let the OS choose first.
        let mut device = DeviceSinkBuilder::from_default_device()
            .and_then(|b| b.with_buffer_size(rodio::cpal::BufferSize::Default).open_stream())
            .or_else(|_| DeviceSinkBuilder::open_default_sink())
            .context("open audio output")?;
        device.log_on_drop(false);
        Ok(Self { device, current: None, volume: 0.8 })
    }

    /// Starts playing `path`, replacing whatever was playing. Returns the
    /// decoder's duration if it knows one.
    pub fn play(&mut self, path: &Path) -> Result<Option<Duration>> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let source = Decoder::try_from(file).with_context(|| format!("decode {}", path.display()))?;
        let duration = source.total_duration();

        // A fresh rodio player per track; dropping the old one stops it.
        let player = RodioPlayer::connect_new(self.device.mixer());
        player.set_volume(self.volume);
        player.append(source);
        self.current = Some(player);
        Ok(duration)
    }

    pub fn stop(&mut self) {
        self.current = None;
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
}
