//! Main window: sidebar with playlists, track list, and player bar.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::{ScrollableElement as _, Scrollbar};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::audio::Audio;
use crate::model::{Library, Paths, Platform, Playlist, RepeatMode, Track, format_duration};
use crate::sources::{self, Resolved};
use crate::ytdlp::{self, Backend, YtDlp};

/// Shared state of one background audio download.
struct Download {
    progress: AtomicU32, // f32 bits, 0..1
    result: Mutex<Option<Result<PathBuf, String>>>,
}

impl Download {
    fn progress(&self) -> f32 {
        f32::from_bits(self.progress.load(Ordering::Relaxed))
    }
}

#[derive(Clone)]
struct NowPlaying {
    playlist_id: String,
    track: Track,
}

enum PlayState {
    Idle,
    /// Waiting for the audio file of `now` to download.
    Loading { key: String },
    Playing,
}

pub struct LocalPlaylist {
    paths: Paths,
    ytdlp: YtDlp,
    library: Library,
    selected: Option<String>,

    audio: Option<Audio>,
    now: Option<NowPlaying>,
    state: PlayState,
    /// Order used when shuffle is on (track entry ids).
    shuffle_order: Vec<String>,
    downloads: HashMap<String, Arc<Download>>,

    url_input: Entity<InputState>,
    import_input: Entity<InputState>,
    name_input: Entity<InputState>,
    seek: Entity<SliderState>,
    volume: Entity<SliderState>,
    seeking: Option<f32>,

    adding: bool,
    importing: bool,
    backend: Option<Backend>,
    ytdlp_status: Option<String>,
    confirm_delete: bool,
    pending_notes: Vec<Notification>,

    list_scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
    _ticker: Task<()>,
}

