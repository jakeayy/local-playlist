//! Soulseek: finding another copy of a track on the Soulseek network.
//!
//! Soulseek is peer to peer. A central server logs users in and passes
//! searches on to the network; results and files come straight from other
//! users' computers. This is a small client for just what "find this song on
//! Soulseek" needs: log in, search, and download one file. It shares nothing
//! and doesn't listen for connections, so the only peers it reaches are ones
//! that accept connections (most do): the server asks them to connect to us,
//! they can't, and ask the server to have us connect to them instead
//! ("indirect" connections, answered with `PierceFireWall`).
//!
//! Protocol notes, from the Nicotine+ documentation: every number is little
//! endian; a string is a `u32` length and its bytes. Server and peer messages
//! are `u32 length, u32 code, body`; the first message on a peer connection
//! is `u32 length, u8 code, body`. File connections carry no framing: the
//! uploader sends the transfer token (`u32`), we send the offset to start at
//! (`u64`), and the file's bytes follow.
//!
//! Downloads use the current flow: we queue the file (`QueueUpload`); when
//! its turn comes the uploader asks to send it (`TransferRequest`, possibly on
//! a new connection), we accept, and it opens a file connection.
//!
//! Every connection runs on its own thread, with timeouts, and every length
//! read off the network is capped before anything is allocated for it.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};

const SERVER: &str = "server.slsknet.org:2242";
/// The client version Nicotine+ reports; the server rejects unknown old ones.
const VERSION: u32 = 160;
const MINOR_VERSION: u32 = 1;

// Server message codes.
const LOGIN: u32 = 1;
const GET_PEER_ADDRESS: u32 = 3;
const CONNECT_TO_PEER: u32 = 18;
const FILE_SEARCH: u32 = 26;
const SHARED_FOLDERS_FILES: u32 = 35;
const RELOGGED: u32 = 41;

// Peer message codes.
const FILE_SEARCH_RESPONSE: u32 = 9;
const TRANSFER_REQUEST: u32 = 40;
const TRANSFER_RESPONSE: u32 = 41;
const QUEUE_UPLOAD: u32 = 43;
const PLACE_IN_QUEUE_RESPONSE: u32 = 44;
const UPLOAD_FAILED: u32 = 46;
const UPLOAD_DENIED: u32 = 50;
const PLACE_IN_QUEUE_REQUEST: u32 = 51;

// Peer init codes.
const PIERCE_FIREWALL: u8 = 0;
const PEER_INIT: u8 = 1;

/// Largest server message we read (room lists are the big ones).
const MAX_SERVER_MESSAGE: usize = 16 << 20;
/// Largest peer message, and largest search response once inflated.
const MAX_PEER_MESSAGE: usize = 16 << 20;
const MAX_INFLATED: u64 = 64 << 20;
/// Search results kept (popular songs get thousands).
const MAX_RESULTS: usize = 1500;
/// Peer connections open at once; more are dropped (a search can bring hundreds).
const MAX_PEER_THREADS: usize = 48;
/// How long after a search its results are still taken.
const SEARCH_WINDOW: Duration = Duration::from_secs(120);

/// File types the player can decode.
pub const PLAYABLE: &[&str] = &["flac", "mp3", "m4a", "ogg", "oga", "aac", "wav"];
const LOSSLESS: &[&str] = &["flac", "wav"];

/// One file a search found.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchResult {
    pub user: String,
    /// The path as the peer sent it (needed byte for byte to download it).
    filename: Vec<u8>,
    pub size: u64,
    /// Lowercase file extension.
    pub ext: String,
    /// kbit/s.
    pub bitrate: Option<u32>,
    pub duration_secs: Option<u32>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub free_slot: bool,
    /// Upload speed, bytes/s.
    pub speed: u32,
    pub queue: u32,
}

impl SearchResult {
    /// The full path, for showing.
    pub fn path(&self) -> String {
        String::from_utf8_lossy(&self.filename).into_owned()
    }

    /// File name without the folders.
    pub fn file_name(&self) -> String {
        let path = self.path();
        path.rsplit(['\\', '/']).next().unwrap_or_default().to_string()
    }

    /// The folder holding the file (usually "Artist - Album").
    pub fn folder(&self) -> String {
        let path = self.path();
        let mut parts = path.rsplit(['\\', '/']).skip(1);
        parts.next().unwrap_or_default().to_string()
    }

    pub fn lossless(&self) -> bool {
        LOSSLESS.contains(&self.ext.as_str())
    }

    /// Bitrate as reported, or worked out from size and length.
    pub fn kbps(&self) -> Option<u32> {
        self.bitrate.filter(|b| *b > 0).or_else(|| {
            let secs = self.duration_secs.filter(|d| *d > 0)?;
            Some((self.size * 8 / 1000 / secs as u64) as u32)
        })
    }

