//! `local-playlist --server`: serves the web UI and a small API that does what
//! a browser can't (platform metadata requests and yt-dlp).
//!
//! Privacy and safety:
//! * The server never sees platform logins/cookies; it only handles public content.
//! * Everything personal (library, settings) stays in the browser.
//! * Access requires the server password; the API accepts only YouTube and
//!   SoundCloud links (no arbitrary URLs reach yt-dlp).
//! * Request URLs aren't logged; audio is cached briefly and then deleted.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result, bail};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use crate::download::{DlState, Download};
use crate::model::{Paths, Platform, Track};
use crate::secrets::Credentials;
use crate::sources::{self, Link};
use crate::storage::{self, find_file};
use crate::ytdlp::{Component, Source, YtDlp};

const INDEX_HTML: &str = include_str!("../web/index.html");
const BOOT_JS: &str = include_str!("../web/boot.js");
#[cfg(web_ui)]
const APP_JS: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/web/dist/local_playlist.js.gz"));
#[cfg(web_ui)]
const APP_WASM: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/web/dist/local_playlist_bg.wasm.gz"));

// Icons used by the web UI (web/icons/*.svg, embedded by build.rs).
include!(concat!(env!("OUT_DIR"), "/icons.rs"));

const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; connect-src 'self'; \
    media-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; base-uri 'none'; \
    frame-ancestors 'none'; form-action 'none'";

struct Options {
    listen: String,
    data_dir: Option<PathBuf>,
    password: Option<String>,
    system_tools: bool,
    cache_minutes: u64,
}

struct State {
    password: String,
    tokens: Mutex<HashSet<String>>,
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    ytdlp: YtDlp,
    paths: Paths,
    audio_dir: PathBuf,
    downloads: Mutex<HashMap<String, Arc<Download>>>,
}

fn usage() -> &'static str {
    "Usage: local-playlist --server [--listen ADDR] [--data-dir DIR] [--password-file FILE] [--system-tools] [--cache-minutes N]

  --listen ADDR         Address to listen on (default 0.0.0.0:8080)
  --data-dir DIR        Where components and the audio cache live
                        (default: $LOCAL_PLAYLIST_DATA or the user data dir)
  --password-file FILE  Read the access password from FILE
                        (or set $LOCAL_PLAYLIST_PASSWORD; otherwise you are prompted)
  --system-tools        Use yt-dlp / JS runtime from PATH instead of downloading them
  --cache-minutes N     Delete cached audio after N minutes without use (default 120)"
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut o = Options {
        listen: std::env::var("LOCAL_PLAYLIST_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into()),
        data_dir: std::env::var_os("LOCAL_PLAYLIST_DATA").map(PathBuf::from),
        password: std::env::var("LOCAL_PLAYLIST_PASSWORD").ok().filter(|p| !p.is_empty()),
        system_tools: false,
        cache_minutes: 120,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().with_context(|| format!("{a} needs a value\n\n{}", usage()));
        match a.as_str() {
            "--listen" => o.listen = value()?,
            "--data-dir" => o.data_dir = Some(PathBuf::from(value()?)),
            "--password-file" => {
                let f = value()?;
                let p = std::fs::read_to_string(&f).with_context(|| format!("cannot read {f}"))?;
                o.password = Some(p.trim_end_matches(['\r', '\n']).to_string());
            }
            "--system-tools" => o.system_tools = true,
            "--cache-minutes" => o.cache_minutes = value()?.parse().context("--cache-minutes must be a number")?,
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => bail!("unknown option {other}\n\n{}", usage()),
        }
    }
    Ok(o)
}

fn read_password(o: &mut Options) -> Result<String> {
    if let Some(p) = o.password.take() {
        return Ok(p);
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        bail!("no access password: set LOCAL_PLAYLIST_PASSWORD or use --password-file");
    }
    let p1 = rpassword::prompt_password("Set an access password for the web UI: ")?;
    let p2 = rpassword::prompt_password("Repeat it: ")?;
    if p1 != p2 {
        bail!("passwords don't match");
    }
    Ok(p1)
}

