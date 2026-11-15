//! lrc.red: a lyrics database with a public, keyless API. Songs are found by
//! a text search and identified by ISRC; their lyrics come as Lyricsfiles,
//! usually timed word by word. The fallback when LRCLIB has nothing.

use anyhow::{Result, bail};
use serde::Deserialize;

use super::query::{self, Query};
use super::{Lyrics, SYNC_TOLERANCE_SECS, Source, formats};

const SITE: &str = "https://lrc.red";

#[derive(Debug, Deserialize)]
struct Search {
    #[serde(default)]
    hits: Vec<Hit>,
}

#[derive(Debug, Deserialize)]
struct Hit {
    isrc: String,
    title: String,
    artist: String,
    #[serde(default)]
    duration: Option<f64>,
}

/// Lyrics for the first guess lrc.red knows.
pub(super) fn find(guesses: &[Query], duration_secs: Option<u32>) -> Result<Option<Lyrics>> {
    let q = &guesses[0];
    let mut resp = crate::sources::http()
        .get(format!("{SITE}/search.json"))
        .query("q", format!("{} {}", q.title, q.artist))
        .call()?;
    match resp.status().as_u16() {
        200 => {}
        404 => return Ok(None),
        s => bail!("search: HTTP {s}"),
    }
    let search: Search = resp.body_mut().read_json()?;
    let Some(hit) = pick(search.hits, guesses, duration_secs) else { return Ok(None) };
    let mut resp = crate::sources::http().get(format!("{SITE}/s/{}.lyricsfile.yaml", hit.isrc)).call()?;
    match resp.status().as_u16() {
        200 => {}
        404 => return Ok(None),
        s => bail!("lyrics: HTTP {s}"),
    }
    let text = resp.body_mut().with_config().limit(4 << 20).read_to_string()?;
    let Some(file) = formats::parse_lyricsfile(&text) else { return Ok(None) };
    if file.instrumental {
        return Ok(Some(Lyrics::instrumental(Source::LrcRed)));
    }
    if file.lines.is_empty() {
        return Ok(file.plain.as_deref().and_then(|p| Lyrics::plain(Source::LrcRed, p)));
    }
    let lyrics = Lyrics { source: Source::LrcRed, synced: true, instrumental: false, lines: file.lines };
    let fits = off(&hit, duration_secs).is_none_or(|d| d <= SYNC_TOLERANCE_SECS);
    Ok(Some(if fits { lyrics } else { lyrics.unsynced() }))
}

fn off(hit: &Hit, duration_secs: Option<u32>) -> Option<f64> {
    Some((hit.duration? - duration_secs? as f64).abs())
}

/// The hit that is the song asked for (title and artist of a guess), closest
/// in length. Only well-formed ISRCs, as they go into the lyrics URL.
fn pick(hits: Vec<Hit>, guesses: &[Query], duration_secs: Option<u32>) -> Option<Hit> {
    hits.into_iter()
        .filter(|h| is_isrc(&h.isrc))
        .filter(|h| {
            guesses.iter().any(|q| query::same_title(&h.title, &q.title) && query::same_artist(&h.artist, &q.artist))
        })
        .min_by_key(|h| (off(h, duration_secs).unwrap_or(0.0) * 10.0) as u64)
}

/// `CC-XXX-YY-NNNNN` without dashes: 2 letters, 3 letters or digits, 7 digits.
fn is_isrc(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 12
        && b[..2].iter().all(u8::is_ascii_uppercase)
        && b[2..5].iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && b[5..].iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(isrc: &str, title: &str, artist: &str, duration: f64) -> Hit {
        Hit { isrc: isrc.into(), title: title.into(), artist: artist.into(), duration: Some(duration) }
    }

    #[test]
    fn picks_the_song_closest_in_length() {
        let hits = vec![
            hit("GBAAA0000001", "Song (Pianoforte)", "Band", 200.0),
            hit("GBAAA0000002", "Song", "Band", 260.0),
            hit("GBAAA0000003", "Song (2022 Remaster)", "Band", 201.0),
            hit("../../x", "Song", "Band", 200.0),
            hit("GBAAA0000004", "Song", "Other", 200.0),
        ];
        let q = [Query { artist: "Band".into(), title: "Song".into() }];
        assert_eq!(pick(hits, &q, Some(200)).map(|h| h.isrc), Some("GBAAA0000003".into()));
        assert!(pick(Vec::new(), &q, Some(200)).is_none());
    }

    #[test]
    fn checks_isrcs() {
        assert!(is_isrc("GBARL9300135"));
        assert!(!is_isrc("gbarl9300135"));
        assert!(!is_isrc("GBARL930013"));
        assert!(!is_isrc("GB/RL9300135"));
    }

    #[test]
    fn parses_its_lyricsfiles() {
        // lrc.red's style: quoted strings, an indented list, word end times.
        let file = "version: '1.0'\nmetadata:\n  title: \"Made Up\"\n  artist: \"Nobody\"\n  duration_ms: 200000\n\
            lines:\n  - text: \"Hello there\"\n    start_ms: 1000\n    end_ms: 2000\n    words:\n      - text: \"Hello \"\n\
            \x20       start_ms: 1000\n        end_ms: 1500\n      - text: \"there\"\n        start_ms: 1500\n\
            \x20       end_ms: 2000\n";
        let t = formats::parse_lyricsfile(file).unwrap();
        assert_eq!(t.lines.len(), 1);
        assert_eq!(t.lines[0].text, "Hello there");
        assert_eq!(t.lines[0].words.iter().map(|w| w.end).collect::<Vec<_>>(), [6, 11]);
    }

    #[test]
    #[ignore = "network"]
    fn live_lrcred_word_timings() {
        let q = Query { artist: "Rick Astley".into(), title: "Never Gonna Give You Up".into() };
        let l = find(&[q], Some(213)).unwrap().expect("lyrics");
        assert_eq!(l.source, Source::LrcRed);
        assert!(l.has_words());
    }
}
