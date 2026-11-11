//! Managed yt-dlp installation.
//!
//! "Update yt-dlp" downloads the latest commit of yt-dlp's master branch from
//! GitHub, plus the matching `yt-dlp-ejs` package (YouTube's JS challenge
//! solver scripts) from PyPI, and stores them in the app's data dir. yt-dlp is
//! then run from source with the system Python, so nothing has to be
//! compiled. If no managed copy exists, a `yt-dlp` found on PATH is used.

use std::io::{BufRead, BufReader, Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

use crate::model::{Paths, Track};
use crate::sources::http;

const REPO_API: &str = "https://api.github.com/repos/yt-dlp/yt-dlp/commits/master";
/// Formats rodio/symphonia can decode, best first. `ba` = best audio-only.
const FORMAT: &str = "ba[acodec^=mp4a]/ba[acodec=mp3]/ba[acodec=vorbis]/ba[acodec=flac]/ba/b";
pub const AUDIO_EXTS: &[&str] = &["m4a", "mp4", "mp3", "ogg", "oga", "flac", "wav", "aac", "webm", "opus"];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installed {
    pub commit: String,
    pub date: String,
    pub ejs_version: Option<String>,
}

impl Installed {
    pub fn short(&self) -> String {
        let date = self.date.split('T').next().unwrap_or(&self.date);
        format!("{} ({})", &self.commit[..self.commit.len().min(7)], date)
    }
}

#[derive(Clone, Debug)]
pub enum Backend {
    Managed(Installed),
    System(String),
    Missing,
}

#[derive(Clone)]
pub struct YtDlp {
    root: PathBuf,
    yt_cache: PathBuf,
}

impl YtDlp {
    pub fn new(paths: &Paths) -> Self {
        Self { root: paths.ytdlp_dir(), yt_cache: paths.cache.join("yt-dlp") }
    }

    fn current(&self) -> PathBuf {
        self.root.join("current")
    }

    pub fn installed(&self) -> Option<Installed> {
        let bytes = std::fs::read(self.current().join("version.json")).ok()?;
        let info: Installed = serde_json::from_slice(&bytes).ok()?;
        self.current().join("lib/yt_dlp/__main__.py").exists().then_some(info)
    }

    /// Blocking: may run a subprocess to query the system yt-dlp version.
    pub fn backend(&self) -> Backend {
        if let Some(i) = self.installed() {
            if find_python().is_some() {
                return Backend::Managed(i);
            }
        }
        match which("yt-dlp") {
            Some(path) => {
                let version = Command::new(&path)
                    .arg("--version")
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default();
                Backend::System(version)
            }
            None => Backend::Missing,
        }
    }

    fn command(&self) -> Result<Command> {
        let mut cmd = if self.installed().is_some() && let Some(py) = find_python() {
            let mut c = Command::new(py);
            c.arg("-m").arg("yt_dlp").env("PYTHONPATH", self.current().join("lib"));
            c
        } else if let Some(bin) = which("yt-dlp") {
            Command::new(bin)
        } else {
            bail!("yt-dlp is not installed yet — click \"Update yt-dlp\" first (requires Python 3.10+)");
        };
        cmd.arg("--ignore-config")
            .arg("--no-warnings")
            .arg("--cache-dir")
            .arg(&self.yt_cache);
        // yt-dlp needs a JavaScript runtime to solve YouTube's challenges.
        for rt in ["deno", "node", "bun", "qjs"] {
            if which(rt).is_some() {
                let name = if rt == "qjs" { "quickjs" } else { rt };
                cmd.arg("--js-runtimes").arg(name);
            }
        }
        cmd.stdin(Stdio::null());
        Ok(cmd)
    }

    /// `yt-dlp -J --flat-playlist URL` — metadata fallback.
    pub fn extract_info(&self, url: &str) -> Result<serde_json::Value> {
        let out = self
            .command()?
            .args(["-J", "--flat-playlist", "--", url])
            .output()
            .context("failed to run yt-dlp")?;
        if !out.status.success() {
            bail!("{}", last_error(&String::from_utf8_lossy(&out.stderr)));
        }
        Ok(serde_json::from_slice(&out.stdout)?)
    }

    /// Download the best decodable audio for `track` into `dir`, returning the
    /// final file path. `progress` receives 0.0..=1.0.
    pub fn download(&self, track: &Track, dir: &Path, progress: impl Fn(f32)) -> Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let template = dir.join(format!("{}.%(ext)s", track.cache_key()));
        let mut cmd = self.command()?;
        cmd.args(["--no-playlist", "--no-mtime", "--progress", "--newline"])
            .args(["-f", FORMAT])
            .args(["--progress-template", "download:LPPROGRESS %(progress._percent_str)s"])
            .args(["--print", "after_move:LPFILE %(filepath)s"])
            .arg("-o")
            .arg(&template);
        if which("ffmpeg").is_some() {
            // Make sure we end up with something we can decode (e.g. Opus -> AAC).
            // Already-AAC sources are only remuxed, not re-encoded.
            cmd.args(["-x", "--audio-format", "m4a"]);
        }
        cmd.arg("--").arg(&track.url).stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd.spawn().context("failed to start yt-dlp")?;
        let stderr = child.stderr.take().unwrap();
        let err_thread = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = BufReader::new(stderr).read_to_string(&mut s);
            s
        });
        let mut file = None;
        for line in BufReader::new(child.stdout.take().unwrap()).lines() {
            let line = line?;
            if let Some(p) = line.strip_prefix("LPPROGRESS") {
                if let Ok(pct) = p.trim().trim_end_matches('%').trim().parse::<f32>() {
                    progress((pct / 100.0).clamp(0.0, 1.0));
                }
            } else if let Some(f) = line.strip_prefix("LPFILE ") {
                file = Some(PathBuf::from(f.trim()));
            }
        }
        let status = child.wait()?;
        let stderr = err_thread.join().unwrap_or_default();
        if !status.success() {
            bail!("{}", last_error(&stderr));
        }
        match file {
            Some(f) if f.exists() => Ok(f),
            _ => find_cached(dir, &track.cache_key())
                .context("yt-dlp finished but the audio file was not found"),
        }
    }

    /// Fetch latest master + matching yt-dlp-ejs and install atomically.
    /// Returns the new version, or `None` when already up to date.
    pub fn update(&self, status: impl Fn(&str)) -> Result<Option<Installed>> {
        let python = find_python().context(
            "Python 3.10+ is required to run yt-dlp from source, but python3 was not found on PATH",
        )?;

        status("Checking latest yt-dlp commit…");
        let (commit, date) = latest_commit()?;
        if let Some(cur) = self.installed() {
            if cur.commit == commit {
                return Ok(None);
            }
        }

        status("Downloading yt-dlp source…");
        let zip_bytes = download_bytes(&format!("https://codeload.github.com/yt-dlp/yt-dlp/zip/{commit}"))?;

        std::fs::create_dir_all(&self.root)?;
        let staging = self.root.join("staging");
        let _ = std::fs::remove_dir_all(&staging);
        let lib = staging.join("lib");
        std::fs::create_dir_all(&lib)?;

        status("Unpacking yt-dlp…");
        let pyproject = extract_ytdlp(&zip_bytes, &lib)?;

        let ejs_version = pinned_ejs_version(&pyproject);
        if let Some(v) = &ejs_version {
            status("Downloading YouTube challenge solver (yt-dlp-ejs)…");
            let wheel = ejs_wheel_url(v)?;
            let bytes = download_bytes(&wheel)?;
            extract_wheel(&bytes, &lib)?;
        }

        status("Verifying…");
        let out = Command::new(&python)
            .args(["-m", "yt_dlp", "--version"])
            .env("PYTHONPATH", &lib)
            .stdin(Stdio::null())
            .output()
            .context("failed to run python")?;
        if !out.status.success() {
            bail!(
                "The downloaded yt-dlp failed to start: {}",
                last_error(&String::from_utf8_lossy(&out.stderr))
            );
        }

        let info = Installed { commit, date, ejs_version };
        std::fs::write(staging.join("version.json"), serde_json::to_vec_pretty(&info)?)?;

        let current = self.current();
        let old = self.root.join("old");
        let _ = std::fs::remove_dir_all(&old);
        if current.exists() {
            std::fs::rename(&current, &old)?;
        }
        std::fs::rename(&staging, &current)?;
        let _ = std::fs::remove_dir_all(&old);
        Ok(Some(info))
    }
}

