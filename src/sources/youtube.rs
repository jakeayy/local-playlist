//! YouTube metadata via the public InnerTube web API (what youtube.com itself
//! uses for logged-out visitors) with oEmbed as a lightweight fallback.

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};

use super::http;
use crate::model::{Platform, Track};

const INNERTUBE: &str = "https://www.youtube.com/youtubei/v1";
const CLIENT_VERSION: &str = "2.20251105.01.00";
/// Hard cap so a gigantic playlist can't loop forever.
const MAX_ITEMS: usize = 10_000;

pub fn is_video_id(id: &str) -> bool {
    id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn watch_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

fn innertube(endpoint: &str, mut body: Value) -> Result<Value> {
    body["context"] = json!({
        "client": { "clientName": "WEB", "clientVersion": CLIENT_VERSION, "hl": "en", "gl": "US" }
    });
    let mut resp = http()
        .post(format!("{INNERTUBE}/{endpoint}?prettyPrint=false"))
        .header("Content-Type", "application/json")
        .header("X-YouTube-Client-Name", "1")
        .header("X-YouTube-Client-Version", CLIENT_VERSION)
        .header("Origin", "https://www.youtube.com")
        .send_json(&body)?;
    if !resp.status().is_success() {
        bail!("YouTube returned HTTP {}", resp.status());
    }
    Ok(resp.body_mut().with_config().limit(64 * 1024 * 1024).read_json()?)
}

/// Single video metadata.
pub fn video(id: &str) -> Result<Track> {
    match video_innertube(id) {
        Ok(t) => Ok(t),
        Err(e) => {
            eprintln!("innertube player failed for {id}: {e:#}; trying oEmbed");
            video_oembed(id)
        }
    }
}

fn video_innertube(id: &str) -> Result<Track> {
    let resp = innertube("player", json!({ "videoId": id }))?;
    let details = resp.get("videoDetails").context("no videoDetails in response")?;
    let title = details["title"].as_str().context("no title")?.to_string();
    let artist = details["author"].as_str().unwrap_or_default().trim_end_matches(" - Topic").to_string();
    let duration = details["lengthSeconds"].as_str().and_then(|s| s.parse().ok());
    Ok(Track::new(Platform::YouTube, id, watch_url(id), title, artist, duration))
}

fn video_oembed(id: &str) -> Result<Track> {
    let url = format!("https://www.youtube.com/oembed?format=json&url={}", watch_url(id));
    let mut resp = http().get(&url).call()?;
    if !resp.status().is_success() {
        bail!("Video not found or unavailable (HTTP {})", resp.status());
    }
    let v: Value = resp.body_mut().read_json()?;
    let title = v["title"].as_str().context("no title")?.to_string();
    let artist = v["author_name"].as_str().unwrap_or_default().to_string();
    Ok(Track::new(Platform::YouTube, id, watch_url(id), title, artist, None))
}

/// Fetch a public/unlisted playlist: (title, tracks).
pub fn playlist(list: &str) -> Result<(String, Vec<Track>)> {
    let browse_id = if list.starts_with("VL") { list.to_string() } else { format!("VL{list}") };
    let first = innertube("browse", json!({ "browseId": browse_id }))?;

    if let Some(alert) = find_alert(&first) {
        bail!("YouTube: {alert}");
    }
    let title = first
        .pointer("/metadata/playlistMetadataRenderer/title")
        .and_then(Value::as_str)
        .or_else(|| {
            first
                .pointer("/header/pageHeaderRenderer/pageTitle")
                .and_then(Value::as_str)
        })
        .unwrap_or("YouTube playlist")
        .to_string();

    let mut tracks = Vec::new();
    let mut token = collect_page(first.get("contents").unwrap_or(&Value::Null), &mut tracks);
    while let Some(t) = token.take() {
        if tracks.len() >= MAX_ITEMS {
            break;
        }
        let page = innertube("browse", json!({ "continuation": t }))?;
        let before = tracks.len();
        token = collect_page(page.get("onResponseReceivedActions").unwrap_or(&Value::Null), &mut tracks);
        if tracks.len() == before {
            break;
        }
    }
    if tracks.is_empty() {
        bail!("Playlist is empty, private, or could not be read");
    }
    Ok((title, tracks))
}

fn find_alert(v: &Value) -> Option<String> {
    let alerts = v.get("alerts")?.as_array()?;
    alerts.iter().find_map(|a| {
        let r = a.get("alertRenderer").or_else(|| a.get("alertWithButtonRenderer"))?;
        if r["type"].as_str() != Some("ERROR") {
            return None;
        }
        let text = &r["text"];
        text["simpleText"].as_str().map(str::to_string).or_else(|| {
            Some(
                text["runs"]
                    .as_array()?
                    .iter()
                    .filter_map(|r| r["text"].as_str())
                    .collect::<String>(),
            )
        })
    })
}

/// Walks a response subtree in document order, collecting videos and returning
/// the continuation token for the next page, if any.
fn collect_page(root: &Value, out: &mut Vec<Track>) -> Option<String> {
    let mut token = None;
    walk(root, out, &mut token);
    token
}

fn walk(v: &Value, out: &mut Vec<Track>, token: &mut Option<String>) {
    match v {
        Value::Object(map) => {
            if let Some(lockup) = map.get("lockupViewModel") {
                if let Some(t) = from_lockup(lockup) {
                    out.push(t);
                }
                return;
            }
            if let Some(r) = map.get("playlistVideoRenderer") {
                if let Some(t) = from_playlist_video_renderer(r) {
                    out.push(t);
                }
                return;
            }
            if let Some(c) = map.get("continuationItemViewModel").or(map.get("continuationItemRenderer")) {
                if token.is_none() {
                    *token = find_token(c);
                }
                return;
            }
            for child in map.values() {
                walk(child, out, token);
            }
        }
        Value::Array(items) => items.iter().for_each(|i| walk(i, out, token)),
        _ => {}
    }
}

fn find_token(v: &Value) -> Option<String> {
    match v {
        Value::Object(map) => {
            if let Some(t) = map.get("continuationCommand").and_then(|c| c["token"].as_str()) {
                return Some(t.to_string());
            }
            map.values().find_map(find_token)
        }
        Value::Array(items) => items.iter().find_map(find_token),
        _ => None,
    }
}

fn is_unavailable(title: &str) -> bool {
    matches!(title, "[Deleted video]" | "[Private video]" | "[Unavailable video]")
}

fn from_lockup(l: &Value) -> Option<Track> {
    if l["contentType"].as_str() != Some("LOCKUP_CONTENT_TYPE_VIDEO") {
        return None;
    }
    let id = l["contentId"].as_str().filter(|id| is_video_id(id))?;
    let meta = l.pointer("/metadata/lockupMetadataViewModel")?;
    let title = meta.pointer("/title/content")?.as_str()?;
    if is_unavailable(title) {
        return None;
    }
    let artist = meta
        .pointer("/metadata/contentMetadataViewModel/metadataRows/0/metadataParts/0/text/content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let duration = l
        .pointer("/contentImage/thumbnailViewModel/overlays")
        .and_then(Value::as_array)
        .and_then(|overlays| {
            overlays.iter().find_map(|o| {
                o.pointer("/thumbnailBottomOverlayViewModel/badges")?
                    .as_array()?
                    .iter()
                    .find_map(|b| parse_clock(b.pointer("/thumbnailBadgeViewModel/text")?.as_str()?))
            })
        });
    Some(Track::new(Platform::YouTube, id, watch_url(id), title, artist.trim_end_matches(" - Topic"), duration))
}

fn from_playlist_video_renderer(r: &Value) -> Option<Track> {
    let id = r["videoId"].as_str().filter(|id| is_video_id(id))?;
    let title = runs_text(&r["title"])?;
    if is_unavailable(&title) {
        return None;
    }
    let artist = runs_text(&r["shortBylineText"]).unwrap_or_default();
    let duration = r["lengthSeconds"].as_str().and_then(|s| s.parse().ok());
    Some(Track::new(Platform::YouTube, id, watch_url(id), title, artist.trim_end_matches(" - Topic"), duration))
}

fn runs_text(v: &Value) -> Option<String> {
    if let Some(s) = v["simpleText"].as_str() {
        return Some(s.to_string());
    }
    let runs = v["runs"].as_array()?;
    Some(runs.iter().filter_map(|r| r["text"].as_str()).collect())
}

/// Parse "3:55" / "1:02:03" into seconds.
fn parse_clock(s: &str) -> Option<u32> {
    let mut total = 0u32;
    let mut parts = 0;
    for p in s.trim().split(':') {
        total = total * 60 + p.parse::<u32>().ok()?;
        parts += 1;
    }
    (parts >= 2).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock() {
        assert_eq!(parse_clock("3:55"), Some(235));
        assert_eq!(parse_clock("1:02:03"), Some(3723));
        assert_eq!(parse_clock("LIVE"), None);
        assert_eq!(parse_clock("12"), None);
    }

    #[test]
    #[ignore = "network"]
    fn live_playlist() {
        let (title, tracks) = playlist("PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI").unwrap();
        println!("{title}: {} tracks; first {:?}", tracks.len(), tracks.first());
        assert!(tracks.len() > 100, "pagination should fetch more than one page");
    }

    #[test]
    #[ignore = "network"]
    fn live_video() {
        let t = video("dQw4w9WgXcQ").unwrap();
        println!("{t:?}");
        assert_eq!(t.duration_secs, Some(213));
    }
}
