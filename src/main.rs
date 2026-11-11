//! Local Playlist — a privacy-friendly, local-first music player for links
//! from YouTube / YouTube Music and SoundCloud.

mod app;
mod audio;
mod login;
mod model;
mod secrets;
mod sources;
mod ytdlp;

use std::borrow::Cow;

use gpui_kit::*;

gpui_kit::assets::icon_assets!(
    AppIcons,
    [
        ArrowDown, ArrowUp, CircleAlert, Disc3, Download, Import, Link, ListMusic, LoaderCircle, LogIn,
        LogOut, Pause, Play, Plus, RefreshCw, Repeat, Repeat1, Settings, Shuffle, SkipBack, SkipForward,
        Trash, Volume2, VolumeX, X
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
    // Webview login helper process (see login.rs).
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(login::LOGIN_ARG) {
        login::run_helper(args.get(2).map(String::as_str).unwrap_or_default());
    }

    let paths = match model::Paths::new() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("local-playlist: {e:#}");
            std::process::exit(1);
        }
    };

    // `local-playlist --diagnose <youtube playlist url>`: print what YouTube
    // returns with the saved session (cookie names only, never values).
    if args.get(1).map(String::as_str) == Some("--diagnose") {
        let url = args.get(2).cloned().unwrap_or_default();
        let creds = secrets::SecretStore::new(&paths).load().map(|(c, _)| c);
        let result = (|| -> anyhow::Result<String> {
            let creds = creds?;
            let list = match sources::parse_link(&url)? {
                sources::Link::YouTubePlaylist { list } | sources::Link::YouTubeVideo { list: Some(list), .. } => list,
                other => anyhow::bail!("not a YouTube playlist link: {other:?}"),
            };
            sources::youtube::diagnose(&list, creds.get(model::Platform::YouTube))
        })();
        match result {
            Ok(s) => print!("{s}"),
            Err(e) => eprintln!("{e:#}"),
        }
        return;
    }

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
