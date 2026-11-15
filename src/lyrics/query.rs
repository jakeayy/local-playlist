//! Guessing a song's artist and title from an upload's metadata.
//!
//! Music on YouTube and SoundCloud is mostly uploads, not catalogue entries:
//! "Artist - Title (Official Music Video) [4K]" posted by "ArtistVEVO", a
//! label's channel, or "Artist - Topic". Lyrics databases want "Artist" and
//! "Title", so [`guesses`] strips the labels uploads add and splits
//! "Artist - Title", giving a few candidates, the most likely first.

/// A candidate (artist, title) to look lyrics up by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub artist: String,
    pub title: String,
}

/// Words that mark a bracketed part of a title as upload decoration rather
/// than part of the song's name.
const NOISE: &[&str] = &[
    "official",
    "video",
    "audio",
    "lyric",
    "lyrics",
    "visualizer",
    "visualiser",
    "mv",
    "m/v",
    "hd",
    "hq",
    "4k",
    "8k",
    "1080p",
    "720p",
    "remaster",
    "remastered",
    "explicit",
    "clean",
    "colour coded",
    "color coded",
    "letra",
    "legendado",
    "subtitulado",
    "sub español",
    "free download",
    "free dl",
    "out now",
    "premiere",
    "performance",
    "live",
    "clip",
];

/// Separators between artist and title ("Artist - Title").
const DASHES: &[&str] = &[" - ", " – ", " — ", " ~ "];

/// Candidate (artist, title) pairs for an upload's title and uploader,
/// most likely first, without duplicates (at most four).
pub fn guesses(title: &str, artist: &str) -> Vec<Query> {
    let (channel, topic) = clean_channel(artist);
    let title = cut_trailer(&normalize_brackets(title));
    let mut out: Vec<Query> = Vec::new();
    // "Artist - Topic" channels are YouTube's catalogue: names are exact.
    if topic {
        add_guess(&mut out, &channel, &clean_title(&title));
    }
    match split_dash(&title) {
        Some((left, right)) => {
            let (left, right) = (clean_title(&left), clean_title(&right));
            // Usually "Artist - Title"; "Title - Artist" when the uploader is the artist.
            if same_artist(&right, &channel) && !same_artist(&left, &channel) {
                add_guess(&mut out, &channel, &left);
            }
            let right = quoted(&right).map(|(_, q)| q).unwrap_or(right);
            add_guess(&mut out, &left, &right);
            if let Some(first) = first_artist(&left) {
                add_guess(&mut out, &first, &right);
            }
            add_guess(&mut out, &channel, &right);
        }
        None => {
            let cleaned = clean_title(&title);
            // Artist 'Title' (common for K-pop and J-pop music videos).
            if let Some((before, q)) = quoted(&cleaned) {
                add_guess(&mut out, if before.is_empty() { &channel } else { &before }, &q);
            }
            add_guess(&mut out, &channel, &cleaned);
        }
    }
    // Last resort: everything in brackets dropped (versions, remixes...).
    if let Some(first) = out.first().cloned() {
        let bare = strip_brackets(&first.title);
        if bare != first.title {
            add_guess(&mut out, &first.artist, &bare);
        }
    }
    out.truncate(4);
    out
}

fn add_guess(out: &mut Vec<Query>, artist: &str, title: &str) {
    let (artist, title) = (strip_feat(artist.trim()), title.trim().to_string());
    if artist.is_empty() || title.is_empty() {
        return;
    }
    let q = Query { artist, title };
    let same = |o: &Query| normalize(&o.artist) == normalize(&q.artist) && normalize(&o.title) == normalize(&q.title);
    if !out.iter().any(same) {
        out.push(q);
    }
}

/// Uploader name without the bits channels add ("- Topic", "VEVO",
/// "Official"), and whether it was an auto-generated "- Topic" channel.
fn clean_channel(artist: &str) -> (String, bool) {
    let mut a = artist.trim().to_string();
    let topic = strip_suffix_ci(&mut a, " - topic");
    let vevo = strip_suffix_ci(&mut a, "vevo");
    strip_suffix_ci(&mut a, " official channel");
    strip_suffix_ci(&mut a, " official");
    if vevo && !a.contains(' ') {
        a = split_camel(&a);
    }
    (a.trim().to_string(), topic)
}

fn strip_suffix_ci(s: &mut String, suffix: &str) -> bool {
    let lower = s.to_lowercase();
    // `to_lowercase` can change byte lengths (rarely); only cut when it didn't.
    if lower.len() == s.len() && lower.ends_with(suffix) && s.len() > suffix.len() {
        s.truncate(s.len() - suffix.len());
        true
    } else {
        false
    }
}

/// "RickAstley" → "Rick Astley".
fn split_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let mut prev_lower = false;
    for c in s.chars() {
        if c.is_uppercase() && prev_lower {
            out.push(' ');
        }
        prev_lower = c.is_lowercase();
        out.push(c);
    }
    out
}

