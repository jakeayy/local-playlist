//! Link parsing and metadata resolution for supported platforms.
//!
//! Metadata comes from each platform's public web endpoints, the same ones
//! their websites use for logged-out visitors, so no account or login is needed.
//! When the native path fails (e.g. the site changed), we fall back to yt-dlp.

#[cfg(not(target_family = "wasm"))]
pub mod soundcloud;
#[cfg(not(target_family = "wasm"))]
pub mod youtube;
#[cfg(target_family = "wasm")]
#[path = "../web/youtube.rs"]
pub mod youtube;

#[cfg(not(target_family = "wasm"))]
use std::sync::OnceLock;
#[cfg(not(target_family = "wasm"))]
use std::time::Duration;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::model::{Paths, Platform, Track};
use crate::secrets::Credentials;
#[cfg(not(target_family = "wasm"))]
use crate::secrets::find;
use crate::ytdlp::YtDlp;

/// A parsed, supported link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    YouTubeVideo {
        id: String,
        list: Option<String>,
    },
    YouTubePlaylist {
        list: String,
    },
    SoundCloudTrack {
        url: String,
    },
    SoundCloudSet {
        url: String,
    },
    SoundCloudLikes {
        user_url: String,
    },
    SoundCloudUser {
        user_url: String,
    },
    /// SoundCloud short link (on.soundcloud.com) which must be expanded first.
    SoundCloudShort {
        url: String,
    },
    /// The logged-in user's own likes.
    SoundCloudMyLikes,
}

impl Link {
    pub fn platform(&self) -> Platform {
        match self {
            Link::YouTubeVideo { .. } | Link::YouTubePlaylist { .. } => Platform::YouTube,
            _ => Platform::SoundCloud,
        }
    }

    pub fn is_collection(&self) -> bool {
        matches!(
            self,
            Link::YouTubePlaylist { .. }
                | Link::SoundCloudSet { .. }
                | Link::SoundCloudLikes { .. }
                | Link::SoundCloudUser { .. }
                | Link::SoundCloudMyLikes
        )
    }
}

/// What a link resolved to (also the `/api/resolve` response).
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Resolved {
    Track { track: Track },
    Collection { title: String, tracks: Vec<Track> },
}

impl Resolved {
    pub fn into_tracks(self) -> Vec<Track> {
        match self {
            Resolved::Track { track } => vec![track],
            Resolved::Collection { tracks, .. } => tracks,
        }
    }
}

const SC_RESERVED_ROOT: &[&str] = &[
    "discover",
    "search",
    "stream",
    "upload",
    "you",
    "settings",
    "messages",
    "notifications",
    "charts",
    "pages",
    "terms-of-use",
    "popular",
    "people",
    "feed",
    "signin",
    "logout",
];

const SC_USER_SUBPAGES: &[&str] =
    &["tracks", "popular-tracks", "albums", "reposts", "followers", "following", "comments", "spotlight"];

