//! SponsorBlock: community-submitted segments of YouTube videos (sponsors,
//! intros, non-music parts…) that the player skips over.
//!
//! The audio itself is never cut, so file lengths and track durations stay as
//! the platform reports them; playback just jumps past the segments.
//!
//! Lookups use SponsorBlock's privacy-preserving endpoint: only the first
//! four hex characters of the video id's SHA-256 are sent, and the matching
//! video is picked out of the returned batch locally.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub category: String,
    pub start: f64,
    pub end: f64,
}

/// The categories skipped (SponsorBlock's API names): all of them. Non-music
/// parts, sponsors, self-promotion, "like and subscribe" reminders, intros,
/// outros, previews and recaps, and filler.
pub const CATEGORIES: &[&str] =
    &["music_offtopic", "sponsor", "selfpromo", "interaction", "intro", "outro", "preview", "filler"];

/// Segments shorter than this aren't worth a seek.
const MIN_LEN: f64 = 1.0;

/// Where to jump to when playback is at `pos` (seconds), if inside a
/// segment. Overlapping and touching segments are chained, so one jump clears
/// them all.
pub fn skip_target(segments: &[Segment], pos: f64) -> Option<f64> {
    let mut target: Option<f64> = None;
    for (start, end) in skipped_ranges(segments) {
        let at = target.unwrap_or(pos);
        // Small slack at the end so we never re-trigger right after a jump.
        if start <= at + 0.05 && at < end - 0.3 {
            target = Some(end);
        }
    }
    target
}

/// The (start, end) ranges that get skipped, sorted, with overlapping and
/// touching segments merged (what the player bar shows).
pub fn skipped_ranges(segments: &[Segment]) -> Vec<(f64, f64)> {
    let mut ranges: Vec<(f64, f64)> = segments
        .iter()
        // A category SponsorBlock adds later isn't skipped unasked.
        .filter(|s| s.end - s.start >= MIN_LEN && CATEGORIES.contains(&s.category.as_str()))
        .map(|s| (s.start, s.end))
        .collect();
    ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 + 0.05 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

#[cfg(not(target_family = "wasm"))]
const API: &str = "https://sponsor.ajay.app/api/skipSegments";

/// Fetch every skippable segment of a YouTube video (blocking).
/// An unknown video simply has none.
#[cfg(not(target_family = "wasm"))]
pub fn fetch(video_id: &str) -> anyhow::Result<Vec<Segment>> {
    use sha2::Digest as _;
    let hash: String = sha2::Sha256::digest(video_id.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    let categories = serde_json::to_string(CATEGORIES)?;
    let mut resp = crate::sources::http()
        .get(format!("{API}/{}", &hash[..4]))
        .query("categories", &categories)
        .query("actionTypes", r#"["skip"]"#)
        .call()?;
    match resp.status().as_u16() {
        404 => return Ok(Vec::new()),
        200 => {}
        s => anyhow::bail!("SponsorBlock returned HTTP {s}"),
    }
    let body: serde_json::Value = resp.body_mut().read_json()?;
    Ok(parse(&body, video_id))
}

/// Pick `video_id`'s segments out of a hash-prefix response.
#[cfg(not(target_family = "wasm"))]
fn parse(body: &serde_json::Value, video_id: &str) -> Vec<Segment> {
    let Some(video) = body.as_array().and_then(|a| a.iter().find(|v| v["videoID"] == video_id)) else {
        return Vec::new();
    };
    video["segments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["actionType"].as_str().is_none_or(|a| a == "skip"))
        .filter_map(|s| {
            let range = s["segment"].as_array()?;
            Some(Segment {
                category: s["category"].as_str()?.to_string(),
                start: range.first()?.as_f64()?,
                end: range.get(1)?.as_f64()?,
            })
        })
        .filter(|s| s.end > s.start)
        .collect()
}

/// On-disk cache so skipping keeps working offline.
#[cfg(not(target_family = "wasm"))]
pub mod cache {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use serde::{Deserialize, Serialize};

    use super::Segment;

    #[derive(Clone, Serialize, Deserialize)]
    pub struct Entry {
        pub fetched_ms: f64,
        pub segments: Vec<Segment>,
    }

    pub fn file(paths: &crate::model::Paths) -> PathBuf {
        paths.cache.join("sponsorblock.json")
    }

    pub fn load(path: &Path) -> HashMap<String, Entry> {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(path: &Path, map: &HashMap<String, Entry>) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        if let Ok(bytes) = serde_json::to_vec(map)
            && std::fs::write(&tmp, bytes).is_ok()
        {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(category: &str, start: f64, end: f64) -> Segment {
        Segment { category: category.into(), start, end }
    }

    #[test]
    fn skips_known_categories_only() {
        let segs = [seg("sponsor", 10., 20.), seg("intro", 0., 5.), seg("something_new", 30., 40.)];
        assert_eq!(skip_target(&segs, 2.), Some(5.));
        assert_eq!(skip_target(&segs, 9.), None);
        assert_eq!(skip_target(&segs, 10.), Some(20.));
        assert_eq!(skip_target(&segs, 15.), Some(20.));
        assert_eq!(skip_target(&segs, 19.8), None);
        assert_eq!(skip_target(&segs, 25.), None);
        assert_eq!(skip_target(&segs, 32.), None);
    }

    #[test]
    fn chains_overlapping_segments() {
        let segs = [seg("music_offtopic", 30., 45.), seg("sponsor", 10., 31.), seg("selfpromo", 45., 50.)];
        assert_eq!(skip_target(&segs, 12.), Some(50.));
    }

    #[test]
    fn merges_ranges_for_display() {
        let segs = [
            seg("selfpromo", 45., 50.),
            seg("sponsor", 10., 31.),
            seg("music_offtopic", 30., 45.),
            seg("intro", 0., 5.),
        ];
        assert_eq!(skipped_ranges(&segs), vec![(0., 5.), (10., 50.)]);
    }

    #[test]
    fn ignores_tiny_segments() {
        assert_eq!(skip_target(&[seg("sponsor", 10., 10.5)], 10.1), None);
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    #[ignore = "network"]
    fn fetches_live_segments() {
        fetch("aQp-iM0bCs4").unwrap();
        assert!(fetch("dQw4w9WgXcQ").is_ok());
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn parses_hash_prefix_response() {
        let body = serde_json::json!([
            { "videoID": "other", "segments": [{ "category": "sponsor", "segment": [1.0, 2.0], "actionType": "skip" }] },
            { "videoID": "abc", "segments": [
                { "category": "sponsor", "segment": [5.5, 9.0], "actionType": "skip" },
                { "category": "sponsor", "segment": [12.0, 30.0], "actionType": "mute" }
            ] }
        ]);
        assert_eq!(parse(&body, "abc"), vec![seg("sponsor", 5.5, 9.0)]);
        assert!(parse(&body, "missing").is_empty());
    }
}
