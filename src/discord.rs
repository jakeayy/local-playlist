//! Discord Rich Presence (desktop, off by default): shows what's playing on
//! the user's Discord profile ("Listening to …"), with the cover, a progress
//! bar, a play/pause icon and optional buttons to the song and the project.
//!
//! The app talks only to the Discord app on this computer, over its local IPC
//! socket (a Unix socket, or a named pipe on Windows): no Discord library, no
//! account token. Discord's servers then see the song, its link and its cover
//! URL (they fetch the cover themselves), which is why this is opt-in. The
//! fallback cover and the stock play/pause icons are Art Assets of the "Local
//! Playlist" Discord application (`docs/discord/`), whose name is what
//! "Listening to" shows by default. With colours from cover art on, the icon
//! takes the app's accent instead: Discord can't recolour an image, so it is
//! one of 24 pre-drawn hues (`docs/discord/accent/`, made by
//! [`tests::draws_accent_icons`]), which Discord loads from GitHub.
//!
//! The connection lives on its own thread: connecting, writing and waiting for
//! Discord's replies block, and Discord limits how often an activity may
//! change, so the thread sends only the latest state, spaced out. Closing the
//! connection (turning this off, quitting) makes Discord clear the activity.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};

use crate::model::{DiscordSettings, DiscordStatus, Paths, Platform, Track};

/// The "Local Playlist" application in the Discord Developer Portal.
const CLIENT_ID: &str = "1575047500807540766";
const GITHUB_URL: &str = "https://github.com/jakeayy/local-playlist";

/// Art Asset keys of the Discord application.
const ASSET_ICON: &str = "icon";
const ASSET_PLAYING: &str = "playing";
const ASSET_PAUSED: &str = "paused";
/// Play/pause icons in the accent's hue: `<playing|paused>-<degrees>.png`.
const ACCENT_ICONS: &str = "https://raw.githubusercontent.com/jakeayy/local-playlist/main/docs/discord/accent";
/// Hues of those icons, evenly spaced: close enough to the app's on a badge.
const ACCENT_STEPS: u16 = 24;

/// How far the progress bar may be off before it's sent again: seeking,
/// SponsorBlock jumps and stalls move it; the clock's jitter shouldn't.
const DRIFT_MS: i64 = 2000;
/// Discord allows 5 activity updates per 20 seconds.
const BURST: usize = 5;
const BURST_WINDOW: Duration = Duration::from_secs(20);
const MIN_GAP: Duration = Duration::from_secs(1);
/// Wait for things to settle before sending: a new track's cover colour comes
/// in just after the track, and skipping through tracks sends many.
const SETTLE: Duration = Duration::from_millis(500);
/// How often to look for Discord while it isn't running.
const RETRY: Duration = Duration::from_secs(15);

/// What's playing, as the main view sees it.
pub struct Now<'a> {
    pub track: &'a Track,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub paused: bool,
    /// Buffering: the position stands still, so the progress bar isn't moved.
    pub buffering: bool,
    /// The app's accent (hue, saturation), if it takes it from the cover.
    pub accent: Option<(f32, f32)>,
}

/// Whether the presence reaches Discord (shown in Settings).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connection {
    /// Nothing was shown yet, so nothing tried to connect.
    Idle,
    Connected,
    /// Discord isn't running, or its socket wasn't found.
    NotFound,
}

/// The activity to show; the worker thread turns it into Discord's JSON.
#[derive(Clone, Debug, PartialEq)]
struct Activity {
    title: String,
    artist: String,
    platform: Platform,
    source_id: String,
    url: String,
    /// Start and end of the song (Unix ms) for the progress bar; none while paused.
    timestamps: Option<(i64, Option<i64>)>,
    paused: bool,
    /// Hue of the play/pause icon, in degrees (a multiple of 360 / [`ACCENT_STEPS`]).
    accent: Option<u16>,
    settings: DiscordSettings,
}

