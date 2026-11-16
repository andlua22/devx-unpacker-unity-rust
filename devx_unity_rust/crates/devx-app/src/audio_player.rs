//! Cross-platform `rodio` 0.23 playback engine for Unity `AudioClip` PCM streams.

use devx_plugin_api::AssetRef;
use rodio::buffer::SamplesBuffer;
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};
use std::num::NonZero;

/// Manages lazy audio device initialization and single-clip playback state.
pub struct AudioPlaybackEngine {
    sink: Option<MixerDeviceSink>,
    player: Option<Player>,
    active_asset: Option<AssetRef>,
    volume: f32,
    last_error: Option<String>,
}

impl Default for AudioPlaybackEngine {
    fn default() -> Self {
        Self {
            sink: None,
            player: None,
            active_asset: None,
            volume: 0.85,
            last_error: None,
        }
    }
}

impl AudioPlaybackEngine {
    /// Starts playback of interleaved signed 16-bit PCM samples for `asset_ref`.
    pub fn play_i16(
        &mut self,
        asset_ref: AssetRef,
        sample_rate: u32,
        channels: u16,
        samples_i16: &[i16],
    ) {
        self.last_error = None;
        let Some(sr) = NonZero::new(sample_rate) else {
            self.last_error = Some("Audio clip reports invalid 0 Hz sample rate.".to_owned());
            return;
        };
        let Some(ch) = NonZero::new(channels) else {
            self.last_error = Some("Audio clip reports 0 channels.".to_owned());
            return;
        };
        if samples_i16.is_empty() {
            self.last_error = Some("Audio clip contains no decoded PCM samples.".to_owned());
            return;
        }

        if let Err(err) = self.ensure_output() {
            self.last_error = Some(err);
            return;
        }

        let Some(player) = self.player.as_ref() else {
            return;
        };
        player.stop();

        let float_samples: Vec<f32> = samples_i16
            .iter()
            .map(|&s| f32::from(s) / 32768.0)
            .collect();
        let buffer = SamplesBuffer::new(ch, sr, float_samples);
        player.set_volume(self.volume);
        player.append(buffer);
        player.play();
        self.active_asset = Some(asset_ref);
    }

    pub fn toggle_pause(&mut self) {
        if let Some(player) = self.player.as_ref() {
            if player.is_paused() {
                player.play();
            } else {
                player.pause();
            }
        }
    }

    pub fn stop(&mut self) {
        if let Some(player) = self.player.as_ref() {
            player.stop();
        }
        self.active_asset = None;
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.5);
        if let Some(player) = self.player.as_ref() {
            player.set_volume(self.volume);
        }
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn active_asset(&self) -> Option<AssetRef> {
        self.active_asset
    }

    pub fn is_paused(&self) -> bool {
        self.player.as_ref().is_some_and(Player::is_paused)
    }

    pub fn is_active(&self) -> bool {
        self.player
            .as_ref()
            .is_some_and(|p| !p.empty() && self.active_asset.is_some())
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    fn ensure_output(&mut self) -> Result<(), String> {
        if self.sink.is_some() && self.player.is_some() {
            return Ok(());
        }
        let mut sink = DeviceSinkBuilder::open_default_sink()
            .map_err(|e| format!("Audio output device unavailable: {e}"))?;
        sink.log_on_drop(false);
        let player = Player::connect_new(sink.mixer());
        self.sink = Some(sink);
        self.player = Some(player);
        Ok(())
    }
}
