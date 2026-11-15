//! Local Playlist entry point: desktop app, diagnostics or server. (The
//! webview login is a separate executable, see `src/bin/login.rs`.)

// On Windows the desktop app is a GUI program, so no console window opens
// behind it. Started from a terminal, it attaches to that terminal instead
// (see `attach_terminal`), so its output and logs still show there. Debug
// builds and server-only builds stay console programs.
#![cfg_attr(all(windows, feature = "desktop", not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(feature = "desktop")]
use local_playlist::model;
#[cfg(feature = "server")]
use local_playlist::server;
#[cfg(feature = "desktop")]
use local_playlist::{secrets, sources, ytdlp};

fn main() {
    local_playlist::mem::tune();
    #[cfg(all(windows, feature = "desktop", not(debug_assertions)))]
    attach_terminal();
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str);

    if matches!(mode, Some("--version" | "-V")) {
        println!("local-playlist {}", env!("LOCAL_PLAYLIST_VERSION"));
        return;
    }

    #[cfg(feature = "server")]
    if matches!(mode, Some("--server" | "--headless")) {
        if let Err(e) = server::run(&args[2..]) {
            eprintln!("local-playlist: {e:#}");
            std::process::exit(1);
        }
        return;
    }

    #[cfg(not(feature = "server"))]
    if matches!(mode, Some("--server" | "--headless")) {
        eprintln!("This build doesn't include the server. Build it with: cargo build --release --features server");
        std::process::exit(2);
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
        // One copy at a time (it may be playing in the tray): a second start
        // shows the running copy's window instead.
        let _lock = match local_playlist::instance::start(&paths) {
            local_playlist::instance::Start::First(lock) => lock,
            local_playlist::instance::Start::Running => {
                eprintln!("Local Playlist is already running: showing its window.");
                return;
            }
        };
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

/// Windows GUI build: when started from a terminal, send output (`--version`,
/// `--diagnose`, logs) to it. Started from Explorer there's no terminal and
/// nothing happens. Output already redirected (to a file or pipe) is kept.
#[cfg(all(windows, feature = "desktop", not(debug_assertions)))]
fn attach_terminal() {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn AttachConsole(process_id: u32) -> i32;
    }
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    // SAFETY: plain Win32 calls without preconditions; attaching fails
    // harmlessly when the parent has no console.
    unsafe {
        let redirected = |which| {
            let h = GetStdHandle(which);
            !h.is_null() && h as isize != -1
        };
        if !redirected(STD_OUTPUT_HANDLE) && !redirected(STD_ERROR_HANDLE) {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}