/// What was last handed to the worker, to send only changes.
struct Shown {
    entry: String,
    paused: bool,
    start: i64,
    accent: Option<u16>,
    settings: DiscordSettings,
}

/// The accent icon hue nearest to `hue` (0..1), in degrees.
fn accent_step(hue: f32) -> u16 {
    let step = 360 / ACCENT_STEPS;
    ((hue.rem_euclid(1.0) * ACCENT_STEPS as f32).round() as u16 % ACCENT_STEPS) * step
}

/// Handle to the worker thread; dropping it disconnects from Discord.
pub struct Presence {
    tx: Sender<Option<Activity>>,
    shown: Option<Shown>,
    connection: Arc<AtomicU8>,
    last_connection: Connection,
}

impl Presence {
    pub fn start(paths: Paths) -> Self {
        let (tx, rx) = mpsc::channel();
        let connection = Arc::new(AtomicU8::new(Connection::Idle as u8));
        let state = connection.clone();
        let spawned =
            std::thread::Builder::new().name("discord".into()).spawn(move || Worker::new(paths, state).run(rx));
        if let Err(e) = spawned {
            eprintln!("Discord: can't start the presence thread: {e}");
        }
        Self { tx, shown: None, connection, last_connection: Connection::Idle }
    }

    pub fn connection(&self) -> Connection {
        match self.connection.load(Ordering::Relaxed) {
            1 => Connection::Connected,
            2 => Connection::NotFound,
            _ => Connection::Idle,
        }
    }

    /// Show `now` (or nothing). Cheap when nothing changed, so it can run on
    /// every tick. Returns whether [`Presence::connection`] changed since the
    /// last call.
    pub fn update(&mut self, now: Option<Now<'_>>, settings: &DiscordSettings) -> bool {
        match now {
            None => {
                if self.shown.take().is_some() {
                    let _ = self.tx.send(None);
                }
            }
            Some(now) => {
                let start = crate::now_ms() as i64 - now.position.as_millis() as i64;
                let accent = now.accent.map(|(hue, _)| accent_step(hue));
                let same = self.shown.as_ref().is_some_and(|s| {
                    s.entry == now.track.id
                        && s.paused == now.paused
                        && s.accent == accent
                        && s.settings == *settings
                        && (now.paused || now.buffering || (s.start - start).abs() < DRIFT_MS)
                });
                if !same {
                    let timestamps = (!now.paused).then(|| {
                        let end = now.duration.map(|d| start + d.as_millis() as i64).filter(|&end| end > start);
                        (start, end)
                    });
                    let track = now.track;
                    let _ = self.tx.send(Some(Activity {
                        title: track.title.clone(),
                        artist: track.artist.clone(),
                        platform: track.platform,
                        source_id: track.source_id.clone(),
                        url: track.url.clone(),
                        timestamps,
                        paused: now.paused,
                        accent,
                        settings: *settings,
                    }));
                    self.shown =
                        Some(Shown { entry: track.id.clone(), paused: now.paused, start, accent, settings: *settings });
                }
            }
        }
        let connection = self.connection();
        std::mem::replace(&mut self.last_connection, connection) != connection
    }
}

// --------------------------------------------------------------- worker

struct Worker {
    paths: Paths,
    state: Arc<AtomicU8>,
    conn: Option<Ipc>,
    /// What Discord shows from this connection; `None` = unknown (send again).
    shown: Option<Option<Activity>>,
    retry_at: Option<Instant>,
    /// When the latest state came in (see [`SETTLE`]).
    changed: Instant,
    sends: VecDeque<Instant>,
    /// SoundCloud artwork URLs by track id (`None`: the track has none).
    artwork: HashMap<String, Option<String>>,
}

impl Worker {
    fn new(paths: Paths, state: Arc<AtomicU8>) -> Self {
        Self {
            paths,
            state,
            conn: None,
            shown: None,
            retry_at: None,
            changed: Instant::now(),
            sends: VecDeque::new(),
            artwork: HashMap::new(),
        }
    }

