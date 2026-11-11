//! Audio playback with streaming support.
//!
//! A decoder thread reads the audio file (possibly one yt-dlp is still
//! writing) and pushes decoded sample chunks through a bounded channel to an
//! output source attached to rodio. The output source never blocks the audio
//! device: when it runs out of samples it plays silence and reports
//! "buffering" until enough audio has been decoded again.
//!
//! Seeking: on a complete file the decoder seeks directly. While the file is
//! still downloading, a forward seek decodes ahead (waiting for data if
//! needed) and a backward seek reopens the partial file and decodes forward.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use rodio::{Decoder, MixerDeviceSink, Player, Source};

use crate::download::{DlState, Download};

/// Samples per chunk sent from the decoder thread.
const CHUNK: usize = 4096;
/// Chunks the channel can hold (~12 s of 44.1 kHz stereo).
const QUEUE: usize = 256;
/// Seconds of audio to buffer before starting a stream / after an underrun.
pub const PREBUFFER_SECS: f32 = 3.0;
const REBUFFER_SECS: f32 = 1.5;
/// For complete local files.
const LOCAL_PREBUFFER_SECS: f32 = 0.15;

pub enum Input {
    File(PathBuf),
    Stream(Arc<Download>),
}

struct Chunk {
    generation: u64,
    samples: Vec<f32>,
}

enum Cmd {
    Seek(Duration),
}

#[derive(Default)]
struct Shared {
    /// Frames played (playback position).
    frames: AtomicU64,
    rate: AtomicU64,
    channels: AtomicU64,
    /// Samples waiting in the channel.
    queued: AtomicUsize,
    buffering: AtomicBool,
    /// Decoder reached the end of the (complete) input.
    decoder_done: AtomicBool,
    /// Input is still being downloaded.
    streaming: AtomicBool,
    generation: AtomicU64,
    /// Set when a command is pending, to break out of blocking reads.
    interrupt: AtomicBool,
    /// A read was aborted because of `interrupt`: the decoder must be reopened.
    read_aborted: AtomicBool,
    duration_ms: AtomicU64,
    error: Mutex<Option<String>>,
}

impl Shared {
    fn secs_queued(&self) -> f32 {
        let rate = self.rate.load(Ordering::Relaxed).max(1) as f32;
        let ch = self.channels.load(Ordering::Relaxed).max(1) as f32;
        self.queued.load(Ordering::Relaxed) as f32 / rate / ch
    }
}

struct Stream {
    shared: Arc<Shared>,
    ctrl: mpsc::Sender<Cmd>,
    /// Ready signal: (sample rate, channels) or an error.
    ready: Option<Receiver<Result<(u32, u16), String>>>,
    rx: Option<Receiver<Chunk>>,
}

pub struct Audio {
    // Must be kept alive for as long as audio should play.
    _sink: MixerDeviceSink,
    player: Player,
    stream: Option<Stream>,
}

impl Audio {
    pub fn new(volume: f32) -> Result<Self> {
        let mut sink = rodio::DeviceSinkBuilder::open_default_sink()
            .context("could not open the default audio output device")?;
        sink.log_on_drop(false);
        let player = Player::connect_new(sink.mixer());
        player.set_volume(volume);
        Ok(Self { _sink: sink, player, stream: None })
    }

