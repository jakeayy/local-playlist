//! Main window: sidebar with playlists, track list, and player bar.

mod settings;

use std::collections::HashMap;
use std::path::PathBuf;

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
use crate::secrets::{Credentials, KeyStorage, SecretStore};
use crate::sources::{self, Link, Resolved};
use crate::ytdlp::{self, Component, State, Status, YtDlp};
use gpui_kit::component::input::TextareaState;

use crate::audio::Input as AudioInput;
use crate::download::Download;
use crate::storage;

#[derive(Clone)]
struct NowPlaying {
    playlist_id: String,
    track: Track,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Playlist,
    Settings,
}

/// A running "Download all" for one playlist.
#[derive(Default)]
struct Batch {
    total: usize,
    done: usize,
    failed: usize,
    /// Downloads started by this batch, by target key.
    active: std::collections::HashSet<PathBuf>,
}

#[derive(PartialEq, Eq)]
enum PlayState {
    Idle,
    /// A track is loaded (it may still be buffering; see `Audio::is_buffering`).
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
    /// Running downloads keyed by target file stem (`<playlist dir>/<key>`).
    downloads: HashMap<PathBuf, Arc<Download>>,
    /// "Download all" queue: (playlist id, track entry id).
    download_queue: std::collections::VecDeque<(String, String)>,
    /// Progress of "Download all" per playlist id.
    batches: HashMap<String, Batch>,
    /// Last download error per target (`<playlist dir>/<key>`).
    download_errors: HashMap<PathBuf, String>,
    /// File stems present in each playlist folder (rescanned lazily).
    on_disk: HashMap<PathBuf, std::collections::HashSet<String>>,
    /// Playlist whose folder should follow its (edited) name, and when it was last edited.
    pending_folder_rename: Option<(String, f64)>,

    url_input: Entity<InputState>,
    import_input: Entity<InputState>,
    name_input: Entity<InputState>,
    seek: Entity<SliderState>,
    volume: Entity<SliderState>,
    seeking: Option<f32>,

    adding: bool,
    importing: bool,
    view: View,
    components: Option<Status>,
    /// Status text and progress of a running component install.
    installing: Option<Arc<Mutex<(String, Option<f32>)>>>,
    creds: Credentials,
    key_storage: Option<KeyStorage>,
    logging_in: Option<Platform>,
    /// Platform whose "paste cookies" editor is open.
    cookie_target: Option<Platform>,
    cookie_input: Entity<TextareaState>,
    /// YouTube channels of the logged-in account (loaded when the picker opens).
    yt_channels: Option<Result<Vec<sources::youtube::Channel>, String>>,
    channel_picker_open: bool,
    moving_music: bool,
    /// Web: the server wants its access password.
    server_locked: bool,
    server_password: Entity<InputState>,
    server_login_busy: bool,
    server_error: Option<String>,
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