/// Look for an already-downloaded audio file for `key` in `dir`.
pub fn find_cached(dir: &Path, key: &str) -> Option<PathBuf> {
    let prefix = format!("{key}.");
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        name.starts_with(&prefix)
            && p.extension().and_then(|e| e.to_str()).is_some_and(|e| AUDIO_EXTS.contains(&e))
    })
}

fn latest_commit() -> Result<(String, String)> {
    let mut resp = http()
        .get(REPO_API)
        .header("Accept", "application/vnd.github+json")
        .call()?;
    if !resp.status().is_success() {
        bail!("GitHub returned HTTP {} while checking for updates", resp.status());
    }
    let v: serde_json::Value = resp.body_mut().read_json()?;
    let sha = v["sha"].as_str().context("no commit sha from GitHub")?.to_string();
    if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("unexpected commit id from GitHub");
    }
    let date = v.pointer("/commit/committer/date").and_then(|d| d.as_str()).unwrap_or("").to_string();
    Ok((sha, date))
}

fn download_bytes(url: &str) -> Result<Vec<u8>> {
    let mut resp = http().get(url).call()?;
    if !resp.status().is_success() {
        bail!("HTTP {} downloading {url}", resp.status());
    }
    Ok(resp.body_mut().with_config().limit(200 * 1024 * 1024).read_to_vec()?)
}