    /// Start playing `input`, replacing whatever was playing. Opening happens
    /// on a background thread; call [`Audio::poll`] regularly.
    pub fn load(&mut self, input: Input) {
        self.stop();
        let shared = Arc::new(Shared::default());
        shared.streaming.store(matches!(&input, Input::Stream(d) if matches!(d.state(), DlState::Running)), Ordering::Relaxed);
        shared.buffering.store(true, Ordering::Relaxed);
        let (ctrl_tx, ctrl_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let sh = shared.clone();
        std::thread::Builder::new()
            .name("decoder".into())
            .spawn(move || decoder_thread(input, sh, ctrl_rx, tx, ready_tx))
            .expect("spawn decoder thread");
        self.stream = Some(Stream { shared, ctrl: ctrl_tx, ready: Some(ready_rx), rx: Some(rx) });
        self.player.play();
    }

    /// Hook the decoded stream to the output once the decoder is ready.
    /// Returns an error message if opening failed.
    pub fn poll(&mut self) -> Option<String> {
        let stream = self.stream.as_mut()?;
        if let Some(ready) = &stream.ready {
            match ready.try_recv() {
                Ok(Ok((rate, channels))) => {
                    stream.ready = None;
                    stream.shared.rate.store(rate as u64, Ordering::Relaxed);
                    stream.shared.channels.store(channels as u64, Ordering::Relaxed);
                    let source = OutputSource {
                        rx: stream.rx.take().unwrap(),
                        shared: stream.shared.clone(),
                        rate: NonZero::new(rate).unwrap_or(NonZero::new(44_100).unwrap()),
                        channels: NonZero::new(channels).unwrap_or(NonZero::new(2).unwrap()),
                        cur: Vec::new(),
                        idx: 0,
                        silence: false,
                        prebuffering: true,
                        threshold: None,
                    };
                    self.player.append(source);
                }
                Ok(Err(e)) => {
                    self.stream = None;
                    return Some(e);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.stream = None;
                    return Some("audio decoder stopped unexpectedly".into());
                }
            }
        }
        let stream = self.stream.as_ref()?;
        stream.shared.error.lock().unwrap().take()
    }

    pub fn stop(&mut self) {
        self.player.clear();
        if let Some(s) = self.stream.take() {
            s.shared.interrupt.store(true, Ordering::Relaxed);
            // Dropping the control sender ends the decoder thread.
        }
    }

    pub fn play(&self) {
        self.player.play();
    }

    pub fn pause(&self) {
        self.player.pause();
    }

    pub fn is_paused(&self) -> bool {
        self.player.is_paused()
    }

    pub fn is_loaded(&self) -> bool {
        self.stream.is_some()
    }

    /// True while waiting for audio data (initial buffering or an underrun).
    pub fn is_buffering(&self) -> bool {
        self.stream.as_ref().is_some_and(|s| s.shared.buffering.load(Ordering::Relaxed))
    }

    pub fn seek(&self, pos: Duration) {
        if let Some(s) = &self.stream {
            let rate = s.shared.rate.load(Ordering::Relaxed);
            s.shared.generation.fetch_add(1, Ordering::SeqCst);
            s.shared.frames.store((pos.as_secs_f64() * rate as f64) as u64, Ordering::Relaxed);
            s.shared.buffering.store(true, Ordering::Relaxed);
            s.shared.interrupt.store(true, Ordering::SeqCst);
            let _ = s.ctrl.send(Cmd::Seek(pos));
        }
    }

    pub fn position(&self) -> Duration {
        let Some(s) = &self.stream else { return Duration::ZERO };
        let rate = s.shared.rate.load(Ordering::Relaxed);
        if rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(s.shared.frames.load(Ordering::Relaxed) as f64 / rate as f64)
    }

    pub fn duration(&self) -> Option<Duration> {
        let ms = self.stream.as_ref()?.shared.duration_ms.load(Ordering::Relaxed);
        (ms > 0).then(|| Duration::from_millis(ms))
    }

    pub fn set_volume(&self, volume: f32) {
        self.player.set_volume(volume);
    }

    /// True once a loaded track has played to the end.
    pub fn finished(&self) -> bool {
        self.stream.as_ref().is_some_and(|s| s.ready.is_none()) && self.player.empty()
    }
}

// ------------------------------------------------------------------ output

struct OutputSource {
    rx: Receiver<Chunk>,
    shared: Arc<Shared>,
    rate: rodio::SampleRate,
    channels: rodio::ChannelCount,
    cur: Vec<f32>,
    idx: usize,
    /// `cur` is generated silence (doesn't advance the position).
    silence: bool,
    prebuffering: bool,
    threshold: Option<f32>,
}

impl OutputSource {
    fn fill_silence(&mut self) {
        // 10 ms of silence, whole frames only.
        let frames = (self.rate.get() / 100) as usize;
        self.cur = vec![0.0; frames * self.channels.get() as usize];
        self.idx = 0;
        self.silence = true;
        self.shared.buffering.store(true, Ordering::Relaxed);
    }

