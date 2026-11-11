//! Web stand-in for the desktop component manager: in the browser, yt-dlp
//! runs on the server, so there is nothing to install here.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::model::{Paths, Track};
use crate::secrets::StoredCookie;

pub const AUDIO_EXTS: &[&str] = &["m4a", "mp4", "mp3", "ogg", "oga", "flac", "wav", "aac", "webm", "opus"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    #[default]
    Managed,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Component {
    YtDlp,
    JsRuntime,
}

impl Component {
    pub fn label(self) -> &'static str {
        match self {
            Component::YtDlp => "yt-dlp",
            Component::JsRuntime => "QuickJS",
        }
    }

    pub fn approx_mb(self) -> u32 {
        0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installed {
    pub tag: String,
    pub commit: Option<String>,
    pub kind: String,
    pub exe: String,
}

impl Installed {
    pub fn short(&self) -> String {
        self.tag.clone()
    }
}

#[derive(Clone, Debug)]
pub enum State {
    Managed(Installed),
    System(String),
    Missing,
}

#[derive(Clone, Debug)]
pub struct Status {
    pub ytdlp: State,
    pub js: State,
}

#[derive(Clone)]
pub struct YtDlp {
    pub ytdlp_source: Source,
    pub js_source: Source,
}

impl YtDlp {
    pub fn new(_: &Paths, ytdlp_source: Source, js_source: Source) -> Self {
        Self { ytdlp_source, js_source }
    }

    pub fn installed(&self, _: Component) -> Option<Installed> {
        None
    }

    pub fn missing_managed(&self) -> Vec<Component> {
        Vec::new()
    }

    pub fn status(&self) -> Status {
        let server = || State::System("on the server".into());
        Status { ytdlp: server(), js: server() }
    }

    pub fn install(&self, _: Component, _: &dyn Fn(String, Option<f32>)) -> Result<Option<Installed>> {
        bail!("components are managed by the server")
    }

    pub fn download(
        &self,
        _: &Track,
        _: &Path,
        _: Option<&[StoredCookie]>,
        _: Option<Arc<AtomicBool>>,
        _: impl Fn(f32),
    ) -> Result<PathBuf> {
        bail!("downloads aren't available in the web version")
    }
}

pub fn needs_login(_: &str) -> bool {
    false
}
