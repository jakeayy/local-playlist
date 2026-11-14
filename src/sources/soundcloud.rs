//! SoundCloud metadata via the public api-v2 used by soundcloud.com.
//!
//! The web app authenticates anonymous visitors with a public `client_id`
//! embedded in its JavaScript bundles. We read it from there (and cache it),
//! so no account is required.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

use super::{Link, Resolved, http};
use crate::model::{Paths, Platform, Track};
use ureq::ResponseExt as _;

const API: &str = "https://api-v2.soundcloud.com";
const MAX_ITEMS: usize = 10_000;

static CLIENT_ID: Mutex<Option<String>> = Mutex::new(None);

/// Resolve a SoundCloud link. `oauth` (the session's `oauth_token`) is only
/// sent for "my likes" or when the anonymous request fails (private content).
pub fn resolve(link: &Link, paths: &Paths, oauth: Option<&str>) -> Result<Resolved> {
    if matches!(link, Link::SoundCloudMyLikes) {
        let Some(token) = oauth else { bail!("Log in to SoundCloud (sidebar → Accounts) to import your likes") };
        let me = api_get(&format!("{API}/me"), paths, Some(token))?;
        let (id, _) = user_info(&me)?;
        let first = format!("{API}/users/{id}/likes?limit=200&linked_partitioning=1");
        let tracks = paginate(&first, paths, Some(token), |item| item.get("track").and_then(track_from_json))?;
        return Ok(Resolved::Collection { title: "My SoundCloud likes".into(), tracks });
    }
    match resolve_with(link, paths, None) {
        Ok(r) => Ok(r),
        Err(_) if oauth.is_some() => resolve_with(link, paths, oauth),
        Err(e) => Err(e),
    }
}

fn resolve_with(link: &Link, paths: &Paths, auth: Option<&str>) -> Result<Resolved> {
    match link {
        Link::SoundCloudTrack { url } => {
            let v = api_resolve(url, paths, auth)?;
            if v["kind"].as_str() != Some("track") {
                bail!("That SoundCloud link is not a track");
            }
            Ok(Resolved::Track { track: track_from_json(&v).context("Track has no usable metadata")? })
        }
        Link::SoundCloudSet { url } => {
            let v = api_resolve(url, paths, auth)?;
            if v["kind"].as_str() != Some("playlist") {
                bail!("That SoundCloud link is not a playlist/album");
            }
            let title = v["title"].as_str().unwrap_or("SoundCloud set").to_string();
            let tracks = playlist_tracks(&v, paths, auth)?;
            Ok(Resolved::Collection { title, tracks })
        }
        Link::SoundCloudLikes { user_url } => {
            let user = api_resolve(user_url, paths, auth)?;
            let (id, name) = user_info(&user)?;
            let first = format!("{API}/users/{id}/likes?limit=200&linked_partitioning=1");
            let tracks = paginate(&first, paths, auth, |item| {
                // Likes contain both tracks and playlists; we take tracks.
                item.get("track").and_then(track_from_json)
            })?;
            Ok(Resolved::Collection { title: format!("{name} — Likes"), tracks })
        }
        Link::SoundCloudUser { user_url } => {
            let user = api_resolve(user_url, paths, auth)?;
            let (id, name) = user_info(&user)?;
            let first = format!("{API}/users/{id}/tracks?limit=200&linked_partitioning=1");
            let tracks = paginate(&first, paths, auth, track_from_json)?;
            Ok(Resolved::Collection { title: format!("{name} — Tracks"), tracks })
        }
        _ => bail!("Not a SoundCloud link"),
    }
}

/// Expand an on.soundcloud.com short link by following its redirect.
pub fn expand_short_link(url: &str) -> Result<String> {
    let resp = http().get(url).call()?;
    let final_url = resp.get_uri().to_string();
    Ok(final_url)
}

fn user_info(user: &Value) -> Result<(u64, String)> {
    if user["kind"].as_str() != Some("user") {
        bail!("That SoundCloud link is not a profile");
    }
    let id = user["id"].as_u64().context("user has no id")?;
    let name = user["username"].as_str().unwrap_or("SoundCloud user").to_string();
    Ok((id, name))
}

