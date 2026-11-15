//! Lyrics for the playing track: timed (word by word or line by line) for the
//! karaoke view when available, plain text otherwise.
//!
//! Sources, tried in order, both public APIs used without keys, tokens or
//! accounts:
//! * LRCLIB (lrclib.net), an open, community-run lyrics database: timed
//!   lyrics, often word by word, or plain ones (`lrclib.rs`).
//! * lrc.red, a lyrics database searched by title and artist: timed lyrics,
//!   usually word by word (`lrcred.rs`).
//!
//! (Musixmatch needs a session token even for its desktop app's API, and
//! Genius's website search sits behind a bot challenge that only real
//! browsers pass, so neither is used.)
//!
//! Privacy: a lookup sends the track's title, artist and length to these
//! services (unlike SponsorBlock, neither offers a lookup that hides what is
//! asked for). So the app looks lyrics up only while the lyrics panel is open,
//! sends no cookies, and caches the result on disk, found or not, so each
//! track is asked about once.
//!
//! Uploads rarely carry clean metadata ("Artist - Title (Official Video)" by
//! "ArtistVEVO"), so `query.rs` first guesses the real artist and title.

pub mod formats;
#[cfg(not(target_family = "wasm"))]
mod lrclib;
#[cfg(not(target_family = "wasm"))]
mod lrcred;
pub mod query;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Lrclib,
    LrcRed,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Lrclib => "LRCLIB",
            Source::LrcRed => "lrc.red",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lyrics {
    pub source: Source,
    /// Lines have start times (and maybe word timings); otherwise plain text.
    pub synced: bool,
    /// The song has no lyrics (and `lines` is empty).
    #[serde(default)]
    pub instrumental: bool,
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line {
    /// When the line starts, in milliseconds (0 in plain lyrics).
    #[serde(default)]
    pub start_ms: u32,
    /// When it stops being sung, if the source says (Lyricsfiles do; LRC
    /// only has starts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<u32>,
    pub text: String,
    /// Word timings, in order; they split `text` without gaps. Empty when
    /// only the line is timed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<Word>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub start_ms: u32,
    /// Byte offset in the line's text where this word (and its trailing
    /// space) ends.
    pub end: u32,
}

impl Lyrics {
    /// Plain lyrics from text, one line per line (blank lines kept: they
    /// separate verses).
    pub fn plain(source: Source, text: &str) -> Option<Self> {
        let mut lines: Vec<Line> = Vec::new();
        for l in text.lines().map(str::trim) {
            // At most one blank line in a row.
            if !l.is_empty() || lines.last().is_some_and(|p| !p.text.is_empty()) {
                lines.push(Line { start_ms: 0, end_ms: None, text: l.to_string(), words: Vec::new() });
            }
        }
        while lines.last().is_some_and(|l| l.text.is_empty()) {
            lines.pop();
        }
        (!lines.is_empty()).then_some(Self { source, synced: false, instrumental: false, lines })
    }

    pub fn instrumental(source: Source) -> Self {
        Self { source, synced: false, instrumental: true, lines: Vec::new() }
    }

    /// The same text without timings.
    pub fn unsynced(mut self) -> Self {
        self.synced = false;
        for l in &mut self.lines {
            l.start_ms = 0;
            l.end_ms = None;
            l.words.clear();
        }
        self
    }

    /// Whether any line has word timings.
    pub fn has_words(&self) -> bool {
        self.synced && self.lines.iter().any(|l| !l.words.is_empty())
    }

    /// The line being sung at `ms`, if the lyrics are timed and it has started.
    pub fn line_at(&self, ms: u32) -> Option<usize> {
        if !self.synced {
            return None;
        }
        self.lines.partition_point(|l| l.start_ms <= ms).checked_sub(1)
    }

    /// How much (0..1, in characters) of line `i` has been sung at `ms`.
    ///
    /// With word timings each word fills from its start to the next word's;
    /// without, the whole line fills at a steady pace. Either way the fill
    /// runs to the line's end (where the next line starts, or earlier if the
    /// source gives an end) and gets there a little early, so the line can
    /// still be read to the end before the next takes over.
    pub fn line_progress(&self, i: usize, ms: u32) -> f32 {
        let Some(line) = self.lines.get(i) else { return 0.0 };
        if line.words.is_empty() {
            let end = self.line_end(i, line.start_ms, &line.text);
            return ramp(line.start_ms, read_ahead(line.start_ms, end), ms);
        }
        let chars = line.text.chars().count();
        let current = line.words.partition_point(|w| w.start_ms <= ms);
        let (Some(k), true) = (current.checked_sub(1), chars > 0) else { return 0.0 };
        let word = line.words[k];
        let from = if k == 0 { 0 } else { line.words[k - 1].end as usize };
        let (from, to) = (from.min(line.text.len()), (word.end as usize).min(line.text.len()));
        let (Some(before), Some(own)) = (line.text.get(..from), line.text.get(from..to)) else { return 1.0 };
        let t = match line.words.get(k + 1) {
            Some(next) => ramp(word.start_ms, next.start_ms, ms),
            // The last word ends with the line, like a line without words.
            None => ramp(word.start_ms, read_ahead(word.start_ms, self.line_end(i, word.start_ms, own)), ms),
        };
        (before.chars().count() as f32 + t * own.chars().count() as f32) / chars as f32
    }

    /// When line `i` ends: when the source says, else when the next line
    /// starts, but without an end time no later than `text` sung slowly from
    /// `from` (a long gap before the next line is a break, not slow singing).
    fn line_end(&self, i: usize, from: u32, text: &str) -> u32 {
        let line = &self.lines[i];
        let next = self.lines.get(i + 1).map(|l| l.start_ms);
        let slowest = from.saturating_add((text.chars().count() as u32 * 250).clamp(2500, 12_000));
        match (line.end_ms.filter(|&e| e > line.start_ms), next) {
            (Some(e), Some(n)) => e.min(n),
            (Some(e), None) => e,
            (None, Some(n)) => n.min(slowest),
            (None, None) => slowest,
        }
    }
}

/// A fill from `start` to `end` finishes this much early, and at most a fifth
/// of its length early.
const READ_AHEAD_MS: u32 = 800;

fn read_ahead(start: u32, end: u32) -> u32 {
    let span = end.saturating_sub(start);
    start + span - (span / 5).min(READ_AHEAD_MS)
}

/// 0 at `start`, 1 at `end`, linear between.
fn ramp(start: u32, end: u32, ms: u32) -> f32 {
    if end <= start {
        return 1.0;
    }
    (ms.saturating_sub(start) as f32 / (end - start) as f32).clamp(0.0, 1.0)
}

impl Line {
    /// A section label such as "[Chorus]" (some plain lyrics have them).
    pub fn is_label(&self) -> bool {
        self.text.len() > 2 && self.text.starts_with('[') && self.text.ends_with(']')
    }
}

/// Timed lyrics further off than this from the track's length were timed for
/// another cut (a music video with an intro, a radio edit): shown untimed.
pub const SYNC_TOLERANCE_SECS: f64 = 3.0;

/// What a lookup found.
#[cfg(not(target_family = "wasm"))]
pub struct Found {
    pub lyrics: Option<Lyrics>,
    /// Every source that was needed answered, so this is worth caching.
    /// (lrc.red's lyrics while LRCLIB was unreachable are shown, but LRCLIB
    /// is asked again next time, as it may have better ones.)
    pub complete: bool,
}

/// Look up lyrics for a track (blocking; a handful of requests at most).
/// An error means no source could be asked (offline, blocked).
#[cfg(not(target_family = "wasm"))]
pub fn lookup(title: &str, artist: &str, duration_secs: Option<u32>) -> anyhow::Result<Found> {
    let guesses = query::guesses(title, artist);
    if guesses.is_empty() {
        return Ok(Found { lyrics: None, complete: true });
    }
    let lrclib = match lrclib::find(&guesses, duration_secs) {
        Ok(Some(l)) => return Ok(Found { lyrics: Some(l), complete: true }),
        Ok(None) => Ok(()),
        Err(e) => Err(e.context("LRCLIB")),
    };
    match (lrcred::find(&guesses, duration_secs), lrclib) {
        (Ok(lyrics), Ok(())) => Ok(Found { lyrics, complete: true }),
        (Ok(Some(l)), Err(_)) => Ok(Found { lyrics: Some(l), complete: false }),
        // Nothing found, but LRCLIB wasn't asked: not a real "no lyrics".
        (Ok(None) | Err(_), Err(e)) => Err(e),
        (Err(e), Ok(())) => Err(e.context("lrc.red")),
    }
}

/// Lyrics of a track: from the cache, or looked up (and cached when the
/// answer is final). Blocking.
#[cfg(not(target_family = "wasm"))]
pub fn for_track(paths: &crate::model::Paths, track: &crate::model::Track) -> anyhow::Result<Option<Lyrics>> {
    let (dir, key) = (cache::dir(paths), track.file_key());
    if let Some(cached) = cache::get(&dir, &key) {
        return Ok(cached);
    }
    let found = lookup(&track.title, &track.artist, track.duration_secs)?;
    if found.complete {
        cache::put(&dir, &key, &found.lyrics);
    }
    Ok(found.lyrics)
}

/// On-disk cache: one small JSON file per track, so a track is looked up
/// once and its lyrics work offline.
#[cfg(not(target_family = "wasm"))]
pub mod cache {
    use std::path::{Path, PathBuf};

    use serde::{Deserialize, Serialize};

    use super::Lyrics;

    /// "Not found" is asked again after this long (lyrics get added).
    const MISS_MAX_AGE_MS: f64 = 7.0 * 24.0 * 3600.0 * 1000.0;
    /// Found lyrics are refreshed after this long (timings get improved).
    const HIT_MAX_AGE_MS: f64 = 90.0 * 24.0 * 3600.0 * 1000.0;
    /// Files kept; the oldest go beyond that (each is a few kB).
    const MAX_FILES: usize = 3000;

    #[derive(Serialize, Deserialize)]
    pub struct Entry {
        pub fetched_ms: f64,
        pub lyrics: Option<Lyrics>,
    }

    pub fn dir(paths: &crate::model::Paths) -> PathBuf {
        paths.cache.join("lyrics")
    }

    fn file(dir: &Path, key: &str) -> PathBuf {
        dir.join(format!("{key}.json"))
    }

    /// Cached answer for `key` (a track's file key) if still fresh.
    pub fn get(dir: &Path, key: &str) -> Option<Option<Lyrics>> {
        let e: Entry = serde_json::from_slice(&std::fs::read(file(dir, key)).ok()?).ok()?;
        let max_age = if e.lyrics.is_some() { HIT_MAX_AGE_MS } else { MISS_MAX_AGE_MS };
        (crate::now_ms() - e.fetched_ms < max_age).then_some(e.lyrics)
    }

    pub fn put(dir: &Path, key: &str, lyrics: &Option<Lyrics>) {
        let _ = std::fs::create_dir_all(dir);
        let path = file(dir, key);
        let tmp = path.with_extension("json.tmp");
        let entry = Entry { fetched_ms: crate::now_ms(), lyrics: lyrics.clone() };
        if let Ok(bytes) = serde_json::to_vec(&entry)
            && std::fs::write(&tmp, bytes).is_ok()
        {
            let _ = std::fs::rename(&tmp, &path);
        }
        prune(dir, MAX_FILES);
    }

    /// Delete the least recently written files beyond `keep`.
    fn prune(dir: &Path, keep: usize) {
        let Ok(it) = std::fs::read_dir(dir) else { return };
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = it
            .flatten()
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .filter(|(_, p)| p.extension().is_some_and(|x| x == "json"))
            .collect();
        if files.len() <= keep {
            return;
        }
        files.sort();
        // Down to 90%, so this doesn't run again on every lookup.
        for (_, p) in &files[..files.len() - keep * 9 / 10] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::lyrics::Source;

        #[test]
        fn caches_hits_and_misses() {
            let dir = std::env::temp_dir().join(format!("lp-lyrics-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            assert_eq!(get(&dir, "YouTube-a"), None);
            put(&dir, "YouTube-a", &None);
            assert_eq!(get(&dir, "YouTube-a"), Some(None));
            let l = Lyrics::plain(Source::Lrclib, "one\ntwo");
            put(&dir, "YouTube-b", &l);
            assert_eq!(get(&dir, "YouTube-b"), Some(l));
            for i in 0..12 {
                put(&dir, &format!("k{i}"), &None);
            }
            prune(&dir, 10);
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 9);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(start_ms: u32, text: &str, words: &[(u32, u32)]) -> Line {
        Line {
            start_ms,
            end_ms: None,
            text: text.into(),
            words: words.iter().map(|&(start_ms, end)| Word { start_ms, end }).collect(),
        }
    }

    #[test]
    fn finds_current_line_and_word() {
        let l = Lyrics {
            source: Source::Lrclib,
            synced: true,
            instrumental: false,
            lines: vec![line(1000, "la la", &[(1000, 3), (1500, 5)]), line(3000, "next", &[])],
        };
        assert_eq!(l.line_at(0), None);
        assert_eq!(l.line_at(1000), Some(0));
        assert_eq!(l.line_at(2999), Some(0));
        assert_eq!(l.line_at(60_000), Some(1));
        assert!(l.has_words());
        let plain = l.unsynced();
        assert_eq!(plain.line_at(2000), None);
        assert!(!plain.has_words());
    }

    #[test]
    fn line_progress_runs_to_the_next_line() {
        let timed = |lines: Vec<Line>| Lyrics { source: Source::Lrclib, synced: true, instrumental: false, lines };
        // 4 s to the next line: filled 0.8 s early.
        let l = timed(vec![line(1000, "a line of some length", &[]), line(5000, "next", &[])]);
        assert_eq!(l.line_progress(0, 500), 0.0);
        assert_eq!(l.line_progress(0, 1000), 0.0);
        assert!((l.line_progress(0, 2600) - 0.5).abs() < 1e-6);
        assert_eq!(l.line_progress(0, 4200), 1.0);
        assert_eq!(l.line_progress(0, 4900), 1.0);
        // A short gap: a fifth early, not 0.8 s.
        let l = timed(vec![line(0, "quick", &[]), line(1000, "next", &[])]);
        assert!((l.line_progress(0, 400) - 0.5).abs() < 1e-6);
        // A long break after the line: it isn't stretched over the break.
        let l = timed(vec![line(0, "Yeah", &[]), line(14_000, "verse", &[])]);
        assert_eq!(l.line_progress(0, 2000), 1.0);
        // An end time from the source counts, but never past the next line.
        let mut l = timed(vec![line(0, "Yeah", &[]), line(14_000, "verse", &[])]);
        l.lines[0].end_ms = Some(1500);
        assert_eq!(l.line_progress(0, 1200), 1.0);
        l.lines[0].end_ms = Some(20_000);
        assert!(l.line_progress(0, 6000) < 1.0);
        // Same start as the next line (a duplicated time): already full.
        let l = timed(vec![line(0, "a", &[]), line(0, "b", &[])]);
        assert_eq!(l.line_progress(0, 0), 1.0);
    }

    #[test]
    fn word_progress_runs_through_each_word() {
        let close = |a: f32, b: f32| (a - b).abs() < 1e-5;
        // "ab " sung 1000..2000, "cd" from 2000; the next line at 4000.
        let l = Lyrics {
            source: Source::Lrclib,
            synced: true,
            instrumental: false,
            lines: vec![line(1000, "ab cd", &[(1000, 3), (2000, 5)]), line(4000, "next", &[])],
        };
        assert_eq!(l.line_progress(0, 900), 0.0);
        assert_eq!(l.line_progress(0, 1000), 0.0);
        // Halfway through the first word (3 of 5 characters).
        assert!(close(l.line_progress(0, 1500), 1.5 / 5.0));
        // No pause before the next word: it starts where the first ended.
        assert!(close(l.line_progress(0, 1999), 2.997 / 5.0));
        assert!(close(l.line_progress(0, 2000), 3.0 / 5.0));
        // The last word runs to the next line, 0.4 s early (a fifth of 2 s).
        assert!(close(l.line_progress(0, 2800), 4.0 / 5.0));
        assert_eq!(l.line_progress(0, 3600), 1.0);
        // A long break after the last word doesn't stretch it.
        let l = Lyrics {
            source: Source::Lrclib,
            synced: true,
            instrumental: false,
            lines: vec![line(0, "ab cd", &[(0, 3), (500, 5)]), line(30_000, "next", &[])],
        };
        assert_eq!(l.line_progress(0, 3000), 1.0);
    }

    #[test]
    fn plain_text_keeps_single_blank_lines() {
        let l = Lyrics::plain(Source::Lrclib, "\n[Verse 1]\nOne\n\n\n\nTwo\n\n").unwrap();
        let texts: Vec<&str> = l.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["[Verse 1]", "One", "", "Two"]);
        assert!(l.lines[0].is_label());
        assert!(Lyrics::plain(Source::Lrclib, " \n ").is_none());
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    #[ignore = "network"]
    fn live_lookup_finds_timed_lyrics() {
        // A music video upload, as it comes from YouTube.
        let l =
            lookup("Rick Astley - Never Gonna Give You Up (Official Video) (4K Remaster)", "Rick Astley", Some(213))
                .unwrap()
                .lyrics
                .expect("lyrics");
        assert_eq!(l.source, Source::Lrclib);
        assert!(l.synced);
    }
}
