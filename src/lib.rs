//! Local Playlist — a privacy-friendly, local-first music player for links
//! from YouTube / YouTube Music and SoundCloud.
//!
//! The same crate builds the desktop app (native), the web UI (wasm32, run in
//! the browser against a `--server` instance) and that server.

#[cfg(all(feature = "ui", not(target_family = "wasm"), not(feature = "desktop")))]
compile_error!("on native targets the `ui` feature needs `desktop`");

#[cfg(feature = "ui")]
pub mod app;
pub mod cover;
pub mod download;
#[cfg(feature = "desktop")]
pub mod instance;
pub mod lyrics;
pub mod mem;
pub mod model;
#[cfg(feature = "desktop")]
pub mod remux;
pub mod sources;
#[cfg(feature = "ui")]
pub mod spectrum;
pub mod sponsorblock;
pub mod storage;
#[cfg(feature = "ui")]
pub mod theme;
#[cfg(feature = "desktop")]
pub mod tray;

// Desktop-only implementations, with web stand-ins of the same API so the UI
// code compiles unchanged for the browser (where those features are hidden).
#[cfg(all(not(target_family = "wasm"), feature = "desktop"))]
pub mod audio;
#[cfg(target_family = "wasm")]
#[path = "web/audio.rs"]
pub mod audio;
#[cfg(all(not(target_family = "wasm"), feature = "desktop"))]
pub mod login;
#[cfg(target_family = "wasm")]
#[path = "web/login.rs"]
pub mod login;
pub mod secrets;
#[cfg(not(target_family = "wasm"))]
pub mod ytdlp;
#[cfg(target_family = "wasm")]
#[path = "web/ytdlp.rs"]
pub mod ytdlp;

#[cfg(all(not(target_family = "wasm"), feature = "server"))]
pub mod server;
#[cfg(target_family = "wasm")]
pub mod web;

/// True in the browser build.
pub const WEB: bool = cfg!(target_family = "wasm");

/// Milliseconds since the Unix epoch (`std::time` is unavailable in browsers).
pub fn now_ms() -> f64 {
    #[cfg(target_family = "wasm")]
    return js_sys::Date::now();
    #[cfg(not(target_family = "wasm"))]
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}

#[cfg(feature = "desktop")]
use std::borrow::Cow;

#[cfg(feature = "ui")]
use gpui_kit::*;

#[cfg(feature = "desktop")]
gpui_kit::assets::icon_assets!(
    AppIcons,
    [
        ArrowDown,
        ArrowUp,
        AudioLines,
        ChartColumn,
        Monitor,
        Moon,
        Sun,
        CircleAlert,
        CircleCheck,
        CircleQuestionMark,
        CircleStop,
        Disc3,
        Download,
        FolderOpen,
        HardDriveDownload,
        Import,
        Link,
        ListMusic,
        LoaderCircle,
        LogIn,
        LogOut,
        MicVocal,
        Minus,
        Pause,
        Play,
        Plus,
        RefreshCw,
        Repeat,
        Repeat1,
        Server,
        Settings,
        Shuffle,
        SkipBack,
        SkipForward,
        Trash,
        Volume2,
        VolumeX,
        X
    ]
);

/// Embedded icons for the desktop app.
#[cfg(feature = "desktop")]
pub struct AppAssets;

#[cfg(feature = "desktop")]
impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = AppIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(AppIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

/// Window content shared by desktop and web.
#[cfg(feature = "ui")]
pub fn window_options(cx: &App) -> WindowOptions {
    // Desktop draws its own title bar (`app::LocalPlaylist::render_title_bar`).
    #[cfg(feature = "desktop")]
    let base = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("Local Playlist".into()),
            ..gpui_kit::component::TitleBar::title_bar_options()
        }),
        window_decorations: Some(WindowDecorations::Client),
        ..gpui_kit::component::TitleBar::window_options()
    };
    #[cfg(not(feature = "desktop"))]
    let base = WindowOptions {
        titlebar: Some(TitlebarOptions { title: Some("Local Playlist".into()), ..Default::default() }),
        ..Default::default()
    };
    WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1120.), px(740.)), cx)),
        window_min_size: Some(size(px(820.), px(520.))),
        app_id: Some("local-playlist".into()),
        ..base
    }
}

#[cfg(feature = "desktop")]
pub fn run_desktop(paths: model::Paths) {
    gpui_kit::application().with_assets(AppAssets).run(move |cx| {
        gpui_kit::init(cx);
        theme::init(cx);
        let appearance = model::Library::load(&paths.library_file()).map(|l| l.settings.appearance).unwrap_or_default();
        theme::apply(appearance, None, cx);
        // The app quits when its window closes, unless it keeps playing in
        // the background with a tray icon (`app::window_closed`).
        cx.set_quit_mode(QuitMode::Explicit);
        let main = app::open(paths.clone(), cx);
        cx.on_window_closed(move |cx, _| app::window_closed(&main, cx)).detach();
        cx.activate(true);
    });
}