    fn threshold(&self) -> f32 {
        self.threshold.unwrap_or(if self.shared.streaming.load(Ordering::Relaxed) {
            PREBUFFER_SECS
        } else {
            LOCAL_PREBUFFER_SECS
        })
    }
}

impl Iterator for OutputSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        loop {
            if self.idx < self.cur.len() {
                let s = self.cur[self.idx];
                self.idx += 1;
                return Some(s);
            }
            let done = self.shared.decoder_done.load(Ordering::Relaxed);
            if self.prebuffering {
                if done || self.shared.secs_queued() >= self.threshold() {
                    self.prebuffering = false;
                } else {
                    self.fill_silence();
                    continue;
                }
            }
            match self.rx.try_recv() {
                Ok(chunk) => {
                    self.shared.queued.fetch_sub(chunk.samples.len(), Ordering::Relaxed);
                    if chunk.generation != self.shared.generation.load(Ordering::SeqCst) {
                        // Stale audio from before a seek: drop it and buffer briefly.
                        self.prebuffering = true;
                        self.threshold = Some(LOCAL_PREBUFFER_SECS);
                        continue;
                    }
                    let frames = chunk.samples.len() / self.channels.get() as usize;
                    self.shared.frames.fetch_add(frames as u64, Ordering::Relaxed);
                    self.shared.buffering.store(false, Ordering::Relaxed);
                    self.cur = chunk.samples;
                    self.idx = 0;
                    self.silence = false;
                }
                Err(TryRecvError::Empty) if done => return None,
                Err(TryRecvError::Empty) => {
                    // Underrun: wait for a bit more audio before resuming.
                    self.prebuffering = true;
                    self.threshold = Some(if self.shared.streaming.load(Ordering::Relaxed) {
                        REBUFFER_SECS
                    } else {
                        LOCAL_PREBUFFER_SECS
                    });
                    self.fill_silence();
                }
                Err(TryRecvError::Disconnected) => return None,
            }
        }
    }
}

impl Source for OutputSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.channels
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

// ----------------------------------------------------------------- decoder

/// A file that may still be growing: reads wait for more data until the
/// download finishes.
struct GrowingFile {
    file: File,
    dl: Arc<Download>,
    shared: Arc<Shared>,
}

impl Read for GrowingFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let n = self.file.read(buf)?;
            if n > 0 {
                return Ok(n);
            }
            if self.shared.interrupt.load(Ordering::SeqCst) {
                self.shared.read_aborted.store(true, Ordering::SeqCst);
                return Err(io::Error::other("interrupted"));
            }
            match self.dl.state() {
                DlState::Running => std::thread::sleep(Duration::from_millis(50)),
                // Data written just before completion may have arrived meanwhile.
                DlState::Done(_) => return self.file.read(buf),
                DlState::Failed(e) => return Err(io::Error::other(e)),
            }
        }
    }
}

impl Seek for GrowingFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match pos {
            SeekFrom::End(_) if matches!(self.dl.state(), DlState::Running) => {
                Err(io::Error::new(io::ErrorKind::Unsupported, "file is still downloading"))
            }
            p => self.file.seek(p),
        }
    }
}

enum Reader {
    File(BufReader<File>),
    Growing(BufReader<GrowingFile>),
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Reader::File(r) => r.read(buf),
            Reader::Growing(r) => r.read(buf),
        }
    }
}

impl Seek for Reader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            Reader::File(r) => r.seek(pos),
            Reader::Growing(r) => r.seek(pos),
        }
    }
}

fn hint(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("m4a").to_string()
}

fn open_complete(path: &Path) -> Result<Decoder<Reader>> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let len = file.metadata()?.len();
    Decoder::builder()
        .with_data(Reader::File(BufReader::new(file)))
        .with_byte_len(len)
        .with_seekable(true)
        .with_hint(&hint(path))
        .build()
        .context("unsupported or corrupt audio file")
}