/// Artwork URL of a track (the 100×100 `-large` variant), falling back to the
/// uploader's avatar.
pub fn artwork_url(id: &str, paths: &Paths) -> Result<Option<String>> {
    let v = api_get(&format!("{API}/tracks/{id}"), paths, None)?;
    let url = [v["artwork_url"].as_str(), v.pointer("/user/avatar_url").and_then(Value::as_str)]
        .into_iter()
        .flatten()
        .find(|u| !u.is_empty() && !u.contains("default_avatar"));
    Ok(url.map(str::to_string))
}

fn api_resolve(url: &str, paths: &Paths, auth: Option<&str>) -> Result<Value> {
    let encoded: String = url::form_urlencoded::byte_serialize(url.as_bytes()).collect();
    api_get(&format!("{API}/resolve?url={encoded}"), paths, auth)
}

/// GET an api-v2 URL, adding the client_id and refreshing it once if rejected.
fn api_get(url: &str, paths: &Paths, auth: Option<&str>) -> Result<Value> {
    for attempt in 0..2 {
        let cid = client_id(paths, attempt > 0)?;
        let sep = if url.contains('?') { '&' } else { '?' };
        let mut req = http().get(format!("{url}{sep}client_id={cid}"));
        if let Some(token) = auth {
            req = req.header("Authorization", format!("OAuth {token}"));
        }
        let mut resp = req.call()?;
        match resp.status().as_u16() {
            200..=299 => {
                return Ok(resp.body_mut().with_config().limit(64 * 1024 * 1024).read_json()?);
            }
            401 | 403 if attempt == 0 => continue,
            404 => bail!("Not found on SoundCloud (it may be private or deleted)"),
            s => bail!("SoundCloud returned HTTP {s}"),
        }
    }
    bail!("SoundCloud rejected the request")
}

fn client_id(paths: &Paths, refresh: bool) -> Result<String> {
    let mut guard = CLIENT_ID.lock().unwrap();
    if !refresh {
        if let Some(id) = guard.as_ref() {
            return Ok(id.clone());
        }
        if let Ok(id) = std::fs::read_to_string(paths.soundcloud_client_id_file()) {
            let id = id.trim().to_string();
            if id.len() == 32 {
                *guard = Some(id.clone());
                return Ok(id);
            }
        }
    }
    let id = scrape_client_id()?;
    let _ = std::fs::create_dir_all(&paths.cache);
    let _ = std::fs::write(paths.soundcloud_client_id_file(), &id);
    *guard = Some(id.clone());
    Ok(id)
}

fn scrape_client_id() -> Result<String> {
    let html = http().get("https://soundcloud.com/").call()?.body_mut().read_to_string()?;
    let mut scripts = Vec::new();
    let mut rest = html.as_str();
    while let Some(start) = rest.find("https://a-v2.sndcdn.com/assets/") {
        let tail = &rest[start..];
        let end = tail.find('"').unwrap_or(tail.len());
        let src = &tail[..end];
        if src.ends_with(".js") {
            scripts.push(src.to_string());
        }
        rest = &tail[end..];
    }
    // The client_id lives in one of the later bundles.
    for src in scripts.iter().rev() {
        let Ok(mut resp) = http().get(src).call() else { continue };
        let Ok(js) = resp.body_mut().with_config().limit(32 * 1024 * 1024).read_to_string() else {
            continue;
        };
        if let Some(id) = find_client_id(&js) {
            return Ok(id);
        }
    }
    bail!("Could not obtain a SoundCloud client id (site layout may have changed)")
}

fn find_client_id(js: &str) -> Option<String> {
    for pat in ["client_id:\"", "client_id=\"", "client_id:'"] {
        let mut rest = js;
        while let Some(pos) = rest.find(pat) {
            let candidate = &rest[pos + pat.len()..];
            let id: String = candidate.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            if id.len() == 32 {
                return Some(id);
            }
            rest = &rest[pos + pat.len()..];
        }
    }
    None
}

