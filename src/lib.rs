//! Local Playlist — a privacy-friendly, local-first music player for links
//! from YouTube / YouTube Music and SoundCloud.
//!
//! The same crate builds the desktop app (native), the web UI (wasm32, run in
//! the browser against a `--server` instance) and that server.

pub mod app;
pub mod download;
pub mod model;
pub mod sources;
pub mod storage;
pub mod theme;

// Desktop-only implementations, with web stand-ins of the same API so the UI
// code compiles unchanged for the browser (where those features are hidden).
#[cfg(not(target_family = "wasm"))]
pub mod audio;
#[cfg(target_family = "wasm")]
#[path = "web/audio.rs"]
pub mod audio;
#[cfg(not(target_family = "wasm"))]
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

#[cfg(not(target_family = "wasm"))]
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
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

#[cfg(not(target_family = "wasm"))]
use std::borrow::Cow;

use gpui_kit::*;

#[cfg(not(target_family = "wasm"))]
gpui_kit::assets::icon_assets!(
    AppIcons,
    [
        ArrowDown, ArrowUp, Monitor, Moon, Sun, CircleAlert, CircleCheck, CircleQuestionMark, CircleStop, Disc3,
        Download, FolderOpen, HardDriveDownload, Import, Link, ListMusic, LoaderCircle, LogIn, LogOut, Pause,
        Play, Plus, RefreshCw, Repeat, Repeat1, Server, Settings, Shuffle, SkipBack, SkipForward, Trash, Volume2,
        VolumeX, X
    ]
);

/// Embedded icons (desktop), also served to the web UI by `--server`.
#[cfg(not(target_family = "wasm"))]
pub struct AppAssets;

#[cfg(not(target_family = "wasm"))]
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
pub fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        titlebar: Some(TitlebarOptions { title: Some("Local Playlist".into()), ..Default::default() }),
        window_bounds: Some(WindowBounds::centered(size(px(1120.), px(740.)), cx)),
        window_min_size: Some(size(px(820.), px(520.))),
        app_id: Some("local-playlist".into()),
        ..Default::default()
    }
}

#[cfg(not(target_family = "wasm"))]
pub fn run_desktop(paths: model::Paths) {
    gpui_kit::application().with_assets(AppAssets).run(move |cx| {
        gpui_kit::init(cx);
        theme::init(cx);
        let appearance = model::Library::load(&paths.library_file()).map(|l| l.settings.appearance).unwrap_or_default();
        theme::apply(appearance, None, cx);
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        gpui_kit::open_window(window_options(cx), cx, |window, cx| {
            cx.new(|cx| app::LocalPlaylist::new(paths.clone(), window, cx))
        })
        .expect("failed to open window");
        cx.activate(true);
    });
}
