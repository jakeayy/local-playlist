//! Local Playlist entry point: desktop app, login helper, diagnostics or server.

#[cfg(feature = "desktop")]
use local_playlist::{login, secrets, sources, ytdlp};
#[cfg(feature = "desktop")]
use local_playlist::model;
#[cfg(feature = "server")]
use local_playlist::server;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str);

    // Webview login helper process (see login.rs).
    #[cfg(feature = "desktop")]
    if mode == Some(login::LOGIN_ARG) {
        login::run_helper(args.get(2).map(String::as_str).unwrap_or_default());
    }

    #[cfg(feature = "server")]
    if matches!(mode, Some("--server" | "--headless")) {
        if let Err(e) = server::run(&args[2..]) {
            eprintln!("local-playlist: {e:#}");
            std::process::exit(1);
        }
        return;
    }

    #[cfg(feature = "desktop")]
    {
        let paths = match model::Paths::new() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("local-playlist: {e:#}");
                std::process::exit(1);
            }
        };
        if mode == Some("--diagnose") {
            diagnose(&args, &paths);
            return;
        }
        local_playlist::run_desktop(paths);
    }

    #[cfg(not(feature = "desktop"))]
    {
        let _ = mode;
        eprintln!("This build only contains the server. Run: local-playlist --server --help");
        std::process::exit(2);
    }
}

/// `local-playlist --diagnose <youtube playlist url> [channel name]`: print what
/// YouTube returns with the saved session (cookie names only, never values).
#[cfg(feature = "desktop")]
fn diagnose(args: &[String], paths: &model::Paths) {
    let url = args.get(2).cloned().unwrap_or_default();
    let creds = secrets::SecretStore::new(paths).load().map(|(c, _)| c);
    let result = (|| -> anyhow::Result<String> {
        let creds = creds?;
        let list = match sources::parse_link(&url)? {
        sources::Link::YouTubePlaylist { list } | sources::Link::YouTubeVideo { list: Some(list), .. } => list,
        other => anyhow::bail!("not a YouTube playlist link: {other:?}"),
        };
        let mut creds = creds;
        // Optional 3rd arg: act as the channel with this name.
        if let (Some(name), Some(cookies)) = (args.get(3), creds.get(model::Platform::YouTube)) {
        let ch = sources::youtube::channels(cookies)?;
        creds.youtube_channel = ch.into_iter().find(|c| &c.name == name);
        anyhow::ensure!(creds.youtube_channel.is_some(), "no channel named {name}");
        }
        let mut out = sources::youtube::diagnose(&list, creds.yt_auth())?;
        // Then run the real import path.
        let ytdlp = ytdlp::YtDlp::new(paths, Default::default(), Default::default());
        match sources::resolve(&sources::parse_link(&url)?, true, paths, &ytdlp, &creds) {
        Ok(sources::Resolved::Collection { title, tracks }) => {
            out += &format!("import: \"{title}\" with {} tracks\n", tracks.len())
        }
        Ok(sources::Resolved::Track { .. }) => out += "import: single track\n",
        Err(e) => out += &format!("import error: {e:#}\n"),
        }
        Ok(out)
    })();
    match result {
        Ok(s) => print!("{s}"),
        Err(e) => eprintln!("{e:#}"),
    }
}