/// Parse a pasted link. Accepts URLs with or without scheme.
pub fn parse_link(input: &str) -> Result<Link> {
    let input = input.trim();
    if input.is_empty() {
        bail!("Paste a YouTube or SoundCloud link");
    }
    let with_scheme = if input.contains("://") { input.to_string() } else { format!("https://{input}") };
    let url = Url::parse(&with_scheme).map_err(|_| anyhow::anyhow!("That doesn't look like a link"))?;
    let host = url.host_str().unwrap_or_default().trim_start_matches("www.").to_ascii_lowercase();
    let segments: Vec<&str> = url.path_segments().map(|s| s.filter(|s| !s.is_empty()).collect()).unwrap_or_default();
    let query = |key: &str| url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned());

    match host.as_str() {
        "youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtube-nocookie.com" => {
            let list = query("list").filter(|l| !l.is_empty());
            match segments.as_slice() {
                ["watch"] => match query("v") {
                    Some(id) if youtube::is_video_id(&id) => Ok(Link::YouTubeVideo { id, list }),
                    _ => match list {
                        Some(list) => Ok(Link::YouTubePlaylist { list }),
                        None => bail!("YouTube link is missing the video id"),
                    },
                },
                ["playlist"] | ["browse", _] if list.is_some() => Ok(Link::YouTubePlaylist { list: list.unwrap() }),
                ["browse", id] if id.starts_with("VL") => Ok(Link::YouTubePlaylist { list: id[2..].to_string() }),
                ["shorts" | "live" | "embed" | "v", id] if youtube::is_video_id(id) => {
                    Ok(Link::YouTubeVideo { id: id.to_string(), list })
                }
                _ => bail!("Unsupported YouTube link (use a watch or playlist link)"),
            }
        }
        "youtu.be" => match segments.as_slice() {
            [id] if youtube::is_video_id(id) => Ok(Link::YouTubeVideo { id: id.to_string(), list: query("list") }),
            _ => bail!("Unsupported youtu.be link"),
        },
        "on.soundcloud.com" => Ok(Link::SoundCloudShort { url: url.to_string() }),
        "soundcloud.com" | "m.soundcloud.com" => {
            let base = |parts: &[&str]| format!("https://soundcloud.com/{}", parts.join("/"));
            match segments.as_slice() {
                [user, ..] if SC_RESERVED_ROOT.contains(user) => {
                    bail!("Unsupported SoundCloud page (use a track, set, profile or likes link)")
                }
                [user] => Ok(Link::SoundCloudUser { user_url: base(&[user]) }),
                [user, "likes"] => Ok(Link::SoundCloudLikes { user_url: base(&[user]) }),
                [user, "sets", set, ..] => Ok(Link::SoundCloudSet { url: base(&[user, "sets", set]) }),
                [user, "tracks"] => Ok(Link::SoundCloudUser { user_url: base(&[user]) }),
                [_, sub] if SC_USER_SUBPAGES.contains(sub) => {
                    bail!("Unsupported SoundCloud page (use a track, set, profile or likes link)")
                }
                [user, track, rest @ ..] => {
                    // Private share links have a secret token as third segment.
                    let mut parts = vec![*user, *track];
                    if let Some(secret) = rest.first().filter(|s| s.starts_with("s-")) {
                        parts.push(secret);
                    }
                    Ok(Link::SoundCloudTrack { url: base(&parts) })
                }
                [] => bail!("Use a SoundCloud track, set, profile or likes link"),
            }
        }
        _ => bail!("Only YouTube and SoundCloud links are supported for now"),
    }
}

/// Links found in a list of them (a text file, the clipboard, a pasted block).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LinkList {
    /// Supported links, in order, without repeats.
    pub links: Vec<Link>,
    /// Lines that held text but no supported link.
    pub skipped: usize,
}

/// Find the links in a list with one link per line. Blank lines and lines
/// starting with `#` are ignored, so a commented text file works as is. A line
/// can carry other text around its link ("Artist - Title https://…"), as
/// exports from other players often do; then each word is tried in turn.
pub fn parse_link_list(text: &str) -> LinkList {
    let mut list = LinkList::default();
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Not the whole line: spaces would end up inside a SoundCloud path.
        let found = line
            .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '<' | '>' | '"' | '\''))
            .map(|word| word.trim_matches(|c| matches!(c, '(' | ')' | '[' | ']')))
            .filter(|word| word.contains('.'))
            .filter_map(|word| parse_link(word).ok());
        let mut any = false;
        for link in found {
            any = true;
            if !list.links.contains(&link) {
                list.links.push(link);
            }
        }
        if !any {
            list.skipped += 1;
        }
    }
    list
}

#[cfg(not(target_family = "wasm"))]
/// Shared HTTP agent. A generic browser-like user agent is used; no cookies are kept.
pub fn http() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0")
            .http_status_as_error(false)
            .build()
            .into()
    })
}

/// Resolve on the desktop (blocking work on a background thread) or, in the
/// browser, by asking the server.
#[cfg(feature = "ui")]
pub fn resolve_task(
    cx: &gpui_kit::App,
    link: Link,
    as_collection: bool,
    paths: Paths,
    ytdlp: YtDlp,
    creds: Credentials,
) -> gpui_kit::Task<Result<Resolved>> {
    #[cfg(not(target_family = "wasm"))]
    {
        use gpui_kit::AppContext as _;
        cx.background_spawn(async move { resolve(&link, as_collection, &paths, &ytdlp, &creds) })
    }
    #[cfg(target_family = "wasm")]
    {
        let _ = (paths, ytdlp, creds);
        cx.foreground_executor().spawn(async move { crate::web::api::resolve(&link, as_collection).await })
    }
}