    fn run(mut self, rx: Receiver<Option<Activity>>) {
        let mut desired: Option<Activity> = None;
        loop {
            let pending = self.shown.as_ref() != Some(&desired);
            let msg = match self.wait(pending, desired.is_some()) {
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                Some(wait) => rx.recv_timeout(wait),
            };
            match msg {
                Ok(activity) => {
                    // Only the latest state matters.
                    desired = activity;
                    while let Ok(activity) = rx.try_recv() {
                        desired = activity;
                    }
                    self.changed = Instant::now();
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if !pending {
                continue;
            }
            if self.conn.is_none() {
                if desired.is_none() {
                    // Not connected: Discord shows nothing from us anyway.
                    self.shown = Some(None);
                    continue;
                }
                self.connect();
                if self.conn.is_none() {
                    continue;
                }
            }
            self.send(&desired);
        }
        // Turned off or quitting: clear the status rather than rely on
        // Discord noticing the closed connection.
        if let Some(conn) = self.conn.as_mut()
            && matches!(self.shown, Some(Some(_)) | None)
        {
            let _ = conn.set_activity(None);
        }
    }

    /// How long to wait for news before acting: `None` = until there is some.
    fn wait(&self, pending: bool, want: bool) -> Option<Duration> {
        if !pending {
            return None;
        }
        let now = Instant::now();
        let mut at = now.max(self.changed + SETTLE);
        if self.conn.is_none() && want {
            at = at.max(self.retry_at.unwrap_or(now));
        }
        if let Some(&last) = self.sends.back() {
            at = at.max(last + MIN_GAP);
        }
        if self.sends.len() >= BURST {
            at = at.max(self.sends[self.sends.len() - BURST] + BURST_WINDOW);
        }
        Some(at - now)
    }

    /// Set the connection state; whether it changed.
    fn set_state(&mut self, connection: Connection) -> bool {
        self.state.swap(connection as u8, Ordering::Relaxed) != connection as u8
    }

    fn connect(&mut self) {
        match Ipc::connect() {
            Ok(ipc) => {
                self.conn = Some(ipc);
                self.shown = None;
                self.retry_at = None;
                if self.set_state(Connection::Connected) {
                    eprintln!("Discord: connected");
                }
            }
            Err(e) => {
                self.retry_at = Some(Instant::now() + RETRY);
                // Logged once, not on every retry.
                if self.set_state(Connection::NotFound) {
                    eprintln!("Discord: {e:#}");
                }
            }
        }
    }

    fn send(&mut self, desired: &Option<Activity>) {
        let activity = desired.as_ref().map(|a| {
            let cover = self.cover_url(a);
            activity_json(a, cover.as_deref())
        });
        let Some(conn) = self.conn.as_mut() else { return };
        let now = Instant::now();
        self.sends.push_back(now);
        while self.sends.front().is_some_and(|&t| now - t > BURST_WINDOW) {
            self.sends.pop_front();
        }
        match conn.set_activity(activity) {
            Ok(None) => self.shown = Some(desired.clone()),
            Ok(Some(error)) => {
                // Sending it again wouldn't help.
                eprintln!("Discord rejected the activity: {error}");
                self.shown = Some(desired.clone());
            }
            Err(e) => {
                // Discord quit or restarted: connect again soon.
                eprintln!("Discord: connection lost ({e:#})");
                self.conn = None;
                self.shown = None;
                self.retry_at = Some(Instant::now() + MIN_GAP);
            }
        }
    }

    /// A URL of the track's cover that Discord can load, if there is one.
    fn cover_url(&mut self, a: &Activity) -> Option<String> {
        if !crate::cover::valid_id(a.platform, &a.source_id) {
            return None;
        }
        match a.platform {
            // `mqdefault` (320×180) always exists and has no black bars;
            // Discord crops it to a square.
            Platform::YouTube => Some(format!("https://i.ytimg.com/vi/{}/mqdefault.jpg", a.source_id)),
            Platform::SoundCloud => {
                if let Some(url) = self.artwork.get(&a.source_id) {
                    return url.clone();
                }
                let url = match crate::sources::soundcloud::artwork_url(&a.source_id, &self.paths) {
                    // `-large` is 100×100.
                    Ok(url) => url.map(|u| u.replace("-large.", "-t500x500.")),
                    Err(e) => {
                        eprintln!("Discord: no SoundCloud artwork: {e:#}");
                        None
                    }
                }
                .filter(|u| u.starts_with("https://") && u.len() <= 256);
                if self.artwork.len() >= 256 {
                    self.artwork.clear();
                }
                self.artwork.insert(a.source_id.clone(), url.clone());
                url
            }
        }
    }
}

/// Discord's activity object for `a`.
fn activity_json(a: &Activity, cover: Option<&str>) -> Value {
    let (state, small_text) = if a.paused { (ASSET_PAUSED, "Paused") } else { (ASSET_PLAYING, "Playing") };
    let small_image = match a.accent {
        Some(degrees) => format!("{ACCENT_ICONS}/{state}-{degrees:03}.png"),
        None => state.to_string(),
    };
    let mut activity = json!({
        // Listening
        "type": 2,
        "details": text(&a.title),
        "assets": {
            "large_image": cover.unwrap_or(ASSET_ICON),
            "small_image": small_image,
            "small_text": small_text,
        },
    });
    let artist = a.artist.trim();
    if !artist.is_empty() {
        activity["state"] = text(artist).into();
    }
    // What "Listening to …" names: 0 = the application, 1 = `state`.
    let by_artist = a.settings.status == DiscordStatus::Artist && !artist.is_empty();
    activity["status_display_type"] = if by_artist { 1 } else { 0 }.into();
    if let Some((start, end)) = a.timestamps {
        let mut timestamps = json!({ "start": start });
        if let Some(end) = end {
            timestamps["end"] = end.into();
        }
        activity["timestamps"] = timestamps;
    }
    let mut buttons = Vec::new();
    if a.settings.song_button
        && let Some(url) = song_url(a)
    {
        buttons.push(json!({ "label": "Listen to song", "url": url }));
    }
    if a.settings.github_button {
        buttons.push(json!({ "label": "GitHub", "url": GITHUB_URL }));
    }
    if !buttons.is_empty() {
        activity["buttons"] = buttons.into();
    }
    activity
}

/// The song's page: the link it was added from, or one made from its id.
fn song_url(a: &Activity) -> Option<String> {
    let url = if a.url.starts_with("https://") {
        a.url.clone()
    } else if a.platform == Platform::YouTube && crate::cover::valid_id(a.platform, &a.source_id) {
        crate::sources::youtube::watch_url(&a.source_id)
    } else {
        return None;
    };
    (url.len() <= 512).then_some(url)
}

/// A text field as Discord accepts it: 2 to 128 characters (UTF-16 units).
fn text(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(128));
    let mut units = 0;
    for c in s.trim().chars() {
        units += c.len_utf16();
        if units > 128 {
            // Make room for the ellipsis.
            while out.encode_utf16().count() > 127 {
                out.pop();
            }
            out.push('…');
            break;
        }
        out.push(c);
    }
    // Discord rejects one-character fields; pad with a space it keeps.
    while out.encode_utf16().count() < 2 {
        out.push('\u{2800}');
    }
    out
}

// ------------------------------------------------------------------ IPC

/// Frame opcodes of Discord's IPC.
const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const OP_PING: u32 = 3;
const OP_PONG: u32 = 4;
const MAX_FRAME: usize = 1 << 20;

trait Pipe: Read + Write + Send {}
impl<T: Read + Write + Send> Pipe for T {}

struct Ipc {
    pipe: Box<dyn Pipe>,
    nonce: u64,
}

impl Ipc {
    /// Connect and handshake with the first Discord that answers.
    fn connect() -> Result<Self> {
        let pipe = open_pipe().context("Discord isn't running")?;
        let mut ipc = Self { pipe, nonce: 0 };
        ipc.write(OP_HANDSHAKE, &json!({ "v": 1, "client_id": CLIENT_ID }))?;
        let ready = ipc.read_reply()?;
        if ready["evt"] != "READY" {
            bail!("unexpected handshake reply: {}", ready["evt"]);
        }
        Ok(ipc)
    }