    /// "FLAC 24/96", "MP3 320", "OGG".
    pub fn quality_label(&self) -> String {
        let ext = self.ext.to_uppercase();
        if self.lossless() {
            match (self.bit_depth, self.sample_rate) {
                (Some(bits), Some(rate)) => format!("{ext} {bits}/{}", fmt_rate(rate)),
                (None, Some(rate)) => format!("{ext} {}", fmt_rate(rate)),
                _ => ext,
            }
        } else {
            match self.kbps() {
                Some(kbps) => format!("{ext} {kbps}"),
                None => ext,
            }
        }
    }

    /// Ordering key, best first: lossless before lossy, then the higher
    /// resolution or bitrate, then the sooner it can start.
    fn rank(&self) -> (std::cmp::Reverse<(u8, u32, u32)>, bool, u32, std::cmp::Reverse<u32>) {
        let quality = if self.lossless() {
            (2, self.bit_depth.unwrap_or(16), self.sample_rate.unwrap_or(44_100))
        } else {
            // Bitrates within a step count as equal (VBR files land anywhere near 320).
            (1, self.kbps().unwrap_or(0) / 32, 0)
        };
        (std::cmp::Reverse(quality), !self.free_slot, self.queue, std::cmp::Reverse(self.speed))
    }
}

fn fmt_rate(hz: u32) -> String {
    if hz % 1000 == 0 { (hz / 1000).to_string() } else { format!("{:.1}", hz as f32 / 1000.0) }
}

/// Sort results best first (see [`SearchResult::rank`]).
pub fn sort_by_quality(results: &mut [SearchResult]) {
    results.sort_by_cached_key(|r| r.rank());
}

/// What to search for a track: "Artist - Title" (shown, and editable).
pub fn query_for(artist: &str, title: &str) -> String {
    match crate::lyrics::query::guesses(title, artist).into_iter().next() {
        Some(q) if !q.artist.is_empty() => format!("{} - {}", q.artist, q.title),
        Some(q) => q.title,
        None => title.to_string(),
    }
}

/// The search terms sent for `text`. Soulseek matches words anywhere in a
/// file's path; a word starting with `-` excludes files, so punctuation
/// (the dash in "Artist - Title" included) becomes spaces.
pub fn search_terms(text: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for word in text.split(|c: char| !(c.is_alphanumeric() || c == '\'')) {
        let word = word.trim_matches('\'').to_lowercase();
        if !word.is_empty() && !words.contains(&word) {
            words.push(word);
        }
    }
    words.join(" ")
}

/// A download, shared with the threads doing it.
pub struct Transfer {
    pub user: String,
    filename: Vec<u8>,
    /// Where the finished file goes.
    pub dest: PathBuf,
    state: Mutex<TransferState>,
    /// Token and size the uploader gave in its `TransferRequest`.
    offer: Mutex<Option<(u32, u64)>>,
    cancel: AtomicBool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferState {
    Connecting,
    /// Waiting in the uploader's queue (at this place, if it said).
    Queued(Option<u32>),
    /// Bytes received of the total.
    Downloading(u64, u64),
    Done,
    Failed(String),
}

impl Transfer {
    pub fn state(&self) -> TransferState {
        self.state.lock().unwrap().clone()
    }

    pub fn finished(&self) -> bool {
        matches!(self.state(), TransferState::Done | TransferState::Failed(_))
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.fail("Cancelled");
    }

    fn set(&self, state: TransferState) {
        let mut s = self.state.lock().unwrap();
        if !matches!(*s, TransferState::Done | TransferState::Failed(_)) {
            *s = state;
        }
    }

    fn fail(&self, why: impl Into<String>) {
        self.set(TransferState::Failed(why.into()));
    }

    fn part_file(&self) -> PathBuf {
        let mut name = self.dest.file_name().unwrap_or_default().to_os_string();
        name.push(".part");
        self.dest.with_file_name(name)
    }
}

/// A logged-in session. Dropping it disconnects (downloads in progress end).
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    username: String,
    server: Mutex<TcpStream>,
    state: Mutex<State>,
    closed: AtomicBool,
    peer_threads: AtomicUsize,
}

#[derive(Default)]
struct State {
    search: Option<(u32, Instant)>,
    results: Vec<SearchResult>,
    /// Users whose results arrived for the current search.
    responders: usize,
    addr_waiters: Vec<(String, mpsc::Sender<SocketAddrV4>)>,
    transfers: Vec<Arc<Transfer>>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.inner.closed.store(true, Ordering::Relaxed);
        let _ = self.inner.server.lock().unwrap().shutdown(Shutdown::Both);
    }
}

impl Client {
    /// Log in (blocking). A username nobody has yet makes a new account with
    /// that password.
    pub fn login(username: &str, password: &str) -> Result<Client> {
        let username = username.trim();
        if username.is_empty() || password.is_empty() {
            bail!("Enter a Soulseek username and password");
        }
        let addr = server_addr()?;
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(20))
            .with_context(|| "Couldn't reach the Soulseek server")?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;
        let _ = stream.set_nodelay(true);

