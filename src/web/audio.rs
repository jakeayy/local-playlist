//! Browser playback through an `<audio>` element. The server streams the
//! audio (and supports range requests once a file is complete), so the
//! browser handles buffering and seeking itself.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use web_sys::HtmlAudioElement;

use crate::download::Download;

pub const PREBUFFER_SECS: f32 = 3.0;

pub enum Input {
    /// Desktop-only.
    File(PathBuf),
    /// Desktop-only.
    Stream(Arc<Download>),
    /// Stream URL served by the Local Playlist server.
    Url(String),
}

/// What is being played, for visualizations. Not wired up in the browser yet.
#[derive(Clone)]
pub struct Tap;

impl Tap {
    /// Same contract as the desktop tap; always silent here.
    pub fn latest(&self, out: &mut [f32]) -> (u32, u64) {
        out.fill(0.0);
        (44_100, 0)
    }
}

pub struct Audio {
    el: HtmlAudioElement,
    loaded: bool,
    reported_error: bool,
}

impl Audio {
    pub fn new(volume: f32) -> Result<Self> {
        let el = HtmlAudioElement::new().map_err(|_| anyhow::anyhow!("audio playback isn't available"))?;
        el.set_preload("auto");
        el.set_volume(volume as f64);
        Ok(Self { el, loaded: false, reported_error: false })
    }

    pub fn load(&mut self, input: Input) {
        self.stop();
        if let Input::Url(url) = input {
            self.el.set_src(&url);
            let _ = self.el.play();
            self.loaded = true;
            self.reported_error = false;
        }
    }

    pub fn poll(&mut self) -> Option<String> {
        if !self.loaded || self.reported_error {
            return None;
        }
        let err = self.el.error()?;
        self.reported_error = true;
        self.loaded = false;
        Some(match err.code() {
            2 => "network error while streaming".to_string(),
            3 => "the audio couldn't be decoded".to_string(),
            4 => "the server couldn't provide this track".to_string(),
            _ => "playback failed".to_string(),
        })
    }

    pub fn stop(&mut self) {
        let _ = self.el.pause();
        self.el.remove_attribute("src").ok();
        self.el.load();
        self.loaded = false;
    }

    pub fn play(&self) {
        let _ = self.el.play();
    }

    pub fn pause(&self) {
        let _ = self.el.pause();
    }

    pub fn is_paused(&self) -> bool {
        self.el.paused()
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// HAVE_FUTURE_DATA (3) means playback can continue.
    pub fn is_buffering(&self) -> bool {
        self.loaded && !self.el.paused() && self.el.ready_state() < 3
    }

    pub fn seek(&self, pos: Duration) {
        self.el.set_current_time(pos.as_secs_f64());
    }

    pub fn position(&self) -> Duration {
        Duration::from_secs_f64(self.el.current_time().max(0.0))
    }

    pub fn duration(&self) -> Option<Duration> {
        let d = self.el.duration();
        (d.is_finite() && d > 0.0).then(|| Duration::from_secs_f64(d))
    }

    pub fn set_volume(&self, volume: f32) {
        self.el.set_volume(volume.clamp(0.0, 1.0) as f64);
    }

    pub fn tap(&self) -> Tap {
        Tap
    }

    pub fn finished(&self) -> bool {
        self.loaded && self.el.ended()
    }
}