    /// Set (or with `None` clear) the activity. `Ok(Some(_))` is Discord's
    /// error message for a rejected activity.
    fn set_activity(&mut self, activity: Option<Value>) -> Result<Option<String>> {
        self.nonce += 1;
        let mut args = json!({ "pid": std::process::id() });
        if let Some(activity) = activity {
            args["activity"] = activity;
        }
        let nonce = self.nonce.to_string();
        self.write(OP_FRAME, &json!({ "cmd": "SET_ACTIVITY", "args": args, "nonce": nonce }))?;
        loop {
            let reply = self.read_reply()?;
            if reply["nonce"] != nonce.as_str() {
                continue;
            }
            if reply["evt"] == "ERROR" {
                return Ok(Some(reply["data"]["message"].as_str().unwrap_or("unknown error").to_string()));
            }
            return Ok(None);
        }
    }

    fn write(&mut self, op: u32, payload: &Value) -> io::Result<()> {
        self.pipe.write_all(&encode(op, payload))?;
        self.pipe.flush()
    }

    /// The next message frame, answering pings on the way.
    fn read_reply(&mut self) -> Result<Value> {
        loop {
            let mut header = [0; 8];
            self.pipe.read_exact(&mut header)?;
            let op = u32::from_le_bytes(header[..4].try_into().unwrap());
            let len = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
            if len > MAX_FRAME {
                bail!("frame too large ({len} bytes)");
            }
            let mut body = vec![0; len];
            self.pipe.read_exact(&mut body)?;
            let payload: Value = serde_json::from_slice(&body).context("invalid frame")?;
            match op {
                OP_FRAME => return Ok(payload),
                OP_PING => self.write(OP_PONG, &payload)?,
                OP_CLOSE => bail!("Discord closed the connection: {}", payload["message"].as_str().unwrap_or("")),
                _ => {}
            }
        }
    }
}

fn encode(op: u32, payload: &Value) -> Vec<u8> {
    let body = payload.to_string();
    let mut frame = Vec::with_capacity(8 + body.len());
    frame.extend_from_slice(&op.to_le_bytes());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(body.as_bytes());
    frame
}

/// Discord listens on `discord-ipc-0` to `-9` (several Discords can run).
#[cfg(unix)]
fn open_pipe() -> io::Result<Box<dyn Pipe>> {
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;

    let mut dirs: Vec<PathBuf> = Vec::new();
    for var in ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"] {
        if let Some(dir) = std::env::var_os(var).filter(|d| !d.is_empty()) {
            dirs.push(dir.into());
        }
    }
    dirs.push("/tmp".into());
    dirs.dedup();
    // Sandboxed Discords (Flatpak, Snap, Vesktop) put the socket in their own folder.
    const SUBDIRS: [&str; 5] = [
        "",
        "app/com.discordapp.Discord",
        "app/com.discordapp.DiscordCanary",
        "snap.discord",
        ".flatpak/dev.vencord.Vesktop/xdg-run",
    ];
    let mut last = io::Error::from(io::ErrorKind::NotFound);
    for i in 0..10 {
        for dir in &dirs {
            for sub in SUBDIRS {
                let path = dir.join(sub).join(format!("discord-ipc-{i}"));
                match UnixStream::connect(&path) {
                    Ok(stream) => {
                        // A hung Discord shouldn't hang this thread for good.
                        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
                        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
                        return Ok(Box::new(stream));
                    }
                    Err(e) if e.kind() != io::ErrorKind::NotFound => last = e,
                    Err(_) => {}
                }
            }
        }
    }
    Err(last)
}

