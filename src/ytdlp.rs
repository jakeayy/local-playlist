//! External components: yt-dlp and a JavaScript runtime.
//!
//! By default the app manages its own copies so nothing needs to be installed:
//!
//! * **yt-dlp**: the official build of the latest master commit from
//!   `yt-dlp/yt-dlp-master-builds` (self-contained: Python and the `yt-dlp-ejs`
//!   YouTube challenge solver are bundled), verified against the published
//!   SHA-256 sums. On platforms without a build, the master source plus the
//!   pinned `yt-dlp-ejs` wheel is installed and run with the system Python.
//! * **JavaScript runtime**: QuickJS-ng (~2.5 MB), which yt-dlp uses to solve
//!   YouTube's challenges.
//!
//! Either component can instead come from the system PATH.

use std::io::{BufRead, BufReader, Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::model::{Paths, Track};
use crate::secrets::{StoredCookie, write_private};
use crate::sources::http;

/// Formats rodio/symphonia can decode, best first. `ba` = best audio-only.
const FORMAT: &str = "ba[acodec^=mp4a]/ba[acodec=mp3]/ba[acodec=vorbis]/ba[acodec=flac]/ba/b";
pub const AUDIO_EXTS: &[&str] = &["m4a", "mp4", "mp3", "ogg", "oga", "flac", "wav", "aac", "webm", "opus"];
const YTDLP_RELEASES: &str = "https://api.github.com/repos/yt-dlp/yt-dlp-master-builds/releases/latest";
const QJS_RELEASES: &str = "https://api.github.com/repos/quickjs-ng/quickjs/releases/latest";

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

    /// Approximate download size, for the setup prompt.
    pub fn approx_mb(self) -> u32 {
        match self {
            Component::YtDlp => {
                if ytdlp_asset().is_some() {
                    41
                } else {
                    4
                }
            }
            Component::JsRuntime => 3,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installed {
    /// Release tag (yt-dlp master builds are tagged with the build date).
    pub tag: String,
    #[serde(default)]
    pub commit: Option<String>,
    /// "binary" or "source".
    #[serde(default)]
    pub kind: String,
    /// Relative path of the executable inside the install dir.
    #[serde(default)]
    pub exe: String,
}

impl Installed {
    pub fn short(&self) -> String {
        match &self.commit {
            Some(c) => format!("{} ({})", &c[..c.len().min(7)], self.tag),
            None => self.tag.clone(),
        }
    }
}

/// What a component resolves to right now.
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
    root: PathBuf,
    yt_cache: PathBuf,
    tmp: PathBuf,
    pub ytdlp_source: Source,
    pub js_source: Source,
}

impl YtDlp {
    pub fn new(paths: &Paths, ytdlp_source: Source, js_source: Source) -> Self {
        Self {
            root: paths.data.join("components"),
            yt_cache: paths.cache.join("yt-dlp"),
            tmp: paths.cache.join("tmp"),
            ytdlp_source,
            js_source,
        }
    }

    fn dir(&self, c: Component) -> PathBuf {
        self.root.join(match c {
            Component::YtDlp => "yt-dlp",
            Component::JsRuntime => "quickjs",
        })
    }

    pub fn installed(&self, c: Component) -> Option<Installed> {
        let dir = self.dir(c).join("current");
        let info: Installed = serde_json::from_slice(&std::fs::read(dir.join("version.json")).ok()?).ok()?;
        dir.join(&info.exe).exists().then_some(info)
    }

    fn managed_exe(&self, c: Component) -> Option<PathBuf> {
        let info = self.installed(c)?;
        Some(self.dir(c).join("current").join(info.exe))
    }

    /// Blocking: may run subprocesses to query system versions.
    pub fn status(&self) -> Status {
        let ytdlp = match self.ytdlp_source {
            Source::Managed => match self.installed(Component::YtDlp) {
                Some(i) if i.kind != "source" || find_python().is_some() => State::Managed(i),
                _ => State::Missing,
            },
            Source::System => match which("yt-dlp") {
                Some(p) => State::System(format!("yt-dlp {}", version_of(&p, "--version"))),
                None => State::Missing,
            },
        };
        let js = match self.js_source {
            Source::Managed => match self.installed(Component::JsRuntime) {
                Some(i) => State::Managed(i),
                None => State::Missing,
            },
            Source::System => match system_js_runtimes().first() {
                Some((name, _)) => State::System(name.to_string()),
                None => State::Missing,
            },
        };
        Status { ytdlp, js }
    }

    /// Managed components that still need downloading.
    pub fn missing_managed(&self) -> Vec<Component> {
        let mut out = Vec::new();
        if self.ytdlp_source == Source::Managed && self.installed(Component::YtDlp).is_none() {
            out.push(Component::YtDlp);
        }
        if self.js_source == Source::Managed && self.installed(Component::JsRuntime).is_none() {
            out.push(Component::JsRuntime);
        }
        out
    }

    fn command(&self) -> Result<Command> {
        let mut cmd = match self.ytdlp_source {
            Source::Managed => {
                let info = self.installed(Component::YtDlp).context(
                    "yt-dlp hasn't been downloaded yet. Use \"Download components\" or switch to the system yt-dlp.",
                )?;
                let exe = self.dir(Component::YtDlp).join("current").join(&info.exe);
                if info.kind == "source" {
                    let py = find_python().context("Python 3.10+ is required for the source build of yt-dlp")?;
                    let mut c = Command::new(py);
                    c.arg("-m").arg("yt_dlp").env("PYTHONPATH", self.dir(Component::YtDlp).join("current/lib"));
                    c
                } else {
                    Command::new(exe)
                }
            }
            Source::System => Command::new(which("yt-dlp").context("yt-dlp was not found on PATH")?),
        };
        cmd.arg("--ignore-config").arg("--no-warnings").arg("--cache-dir").arg(&self.yt_cache);
        // yt-dlp needs a JavaScript runtime to solve YouTube's challenges.
        cmd.arg("--no-js-runtimes");
        match self.js_source {
            Source::Managed => {
                if let Some(qjs) = self.managed_exe(Component::JsRuntime) {
                    cmd.arg("--js-runtimes").arg(format!("quickjs:{}", qjs.display()));
                }
            }
            Source::System => {
                for (name, path) in system_js_runtimes() {
                    cmd.arg("--js-runtimes").arg(format!("{name}:{}", path.display()));
                }
            }
        }
        cmd.stdin(Stdio::null());
        hide_console(&mut cmd);
        Ok(cmd)
    }

    /// `yt-dlp -J --flat-playlist URL` — metadata fallback.
    pub fn extract_info(&self, url: &str, cookies: Option<&[StoredCookie]>) -> Result<serde_json::Value> {
        let mut cmd = self.command()?;
        let cookie_file = cookies.map(|c| self.cookie_file(c)).transpose()?;
        if let Some(f) = &cookie_file {
            cmd.arg("--cookies").arg(f.path());
        }
        let out = cmd.args(["-J", "--flat-playlist", "--", url]).output().context("failed to run yt-dlp")?;
        if !out.status.success() {
            bail!("{}", last_error(&String::from_utf8_lossy(&out.stderr)));
        }
        Ok(serde_json::from_slice(&out.stdout)?)
    }

    fn cookie_file(&self, cookies: &[StoredCookie]) -> Result<TempFile> {
        let mut name = [0u8; 8];
        getrandom::fill(&mut name).map_err(|e| anyhow::anyhow!("rng: {e}"))?;
        let path = self.tmp.join(format!("cookies-{}.txt", hex(&name)));
        write_private(&path, crate::secrets::to_netscape(cookies).as_bytes())?;
        Ok(TempFile(path))
    }

    /// Download the best decodable audio for `track` into `dir`, returning the
    /// final file path. `progress` receives 0.0..=1.0.
    pub fn download(
        &self,
        track: &Track,
        dir: &Path,
        cookies: Option<&[StoredCookie]>,
        progress: impl Fn(f32),
    ) -> Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let template = dir.join(format!("{}.%(ext)s", track.file_key()));
        let mut cmd = self.command()?;
        let cookie_file = cookies.map(|c| self.cookie_file(c)).transpose()?;
        if let Some(f) = &cookie_file {
            cmd.arg("--cookies").arg(f.path());
        }
        // --no-part: write straight to the final name so the player can read
        // the file while it grows (the download happens in a private temp dir).
        cmd.args(["--no-playlist", "--no-mtime", "--no-part", "--progress", "--newline", "-N", "4"])
            .args(["-f", FORMAT])
            .args(["--progress-template", "download:LPPROGRESS %(progress._percent_str)s"])
            .args(["--print", "after_move:LPFILE %(filepath)s"])
            .arg("-o")
            .arg(&template);
        if which("ffmpeg").is_some() {
            // Make sure we end up with something we can decode (e.g. Opus -> AAC).
            // Already-AAC sources are only remuxed, not re-encoded. Without ffmpeg
            // yt-dlp still produces (fragmented) m4a, which we can play too.
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
        drop(cookie_file);
        if !status.success() {
            bail!("{}", last_error(&stderr));
        }
        match file {
            Some(f) if f.exists() => Ok(f),
            _ => crate::storage::find_file(dir, &track.file_key())
                .context("yt-dlp finished but the audio file was not found"),
        }
    }

    /// Download/refresh a managed component. Returns `None` if already current.
    pub fn install(&self, c: Component, status: &dyn Fn(String, Option<f32>)) -> Result<Option<Installed>> {
        match c {
            Component::YtDlp => match ytdlp_asset() {
                Some(asset) => self.install_ytdlp_binary(asset, status),
                None => self.install_ytdlp_source(status),
            },
            Component::JsRuntime => self.install_quickjs(status),
        }
    }

    fn install_ytdlp_binary(&self, asset: &str, status: &dyn Fn(String, Option<f32>)) -> Result<Option<Installed>> {
        status("Checking latest yt-dlp build…".into(), None);
        let release = github_json(YTDLP_RELEASES)?;
        let tag = release["tag_name"].as_str().context("release has no tag")?.to_string();
        let commit = release["body"].as_str().and_then(commit_from_body);
        if let Some(cur) = self.installed(Component::YtDlp)
            && cur.tag == tag
            && cur.kind == "binary"
        {
            return Ok(None);
        }
        let url = asset_url(&release, asset)?;
        let bytes =
            download_with_progress(&url, &|f| status(format!("Downloading yt-dlp… {:.0}%", f * 100.), Some(f)))?;

        status("Verifying checksum…".into(), None);
        let sums = String::from_utf8(download_with_progress(&asset_url(&release, "SHA2-256SUMS")?, &|_| {})?)?;
        let expected = sums
            .lines()
            .find_map(|l| {
                let (hash, name) = l.split_once(char::is_whitespace)?;
                (name.trim() == asset).then(|| hash.to_ascii_lowercase())
            })
            .context("checksum for yt-dlp build not published")?;
        let actual = hex(&sha2::Sha256::digest(&bytes));
        if actual != expected {
            bail!("yt-dlp download is corrupt (checksum mismatch)");
        }

        status("Unpacking yt-dlp…".into(), None);
        let staging = self.staging(Component::YtDlp)?;
        extract_zip(&bytes, &staging)?;
        // The launcher is the single file at the archive root.
        let exe = std::fs::read_dir(&staging)?
            .flatten()
            .find(|e| e.path().is_file())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .context("no executable in yt-dlp archive")?;
        make_executable(&staging.join(&exe))?;

        status("Testing yt-dlp…".into(), None);
        if version_of(&staging.join(&exe), "--version").is_empty() {
            bail!("the downloaded yt-dlp failed to start");
        }
        let info = Installed { tag, commit, kind: "binary".into(), exe };
        self.commit_install(Component::YtDlp, staging, &info)?;
        Ok(Some(info))
    }

    /// Fallback for platforms without an official build: master source + ejs wheel.
    fn install_ytdlp_source(&self, status: &dyn Fn(String, Option<f32>)) -> Result<Option<Installed>> {
        let python = find_python().context(
            "No prebuilt yt-dlp exists for this platform, and Python 3.10+ (needed for the source version) wasn't found",
        )?;
        status("Checking latest yt-dlp commit…".into(), None);
        let commit_info = github_json("https://api.github.com/repos/yt-dlp/yt-dlp/commits/master")?;
        let sha = commit_info["sha"].as_str().context("no commit sha")?.to_string();
        if self.installed(Component::YtDlp).is_some_and(|i| i.commit.as_deref() == Some(sha.as_str())) {
            return Ok(None);
        }
        let zip = download_with_progress(&format!("https://codeload.github.com/yt-dlp/yt-dlp/zip/{sha}"), &|f| {
            status(format!("Downloading yt-dlp source… {:.0}%", f * 100.), Some(f))
        })?;
        let staging = self.staging(Component::YtDlp)?;
        let lib = staging.join("lib");
        let pyproject = extract_ytdlp_source(&zip, &lib)?;
        if let Some(v) = pinned_ejs_version(&pyproject) {
            status("Downloading YouTube challenge solver…".into(), None);
            let meta = github_json(&format!("https://pypi.org/pypi/yt-dlp-ejs/{v}/json"))?;
            let wheel = meta["urls"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|u| u["packagetype"].as_str() == Some("bdist_wheel"))
                .and_then(|u| u["url"].as_str())
                .context("no wheel for yt-dlp-ejs")?;
            extract_zip(&download_with_progress(wheel, &|_| {})?, &lib)?;
        }
        let mut cmd = Command::new(python);
        cmd.args(["-m", "yt_dlp", "--version"]).env("PYTHONPATH", &lib).stdin(Stdio::null());
        hide_console(&mut cmd);
        if !cmd.output().is_ok_and(|o| o.status.success()) {
            bail!("the downloaded yt-dlp failed to start");
        }
        let date = commit_info.pointer("/commit/committer/date").and_then(|d| d.as_str()).unwrap_or("");
        let info = Installed {
            tag: date.split('T').next().unwrap_or("").to_string(),
            commit: Some(sha),
            kind: "source".into(),
            exe: "lib/yt_dlp/__main__.py".into(),
        };
        self.commit_install(Component::YtDlp, staging, &info)?;
        Ok(Some(info))
    }

    fn install_quickjs(&self, status: &dyn Fn(String, Option<f32>)) -> Result<Option<Installed>> {
        let asset = qjs_asset().context("No QuickJS build for this platform; use a system JavaScript runtime")?;
        status("Checking latest QuickJS…".into(), None);
        let release = github_json(QJS_RELEASES)?;
        let tag = release["tag_name"].as_str().context("release has no tag")?.to_string();
        if self.installed(Component::JsRuntime).is_some_and(|i| i.tag == tag) {
            return Ok(None);
        }
        let bytes = download_with_progress(&asset_url(&release, asset)?, &|f| {
            status(format!("Downloading QuickJS… {:.0}%", f * 100.), Some(f))
        })?;
        let staging = self.staging(Component::JsRuntime)?;
        let exe = if cfg!(windows) { "qjs.exe" } else { "qjs" };
        std::fs::write(staging.join(exe), &bytes)?;
        make_executable(&staging.join(exe))?;
        let mut cmd = Command::new(staging.join(exe));
        cmd.args(["-e", "0"]).stdin(Stdio::null());
        hide_console(&mut cmd);
        if !cmd.output().is_ok_and(|o| o.status.success()) {
            bail!("the downloaded QuickJS failed to start");
        }
        let info = Installed { tag, commit: None, kind: "binary".into(), exe: exe.into() };
        self.commit_install(Component::JsRuntime, staging, &info)?;
        Ok(Some(info))
    }

    fn staging(&self, c: Component) -> Result<PathBuf> {
        let staging = self.dir(c).join("staging");
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        Ok(staging)
    }

    fn commit_install(&self, c: Component, staging: PathBuf, info: &Installed) -> Result<()> {
        std::fs::write(staging.join("version.json"), serde_json::to_vec_pretty(info)?)?;
        let dir = self.dir(c);
        let (current, old) = (dir.join("current"), dir.join("old"));
        let _ = std::fs::remove_dir_all(&old);
        if current.exists() {
            std::fs::rename(&current, &old)?;
        }
        std::fs::rename(&staging, &current)?;
        let _ = std::fs::remove_dir_all(&old);
        Ok(())
    }
}

/// Deleted on drop.
struct TempFile(PathBuf);

impl TempFile {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Errors that a logged-in session might fix.
pub fn needs_login(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    ["sign in", "login", "log in", "private", "members", "confirm your age", "cookies", "registered users"]
        .iter()
        .any(|k| e.contains(k))
}

fn ytdlp_asset() -> Option<&'static str> {
    let musl = cfg!(target_env = "musl");
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") if musl => "yt-dlp_musllinux.zip",
        ("linux", "x86_64") => "yt-dlp_linux.zip",
        ("linux", "aarch64") if musl => "yt-dlp_musllinux_aarch64.zip",
        ("linux", "aarch64") => "yt-dlp_linux_aarch64.zip",
        ("windows", "x86_64") => "yt-dlp_win.zip",
        ("windows", "aarch64") => "yt-dlp_win_arm64.zip",
        ("windows", "x86") => "yt-dlp_win_x86.zip",
        ("macos", _) => "yt-dlp_macos.zip",
        _ => return None,
    })
}