/// Open the input. For a running download, waits for the file to appear and
/// opens it as a growing stream; falls back to waiting for completion if the
/// partial file can't be decoded progressively.
fn open(input: &Input, shared: &Arc<Shared>) -> Result<(Decoder<Reader>, bool)> {
    let dl = match input {
        Input::File(p) => return Ok((open_complete(p)?, true)),
        Input::Stream(dl) => dl,
    };
    loop {
        if shared.interrupt.load(Ordering::SeqCst) {
            bail!("cancelled");
        }
        match dl.state() {
            DlState::Done(p) => return Ok((open_complete(&p)?, true)),
            DlState::Failed(e) => bail!("{e}"),
            DlState::Running => {}
        }
        if let Some(partial) = dl.partial_file()
            && std::fs::metadata(&partial).is_ok_and(|m| m.len() > 0)
        {
            let file = File::open(&partial)?;
            let reader = GrowingFile { file, dl: dl.clone(), shared: shared.clone() };
            match Decoder::builder()
                .with_data(Reader::Growing(BufReader::new(reader)))
                .with_seekable(false)
                .with_hint(&hint(&partial))
                .build()
            {
                Ok(d) => return Ok((d, false)),
                Err(e) => {
                    if shared.interrupt.load(Ordering::SeqCst) {
                        bail!("cancelled");
                    }
                    // Not progressively decodable (e.g. needs post-processing):
                    // wait for the finished file.
                    eprintln!("progressive decode not possible ({e}); waiting for download");
                    loop {
                        match dl.state() {
                            DlState::Done(p) => return Ok((open_complete(&p)?, true)),
                            DlState::Failed(e) => bail!("{e}"),
                            DlState::Running => std::thread::sleep(Duration::from_millis(100)),
                        }
                        if shared.interrupt.load(Ordering::SeqCst) {
                            bail!("cancelled");
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn decoder_thread(
    input: Input,
    shared: Arc<Shared>,
    ctrl: Receiver<Cmd>,
    tx: SyncSender<Chunk>,
    ready: mpsc::Sender<Result<(u32, u16), String>>,
) {
    let (mut dec, mut complete) = match open(&input, &shared) {
        Ok(d) => d,
        Err(e) => {
            let _ = ready.send(Err(format!("{e:#}")));
            return;
        }
    };
    let rate = dec.sample_rate().get();
    let channels = dec.channels().get();
    if let Some(d) = dec.total_duration() {
        shared.duration_ms.store(d.as_millis() as u64, Ordering::Relaxed);
    }
    shared.streaming.store(!complete, Ordering::Relaxed);
    if ready.send(Ok((rate, channels))).is_err() {
        return;
    }
    let frame_len = channels as u64;
    // Samples decoded by the current decoder since its start position.
    let mut decoded: u64 = 0;
    let mut start_frames: u64 = 0;
    let mut broken = false;
    let mut pending: Option<Duration> = None;

    loop {
        // Commands (latest seek wins).
        let mut seek_to = pending.take();
        loop {
            match ctrl.try_recv() {
                Ok(Cmd::Seek(t)) => seek_to = Some(t),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if let Some(t) = seek_to {
            shared.interrupt.store(false, Ordering::SeqCst);
            shared.decoder_done.store(false, Ordering::Relaxed);
            let target = (t.as_secs_f64() * rate as f64) as u64;
            let cur = start_frames + decoded / frame_len;
            // Upgrade to the finished file when the download is done.
            if !complete
                && let Input::Stream(dl) = &input
                && let DlState::Done(p) = dl.state()
                && let Ok(d) = open_complete(&p)
            {
                dec = d;
                complete = true;
                broken = true; // position unknown: force a seek below
                shared.streaming.store(false, Ordering::Relaxed);
                if let Some(d) = dec.total_duration() {
                    shared.duration_ms.store(d.as_millis() as u64, Ordering::Relaxed);
                }
            }
            let mut skip = 0u64;
            if complete && dec.try_seek(t).is_ok() {
                // Done.
            } else if !broken && target >= cur {
                skip = (target - cur) * frame_len;
            } else {
                match open(&input, &shared) {
                    Ok((d, c)) => {
                        dec = d;
                        complete = c;
                        if !(complete && dec.try_seek(t).is_ok()) {
                            skip = target * frame_len;
                        }
                    }
                    Err(e) => {
                        *shared.error.lock().unwrap() = Some(format!("{e:#}"));
                        return;
                    }
                }
            }
            // Decode-and-discard up to the target (checks for newer commands).
            while skip > 0 {
                let n = skip.min(65_536);
                let got = dec.by_ref().take(n as usize).count() as u64;
                skip -= got;
                if got < n || shared.interrupt.load(Ordering::SeqCst) {
                    break;
                }
            }
            broken = false;
            start_frames = target;
            decoded = 0;
            if shared.interrupt.load(Ordering::SeqCst) {
                continue;
            }
        }

        let generation = shared.generation.load(Ordering::SeqCst);
        let mut samples = Vec::with_capacity(CHUNK);
        while samples.len() < CHUNK {
            match dec.next() {
                Some(s) => samples.push(s),
                None => break,
            }
        }
        decoded += samples.len() as u64;
        if shared.read_aborted.swap(false, Ordering::SeqCst) {
            // A read was cut short for a pending command; decoder state is unreliable.
            broken = true;
        }
        if shared.interrupt.load(Ordering::SeqCst) {
            // Handle the pending command first; this chunk predates it.
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        if samples.is_empty() {
            shared.decoder_done.store(true, Ordering::Relaxed);
            // Stay alive for seeks (e.g. seeking back after the end).
            match ctrl.recv() {
                Ok(Cmd::Seek(t)) => {
                    pending = Some(t);
                    continue;
                }
                Err(_) => return,
            }
        }
        if generation != shared.generation.load(Ordering::SeqCst) {
            continue; // a seek happened meanwhile; drop this chunk
        }
        let len = samples.len();
        shared.queued.fetch_add(len, Ordering::Relaxed);
        let mut chunk = Chunk { generation, samples };
        loop {
            match tx.try_send(chunk) {
                Ok(()) => break,
                Err(TrySendError::Full(c)) => {
                    if shared.interrupt.load(Ordering::SeqCst) {
                        shared.queued.fetch_sub(len, Ordering::Relaxed);
                        break;
                    }
                    chunk = c;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// Decode a file given by LP_DECODE_FILE to the end (and test seeking).
    #[test]
    #[ignore = "needs LP_DECODE_FILE"]
    fn decode_file() {
        use rodio::Source as _;
        let path = std::env::var("LP_DECODE_FILE").unwrap();
        let mut d = rodio::Decoder::try_from(std::fs::File::open(&path).unwrap()).unwrap();
        println!("duration {:?}", d.total_duration());
        d.try_seek(std::time::Duration::from_secs(60)).unwrap();
        let n = d.count();
        println!("samples after seek: {n}");
        assert!(n > 44100);
    }

    /// Stream tracks while yt-dlp downloads them (volume 0) and exercise seeking.
    #[test]
    #[ignore = "network + audio device; uses components from the ytdlp live test"]
    fn live_stream() {
        use super::*;
        use crate::model::{Paths, Platform, Track};
        use crate::ytdlp::{Component, Source, YtDlp};
        use std::time::Instant;

        let dir = std::env::temp_dir().join("local-playlist-components-test");
        let paths = Paths { data: dir.join("data"), cache: dir.join("cache") };
        let y = YtDlp::new(&paths, Source::Managed, Source::Managed);
        for c in [Component::YtDlp, Component::JsRuntime] {
            if y.installed(c).is_none() {
                y.install(c, &|_, _| {}).unwrap();
            }
        }
        let mut audio = Audio::new(0.0).unwrap();
        for (platform, id, url) in [
            (Platform::SoundCloud, "297", "https://soundcloud.com/forss/journeyman"),
            (Platform::YouTube, "dQw4w9WgXcQ", "https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
        ] {
            let t = Track::new(platform, id, url, "t", "a", None);
            let tmp = dir.join("stream").join(t.file_key());
            let _ = std::fs::remove_dir_all(&tmp);
            let dl = Arc::new(Download::new(tmp.clone(), t.file_key()));
            let (dl2, y2, t2) = (dl.clone(), y.clone(), t.clone());
            let start = Instant::now();
            std::thread::spawn(move || {
                let r = y2.download(&t2, &tmp, None, None, |p| dl2.set_progress(p));
                dl2.finish(r.map_err(|e| format!("{e:#}")));
            });
            audio.load(Input::Stream(dl.clone()));
            let wait = |audio: &mut Audio, what: &str, cond: &dyn Fn(&Audio) -> bool| {
                let t0 = Instant::now();
                while !cond(audio) {
                    if let Some(e) = audio.poll() {
                        panic!("{what}: {e}");
                    }
                    assert!(t0.elapsed() < Duration::from_secs(120), "timeout: {what}");
                    std::thread::sleep(Duration::from_millis(20));
                }
                t0.elapsed()
            };
            wait(&mut audio, "first audio", &|a| !a.is_buffering() && a.position() > Duration::ZERO);
            let first = start.elapsed();
            let still_downloading = matches!(dl.state(), DlState::Running);
            println!(
                "{platform:?}: first audio after {first:.1?} (download {:.0}% done, still running: {still_downloading})",
                dl.progress() * 100.0
            );
            // Forward seek while (possibly) still downloading.
            audio.seek(Duration::from_secs(60));
            let took = wait(&mut audio, "seek 60", &|a| !a.is_buffering() && a.position() >= Duration::from_secs(60));
            println!("  seek -> 60s resumed after {took:.1?}, pos {:.1?}", audio.position());
            // Backward seek.
            audio.seek(Duration::from_secs(5));
            let took = wait(&mut audio, "seek 5", &|a| {
                !a.is_buffering() && a.position() >= Duration::from_secs(5) && a.position() < Duration::from_secs(30)
            });
            println!("  seek -> 5s resumed after {took:.1?}, pos {:.1?}", audio.position());
            wait(&mut audio, "download", &|_| !matches!(dl.state(), DlState::Running));
            println!("  download finished after {:.1?}", start.elapsed());
            audio.seek(Duration::from_secs(150));
            let took = wait(&mut audio, "seek 150", &|a| !a.is_buffering() && a.position() >= Duration::from_secs(150));
            println!("  seek -> 150s (complete file) resumed after {took:.1?}, duration {:?}", audio.duration());
            std::thread::sleep(Duration::from_millis(500));
            let p1 = audio.position();
            std::thread::sleep(Duration::from_millis(1000));
            let p2 = audio.position();
            println!("  advancing: {:.2?} in 1s", p2 - p1);
            assert!(p2 > p1);
        }
    }
}