        let (creds, key_storage) = match SecretStore::new(&paths).load() {
            Ok(r) => r,
            Err(e) => {
                pending_notes.push(Notification::error(format!(
                    "{e:#}. Log in again from Settings → Accounts."
                )));
                (Credentials::default(), None)
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
        let server_password = cx.new(|cx| InputState::new(window, cx).placeholder("Server password").masked(true));
        let cookie_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Paste a Netscape cookies.txt export, or a Cookie header (name=value; name2=value2)")
        });
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
            cx.subscribe_in(&server_password, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.server_login(window, cx);
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
                        if !crate::WEB {
                            this.pending_folder_rename = Some((id.clone(), crate::now_ms()));
                        }
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

        let mut subscriptions = subscriptions;
        subscriptions.push(cx.observe_window_appearance(window, |this, window, cx| {
            if this.library.settings.appearance == crate::theme::Appearance::System {
                crate::theme::apply(crate::theme::Appearance::System, Some(window), cx);
            }
        }));

        let ticker = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(200)).await;
                if this.update_in(cx, |this, window, cx| this.tick(window, cx)).is_err() {
                    break;
                }
            }
        });

        let mut library = library;
        for i in 0..library.playlists.len() {
            if library.playlists[i].folder.is_none() {
                let (id, name) = (library.playlists[i].id.clone(), library.playlists[i].name.clone());
                library.playlists[i].folder = Some(storage::unique_folder(&library, &id, &name));
            }
        }
        let selected = library.playlists.first().map(|p| p.id.clone());
        let mut this = Self {
            ytdlp: YtDlp::new(&paths, library.settings.ytdlp_source, library.settings.js_source),
            paths,
            library,
            selected: None,
            audio,
            now: None,
            state: PlayState::Idle,
            shuffle_order: Vec::new(),
            downloads: HashMap::new(),
            download_queue: Default::default(),
            batches: HashMap::new(),
            download_errors: HashMap::new(),
            on_disk: HashMap::new(),
            pending_folder_rename: None,
            url_input,
            import_input,
            name_input,
            seek,
            volume,
            seeking: None,
            adding: false,
            importing: false,
            view: View::Playlist,
            components: None,
            installing: None,
            creds,
            key_storage,
            logging_in: None,
            cookie_target: None,
            cookie_input,
            yt_channels: None,
            channel_picker_open: false,
            moving_music: false,
            server_locked: crate::WEB,
            server_password,
            server_login_busy: false,
            server_error: None,
            confirm_delete: false,
            pending_notes,
            list_scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
            _ticker: ticker,
        };
        this.select(selected, window, cx);
        this.refresh_components(cx);
        #[cfg(target_family = "wasm")]
        cx.spawn(async move |this, cx| {
            let ok = crate::web::api::check().await;
            let _ = this.update(cx, |this, cx| {
                this.server_locked = !ok;
                cx.notify();
            });
        })
        .detach();
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
        self.view = View::Playlist;
        self.selected = id;
        self.confirm_delete = false;
        let name = self.selected_playlist().map(|p| p.name.clone()).unwrap_or_default();
        self.name_input.update(cx, |input, cx| input.set_value(name, window, cx));
        self.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn new_playlist(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.library.playlists.len() + 1;
        let mut playlist = Playlist::new(format!("Playlist {n}"));
        playlist.folder = Some(storage::unique_folder(&self.library, &playlist.id, &playlist.name));
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
        if let Some(p) = self.library.playlist(&id).cloned().filter(|_| !crate::WEB) {
            let (paths, root, limit) = (self.paths.clone(), self.music_root(), self.removed_limit());
            std::thread::spawn(move || storage::retire_playlist(&paths, &root, &p, limit));
        }
        self.stop_batch(&id, cx);
        self.on_disk.clear();
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
        let (root, limit) = (self.music_root(), self.removed_limit());
        if let Some(p) = self.library.playlist_mut(&pid)
            && let Some(track) = p.tracks.iter().find(|t| t.id == track_id).cloned()
        {
            p.tracks.retain(|t| t.id != track_id);
            // Keep the file if the same song is still in this playlist.
            if !crate::WEB && !p.tracks.iter().any(|t| t.file_key() == track.file_key()) {
                storage::retire(&self.paths, &root, p, &track, limit);
            }
        }
        self.download_queue.retain(|(_, tid)| tid != track_id);
        self.on_disk.clear();
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

        let (paths, ytdlp, creds) = (self.paths.clone(), self.ytdlp.clone(), self.creds.clone());
        let as_collection = link.is_collection();
        let task = sources::resolve_task(cx, link, as_collection, paths, ytdlp, creds);
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
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
                    Err(e) => this.report_error(e, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Show an error, or the server login screen if the server wants a password.
    fn report_error(&mut self, e: anyhow::Error, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(target_family = "wasm")]
        if e.to_string() == crate::web::api::UNAUTHORIZED {
            self.server_locked = true;
            cx.notify();
            return;
        }
        window.push_notification(Notification::error(format!("{e:#}")), cx);
    }

    /// Web: log in to the server with its access password.
    fn server_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.server_login_busy {
            return;
        }
        let password = self.server_password.read(cx).value().to_string();
        if password.is_empty() {
            return;
        }
        self.server_login_busy = true;
        self.server_error = None;
        cx.notify();
        #[cfg(target_family = "wasm")]
        cx.spawn_in(window, async move |this, cx| {
            let result = crate::web::api::login(password).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.server_login_busy = false;
                match result {
                    Ok(()) => {
                        this.server_locked = false;
                        this.server_password.update(cx, |i, cx| i.set_value("", window, cx));
                    }
                    Err(e) => this.server_error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        #[cfg(not(target_family = "wasm"))]
        let _ = (password, window);
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
        self.import_link(link, true, window, cx);
    }

    /// Import a collection link as a new playlist.
    fn import_link(&mut self, link: Link, clear_input: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.importing {
            return;
        }
        self.importing = true;
        cx.notify();
        let (paths, ytdlp, creds) = (self.paths.clone(), self.ytdlp.clone(), self.creds.clone());
        let task = sources::resolve_task(cx, link, true, paths, ytdlp, creds);
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.importing = false;
                match result {
                    Ok(Resolved::Collection { title, tracks }) => {
                        let n = tracks.len();
                        let mut playlist = Playlist::new(title);
                        playlist.folder = Some(storage::unique_folder(&this.library, &playlist.id, &playlist.name));
                        playlist.tracks = tracks;
                        let id = playlist.id.clone();
                        this.library.playlists.push(playlist);
                        this.save();
                        if clear_input {
                            this.import_input.update(cx, |i, cx| i.set_value("", window, cx));
                        }
                        this.select(Some(id), window, cx);
                        window.push_notification(Notification::success(format!("Imported {n} tracks")), cx);
                    }
                    Ok(Resolved::Track { .. }) => window.push_notification(
                        Notification::warning("That link points to a single track, not a playlist."),
                        cx,
                    ),
                    Err(e) => this.report_error(e, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ------------------------------------------------------------- yt-dlp

    fn music_root(&self) -> PathBuf {
        storage::music_root(&self.library.settings)
    }

    fn removed_limit(&self) -> u64 {
        self.library.settings.removed_cache_mb * 1024 * 1024
    }

    fn clear_removed_cache(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = storage::removed_dir(&self.paths);
        let size = storage::dir_size(&dir);
        storage::enforce_limit(&dir, 0);
        let _ = std::fs::remove_dir_all(self.paths.cache.join("audio")); // pre-0.2 flat cache
        window.push_notification(
            Notification::info(format!("Freed {:.1} MB of removed tracks", size as f64 / 1e6)),
            cx,
        );
        cx.notify();
    }

    // ------------------------------------------------------------- playback

    /// Start (or join) the download of `track` into `playlist`'s folder.
    fn ensure_download(&mut self, playlist: &Playlist, track: &Track) -> Arc<Download> {
        let dir = storage::playlist_dir(&self.music_root(), playlist);
        let key = track.file_key();
        let map_key = dir.join(&key);
        if let Some(d) = self.downloads.get(&map_key) {
            return d.clone();
        }
        let tmp = dir.join(storage::DOWNLOADING_DIR).join(&key);
        let dl = Arc::new(Download::new(tmp.clone(), key.clone()));
        self.download_errors.remove(&map_key);
        self.downloads.insert(map_key, dl.clone());
        let (ytdlp, track, dl2) = (self.ytdlp.clone(), track.clone(), dl.clone());
        let cancel = dl.cancel.clone();
        let cookies = self.creds.get(track.platform).map(<[_]>::to_vec);
        std::thread::spawn(move || {
            let _ = std::fs::remove_dir_all(&tmp);
            let progress = |p: f32| dl2.set_progress(p);
            // Try anonymously first; only use the session if the content needs it.
            let mut res = ytdlp.download(&track, &tmp, None, Some(cancel.clone()), progress);
            if let (Err(e), Some(c)) = (&res, &cookies)
                && ytdlp::needs_login(&format!("{e:#}"))
            {
                let _ = std::fs::remove_dir_all(&tmp);
                res = ytdlp.download(&track, &tmp, Some(c), Some(cancel.clone()), progress);
            }
            // Move the finished file into the playlist folder.
            let res = res.and_then(|file| {
                let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("m4a").to_string();
                let dest = dir.join(format!("{key}.{ext}"));
                storage::move_file(&file, &dest)?;
                Ok(dest)
            });
            let _ = std::fs::remove_dir_all(&tmp);
            let _ = std::fs::remove_dir(dir.join(storage::DOWNLOADING_DIR));
            dl2.finish(res.map_err(|e| format!("{e:#}")));
        });
        dl
    }

    /// Local file for the track if available (reusing copies), else `None`.
    fn local_file(&self, playlist: &Playlist, track: &Track) -> Option<PathBuf> {
        let dir = storage::playlist_dir(&self.music_root(), playlist);
        if self.downloads.contains_key(&dir.join(track.file_key())) {
            return None;
        }
        storage::materialize(&self.paths, &self.library, playlist, track)
    }

    fn play(&mut self, playlist_id: &str, track_id: &str, cx: &mut Context<Self>) {
        let Some(playlist) = self.library.playlist(playlist_id).cloned() else { return };
        let Some(track) = playlist.tracks.iter().find(|t| t.id == track_id).cloned() else { return };
        if self.audio.is_none() {
            self.notify_error("No audio output device available");
            return;
        }
        let new_playlist = self.now.as_ref().is_none_or(|n| n.playlist_id != playlist_id);
        self.now = Some(NowPlaying { playlist_id: playlist_id.to_string(), track: track.clone() });
        if new_playlist || self.shuffle_order.is_empty() {
            self.reshuffle(track_id);
        }
        self.on_disk.clear(); // the lookup may have restored or copied a file
        #[cfg(not(target_family = "wasm"))]
        let input = match self.local_file(&playlist, &track) {
            Some(path) => AudioInput::File(path),
            None => AudioInput::Stream(self.ensure_download(&playlist, &track)),
        };
        #[cfg(target_family = "wasm")]
        let input = AudioInput::Url(crate::web::api::audio_url(&track));
        if let Some(a) = self.audio.as_mut() {
            a.load(input);
        }
        self.state = PlayState::Playing;
        if !crate::WEB {
            self.prefetch_next();
        }
        cx.notify();
    }

    fn prefetch_next(&mut self) {
        let Some(now) = &self.now else { return };
        let Some(playlist) = self.library.playlist(&now.playlist_id).cloned() else { return };
        if let Some(next) = self.neighbor(1, false)
            && self.local_file(&playlist, &next).is_none()
        {
            self.ensure_download(&playlist, &next);
        }
    }

    /// Queue every track of the playlist that isn't on disk yet.
    fn download_all(&mut self, playlist_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(playlist) = self.library.playlist(playlist_id).cloned() else { return };
        let mut queued = 0;
        for t in &playlist.tracks {
            let already = self.download_queue.iter().any(|(_, id)| *id == t.id);
            if !already && self.local_file(&playlist, t).is_none() {
                self.download_queue.push_back((playlist.id.clone(), t.id.clone()));
                queued += 1;
            }
        }
        if queued > 0 {
            self.batches.entry(playlist.id.clone()).or_default().total += queued;
        }
        let msg = if queued == 0 {
            "All tracks are already downloaded".to_string()
        } else {
            format!("Downloading {queued} tracks to {}", storage::playlist_dir(&self.music_root(), &playlist).display())
        };
        window.push_notification(Notification::info(msg), cx);
        cx.notify();
    }

    /// Keep up to two queued downloads running.
    fn pump_queue(&mut self) {
        const PARALLEL: usize = 2;
        while self.downloads.len() < PARALLEL {
            let Some((pid, tid)) = self.download_queue.pop_front() else { break };
            let track = self.library.playlist(&pid).and_then(|p| p.tracks.iter().find(|t| t.id == tid).cloned());
            let Some(playlist) = self.library.playlist(&pid).cloned() else { continue };
            let Some(track) = track else {
                // Removed from the playlist meanwhile.
                if let Some(b) = self.batches.get_mut(&pid) {
                    b.total = b.total.saturating_sub(1);
                }
                continue;
            };
            if self.local_file(&playlist, &track).is_none() {
                self.ensure_download(&playlist, &track);
                let key = storage::playlist_dir(&self.music_root(), &playlist).join(track.file_key());
                self.batches.entry(pid).or_default().active.insert(key);
            } else if let Some(b) = self.batches.get_mut(&pid) {
                b.done += 1;
            }
        }
    }

    /// Stop a playlist's "Download all": drop queued tracks and cancel running
    /// downloads (except the track that's playing right now).
    fn stop_batch(&mut self, playlist_id: &str, cx: &mut Context<Self>) {
        self.download_queue.retain(|(pid, _)| pid != playlist_id);
        let current = self.current_download_key();
        if let Some(batch) = self.batches.remove(playlist_id) {
            for key in batch.active {
                if Some(&key) != current.as_ref()
                    && let Some(dl) = self.downloads.get(&key)
                {
                    dl.cancel();
                }
            }
        }
        cx.notify();
    }

    fn current_download_key(&self) -> Option<PathBuf> {
        let now = self.now.as_ref()?;
        let playlist = self.library.playlist(&now.playlist_id)?;
        Some(storage::playlist_dir(&self.music_root(), playlist).join(now.track.file_key()))
    }

    /// File stems in a playlist folder (cached; cleared when files change).
    fn disk_keys(&mut self, dir: &std::path::Path) -> &std::collections::HashSet<String> {
        self.on_disk.entry(dir.to_path_buf()).or_insert_with(|| {
            std::fs::read_dir(dir)
                .map(|it| {
                    it.flatten()
                        .filter_map(|e| {
                            let p = e.path();
                            let ext = p.extension()?.to_str()?;
                            (p.is_file() && ytdlp::AUDIO_EXTS.contains(&ext))
                                .then(|| p.file_stem()?.to_str().map(str::to_string))
                                .flatten()
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    /// Rename a playlist's folder to follow its name (debounced, skipped while downloading into it).
    fn apply_folder_rename(&mut self) {
        let Some((id, at)) = self.pending_folder_rename.clone() else { return };
        if crate::now_ms() - at < 1200.0 {
            return;
        }
        let Some(p) = self.library.playlist(&id).cloned() else {
            self.pending_folder_rename = None;
            return;
        };
        let root = self.music_root();
        let old_dir = storage::playlist_dir(&root, &p);
        if self.downloads.keys().any(|k| k.starts_with(&old_dir)) {
            return; // retry when downloads into it are done
        }
        self.pending_folder_rename = None;
        let folder = storage::unique_folder(&self.library, &id, &p.name);
        if p.folder.as_deref() == Some(folder.as_str()) {
            return;
        }
        let new_dir = root.join(&folder);
        if old_dir.exists() && !new_dir.exists() {
            if let Err(e) = std::fs::rename(&old_dir, &new_dir) {
                self.notify_error(format!("Couldn't rename playlist folder: {e}"));
                return;
            }
        }
        if let Some(p) = self.library.playlist_mut(&id) {
            p.folder = Some(folder);
        }
        self.on_disk.clear();
        self.save();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(a) = self.audio.as_mut() {
            a.stop();
        }
        self.state = PlayState::Idle;
        self.now = None;
        cx.notify();
    }

    fn is_buffering(&self) -> bool {
        self.state == PlayState::Playing && self.audio.as_ref().is_some_and(|a| a.is_buffering())
    }

    /// Download progress of the current track, if it's being downloaded.
    fn current_download(&self) -> Option<f32> {
        let now = self.now.as_ref()?;
        let playlist = self.library.playlist(&now.playlist_id)?;
        let dir = storage::playlist_dir(&self.music_root(), playlist);
        self.downloads.get(&dir.join(now.track.file_key())).map(|d| d.progress())
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        match (&self.state, &self.audio) {
            (PlayState::Playing, Some(a)) if a.is_loaded() => {
                if a.is_paused() { a.play() } else { a.pause() }
            }
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
        // Metadata first: while streaming, players only know the length received so far.
        self.now
            .as_ref()
            .and_then(|n| n.track.duration_secs.map(|s| Duration::from_secs(s as u64)))
            .or_else(|| self.audio.as_ref().and_then(|a| a.duration()))
    }

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for note in self.pending_notes.drain(..) {
            window.push_notification(note, cx);
        }
        if self.installing.is_some() {
            cx.notify();
        }

        // Collect finished downloads.
        let finished: Vec<(PathBuf, Result<PathBuf, String>)> =
            self.downloads.iter().filter_map(|(k, d)| d.result().map(|r| (k.clone(), r))).collect();
        for (key, result) in finished {
            self.downloads.remove(&key);
            if let Some(dir) = key.parent() {
                self.on_disk.remove(dir);
            }
            let ok = result.is_ok();
            match result {
                Err(e) if e == crate::download::CANCELLED => {}
                Err(e) => {
                    eprintln!("download failed for {}: {e}", key.display());
                    self.download_errors.insert(key.clone(), e);
                }
                Ok(_) => {}
            }
            for batch in self.batches.values_mut() {
                if batch.active.remove(&key) {
                    if ok { batch.done += 1 } else { batch.failed += 1 }
                }
            }
            cx.notify();
        }
        // Finished batches.
        let finished_batches: Vec<String> = self
            .batches
            .iter()
            .filter(|(pid, b)| b.active.is_empty() && !self.download_queue.iter().any(|(q, _)| q == *pid))
            .map(|(pid, _)| pid.clone())
            .collect();
        for pid in finished_batches {
            let b = self.batches.remove(&pid).unwrap();
            let name = self.library.playlist(&pid).map(|p| p.name.clone()).unwrap_or_default();
            let note = if b.failed == 0 {
                Notification::success(format!("\"{name}\": {} tracks downloaded", b.done))
            } else {
                Notification::warning(format!(
                    "\"{name}\": {} downloaded, {} failed (hover the ⚠ icons for details)",
                    b.done, b.failed
                ))
            };
            window.push_notification(note, cx);
        }
        self.pump_queue();
        self.apply_folder_rename();

        // Player events.
        if let Some(err) = self.audio.as_mut().and_then(|a| a.poll()) {
            self.state = PlayState::Idle;
            let title = self.now.as_ref().map(|n| n.track.title.clone()).unwrap_or_default();
            window.push_notification(
                Notification::error(format!("Couldn't play \"{title}\": {err}")).autohide(false),
                cx,
            );
        }
        if self.state == PlayState::Playing {
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
        } else if !self.downloads.is_empty() {
            cx.notify();
        }
    }
}

/// Small xorshift shuffle; no need for cryptographic randomness.
fn shuffle<T>(v: &mut [T]) {
    let mut seed = (crate::now_ms() * 1000.0) as u64 ^ 0x9E37_79B9_7F4A_7C15 | 1;
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
                .rounded_full()
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

        let components_label = self.components_label();
        let setup_needed = self.setup_needed();
        let settings_open = self.view == View::Settings;

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
                            .child("YouTube playlists, SoundCloud sets, profiles and /likes. Private lists need a login (Settings)."),
                    ),
            )
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .text_color(if setup_needed { theme.warning } else { theme.muted_foreground })
                            .when(setup_needed, |d| d.child(Icon::new(IconName::CircleAlert).xsmall()))
                            .child(div().truncate().child(components_label)),
                    )
                    .child(
                        Button::new("open-settings")
                            .small()
                            .when(settings_open, |b| b.primary())
                            .icon(IconName::Settings)
                            .label("Settings & accounts")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.view = if this.view == View::Settings { View::Playlist } else { View::Settings };
                                cx.notify();
                            })),
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
        let playlist_id_for_dl = playlist.id.clone();

        let list = uniform_list(
            "tracks",
            count,
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                let theme = cx.theme().clone();
                let Some(dir) = this.selected_playlist().map(|p| storage::playlist_dir(&this.music_root(), p)) else {
                    return Vec::new();
                };
                let on_disk = if crate::WEB { Default::default() } else { this.disk_keys(&dir).clone() };
                let Some(p) = this.selected_playlist() else { return Vec::new() };
                let now_id = this.now.as_ref().map(|n| n.track.id.clone());
                let loading = this.is_buffering();
                let paused = this.audio.as_ref().is_some_and(|a| a.is_paused());
                range
                    .filter_map(|ix| p.tracks.get(ix).map(|t| (ix, t.clone())))
                    .map(|(ix, t)| {
                        let current = now_id.as_deref() == Some(&t.id);
                        let (pid, tid) = (p.id.clone(), t.id.clone());
                        let (tid_up, tid_down, tid_rm) = (t.id.clone(), t.id.clone(), t.id.clone());
                        let progress = this.downloads.get(&dir.join(t.file_key())).map(|d| d.progress());
                        h_flex()
                            .id(ix)
                            .w_full()
                            .h(px(48.))
                            .px_3()
                            .gap_3()
                            .rounded_lg()
                            .cursor_pointer()
                            .when(current, |d| d.bg(theme.primary.alpha(0.12)))
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
                            .child(
                                // Download status: progress, error, or on disk.
                                div().w(px(36.)).flex_none().flex().justify_end().child(
                                    if let Some(p) = progress {
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("{:.0}%", p * 100.))
                                            .into_any_element()
                                    } else if let Some(err) = this.download_errors.get(&dir.join(t.file_key())).cloned() {
                                        div()
                                            .id(("dl-err", ix))
                                            .child(Icon::new(IconName::CircleAlert).small().text_color(theme.warning))
                                            .tooltip(move |window, cx| {
                                                gpui_kit::component::tooltip::Tooltip::new(format!("Download failed: {err}"))
                                                    .build(window, cx)
                                            })
                                            .into_any_element()
                                    } else if on_disk.contains(&t.file_key()) {
                                        div()
                                            .id(("dl-ok", ix))
                                            .child(Icon::new(IconName::CircleCheck).small().text_color(theme.success))
                                            .tooltip(|window, cx| {
                                                gpui_kit::component::tooltip::Tooltip::new("Downloaded — plays offline")
                                                    .build(window, cx)
                                            })
                                            .into_any_element()
                                    } else {
                                        div().into_any_element()
                                    },
                                ),
                            )
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
                            .when(!crate::WEB, |d| d.child(self.render_download_all(&playlist_id_for_dl, count, cx)))
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

    /// "Download all" button, or batch progress with a stop button while running.
    fn render_download_all(&mut self, playlist_id: &str, count: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let pid = playlist_id.to_string();
        if let Some(b) = self.batches.get(playlist_id) {
            let active: f32 = b.active.iter().filter_map(|k| self.downloads.get(k)).map(|d| d.progress()).sum();
            let frac = ((b.done + b.failed) as f32 + active) / b.total.max(1) as f32;
            return h_flex()
                .gap_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(
                    div()
                        .w(px(120.))
                        .h(px(6.))
                        .rounded_full()
                        .bg(theme.muted)
                        .child(div().h_full().rounded_full().bg(theme.primary).w(relative(frac.clamp(0., 1.)))),
                )
                .child(format!("{}/{}", b.done + b.failed, b.total))
                .when(b.failed > 0, |d| d.child(div().text_color(theme.warning).child(format!("{} failed", b.failed))))
                .child(
                    Button::new("stop-download")
                        .ghost()
                        .xsmall()
                        .icon(IconName::CircleStop)
                        .tooltip("Stop downloading")
                        .on_click(cx.listener(move |this, _, _, cx| this.stop_batch(&pid, cx))),
                )
                .into_any_element();
        }
        let target = self.library.playlist(playlist_id).map(|p| {
            (storage::playlist_dir(&self.music_root(), p), p.tracks.iter().map(|t| t.file_key()).collect::<Vec<_>>())
        });
        let all_on_disk = target.is_some_and(|(dir, keys)| {
            let on_disk = self.disk_keys(&dir);
            !keys.is_empty() && keys.iter().all(|k| on_disk.contains(k))
        });
        Button::new("download-all")
            .ghost()
            .small()
            .icon(if all_on_disk { IconName::CircleCheck } else { IconName::HardDriveDownload })
            .tooltip(if all_on_disk {
                "All tracks are downloaded"
            } else {
                "Download all tracks for offline playback"
            })
            .disabled(count == 0)
            .on_click(cx.listener(move |this, _, window, cx| this.download_all(&pid, window, cx)))
            .into_any_element()
    }

    fn render_player(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let settings = self.library.settings.clone();
        let playing = matches!(self.state, PlayState::Playing)
            && self.audio.as_ref().is_some_and(|a| !a.is_paused());
        let buffering = self.is_buffering();
        let download = self.current_download();
        let loading = buffering.then(|| download.unwrap_or(0.));
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
                        Some(p) if p > 0. => format!("Buffering… (downloaded {:.0}%)", p * 100.),
                        Some(_) => "Starting stream…".to_string(),
                        None => match download {
                            Some(p) => format!("{} · streaming, {:.0}% downloaded", n.track.artist, p * 100.),
                            None => format!("{} · {}", n.track.artist, n.track.platform.label()),
                        },
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
            .bg(theme.sidebar)
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
    // Tonal chip in the platform's hue, tuned for light/dark surfaces.
    let hue = match platform {
        Platform::YouTube => 0. / 360.,
        Platform::SoundCloud => 22. / 360.,
    };
    let (fg, bg) = if theme.is_dark() {
        (hsla(hue, 0.9, 0.78, 1.), hsla(hue, 0.6, 0.5, 0.18))
    } else {
        (hsla(hue, 0.75, 0.38, 1.), hsla(hue, 0.9, 0.55, 0.12))
    };
    div()
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded_full()
        .text_xs()
        .font_medium()
        .text_color(fg)
        .bg(bg)
        .child(platform.label())
}

impl Render for LocalPlaylist {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if crate::WEB && self.server_locked {
            return self.render_server_login(cx).into_any_element();
        }
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
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .when_some(self.render_setup_banner(cx), |d, banner| d.child(banner))
                            .child(div().flex_1().min_h_0().child(match self.view {
                                View::Playlist => self.render_main(window, cx).into_any_element(),
                                View::Settings => self.render_settings(window, cx).into_any_element(),
                            })),
                    ),
            )
            .child(self.render_player(window, cx))
            .into_any_element()
    }
}

impl LocalPlaylist {
    /// Web: the server's access password prompt.
    fn render_server_login(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                v_flex()
                    .w(px(380.))
                    .gap_4()
                    .p_6()
                    .rounded_lg()
                    .bg(theme.sidebar)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Icon::new(IconName::Disc3).text_color(theme.primary))
                            .child(div().text_xl().font_semibold().child("Local Playlist")),
                    )
                    .child(div().text_sm().text_color(theme.muted_foreground).child(
                        "Enter this server's access password. Your playlists stay in this browser; the server only fetches public audio.",
                    ))
                    .child(Input::new(&self.server_password).mask_toggle())
                    .when_some(self.server_error.clone(), |d, e| {
                        d.child(div().text_sm().text_color(theme.danger).child(e))
                    })
                    .child(
                        Button::new("server-login")
                            .primary()
                            .icon(IconName::LogIn)
                            .label("Unlock")
                            .loading(self.server_login_busy)
                            .on_click(cx.listener(|this, _, window, cx| this.server_login(window, cx))),
                    ),
            )
    }
}