pub fn run(args: &[String]) -> Result<()> {
    let mut o = parse_args(args)?;
    let password = read_password(&mut o)?;
    if password.chars().count() < 8 {
        bail!("the access password must be at least 8 characters");
    }

    let paths = match &o.data_dir {
        Some(d) => Paths { data: d.clone(), cache: d.join("cache") },
        None => Paths::new()?,
    };
    std::fs::create_dir_all(&paths.data)?;
    let source = if o.system_tools { Source::System } else { Source::Managed };
    let ytdlp = YtDlp::new(&paths, source, source);
    for c in ytdlp.missing_managed() {
        eprintln!("Downloading {}…", c.label());
        ytdlp.install(c, &|_, _| {}).with_context(|| format!("failed to download {}", c.label()))?;
    }
    let audio_dir = paths.cache.join("server-audio");
    let _ = std::fs::remove_dir_all(audio_dir.join(storage::DOWNLOADING_DIR));
    std::fs::create_dir_all(&audio_dir)?;

    let state = Arc::new(State {
        password,
        tokens: Default::default(),
        failures: Default::default(),
        ytdlp,
        paths,
        audio_dir,
        downloads: Default::default(),
    });

    // Keep yt-dlp current (sites change often) and expire cached audio.
    {
        let state = state.clone();
        let max_age = Duration::from_secs(o.cache_minutes * 60);
        std::thread::spawn(move || {
            let mut last_update = Instant::now();
            loop {
                std::thread::sleep(Duration::from_secs(300));
                cleanup(&state, max_age);
                crate::mem::trim();
                if last_update.elapsed() > Duration::from_secs(24 * 3600) && state.ytdlp.ytdlp_source == Source::Managed
                {
                    last_update = Instant::now();
                    match state.ytdlp.install(Component::YtDlp, &|_, _| {}) {
                        Ok(Some(i)) => eprintln!("yt-dlp updated to {}", i.short()),
                        Ok(None) => {}
                        Err(e) => eprintln!("yt-dlp update failed: {e:#}"),
                    }
                }
            }
        });
    }

    let server = Arc::new(Server::http(&o.listen).map_err(|e| anyhow::anyhow!("cannot listen on {}: {e}", o.listen))?);
    eprintln!("Local Playlist server listening on http://{}", o.listen);
    if !cfg!(web_ui) {
        eprintln!("note: built without the web UI (run scripts/build-web.sh first); only the API is available");
    }
    let workers: Vec<_> = (0..16)
        .map(|_| {
            let (server, state) = (server.clone(), state.clone());
            std::thread::spawn(move || {
                while let Ok(req) = server.recv() {
                    handle(&state, req);
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    Ok(())
}

// ------------------------------------------------------------------ routing

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn with_security<R: Read>(mut r: Response<R>) -> Response<R> {
    for (k, v) in [
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        ("Content-Security-Policy", CSP),
        ("Cross-Origin-Opener-Policy", "same-origin"),
    ] {
        r.add_header(header(k, v));
    }
    r
}

fn json(status: u16, value: serde_json::Value) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_data(value.to_string().into_bytes())
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Cache-Control", "no-store"))
}

fn error(status: u16, msg: impl std::fmt::Display) -> Response<std::io::Cursor<Vec<u8>>> {
    json(status, serde_json::json!({ "error": msg.to_string() }))
}

fn handle(state: &State, mut req: Request) {
    let url = match url::Url::parse(&format!("http://local{}", req.url())) {
        Ok(u) => u,
        Err(_) => return respond(req, error(400, "bad request")),
    };
    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
    let path = url.path().to_string();
    let method = req.method().clone();

    match (method, path.as_str()) {
        (Method::Get, "/" | "/index.html") => {
            static_file(req, INDEX_HTML.as_bytes(), "text/html; charset=utf-8", None, "no-cache")
        }
        (Method::Get, "/boot.js") => static_file(req, BOOT_JS.as_bytes(), "text/javascript", None, "no-cache"),
        #[cfg(web_ui)]
        (Method::Get, "/local_playlist.js") => static_file(req, APP_JS, "text/javascript", Some("gzip"), "no-cache"),
        #[cfg(web_ui)]
        (Method::Get, "/local_playlist_bg.wasm") => {
            static_file(req, APP_WASM, "application/wasm", Some("gzip"), "no-cache")
        }
        (Method::Get, p) if p.starts_with("/assets/icons/") && p.ends_with(".svg") => {
            match ICONS.iter().find(|(name, _)| *name == &p["/assets/icons/".len()..]) {
                Some((_, bytes)) => static_file(req, bytes, "image/svg+xml", None, "public, max-age=86400"),
                _ => respond(req, error(404, "not found")),
            }
        }
        (Method::Post, "/api/login") => {
            let ip = req.remote_addr().map(|a| a.ip());
            let mut body = String::new();
            let _ = req.as_reader().take(4096).read_to_string(&mut body);
            let resp = login(state, ip, &body);
            respond(req, resp)
        }
        (Method::Get, "/api/check") => {
            let ok = authorized(state, &req, &query);
            respond(req, if ok { json(200, serde_json::json!({ "ok": true })) } else { error(401, "unauthorized") })
        }
        (Method::Get, "/api/resolve") => {
            if !authorized(state, &req, &query) {
                return respond(req, error(401, "unauthorized"));
            }
            let resp = resolve(state, &query);
            respond(req, resp)
        }
        (Method::Get, "/api/segments") => {
            if !authorized(state, &req, &query) {
                return respond(req, error(401, "unauthorized"));
            }
            let resp = segments(&query);
            respond(req, resp)
        }
        (Method::Get, "/api/cover") => {
            if !authorized(state, &req, &query) {
                return respond(req, error(401, "unauthorized"));
            }
            let resp = cover(state, &query);
            respond(req, resp)
        }
        (Method::Get, "/api/audio") => {
            if !authorized(state, &req, &query) {
                return respond(req, error(401, "unauthorized"));
            }
            audio(state, req, &query)
        }
        _ => respond(req, error(404, "not found")),
    }
}

fn respond<R: Read>(req: Request, resp: Response<R>) {
    let _ = req.respond(with_security(resp));
}

/// Serve a file embedded in the binary, straight from the binary (no copy per
/// request: the web UI is 6 MB). With an ETag, so browsers revalidate instead
/// of downloading it again on every visit.
fn static_file(req: Request, bytes: &'static [u8], ty: &str, encoding: Option<&str>, cache: &str) {
    let etag = static_etag();
    let fresh = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("If-None-Match"))
        .is_some_and(|h| h.value.as_str().split(',').any(|t| t.trim().trim_start_matches("W/") == etag));
    let headers = [header("ETag", etag), header("Cache-Control", cache)];
    if fresh {
        let resp = Response::new(StatusCode(304), headers.to_vec(), std::io::empty(), Some(0), None);
        return respond(req, resp);
    }
    let mut resp =
        Response::new(StatusCode(200), headers.to_vec(), std::io::Cursor::new(bytes), Some(bytes.len()), None)
            .with_header(header("Content-Type", ty));
    if let Some(encoding) = encoding {
        resp.add_header(header("Content-Encoding", encoding));
    }
    respond(req, resp)
}

/// ETag shared by the embedded files: a hash of all of them, so it changes
/// whenever any does (i.e. with each build that changes the web UI).
fn static_etag() -> &'static str {
    use std::hash::{Hash as _, Hasher as _};
    static ETAG: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ETAG.get_or_init(|| {
        let mut h = std::hash::DefaultHasher::new();
        INDEX_HTML.hash(&mut h);
        BOOT_JS.hash(&mut h);
        #[cfg(web_ui)]
        {
            APP_JS.hash(&mut h);
            APP_WASM.hash(&mut h);
        }
        ICONS.hash(&mut h);
        format!("\"{:016x}\"", h.finish())
    })
}

// ------------------------------------------------------------------- auth

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn login(state: &State, ip: Option<IpAddr>, body: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    // Throttle guessing: 10 failures per address per 10 minutes.
    if let Some(ip) = ip {
        let mut f = state.failures.lock().unwrap();
        if let Some((count, since)) = f.get(&ip) {
            if since.elapsed() > Duration::from_secs(600) {
                f.remove(&ip);
            } else if *count >= 10 {
                return error(429, "too many attempts; try again later");
            }
        }
    }
    let password = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["password"].as_str().map(str::to_string))
        .unwrap_or_default();
    if !constant_time_eq(password.as_bytes(), state.password.as_bytes()) {
        std::thread::sleep(Duration::from_millis(800));
        if let Some(ip) = ip {
            let mut f = state.failures.lock().unwrap();
            let e = f.entry(ip).or_insert((0, Instant::now()));
            e.0 += 1;
        }
        return error(401, "wrong password");
    }
    let mut bytes = [0u8; 32];
    if getrandom::fill(&mut bytes).is_err() {
        return error(500, "rng failure");
    }
    let token = crate::ytdlp::hex(&bytes);
    state.tokens.lock().unwrap().insert(token.clone());
    json(200, serde_json::json!({ "token": token }))
}

fn authorized(state: &State, req: &Request, query: &HashMap<String, String>) -> bool {
    let bearer = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .and_then(|h| h.value.as_str().strip_prefix("Bearer ").map(str::to_string));
    // <audio> can't send headers, so media URLs carry the token as a parameter.
    let token = bearer.or_else(|| query.get("token").cloned());
    token.is_some_and(|t| state.tokens.lock().unwrap().contains(&t))
}

// ---------------------------------------------------------------- resolve

fn resolve(state: &State, query: &HashMap<String, String>) -> Response<std::io::Cursor<Vec<u8>>> {
    let Some(url) = query.get("url") else { return error(400, "missing url") };
    let link = match sources::parse_link(url) {
        Ok(l) => l,
        Err(e) => return error(400, e),
    };
    if matches!(link, Link::SoundCloudMyLikes)
        || matches!(&link, Link::YouTubePlaylist { list } if sources::youtube::is_personal_list(list))
    {
        return error(400, "Personal lists need a login, which the web version doesn't support. Use the desktop app.");
    }
    let collection = query.get("collection").is_some_and(|v| v == "1");
    match sources::resolve(&link, collection, &state.paths, &state.ytdlp, &Credentials::default()) {
        Ok(r) => json(200, serde_json::to_value(r).unwrap_or_default()),
        Err(e) => error(502, format!("{e:#}")),
    }
}

/// SponsorBlock segments of a YouTube video (the browser can't reach the API itself).
fn segments(query: &HashMap<String, String>) -> Response<std::io::Cursor<Vec<u8>>> {
    let Some(id) = query.get("id").filter(|id| sources::youtube::is_video_id(id)) else {
        return error(400, "missing or invalid id");
    };
    match crate::sponsorblock::fetch(id) {
        Ok(segs) => json(200, serde_json::to_value(segs).unwrap_or_default()),
        Err(e) => error(502, format!("{e:#}")),
    }
}

fn cover(state: &State, query: &HashMap<String, String>) -> Response<std::io::Cursor<Vec<u8>>> {
    let platform = match query.get("platform").map(String::as_str) {
        Some("yt") => Platform::YouTube,
        Some("sc") => Platform::SoundCloud,
        _ => return error(400, "missing or invalid platform"),
    };
    let Some(id) = query.get("id").filter(|id| crate::cover::valid_id(platform, id)) else {
        return error(400, "missing or invalid id");
    };
    let size = query.get("size").and_then(|s| crate::cover::Size::from_param(s)).unwrap_or(crate::cover::Size::Full);
    match crate::cover::get(&state.paths, platform, id, size) {
        Ok(bytes) => {
            let mime = crate::cover::image_format(&bytes).map_or("application/octet-stream", |f| f.mime());
            Response::from_data(bytes)
                .with_header(header("Content-Type", mime))
                .with_header(header("Cache-Control", "private, max-age=86400"))
        }
        Err(e) => error(502, format!("{e:#}")),
    }
}

// ------------------------------------------------------------------ audio

/// Build the track to download from a client-supplied link. Only single
/// YouTube videos and SoundCloud tracks are accepted.
fn track_for(url: &str) -> Result<Track> {
    match sources::parse_link(url)? {
        Link::YouTubeVideo { id, .. } => {
            Ok(Track::new(Platform::YouTube, id.clone(), sources::youtube::watch_url(&id), "", "", None))
        }
        Link::SoundCloudTrack { url } => {
            let path = url.trim_start_matches("https://soundcloud.com/");
            Ok(Track::new(Platform::SoundCloud, path.replace('/', "_"), url.clone(), "", "", None))
        }
        _ => bail!("not a single track link"),
    }
}

fn ensure_download(state: &State, track: &Track) -> Arc<Download> {
    let key = track.file_key();
    let mut map = state.downloads.lock().unwrap();
    if let Some(d) = map.get(&key) {
        return d.clone();
    }
    let tmp = state.audio_dir.join(storage::DOWNLOADING_DIR).join(&key);
    let dl = Arc::new(Download::new(tmp.clone(), key.clone()));
    map.insert(key.clone(), dl.clone());
    let (ytdlp, dir, track, dl2) = (state.ytdlp.clone(), state.audio_dir.clone(), track.clone(), dl.clone());
    std::thread::spawn(move || {
        let _ = std::fs::remove_dir_all(&tmp);
        let res = ytdlp.download(&track, &tmp, None, None, |p| dl2.set_progress(p)).and_then(|file| {
            let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("m4a").to_string();
            let dest = dir.join(format!("{key}.{ext}"));
            storage::move_file(&file, &dest)?;
            Ok(dest)
        });
        let _ = std::fs::remove_dir_all(&tmp);
        if let Err(e) = &res {
            eprintln!("download failed: {e:#}");
        }
        dl2.finish(res.map_err(|e| format!("{e:#}")));
    });
    dl
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("mp3") => "audio/mpeg",
        Some("ogg" | "oga") => "audio/ogg",
        Some("opus") => "audio/ogg; codecs=opus",
        Some("webm") => "audio/webm",
        Some("flac") => "audio/flac",
        Some("wav") => "audio/wav",
        _ => "audio/mp4",
    }
}