#[cfg(windows)]
fn open_pipe() -> io::Result<Box<dyn Pipe>> {
    let mut last = io::Error::from(io::ErrorKind::NotFound);
    for i in 0..10 {
        match std::fs::OpenOptions::new().read(true).write(true).open(format!(r"\\.\pipe\discord-ipc-{i}")) {
            Ok(pipe) => return Ok(Box::new(pipe)),
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[cfg(not(any(unix, windows)))]
fn open_pipe() -> io::Result<Box<dyn Pipe>> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity(settings: DiscordSettings) -> Activity {
        Activity {
            title: "Extinction".into(),
            artist: "Killstation".into(),
            platform: Platform::YouTube,
            source_id: "4Skf2zDFnsM".into(),
            url: "https://www.youtube.com/watch?v=4Skf2zDFnsM".into(),
            timestamps: Some((1_000_000, Some(1_085_000))),
            paused: false,
            accent: None,
            settings,
        }
    }

    #[test]
    fn frames() {
        let frame = encode(OP_HANDSHAKE, &json!({ "v": 1 }));
        assert_eq!(&frame[..4], &[0, 0, 0, 0]);
        assert_eq!(&frame[4..8], &7u32.to_le_bytes());
        assert_eq!(&frame[8..], br#"{"v":1}"#);
    }

    #[test]
    fn builds_the_activity() {
        let settings = DiscordSettings { enabled: true, song_button: true, github_button: true, ..Default::default() };
        let a = activity(settings);
        let json = activity_json(&a, Some("https://i.ytimg.com/vi/4Skf2zDFnsM/mqdefault.jpg"));
        assert_eq!(json["type"], 2);
        assert_eq!(json["details"], "Extinction");
        assert_eq!(json["state"], "Killstation");
        assert_eq!(json["status_display_type"], 0);
        assert_eq!(json["timestamps"]["end"], 1_085_000);
        assert_eq!(json["assets"]["small_image"], ASSET_PLAYING);
        assert_eq!(json["buttons"][0]["url"], "https://www.youtube.com/watch?v=4Skf2zDFnsM");
        assert_eq!(json["buttons"][1]["url"], GITHUB_URL);
        let tinted = Activity { accent: Some(255), ..a.clone() };
        let json = activity_json(&tinted, None);
        assert_eq!(json["assets"]["small_image"], format!("{ACCENT_ICONS}/playing-255.png"));

        let by_artist = DiscordSettings { status: DiscordStatus::Artist, song_button: false, ..settings };
        let paused = Activity { paused: true, timestamps: None, ..activity(by_artist) };
        let json = activity_json(&paused, None);
        assert_eq!(json["status_display_type"], 1);
        assert_eq!(json["assets"]["large_image"], ASSET_ICON);
        assert_eq!(json["assets"]["small_image"], ASSET_PAUSED);
        assert!(json.get("timestamps").is_none());
        assert_eq!(json["buttons"].as_array().unwrap().len(), 1);

        // Without an artist, "Listening to" names the app.
        let no_artist = Activity { artist: " ".into(), ..activity(by_artist) };
        let json = activity_json(&no_artist, None);
        assert_eq!(json["status_display_type"], 0);
        assert!(json.get("state").is_none());
    }

    #[test]
    fn picks_the_nearest_accent_icon() {
        assert_eq!(accent_step(0.0), 0);
        assert_eq!(accent_step(0.99), 0);
        assert_eq!(accent_step(0.5), 180);
        assert_eq!(accent_step(0.72), 255);
        assert_eq!(accent_step(1.3), 105);
    }

    fn accent_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/discord/accent")
    }

    /// Every icon the app may link to is in the repository.
    #[test]
    fn accent_icons_exist() {
        for step in 0..ACCENT_STEPS {
            for state in [ASSET_PLAYING, ASSET_PAUSED] {
                let file = accent_dir().join(format!("{state}-{:03}.png", step * (360 / ACCENT_STEPS)));
                assert!(file.is_file(), "{} is missing: run draws_accent_icons", file.display());
            }
        }
    }

    /// Draw the accent icons into `docs/discord/accent/`: the stock icons
    /// (`docs/discord/*.svg`) in the colours the cover-tinted dark theme gives
    /// its primary colours, for a cover of each hue and typical saturation.
    #[test]
    #[ignore = "writes docs/discord/accent"]
    fn draws_accent_icons() {
        const BG: (u8, u8, u8) = (0xD0, 0xBC, 0xFF);
        const FG: (u8, u8, u8) = (0x38, 0x1E, 0x72);
        std::fs::create_dir_all(accent_dir()).unwrap();
        for step in 0..ACCENT_STEPS {
            let degrees = step * (360 / ACCENT_STEPS);
            let accent = (degrees as f32 / 360.0, crate::theme::REFERENCE_SATURATION);
            let (bg, fg) = (crate::theme::tint(BG, accent), crate::theme::tint(FG, accent));
            for (state, paused) in [(ASSET_PLAYING, false), (ASSET_PAUSED, true)] {
                let file = accent_dir().join(format!("{state}-{degrees:03}.png"));
                image::RgbaImage::from_raw(128, 128, draw_icon(paused, bg, fg, 128)).unwrap().save(&file).unwrap();
            }
        }
    }

    /// A play or pause glyph on a disc, `size`×`size` straight-alpha RGBA.
    /// Shapes as in the SVGs: a 24-unit glyph placed at (28, 28), scaled by 3,
    /// on a 128-unit disc; filled, with a 2-unit round-joined outline.
    fn draw_icon(paused: bool, bg: (u8, u8, u8), fg: (u8, u8, u8), size: u32) -> Vec<u8> {
        use crate::tray::segment;
        let g = |x: f32, y: f32| (28.0 + 3.0 * x, 28.0 + 3.0 * y);
        let triangle = [g(7.0, 4.5), g(7.0, 19.5), g(20.0, 12.0)];
        // Signed distance to a box (centre, half size) with rounded corners.
        let rounded_box = |x: f32, y: f32, (cx, cy): (f32, f32), (hx, hy): (f32, f32), r: f32| {
            let (qx, qy) = ((x - cx).abs() - (hx - r), (y - cy).abs() - (hy - r));
            qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
        };
        let half_stroke = 3.0;
        let scale = size as f32 / 128.0;
        let mut out = Vec::with_capacity((size * size * 4) as usize);
        for py in 0..size {
            for px in 0..size {
                let (x, y) = ((px as f32 + 0.5) / scale, (py as f32 + 0.5) / scale);
                let disc = (x - 64.0).hypot(y - 64.0) - 64.0;
                let glyph = if paused {
                    // 4×16-unit bars with 1-unit corners at x = 6 and 14, y = 4.
                    let bar = |left: f32| {
                        let (l, t) = g(left, 4.0);
                        rounded_box(x, y, (l + 6.0, t + 24.0), (6.0, 24.0), 3.0)
                    };
                    bar(6.0).min(bar(14.0)) - half_stroke
                } else {
                    let edges = (0..3).map(|i| (triangle[i], triangle[(i + 1) % 3]));
                    let d = edges.clone().map(|(a, b)| segment(x, y, a, b)).fold(f32::MAX, f32::min);
                    let side = |(a, b): ((f32, f32), (f32, f32))| (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
                    let sides: Vec<f32> = edges.map(side).collect();
                    let inside = sides.iter().all(|&s| s >= 0.0) || sides.iter().all(|&s| s <= 0.0);
                    (if inside { -d } else { d }) - half_stroke
                };
                let cover = |d: f32| (0.5 - d * scale).clamp(0.0, 1.0);
                let ink = cover(glyph);
                let mix = |b: u8, f: u8| (b as f32 + (f as f32 - b as f32) * ink).round() as u8;
                out.extend([mix(bg.0, fg.0), mix(bg.1, fg.1), mix(bg.2, fg.2)]);
                out.push((cover(disc) * 255.0).round() as u8);
            }
        }
        out
    }

    #[test]
    fn fits_text_fields() {
        assert_eq!(text("ab"), "ab");
        assert_eq!(text("a").encode_utf16().count(), 2);
        let long = "😀".repeat(100);
        let t = text(&long);
        assert!(t.encode_utf16().count() <= 128);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn sends_only_changes() {
        let (tx, rx) = mpsc::channel();
        let mut p = Presence { tx, shown: None, connection: Arc::default(), last_connection: Connection::Idle };
        let settings = DiscordSettings { enabled: true, ..Default::default() };
        let track = Track::new(Platform::YouTube, "4Skf2zDFnsM", "u", "Extinction", "Killstation", Some(85));
        let now = |secs, paused, buffering| Now {
            track: &track,
            position: Duration::from_secs(secs),
            duration: Some(Duration::from_secs(85)),
            paused,
            buffering,
            accent: None,
        };
        p.update(Some(now(0, false, false)), &settings);
        assert!(rx.try_recv().unwrap().is_some());
        // Playing on: the progress bar runs by itself.
        p.update(Some(now(0, false, false)), &settings);
        assert!(rx.try_recv().is_err());
        // A seek, a pause, a settings change, stopping.
        p.update(Some(now(40, false, false)), &settings);
        assert!(rx.try_recv().unwrap().is_some());
        // Stalled while buffering: not sent.
        p.update(Some(now(10, false, true)), &settings);
        assert!(rx.try_recv().is_err());
        p.update(Some(now(40, true, false)), &settings);
        assert!(rx.try_recv().unwrap().unwrap().timestamps.is_none());
        p.update(Some(now(40, true, false)), &DiscordSettings { github_button: !settings.github_button, ..settings });
        assert!(rx.try_recv().unwrap().is_some());
        // The cover's colour came in.
        p.update(Some(Now { accent: Some((0.5, 0.6)), ..now(40, true, false) }), &settings);
        assert_eq!(rx.try_recv().unwrap().unwrap().accent, Some(180));
        p.update(None, &settings);
        assert!(rx.try_recv().unwrap().is_none());
        p.update(None, &settings);
        assert!(rx.try_recv().is_err());
    }

    /// Shows "Extinction" by Killstation (YouTube) on the profile of the
    /// Discord running on this computer, playing and then paused, then a
    /// SoundCloud track with its artwork, through the same path as the app.
    /// Watch the profile while it runs; "rejected" on stderr is a failure.
    #[test]
    #[ignore = "needs Discord running"]
    fn discord_shows_on_profile() {
        let dir = std::env::temp_dir().join(format!("lp-discord-test-{}", std::process::id()));
        let paths = Paths { data: dir.join("data"), cache: dir.join("cache") };
        let settings = DiscordSettings { enabled: true, ..Default::default() };
        let mut presence = Presence::start(paths.clone());
        let show = |presence: &mut Presence, track: &Track, position: u64, paused: bool| {
            let duration = track.duration_secs.map(|s| Duration::from_secs(s.into()));
            let now = Now {
                track,
                position: Duration::from_secs(position),
                duration,
                paused,
                buffering: false,
                accent: None,
            };
            presence.update(Some(now), &settings);
        };

        let youtube = Track::new(
            Platform::YouTube,
            "4Skf2zDFnsM",
            "https://www.youtube.com/watch?v=4Skf2zDFnsM",
            "Extinction",
            "Killstation",
            Some(85),
        );
        show(&mut presence, &youtube, 20, false);
        std::thread::sleep(Duration::from_secs(10));
        assert_eq!(presence.connection(), Connection::Connected);
        show(&mut presence, &youtube, 30, true);
        std::thread::sleep(Duration::from_secs(5));

        let link = crate::sources::parse_link("https://soundcloud.com/forss/flickermood").unwrap();
        let soundcloud = crate::sources::soundcloud::resolve(&link, &paths, None).unwrap().into_tracks().remove(0);
        show(&mut presence, &soundcloud, 0, false);
        std::thread::sleep(Duration::from_secs(10));

        presence.update(None, &settings);
        std::thread::sleep(Duration::from_secs(3));
        drop(presence);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
