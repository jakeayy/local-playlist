//! Library data model and on-disk persistence.
//!
//! Everything is stored locally as JSON in the platform data directory.
//! Nothing is synced or sent anywhere.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    YouTube,
    SoundCloud,
}

impl Platform {
    pub fn label(self) -> &'static str {
        match self {
            Platform::YouTube => "YouTube",
            Platform::SoundCloud => "SoundCloud",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Platform::YouTube => "yt",
            Platform::SoundCloud => "sc",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    /// Unique id of this entry inside the library (the same song may appear twice).
    pub id: String,
    pub platform: Platform,
    /// Platform specific id (YouTube video id, SoundCloud track id).
    pub source_id: String,
    /// Canonical page URL handed to yt-dlp.
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub duration_secs: Option<u32>,
}

impl Track {
    pub fn new(
        platform: Platform,
        source_id: impl Into<String>,
        url: impl Into<String>,
        title: impl Into<String>,
        artist: impl Into<String>,
        duration_secs: Option<u32>,
    ) -> Self {
        Self {
            id: new_id(),
            platform,
            source_id: source_id.into(),
            url: url.into(),
            title: title.into(),
            artist: artist.into(),
            duration_secs,
        }
    }

    /// Stable key for the audio cache.
    pub fn cache_key(&self) -> String {
        let safe: String = self
            .source_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        format!("{}-{}", self.platform.short(), safe)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub tracks: Vec<Track>,
}

impl Playlist {
    pub fn new(name: impl Into<String>) -> Self {
        Self { id: new_id(), name: name.into(), tracks: Vec::new() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub volume: f32,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// Use the app-managed yt-dlp build, or the one on PATH.
    pub ytdlp_source: crate::ytdlp::Source,
    /// Use the app-managed QuickJS, or JavaScript runtimes on PATH.
    pub js_source: crate::ytdlp::Source,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 0.8,
            shuffle: false,
            repeat: RepeatMode::Off,
            ytdlp_source: Default::default(),
            js_source: Default::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

impl RepeatMode {
    pub fn next(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    pub playlists: Vec<Playlist>,
    pub settings: Settings,
}

impl Library {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("failed to parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    /// Atomically write the library (write to temp file, then rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn playlist(&self, id: &str) -> Option<&Playlist> {
        self.playlists.iter().find(|p| p.id == id)
    }

    pub fn playlist_mut(&mut self, id: &str) -> Option<&mut Playlist> {
        self.playlists.iter_mut().find(|p| p.id == id)
    }
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Application directories (XDG on Linux).
#[derive(Clone, Debug)]
pub struct Paths {
    pub data: PathBuf,
    pub cache: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "local-playlist")
            .context("could not determine home directory")?;
        let paths = Self { data: dirs.data_dir().to_path_buf(), cache: dirs.cache_dir().to_path_buf() };
        std::fs::create_dir_all(&paths.data)?;
        std::fs::create_dir_all(paths.audio_cache())?;
        Ok(paths)
    }

    pub fn library_file(&self) -> PathBuf {
        self.data.join("library.json")
    }

    pub fn audio_cache(&self) -> PathBuf {
        self.cache.join("audio")
    }

    pub fn soundcloud_client_id_file(&self) -> PathBuf {
        self.cache.join("soundcloud_client_id")
    }
}

pub fn format_duration(secs: u32) -> String {
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}