fn qjs_asset() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "qjs-linux-x86_64",
        ("linux", "aarch64") => "qjs-linux-aarch64",
        ("linux", "x86") => "qjs-linux-x86",
        ("linux", "riscv64") => "qjs-linux-riscv64",
        ("windows", "x86_64") => "qjs-windows-x86_64.exe",
        ("windows", "x86") => "qjs-windows-x86.exe",
        ("macos", "aarch64") => "qjs-darwin-arm64",
        ("macos", "x86_64") => "qjs-darwin-x86_64",
        _ => return None,
    })
}

fn github_json(url: &str) -> Result<serde_json::Value> {
    let mut resp = http().get(url).header("Accept", "application/vnd.github+json").call()?;
    if !resp.status().is_success() {
        bail!("HTTP {} from {}", resp.status(), url.split('/').nth(2).unwrap_or(url));
    }
    Ok(resp.body_mut().read_json()?)
}

fn asset_url(release: &serde_json::Value, name: &str) -> Result<String> {
    release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"].as_str() == Some(name))
        .and_then(|a| a["browser_download_url"].as_str())
        .map(str::to_string)
        .with_context(|| format!("release has no {name}"))
}

fn commit_from_body(body: &str) -> Option<String> {
    let pos = body.find("/commit/")?;
    let sha: String = body[pos + 8..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    (sha.len() == 40).then_some(sha)
}

fn download_with_progress(url: &str, progress: &dyn Fn(f32)) -> Result<Vec<u8>> {
    let mut resp = http().get(url).config().timeout_global(None).build().call()?;
    if !resp.status().is_success() {
        bail!("HTTP {} downloading {url}", resp.status());
    }
    let total: Option<u64> =
        resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
    let mut reader = resp.body_mut().with_config().limit(500 * 1024 * 1024).reader();
    let mut out = Vec::with_capacity(total.unwrap_or(0) as usize);
    let mut buf = vec![0u8; 256 * 1024];
    let mut last = 0.0f32;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        if let Some(t) = total.filter(|t| *t > 0) {
            let f = out.len() as f32 / t as f32;
            if f - last >= 0.01 {
                last = f;
                progress(f.min(1.0));
            }
        }
    }
    Ok(out)
}