        let hash = md5_hex(&format!("{username}{password}"));
        let mut w = Writer::default();
        w.str(username.as_bytes()).str(password.as_bytes()).u32(VERSION).str(hash.as_bytes()).u32(MINOR_VERSION);
        stream.write_all(&w.frame(LOGIN))?;
        let body = loop {
            let (code, body) = read_message(&mut stream, MAX_SERVER_MESSAGE).context("Soulseek login")?;
            if code == LOGIN {
                break body;
            }
        };
        let mut r = Reader::new(&body);
        if r.u8()? == 0 {
            let reason = r.string().unwrap_or_default();
            bail!(match reason.as_str() {
                "INVALIDPASS" => "Wrong password for this Soulseek username".to_string(),
                "INVALIDUSERNAME" => "Soulseek doesn't accept that username".to_string(),
                "SVRFULL" => "The Soulseek server is full; try again later".to_string(),
                other => format!("Soulseek refused the login ({other})"),
            });
        }
        // We share nothing, and say so.
        let mut w = Writer::default();
        w.u32(0).u32(0);
        stream.write_all(&w.frame(SHARED_FOLDERS_FILES))?;
        stream.set_read_timeout(None)?;

        let reader = stream.try_clone()?;
        let inner = Arc::new(Inner {
            username: username.to_string(),
            server: Mutex::new(stream),
            state: Mutex::default(),
            closed: AtomicBool::new(false),
            peer_threads: AtomicUsize::new(0),
        });
        let inner2 = inner.clone();
        std::thread::Builder::new().name("soulseek".into()).spawn(move || server_loop(inner2, reader))?;
        Ok(Client { inner })
    }

    pub fn username(&self) -> &str {
        &self.inner.username
    }

    /// The server connection ended (network trouble, or logged in elsewhere).
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Relaxed)
    }

    /// Start a search, replacing the previous one's results.
    pub fn search(&self, text: &str) -> Result<()> {
        let terms = search_terms(text);
        if terms.is_empty() {
            bail!("Type something to search for");
        }
        let token = random_u32();
        {
            let mut state = self.inner.state.lock().unwrap();
            state.search = Some((token, Instant::now()));
            state.results.clear();
            state.responders = 0;
        }
        let mut w = Writer::default();
        w.u32(token).str(terms.as_bytes());
        self.inner.send(&w.frame(FILE_SEARCH))
    }

    /// (results so far, users they came from).
    pub fn result_count(&self) -> (usize, usize) {
        let state = self.inner.state.lock().unwrap();
        (state.results.len(), state.responders)
    }

    /// The current search's results, best first.
    pub fn results(&self) -> Vec<SearchResult> {
        let mut results = self.inner.state.lock().unwrap().results.clone();
        sort_by_quality(&mut results);
        results
    }

    /// Download `result` to `dest` (its folder must exist).
    pub fn download(&self, result: &SearchResult, dest: PathBuf) -> Arc<Transfer> {
        let transfer = Arc::new(Transfer {
            user: result.user.clone(),
            filename: result.filename.clone(),
            dest,
            state: Mutex::new(TransferState::Connecting),
            offer: Mutex::new(None),
            cancel: AtomicBool::new(false),
        });
        self.inner.state.lock().unwrap().transfers.push(transfer.clone());
        let (inner, t) = (self.inner.clone(), transfer.clone());
        let spawned = std::thread::Builder::new().name("soulseek-peer".into()).spawn(move || {
            if let Err(e) = queue_download(&inner, &t) {
                t.fail(format!("{e:#}"));
            }
        });
        if spawned.is_err() {
            transfer.fail("Couldn't start the download");
        }
        transfer
    }
}

impl Inner {
    fn send(&self, bytes: &[u8]) -> Result<()> {
        if self.closed.load(Ordering::Relaxed) {
            bail!("Disconnected from Soulseek");
        }
        self.server.lock().unwrap().write_all(bytes).context("Lost the connection to Soulseek")
    }

    fn transfer_for(&self, user: &str, filename: &[u8]) -> Option<Arc<Transfer>> {
        let state = self.state.lock().unwrap();
        state.transfers.iter().find(|t| t.user == user && t.filename == filename && !t.finished()).cloned()
    }

    fn wants_peer(&self, user: &str) -> bool {
        let state = self.state.lock().unwrap();
        state.search.is_some_and(|(_, at)| at.elapsed() < SEARCH_WINDOW)
            || state.transfers.iter().any(|t| t.user == user && !t.finished())
    }
}

