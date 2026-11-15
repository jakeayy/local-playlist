//! LRCLIB (lrclib.net): an open, community-run lyrics database with a public
//! API that needs no key. Entries carry plain lyrics, LRC timed lyrics and a
//! "Lyricsfile" with word timings when someone has added them.

use anyhow::{Result, bail};
use serde::Deserialize;

use super::formats;
use super::query::{self, Query};
use super::{Line, Lyrics, SYNC_TOLERANCE_SECS, Source};

const API: &str = "https://lrclib.net/api";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    track_name: String,
    artist_name: String,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    instrumental: bool,
    #[serde(default)]
    plain_lyrics: Option<String>,
    #[serde(default)]
    synced_lyrics: Option<String>,
    #[serde(default)]
    lyricsfile: Option<String>,
}

impl Record {
    fn timed(&self) -> Option<Vec<Line>> {
        let file = self.lyricsfile.as_deref().and_then(formats::parse_lyricsfile).map(|t| t.lines);
        let lines = file.filter(|l| !l.is_empty()).or_else(|| self.synced_lyrics.as_deref().map(formats::parse_lrc))?;
        (!lines.is_empty()).then_some(lines)
    }

    /// How much longer (negative: shorter) this entry is than the track.
    fn length_diff(&self, duration_secs: Option<u32>) -> Option<f64> {
        Some(self.duration? - duration_secs? as f64)
    }

    fn duration_off(&self, duration_secs: Option<u32>) -> Option<f64> {
        self.length_diff(duration_secs).map(f64::abs)
    }

    /// Lyrics from this entry: timed if it has timings (marked as for
    /// another cut if its length doesn't match the track's), plain otherwise.
    fn lyrics(&self, duration_secs: Option<u32>) -> Option<Lyrics> {
        if self.instrumental {
            return Some(Lyrics::instrumental(Source::Lrclib));
        }
        if let Some(lines) = self.timed() {
            return Some(Lyrics::timed(Source::Lrclib, lines, self.length_diff(duration_secs)));
        }
        Lyrics::plain(Source::Lrclib, self.plain_lyrics.as_deref()?)
    }

    fn has_lyrics(&self) -> bool {
        self.instrumental
            || self.plain_lyrics.as_deref().is_some_and(|p| !p.trim().is_empty())
            || self.synced_lyrics.as_deref().is_some_and(|p| !p.trim().is_empty())
    }
}

/// Lyrics for the first guess LRCLIB knows.
pub(super) fn find(guesses: &[Query], duration_secs: Option<u32>) -> Result<Option<Lyrics>> {
    // Exact lookups first: LRCLIB matches the length itself (±2 s).
    if let Some(d) = duration_secs {
        for q in guesses.iter().take(2) {
            let req = crate::sources::http()
                .get(format!("{API}/get"))
                .query("track_name", &q.title)
                .query("artist_name", &q.artist)
                .query("duration", d.to_string());
            if let Some(rec) = get::<Record>(req)?
                && let Some(l) = rec.lyrics(duration_secs)
            {
                return Ok(Some(l));
            }
        }
    }
    // Then a search, for other lengths (music videos) and spellings.
    let q = &guesses[0];
    let by_field = crate::sources::http()
        .get(format!("{API}/search"))
        .query("track_name", &q.title)
        .query("artist_name", &q.artist);
    if let Some(l) = best(get::<Vec<Record>>(by_field)?.unwrap_or_default(), guesses, duration_secs) {
        return Ok(Some(l));
    }
    let free = crate::sources::http().get(format!("{API}/search")).query("q", format!("{} {}", q.artist, q.title));
    Ok(best(get::<Vec<Record>>(free)?.unwrap_or_default(), guesses, duration_secs))
}

/// GET a JSON body; `None` on 404.
fn get<T: serde::de::DeserializeOwned>(req: ureq::RequestBuilder<ureq::typestate::WithoutBody>) -> Result<Option<T>> {
    let mut resp = req.call()?;
    match resp.status().as_u16() {
        200 => Ok(Some(resp.body_mut().read_json()?)),
        404 => Ok(None),
        s => bail!("HTTP {s}"),
    }
}

