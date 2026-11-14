//! `local-playlist-login <platform>`: the webview login helper.
//!
//! A separate executable so the main app never loads the webview libraries
//! (WebKitGTK and GTK on Linux, tens of MB of memory). The app starts it (see
//! `local_playlist::login`); it opens the platform's normal login page in an
//! incognito webview (nothing is written to disk), waits until the session
//! cookie appears, prints the cookies as JSON on stdout and exits.
//! No OAuth apps or client ids are involved: it is the same session a browser
//! would get.
//!
//! Exit codes: 0 logged in (cookies on stdout), 2 window closed, 1 error.

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

use local_playlist::model::Platform;
use local_playlist::secrets::StoredCookie;

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    let code = match helper(&arg) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e:#}");
            1
        }
    };
    std::process::exit(code)
}

/// A Firefox user agent for the current OS. Google is less likely to flag a
/// common desktop browser than an unknown embedded engine. Firefox doesn't
/// send client hints, so the UA string alone is consistent.
fn firefox_user_agent() -> String {
    // Firefox ships every 4 weeks; 128 was released 2024-07-09.
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0);
    let base_day = 19_913; // 2024-07-09
    let version = 128 + days.saturating_sub(base_day) / 28;
    let platform = if cfg!(target_os = "windows") {
        "Windows NT 10.0; Win64; x64".to_string()
    } else if cfg!(target_os = "macos") {
        "Macintosh; Intel Mac OS X 10.15".to_string()
    } else if cfg!(target_arch = "aarch64") {
        "X11; Linux aarch64".to_string()
    } else {
        "X11; Linux x86_64".to_string()
    };
    format!("Mozilla/5.0 ({platform}; rv:{version}.0) Gecko/20100101 Firefox/{version}.0")
}

struct Target {
    title: &'static str,
    start_url: &'static str,
    cookie_urls: &'static [&'static str],
    /// Login is complete once any of these cookies exists.
    session_cookies: &'static [&'static str],
}

fn target(platform: Platform) -> Target {
    match platform {
        Platform::YouTube => Target {
            title: "Log in to YouTube — Local Playlist",
            start_url: "https://accounts.google.com/ServiceLogin?service=youtube&continue=https%3A%2F%2Fwww.youtube.com%2F",
            cookie_urls: &["https://www.youtube.com/"],
            session_cookies: &["SAPISID", "__Secure-3PAPISID"],
        },
        Platform::SoundCloud => Target {
            title: "Log in to SoundCloud — Local Playlist",
            start_url: "https://soundcloud.com/signin",
            cookie_urls: &["https://soundcloud.com/", "https://api-v2.soundcloud.com/"],
            session_cookies: &["oauth_token"],
        },
    }
}

fn stored_cookie(c: &wry::cookie::Cookie<'_>) -> StoredCookie {
    StoredCookie {
        name: c.name().to_string(),
        value: c.value().to_string(),
        domain: c.domain().unwrap_or_default().to_string(),
        path: c.path().unwrap_or("/").to_string(),
        secure: c.secure().unwrap_or(false),
        http_only: c.http_only().unwrap_or(false),
        expires: c.expires_datetime().map(|t| t.unix_timestamp()),
    }
}

fn helper(platform_arg: &str) -> Result<()> {
    let platform = match platform_arg {
        "youtube" => Platform::YouTube,
        "soundcloud" => Platform::SoundCloud,
        other => bail!("unknown platform {other:?} (expected youtube or soundcloud)"),
    };
    let t = target(platform);

    let event_loop = EventLoopBuilder::new().build();
    let window = WindowBuilder::new()
        .with_title(t.title)
        .with_inner_size(tao::dpi::LogicalSize::new(520.0, 760.0))
        .build(&event_loop)
        .context("could not open login window")?;

    let builder = WebViewBuilder::new()
        .with_url(t.start_url)
        .with_user_agent(firefox_user_agent())
        .with_incognito(true);

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let webview = {
        use tao::platform::unix::WindowExtUnix as _;
        use wry::WebViewBuilderExtUnix as _;
        builder.build_gtk(window.default_vbox().context("no GTK container")?)?
    };
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let webview = builder.build(&window)?;

    let mut last_check = Instant::now();
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(750));
        let _ = &window;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                eprintln!("Login window closed before signing in");
                std::process::exit(2);
            }
            Event::NewEvents(_) if last_check.elapsed() >= Duration::from_millis(700) => {
                last_check = Instant::now();
                let mut cookies = Vec::new();
                for url in t.cookie_urls {
                    if let Ok(list) = webview.cookies_for_url(url) {
                        cookies.extend(list.iter().map(stored_cookie));
                    }
                }
                cookies.sort_by(|a, b| (&a.domain, &a.name).cmp(&(&b.domain, &b.name)));
                cookies.dedup_by(|a, b| a.domain == b.domain && a.name == b.name && a.path == b.path);
                let logged_in = cookies
                    .iter()
                    .any(|c| t.session_cookies.contains(&c.name.as_str()) && !c.value.is_empty());
                // For YouTube wait until we're back on youtube.com so all cookies are set.
                let on_site = match platform {
                    Platform::YouTube => webview.url().is_ok_and(|u| u.contains("youtube.com")),
                    Platform::SoundCloud => true,
                };
                if logged_in && on_site {
                    match serde_json::to_string(&cookies) {
                        Ok(json) => {
                            println!("{json}");
                            std::process::exit(0);
                        }
                        Err(e) => {
                            eprintln!("{e}");
                            std::process::exit(1);
                        }
                    }
                }
            }
            _ => {}
        }
    })
}