/// The canonical page URL for a link (sent to the server by the web UI).
pub fn link_url(link: &Link) -> String {
    match link {
        Link::YouTubeVideo { id, list: Some(list) } => format!("https://www.youtube.com/watch?v={id}&list={list}"),
        Link::YouTubeVideo { id, list: None } => format!("https://www.youtube.com/watch?v={id}"),
        Link::YouTubePlaylist { list } => format!("https://www.youtube.com/playlist?list={list}"),
        Link::SoundCloudTrack { url } | Link::SoundCloudSet { url } | Link::SoundCloudShort { url } => url.clone(),
        Link::SoundCloudLikes { user_url } => format!("{user_url}/likes"),
        Link::SoundCloudUser { user_url } => user_url.clone(),
        Link::SoundCloudMyLikes => String::new(),
    }
}

#[cfg(not(target_family = "wasm"))]
/// Resolve a link to track(s). `as_collection` decides whether a YouTube watch
/// link that carries a `list=` parameter is treated as the whole playlist.
/// Blocking: run on a background thread.
pub fn resolve(
    link: &Link,
    as_collection: bool,
    paths: &Paths,
    ytdlp: &YtDlp,
    creds: &Credentials,
) -> Result<Resolved> {
    let yt_auth = creds.yt_auth();
    let sc_auth = creds.get(Platform::SoundCloud).and_then(|c| find(c, "oauth_token"));
    let native = match link {
        Link::YouTubeVideo { list: Some(list), .. } if as_collection => {
            youtube::playlist(list, yt_auth).map(collection)
        }
        Link::YouTubeVideo { id, .. } => youtube::video(id).map(|track| Resolved::Track { track }),
        Link::YouTubePlaylist { list } => youtube::playlist(list, yt_auth).map(collection),
        Link::SoundCloudShort { url } => {
            let expanded = soundcloud::expand_short_link(url)?;
            let link = parse_link(&expanded)?;
            if matches!(link, Link::SoundCloudShort { .. }) {
                bail!("Could not expand SoundCloud short link");
            }
            return resolve(&link, as_collection, paths, ytdlp, creds);
        }
        other => soundcloud::resolve(other, paths, sc_auth),
    };

    match native {
        Ok(r) => Ok(r),
        // Login-required lists have nothing to gain from the fallback.
        Err(err)
            if matches!(link, Link::SoundCloudMyLikes)
                || matches!(link, Link::YouTubePlaylist { list } if youtube::is_personal_list(list) && yt_auth.is_none()) =>
        {
            Err(err)
        }
        Err(err) => {
            eprintln!("native resolve failed for {link:?}: {err:#}; trying yt-dlp");
            let cookies = match link.platform() {
                Platform::YouTube => creds.get(Platform::YouTube),
                Platform::SoundCloud => creds.get(Platform::SoundCloud),
            };
            ytdlp_fallback(link, as_collection, ytdlp, cookies)
                .map_err(|e2| anyhow::anyhow!("{err:#}\n(yt-dlp fallback also failed: {e2:#})"))
        }
    }
}

#[cfg(not(target_family = "wasm"))]
fn collection((title, tracks): (String, Vec<Track>)) -> Resolved {
    Resolved::Collection { title, tracks }
}

#[cfg(not(target_family = "wasm"))]
fn ytdlp_fallback(
    link: &Link,
    as_collection: bool,
    ytdlp: &YtDlp,
    cookies: Option<&[crate::secrets::StoredCookie]>,
) -> Result<Resolved> {
    let url = match link {
        Link::YouTubeVideo { list: Some(list), .. } if as_collection => {
            format!("https://www.youtube.com/playlist?list={list}")
        }
        Link::YouTubeVideo { id, .. } => format!("https://www.youtube.com/watch?v={id}"),
        Link::YouTubePlaylist { list } => format!("https://www.youtube.com/playlist?list={list}"),
        Link::SoundCloudTrack { url } | Link::SoundCloudSet { url } => url.clone(),
        Link::SoundCloudLikes { user_url } => format!("{user_url}/likes"),
        Link::SoundCloudUser { user_url } => format!("{user_url}/tracks"),
        Link::SoundCloudShort { url } => url.clone(),
        Link::SoundCloudMyLikes => bail!("not supported"),
    };
    let info = ytdlp.extract_info(&url, cookies)?;
    let platform = link.platform();
    if let Some(entries) = info.get("entries").and_then(|e| e.as_array()) {
        let title = info["title"].as_str().unwrap_or("Imported playlist").to_string();
        let tracks = entries.iter().filter_map(|e| track_from_ytdlp(e, platform)).collect();
        Ok(Resolved::Collection { title, tracks })
    } else {
        track_from_ytdlp(&info, platform)
            .map(|track| Resolved::Track { track })
            .ok_or_else(|| anyhow::anyhow!("yt-dlp returned no usable metadata"))
    }
}

