//! Where audio files live.
//!
//! * Playlist folders: `<music root>/<Playlist name>/<Platform>-<id>.<ext>`.
//!   Files there are ordinary audio files that any player can open.
//! * Removed cache: when a track leaves a playlist its file moves to
//!   `<cache>/removed/`. Re-adding it restores the file instead of downloading
//!   it again. The cache is size-limited and drops the oldest files first.
//! * Temp downloads go to `<playlist folder>/.downloading/<key>/` (same
//!   filesystem, so finishing a download is just a rename).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context as _, Result};

use crate::model::{Library, Paths, Platform, Playlist, Settings, Track};
use crate::ytdlp::AUDIO_EXTS;

pub const DOWNLOADING_DIR: &str = ".downloading";

#[cfg(target_family = "wasm")]
pub fn default_music_root() -> PathBuf {
    PathBuf::from("/music")
}

/// Default music root: `<XDG music dir or ~/Music>/local-playlist`.
#[cfg(not(target_family = "wasm"))]
pub fn default_music_root() -> PathBuf {
    let dirs = directories::UserDirs::new();
    let base = dirs
        .as_ref()
        .and_then(|d| d.audio_dir().map(Path::to_path_buf))
        .or_else(|| dirs.as_ref().map(|d| d.home_dir().join("Music")))
        .unwrap_or_else(|| PathBuf::from("Music"));
    base.join("local-playlist")
}

pub fn music_root(settings: &Settings) -> PathBuf {
    settings.music_dir.clone().unwrap_or_else(default_music_root)
}

/// File-system safe folder name for a playlist.
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    let mut out: String = trimmed.chars().take(100).collect();
    if out.is_empty() {
        out = "Playlist".into();
    }
    // Reserved names on Windows.
    let upper = out.to_ascii_uppercase();
    if ["CON", "PRN", "AUX", "NUL"].contains(&upper.as_str())
        || ((upper.starts_with("COM") || upper.starts_with("LPT")) && upper.len() == 4)
    {
        out.push('_');
    }
    out
}

/// Pick a folder name for `playlist` that no other playlist uses.
pub fn unique_folder(library: &Library, playlist_id: &str, name: &str) -> String {
    let base = sanitize(name);
    let taken = |f: &str| {
        library
            .playlists
            .iter()
            .any(|p| p.id != playlist_id && p.folder.as_deref().is_some_and(|o| o.eq_ignore_ascii_case(f)))
    };
    if !taken(&base) {
        return base;
    }
    (2..).map(|n| format!("{base} ({n})")).find(|f| !taken(f)).unwrap()
}

pub fn playlist_dir(root: &Path, playlist: &Playlist) -> PathBuf {
    root.join(playlist.folder.clone().unwrap_or_else(|| sanitize(&playlist.name)))
}

pub fn removed_dir(paths: &Paths) -> PathBuf {
    paths.cache.join("removed")
}

/// Look for `<key>.<audio ext>` in `dir`.
pub fn find_file(dir: &Path, key: &str) -> Option<PathBuf> {
    let prefix = format!("{key}.");
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        name.starts_with(&prefix)
            && p.is_file()
            && p.extension().and_then(|e| e.to_str()).is_some_and(|e| AUDIO_EXTS.contains(&e))
    })
}

/// How [`materialize_with`] finds files: directly ([`find_file`], for one
/// track), or from each folder's listing read once (for many tracks in a row,
/// e.g. "Download all", which would otherwise list every folder per track).
pub struct FileIndex {
    /// Folder → audio files by key; `None` = look up directly.
    dirs: Option<HashMap<PathBuf, HashMap<String, PathBuf>>>,
}

impl FileIndex {
    pub fn direct() -> Self {
        Self { dirs: None }
    }

    pub fn cached() -> Self {
        Self { dirs: Some(HashMap::new()) }
    }

    /// Like [`find_file`].
    pub fn find(&mut self, dir: &Path, key: &str) -> Option<PathBuf> {
        let Some(dirs) = &mut self.dirs else { return find_file(dir, key) };
        if !dirs.contains_key(dir) {
            let mut files = HashMap::new();
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = entry.path();
                let Some((key, _)) = p.file_name().and_then(|n| n.to_str()).and_then(|n| n.split_once('.')) else {
                    continue;
                };
                if p.extension().and_then(|e| e.to_str()).is_some_and(|e| AUDIO_EXTS.contains(&e)) && p.is_file() {
                    files.entry(key.to_string()).or_insert(p);
                }
            }
            dirs.insert(dir.to_path_buf(), files);
        }
        dirs.get(dir)?.get(key).cloned()
    }

    /// Record that `path` (`<dir>/<key>.<ext>`) now exists, or no longer does.
    fn update(&mut self, dir: &Path, key: &str, path: Option<PathBuf>) {
        if let Some(files) = self.dirs.as_mut().and_then(|d| d.get_mut(dir)) {
            match path {
                Some(p) => {
                    files.insert(key.to_string(), p);
                }
                None => {
                    files.remove(key);
                }
            }
        }
    }
}

/// Key used by the old flat cache (`yt-<id>` / `sc-<id>`).
fn legacy_key(track: &Track) -> String {
    let short = match track.platform {
        Platform::YouTube => "yt",
        Platform::SoundCloud => "sc",
    };
    format!("{short}-{}", track.file_key().split_once('-').map(|x| x.1).unwrap_or_default())
}