/// Full-width and CJK brackets as ASCII ones.
fn normalize_brackets(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '（' => '(',
            '）' => ')',
            '【' | '〖' => '[',
            '】' | '〗' => ']',
            c => c,
        })
        .collect()
}

/// Drop what follows " | " or " // " ("Title | Official Video", "Title // Live at…").
fn cut_trailer(s: &str) -> String {
    let mut s = s;
    for sep in [" | ", " // ", " ｜ "] {
        if let Some(i) = s.find(sep)
            && i > 0
        {
            s = &s[..i];
        }
    }
    s.trim().to_string()
}

/// "Artist - Title" split at the first dash with text on both sides.
fn split_dash(s: &str) -> Option<(String, String)> {
    let (i, sep) = DASHES.iter().filter_map(|d| s.find(d).map(|i| (i, *d))).min_by_key(|(i, _)| *i)?;
    let (left, right) = (s[..i].trim(), s[i + sep.len()..].trim());
    (!left.is_empty() && !right.is_empty()).then(|| (left.to_string(), right.to_string()))
}

/// A title without decoration: bracketed parts that are just upload labels,
/// "feat." credits, and such labels left unbracketed at the end.
pub fn clean_title(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(open) = rest.find(['(', '[']) {
        let close_ch = if rest.as_bytes()[open] == b'(' { ')' } else { ']' };
        let Some(len) = rest[open + 1..].find(close_ch) else { break };
        let inner = &rest[open + 1..open + 1 + len];
        out.push_str(&rest[..open]);
        if !is_noise(inner) && !is_feat(inner) {
            out.push_str(&rest[open..open + len + 2]);
        }
        rest = &rest[open + len + 2..];
    }
    out.push_str(rest);
    let mut t = collapse_spaces(&strip_feat(&out));
    // Unbracketed labels at the end: "Title Official Video", "Title Lyrics".
    loop {
        let lower = t.to_lowercase();
        let Some(word) = ["official music video", "official video", "official audio", "lyric video", "lyrics", "mv"]
            .iter()
            .find(|w| lower.ends_with(&format!(" {w}")))
        else {
            break;
        };
        if lower.len() != t.len() {
            break;
        }
        t.truncate(t.len() - word.len() - 1);
        t = t.trim_end_matches([' ', '-', '|']).to_string();
    }
    t
}

fn is_noise(inner: &str) -> bool {
    let words = normalize(inner);
    let padded = format!(" {words} ");
    NOISE.iter().any(|n| padded.contains(&format!(" {} ", normalize(n)))) || inner.to_lowercase().contains("m/v")
}

fn is_feat(inner: &str) -> bool {
    let l = inner.trim_start().to_lowercase();
    ["feat.", "feat ", "ft.", "ft ", "featuring ", "with "].iter().any(|p| l.starts_with(p))
}

/// Without a "feat. X" credit at the end ("Title feat. X", "Artist ft. X").
fn strip_feat(s: &str) -> String {
    let lower = s.to_lowercase();
    if lower.len() != s.len() {
        return s.trim().to_string();
    }
    let cut = [" feat. ", " feat ", " ft. ", " ft ", " featuring "].iter().filter_map(|p| lower.find(p)).min();
    match cut {
        Some(i) if i > 0 => s[..i].trim().to_string(),
        _ => s.trim().to_string(),
    }
}

/// Without any bracketed part.
fn strip_brackets(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    collapse_spaces(&out)
}

/// The first of several credited artists ("A & B", "A, B", "A x B").
fn first_artist(s: &str) -> Option<String> {
    let i = [" & ", ", ", " x ", " X ", " and "].iter().filter_map(|p| s.find(p)).min()?;
    Some(s[..i].trim().to_string()).filter(|a| !a.is_empty())
}

/// A title in quotes, with what comes before it: `Artist 'Title' M/V`.
fn quoted(s: &str) -> Option<(String, String)> {
    for (open, close) in [('\'', '\''), ('"', '"'), ('‘', '’'), ('“', '”'), ('「', '」'), ('『', '』')] {
        let Some(start) = s
            .char_indices()
            .find(|&(i, c)| c == open && (i == 0 || s[..i].ends_with(char::is_whitespace) || open != '\''))
        else {
            continue;
        };
        let after = start.0 + open.len_utf8();
        let Some(len) = s[after..].find(close) else { continue };
        let inner = s[after..after + len].trim();
        // A closing quote must end a word, or "Don't" would count.
        let next = s[after + len + close.len_utf8()..].chars().next();
        if inner.is_empty() || next.is_some_and(char::is_alphanumeric) {
            continue;
        }
        return Some((s[..start.0].trim().to_string(), inner.to_string()));
    }
    None
}