/// The search result that best matches a guess: right title and artist,
/// then timed lyrics at the track's length, then the closest length.
fn best(records: Vec<Record>, guesses: &[Query], duration_secs: Option<u32>) -> Option<Lyrics> {
    let matches = |r: &Record| {
        guesses
            .iter()
            .any(|q| query::same_title(&r.track_name, &q.title) && query::same_artist(&r.artist_name, &q.artist))
    };
    let rank = |r: &Record| {
        let off = r.duration_off(duration_secs);
        let fits = off.is_none_or(|d| d <= SYNC_TOLERANCE_SECS);
        let timed = r.synced_lyrics.as_deref().is_some_and(|s| !s.trim().is_empty());
        let words = r.lyricsfile.as_deref().is_some_and(|f| f.contains("words:") || f.contains('<'));
        // Lower is better.
        (!(fits && timed), !(fits && words), (off.unwrap_or(0.0) * 10.0) as u64)
    };
    records.into_iter().filter(|r| r.has_lyrics() && matches(r)).min_by_key(rank)?.lyrics(duration_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(title: &str, artist: &str, duration: f64, synced: Option<&str>, plain: Option<&str>) -> Record {
        Record {
            track_name: title.into(),
            artist_name: artist.into(),
            duration: Some(duration),
            instrumental: false,
            plain_lyrics: plain.map(Into::into),
            synced_lyrics: synced.map(Into::into),
            lyricsfile: None,
        }
    }

    fn guess(artist: &str, title: &str) -> Vec<Query> {
        vec![Query { artist: artist.into(), title: title.into() }]
    }

    #[test]
    fn marks_timings_for_another_cut() {
        let r = record("Song", "Band", 200.0, Some("[00:01.00]La"), Some("La"));
        assert_eq!(r.lyrics(Some(201)).unwrap().other_cut, None);
        assert_eq!(r.lyrics(None).unwrap().other_cut, None);
        // The track is a music video a minute longer than the song.
        let off = r.lyrics(Some(260)).unwrap();
        assert!(off.synced);
        assert_eq!(off.other_cut, Some(-60));
        assert_eq!(off.lines[0].start_ms, 1000);
    }

    #[test]
    fn picks_the_matching_timed_entry() {
        let records = vec![
            record("Other Song", "Band", 200.0, Some("[00:01.00]No"), None),
            record("Song", "Band", 230.0, Some("[00:01.00]Far"), None),
            record("Song", "Band", 201.0, None, Some("Plain")),
            record("Band - Song", "Band", 199.0, Some("[00:01.00]Near"), None),
            record("Song", "Someone Else", 200.0, Some("[00:01.00]Wrong artist"), None),
        ];
        let l = best(records, &guess("Band", "Song"), Some(200)).unwrap();
        assert!(l.synced && l.other_cut.is_none());
        assert_eq!(l.lines[0].text, "Near");
        assert!(best(Vec::new(), &guess("Band", "Song"), Some(200)).is_none());
    }

    #[test]
    fn parses_api_records() {
        let json = r#"{"id":1,"name":"Song","trackName":"Song","artistName":"Band","albumName":"A","duration":200.0,
            "instrumental":false,"hasWordSync":false,"plainLyrics":"La\nLa","syncedLyrics":"[00:01.00]La\n[00:02.00]La",
            "lyricsfile":null}"#;
        let r: Record = serde_json::from_str(json).unwrap();
        assert_eq!(r.lyrics(Some(200)).unwrap().lines.len(), 2);
    }

    #[test]
    #[ignore = "network"]
    fn live_lrclib_word_timings() {
        let l = find(&guess("Rick Astley", "Never Gonna Give You Up"), Some(213)).unwrap().expect("lyrics");
        assert!(l.synced);
        assert!(l.has_words(), "LRCLIB has word timings for this one");
    }
}