/// Make sure the playlist folder holds the track's audio, reusing a copy from
/// the removed cache, another playlist folder or the legacy cache. Returns the
/// path, or `None` if it has to be downloaded.
pub fn materialize(paths: &Paths, library: &Library, playlist: &Playlist, track: &Track) -> Option<PathBuf> {
    materialize_with(&mut FileIndex::direct(), paths, library, playlist, track)
}

/// [`materialize`], finding files through `index`.
pub fn materialize_with(
    index: &mut FileIndex,
    paths: &Paths,
    library: &Library,
    playlist: &Playlist,
    track: &Track,
) -> Option<PathBuf> {
    let root = music_root(&library.settings);
    let dir = playlist_dir(&root, playlist);
    let key = track.file_key();
    if let Some(p) = index.find(&dir, &key) {
        return Some(p);
    }
    let place = |index: &mut FileIndex, src: &Path, keep_source: bool| -> Option<PathBuf> {
        std::fs::create_dir_all(&dir).ok()?;
        let dest = dir.join(src.file_name()?);
        let ok = if keep_source {
            std::fs::hard_link(src, &dest).is_ok() || std::fs::copy(src, &dest).is_ok()
        } else {
            move_file(src, &dest).is_ok()
        };
        index.update(&dir, &key, ok.then(|| dest.clone()));
        ok.then_some(dest)
    };
    // Previously removed from some playlist.
    let removed = removed_dir(paths);
    if let Some(p) = index.find(&removed, &key) {
        index.update(&removed, &key, None);
        return place(index, &p, false);
    }
    // Present in another playlist's folder.
    for other in library.playlists.iter().filter(|p| p.id != playlist.id) {
        if let Some(p) = index.find(&playlist_dir(&root, other), &key) {
            return place(index, &p, true);
        }
    }
    // Old flat cache from earlier versions.
    let legacy = paths.cache.join("audio");
    if let Some(p) = index.find(&legacy, &legacy_key(track)) {
        index.update(&legacy, &legacy_key(track), None);
        let dest_name = format!("{key}.{}", p.extension()?.to_str()?);
        std::fs::create_dir_all(&dir).ok()?;
        let dest = dir.join(dest_name);
        let moved = move_file(&p, &dest).is_ok();
        index.update(&dir, &key, moved.then(|| dest.clone()));
        return moved.then_some(dest);
    }
    None
}

/// Move a track's file out of a playlist folder into the removed cache.
pub fn retire(paths: &Paths, root: &Path, playlist: &Playlist, track: &Track, limit_bytes: u64) {
    let Some(file) = find_file(&playlist_dir(root, playlist), &track.file_key()) else { return };
    let removed = removed_dir(paths);
    if std::fs::create_dir_all(&removed).is_err() {
        return;
    }
    let dest = removed.join(file.file_name().unwrap());
    if move_file(&file, &dest).is_ok() {
        // Mark as most recently used for the size limit.
        if let Ok(f) = std::fs::File::options().write(true).open(&dest) {
            let _ = f.set_modified(SystemTime::now());
        }
    }
    enforce_limit(&removed, limit_bytes);
}

/// Delete the oldest files in `dir` until its total size is at most `limit`.
pub fn enforce_limit(dir: &Path, limit: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort_by_key(|f| f.0);
    for (_, len, path) in files {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

pub fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|it| it.flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum())
        .unwrap_or(0)
}

/// Rename (or copy + delete across filesystems).
pub fn move_file(src: &Path, dest: &Path) -> Result<()> {
    if std::fs::rename(src, dest).is_ok() {
        return Ok(());
    }
    std::fs::copy(src, dest).with_context(|| format!("copy {} -> {}", src.display(), dest.display()))?;
    std::fs::remove_file(src)?;
    Ok(())
}

/// Move a whole directory tree (rename, or copy + delete across filesystems).
pub fn move_dir(src: &Path, dest: &Path) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !dest.exists() && std::fs::rename(src, dest).is_ok() {
        return Ok(());
    }
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let target = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            move_dir(&entry.path(), &target)?;
        } else if !target.exists() {
            move_file(&entry.path(), &target)?;
        }
    }
    let _ = std::fs::remove_dir_all(src);
    Ok(())
}

/// Retire all of a playlist's files and remove its folder if nothing else is left.
pub fn retire_playlist(paths: &Paths, root: &Path, playlist: &Playlist, limit_bytes: u64) {
    for t in &playlist.tracks {
        retire(paths, root, playlist, t, limit_bytes);
    }
    let dir = playlist_dir(root, playlist);
    let _ = std::fs::remove_dir_all(dir.join(DOWNLOADING_DIR));
    // Only removes the folder when empty, so unrelated user files are never deleted.
    let _ = std::fs::remove_dir(&dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes() {
        assert_eq!(sanitize("AC/DC: Best?"), "AC_DC_ Best_");
        assert_eq!(sanitize("  ..  "), "Playlist");
        assert_eq!(sanitize("con"), "con_");
        assert_eq!(sanitize("Chill 🎧"), "Chill 🎧");
    }

    #[test]
    fn limit_drops_oldest() {
        let dir = std::env::temp_dir().join(format!("lp-limit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (i, name) in ["a", "b", "c"].iter().enumerate() {
            let p = dir.join(name);
            std::fs::write(&p, vec![0u8; 100]).unwrap();
            let f = std::fs::File::options().write(true).open(&p).unwrap();
            f.set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000 + i as u64)).unwrap();
        }
        enforce_limit(&dir, 150);
        assert!(!dir.join("a").exists() && !dir.join("b").exists() && dir.join("c").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