fn collapse_spaces(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lowercase words of letters and digits, single-spaced: for comparing names.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if c == '&' {
            out.push_str(" and ");
        } else if c == '\'' || c == '’' {
            // "Don't" and "Dont" are the same word.
        } else {
            out.push(' ');
        }
    }
    collapse_spaces(&out)
}

/// Whether a found artist name is the one wanted (or credits it).
pub fn same_artist(found: &str, wanted: &str) -> bool {
    let (f, w) = (normalize(found), normalize(wanted));
    if f.is_empty() || w.is_empty() {
        return false;
    }
    f == w
        || format!(" {f} ").contains(&format!(" {w} "))
        || format!(" {w} ").contains(&format!(" {f} "))
        || f.replace(' ', "") == w.replace(' ', "")
}

/// Whether a found title is the song wanted, ignoring decoration (and an
/// "Artist - " prefix some entries have).
pub fn same_title(found: &str, wanted: &str) -> bool {
    let w = normalize(&clean_title(wanted));
    if w.is_empty() {
        return false;
    }
    let f = normalize(&clean_title(found));
    f == w || split_dash(found).is_some_and(|(_, right)| normalize(&clean_title(&right)) == w)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(title: &str, artist: &str) -> (String, String) {
        let q = guesses(title, artist).into_iter().next().expect("a guess");
        (q.artist, q.title)
    }

    fn pair(a: &str, t: &str) -> (String, String) {
        (a.to_string(), t.to_string())
    }

    #[test]
    fn music_videos() {
        assert_eq!(
            first("Rick Astley - Never Gonna Give You Up (Official Video) (4K Remaster)", "Rick Astley"),
            pair("Rick Astley", "Never Gonna Give You Up")
        );
        assert_eq!(first("Never Gonna Give You Up", "RickAstleyVEVO"), pair("Rick Astley", "Never Gonna Give You Up"));
        assert_eq!(first("Daft Punk - Get Lucky [Official Audio] ft. Pharrell Williams", "Daft Punk"), {
            pair("Daft Punk", "Get Lucky")
        });
        assert_eq!(first("Song Name (feat. Someone) [Lyric Video]", "Some Band"), pair("Some Band", "Song Name"));
        assert_eq!(first("Artist - Title | Official Music Video", "Label Records"), pair("Artist", "Title"));
        assert_eq!(first("Artist – Title (Live at Wembley)", "Label"), pair("Artist", "Title"));
        assert_eq!(first("Title Official Video", "The Artist"), pair("The Artist", "Title"));
        assert_eq!(first("ARTIST 'Song Title' M/V", "Some Entertainment"), pair("ARTIST", "Song Title"));
        assert_eq!(first("【MV】Artist「Title」", "Channel"), pair("Artist", "Title"));
    }

    #[test]
    fn topic_channels_are_exact() {
        assert_eq!(first("Get Lucky (feat. Pharrell Williams)", "Daft Punk - Topic"), pair("Daft Punk", "Get Lucky"));
    }

    #[test]
    fn keeps_parts_of_the_name() {
        // Brackets that belong to the song stay in the first guess…
        assert_eq!(first("Song (Part II)", "Band"), pair("Band", "Song (Part II)"));
        // …and a later guess drops them.
        let all = guesses("Song (Part II)", "Band");
        assert!(all.iter().any(|q| q.title == "Song"));
        assert_eq!(first("Don't Stop Me Now", "Queen Official"), pair("Queen", "Don't Stop Me Now"));
    }

    #[test]
    fn title_then_artist_when_the_uploader_is_the_artist() {
        assert_eq!(first("Title - Artist", "Artist"), pair("Artist", "Title"));
    }

    #[test]
    fn several_artists() {
        let all = guesses("A & B - Song", "Label");
        assert_eq!(all[0], Query { artist: "A & B".into(), title: "Song".into() });
        assert!(all.contains(&Query { artist: "A".into(), title: "Song".into() }));
        assert!(all.len() <= 4);
    }

    #[test]
    fn nothing_to_guess() {
        assert!(guesses("", "Artist").is_empty());
        assert!(guesses("Title", "").is_empty());
    }

    #[test]
    fn compares_names() {
        assert!(same_artist("Rick Astley", "rick astley"));
        assert!(same_artist("Daft Punk & Pharrell Williams", "Daft Punk"));
        assert!(same_artist("RickAstley", "Rick Astley"));
        assert!(!same_artist("Rickie Lee Jones", "Rick"));
        assert!(same_title("Never Gonna Give You Up (Remastered)", "Never Gonna Give You Up"));
        assert!(same_title("Rick Astley - Never Gonna Give You Up", "Never Gonna Give You Up"));
        assert!(same_title("Dont Stop", "Don't Stop"));
        assert!(!same_title("Never Gonna Give You Up (Deutsche Übersetzung)", "Never Gonna Give You Up"));
    }
}