#[cfg(not(target_family = "wasm"))]
fn track_from_ytdlp(v: &serde_json::Value, platform: Platform) -> Option<Track> {
    let id = match &v["id"] {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let title = v["title"].as_str()?.to_string();
    let artist = ["artist", "uploader", "channel"].iter().find_map(|k| v[*k].as_str()).unwrap_or_default().to_string();
    let url = match platform {
        Platform::YouTube => format!("https://www.youtube.com/watch?v={id}"),
        Platform::SoundCloud => v["webpage_url"]
            .as_str()
            .or(v["url"].as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("https://api.soundcloud.com/tracks/{id}")),
    };
    let duration = v["duration"].as_f64().map(|d| d.round() as u32);
    Some(Track::new(platform, id, url, title, artist, duration))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_youtube() {
        assert_eq!(
            parse_link("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10").unwrap(),
            Link::YouTubeVideo { id: "dQw4w9WgXcQ".into(), list: None }
        );
        assert_eq!(
            parse_link("music.youtube.com/watch?v=dQw4w9WgXcQ&list=PLabc").unwrap(),
            Link::YouTubeVideo { id: "dQw4w9WgXcQ".into(), list: Some("PLabc".into()) }
        );
        assert_eq!(
            parse_link("https://youtu.be/dQw4w9WgXcQ?si=xyz").unwrap(),
            Link::YouTubeVideo { id: "dQw4w9WgXcQ".into(), list: None }
        );
        assert_eq!(
            parse_link("https://youtube.com/shorts/dQw4w9WgXcQ").unwrap(),
            Link::YouTubeVideo { id: "dQw4w9WgXcQ".into(), list: None }
        );
        assert_eq!(
            parse_link("https://music.youtube.com/playlist?list=PLxyz").unwrap(),
            Link::YouTubePlaylist { list: "PLxyz".into() }
        );
        assert!(parse_link("https://example.com/x").is_err());
    }

    #[test]
    fn parses_soundcloud() {
        assert_eq!(
            parse_link("https://soundcloud.com/forss/flickermood?in=x").unwrap(),
            Link::SoundCloudTrack { url: "https://soundcloud.com/forss/flickermood".into() }
        );
        assert_eq!(
            parse_link("https://soundcloud.com/forss/sets/soulhack").unwrap(),
            Link::SoundCloudSet { url: "https://soundcloud.com/forss/sets/soulhack".into() }
        );
        assert_eq!(
            parse_link("https://m.soundcloud.com/forss/likes").unwrap(),
            Link::SoundCloudLikes { user_url: "https://soundcloud.com/forss".into() }
        );
        assert_eq!(
            parse_link("soundcloud.com/forss").unwrap(),
            Link::SoundCloudUser { user_url: "https://soundcloud.com/forss".into() }
        );
        assert!(parse_link("https://soundcloud.com/discover").is_err());
        assert!(parse_link("https://soundcloud.com/forss/followers").is_err());
    }

    #[test]
    fn parses_link_lists() {
        let text = "\u{feff}# my songs\r\n\
            https://www.youtube.com/watch?v=dQw4w9WgXcQ\r\n\
            \r\n\
            youtu.be/dQw4w9WgXcQ\n\
            Forss - Flickermood (https://soundcloud.com/forss/flickermood)\n\
            not a link\n\
            https://example.com/song\n\
            https://music.youtube.com/playlist?list=PLxyz, https://soundcloud.com/forss/sets/soulhack\n";
        let list = parse_link_list(text);
        assert_eq!(
            list.links,
            vec![
                Link::YouTubeVideo { id: "dQw4w9WgXcQ".into(), list: None },
                Link::SoundCloudTrack { url: "https://soundcloud.com/forss/flickermood".into() },
                Link::YouTubePlaylist { list: "PLxyz".into() },
                Link::SoundCloudSet { url: "https://soundcloud.com/forss/sets/soulhack".into() },
            ]
        );
        assert_eq!(list.skipped, 2);
        assert_eq!(parse_link_list(""), LinkList::default());
    }
}