fn parse_range(req: &Request) -> Option<(u64, Option<u64>)> {
    let h = req.headers().iter().find(|h| h.field.equiv("Range"))?;
    let spec = h.value.as_str().strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    Some((start.trim().parse().ok()?, end.trim().parse().ok()))
}

fn audio(state: &State, req: Request, query: &HashMap<String, String>) {
    let track = match query.get("url").map(|u| track_for(u)) {
        Some(Ok(t)) => t,
        Some(Err(e)) => return respond(req, error(400, e)),
        None => return respond(req, error(400, "missing url")),
    };
    let key = track.file_key();
    let complete = |state: &State| {
        let running = state.downloads.lock().unwrap().get(&key).is_some_and(|d| matches!(d.state(), DlState::Running));
        if running { None } else { find_file(&state.audio_dir, &key) }
    };
    if let Some(file) = complete(state) {
        return serve_file(req, &file);
    }
    let dl = ensure_download(state, &track);
    // Wait for the first bytes (or completion).
    let deadline = Instant::now() + Duration::from_secs(90);
    let partial = loop {
        match dl.state() {
            DlState::Done(p) => return serve_file(req, &p),
            DlState::Failed(e) => {
                state.downloads.lock().unwrap().remove(&key);
                return respond(req, error(502, e));
            }
            DlState::Running => {}
        }
        if let Some(p) = dl.partial_file().filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() > 0)) {
            break p;
        }
        if Instant::now() > deadline {
            return respond(req, error(504, "timed out waiting for audio"));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // Stream the growing file (chunked) from the requested offset.
    let Ok(mut file) = File::open(&partial) else { return respond(req, error(500, "cannot open audio")) };
    let start = parse_range(&req).map(|r| r.0).unwrap_or(0);
    let _ = file.seek(SeekFrom::Start(start));
    let reader = GrowingReader { file, dl };
    let mut resp =
        Response::new(StatusCode(200), vec![header("Content-Type", content_type(&partial))], reader, None, None)
            .with_header(header("Cache-Control", "no-store"));
    if start > 0 {
        resp = resp.with_status_code(206);
        resp.add_header(header("Content-Range", &format!("bytes {start}-*/*")));
    }
    respond(req, resp);
}

