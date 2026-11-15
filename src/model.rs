//! Library data model and on-disk persistence.
//!
//! Everything is stored locally as JSON in the platform data directory.
//! Nothing is synced or sent anywhere.

use std::path::{Path, PathBuf};

#[cfg(not(target_family = "wasm"))]
use anyhow::Context as _;
use anyhow::Result;
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

    /// Whether every search term occurs in the title or artist (terms lowercase).
    pub fn matches(&self, terms: &[String]) -> bool {
        let (title, artist) = (self.title.to_lowercase(), self.artist.to_lowercase());
        terms.iter().all(|t| title.contains(t.as_str()) || artist.contains(t.as_str()))
    }

    /// File name stem for this track's audio: `<Platform>-<id>`.
    pub fn file_key(&self) -> String {
        let mut key = String::with_capacity(self.platform.label().len() + 1 + self.source_id.len());
        self.write_file_key(&mut key);
        key
    }

    /// [`Track::file_key`] into `out` (cleared first), to check many tracks
    /// without allocating for each.
    pub fn write_file_key(&self, out: &mut String) {
        out.clear();
        out.push_str(self.platform.label());
        out.push('-');
        out.extend(
            self.source_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }),
        );
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    /// Folder name under the music root (follows `name`, kept unique).
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub tracks: Vec<Track>,
}

impl Playlist {
    pub fn new(name: impl Into<String>) -> Self {
        Self { id: new_id(), name: name.into(), folder: None, tracks: Vec::new() }
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
    /// Root folder for playlist folders; `None` = `<Music>/local-playlist`.
    pub music_dir: Option<PathBuf>,
    /// Size limit of the removed-tracks cache, in MB.
    pub removed_cache_mb: u64,
    /// System / light / dark.
    pub appearance: Appearance,
    /// Tint the theme with the playing track's cover colour.
    pub cover_colors: bool,
    /// Skip SponsorBlock segments in YouTube tracks.
    pub sponsorblock: bool,
    /// SponsorBlock categories to skip (see `sponsorblock::CATEGORIES`).
    pub sponsorblock_categories: Vec<String>,
    /// Desktop: closing the window keeps the app playing, with a tray icon.
    pub background: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 0.8,
            shuffle: false,
            repeat: RepeatMode::Off,
            ytdlp_source: Default::default(),
            js_source: Default::default(),
            music_dir: None,
            removed_cache_mb: 2048,
            appearance: Default::default(),
            cover_colors: true,
            sponsorblock: true,
            sponsorblock_categories: crate::sponsorblock::default_categories(),
            background: true,
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

#[cfg(target_family = "wasm")]
const WEB_LIBRARY_KEY: &str = "local-playlist.library";

/// In the browser the library lives in localStorage.
#[cfg(target_family = "wasm")]
impl Library {
    pub fn load(_: &Path) -> Result<Self> {
        match crate::web::storage_get(WEB_LIBRARY_KEY) {
            Some(json) => Ok(serde_json::from_str(&json)?),
            None => Ok(Self::default()),
        }
    }

    pub fn save(&self, _: &Path) -> Result<()> {
        crate::web::storage_set(WEB_LIBRARY_KEY, &serde_json::to_string(self)?)
    }
}

impl Library {
    pub fn playlist(&self, id: &str) -> Option<&Playlist> {
        self.playlists.iter().find(|p| p.id == id)
    }

    pub fn playlist_mut(&mut self, id: &str) -> Option<&mut Playlist> {
        self.playlists.iter_mut().find(|p| p.id == id)
    }
}

#[cfg(not(target_family = "wasm"))]
impl Library {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("failed to parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    /// Atomically write the library (write to temp file, then rename). The
    /// JSON is streamed to the file rather than built in memory first.
    pub fn save(&self, path: &Path) -> Result<()> {
        use std::io::Write as _;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let mut out = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        serde_json::to_writer_pretty(&mut out, self)?;
        out.flush()?;
        drop(out);
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(not(target_family = "wasm"))]
pub fn new_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("system random number generator unavailable");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(target_family = "wasm")]
pub fn new_id() -> String {
    (0..4).map(|_| format!("{:08x}", (js_sys::Math::random() * u32::MAX as f64) as u32)).collect()
}

/// Application directories (XDG on Linux).
#[derive(Clone, Debug)]
pub struct Paths {
    pub data: PathBuf,
    pub cache: PathBuf,
}

impl Paths {
    /// Placeholder paths for the browser build (nothing touches the disk there).
    pub fn web() -> Self {
        Self { data: PathBuf::from("/"), cache: PathBuf::from("/") }
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn new() -> Result<Self> {
        let dirs =
            directories::ProjectDirs::from("", "", "local-playlist").context("could not determine home directory")?;
        let paths = Self { data: dirs.data_dir().to_path_buf(), cache: dirs.cache_dir().to_path_buf() };
        std::fs::create_dir_all(&paths.data)?;
        Ok(paths)
    }

    pub fn library_file(&self) -> PathBuf {
        self.data.join("library.json")
    }

    pub fn soundcloud_client_id_file(&self) -> PathBuf {
        self.cache.join("soundcloud_client_id")
    }
}

pub fn format_duration(secs: u32) -> String {
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// Light/dark choice (System follows the OS).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_matches_title_and_artist() {
        let t = Track::new(Platform::YouTube, "x", "u", "Never Gonna Give You Up", "Rick Astley", None);
        let q = |s: &str| s.split_whitespace().map(str::to_lowercase).collect::<Vec<_>>();
        assert!(t.matches(&q("")));
        assert!(t.matches(&q("gonna")));
        assert!(t.matches(&q("RICK never")));
        assert!(!t.matches(&q("rick down")));
    }
}
