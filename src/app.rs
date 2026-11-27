//! Main window: sidebar with playlists, track list, and player bar.

mod backdrop;
mod lyrics;
mod progress;
mod settings;
mod thumbs;
mod visualizer;

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

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
use gpui_kit::component::input::{Textarea, TextareaState};

use crate::audio::Input as AudioInput;
use crate::download::Download;
use crate::sponsorblock::Segment;
use crate::storage;

#[derive(Clone)]
struct NowPlaying {
    playlist_id: String,
    track: Track,
}

/// Rows of the selected playlist a search shows.
struct FilterCache {
    playlist_id: String,
    query: String,
    library_rev: u64,
    /// Indices into the playlist's tracks.
    rows: Rc<Vec<usize>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Playlist,
    Settings,
}

/// What the playlist page shows: its tracks, or a page about what's playing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Tracks,
    Visualizer,
    Lyrics,
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
    /// The window showing this view; none while playing in the background
    /// (desktop: the window closed, the tray icon kept the app running).
    window: Option<AnyWindowHandle>,
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
    /// Size of the removed-tracks cache and when it was measured (Settings).
    removed_size: Option<(u64, f64)>,
    /// Playlist whose folder should follow its (edited) name, and when it was last edited.
    pending_folder_rename: Option<(String, f64)>,
    /// When unsaved library changes were made (saved after a pause; see `save_later`).
    save_due: Option<f64>,
    /// Bumped on every library change (they all save), to invalidate caches.
    library_rev: u64,
    /// Rows a search shows (see `filtered_rows`).
    filter_cache: Option<FilterCache>,
    /// SponsorBlock segments by YouTube video id, with when they were fetched.
    segments: HashMap<String, (f64, Vec<Segment>)>,
    /// Video ids whose segments are being fetched.
    segments_loading: std::collections::HashSet<String>,
    /// Last SponsorBlock jump (target secs, when), so a slow seek isn't repeated.
    last_skip: Option<(f64, f64)>,

    url_input: Entity<InputState>,
    import_input: Entity<InputState>,
    /// Many links, one per line ("Add many links").
    bulk_input: Entity<TextareaState>,
    name_input: Entity<InputState>,
    search_input: Entity<InputState>,
    seek: Entity<SliderState>,
    volume: Entity<SliderState>,
    seeking: Option<f32>,
    /// The seek row of the player bar (see `progress.rs`).
    progress: Entity<progress::Progress>,
    /// Playback state shown outside the seek row (buffering, paused) at the
    /// last tick, to redraw the window only when it changes.
    last_shown: (bool, bool),

    adding: bool,
    importing: bool,
    /// The "Add many links" panel above the track list is open.
    bulk_open: bool,
    /// Links of a running "Add many links" resolved so far, and how many there are.
    bulk_progress: Option<(usize, usize)>,
    /// The sidebar's "Import playlist" section is expanded.
    import_open: bool,
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
    /// The rows the track list shows (indices into the selected playlist's
    /// tracks; `None` = all), as last drawn.
    list_rows: Option<Rc<Vec<usize>>>,
    /// Shown in place of the track list (`panel`).
    visualizer: Entity<visualizer::Visualizer>,
    lyrics: Entity<lyrics::LyricsView>,
    panel: Panel,
    /// Subscriptions to the window and its inputs (renewed with the window).
    window_subscriptions: Vec<Subscription>,
    /// Tray icon, while playing in the background is on and the desktop has a tray.
    #[cfg(feature = "desktop")]
    tray: Option<crate::tray::Tray>,
    #[cfg(feature = "desktop")]
    tray_commands: futures::channel::mpsc::UnboundedSender<crate::tray::Command>,
    /// The desktop's tray shows the icon (it can go away, e.g. a panel restart).
    #[cfg(feature = "desktop")]
    tray_shown: bool,
    /// What the tray last showed.
    #[cfg(feature = "desktop")]
    tray_status: crate::tray::Status,
    /// Discord Rich Presence, while it's on.
    #[cfg(feature = "desktop")]
    presence: Option<crate::discord::Presence>,
    /// Track (platform, id) whose cover the visualizer shows or is loading.
    cover_for: Option<(Platform, String)>,
    /// Cover of the playing track (player bar).
    now_cover: Option<Arc<RenderImage>>,
    /// Covers replaced by another track's, with when: dropped from GPUI's
    /// texture atlas on a later tick, once no frame shows them.
    stale_covers: Vec<(Arc<RenderImage>, f64)>,
    /// Dominant (hue, saturation) of that cover, for `Settings::cover_colors`.
    cover_accent: Option<(f32, f32)>,
    /// Accent the theme shows (or is fading to); `None` = stock.
    accent_shown: Option<(f32, f32)>,
    accent_fade: Option<Task<()>>,
    /// Track list thumbnails.
    thumbs: thumbs::Thumbs,
    _subscriptions: Vec<Subscription>,
    _ticker: Task<()>,
}

/// The text inputs. They belong to the window they were made for (focus,
/// input methods), so a window reopened from the tray gets new ones.
struct Inputs {
    url_input: Entity<InputState>,
    import_input: Entity<InputState>,
    bulk_input: Entity<TextareaState>,
    name_input: Entity<InputState>,
    search_input: Entity<InputState>,
    server_password: Entity<InputState>,
    cookie_input: Entity<TextareaState>,
}