/// Playlists only embed full data for the first few tracks; fetch the rest by id.
fn playlist_tracks(v: &Value, paths: &Paths, auth: Option<&str>) -> Result<Vec<Track>> {
    let items = v["tracks"].as_array().cloned().unwrap_or_default();
    let mut known: HashMap<u64, Track> = HashMap::new();
    let mut missing = Vec::new();
    for item in &items {
        let Some(id) = item["id"].as_u64() else { continue };
        match track_from_json(item) {
            Some(t) => {
                known.insert(id, t);
            }
            None => missing.push(id),
        }
    }
    for chunk in missing.chunks(50) {
        let ids: Vec<String> = chunk.iter().map(u64::to_string).collect();
        let fetched = api_get(&format!("{API}/tracks?ids={}", ids.join("%2C")), paths, auth)?;
        for t in fetched.as_array().into_iter().flatten() {
            if let (Some(id), Some(track)) = (t["id"].as_u64(), track_from_json(t)) {
                known.insert(id, track);
            }
        }
    }
    // Preserve playlist order; give duplicates fresh entry ids.
    Ok(items
        .iter()
        .filter_map(|i| i["id"].as_u64())
        .filter_map(|id| known.get(&id).cloned())
        .map(|mut t| {
            t.id = crate::model::new_id();
            t
        })
        .collect())
}

fn paginate(
    first: &str,
    paths: &Paths,
    auth: Option<&str>,
    map: impl Fn(&Value) -> Option<Track>,
) -> Result<Vec<Track>> {
    let mut out = Vec::new();
    let mut next = Some(first.to_string());
    while let Some(url) = next.take() {
        // next_href already carries query params but not our client_id.
        let url = strip_client_id(&url);
        let page = api_get(&url, paths, auth)?;
        for item in page["collection"].as_array().into_iter().flatten() {
            if let Some(t) = map(item) {
                out.push(t);
            }
        }
        if out.len() >= MAX_ITEMS {
            break;
        }
        next = page["next_href"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    }
    Ok(out)
}

fn strip_client_id(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut u) => {
            let pairs: Vec<(String, String)> = u
                .query_pairs()
                .filter(|(k, _)| k != "client_id")
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            u.query_pairs_mut().clear().extend_pairs(pairs);
            u.to_string()
        }
        Err(_) => url.to_string(),
    }
}

fn track_from_json(v: &Value) -> Option<Track> {
    if v.get("kind").and_then(Value::as_str).is_some_and(|k| k != "track") {
        return None;
    }
    let id = v["id"].as_u64()?;
    let title = v["title"].as_str()?;
    let url = v["permalink_url"].as_str()?;
    let artist = v
        .pointer("/publisher_metadata/artist")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| v.pointer("/user/username").and_then(Value::as_str))
        .unwrap_or_default();
    let duration = v["full_duration"].as_u64().or(v["duration"].as_u64()).map(|ms| (ms / 1000) as u32);
    Some(Track::new(Platform::SoundCloud, id.to_string(), url, title, artist, duration))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_client_id() {
        let js = r#"foo({client_id:"c0lcDb60e4gNCrZY24mdmz9uWo1Ih29e",env:"x"})"#;
        assert_eq!(find_client_id(js).as_deref(), Some("c0lcDb60e4gNCrZY24mdmz9uWo1Ih29e"));
        assert_eq!(find_client_id("client_id:\"short\""), None);
    }

    fn test_paths() -> Paths {
        let dir = std::env::temp_dir().join("local-playlist-test");
        Paths { data: dir.join("data"), cache: dir.join("cache") }
    }

    #[test]
    #[ignore = "network"]
    fn live_set() {
        let link = super::super::parse_link("https://soundcloud.com/forss/sets/soulhack").unwrap();
        let Resolved::Collection { title, tracks } = resolve(&link, &test_paths(), None).unwrap() else { panic!() };
        println!("{title}: {:?}", tracks.iter().map(|t| &t.title).collect::<Vec<_>>());
        assert_eq!(tracks.len(), 11);
    }

    #[test]
    #[ignore = "network"]
    fn live_likes() {
        let link = super::super::parse_link("https://soundcloud.com/forss/likes").unwrap();
        let Resolved::Collection { title, tracks } = resolve(&link, &test_paths(), None).unwrap() else { panic!() };
        println!("{title}: {} tracks", tracks.len());
        assert!(!tracks.is_empty());
    }
}