impl LocalPlaylist {
    pub fn new(paths: Paths, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut pending_notes = Vec::new();
        let library = match Library::load(&paths.library_file()) {
            Ok(lib) => lib,
            Err(e) => {
                // Don't overwrite a library we failed to parse: back it up first.
                let backup = paths.library_file().with_extension("json.broken");
                let _ = std::fs::copy(paths.library_file(), &backup);
                pending_notes.push(Notification::error(format!(
                    "Could not read library ({e:#}). A backup was saved to {}",
                    backup.display()
                )));
                Library::default()
            }
        };

        let audio = match Audio::new(library.settings.volume) {
            Ok(a) => Some(a),
            Err(e) => {
                pending_notes.push(Notification::error(format!("{e:#}")));
                None
            }
        };

        let url_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Paste a YouTube / SoundCloud track or playlist link…")
        });
        let import_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Playlist, set or likes URL"));
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Playlist name"));
        let seek = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.001).default_value(0.));
        let volume = cx.new(|_| {
            SliderState::new().min(0.).max(1.).step(0.01).default_value(library.settings.volume)
        });

        let subscriptions = vec![
            cx.subscribe_in(&url_input, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.add_from_input(window, cx);
                }
            }),
            cx.subscribe_in(&import_input, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.import_from_input(window, cx);
                }
            }),
            cx.subscribe_in(&name_input, window, |this, input, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    let name = input.read(cx).value().to_string();
                    if let Some(id) = this.selected.clone()
                        && let Some(p) = this.library.playlist_mut(&id)
                        && p.name != name
                    {
                        p.name = name;
                        this.save();
                        cx.notify();
                    }
                }
            }),
            cx.subscribe_in(&seek, window, |this, _, ev: &SliderEvent, _, cx| match ev {
                SliderEvent::Change(v) => {
                    this.seeking = Some(v.start());
                    cx.notify();
                }
                SliderEvent::Release(v) => {
                    this.seeking = None;
                    if let Some(audio) = &this.audio
                        && let Some(total) = this.total_duration()
                    {
                        audio.seek(total.mul_f32(v.start().clamp(0., 1.)));
                    }
                    cx.notify();
                }
            }),
            cx.subscribe_in(&volume, window, |this, _, ev: &SliderEvent, _, cx| {
                let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
                this.library.settings.volume = v.start();
                if let Some(a) = &this.audio {
                    a.set_volume(v.start());
                }
                if matches!(ev, SliderEvent::Release(_)) {
                    this.save();
                }
                cx.notify();
            }),
        ];

        let ticker = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(200)).await;
                if this.update_in(cx, |this, window, cx| this.tick(window, cx)).is_err() {
                    break;
                }
            }
        });

        let selected = library.playlists.first().map(|p| p.id.clone());
        let mut this = Self {
            ytdlp: YtDlp::new(&paths),
            paths,
            library,
            selected: None,
            audio,
            now: None,
            state: PlayState::Idle,
            shuffle_order: Vec::new(),
            downloads: HashMap::new(),
            url_input,
            import_input,
            name_input,
            seek,
            volume,
            seeking: None,
            adding: false,
            importing: false,
            backend: None,
            ytdlp_status: None,
            confirm_delete: false,
            pending_notes,
            list_scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
            _ticker: ticker,
        };
        this.select(selected, window, cx);
        this.refresh_backend(cx);
        this
    }

    // ---------------------------------------------------------------- state

    fn save(&mut self) {
        if let Err(e) = self.library.save(&self.paths.library_file()) {
            self.pending_notes.push(Notification::error(format!("Failed to save library: {e:#}")));
        }
    }

    fn notify_error(&mut self, msg: impl Into<SharedString>) {
        self.pending_notes.push(Notification::error(msg));
    }

    fn selected_playlist(&self) -> Option<&Playlist> {
        self.selected.as_deref().and_then(|id| self.library.playlist(id))
    }

    fn select(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = id;
        self.confirm_delete = false;
        let name = self.selected_playlist().map(|p| p.name.clone()).unwrap_or_default();
        self.name_input.update(cx, |input, cx| input.set_value(name, window, cx));
        self.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn new_playlist(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.library.playlists.len() + 1;
        let playlist = Playlist::new(format!("Playlist {n}"));
        let id = playlist.id.clone();
        self.library.playlists.push(playlist);
        self.save();
        self.select(Some(id), window, cx);
    }

    fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else { return };
        if !self.confirm_delete {
            self.confirm_delete = true;
            cx.notify();
            return;
        }
        if self.now.as_ref().is_some_and(|n| n.playlist_id == id) {
            self.stop(cx);
        }
        let idx = self.library.playlists.iter().position(|p| p.id == id).unwrap_or(0);
        self.library.playlists.retain(|p| p.id != id);
        self.save();
        let next = self
            .library
            .playlists
            .get(idx.min(self.library.playlists.len().saturating_sub(1)))
            .map(|p| p.id.clone());
        self.select(next, window, cx);
    }

    fn remove_track(&mut self, track_id: &str, cx: &mut Context<Self>) {
        let Some(pid) = self.selected.clone() else { return };
        if let Some(p) = self.library.playlist_mut(&pid) {
            p.tracks.retain(|t| t.id != track_id);
        }
        self.shuffle_order.retain(|t| t != track_id);
        self.save();
        cx.notify();
    }

    fn move_track(&mut self, track_id: &str, delta: isize, cx: &mut Context<Self>) {
        let Some(pid) = self.selected.clone() else { return };
        if let Some(p) = self.library.playlist_mut(&pid)
            && let Some(i) = p.tracks.iter().position(|t| t.id == track_id)
        {
            let j = i as isize + delta;
            if j >= 0 && (j as usize) < p.tracks.len() {
                p.tracks.swap(i, j as usize);
                self.save();
                cx.notify();
            }
        }
    }

    // ------------------------------------------------------- adding/importing

    fn add_from_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.adding {
            return;
        }
        let text = self.url_input.read(cx).value().to_string();
        let link = match sources::parse_link(&text) {
            Ok(l) => l,
            Err(e) => {
                window.push_notification(Notification::warning(e.to_string()), cx);
                return;
            }
        };
        if self.selected.is_none() {
            self.new_playlist(window, cx);
        }
        let Some(target) = self.selected.clone() else { return };
        self.adding = true;
        cx.notify();

        let (paths, ytdlp) = (self.paths.clone(), self.ytdlp.clone());
        let as_collection = link.is_collection();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { sources::resolve(&link, as_collection, &paths, &ytdlp) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.adding = false;
                match result {
                    Ok(resolved) => {
                        let tracks = resolved.into_tracks();
                        let n = tracks.len();
                        if let Some(p) = this.library.playlist_mut(&target) {
                            p.tracks.extend(tracks);
                            this.save();
                            this.url_input.update(cx, |i, cx| i.set_value("", window, cx));
                            let msg = if n == 1 { "Added 1 track".to_string() } else { format!("Added {n} tracks") };
                            window.push_notification(Notification::success(msg), cx);
                        }
                    }
                    Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn import_from_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.importing {
            return;
        }
        let text = self.import_input.read(cx).value().to_string();
        let link = match sources::parse_link(&text) {
            Ok(l) => l,
            Err(e) => {
                window.push_notification(Notification::warning(e.to_string()), cx);
                return;
            }
        };
        let is_list = link.is_collection()
            || matches!(&link, sources::Link::YouTubeVideo { list: Some(_), .. })
            || matches!(&link, sources::Link::SoundCloudShort { .. });
        if !is_list {
            window.push_notification(
                Notification::warning("That's a single track — use the field above the track list to add it."),
                cx,
            );
            return;
        }
        self.importing = true;
        cx.notify();
        let (paths, ytdlp) = (self.paths.clone(), self.ytdlp.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { sources::resolve(&link, true, &paths, &ytdlp) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.importing = false;
                match result {
                    Ok(Resolved::Collection { title, tracks }) => {
                        let n = tracks.len();
                        let mut playlist = Playlist::new(title);
                        playlist.tracks = tracks;
                        let id = playlist.id.clone();
                        this.library.playlists.push(playlist);
                        this.save();
                        this.import_input.update(cx, |i, cx| i.set_value("", window, cx));
                        this.select(Some(id), window, cx);
                        window.push_notification(Notification::success(format!("Imported {n} tracks")), cx);
                    }
                    Ok(Resolved::Track(_)) => window.push_notification(
                        Notification::warning("That link points to a single track, not a playlist."),
                        cx,
                    ),
                    Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ------------------------------------------------------------- yt-dlp

    fn refresh_backend(&mut self, cx: &mut Context<Self>) {
        let ytdlp = self.ytdlp.clone();
        cx.spawn(async move |this, cx| {
            let backend = cx.background_spawn(async move { ytdlp.backend() }).await;
            let _ = this.update(cx, |this, cx| {
                this.backend = Some(backend);
                cx.notify();
            });
        })
        .detach();
    }

    fn update_ytdlp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ytdlp_status.is_some() {
            return;
        }
        self.ytdlp_status = Some("Starting update…".into());
        cx.notify();
        let ytdlp = self.ytdlp.clone();
        let status = Arc::new(Mutex::new(String::new()));
        let status_bg = status.clone();
        cx.spawn_in(window, async move |this, cx| {
            let task = cx.background_spawn(async move {
                ytdlp.update(|s| *status_bg.lock().unwrap() = s.to_string())
            });
            // Mirror status text while the update runs.
            let poll_status = status.clone();
            let this2 = this.clone();
            let mut cx2 = cx.clone();
            let poller = cx.spawn(async move |_| loop {
                cx2.background_executor().timer(Duration::from_millis(150)).await;
                let s = poll_status.lock().unwrap().clone();
                if this2
                    .update(&mut cx2, |this, cx| {
                        if this.ytdlp_status.is_some() && !s.is_empty() {
                            this.ytdlp_status = Some(s);
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            });
            let result = task.await;
            drop(poller);
            let _ = this.update_in(cx, |this, window, cx| {
                this.ytdlp_status = None;
                match result {
                    Ok(Some(info)) => window.push_notification(
                        Notification::success(format!("yt-dlp updated to {}", info.short())),
                        cx,
                    ),
                    Ok(None) => window.push_notification(
                        Notification::info("yt-dlp is already at the latest commit"),
                        cx,
                    ),
                    Err(e) => window.push_notification(
                        Notification::error(format!("yt-dlp update failed: {e:#}")),
                        cx,
                    ),
                }
                this.refresh_backend(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn clear_cache(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.paths.audio_cache();
        let current = self.now.as_ref().map(|n| n.track.cache_key());
        let mut removed = 0u64;
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                // Don't pull the file out from under the current track or running downloads.
                let busy = current.as_ref().is_some_and(|k| name.starts_with(&format!("{k}.")))
                    || self.downloads.keys().any(|k| name.starts_with(&format!("{k}.")));
                if !busy {
                    removed += e.metadata().map(|m| m.len()).unwrap_or(0);
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        window.push_notification(
            Notification::info(format!("Freed {:.1} MB of cached audio", removed as f64 / 1e6)),
            cx,
        );
    }

    // ------------------------------------------------------------- playback

    fn ensure_download(&mut self, track: &Track) -> Arc<Download> {
        let key = track.cache_key();
        if let Some(d) = self.downloads.get(&key) {
            return d.clone();
        }
        let dl = Arc::new(Download { progress: AtomicU32::new(0), result: Mutex::new(None) });
        self.downloads.insert(key, dl.clone());
        let (ytdlp, dir, track, dl2) = (self.ytdlp.clone(), self.paths.audio_cache(), track.clone(), dl.clone());
        std::thread::spawn(move || {
            let res = ytdlp
                .download(&track, &dir, |p| {
                    // Non-negative f32 bit patterns order like the floats, so fetch_max keeps progress monotonic.
                    dl2.progress.fetch_max(p.to_bits(), Ordering::Relaxed);
                })
                .map_err(|e| format!("{e:#}"));
            *dl2.result.lock().unwrap() = Some(res);
        });
        dl
    }

    fn play(&mut self, playlist_id: &str, track_id: &str, cx: &mut Context<Self>) {
        let Some(track) = self
            .library
            .playlist(playlist_id)
            .and_then(|p| p.tracks.iter().find(|t| t.id == track_id))
            .cloned()
        else {
            return;
        };
        if self.audio.is_none() {
            self.notify_error("No audio output device available");
            return;
        }
        let new_playlist = self.now.as_ref().is_none_or(|n| n.playlist_id != playlist_id);
        self.now = Some(NowPlaying { playlist_id: playlist_id.to_string(), track: track.clone() });
        if new_playlist || self.shuffle_order.is_empty() {
            self.reshuffle(track_id);
        }
        if let Some(a) = self.audio.as_mut() {
            a.stop();
        }

        let key = track.cache_key();
        match ytdlp::find_cached(&self.paths.audio_cache(), &key) {
            Some(path) if !self.downloads.contains_key(&key) => self.start_file(path, cx),
            _ => {
                self.ensure_download(&track);
                self.state = PlayState::Loading { key };
            }
        }
        cx.notify();
    }

    fn start_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(audio) = self.audio.as_mut() else { return };
        match audio.load(&path) {
            Ok(()) => {
                self.state = PlayState::Playing;
                self.prefetch_next();
            }
            Err(e) => {
                // A broken cache file: delete it so the next attempt re-downloads.
                let _ = std::fs::remove_file(&path);
                self.state = PlayState::Idle;
                self.notify_error(format!("Playback failed: {e:#}"));
            }
        }
        cx.notify();
    }

    fn prefetch_next(&mut self) {
        if let Some(next) = self.neighbor(1, false)
            && ytdlp::find_cached(&self.paths.audio_cache(), &next.cache_key()).is_none()
        {
            self.ensure_download(&next);
        }
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(a) = self.audio.as_mut() {
            a.stop();
        }
        self.state = PlayState::Idle;
        self.now = None;
        cx.notify();
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        match (&self.state, &self.audio) {
            (PlayState::Playing, Some(a)) if a.is_loaded() => {
                if a.is_paused() { a.play() } else { a.pause() }
            }
            (PlayState::Loading { .. }, _) => {}
            _ => {
                // Start the selected playlist from the top (or resume last track).
                let target = match &self.now {
                    Some(n) => Some((n.playlist_id.clone(), n.track.id.clone())),
                    None => self
                        .selected_playlist()
                        .and_then(|p| p.tracks.first().map(|t| (p.id.clone(), t.id.clone()))),
                };
                if let Some((pid, tid)) = target {
                    self.play(&pid, &tid, cx);
                }
            }
        }
        cx.notify();
    }

    fn order(&self) -> Vec<String> {
        let Some(now) = &self.now else { return Vec::new() };
        let Some(p) = self.library.playlist(&now.playlist_id) else { return Vec::new() };
        if self.library.settings.shuffle {
            // Keep shuffle order, dropping removed tracks and appending new ones.
            let mut order: Vec<String> = self
                .shuffle_order
                .iter()
                .filter(|id| p.tracks.iter().any(|t| &t.id == *id))
                .cloned()
                .collect();
            for t in &p.tracks {
                if !order.contains(&t.id) {
                    order.push(t.id.clone());
                }
            }
            order
        } else {
            p.tracks.iter().map(|t| t.id.clone()).collect()
        }
    }

    fn reshuffle(&mut self, first: &str) {
        let Some(now) = &self.now else { return };
        let Some(p) = self.library.playlist(&now.playlist_id) else { return };
        let mut ids: Vec<String> = p.tracks.iter().map(|t| t.id.clone()).filter(|id| id != first).collect();
        shuffle(&mut ids);
        ids.insert(0, first.to_string());
        self.shuffle_order = ids;
    }

    /// Track `delta` steps away from the current one in play order.
    fn neighbor(&self, delta: isize, wrap: bool) -> Option<Track> {
        let now = self.now.as_ref()?;
        let order = self.order();
        let pos = order.iter().position(|id| *id == now.track.id)? as isize;
        let mut next = pos + delta;
        let len = order.len() as isize;
        if next < 0 || next >= len {
            if !wrap || len == 0 {
                return None;
            }
            next = next.rem_euclid(len);
        }
        let id = &order[next as usize];
        self.library.playlist(&now.playlist_id)?.tracks.iter().find(|t| &t.id == id).cloned()
    }

    fn skip(&mut self, delta: isize, auto: bool, cx: &mut Context<Self>) {
        let Some(now) = self.now.clone() else { return };
        let repeat = self.library.settings.repeat;
        if auto && repeat == RepeatMode::One {
            self.play(&now.playlist_id, &now.track.id, cx);
            return;
        }
        let wrap = repeat != RepeatMode::Off || !auto;
        match self.neighbor(delta, wrap) {
            Some(t) => self.play(&now.playlist_id, &t.id, cx),
            None => {
                if let Some(a) = self.audio.as_mut() {
                    a.stop();
                }
                self.state = PlayState::Idle;
                cx.notify();
            }
        }
    }

    fn previous(&mut self, cx: &mut Context<Self>) {
        if let (PlayState::Playing, Some(a)) = (&self.state, &self.audio)
            && a.position() > Duration::from_secs(3)
        {
            a.seek(Duration::ZERO);
            return;
        }
        self.skip(-1, false, cx);
    }

    fn total_duration(&self) -> Option<Duration> {
        self.audio
            .as_ref()
            .and_then(|a| a.duration())
            .or_else(|| self.now.as_ref()?.track.duration_secs.map(|s| Duration::from_secs(s as u64)))
    }

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for note in self.pending_notes.drain(..) {
            window.push_notification(note, cx);
        }

        // Collect finished downloads.
        let finished: Vec<(String, Result<PathBuf, String>)> = self
            .downloads
            .iter()
            .filter_map(|(k, d)| d.result.lock().unwrap().clone().map(|r| (k.clone(), r)))
            .collect();
        for (key, result) in finished {
            self.downloads.remove(&key);
            let waiting = matches!(&self.state, PlayState::Loading { key: k } if *k == key);
            match result {
                Ok(path) if waiting => self.start_file(path, cx),
                Err(e) if waiting => {
                    self.state = PlayState::Idle;
                    let title = self.now.as_ref().map(|n| n.track.title.clone()).unwrap_or_default();
                    window.push_notification(
                        Notification::error(format!("Couldn't load \"{title}\": {e}")).autohide(false),
                        cx,
                    );
                }
                Err(e) => eprintln!("prefetch failed: {e}"),
                Ok(_) => {}
            }
        }

        match &self.state {
            PlayState::Playing => {
                if self.audio.as_ref().is_some_and(|a| a.finished()) {
                    self.skip(1, true, cx);
                } else if self.seeking.is_none() {
                    let pos = self.audio.as_ref().map(|a| a.position()).unwrap_or_default();
                    if let Some(total) = self.total_duration().filter(|d| !d.is_zero()) {
                        let frac = (pos.as_secs_f32() / total.as_secs_f32()).clamp(0., 1.);
                        self.seek.update(cx, |s, cx| s.set_value(frac, window, cx));
                    }
                }
                cx.notify();
            }
            PlayState::Loading { .. } => cx.notify(),
            PlayState::Idle => {}
        }
    }
}

/// Small xorshift shuffle; no need for cryptographic randomness.
fn shuffle<T>(v: &mut [T]) {
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E3779B97F4A7C15)
        | 1;
    for i in (1..v.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        v.swap(i, (seed % (i as u64 + 1)) as usize);
    }
}

// ------------------------------------------------------------------ rendering

impl LocalPlaylist {
    fn render_sidebar(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let playlists = self.library.playlists.iter().map(|p| {
            let selected = self.selected.as_deref() == Some(&p.id);
            let playing = self.now.as_ref().is_some_and(|n| n.playlist_id == p.id);
            let id = p.id.clone();
            h_flex()
                .id(SharedString::from(format!("pl-{}", p.id)))
                .px_2()
                .py_1p5()
                .gap_2()
                .rounded_md()
                .cursor_pointer()
                .when(selected, |d| d.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground))
                .hover(|d| d.bg(theme.sidebar_accent))
                .child(
                    Icon::new(if playing { IconName::Volume2 } else { IconName::ListMusic })
                        .small()
                        .text_color(if playing { theme.primary } else { theme.muted_foreground }),
                )
                .child(div().flex_1().truncate().child(p.name.clone()))
                .child(div().text_xs().text_color(theme.muted_foreground).child(p.tracks.len().to_string()))
                .on_click(cx.listener(move |this, _, window, cx| this.select(Some(id.clone()), window, cx)))
        });

        let backend_label: SharedString = match &self.backend {
            None => "yt-dlp: checking…".into(),
            Some(Backend::Managed(i)) => format!("yt-dlp {}", i.short()).into(),
            Some(Backend::System(v)) => format!("yt-dlp {v} (system)").into(),
            Some(Backend::Missing) => "yt-dlp not installed".into(),
        };

        v_flex()
            .w(px(260.))
            .h_full()
            .flex_none()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                h_flex()
                    .px_3()
                    .pt_3()
                    .pb_2()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Icon::new(IconName::Disc3).text_color(theme.primary))
                            .child(div().font_semibold().child("Local Playlist")),
                    )
                    .child(
                        Button::new("new-playlist")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .tooltip("New playlist")
                            .on_click(cx.listener(|this, _, window, cx| this.new_playlist(window, cx))),
                    ),
            )
            .child(
                div().flex_1().min_h_0().px_2().child(
                    v_flex()
                        .id("playlists")
                        .size_full()
                        .gap_0p5()
                        .overflow_y_scrollbar()
                        .children(playlists)
                        .when(self.library.playlists.is_empty(), |d| {
                            d.child(
                                div()
                                    .p_2()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child("No playlists yet. Create one with + or import below."),
                            )
                        }),
                ),
            )
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child("IMPORT PLAYLIST"))
                    .child(Input::new(&self.import_input).small().cleanable(true))
                    .child(
                        Button::new("import")
                            .small()
                            .icon(IconName::Import)
                            .label(if self.importing { "Importing…" } else { "Import as new playlist" })
                            .loading(self.importing)
                            .on_click(cx.listener(|this, _, window, cx| this.import_from_input(window, cx))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("YouTube playlists, SoundCloud sets, profiles and /likes. No login needed."),
                    ),
            )
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(self.ytdlp_status.clone().map(SharedString::from).unwrap_or(backend_label)),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("update-ytdlp")
                                    .small()
                                    .flex_1()
                                    .icon(IconName::RefreshCw)
                                    .label("Update yt-dlp")
                                    .loading(self.ytdlp_status.is_some())
                                    .tooltip("Download the latest yt-dlp commit from GitHub")
                                    .on_click(cx.listener(|this, _, window, cx| this.update_ytdlp(window, cx))),
                            )
                            .child(
                                Button::new("clear-cache")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Trash)
                                    .tooltip("Clear downloaded audio cache")
                                    .on_click(cx.listener(|this, _, window, cx| this.clear_cache(window, cx))),
                            ),
                    ),
            )
    }

    fn render_main(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let Some(playlist) = self.selected_playlist() else {
            return v_flex()
                .flex_1()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::ListMusic).large())
                .child("Create a playlist or import one to get started.")
                .child(
                    Button::new("empty-new")
                        .primary()
                        .icon(IconName::Plus)
                        .label("New playlist")
                        .on_click(cx.listener(|this, _, window, cx| this.new_playlist(window, cx))),
                )
                .into_any_element();
        };
        let count = playlist.tracks.len();
        let total: u32 = playlist.tracks.iter().filter_map(|t| t.duration_secs).sum();
        let pid = playlist.id.clone();

        let list = uniform_list(
            "tracks",
            count,
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                let theme = cx.theme().clone();
                let Some(p) = this.selected_playlist() else { return Vec::new() };
                let now_id = this.now.as_ref().map(|n| n.track.id.clone());
                let loading = matches!(this.state, PlayState::Loading { .. });
                let paused = this.audio.as_ref().is_some_and(|a| a.is_paused());
                range
                    .filter_map(|ix| p.tracks.get(ix).map(|t| (ix, t.clone())))
                    .map(|(ix, t)| {
                        let current = now_id.as_deref() == Some(&t.id);
                        let (pid, tid) = (p.id.clone(), t.id.clone());
                        let (tid_up, tid_down, tid_rm) = (t.id.clone(), t.id.clone(), t.id.clone());
                        let progress = this.downloads.get(&t.cache_key()).map(|d| d.progress());
                        h_flex()
                            .id(ix)
                            .w_full()
                            .h(px(48.))
                            .px_3()
                            .gap_3()
                            .rounded_md()
                            .cursor_pointer()
                            .when(current, |d| d.bg(theme.list_active))
                            .hover(|d| d.bg(theme.list_hover))
                            .child(
                                div().w(px(28.)).flex_none().text_sm().text_color(theme.muted_foreground).child(
                                    if current && loading {
                                        Icon::new(IconName::LoaderCircle).small().text_color(theme.primary).into_any_element()
                                    } else if current && !paused {
                                        Icon::new(IconName::Volume2).small().text_color(theme.primary).into_any_element()
                                    } else {
                                        (ix + 1).to_string().into_any_element()
                                    },
                                ),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .truncate()
                                            .when(current, |d| d.text_color(theme.primary).font_medium())
                                            .child(t.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .truncate()
                                            .text_color(theme.muted_foreground)
                                            .child(if t.artist.is_empty() { "—".to_string() } else { t.artist.clone() }),
                                    ),
                            )
                            .when_some(progress.filter(|p| *p > 0.), |d, p| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("{:.0}%", p * 100.)),
                                )
                            })
                            .child(platform_badge(t.platform, &theme))
                            .child(
                                div()
                                    .w(px(56.))
                                    .flex_none()
                                    .text_right()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(t.duration_secs.map(format_duration).unwrap_or_default()),
                            )
                            .child(
                                h_flex()
                                    .gap_0p5()
                                    .child(
                                        Button::new(("up", ix))
                                            .ghost()
                                            .xsmall()
                                            .icon(IconName::ArrowUp)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.move_track(&tid_up, -1, cx);
                                            })),
                                    )
                                    .child(
                                        Button::new(("down", ix))
                                            .ghost()
                                            .xsmall()
                                            .icon(IconName::ArrowDown)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.move_track(&tid_down, 1, cx);
                                            })),
                                    )
                                    .child(
                                        Button::new(("rm", ix))
                                            .ghost()
                                            .xsmall()
                                            .icon(IconName::X)
                                            .tooltip("Remove from playlist")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.remove_track(&tid_rm, cx);
                                            })),
                                    ),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| this.play(&pid, &tid, cx)))
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.list_scroll)
        .size_full();

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                v_flex()
                    .p_4()
                    .gap_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&self.name_input).appearance(false).text_xl()))
                            .child(
                                Button::new("play-all")
                                    .primary()
                                    .small()
                                    .icon(IconName::Play)
                                    .label("Play")
                                    .disabled(count == 0)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let first = this.library.playlist(&pid).and_then(|p| {
                                            if this.library.settings.shuffle && !p.tracks.is_empty() {
                                                let mut ids: Vec<_> = p.tracks.iter().map(|t| t.id.clone()).collect();
                                                shuffle(&mut ids);
                                                ids.first().cloned()
                                            } else {
                                                p.tracks.first().map(|t| t.id.clone())
                                            }
                                        });
                                        if let Some(tid) = first {
                                            this.now = None; // force a fresh shuffle order
                                            this.play(&pid, &tid, cx);
                                        }
                                    })),
                            )
                            .child(
                                Button::new("delete-playlist")
                                    .small()
                                    .when(self.confirm_delete, |b| b.danger().label("Click again to delete"))
                                    .when(!self.confirm_delete, |b| b.ghost())
                                    .icon(IconName::Trash)
                                    .tooltip("Delete playlist")
                                    .on_click(cx.listener(|this, _, window, cx| this.delete_selected(window, cx))),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{count} track{}{}",
                                if count == 1 { "" } else { "s" },
                                if total > 0 { format!(" · {}", format_duration(total)) } else { String::new() }
                            )),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div().flex_1().child(
                                    Input::new(&self.url_input)
                                        .prefix(Icon::new(IconName::Link).small().text_color(theme.muted_foreground))
                                        .cleanable(true),
                                ),
                            )
                            .child(
                                Button::new("add")
                                    .icon(IconName::Plus)
                                    .label("Add")
                                    .loading(self.adding)
                                    .on_click(cx.listener(|this, _, window, cx| this.add_from_input(window, cx))),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .px_2()
                    .py_1()
                    .when(count == 0, |d| {
                        d.child(
                            v_flex()
                                .size_full()
                                .items_center()
                                .justify_center()
                                .text_color(theme.muted_foreground)
                                .child("This playlist is empty. Paste a link above to add tracks."),
                        )
                    })
                    .when(count > 0, |d| {
                        d.child(list).child(
                            div()
                                .absolute()
                                .top_0()
                                .right_0()
                                .bottom_0()
                                .w(Scrollbar::width())
                                .child(Scrollbar::vertical(&self.list_scroll)),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_player(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let settings = self.library.settings.clone();
        let playing = matches!(self.state, PlayState::Playing)
            && self.audio.as_ref().is_some_and(|a| !a.is_paused());
        let loading = match &self.state {
            PlayState::Loading { key } => Some(self.downloads.get(key).map(|d| d.progress()).unwrap_or(0.)),
            _ => None,
        };
        let total = self.total_duration();
        let pos = match (self.seeking, total) {
            (Some(frac), Some(t)) => t.mul_f32(frac),
            _ => self.audio.as_ref().map(|a| a.position()).unwrap_or_default(),
        };
        let fmt = |d: Duration| format_duration(d.as_secs() as u32);

        let info = match &self.now {
            Some(n) => v_flex()
                .min_w_0()
                .child(div().truncate().font_medium().child(n.track.title.clone()))
                .child(
                    div().text_xs().truncate().text_color(theme.muted_foreground).child(match loading {
                        Some(p) if p > 0. => format!("Downloading… {:.0}%", p * 100.),
                        Some(_) => "Fetching audio…".to_string(),
                        None => format!("{} · {}", n.track.artist, n.track.platform.label()),
                    }),
                ),
            None => v_flex().child(div().text_color(theme.muted_foreground).child("Nothing playing")),
        };

        h_flex()
            .h(px(84.))
            .flex_none()
            .px_4()
            .gap_4()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(div().w(px(260.)).flex_none().child(info))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .items_center()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("shuffle")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Shuffle)
                                    .toggled(settings.shuffle)
                                    .tooltip(if settings.shuffle { "Shuffle: on" } else { "Shuffle: off" })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.library.settings.shuffle = !this.library.settings.shuffle;
                                        if let Some(id) = this.now.as_ref().map(|n| n.track.id.clone()) {
                                            this.reshuffle(&id);
                                        }
                                        this.save();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("prev")
                                    .ghost()
                                    .icon(IconName::SkipBack)
                                    .on_click(cx.listener(|this, _, _, cx| this.previous(cx))),
                            )
                            .child(
                                Button::new("play")
                                    .primary()
                                    .rounded(gpui_kit::component::button::ButtonRounded::Large)
                                    .icon(if playing { IconName::Pause } else { IconName::Play })
                                    .loading(loading.is_some())
                                    .on_click(cx.listener(|this, _, _, cx| this.toggle_play(cx))),
                            )
                            .child(
                                Button::new("next")
                                    .ghost()
                                    .icon(IconName::SkipForward)
                                    .on_click(cx.listener(|this, _, _, cx| this.skip(1, false, cx))),
                            )
                            .child(
                                Button::new("repeat")
                                    .ghost()
                                    .small()
                                    .icon(if settings.repeat == RepeatMode::One {
                                        IconName::Repeat1
                                    } else {
                                        IconName::Repeat
                                    })
                                    .toggled(settings.repeat != RepeatMode::Off)
                                    .tooltip(match settings.repeat {
                                        RepeatMode::Off => "Repeat: off",
                                        RepeatMode::All => "Repeat: playlist",
                                        RepeatMode::One => "Repeat: track",
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.library.settings.repeat = this.library.settings.repeat.next();
                                        this.save();
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .max_w(px(640.))
                            .gap_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(div().w(px(44.)).text_right().child(fmt(pos)))
                            .child(
                                div()
                                    .flex_1()
                                    .child(Slider::new(&self.seek).disabled(!matches!(self.state, PlayState::Playing))),
                            )
                            .child(div().w(px(44.)).child(total.map(fmt).unwrap_or_else(|| "--:--".into()))),
                    ),
            )
            .child(
                h_flex()
                    .w(px(180.))
                    .flex_none()
                    .gap_2()
                    .child(
                        Icon::new(if settings.volume <= 0.001 { IconName::VolumeX } else { IconName::Volume2 })
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().flex_1().child(Slider::new(&self.volume))),
            )
    }
}

fn platform_badge(platform: Platform, theme: &gpui_kit::component::Theme) -> impl IntoElement {
    let color = match platform {
        Platform::YouTube => hsla(0. / 360., 0.75, 0.5, 1.),
        Platform::SoundCloud => hsla(22. / 360., 0.95, 0.52, 1.),
    };
    div()
        .flex_none()
        .px_1p5()
        .rounded_sm()
        .text_xs()
        .border_1()
        .border_color(color.alpha(0.5))
        .text_color(color)
        .bg(color.alpha(0.08))
        .child(platform.label())
        .when(theme.is_dark(), |d| d.bg(color.alpha(0.15)))
}

impl Render for LocalPlaylist {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar(window, cx))
                    .child(self.render_main(window, cx)),
            )
            .child(self.render_player(window, cx))
    }
}