/// Extract `<root>/yt_dlp/**` into `lib/yt_dlp`, returning pyproject.toml contents.
fn extract_ytdlp(zip_bytes: &[u8], lib: &Path) -> Result<String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))?;
    let mut pyproject = String::new();
    let mut count = 0;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let Some(path) = file.enclosed_name() else { continue };
        let mut comps = path.components();
        comps.next(); // top-level "yt-dlp-<sha>/"
        let rel = comps.as_path();
        if rel == Path::new("pyproject.toml") {
            file.read_to_string(&mut pyproject)?;
            continue;
        }
        if !rel.starts_with("yt_dlp") {
            continue;
        }
        let dest = lib.join(rel);
        if file.is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&dest)?;
            std::io::copy(&mut file, &mut out)?;
            count += 1;
        }
    }
    if count == 0 || !lib.join("yt_dlp/__main__.py").exists() {
        bail!("downloaded archive does not look like yt-dlp");
    }
    Ok(pyproject)
}

fn pinned_ejs_version(pyproject: &str) -> Option<String> {
    let pos = pyproject.find("yt-dlp-ejs==")?;
    let rest = &pyproject[pos + "yt-dlp-ejs==".len()..];
    let v: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '.').collect();
    (!v.is_empty()).then_some(v)
}

fn ejs_wheel_url(version: &str) -> Result<String> {
    let mut resp = http().get(format!("https://pypi.org/pypi/yt-dlp-ejs/{version}/json")).call()?;
    if !resp.status().is_success() {
        bail!("PyPI returned HTTP {} for yt-dlp-ejs {version}", resp.status());
    }
    let v: serde_json::Value = resp.body_mut().read_json()?;
    v["urls"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|u| u["packagetype"].as_str() == Some("bdist_wheel"))
        .and_then(|u| u["url"].as_str())
        .map(str::to_string)
        .context("no wheel published for yt-dlp-ejs")
}

fn extract_wheel(bytes: &[u8], lib: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let Some(rel) = file.enclosed_name() else { continue };
        let dest = lib.join(rel);
        if file.is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::io::copy(&mut file, &mut std::fs::File::create(&dest)?)?;
        }
    }
    Ok(())
}

fn find_python() -> Option<PathBuf> {
    ["python3", "python"].iter().find_map(|p| which(p))
}

pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| {
        p.is_file() && {
            use std::os::unix::fs::PermissionsExt;
            p.metadata().map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
        }
    })
}

/// Pick the most useful line out of yt-dlp's stderr.
fn last_error(stderr: &str) -> String {
    stderr
        .lines()
        .rev()
        .find(|l| l.starts_with("ERROR"))
        .or_else(|| stderr.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("yt-dlp failed")
        .trim_start_matches("ERROR: ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ejs_pin() {
        let toml = "dependencies = [\n  \"yt-dlp-ejs==0.8.0\",\n]";
        assert_eq!(pinned_ejs_version(toml).as_deref(), Some("0.8.0"));
        assert_eq!(pinned_ejs_version("nothing"), None);
    }
}