fn serve_file(req: Request, path: &Path) {
    let Ok(mut file) = File::open(path) else { return respond(req, error(404, "not found")) };
    // Mark as recently used for the cache cleanup.
    let _ = file.set_modified(SystemTime::now());
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let ty = content_type(path);
    match parse_range(&req) {
        Some((start, end)) if start < len => {
            let end = end.unwrap_or(len - 1).min(len - 1);
            let count = end - start + 1;
            let _ = file.seek(SeekFrom::Start(start));
            let resp = Response::new(
                StatusCode(206),
                vec![
                    header("Content-Type", ty),
                    header("Accept-Ranges", "bytes"),
                    header("Content-Range", &format!("bytes {start}-{end}/{len}")),
                ],
                file.take(count),
                Some(count as usize),
                None,
            );
            respond(req, resp)
        }
        Some(_) => respond(
            req,
            Response::empty(StatusCode(416)).with_header(header("Content-Range", &format!("bytes */{len}"))),
        ),
        None => {
            let resp = Response::new(
                StatusCode(200),
                vec![header("Content-Type", ty), header("Accept-Ranges", "bytes")],
                file,
                Some(len as usize),
                None,
            );
            respond(req, resp)
        }
    }
}

/// Reads a file yt-dlp is still writing, waiting for more data until done.
struct GrowingReader {
    file: File,
    dl: Arc<Download>,
}

