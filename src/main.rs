//! Local Playlist — a privacy-friendly, local-first music player for links
//! from YouTube / YouTube Music and SoundCloud.

mod app;
mod audio;
mod model;
mod sources;
mod ytdlp;

use std::borrow::Cow;

use gpui_kit::*;

gpui_kit::assets::icon_assets!(
    AppIcons,
    [
        ArrowDown, ArrowUp, Disc3, Import, Link, ListMusic, LoaderCircle, Pause, Play, Plus,
        RefreshCw, Repeat, Repeat1, Shuffle, SkipBack, SkipForward, Trash, Volume2, VolumeX, X
    ]
);

struct AppAssets;

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

fn main() {
    let paths = match model::Paths::new() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("local-playlist: {e:#}");
            std::process::exit(1);
        }
    };

    gpui_kit::application().with_assets(AppAssets).run(move |cx| {
        gpui_kit::init(cx);
        gpui_kit::component::Theme::sync_system_appearance(None, cx);
        cx.on_window_closed(|cx, _| cx.quit()).detach();

        let options = WindowOptions {
            titlebar: Some(TitlebarOptions { title: Some("Local Playlist".into()), ..Default::default() }),
            window_bounds: Some(WindowBounds::centered(size(px(1120.), px(740.)), cx)),
            window_min_size: Some(size(px(820.), px(520.))),
            app_id: Some("local-playlist".into()),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| app::LocalPlaylist::new(paths.clone(), window, cx))
        })
        .expect("failed to open window");
        cx.activate(true);
    });
}