/// Extract a zip into `dest`, preserving unix permissions.
fn extract_zip(bytes: &[u8], dest: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let Some(rel) = file.enclosed_name() else { continue };
        let out = dest.join(&rel);
        if file.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::io::copy(&mut file, &mut std::fs::File::create(&out)?)?;
        #[cfg(unix)]
        if let Some(mode) = file.unix_mode() {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode & 0o777))?;
        }
    }
    Ok(())
}

/// Extract `<root>/yt_dlp/**` into `lib/yt_dlp`, returning pyproject.toml contents.
fn extract_ytdlp_source(zip_bytes: &[u8], lib: &Path) -> Result<String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))?;
    let mut pyproject = String::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let Some(path) = file.enclosed_name() else { continue };
        let mut comps = path.components();
        comps.next(); // top-level "yt-dlp-<sha>/"
        let rel = comps.as_path();
        if rel == Path::new("pyproject.toml") {
            file.read_to_string(&mut pyproject)?;
        } else if rel.starts_with("yt_dlp") && !file.is_dir() {
            let dest = lib.join(rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::io::copy(&mut file, &mut std::fs::File::create(&dest)?)?;
        }
    }
    if !lib.join("yt_dlp/__main__.py").exists() {
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

fn make_executable(_path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(_path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn hide_console(_cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        _cmd.creation_flags(CREATE_NO_WINDOW);
    }
}

fn version_of(exe: &Path, flag: &str) -> String {
    let mut cmd = Command::new(exe);
    cmd.arg(flag).stdin(Stdio::null());
    hide_console(&mut cmd);
    cmd.output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string())
        .unwrap_or_default()
}

/// JS runtimes on PATH in yt-dlp's priority order, as (yt-dlp name, path).
fn system_js_runtimes() -> Vec<(&'static str, PathBuf)> {
    [("deno", "deno"), ("node", "node"), ("quickjs", "qjs"), ("bun", "bun")]
        .into_iter()
        .filter_map(|(name, bin)| which(bin).map(|p| (name, p)))
        .collect()
}

fn find_python() -> Option<PathBuf> {
    ["python3", "python"].iter().find_map(|p| which(p))
}

pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{name}.exe"), format!("{name}.cmd"), name.to_string()]
    } else {
        vec![name.to_string()]
    };
    std::env::split_paths(&path).flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        p.is_file() && p.metadata().is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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

    #[test]
    fn commit_body() {
        let b = "Generated from: https://github.com/yt-dlp/yt-dlp/commit/412e7f0a4b0588fe884a02b344ac66a06538475b\n";
        assert_eq!(commit_from_body(b).as_deref(), Some("412e7f0a4b0588fe884a02b344ac66a06538475b"));
    }

    #[test]
    #[ignore = "network; downloads yt-dlp, QuickJS and audio"]
    fn live_install_download_decode() {
        use crate::model::Platform;
        let dir = std::env::temp_dir().join("local-playlist-components-test");
        let paths = Paths { data: dir.join("data"), cache: dir.join("cache") };
        let y = YtDlp::new(&paths, Source::Managed, Source::Managed);
        let log = |s: String, _: Option<f32>| {
            if !s.contains('%') {
                println!("{s}")
            }
        };
        for c in [Component::YtDlp, Component::JsRuntime] {
            println!("{c:?} -> {:?}", y.install(c, &log).unwrap());
        }
        println!("{:?}", y.status());
        assert!(y.missing_managed().is_empty());
        for (platform, id, url) in [
            (Platform::YouTube, "jNQXAC9IVRw", "https://www.youtube.com/watch?v=jNQXAC9IVRw"),
            (Platform::SoundCloud, "293", "https://soundcloud.com/forss/flickermood"),
        ] {
            let t = Track::new(platform, id, url, "t", "a", None);
            let out = dir.join("audio");
            if let Some(f) = crate::storage::find_file(&out, &t.file_key()) {
                let _ = std::fs::remove_file(f);
            }
            let start = std::time::Instant::now();
            let f = y.download(&t, &out, None, |_| {}).unwrap();
            println!("{} in {:?}", f.display(), start.elapsed());
            use rodio::Source as _;
            let d = rodio::Decoder::try_from(std::fs::File::open(&f).unwrap()).unwrap();
            println!("duration {:?}", d.total_duration());
        }
    }
}