impl Inputs {
    fn new(window: &mut Window, cx: &mut App) -> Self {
        Self {
            url_input: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Paste a YouTube / SoundCloud track or playlist link…")
            }),
            import_input: cx.new(|cx| InputState::new(window, cx).placeholder("Playlist, set or likes URL")),
            bulk_input: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder("One YouTube or SoundCloud link per line (# starts a comment)")
            }),
            name_input: cx.new(|cx| InputState::new(window, cx).placeholder("Playlist name")),
            search_input: cx.new(|cx| InputState::new(window, cx).placeholder("Search title or artist")),
            server_password: cx.new(|cx| InputState::new(window, cx).placeholder("Server password").masked(true)),
            cookie_input: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder("Paste a Netscape cookies.txt export, or a Cookie header (name=value; name2=value2)")
            }),
        }
    }
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
                pending_notes.push(Notification::error(format!("{e:#}. Log in again from Settings → Accounts.")));
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
        let visualizer = {
            let tap = audio.as_ref().map(|a| a.tap());
            cx.new(|_| visualizer::Visualizer::new(tap))
        };

        let Inputs { url_input, import_input, bulk_input, name_input, search_input, server_password, cookie_input } =
            Inputs::new(window, cx);
        let seek = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.001).default_value(0.));
        let volume = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.01).default_value(library.settings.volume));

        let subscriptions = vec![
            cx.subscribe(&seek, |this, _, ev: &SliderEvent, cx| match ev {
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
            cx.subscribe(&volume, |this, _, ev: &SliderEvent, cx| {
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

        let progress = {
            let app = cx.weak_entity();
            cx.new(|_| progress::Progress::new(app))
        };
        let lyrics = {
            let app = cx.weak_entity();
            cx.new(|cx| lyrics::LyricsView::new(app, cx))
        };

        let mut subscriptions = subscriptions;
        // Write edits still waiting for `save_if_due`, whichever comes first:
        // quitting, or the window (and with it this view) closing.
        subscriptions.push(cx.on_app_quit(|this, _| {
            if this.save_due.is_some() {
                this.save();
            }
            async {}
        }));
        subscriptions.push(cx.on_release(|this, _| {
            if this.save_due.is_some() {
                this.save();
            }
        }));

        // Not tied to the window: playback (next track, SponsorBlock), the
        // download queue and saving go on while it's closed (desktop).
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(200)).await;
                let Ok(window) = this.update(cx, |this, cx| {
                    this.tick(cx);
                    this.window
                }) else {
                    break;
                };
                if let Some(window) = window {
                    let _ = window.update(cx, |_, window, cx| this.update(cx, |this, cx| this.tick_window(window, cx)));
                }
            }
        });

        #[cfg(feature = "desktop")]
        let tray_commands = {
            use futures::StreamExt as _;
            let (tx, mut rx) = futures::channel::mpsc::unbounded();
            cx.spawn(async move |this, cx| {
                while let Some(command) = rx.next().await {
                    let Some(main) = this.upgrade() else { break };
                    cx.update(|cx| tray_command(&main, command, cx));
                }
            })
            .detach();
            tx
        };

        let mut library = library;
        for i in 0..library.playlists.len() {
            if library.playlists[i].folder.is_none() {
                let (id, name) = (library.playlists[i].id.clone(), library.playlists[i].name.clone());
                library.playlists[i].folder = Some(storage::unique_folder(&library, &id, &name));
            }
        }
        let selected = library.playlists.first().map(|p| p.id.clone());
        #[cfg(not(target_family = "wasm"))]
        let segments = crate::sponsorblock::cache::load(&crate::sponsorblock::cache::file(&paths))
            .into_iter()
            .map(|(id, e)| (id, (e.fetched_ms, e.segments)))
            .collect();
        #[cfg(target_family = "wasm")]
        let segments = HashMap::new();
        let mut this = Self {
            ytdlp: YtDlp::new(&paths, library.settings.ytdlp_source, library.settings.js_source),
            paths,
            window: Some(window.window_handle()),
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
            removed_size: None,
            pending_folder_rename: None,
            save_due: None,
            library_rev: 0,
            filter_cache: None,
            segments,
            segments_loading: Default::default(),
            last_skip: None,
            url_input,
            import_input,
            bulk_input,
            name_input,
            search_input,
            seek,
            volume,
            seeking: None,
            progress,
            last_shown: (false, false),
            adding: false,
            importing: false,
            bulk_open: false,
            bulk_progress: None,
            // Open to start with only while there's nothing to play yet.
            import_open: selected.is_none(),
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
            list_rows: None,
            visualizer,
            lyrics,
            panel: Panel::Tracks,
            window_subscriptions: Vec::new(),
            #[cfg(feature = "desktop")]
            tray: None,
            #[cfg(feature = "desktop")]
            tray_commands,
            #[cfg(feature = "desktop")]
            tray_shown: false,
            #[cfg(feature = "desktop")]
            tray_status: Default::default(),
            #[cfg(feature = "desktop")]
            presence: None,
            cover_for: None,
            now_cover: None,
            stale_covers: Vec::new(),
            cover_accent: None,
            accent_shown: None,
            accent_fade: None,
            thumbs: thumbs::Thumbs::default(),
            _subscriptions: subscriptions,
            _ticker: ticker,
        };
        this.window_subscriptions = this.window_subscriptions(window, cx);
        this.select(selected, window, cx);
        this.refresh_components(cx);
        #[cfg(feature = "desktop")]
        {
            this.start_tray();
            this.update_presence();
        }
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

    // --------------------------------------------------------------- window

    /// Listen to the window and the inputs made for it.
    fn window_subscriptions(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Subscription> {
        vec![
            cx.subscribe_in(&self.url_input, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.add_from_input(window, cx);
                }
            }),
            cx.subscribe_in(&self.server_password, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.server_login(window, cx);
                }
            }),
            cx.subscribe_in(&self.import_input, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.import_from_input(window, cx);
                }
            }),
            cx.subscribe_in(&self.search_input, window, |this, _, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    this.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                }
            }),
            cx.subscribe_in(&self.name_input, window, |this, input, ev: &InputEvent, _, cx| {
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
                        this.save_later();
                        cx.notify();
                    }
                }
            }),
            cx.observe_window_appearance(window, |this, window, cx| {
                if this.library.settings.appearance == crate::theme::Appearance::System {
                    crate::theme::apply(crate::theme::Appearance::System, Some(window), cx);
                }
            }),
        ]
    }

    /// Show this view in a new window, after the previous one was closed and
    /// playback went on in the background (desktop).
    #[cfg(feature = "desktop")]
    fn attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let inputs = Inputs::new(window, cx);
        self.url_input = inputs.url_input;
        self.import_input = inputs.import_input;
        self.bulk_input = inputs.bulk_input;
        self.name_input = inputs.name_input;
        self.search_input = inputs.search_input;
        self.server_password = inputs.server_password;
        self.cookie_input = inputs.cookie_input;
        self.window = Some(window.window_handle());
        self.window_subscriptions = self.window_subscriptions(window, cx);
        crate::theme::apply(self.library.settings.appearance, Some(window), cx);
        // Fills in the new name field.
        self.select(self.selected.clone(), window, cx);
        self.refresh_lyrics(cx);
    }

    /// The window was closed: keep going without it (desktop).
    #[cfg(feature = "desktop")]
    fn detach(&mut self) {
        self.window = None;
        self.window_subscriptions.clear();
        self.seeking = None;
        // Their textures went with the window; thumbnails load again when shown.
        self.stale_covers.clear();
        self.thumbs = thumbs::Thumbs::default();
        crate::mem::trim();
    }

    /// Finish work started from the window: with the window if one is open
    /// (for its inputs and notifications), without it if it was closed in the
    /// meantime (desktop, playing in the background), so results aren't lost.
    fn finish(
        this: &WeakEntity<Self>,
        cx: &mut AsyncApp,
        f: impl FnOnce(&mut Self, Option<&mut Window>, &mut Context<Self>),
    ) {
        let mut f = Some(f);
        if let Ok(Some(handle)) = this.read_with(cx, |this, _| this.window) {
            let _ = handle.update(cx, |_, window, cx| {
                this.update(cx, |this, cx| {
                    if let Some(f) = f.take() {
                        f(this, Some(window), cx)
                    }
                })
            });
        }
        if let Some(f) = f.take() {
            let _ = this.update(cx, |this, cx| f(this, None, cx));
        }
    }

    /// Show a notification now, or in the next window if none is open.
    fn note(&mut self, window: Option<&mut Window>, note: Notification, cx: &mut Context<Self>) {
        match window {
            Some(window) => window.push_notification(note, cx),
            None => self.pending_notes.push(note),
        }
    }

    // ----------------------------------------------------------------- tray

    /// Show the tray icon, so closing the window keeps the app playing.
    #[cfg(feature = "desktop")]
    fn start_tray(&mut self) {
        self.tray = crate::tray::Tray::start(self.tray_commands.clone());
        self.tray_shown = self.tray.is_some();
    }

    /// Whether closing the window leaves the app running: only while the
    /// tray can bring the window back (without one, e.g. on GNOME without
    /// the AppIndicator extension, closing quits).
    #[cfg(feature = "desktop")]
    fn runs_in_background(&self) -> bool {
        self.tray.is_some() && self.tray_shown
    }

    /// Keep the tray's menu and tooltip up to date.
    #[cfg(feature = "desktop")]
    fn sync_tray(&mut self) {
        let Some(tray) = &self.tray else { return };
        let status = crate::tray::Status {
            track: self.now.as_ref().map(|n| {
                if n.track.artist.is_empty() {
                    n.track.title.clone()
                } else {
                    format!("{} — {}", n.track.title, n.track.artist)
                }
            }),
            playing: self.state == PlayState::Playing && self.audio.as_ref().is_some_and(|a| !a.is_paused()),
            window_open: self.window.is_some(),
        };
        if status != self.tray_status {
            tray.set_status(&status);
            self.tray_status = status;
        }
    }

    // ------------------------------------------------------------- discord

    /// Connect to Discord while Rich Presence is on; dropping the handle
    /// disconnects, which clears what Discord shows.
    #[cfg(feature = "desktop")]
    fn update_presence(&mut self) {
        if !self.library.settings.discord.enabled {
            self.presence = None;
        } else if self.presence.is_none() {
            self.presence = Some(crate::discord::Presence::start(self.paths.clone()));
        }
    }

    /// Keep Discord showing what's playing (paused too; nothing when stopped).
    #[cfg(feature = "desktop")]
    fn sync_presence(&mut self, cx: &mut Context<Self>) {
        if self.presence.is_none() {
            return;
        }
        let duration = self.total_duration();
        let now = match (&self.now, &self.audio) {
            (Some(n), Some(a)) if self.state == PlayState::Playing && a.is_loaded() => Some(crate::discord::Now {
                track: &n.track,
                position: a.position(),
                duration,
                paused: a.is_paused(),
                buffering: a.is_buffering(),
                accent: self.accent_shown,
            }),
            _ => None,
        };
        let Some(presence) = self.presence.as_mut() else { return };
        // Settings shows whether Discord was found.
        if presence.update(now, &self.library.settings.discord) && self.view == View::Settings {
            cx.notify();
        }
    }

    // ---------------------------------------------------------------- state

    fn save(&mut self) {
        self.save_due = None;
        self.library_rev += 1;
        if let Err(e) = self.library.save(&self.paths.library_file()) {
            self.pending_notes.push(Notification::error(format!("Failed to save library: {e:#}")));
        }
    }

    /// Save once edits pause (typing a playlist name would otherwise write the
    /// whole library on every keystroke). Flushed on quit, too.
    fn save_later(&mut self) {
        self.save_due = Some(crate::now_ms());
        self.library_rev += 1;
    }

    /// Write changes left by [`Self::save_later`] if edits paused long enough.
    fn save_if_due(&mut self) {
        if self.save_due.is_some_and(|at| crate::now_ms() - at >= 1000.0) {
            self.save();
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
        self.search_input.update(cx, |input, cx| input.set_value("", window, cx));
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
        let idx = self.library.playlists.iter().position(|p| p.id == id);
        if let Some(idx) = idx {
            let p = self.library.playlists.remove(idx);
            if !crate::WEB {
                let (paths, root, limit) = (self.paths.clone(), self.music_root(), self.removed_limit());
                std::thread::spawn(move || storage::retire_playlist(&paths, &root, &p, limit));
            }
        }
        let idx = idx.unwrap_or(0);
        self.stop_batch(&id, cx);
        self.on_disk.clear();
        self.save();
        let next =
            self.library.playlists.get(idx.min(self.library.playlists.len().saturating_sub(1))).map(|p| p.id.clone());
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
        cx.spawn(async move |this, cx| {
            let result = task.await;
            Self::finish(&this, cx, move |this, mut window, cx| {
                this.adding = false;
                crate::mem::trim(); // resolving parsed large JSON pages
                match result {
                    Ok(resolved) => {
                        let tracks = resolved.into_tracks();
                        let n = tracks.len();
                        if let Some(p) = this.library.playlist_mut(&target) {
                            p.tracks.extend(tracks);
                            this.save();
                            if let Some(window) = window.as_deref_mut() {
                                this.url_input.update(cx, |i, cx| i.set_value("", window, cx));
                            }
                            let msg = if n == 1 { "Added 1 track".to_string() } else { format!("Added {n} tracks") };
                            this.note(window, Notification::success(msg), cx);
                        }
                    }
                    Err(e) => this.report_error(e, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// "Add many links": the links typed or pasted into the panel's box.
    fn add_from_bulk_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.bulk_input.read(cx).value().to_string();
        self.add_link_list(&text, true, window, cx);
    }

    /// "Add many links": the links on the clipboard.
    fn add_from_clipboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bulk_progress.is_some() {
            return;
        }
        // The async read: in the browser the plain one never has anything.
        let read = cx.read_from_clipboard_async();
        cx.spawn_in(window, async move |this, cx| {
            let text = match read.await {
                Ok(item) => item.and_then(|item| item.text()).unwrap_or_default(),
                Err(e) => {
                    let _ = this.update_in(cx, |_, window, cx| {
                        let msg = format!("Couldn't read the clipboard: {e}");
                        window.push_notification(Notification::warning(msg), cx);
                    });
                    return;
                }
            };
            let _ = this.update_in(cx, |this, window, cx| this.add_link_list(&text, false, window, cx));
        })
        .detach();
    }

    /// "Add many links": the links in a text file (desktop; browsers can't
    /// open a file picker for the app).
    fn add_from_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bulk_progress.is_some() {
            return;
        }
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Add links".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match rx.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                // E.g. Linux without a file chooser portal.
                Ok(Err(e)) => {
                    let _ = this.update_in(cx, |_, window, cx| {
                        let msg = format!("Couldn't open a file picker: {e:#}");
                        window.push_notification(Notification::error(msg), cx);
                    });
                    None
                }
                _ => None,
            };
            let Some(path) = path else { return };
            let read = cx.background_spawn(async move { read_link_file(&path) }).await;
            let _ = this.update_in(cx, |this, window, cx| match read {
                Ok(text) => this.add_link_list(&text, false, window, cx),
                Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
            });
        })
        .detach();
    }

    /// Add every link in `text` (one per line) to the selected playlist, or
    /// to a new one. Links are resolved one after another and their tracks
    /// show up as they arrive; tracks the playlist has already are skipped.
    fn add_link_list(&mut self, text: &str, clear_input: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.bulk_progress.is_some() {
            return;
        }
        let sources::LinkList { links, skipped } = sources::parse_link_list(text);
        if links.is_empty() {
            let msg = if skipped > 0 { "No YouTube or SoundCloud links found." } else { "There are no links to add." };
            window.push_notification(Notification::warning(msg), cx);
            return;
        }
        if self.selected.is_none() {
            self.new_playlist(window, cx);
        }
        let Some(target) = self.selected.clone() else { return };
        let total = links.len();
        self.bulk_progress = Some((0, total));
        cx.notify();

        let (paths, ytdlp, creds) = (self.paths.clone(), self.ytdlp.clone(), self.creds.clone());
        cx.spawn(async move |this, cx| {
            let (mut added, mut present, mut failed) = (0, 0, 0);
            let mut first_error = None;
            for (i, link) in links.into_iter().enumerate() {
                let as_collection = link.is_collection();
                let (paths, ytdlp, creds) = (paths.clone(), ytdlp.clone(), creds.clone());
                let task = cx.update(|cx| sources::resolve_task(cx, link, as_collection, paths, ytdlp, creds));
                let result = task.await;
                let go_on = this.update(cx, |this, cx| {
                    this.bulk_progress = Some((i + 1, total));
                    cx.notify();
                    let Some(p) = this.library.playlist_mut(&target) else { return false };
                    match result {
                        Ok(resolved) => {
                            for track in resolved.into_tracks() {
                                let known = |t: &Track| t.platform == track.platform && t.source_id == track.source_id;
                                if p.tracks.iter().any(known) {
                                    present += 1;
                                } else {
                                    p.tracks.push(track);
                                    added += 1;
                                }
                            }
                            this.save_later();
                            true
                        }
                        Err(e) => {
                            failed += 1;
                            // The web UI lost its server session: the rest would fail too.
                            #[cfg(target_family = "wasm")]
                            let locked = e.to_string() == crate::web::api::UNAUTHORIZED;
                            #[cfg(not(target_family = "wasm"))]
                            let locked = false;
                            first_error.get_or_insert(e);
                            !locked
                        }
                    }
                });
                if !go_on.unwrap_or(false) {
                    break;
                }
            }
            Self::finish(&this, cx, move |this, mut window, cx| {
                this.bulk_progress = None;
                this.save();
                crate::mem::trim(); // resolving parsed large JSON pages
                if added == 0
                    && present == 0
                    && let Some(e) = first_error
                {
                    this.report_error(e, window, cx);
                    cx.notify();
                    return;
                }
                if clear_input
                    && failed == 0
                    && let Some(window) = window.as_deref_mut()
                {
                    this.bulk_input.update(cx, |i, cx| i.set_value("", window, cx));
                    this.bulk_open = false;
                }
                let tracks = |n: usize| if n == 1 { "1 track".to_string() } else { format!("{n} tracks") };
                let mut msg = match (added, present) {
                    (0, present) => format!("Nothing new: {} already in the playlist", tracks(present)),
                    (added, 0) => format!("Added {}", tracks(added)),
                    (added, present) => format!("Added {}, {present} already in the playlist", tracks(added)),
                };
                if skipped > 0 {
                    msg += &format!("; {skipped} line{} without a link", if skipped == 1 { "" } else { "s" });
                }
                let note = match first_error {
                    Some(e) => {
                        Notification::warning(format!("{msg}. {failed} of {total} links failed, the first with: {e:#}"))
                    }
                    None => Notification::success(msg),
                };
                this.note(window, note, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Show an error, or the server login screen if the server wants a password.
    fn report_error(&mut self, e: anyhow::Error, window: Option<&mut Window>, cx: &mut Context<Self>) {
        #[cfg(target_family = "wasm")]
        if e.to_string() == crate::web::api::UNAUTHORIZED {
            self.server_locked = true;
            cx.notify();
            return;
        }
        self.note(window, Notification::error(format!("{e:#}")), cx);
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
        self.import_link(link, true, cx);
    }

    /// Import a collection link as a new playlist.
    fn import_link(&mut self, link: Link, clear_input: bool, cx: &mut Context<Self>) {
        if self.importing {
            return;
        }
        self.importing = true;
        cx.notify();
        let (paths, ytdlp, creds) = (self.paths.clone(), self.ytdlp.clone(), self.creds.clone());
        let task = sources::resolve_task(cx, link, true, paths, ytdlp, creds);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            Self::finish(&this, cx, move |this, mut window, cx| {
                this.importing = false;
                crate::mem::trim(); // resolving parsed large JSON pages
                match result {
                    Ok(Resolved::Collection { title, tracks }) => {
                        let n = tracks.len();
                        let mut playlist = Playlist::new(title);
                        playlist.folder = Some(storage::unique_folder(&this.library, &playlist.id, &playlist.name));
                        playlist.tracks = tracks;
                        let id = playlist.id.clone();
                        this.library.playlists.push(playlist);
                        this.save();
                        match window.as_deref_mut() {
                            Some(window) => {
                                if clear_input {
                                    this.import_input.update(cx, |i, cx| i.set_value("", window, cx));
                                }
                                this.select(Some(id), window, cx);
                            }
                            // Shown when a window opens again (`attach`).
                            None => this.selected = Some(id),
                        }
                        this.note(window, Notification::success(format!("Imported {n} tracks")), cx);
                    }
                    Ok(Resolved::Track { .. }) => this.note(
                        window,
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
        self.removed_size = None;
        let _ = std::fs::remove_dir_all(self.paths.cache.join("audio")); // pre-0.2 flat cache
        window
            .push_notification(Notification::info(format!("Freed {:.1} MB of removed tracks", size as f64 / 1e6)), cx);
        cx.notify();
    }

    // ------------------------------------------------------------- playback

    /// Start (or join) the download of `track` into the playlist folder `dir`.
    fn ensure_download(&mut self, dir: PathBuf, track: &Track) -> Arc<Download> {
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
        #[cfg(feature = "desktop")]
        let paths = self.paths.clone();
        std::thread::spawn(move || {
            // Fetch the cover meanwhile, to embed it.
            #[cfg(feature = "desktop")]
            let cover = {
                let (paths, platform, id) = (paths, track.platform, track.source_id.clone());
                std::thread::spawn(move || crate::cover::get(&paths, platform, &id, crate::cover::Size::Full))
            };
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
            // Make the file self-contained for other players (best effort; never
            // fails the download): a plain MP4 rather than streamed fragments
            // (ffmpeg, when installed, has already done that), and the cover
            // inside. Both swap in a new file, so streaming playback of the old
            // one carries on undisturbed.
            #[cfg(feature = "desktop")]
            if let Ok(file) = &res {
                if let Err(e) = crate::remux::defragment_file(file) {
                    eprintln!("couldn't remux {}: {e:#}", track.title);
                }
                let embedded = cover
                    .join()
                    .map_err(|_| anyhow::anyhow!("cover thread panicked"))
                    .and_then(|c| c)
                    .and_then(|c| crate::cover::square(&c, 1000))
                    .and_then(|img| crate::cover::to_jpeg(&img))
                    .and_then(|c| crate::cover::embed(file, &c));
                if let Err(e) = embedded {
                    eprintln!("no cover art for {}: {e:#}", track.title);
                }
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
        self.local_file_with(&mut storage::FileIndex::direct(), playlist, track)
    }

    /// [`Self::local_file`], finding files through `index`.
    fn local_file_with(&self, index: &mut storage::FileIndex, playlist: &Playlist, track: &Track) -> Option<PathBuf> {
        let dir = storage::playlist_dir(&self.music_root(), playlist);
        if self.downloads.contains_key(&dir.join(track.file_key())) {
            return None;
        }
        storage::materialize_with(index, &self.paths, &self.library, playlist, track)
    }

    fn play(&mut self, playlist_id: &str, track_id: &str, cx: &mut Context<Self>) {
        let Some(playlist) = self.library.playlist(playlist_id) else { return };
        let Some(track) = playlist.tracks.iter().find(|t| t.id == track_id).cloned() else { return };
        if self.audio.is_none() {
            self.notify_error("No audio output device available");
            return;
        }
        #[cfg(not(target_family = "wasm"))]
        let (dir, local) = (storage::playlist_dir(&self.music_root(), playlist), self.local_file(playlist, &track));
        let new_playlist = self.now.as_ref().is_none_or(|n| n.playlist_id != playlist_id);
        self.now = Some(NowPlaying { playlist_id: playlist_id.to_string(), track: track.clone() });
        if new_playlist || self.shuffle_order.is_empty() {
            self.reshuffle(track_id);
        }
        self.on_disk.clear(); // the lookup may have restored or copied a file
        #[cfg(not(target_family = "wasm"))]
        let input = match local {
            Some(path) => AudioInput::File(path),
            None => AudioInput::Stream(self.ensure_download(dir, &track)),
        };
        #[cfg(target_family = "wasm")]
        let input = AudioInput::Url(crate::web::api::audio_url(&track));
        if let Some(a) = self.audio.as_mut() {
            a.load(input);
        }
        self.state = PlayState::Playing;
        self.last_skip = None;
        self.load_segments(&track, cx);
        self.load_cover(&track, cx);
        self.refresh_lyrics(cx);
        if !crate::WEB {
            self.prefetch_next(cx);
        }
        cx.notify();
    }

    /// Show the track's cover in the visualizer (fetched in the background).
    fn load_cover(&mut self, track: &Track, cx: &mut Context<Self>) {
        let key = (track.platform, track.source_id.clone());
        if self.cover_for.as_ref() == Some(&key) {
            return;
        }
        self.cover_for = Some(key.clone());
        if let Some(old) = self.now_cover.take() {
            self.stale_covers.push((old, crate::now_ms()));
        }
        self.visualizer.update(cx, |v, cx| {
            v.set_cover(None);
            cx.notify();
        });
        let paths = self.paths.clone();
        cx.spawn(async move |this, cx| {
            let (platform, id) = key.clone();
            let image = fetch_cover(cx, paths, platform, id, crate::cover::Size::Full, 640, true).await;
            let _ = this.update(cx, |this, cx| {
                if this.cover_for.as_ref() != Some(&key) {
                    return; // another track started meanwhile
                }
                match image {
                    Ok((image, accent)) => {
                        this.cover_accent = accent;
                        this.apply_cover_colors(cx);
                        this.now_cover = Some(image.clone());
                        this.visualizer.update(cx, |v, cx| {
                            v.set_cover(Some(image));
                            cx.notify();
                        });
                        cx.notify();
                    }
                    Err(e) => {
                        eprintln!("no cover art for {}: {e:#}", key.1);
                        // Try again next time the track plays (e.g. back online).
                        this.cover_for = None;
                    }
                }
            });
        })
        .detach();
    }

    /// Theme accent from the playing track's cover, or the stock one when the
    /// setting is off or the cover is grey; faded in over a moment.
    fn apply_cover_colors(&mut self, cx: &mut Context<Self>) {
        const FADE_MS: f64 = 400.0;
        const FRAME_MS: u64 = 25;
        let target = self.cover_accent.filter(|_| self.library.settings.cover_colors);
        if target == self.accent_shown {
            return;
        }
        // A fade still running is dropped (cancelled); the new one starts from
        // its target.
        let from = std::mem::replace(&mut self.accent_shown, target);
        if cx.reduce_motion() {
            self.accent_fade = None;
            crate::theme::set_accent(from, target, 1.0, self.library.settings.appearance, cx);
            return;
        }
        let start = crate::now_ms();
        self.accent_fade = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(FRAME_MS)).await;
                let p = ((crate::now_ms() - start) / FADE_MS).min(1.0) as f32;
                let eased = 1.0 - (1.0 - p).powi(3);
                let updated = this.update(cx, |this, cx| {
                    crate::theme::set_accent(from, target, eased, this.library.settings.appearance, cx)
                });
                if updated.is_err() || p >= 1.0 {
                    break;
                }
            }
        }));
    }

    // ---------------------------------------------------------- sponsorblock

    /// Fetch the track's SponsorBlock segments in the background unless we
    /// have fresh ones (stale ones stay in use if the fetch fails, e.g. offline).
    fn load_segments(&mut self, track: &Track, cx: &mut Context<Self>) {
        // Re-check now and then: segments keep being submitted and voted on.
        const MAX_AGE_MS: f64 = 3.0 * 24.0 * 3600.0 * 1000.0;
        if track.platform != Platform::YouTube || !self.library.settings.sponsorblock {
            return;
        }
        let id = track.source_id.clone();
        let fresh = self.segments.get(&id).is_some_and(|(at, _)| crate::now_ms() - at < MAX_AGE_MS);
        if fresh || !self.segments_loading.insert(id.clone()) {
            return;
        }
        cx.spawn(async move |this, cx| {
            #[cfg(not(target_family = "wasm"))]
            let res = {
                let id = id.clone();
                cx.background_spawn(async move { crate::sponsorblock::fetch(&id) }).await
            };
            #[cfg(target_family = "wasm")]
            let res = crate::web::api::segments(&id).await;
            let _ = this.update(cx, |this, _| {
                this.segments_loading.remove(&id);
                match res {
                    Ok(segs) => {
                        this.segments.insert(id, (crate::now_ms(), segs));
                        this.save_segments();
                    }
                    Err(e) => eprintln!("SponsorBlock lookup failed for {id}: {e:#}"),
                }
            });
        })
        .detach();
    }

    fn save_segments(&self) {
        #[cfg(not(target_family = "wasm"))]
        {
            use crate::sponsorblock::cache;
            let map = self
                .segments
                .iter()
                .map(|(id, (at, segs))| (id.clone(), cache::Entry { fetched_ms: *at, segments: segs.clone() }))
                .collect();
            cache::save(&cache::file(&self.paths), &map);
        }
    }

    /// Jump over a SponsorBlock segment if playback is inside one.
    fn skip_segments(&mut self, cx: &mut Context<Self>) {
        let settings = &self.library.settings;
        if !settings.sponsorblock || self.seeking.is_some() {
            return;
        }
        let (Some(now), Some(audio)) = (&self.now, &self.audio) else {
            return;
        };
        if now.track.platform != Platform::YouTube || !audio.is_loaded() || audio.is_buffering() {
            return;
        }
        let Some((_, segs)) = self.segments.get(&now.track.source_id) else {
            return;
        };
        let pos = audio.position().as_secs_f64();
        let Some(target) = crate::sponsorblock::skip_target(segs, pos) else {
            return;
        };
        let t = crate::now_ms();
        if self.last_skip.is_some_and(|(prev, at)| (prev - target).abs() < 0.01 && t - at < 2000.0) {
            return;
        }
        self.last_skip = Some((target, t));
        let total = self.total_duration().map(|d| d.as_secs_f64());
        if total.is_some_and(|total| target >= total - 1.0) {
            // The segment runs to the end: go straight to the next track.
            self.skip(1, true, cx);
        } else {
            audio.seek(Duration::from_secs_f64(target));
        }
    }

    fn prefetch_next(&mut self, cx: &mut Context<Self>) {
        let Some(next) = self.neighbor(1, false) else { return };
        self.load_segments(&next, cx);
        let Some(playlist) = self.now.as_ref().and_then(|n| self.library.playlist(&n.playlist_id)) else { return };
        if self.local_file(playlist, &next).is_none() {
            let dir = storage::playlist_dir(&self.music_root(), playlist);
            self.ensure_download(dir, &next);
        }
    }

    /// Queue every track of the playlist that isn't on disk yet.
    fn download_all(&mut self, playlist_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(playlist) = self.library.playlist(playlist_id) else { return };
        let already: std::collections::HashSet<&str> = self.download_queue.iter().map(|(_, id)| id.as_str()).collect();
        // Each folder is listed once for the whole playlist, not once per track.
        let mut index = storage::FileIndex::cached();
        let missing: Vec<String> = playlist
            .tracks
            .iter()
            .filter(|t| !already.contains(t.id.as_str()) && self.local_file_with(&mut index, playlist, t).is_none())
            .map(|t| t.id.clone())
            .collect();
        let dir = storage::playlist_dir(&self.music_root(), playlist);
        let queued = missing.len();
        self.download_queue.extend(missing.into_iter().map(|tid| (playlist_id.to_string(), tid)));
        self.on_disk.clear(); // the lookups may have restored or copied files
        if queued > 0 {
            self.batches.entry(playlist_id.to_string()).or_default().total += queued;
        }
        let msg = if queued == 0 {
            "All tracks are already downloaded".to_string()
        } else {
            format!("Downloading {queued} tracks to {}", dir.display())
        };
        window.push_notification(Notification::info(msg), cx);
        cx.notify();
    }

    /// Keep up to two queued downloads running.
    fn pump_queue(&mut self) {
        const PARALLEL: usize = 2;
        while self.downloads.len() < PARALLEL {
            let Some((pid, tid)) = self.download_queue.pop_front() else { break };
            let Some(playlist) = self.library.playlist(&pid) else { continue };
            let Some(track) = playlist.tracks.iter().find(|t| t.id == tid).cloned() else {
                // Removed from the playlist meanwhile.
                if let Some(b) = self.batches.get_mut(&pid) {
                    b.total = b.total.saturating_sub(1);
                }
                continue;
            };
            if self.local_file(playlist, &track).is_none() {
                let dir = storage::playlist_dir(&self.music_root(), playlist);
                let key = dir.join(track.file_key());
                self.ensure_download(dir, &track);
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
        if !self.on_disk.contains_key(dir) {
            let keys = std::fs::read_dir(dir)
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
                .unwrap_or_default();
            self.on_disk.insert(dir.to_path_buf(), keys);
        }
        &self.on_disk[dir]
    }

    /// Indices into the selected playlist's tracks of the rows a search for
    /// `query` shows, or `None` without a search (every row). Cached: the list
    /// is drawn several times a second, and matching lowercases every track.
    fn filtered_rows(&mut self, query: &str) -> Option<Rc<Vec<usize>>> {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if terms.is_empty() {
            return None;
        }
        let pid = self.selected.as_deref()?;
        if let Some(c) = &self.filter_cache
            && c.playlist_id == pid
            && c.query == query
            && c.library_rev == self.library_rev
        {
            return Some(c.rows.clone());
        }
        let p = self.library.playlist(pid)?;
        let rows: Rc<Vec<usize>> =
            Rc::new(p.tracks.iter().enumerate().filter(|(_, t)| t.matches(&terms)).map(|(ix, _)| ix).collect());
        self.filter_cache = Some(FilterCache {
            playlist_id: pid.to_string(),
            query: query.to_string(),
            library_rev: self.library_rev,
            rows: rows.clone(),
        });
        Some(rows)
    }

    /// Rename a playlist's folder to follow its name (debounced, skipped while downloading into it).
    fn apply_folder_rename(&mut self) {
        let Some((id, at)) = self.pending_folder_rename.clone() else { return };
        if crate::now_ms() - at < 1200.0 {
            return;
        }
        let root = self.music_root();
        let Some(p) = self.library.playlist(&id) else {
            self.pending_folder_rename = None;
            return;
        };
        let old_dir = storage::playlist_dir(&root, p);
        if self.downloads.keys().any(|k| k.starts_with(&old_dir)) {
            return; // retry when downloads into it are done
        }
        let folder = storage::unique_folder(&self.library, &id, &p.name);
        let unchanged = p.folder.as_deref() == Some(folder.as_str());
        self.pending_folder_rename = None;
        if unchanged {
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
                if a.is_paused() {
                    a.play()
                } else {
                    a.pause()
                }
            }
            _ => {
                // Start the selected playlist from the top (or resume last track).
                let target = match &self.now {
                    Some(n) => Some((n.playlist_id.clone(), n.track.id.clone())),
                    None => {
                        self.selected_playlist().and_then(|p| p.tracks.first().map(|t| (p.id.clone(), t.id.clone())))
                    }
                };
                if let Some((pid, tid)) = target {
                    self.play(&pid, &tid, cx);
                }
            }
        }
        cx.notify();
    }

    /// Play order of playlist `p` (track entry ids).
    fn order<'a>(&'a self, p: &'a Playlist) -> Vec<&'a str> {
        if !self.library.settings.shuffle {
            return p.tracks.iter().map(|t| t.id.as_str()).collect();
        }
        // Keep shuffle order, dropping removed tracks and appending new ones.
        let present: std::collections::HashSet<&str> = p.tracks.iter().map(|t| t.id.as_str()).collect();
        let mut order: Vec<&str> =
            self.shuffle_order.iter().map(String::as_str).filter(|id| present.contains(id)).collect();
        if order.len() < p.tracks.len() {
            let shuffled: std::collections::HashSet<&str> = order.iter().copied().collect();
            order.extend(p.tracks.iter().map(|t| t.id.as_str()).filter(|id| !shuffled.contains(id)));
        }
        order
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
        let p = self.library.playlist(&now.playlist_id)?;
        let order = self.order(p);
        let pos = order.iter().position(|id| *id == now.track.id)? as isize;
        let mut next = pos + delta;
        let len = order.len() as isize;
        if next < 0 || next >= len {
            if !wrap || len == 0 {
                return None;
            }
            next = next.rem_euclid(len);
        }
        let id = order[next as usize];
        p.tracks.iter().find(|t| t.id == id).cloned()
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

    /// Playback position in milliseconds.
    fn position_ms(&self) -> u32 {
        self.audio.as_ref().map_or(0, |a| a.position().as_millis().min(u32::MAX as u128) as u32)
    }

    fn seek_to(&mut self, to: Duration, cx: &mut Context<Self>) {
        if let Some(audio) = &self.audio
            && self.state == PlayState::Playing
        {
            audio.seek(to);
            self.last_skip = None;
            cx.notify();
        }
    }

    /// Set (or with `None` clear) the lyrics offset of the song with file key `key`.
    fn set_lyrics_offset(&mut self, key: &str, offset_ms: Option<i32>, cx: &mut Context<Self>) {
        match offset_ms {
            Some(ms) => self.library.lyrics_offsets.insert(key.to_string(), ms),
            None => self.library.lyrics_offsets.remove(key),
        };
        // Saved once the clicking stops.
        self.save_later();
        cx.notify();
    }

    /// Show or hide a page about what's playing (toggles; from Settings it
    /// goes back to the playlist page).
    fn toggle_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if self.view == View::Playlist && self.panel == panel {
            self.panel = Panel::Tracks;
        } else {
            self.view = View::Playlist;
            self.panel = panel;
        }
        self.refresh_lyrics(cx);
        cx.notify();
    }

    /// Lyrics are looked up only while their page is shown (they tell the
    /// lyrics services what's playing): not with the window closed either.
    fn refresh_lyrics(&mut self, cx: &mut Context<Self>) {
        if self.panel != Panel::Lyrics || self.window.is_none() {
            return;
        }
        let track = self.now.as_ref().map(|n| n.track.clone());
        let paths = self.paths.clone();
        self.lyrics.update(cx, |v, cx| v.show(track.as_ref(), &paths, false, cx));
    }

    fn total_duration(&self) -> Option<Duration> {
        // Metadata first: while streaming, players only know the length received so far.
        self.now
            .as_ref()
            .and_then(|n| n.track.duration_secs.map(|s| Duration::from_secs(s as u64)))
            .or_else(|| self.audio.as_ref().and_then(|a| a.duration()))
    }

    /// Five times a second, with or without a window: playback, downloads,
    /// saving.
    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.installing.is_some() {
            cx.notify();
        }

        // Collect finished downloads.
        let finished: Vec<(PathBuf, Result<PathBuf, String>)> =
            self.downloads.iter().filter_map(|(k, d)| d.result().map(|r| (k.clone(), r))).collect();
        if !finished.is_empty() {
            // Remuxing and tagging the file allocated a lot on its thread.
            crate::mem::trim();
        }
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
            self.pending_notes.push(note);
        }
        self.pump_queue();
        self.apply_folder_rename();
        self.save_if_due();

        // Player events.
        if let Some(err) = self.audio.as_mut().and_then(|a| a.poll()) {
            self.state = PlayState::Idle;
            cx.notify();
            let title = self.now.as_ref().map(|n| n.track.title.clone()).unwrap_or_default();
            self.pending_notes.push(Notification::error(format!("Couldn't play \"{title}\": {err}")).autohide(false));
        }
        if self.state == PlayState::Playing {
            if self.audio.as_ref().is_some_and(|a| a.finished()) {
                self.skip(1, true, cx);
            } else if self.seeking.is_none() {
                self.skip_segments(cx);
            }
            // While a track just plays, only the clock moves: redraw the seek row
            // alone. The whole window when something else shown changes, and
            // while downloads run (their progress shows in the list).
            let shown = (self.is_buffering(), self.audio.as_ref().is_some_and(|a| a.is_paused()));
            if shown != self.last_shown || !self.downloads.is_empty() {
                self.last_shown = shown;
                cx.notify();
            } else {
                self.progress.update(cx, |_, cx| cx.notify());
            }
        } else if !self.downloads.is_empty() {
            cx.notify();
        }
        // Another start of the app asked for the window (see `instance`).
        #[cfg(feature = "desktop")]
        if crate::instance::show_requested(&self.paths) {
            let _ = self.tray_commands.unbounded_send(crate::tray::Command::ShowWindow);
        }
        // Without a window, keep only the latest notifications.
        if self.window.is_none() {
            let excess = self.pending_notes.len().saturating_sub(5);
            self.pending_notes.drain(..excess);
        }
        #[cfg(feature = "desktop")]
        {
            self.sync_tray();
            self.sync_presence(cx);
        }
    }

    /// What needs the window: notifications, the seek slider, and freeing
    /// the textures of replaced covers.
    fn tick_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for note in self.pending_notes.drain(..) {
            window.push_notification(note, cx);
        }
        // Free replaced covers' textures (each up to 640² px, 1.6 MB).
        if !self.stale_covers.is_empty() {
            let now = crate::now_ms();
            let (old, keep): (Vec<_>, Vec<_>) = self.stale_covers.drain(..).partition(|(_, at)| now - at > 500.0);
            self.stale_covers = keep;
            for (img, _) in old {
                cx.drop_image(img, Some(window));
            }
        }
        if self.state == PlayState::Playing
            && self.seeking.is_none()
            && let Some(total) = self.total_duration().filter(|d| !d.is_zero())
        {
            let pos = self.audio.as_ref().map(|a| a.position()).unwrap_or_default();
            let frac = (pos.as_secs_f32() / total.as_secs_f32()).clamp(0., 1.);
            self.seek.update(cx, |s, cx| s.set_value(frac, window, cx));
        }
    }
}

// ------------------------------------------------------------ background

/// Open the app's window (desktop start).
#[cfg(feature = "desktop")]
pub fn open(paths: Paths, cx: &mut App) -> Entity<LocalPlaylist> {
    let (_, main) = gpui_kit::open_window(crate::window_options(cx), cx, |window, cx| {
        cx.new(|cx| LocalPlaylist::new(paths, window, cx))
    })
    .expect("failed to open window");
    main
}

/// The window was closed: keep playing in the background if the tray can
/// bring it back, otherwise quit.
#[cfg(feature = "desktop")]
pub fn window_closed(main: &Entity<LocalPlaylist>, cx: &mut App) {
    let keep = main.update(cx, |this, _| {
        this.detach();
        this.sync_tray();
        this.runs_in_background()
    });
    if !keep {
        cx.quit();
    }
}

/// Open a window around the running app again.
#[cfg(feature = "desktop")]
fn reopen(main: &Entity<LocalPlaylist>, cx: &mut App) {
    if main.read(cx).window.is_some() {
        return;
    }
    let view = main.clone();
    let opened = gpui_kit::open_window(crate::window_options(cx), cx, move |window, cx| {
        view.update(cx, |this, cx| this.attach(window, cx));
        view
    });
    match opened {
        Ok(_) => cx.activate(true),
        Err(e) => eprintln!("couldn't open the window: {e:#}"),
    }
}

#[cfg(feature = "desktop")]
fn tray_command(main: &Entity<LocalPlaylist>, command: crate::tray::Command, cx: &mut App) {
    use crate::tray::Command;
    let window = main.read(cx).window;
    match command {
        Command::ToggleWindow | Command::ShowWindow if window.is_none() => reopen(main, cx),
        Command::ToggleWindow | Command::ShowWindow | Command::HideWindow => {
            if let Some(handle) = window {
                let _ = handle.update(cx, |_, window, _| {
                    // A click hides the window when it's in front, else raises it.
                    let hide =
                        command == Command::HideWindow || command == Command::ToggleWindow && window.is_window_active();
                    if hide { window.remove_window() } else { window.activate_window() }
                });
            }
        }
        Command::PlayPause => main.update(cx, |this, cx| this.toggle_play(cx)),
        Command::Next => main.update(cx, |this, cx| this.skip(1, false, cx)),
        Command::Previous => main.update(cx, |this, cx| this.previous(cx)),
        Command::Quit => cx.quit(),
        Command::TrayGone => {
            main.update(cx, |this, _| this.tray_shown = false);
            // Nothing could bring a closed window back now.
            reopen(main, cx);
        }
        Command::TrayBack => main.update(cx, |this, _| this.tray_shown = true),
    }
}

/// Fetch a track's cover (cached on disk; off the UI thread on the desktop) and
/// prepare it for display at up to `max_px` pixels, with its dominant colour
/// if `accent`.
async fn fetch_cover(
    cx: &mut AsyncApp,
    paths: Paths,
    platform: Platform,
    id: String,
    size: crate::cover::Size,
    max_px: u32,
    accent: bool,
) -> anyhow::Result<CoverImage> {
    #[cfg(not(target_family = "wasm"))]
    return cx
        .background_spawn(async move {
            crate::cover::get(&paths, platform, &id, size).and_then(|b| cover_image(&b, max_px, accent))
        })
        .await;
    #[cfg(target_family = "wasm")]
    {
        let _ = (cx, paths);
        crate::web::api::cover(platform, &id, size).await.and_then(|b| cover_image(&b, max_px, accent))
    }
}

/// A decoded cover and its dominant (hue, saturation).
type CoverImage = (Arc<RenderImage>, Option<(f32, f32)>);

/// A cover ready for display (bars trimmed, square, decoded for GPUI) and, if
/// `accent`, its dominant colour.
fn cover_image(bytes: &[u8], max_px: u32, accent: bool) -> anyhow::Result<CoverImage> {
    let mut img = crate::cover::square(bytes, max_px)?;
    let accent = if accent { crate::cover::dominant_hue(&img) } else { None };
    // GPUI wants BGRA.
    for p in img.pixels_mut() {
        p.0.swap(0, 2);
    }
    Ok((Arc::new(RenderImage::new(vec![image::Frame::new(img)])), accent))
}

/// Small xorshift shuffle; no need for cryptographic randomness.
/// Read a list of links. Lists are small; a size limit keeps a wrongly picked
/// file (a video, a disk image) from being read into memory.
fn read_link_file(path: &std::path::Path) -> anyhow::Result<String> {
    use anyhow::Context as _;
    const MAX: u64 = 4 << 20;
    let size = std::fs::metadata(path).with_context(|| format!("Couldn't open {}", path.display()))?.len();
    if size > MAX {
        anyhow::bail!("{} is too big for a list of links ({} MB)", path.display(), size >> 20);
    }
    let bytes = std::fs::read(path).with_context(|| format!("Couldn't read {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

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
    fn render_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let branded_title_bar = Self::has_title_bar(window);
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
        let import_open = self.import_open;
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
                    .child(if branded_title_bar {
                        // The app name is in the title bar already.
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(theme.muted_foreground)
                            .child("PLAYLISTS")
                            .into_any_element()
                    } else {
                        h_flex()
                            .gap_2()
                            .child(Icon::new(IconName::Disc3).text_color(theme.primary))
                            .child(div().font_semibold().child("Local Playlist"))
                            .into_any_element()
                    })
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
                    .child(
                        h_flex()
                            .id("import-toggle")
                            .justify_between()
                            .cursor_pointer()
                            .text_xs()
                            .font_semibold()
                            .text_color(theme.muted_foreground)
                            .hover(|d| d.text_color(theme.sidebar_foreground))
                            .child("IMPORT PLAYLIST")
                            .child(
                                Icon::new(if import_open { IconName::ChevronDown } else { IconName::ChevronRight })
                                    .xsmall(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.import_open = !this.import_open;
                                cx.notify();
                            })),
                    )
                    .when(import_open, |d| {
                        d.child(Input::new(&self.import_input).small().cleanable(true))
                            .child(
                                Button::new("import")
                                    .small()
                                    .icon(IconName::Import)
                                    .label(if self.importing { "Importing…" } else { "Import as new playlist" })
                                    .loading(self.importing)
                                    .on_click(cx.listener(|this, _, window, cx| this.import_from_input(window, cx))),
                            )
                            .child(div().text_xs().text_color(theme.muted_foreground).child(
                                "YouTube playlists, SoundCloud sets, profiles and /likes. Private lists need a login (Settings).",
                            ))
                    }),
            )
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .when_some(components_label, |d, label| {
                        d.child(
                            h_flex()
                                .gap_1()
                                .text_xs()
                                .text_color(if setup_needed { theme.warning } else { theme.muted_foreground })
                                .when(setup_needed, |d| d.child(Icon::new(IconName::CircleAlert).xsmall()))
                                .child(div().truncate().child(label)),
                        )
                    })
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
        let query = self.search_input.read(cx).value().to_string();
        // Indices into `playlist.tracks` of the rows shown (`None` = all).
        let visible = self.filtered_rows(&query);
        self.list_rows = visible.clone();
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
        let filtering = visible.is_some();
        let shown = visible.as_ref().map_or(count, |v| v.len());
        let pid = playlist.id.clone();
        let playlist_id_for_dl = playlist.id.clone();

        let list = uniform_list(
            "tracks",
            shown,
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                let theme = cx.theme().clone();
                let Some(dir) = this.selected_playlist().map(|p| storage::playlist_dir(&this.music_root(), p)) else {
                    return Vec::new();
                };
                let Some(p) = this.selected_playlist() else { return Vec::new() };
                let now_id = this.now.as_ref().map(|n| n.track.id.clone());
                let loading = this.is_buffering();
                let paused = this.audio.as_ref().is_some_and(|a| a.is_paused());
                let rows: Vec<(usize, Track)> = range
                    .filter_map(|row| {
                        let ix = match &visible {
                            Some(v) => *v.get(row)?,
                            None => row,
                        };
                        p.tracks.get(ix).map(|t| (ix, t.clone()))
                    })
                    .collect();
                let playlist_id = p.id.clone();
                // Each row's file target (`<dir>/<key>`) and whether it's on disk.
                let keys: Vec<String> = rows.iter().map(|(_, t)| t.file_key()).collect();
                let on_disk: Vec<bool> = if crate::WEB {
                    vec![false; rows.len()]
                } else {
                    let keys_on_disk = this.disk_keys(&dir);
                    keys.iter().map(|k| keys_on_disk.contains(k)).collect()
                };
                let row_thumbs: Vec<_> = rows.iter().map(|(_, t)| this.thumb(t, cx)).collect();
                rows.into_iter()
                    .zip(row_thumbs)
                    .zip(keys.into_iter().zip(on_disk))
                    .map(|(((ix, t), thumb), (key, downloaded))| {
                        let current = now_id.as_deref() == Some(&t.id);
                        let (pid, tid) = (playlist_id.clone(), t.id.clone());
                        let (tid_up, tid_down, tid_rm) = (t.id.clone(), t.id.clone(), t.id.clone());
                        let target = dir.join(&key);
                        let progress = this.downloads.get(&target).map(|d| d.progress());
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
                            .child(div().w(px(28.)).flex_none().text_sm().text_color(theme.muted_foreground).child(
                                if current && loading {
                                    Icon::new(IconName::LoaderCircle)
                                        .small()
                                        .text_color(theme.primary)
                                        .into_any_element()
                                } else if current && !paused {
                                    Icon::new(IconName::Volume2).small().text_color(theme.primary).into_any_element()
                                } else {
                                    (ix + 1).to_string().into_any_element()
                                },
                            ))
                            .child(thumbs::cover_tile(thumb, px(36.), &theme))
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
                                        div().text_xs().truncate().text_color(theme.muted_foreground).child(
                                            if t.artist.is_empty() { "—".to_string() } else { t.artist.clone() },
                                        ),
                                    ),
                            )
                            .child(
                                // Download status: progress, error, or on disk.
                                div().w(px(36.)).flex_none().flex().justify_end().child(if let Some(p) = progress {
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("{:.0}%", p * 100.))
                                        .into_any_element()
                                } else if let Some(err) = this.download_errors.get(&target).cloned() {
                                    div()
                                        .id(("dl-err", ix))
                                        .child(Icon::new(IconName::CircleAlert).small().text_color(theme.warning))
                                        .tooltip(move |window, cx| {
                                            gpui_kit::component::tooltip::Tooltip::new(format!(
                                                "Download failed: {err}"
                                            ))
                                            .build(window, cx)
                                        })
                                        .into_any_element()
                                } else if downloaded {
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
                                }),
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
                                    .when(!filtering, |d| {
                                        d.child(
                                            Button::new(("up", ix)).ghost().xsmall().icon(IconName::ArrowUp).on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.move_track(&tid_up, -1, cx);
                                                }),
                                            ),
                                        )
                                    })
                                    .when(!filtering, |d| {
                                        d.child(
                                            Button::new(("down", ix))
                                                .ghost()
                                                .xsmall()
                                                .icon(IconName::ArrowDown)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.move_track(&tid_down, 1, cx);
                                                })),
                                        )
                                    })
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
                        h_flex()
                            .gap_3()
                            .justify_between()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .when(filtering, |d| d.child(format!("{shown} of ")))
                                    .child(format!(
                                        "{count} track{}{}",
                                        if count == 1 { "" } else { "s" },
                                        if total > 0 {
                                            format!(" · {}", format_duration(total))
                                        } else {
                                            String::new()
                                        }
                                    )),
                            )
                            .when(count > 0, |d| {
                                d.child(
                                    div().w(px(260.)).flex_shrink(1.).child(
                                        Input::new(&self.search_input)
                                            .small()
                                            .prefix(
                                                Icon::new(IconName::Search).small().text_color(theme.muted_foreground),
                                            )
                                            .cleanable(true),
                                    ),
                                )
                            }),
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
                            )
                            .child(
                                Button::new("bulk-toggle")
                                    .icon(IconName::ListMusic)
                                    .when(self.bulk_open, |b| b.primary())
                                    .when(!self.bulk_open, |b| b.ghost())
                                    .tooltip("Add many links (a list, the clipboard or a text file)")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.bulk_open = !this.bulk_open;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when(self.bulk_open, |d| d.child(self.render_bulk_add(&theme, cx))),
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
                    .when(count > 0 && shown == 0, |d| {
                        d.child(
                            v_flex()
                                .size_full()
                                .items_center()
                                .justify_center()
                                .text_color(theme.muted_foreground)
                                .child("No tracks match your search."),
                        )
                    })
                    .when(shown > 0, |d| {
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

    /// "Add many links": a box for a list of links, or take them from the
    /// clipboard or a text file.
    fn render_bulk_add(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.bulk_progress.is_some();
        let label = match self.bulk_progress {
            Some((done, total)) => format!("Adding {done} of {total}…"),
            None => "Add links".to_string(),
        };
        v_flex()
            .gap_2()
            .child(Textarea::new(&self.bulk_input).h(px(110.)).disabled(busy))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("bulk-add")
                            .small()
                            .primary()
                            .icon(IconName::Plus)
                            .label(label)
                            .loading(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.add_from_bulk_input(window, cx))),
                    )
                    .child(
                        Button::new("bulk-clipboard")
                            .small()
                            .icon(IconName::Copy)
                            .label("From clipboard")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.add_from_clipboard(window, cx))),
                    )
                    .when(!crate::WEB, |d| {
                        d.child(
                            Button::new("bulk-file")
                                .small()
                                .icon(IconName::FileText)
                                .label("From file…")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| this.add_from_file(window, cx))),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Tracks and playlists are added here; ones already in the playlist are skipped."),
                    ),
            )
            .into_any_element()
    }

    /// The visualizer page: what's playing, over the visualizer.
    fn render_visualizer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        use visualizer::Style;
        let theme = cx.theme().clone();
        let style = self.visualizer.read(cx).style;
        let style_button = |id: &'static str, icon: IconName, tip: &'static str, s: Style| {
            Button::new(id).ghost().small().icon(icon).toggled(style == s).tooltip(tip).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.visualizer.update(cx, |v, cx| {
                        v.style = s;
                        cx.notify();
                    });
                },
            ))
        };
        // The visualizer fills the page, with the title over it.
        div().size_full().relative().child(div().absolute().inset_0().child(self.visualizer.clone())).child(
            h_flex()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .p_6()
                .gap_4()
                .child(
                    match &self.now {
                        Some(n) => v_flex()
                            .min_w_0()
                            .child(div().text_xl().font_semibold().truncate().child(n.track.title.clone()))
                            .child(div().truncate().text_color(theme.muted_foreground).child(n.track.artist.clone())),
                        None => v_flex().child(div().text_color(theme.muted_foreground).child("Nothing playing")),
                    }
                    .flex_1(),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap_1()
                        .child(style_button("vis-circle", IconName::Disc3, "Circle", Style::Circle))
                        .child(style_button("vis-bars", IconName::ChartColumn, "Bars", Style::Bars)),
                ),
        )
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
        // Drawn several times a second: checked without allocating per track,
        // stopping at the first missing one.
        let dir = self.library.playlist(playlist_id).map(|p| storage::playlist_dir(&self.music_root(), p));
        let all_on_disk = dir.is_some_and(|dir| {
            self.disk_keys(&dir);
            let (Some(on_disk), Some(p)) = (self.on_disk.get(&dir), self.library.playlist(playlist_id)) else {
                return false;
            };
            let mut key = String::new();
            !p.tracks.is_empty()
                && p.tracks.iter().all(|t| {
                    t.write_file_key(&mut key);
                    on_disk.contains(&key)
                })
        });
        Button::new("download-all")
            .ghost()
            .small()
            .icon(if all_on_disk { IconName::CircleCheck } else { IconName::HardDriveDownload })
            .tooltip(if all_on_disk { "All tracks are downloaded" } else { "Download all tracks for offline playback" })
            .disabled(count == 0)
            .on_click(cx.listener(move |this, _, window, cx| this.download_all(&pid, window, cx)))
            .into_any_element()
    }

    fn render_player(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // The bar touches the window's bottom corners, which may be rounded.
        let radii = gpui_kit::component::window_corner_radii(window, cx);
        let settings = self.library.settings.clone();
        let playing = matches!(self.state, PlayState::Playing) && self.audio.as_ref().is_some_and(|a| !a.is_paused());
        let buffering = self.is_buffering();
        let download = self.current_download();
        let loading = buffering.then(|| download.unwrap_or(0.));

        let info = match &self.now {
            Some(n) => v_flex().min_w_0().child(div().truncate().font_medium().child(n.track.title.clone())).child(
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
        // The full cover once loaded; the list thumbnail until then.
        let now_track = self.now.as_ref().map(|n| n.track.clone());
        let cover = self.now_cover.clone().or_else(|| now_track.and_then(|t| self.thumb(&t, cx)));
        let info = h_flex()
            .gap_3()
            .min_w_0()
            .when(self.now.is_some(), |d| d.child(thumbs::cover_tile(cover, px(52.), &theme)))
            .child(info.min_w_0().flex_1());

        h_flex()
            .h(px(84.))
            .flex_none()
            .px_4()
            .gap_4()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .rounded_bl(radii.bottom_left)
            .rounded_br(radii.bottom_right)
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
                    // Its own view, so the clock ticking redraws just this row.
                    .child(div().w_full().max_w(px(640.)).child(self.progress.clone())),
            )
            .child(
                h_flex()
                    .w(px(244.))
                    .flex_none()
                    .gap_2()
                    .child(
                        Button::new("lyrics")
                            .ghost()
                            .small()
                            .icon(IconName::MicVocal)
                            .toggled(self.panel == Panel::Lyrics && self.view == View::Playlist)
                            .tooltip("Lyrics")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_panel(Panel::Lyrics, cx))),
                    )
                    .child(
                        Button::new("visualizer")
                            .ghost()
                            .small()
                            .icon(IconName::AudioLines)
                            .toggled(self.panel == Panel::Visualizer && self.view == View::Playlist)
                            .tooltip("Visualizer")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_panel(Panel::Visualizer, cx))),
                    )
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
        let radii = gpui_kit::component::window_corner_radii(window, cx);
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .rounded_tl(radii.top_left)
            .rounded_tr(radii.top_right)
            .rounded_bl(radii.bottom_left)
            .rounded_br(radii.bottom_right)
            .when(Self::has_title_bar(window), |d| d.child(self.render_title_bar(cx)))
            .child(
                h_flex().flex_1().min_h_0().child(self.render_sidebar(window, cx)).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .when_some(self.render_setup_banner(cx), |d, banner| d.child(banner))
                        .child(div().flex_1().min_h_0().child(match self.view {
                            View::Playlist => match self.panel {
                                Panel::Visualizer => self.render_visualizer(cx).into_any_element(),
                                Panel::Lyrics => self.lyrics.clone().into_any_element(),
                                Panel::Tracks => self.render_main(window, cx).into_any_element(),
                            },
                            View::Settings => self.render_settings(window, cx).into_any_element(),
                        })),
                ),
            )
            .child(self.render_player(window, cx))
            .into_any_element()
    }
}

impl LocalPlaylist {
    /// Whether the app draws its own title bar: always on the desktop, except on
    /// Linux when the window manager kept server-side decorations (X11 without a
    /// compositor, some tiling compositors) and so already draws one.
    fn has_title_bar(window: &Window) -> bool {
        if crate::WEB {
            return false;
        }
        !cfg!(target_os = "linux") || matches!(window.window_decorations(), Decorations::Client { .. })
    }

    /// Custom title bar: app name, the playing track, and window controls.
    fn render_title_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let playing = self.now.as_ref().map(|n| {
            if n.track.artist.is_empty() {
                n.track.title.clone()
            } else {
                format!("{} — {}", n.track.title, n.track.artist)
            }
        });
        gpui_kit::component::TitleBar::new().child(
            h_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .gap_3()
                .pr_3()
                .text_sm()
                .child(
                    h_flex()
                        .flex_none()
                        .gap_2()
                        .child(Icon::new(IconName::Disc3).small().text_color(theme.primary))
                        .child(div().font_semibold().child("Local Playlist")),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_center()
                        .text_color(theme.muted_foreground)
                        .children(playing),
                )
                // Balances the name on the left so the track title stays centred.
                .child(div().flex_none().w(px(120.))),
        )
    }

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