/// The server address (debug builds can point at a test server).
fn server_addr() -> Result<SocketAddr> {
    #[cfg(debug_assertions)]
    let host = std::env::var("LOCAL_PLAYLIST_SOULSEEK_SERVER").unwrap_or_else(|_| SERVER.into());
    #[cfg(not(debug_assertions))]
    let host = SERVER.to_string();
    host.to_socket_addrs()
        .with_context(|| "Couldn't look up the Soulseek server")?
        .next()
        .context("Couldn't look up the Soulseek server")
}

fn server_loop(inner: Arc<Inner>, mut stream: TcpStream) {
    loop {
        let (code, body) = match read_message(&mut stream, MAX_SERVER_MESSAGE) {
            Ok(m) => m,
            Err(e) => {
                if !inner.closed.swap(true, Ordering::Relaxed) {
                    eprintln!("soulseek: server connection ended: {e:#}");
                }
                for t in inner.state.lock().unwrap().transfers.drain(..) {
                    if matches!(t.state(), TransferState::Connecting | TransferState::Queued(_)) {
                        t.fail("Disconnected from Soulseek");
                    }
                }
                return;
            }
        };
        let mut r = Reader::new(&body);
        match code {
            GET_PEER_ADDRESS => {
                let (Ok(user), Ok(ip), Ok(port)) = (r.string(), r.u32(), r.u32()) else { continue };
                let addr = SocketAddrV4::new(Ipv4Addr::from(ip), port as u16);
                let mut state = inner.state.lock().unwrap();
                for (_, tx) in state.addr_waiters.iter().filter(|(u, _)| *u == user) {
                    let _ = tx.send(addr);
                }
                state.addr_waiters.retain(|(u, _)| *u != user);
            }
            CONNECT_TO_PEER => {
                let (Ok(user), Ok(kind), Ok(ip), Ok(port), Ok(token)) =
                    (r.string(), r.string(), r.u32(), r.u32(), r.u32())
                else {
                    continue;
                };
                let wanted = match kind.as_str() {
                    "P" => inner.wants_peer(&user),
                    "F" => inner.state.lock().unwrap().transfers.iter().any(|t| t.user == user && !t.finished()),
                    _ => false,
                };
                if !wanted || inner.peer_threads.load(Ordering::Relaxed) >= MAX_PEER_THREADS {
                    continue;
                }
                let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(ip), port as u16));
                inner.peer_threads.fetch_add(1, Ordering::Relaxed);
                let spawned = std::thread::Builder::new().name("soulseek-peer".into()).spawn({
                    let inner = inner.clone();
                    move || {
                        let _ = pierce(&inner, addr, token, &user, &kind);
                        inner.peer_threads.fetch_sub(1, Ordering::Relaxed);
                    }
                });
                if spawned.is_err() {
                    inner.peer_threads.fetch_sub(1, Ordering::Relaxed);
                }
            }
            RELOGGED => {
                eprintln!("soulseek: logged in from somewhere else");
                inner.closed.store(true, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

/// Connect to a peer that asked (through the server) to be connected to.
fn pierce(inner: &Arc<Inner>, addr: SocketAddr, token: u32, user: &str, kind: &str) -> Result<()> {
    let mut stream = connect(addr)?;
    let mut w = Writer::default();
    w.u32(token);
    stream.write_all(&w.init_frame(PIERCE_FIREWALL))?;
    match kind {
        "P" => peer_loop(inner, stream, user),
        _ => file_connection(inner, stream, user),
    }
}

fn connect(addr: SocketAddr) -> Result<TcpStream> {
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(12))?;
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

/// Messages on a peer ("P") connection, until it closes or goes quiet.
fn peer_loop(inner: &Arc<Inner>, mut stream: TcpStream, user: &str) -> Result<()> {
    while !inner.closed.load(Ordering::Relaxed) {
        let (code, body) = read_message(&mut stream, MAX_PEER_MESSAGE)?;
        let mut r = Reader::new(&body);
        match code {
            FILE_SEARCH_RESPONSE => {
                let Ok((token, results)) = parse_search_response(&body) else { continue };
                let mut state = inner.state.lock().unwrap();
                if state.search.is_some_and(|(t, _)| t == token) && !results.is_empty() {
                    state.responders += 1;
                    for result in results {
                        if state.results.len() >= MAX_RESULTS {
                            break;
                        }
                        if !state.results.iter().any(|o| o.user == result.user && o.filename == result.filename) {
                            state.results.push(result);
                        }
                    }
                }
            }
            TRANSFER_REQUEST => {
                let direction = r.u32()?;
                let token = r.u32()?;
                let filename = r.bytes()?;
                let size = if direction == 1 { r.u64()? } else { 0 };
                let mut w = Writer::default();
                w.u32(token);
                match inner.transfer_for(user, &filename).filter(|_| direction == 1) {
                    Some(t) => {
                        *t.offer.lock().unwrap() = Some((token, size));
                        t.set(TransferState::Downloading(0, size));
                        w.u8(1);
                    }
                    None => {
                        w.u8(0).str(b"Cancelled");
                    }
                }
                stream.write_all(&w.frame(TRANSFER_RESPONSE))?;
            }
            PLACE_IN_QUEUE_RESPONSE => {
                let filename = r.bytes()?;
                let place = r.u32()?;
                if let Some(t) = inner.transfer_for(user, &filename)
                    && matches!(t.state(), TransferState::Connecting | TransferState::Queued(_))
                {
                    t.set(TransferState::Queued(Some(place)));
                }
            }
            UPLOAD_FAILED => {
                if let Some(t) = inner.transfer_for(user, &r.bytes()?) {
                    t.fail(format!("{user} couldn't send the file"));
                }
            }
            UPLOAD_DENIED => {
                let filename = r.bytes()?;
                let reason = r.string().unwrap_or_default();
                if let Some(t) = inner.transfer_for(user, &filename) {
                    t.fail(denied_reason(user, &reason));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn denied_reason(user: &str, reason: &str) -> String {
    match reason {
        "Banned" => format!("{user} doesn't share with you (banned)"),
        "File not shared." | "File not shared" => format!("{user} no longer shares that file"),
        "Too many files" | "Too many megabytes" => format!("{user}'s queue is full"),
        "" => format!("{user} declined"),
        other => format!("{user} declined: {other}"),
    }
}

/// Ask the peer for the file: connect to it and queue the download. The
/// rest arrives on connections the peer starts ([`peer_loop`], [`file_connection`]).
fn queue_download(inner: &Arc<Inner>, t: &Arc<Transfer>) -> Result<()> {
    let addr = peer_address(inner, &t.user)?;
    let mut stream = connect(SocketAddr::V4(addr)).with_context(|| format!("Couldn't connect to {}", t.user))?;
    let mut w = Writer::default();
    w.str(inner.username.as_bytes()).str(b"P").u32(random_u32());
    stream.write_all(&w.init_frame(PEER_INIT))?;
    let mut w = Writer::default();
    w.str(&t.filename);
    stream.write_all(&w.frame(QUEUE_UPLOAD))?;
    stream.write_all(&w.frame(PLACE_IN_QUEUE_REQUEST))?;
    t.set(TransferState::Queued(None));
    // Queued downloads can wait a long time; the connection may close
    // meanwhile without that being an error.
    stream.set_read_timeout(Some(Duration::from_secs(600)))?;
    let _ = peer_loop(inner, stream, &t.user);
    Ok(())
}

fn peer_address(inner: &Inner, user: &str) -> Result<SocketAddrV4> {
    let (tx, rx) = mpsc::channel();
    inner.state.lock().unwrap().addr_waiters.push((user.to_string(), tx));
    let mut w = Writer::default();
    w.str(user.as_bytes());
    inner.send(&w.frame(GET_PEER_ADDRESS))?;
    let addr =
        rx.recv_timeout(Duration::from_secs(20)).with_context(|| format!("Soulseek doesn't know where {user} is"))?;
    if addr.ip().is_unspecified() || addr.port() == 0 {
        bail!("{user} is offline");
    }
    Ok(addr)
}

/// A file ("F") connection: the uploader names the transfer, we say where to
/// start, the file follows.
fn file_connection(inner: &Arc<Inner>, mut stream: TcpStream, user: &str) -> Result<()> {
    let mut token = [0u8; 4];
    stream.read_exact(&mut token)?;
    let token = u32::from_le_bytes(token);
    let transfer = {
        let state = inner.state.lock().unwrap();
        state
            .transfers
            .iter()
            .find(|t| t.user == user && !t.finished() && t.offer.lock().unwrap().is_some_and(|(k, _)| k == token))
            .cloned()
    };
    let Some(t) = transfer else { return Ok(()) };
    let size = t.offer.lock().unwrap().map(|(_, s)| s).unwrap_or(0);
    let result = receive_file(&mut stream, &t, size);
    match result {
        Ok(()) => t.set(TransferState::Done),
        Err(e) => {
            let _ = std::fs::remove_file(t.part_file());
            t.fail(format!("{e:#}"));
        }
    }
    inner.state.lock().unwrap().transfers.retain(|o| !Arc::ptr_eq(o, &t));
    Ok(())
}

fn receive_file(stream: &mut TcpStream, t: &Transfer, size: u64) -> Result<()> {
    stream.write_all(&0u64.to_le_bytes())?;
    let part = t.part_file();
    let mut file = std::fs::File::create(&part).with_context(|| format!("create {}", part.display()))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    t.set(TransferState::Downloading(0, size));
    while done < size {
        if t.cancel.load(Ordering::Relaxed) {
            bail!("Cancelled");
        }
        let want = buf.len().min((size - done) as usize);
        let n = stream.read(&mut buf[..want])?;
        if n == 0 {
            bail!("{} stopped sending at {}%", t.user, done * 100 / size.max(1));
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        t.set(TransferState::Downloading(done, size));
    }
    file.sync_all()?;
    drop(file);
    std::fs::rename(&part, &t.dest).with_context(|| format!("rename {}", part.display()))?;
    Ok(())
}

/// Parse a (zlib-compressed) `FileSearchResponse`: (search token, playable files).
fn parse_search_response(body: &[u8]) -> Result<(u32, Vec<SearchResult>)> {
    let mut inflated = Vec::new();
    flate2::read::ZlibDecoder::new(body).take(MAX_INFLATED).read_to_end(&mut inflated)?;
    let mut r = Reader::new(&inflated);
    let user = r.string()?;
    let token = r.u32()?;
    let count = r.u32()? as usize;
    let mut files = Vec::with_capacity(count.min(1000));
    for _ in 0..count {
        let _code = r.u8()?;
        let filename = r.bytes()?;
        let size = r.u64()?;
        let _ext = r.bytes()?;
        let attrs = r.u32()?;
        let (mut bitrate, mut duration_secs, mut sample_rate, mut bit_depth) = (None, None, None, None);
        for _ in 0..attrs.min(64) {
            let (code, value) = (r.u32()?, r.u32()?);
            match code {
                0 => bitrate = Some(value),
                1 => duration_secs = Some(value),
                4 => sample_rate = Some(value),
                5 => bit_depth = Some(value),
                _ => {}
            }
        }
        files.push((filename, size, bitrate, duration_secs, sample_rate, bit_depth));
    }
    // Older clients stop early; missing fields keep their defaults.
    let free_slot = r.u8().map(|b| b != 0).unwrap_or(true);
    let speed = r.u32().unwrap_or(0);
    let queue = r.u32().unwrap_or(0);
    let results = files
        .into_iter()
        .filter_map(|(filename, size, bitrate, duration_secs, sample_rate, bit_depth)| {
            let name = String::from_utf8_lossy(&filename);
            let ext = Path::new(name.rsplit(['\\', '/']).next()?).extension()?.to_str()?.to_ascii_lowercase();
            PLAYABLE.contains(&ext.as_str()).then(|| SearchResult {
                user: user.clone(),
                filename,
                size,
                ext,
                bitrate,
                duration_secs,
                sample_rate,
                bit_depth,
                free_slot,
                speed,
                queue,
            })
        })
        .collect();
    Ok((token, results))
}

// ------------------------------------------------------------------ wire

fn read_message(stream: &mut impl Read, max: usize) -> Result<(u32, Vec<u8>)> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if !(4..=max).contains(&len) {
        bail!("bad message length {len}");
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    let code = u32::from_le_bytes(buf[..4].try_into().unwrap());
    buf.drain(..4);
    Ok((code, buf))
}

#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    #[cfg(test)]
    fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn str(&mut self, v: &[u8]) -> &mut Self {
        self.u32(v.len() as u32);
        self.0.extend_from_slice(v);
        self
    }
    /// As a server or peer message.
    fn frame(&self, code: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.0.len() + 8);
        out.extend_from_slice(&(self.0.len() as u32 + 4).to_le_bytes());
        out.extend_from_slice(&code.to_le_bytes());
        out.extend_from_slice(&self.0);
        out
    }
    /// As the first message on a peer connection.
    fn init_frame(&self, code: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.0.len() + 5);
        out.extend_from_slice(&(self.0.len() as u32 + 1).to_le_bytes());
        out.push(code);
        out.extend_from_slice(&self.0);
        out
    }
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.buf.len() < n {
            bail!("message too short");
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn bytes(&mut self) -> Result<Vec<u8>> {
        let len = self.u32()? as usize;
        Ok(self.take(len)?.to_vec())
    }
    fn string(&mut self) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.bytes()?).into_owned())
    }
}

fn md5_hex(s: &str) -> String {
    use md5::Digest as _;
    md5::Md5::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn random_u32() -> u32 {
    let mut bytes = [0u8; 4];
    let _ = getrandom::fill(&mut bytes);
    u32::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::TcpListener;

    fn by_user(results: &[SearchResult]) -> HashMap<String, usize> {
        let mut map = HashMap::new();
        for r in results {
            *map.entry(r.user.clone()).or_insert(0) += 1;
        }
        map
    }

    fn result(ext: &str, bitrate: Option<u32>, bits: Option<u32>, rate: Option<u32>, free: bool) -> SearchResult {
        SearchResult {
            user: "u".into(),
            filename: format!("Music\\Artist - Album\\01 Song.{ext}").into_bytes(),
            size: 8_000_000,
            ext: ext.into(),
            bitrate,
            duration_secs: Some(200),
            sample_rate: rate,
            bit_depth: bits,
            free_slot: free,
            speed: 100_000,
            queue: 0,
        }
    }

    #[test]
    fn sorts_best_first() {
        let mut v = vec![
            result("mp3", Some(128), None, None, true),
            result("flac", None, Some(16), Some(44_100), true),
            result("mp3", Some(320), None, None, false),
            result("flac", None, Some(24), Some(96_000), true),
            result("mp3", Some(320), None, None, true),
            result("ogg", None, None, None, true),
        ];
        sort_by_quality(&mut v);
        let labels: Vec<_> = v.iter().map(|r| (r.quality_label(), r.free_slot)).collect();
        let expected = [
            ("FLAC 24/96", true),
            ("FLAC 16/44.1", true),
            ("MP3 320", true),
            // No bitrate given: 8 MB over 200 s.
            ("OGG 320", true),
            ("MP3 320", false),
            ("MP3 128", true),
        ];
        assert_eq!(labels, expected.map(|(l, f)| (l.to_string(), f)));
        assert_eq!(v[0].file_name(), "01 Song.flac");
        assert_eq!(v[0].folder(), "Artist - Album");
    }

    #[test]
    fn builds_queries() {
        assert_eq!(
            query_for("Rick Astley", "Rick Astley - Never Gonna Give You Up (Official Video)"),
            "Rick Astley - Never Gonna Give You Up"
        );
        assert_eq!(search_terms("Rick Astley - Never Gonna Give You Up"), "rick astley never gonna give you up");
        assert_eq!(search_terms("AC/DC - Don't Stop (-live-)"), "ac dc don't stop live");
    }

    #[test]
    fn parses_search_responses() {
        let mut w = Writer::default();
        w.str(b"peer").u32(7).u32(3);
        for (name, attrs) in [
            ("a\\b\\song.flac", vec![(1u32, 210u32), (4, 44_100), (5, 16)]),
            ("a\\b\\cover.jpg", vec![]),
            ("a\\b\\song.mp3", vec![(0, 320), (1, 210)]),
        ] {
            w.u8(1).str(name.as_bytes()).u64(1000).str(b"").u32(attrs.len() as u32);
            for (c, v) in attrs {
                w.u32(c).u32(v);
            }
        }
        w.u8(1).u32(5000).u32(2);
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&w.0).unwrap();
        let (token, results) = parse_search_response(&z.finish().unwrap()).unwrap();
        assert_eq!(token, 7);
        assert_eq!(results.len(), 2, "the cover isn't playable");
        assert_eq!(results[0].quality_label(), "FLAC 16/44.1");
        assert_eq!((results[1].kbps(), results[1].queue, results[1].free_slot), (Some(320), 2, true));
        assert!(Reader::new(&[1, 2]).u32().is_err());
    }

    #[test]
    fn md5_matches() {
        assert_eq!(md5_hex(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    // ------------------------------------------------------- fake network

    /// A Soulseek server and one peer ("alice", reachable, sharing one file)
    /// on localhost: enough to run login, search and download end to end.
    struct FakeNetwork {
        server: SocketAddr,
        file: Vec<u8>,
    }

    fn send(stream: &mut TcpStream, code: u32, w: &Writer) {
        stream.write_all(&w.frame(code)).unwrap();
    }

    /// `password` is the only one the fake server accepts.
    fn fake_network(password: String) -> FakeNetwork {
        let file: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let peer = TcpListener::bind("127.0.0.1:0").unwrap();
        let (server_addr, peer_port) = (server.local_addr().unwrap(), peer.local_addr().unwrap().port());
        // The client's server connection, for the peer to "ask the server" through.
        let client_conn: Arc<Mutex<Option<TcpStream>>> = Arc::default();

        let conn = client_conn.clone();
        std::thread::spawn(move || {
            // One client connection at a time (a failed login closes its own).
            for s in server.incoming() {
                let mut s = s.unwrap();
                *conn.lock().unwrap() = Some(s.try_clone().unwrap());
                loop {
                    let Ok((code, body)) = read_message(&mut s, MAX_SERVER_MESSAGE) else { break };
                    let mut r = Reader::new(&body);
                    match code {
                        LOGIN => {
                            let (user, pass) = (r.string().unwrap(), r.string().unwrap());
                            let mut w = Writer::default();
                            if pass == password {
                                w.u8(1).str(b"hi").u32(0x7f00_0001).str(b"x").u8(0);
                            } else {
                                w.u8(0).str(b"INVALIDPASS");
                            }
                            let _ = user;
                            send(&mut s, LOGIN, &w);
                        }
                        FILE_SEARCH => {
                            // Alice can't reach us: the server relays her request.
                            let token = r.u32().unwrap();
                            assert_eq!(r.string().unwrap(), "artist song");
                            PEER_SEARCH_TOKEN.store(token as usize, Ordering::Relaxed);
                            let mut w = Writer::default();
                            w.str(b"alice").str(b"P").u32(0x7f00_0001).u32(peer_port as u32).u32(111).u8(0);
                            send(&mut s, CONNECT_TO_PEER, &w);
                        }
                        GET_PEER_ADDRESS => {
                            let user = r.string().unwrap();
                            let mut w = Writer::default();
                            w.str(user.as_bytes()).u32(0x7f00_0001).u32(peer_port as u32);
                            send(&mut s, GET_PEER_ADDRESS, &w);
                        }
                        _ => {}
                    }
                }
            }
        });

        let shared = file.clone();
        std::thread::spawn(move || {
            for stream in peer.incoming() {
                let mut s = stream.unwrap();
                let (conn, file) = (client_conn.clone(), shared.clone());
                std::thread::spawn(move || {
                    let mut len = [0u8; 4];
                    s.read_exact(&mut len).unwrap();
                    let mut init = vec![0u8; u32::from_le_bytes(len) as usize];
                    s.read_exact(&mut init).unwrap();
                    match init[0] {
                        PIERCE_FIREWALL => {
                            let token = u32::from_le_bytes(init[1..5].try_into().unwrap());
                            if token == 111 {
                                // Search results.
                                let mut w = Writer::default();
                                let token = PEER_SEARCH_TOKEN.load(Ordering::Relaxed) as u32;
                                w.str(b"alice").u32(token).u32(2);
                                for (name, attrs) in [
                                    ("Music\\Artist\\Song.mp3", vec![(0u32, 192u32), (1, 180)]),
                                    ("Music\\Artist\\Song.flac", vec![(1, 180), (4, 44_100), (5, 16)]),
                                ] {
                                    w.u8(1)
                                        .str(name.as_bytes())
                                        .u64(file.len() as u64)
                                        .str(b"")
                                        .u32(attrs.len() as u32);
                                    for (c, v) in attrs {
                                        w.u32(c).u32(v);
                                    }
                                }
                                w.u8(1).u32(1_000_000).u32(0);
                                let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                                z.write_all(&w.0).unwrap();
                                let mut out = Writer::default();
                                out.0 = z.finish().unwrap();
                                send(&mut s, FILE_SEARCH_RESPONSE, &out);
                            } else if token == 222 {
                                // File connection: name the transfer, read the offset, send.
                                s.write_all(&333u32.to_le_bytes()).unwrap();
                                let mut offset = [0u8; 8];
                                s.read_exact(&mut offset).unwrap();
                                assert_eq!(u64::from_le_bytes(offset), 0);
                                s.write_all(&file).unwrap();
                            }
                        }
                        PEER_INIT => {
                            // The downloader queues a file; offer it, then
                            // connect for the data through the server.
                            loop {
                                let Ok((code, body)) = read_message(&mut s, MAX_PEER_MESSAGE) else { return };
                                let mut r = Reader::new(&body);
                                match code {
                                    QUEUE_UPLOAD => {
                                        let name = r.bytes().unwrap();
                                        let mut w = Writer::default();
                                        w.u32(1).u32(333).str(&name).u64(file.len() as u64);
                                        send(&mut s, TRANSFER_REQUEST, &w);
                                    }
                                    TRANSFER_RESPONSE => {
                                        assert_eq!((r.u32().unwrap(), r.u8().unwrap()), (333, 1));
                                        let mut w = Writer::default();
                                        w.str(b"alice").str(b"F").u32(0x7f00_0001).u32(peer_port as u32).u32(222).u8(0);
                                        let mut c = conn.lock().unwrap();
                                        send(c.as_mut().unwrap(), CONNECT_TO_PEER, &w);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                });
            }
        });
        FakeNetwork { server: server_addr, file }
    }

    static PEER_SEARCH_TOKEN: AtomicUsize = AtomicUsize::new(0);

    fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
        let start = Instant::now();
        while !f() {
            assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn logs_in_searches_and_downloads() {
        // Made up per run: no password is written in the code.
        let password = random_u32().to_string();
        let net = fake_network(password.clone());
        // SAFETY: the only test that reads this variable.
        unsafe { std::env::set_var("LOCAL_PLAYLIST_SOULSEEK_SERVER", net.server.to_string()) };

        let wrong = format!("{password}x");
        assert!(Client::login("me", &wrong).is_err_and(|e| e.to_string().contains("Wrong password")));
        let client = Client::login("me", &password).unwrap();
        client.search("Artist - Song").unwrap();
        wait_until("results", || client.result_count().0 == 2);
        let results = client.results();
        assert_eq!(results[0].quality_label(), "FLAC 16/44.1", "best first");
        assert_eq!(by_user(&results)["alice"], 2);

        let dir = std::env::temp_dir().join(format!("lp-slsk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = client.download(&results[0], dir.join("song.flac"));
        wait_until("the download", || t.finished());
        assert_eq!(t.state(), TransferState::Done);
        assert_eq!(std::fs::read(dir.join("song.flac")).unwrap(), net.file);
        let _ = std::fs::remove_dir_all(dir);
    }
}
