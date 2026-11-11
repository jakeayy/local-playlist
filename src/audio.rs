//! Local audio playback of downloaded files using rodio (symphonia decoders).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result};
use rodio::{Decoder, MixerDeviceSink, Player, Source as _};

pub struct Audio {
    // Must be kept alive for as long as audio should play.
    _sink: MixerDeviceSink,
    player: Player,
    duration: Option<Duration>,
    loaded: bool,
}

impl Audio {
    pub fn new(volume: f32) -> Result<Self> {
        let mut sink = rodio::DeviceSinkBuilder::open_default_sink()
            .context("could not open the default audio output device")?;
        sink.log_on_drop(false);
        let player = Player::connect_new(sink.mixer());
        player.set_volume(volume);
        Ok(Self { _sink: sink, player, duration: None, loaded: false })
    }

    /// Replace whatever is playing with `path` and start playback.
    pub fn load(&mut self, path: &Path) -> Result<()> {
        let file = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let decoder = Decoder::try_from(file).context("unsupported or corrupt audio file")?;
        self.duration = decoder.total_duration();
        self.player.clear();
        self.player.append(decoder);
        self.player.play();
        self.loaded = true;
        Ok(())
    }

    pub fn stop(&mut self) {
        self.player.clear();
        self.loaded = false;
        self.duration = None;
    }

    pub fn play(&self) {
        self.player.play();
    }

    pub fn pause(&self) {
        self.player.pause();
    }

    pub fn is_paused(&self) -> bool {
        self.player.is_paused()
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn seek(&self, pos: Duration) {
        if let Err(e) = self.player.try_seek(pos) {
            eprintln!("seek failed: {e}");
        }
    }

    pub fn position(&self) -> Duration {
        if self.loaded { self.player.get_pos() } else { Duration::ZERO }
    }

    pub fn duration(&self) -> Option<Duration> {
        self.duration
    }

    pub fn set_volume(&self, volume: f32) {
        self.player.set_volume(volume);
    }

    /// True once a loaded track has played to the end.
    pub fn finished(&self) -> bool {
        self.loaded && self.player.empty()
    }
}