impl Read for GrowingReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let n = self.file.read(buf)?;
            if n > 0 {
                return Ok(n);
            }
            match self.dl.state() {
                DlState::Running => std::thread::sleep(Duration::from_millis(100)),
                DlState::Done(_) => return self.file.read(buf),
                DlState::Failed(e) => return Err(std::io::Error::other(e)),
            }
        }
    }
}

fn cleanup(state: &State, max_age: Duration) {
    // Forget finished downloads.
    state.downloads.lock().unwrap().retain(|_, d| matches!(d.state(), DlState::Running));
    let Ok(entries) = std::fs::read_dir(&state.audio_dir) else { return };
    for e in entries.flatten() {
        let Ok(m) = e.metadata() else { continue };
        let old = m.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > max_age);
        if m.is_file() && old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_single_tracks_are_downloadable() {
        assert!(track_for("https://www.youtube.com/watch?v=dQw4w9WgXcQ").is_ok());
        assert_eq!(
            track_for("https://soundcloud.com/forss/flickermood").unwrap().file_key(),
            "SoundCloud-forss_flickermood"
        );
        assert!(track_for("https://soundcloud.com/forss/sets/soulhack").is_err());
        assert!(track_for("https://example.com/evil").is_err());
        assert!(track_for("file:///etc/passwd").is_err());
    }

    #[test]
    fn eq() {
        assert!(constant_time_eq(b"abcdefgh", b"abcdefgh"));
        assert!(!constant_time_eq(b"abcdefgh", b"abcdefgx"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
