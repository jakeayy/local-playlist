//! Parsers for the lyrics formats the sources return:
//! * LRC (`[mm:ss.xx]line`), with "enhanced" word times (`<mm:ss.xx>word`);
//! * Lyricsfile (LRCLIB, lrc.red), a small YAML document with line and word
//!   timings.
//!
//! Both are parsed here by hand: Lyricsfiles need a small, well-defined part
//! of YAML, which doesn't justify a YAML parser dependency.

use super::{Line, Word};

/// Inputs larger than this aren't lyrics (a Lyricsfile with word timings for
/// a long song is ~100 kB).
const MAX_INPUT: usize = 2 << 20;
const MAX_LINES: usize = 5000;

/// A parse of a timed format.
#[derive(Debug, Default, PartialEq)]
pub struct Timed {
    pub lines: Vec<Line>,
    /// Untimed text that came with it (Lyricsfile `plain`).
    pub plain: Option<String>,
    pub instrumental: bool,
}

// ------------------------------------------------------------------- LRC

/// Lines of an LRC file, sorted by time. Lines with several timestamps
/// (choruses: `[00:12.00][01:30.00]text`) appear once per timestamp.
pub fn parse_lrc(s: &str) -> Vec<Line> {
    if s.len() > MAX_INPUT {
        return Vec::new();
    }
    let mut offset_ms: i64 = 0;
    let mut lines = Vec::new();
    for raw in s.lines() {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while let Some(tag) = rest.strip_prefix('[').and_then(|r| r.find(']').map(|end| &r[..end])) {
            rest = &rest[tag.len() + 2..];
            if let Some(ms) = parse_time(tag) {
                times.push(ms);
            } else if let Some(v) = tag.strip_prefix("offset:") {
                offset_ms = v.trim().parse().unwrap_or(0);
            }
        }
        let (text, words) = parse_words(rest.trim());
        for (i, &t) in times.iter().enumerate() {
            // A positive offset shows the lyrics earlier.
            let start = (t as i64 - offset_ms).max(0) as u32;
            let shift = |w: &Word| Word { start_ms: (w.start_ms as i64 - offset_ms).max(0) as u32, ..*w };
            // Word times belong to the first occurrence only.
            let words = if i == 0 { words.iter().map(shift).collect() } else { Vec::new() };
            lines.push(Line { start_ms: start, end_ms: None, text: text.clone(), words });
            if lines.len() >= MAX_LINES {
                break;
            }
        }
    }
    lines.sort_by_key(|l| l.start_ms);
    lines
}

/// `mm:ss`, `mm:ss.x`, `mm:ss.xx`, `mm:ss.xxx` (or `mm:ss:xx`) in milliseconds.
pub fn parse_time(s: &str) -> Option<u32> {
    let (min, rest) = s.split_once(':')?;
    let (sec, frac) = match rest.find(['.', ':']) {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(min) || !digits(sec) || !(frac.is_empty() || digits(frac)) || sec.len() > 2 || frac.len() > 3 {
        return None;
    }
    let frac_ms = match frac.len() {
        0 => 0,
        1 => frac.parse::<u32>().ok()? * 100,
        2 => frac.parse::<u32>().ok()? * 10,
        _ => frac.parse::<u32>().ok()?,
    };
    min.parse::<u32>().ok()?.checked_mul(60_000)?.checked_add(sec.parse::<u32>().ok()? * 1000 + frac_ms)
}

/// A line's text with `<mm:ss.xx>` word times taken out, and those times.
/// Without (valid) word times: the text as is, no words.
pub fn parse_words(s: &str) -> (String, Vec<Word>) {
    // (time of the tag before it, text) pieces; the first has no tag before it.
    let mut pieces: Vec<(Option<u32>, &str)> = Vec::new();
    let mut time = None;
    let mut rest = s;
    loop {
        let tag = rest.find('<').and_then(|open| {
            let close = rest[open..].find('>')? + open;
            parse_time(&rest[open + 1..close]).map(|t| (open, close, t))
        });
        let Some((open, close, t)) = tag else {
            pieces.push((time, rest));
            break;
        };
        pieces.push((time, &rest[..open]));
        time = Some(t);
        rest = &rest[close + 1..];
    }
    let Some(first) = pieces.iter().find_map(|p| p.0) else {
        return (s.to_string(), Vec::new());
    };
    let text: String = pieces.iter().map(|p| p.1).collect::<String>().trim_end().to_string();
    let mut words: Vec<Word> = Vec::new();
    let mut end = 0usize;
    for (t, piece) in pieces {
        end = (end + piece.len()).min(text.len());
        // Empty pieces (a closing time tag, trailing spaces) aren't words.
        // Text before the first tag is sung from the first time on.
        if words.last().is_none_or(|w| (w.end as usize) < end) && !piece.is_empty() {
            words.push(Word { start_ms: t.unwrap_or(first), end: end as u32 });
        }
    }
    (text, words)
}

// ------------------------------------------------------------ Lyricsfile

/// LRCLIB's Lyricsfile (version 1.0): line timings, word timings when known,
/// plain text, and the instrumental flag. `None` if it doesn't parse.
pub fn parse_lyricsfile(s: &str) -> Option<Timed> {
    if s.len() > MAX_INPUT {
        return None;
    }
    let doc = yaml::parse(s)?;
    if doc.get("version")?.as_str()? != "1.0" {
        return None;
    }
    let meta = doc.get("metadata");
    let instrumental = meta.and_then(|m| m.get("instrumental")).and_then(yaml::Node::as_str) == Some("true");
    let mut lines = Vec::new();
    for item in doc.get("lines").map(yaml::Node::items).unwrap_or_default().iter().take(MAX_LINES) {
        let (Some(text), Some(start_ms)) = (item.get("text").and_then(yaml::Node::as_str), item.get("start_ms")) else {
            continue;
        };
        let Some(start_ms) = start_ms.as_u32() else { continue };
        let end_ms = item.get("end_ms").and_then(yaml::Node::as_u32).filter(|&e| e > start_ms);
        let (text, mut words) = parse_words(text);
        if words.is_empty() {
            words = word_list(item.get("words").map(yaml::Node::items).unwrap_or_default(), &text);
        }
        lines.push(Line { start_ms, end_ms, text, words });
    }
    lines.sort_by_key(|l| l.start_ms);
    let plain = doc.get("plain").and_then(yaml::Node::as_str).map(str::to_string).filter(|p| !p.trim().is_empty());
    Some(Timed { lines, plain, instrumental })
}

/// Word timings from a Lyricsfile `words` list, if their texts make up `text`.
fn word_list(items: &[yaml::Node], text: &str) -> Vec<Word> {
    let mut words = Vec::with_capacity(items.len());
    let mut joined = String::with_capacity(text.len() + 1);
    for w in items {
        let (Some(t), Some(start_ms)) = (w.get("text").and_then(yaml::Node::as_str), w.get("start_ms")) else {
            return Vec::new();
        };
        let Some(start_ms) = start_ms.as_u32() else { return Vec::new() };
        joined.push_str(t);
        // A trailing space on the last word may be missing from the line.
        words.push(Word { start_ms, end: joined.len().min(text.len()) as u32 });
    }
    if words.is_empty() || joined.trim_end() != text.trim_end() {
        return Vec::new();
    }
    words.sort_by_key(|w| w.start_ms);
    // Out-of-order times would make the ends go backwards: not usable.
    if words.windows(2).any(|p| p[1].end < p[0].end) {
        return Vec::new();
    }
    words
}

/// Just enough YAML for Lyricsfiles: block mappings and sequences, plain and
/// quoted scalars (also continued over several lines), `|`/`>` block scalars,
/// and empty flow collections (`[]`, `{}`).
mod yaml {
    #[derive(Debug, PartialEq)]
    pub enum Node {
        Null,
        Str(String),
        Seq(Vec<Node>),
        Map(Vec<(String, Node)>),
    }

    impl Node {
        pub fn get(&self, key: &str) -> Option<&Node> {
            match self {
                Node::Map(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
                _ => None,
            }
        }

        pub fn as_str(&self) -> Option<&str> {
            match self {
                Node::Str(s) => Some(s),
                _ => None,
            }
        }

        pub fn as_u32(&self) -> Option<u32> {
            self.as_str()?.parse().ok()
        }

        pub fn items(&self) -> &[Node] {
            match self {
                Node::Seq(v) => v,
                _ => &[],
            }
        }
    }

    const MAX_DEPTH: usize = 16;

    struct Parser<'a> {
        lines: Vec<&'a str>,
        i: usize,
        /// The rest of a `- ` line, parsed as if it were a line of its own
        /// at the column it starts in.
        pending: Option<(usize, &'a str)>,
    }

    pub fn parse(s: &str) -> Option<Node> {
        let mut p = Parser { lines: s.lines().collect(), i: 0, pending: None };
        let (indent, _) = p.peek()?;
        let node = p.node(indent, 0)?;
        // Anything left over means a structure this parser doesn't follow.
        p.peek().is_none().then_some(node)
    }

    fn indent_of(line: &str) -> usize {
        line.len() - line.trim_start_matches(' ').len()
    }

    fn is_blank(line: &str) -> bool {
        let t = line.trim();
        t.is_empty() || t.starts_with('#')
    }

    fn is_item(content: &str) -> bool {
        content == "-" || content.starts_with("- ")
    }

    /// `key: value` → (key, value); keys here are plain words.
    fn split_key(content: &str) -> Option<(&str, &str)> {
        let colon = content.find(':')?;
        let key = &content[..colon];
        let rest = &content[colon + 1..];
        let ok_key = !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        (ok_key && (rest.is_empty() || rest.starts_with(' '))).then(|| (key, rest.trim()))
    }

    impl<'a> Parser<'a> {
        fn peek(&mut self) -> Option<(usize, &'a str)> {
            if let Some(p) = self.pending {
                return Some(p);
            }
            while self.i < self.lines.len() && is_blank(self.lines[self.i]) {
                self.i += 1;
            }
            let line = self.lines.get(self.i)?;
            Some((indent_of(line), line.trim()))
        }

        fn bump(&mut self) {
            if self.pending.take().is_none() {
                self.i += 1;
            }
        }

        fn node(&mut self, indent: usize, depth: usize) -> Option<Node> {
            if depth > MAX_DEPTH {
                return None;
            }
            let (_, content) = self.peek()?;
            if is_item(content) {
                self.seq(indent, depth)
            } else if split_key(content).is_some() {
                self.map(indent, depth)
            } else {
                self.bump();
                Some(scalar(&self.continued(content.to_string(), indent)))
            }
        }

        fn seq(&mut self, indent: usize, depth: usize) -> Option<Node> {
            let mut items = Vec::new();
            while let Some((ind, content)) = self.peek()
                && ind == indent
                && is_item(content)
            {
                self.bump();
                let rest = content[1..].trim_start();
                if rest.is_empty() {
                    match self.peek() {
                        Some((next, _)) if next > indent => items.push(self.node(next, depth + 1)?),
                        _ => items.push(Node::Null),
                    }
                } else {
                    let column = indent + (content.len() - rest.len());
                    self.pending = Some((column, rest));
                    items.push(self.node(column, depth + 1)?);
                }
            }
            Some(Node::Seq(items))
        }

        fn map(&mut self, indent: usize, depth: usize) -> Option<Node> {
            let mut entries = Vec::new();
            while let Some((ind, content)) = self.peek()
                && ind == indent
                && !is_item(content)
            {
                let (key, rest) = split_key(content)?;
                self.bump();
                let value = if rest.is_empty() {
                    match self.peek() {
                        Some((next, _)) if next > indent => self.node(next, depth + 1)?,
                        // `key:` followed by `- item` lines at the same indent.
                        Some((next, c)) if next == indent && is_item(c) => self.seq(indent, depth + 1)?,
                        _ => Node::Null,
                    }
                } else if rest.starts_with('|') || rest.starts_with('>') {
                    self.block_scalar(indent, rest)
                } else {
                    scalar(&self.continued(rest.to_string(), indent))
                };
                entries.push((key.to_string(), value));
            }
            Some(Node::Map(entries))
        }

        /// A scalar's text plus the lines that continue it (indented deeper
        /// than its key), folded with spaces.
        fn continued(&mut self, mut text: String, indent: usize) -> String {
            while self.pending.is_none()
                && let Some(line) = self.lines.get(self.i)
                && !line.trim().is_empty()
                && indent_of(line) > indent
            {
                text.push(' ');
                text.push_str(line.trim());
                self.i += 1;
            }
            text
        }

        fn block_scalar(&mut self, indent: usize, header: &str) -> Node {
            let folded = header.starts_with('>');
            let keep = header.contains('+');
            let strip = header.contains('-');
            let mut raw = Vec::new();
            while let Some(line) = self.lines.get(self.i)
                && (line.trim().is_empty() || indent_of(line) > indent)
            {
                raw.push(*line);
                self.i += 1;
            }
            let block_indent = raw.iter().find(|l| !l.trim().is_empty()).map(|l| indent_of(l)).unwrap_or(0);
            let body: Vec<&str> = raw.iter().map(|l| l.get(block_indent..).unwrap_or("").trim_end()).collect();
            let mut text = if folded {
                let mut out = String::new();
                for (i, l) in body.iter().enumerate() {
                    if l.is_empty() {
                        out.push('\n');
                    } else {
                        if i > 0 && !body[i - 1].is_empty() {
                            out.push(' ');
                        }
                        out.push_str(l);
                    }
                }
                out
            } else {
                body.join("\n")
            };
            if !strip {
                let trimmed = text.trim_end_matches('\n').len();
                text.truncate(trimmed);
                text.push('\n');
                if keep {
                    text.extend(std::iter::repeat_n('\n', body.iter().rev().take_while(|l| l.is_empty()).count()));
                }
            } else {
                let trimmed = text.trim_end_matches('\n').len();
                text.truncate(trimmed);
            }
            Node::Str(text)
        }
    }

    fn scalar(s: &str) -> Node {
        let s = s.trim();
        if let Some(body) = s.strip_prefix('\'') {
            let mut out = String::new();
            let mut chars = body.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\'' {
                    if chars.peek() == Some(&'\'') {
                        chars.next();
                        out.push('\'');
                    } else {
                        break;
                    }
                } else {
                    out.push(c);
                }
            }
            return Node::Str(out);
        }
        if let Some(body) = s.strip_prefix('"') {
            return Node::Str(double_quoted(body));
        }
        match s {
            "" | "~" | "null" => Node::Null,
            "[]" => Node::Seq(Vec::new()),
            "{}" => Node::Map(Vec::new()),
            // A plain scalar ends at a comment.
            _ => Node::Str(s.split(" #").next().unwrap_or(s).trim_end().to_string()),
        }
    }

    fn double_quoted(body: &str) -> String {
        let mut out = String::new();
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('0') => out.push('\0'),
                    Some(' ') => out.push(' '),
                    Some(e @ ('x' | 'u' | 'U')) => {
                        let n = match e {
                            'x' => 2,
                            'u' => 4,
                            _ => 8,
                        };
                        let hex: String = chars.by_ref().take(n).collect();
                        if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                            out.push(ch);
                        }
                    }
                    Some(other) => out.push(other),
                    None => {}
                },
                c => out.push(c),
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn s(v: &str) -> Node {
            Node::Str(v.into())
        }

        #[test]
        fn parses_nested_blocks_and_scalars() {
            let doc = "\
a: 1
b:
  c: 'it''s'
  d: \"tab\\there \\u00e9\"
list:
- x: plain text
    that continues
  y: []
- x: two
nested:
  - k: v
block: |-
  line one

  line three
folded: >
  one
  two
";
            let n = parse(doc).unwrap();
            assert_eq!(n.get("a"), Some(&s("1")));
            assert_eq!(n.get("b").unwrap().get("c"), Some(&s("it's")));
            assert_eq!(n.get("b").unwrap().get("d"), Some(&s("tab\there é")));
            let list = n.get("list").unwrap().items();
            assert_eq!(list.len(), 2);
            assert_eq!(list[0].get("x"), Some(&s("plain text that continues")));
            assert_eq!(list[0].get("y"), Some(&Node::Seq(Vec::new())));
            assert_eq!(list[1].get("x"), Some(&s("two")));
            assert_eq!(n.get("nested").unwrap().items()[0].get("k"), Some(&s("v")));
            assert_eq!(n.get("block"), Some(&s("line one\n\nline three")));
            assert_eq!(n.get("folded"), Some(&s("one two\n")));
        }

        #[test]
        fn rejects_what_it_cannot_follow() {
            assert!(parse("a: 1\n  - b\nc").is_none());
            assert!(parse("").is_none());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[Line]) -> Vec<(u32, &str)> {
        lines.iter().map(|l| (l.start_ms, l.text.as_str())).collect()
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("01:02.5"), Some(62_500));
        assert_eq!(parse_time("01:02.50"), Some(62_500));
        assert_eq!(parse_time("01:02.505"), Some(62_505));
        assert_eq!(parse_time("1:02"), Some(62_000));
        assert_eq!(parse_time("01:02:50"), Some(62_500));
        assert_eq!(parse_time("ar:Someone"), None);
        assert_eq!(parse_time("01:2x"), None);
    }

    #[test]
    fn lrc_lines_tags_and_repeats() {
        let lrc = "[ar:Somebody]\n[ti:Song]\n[00:01.00]First\n[00:05.00][00:20.00]Chorus\n[00:10.50]\n[00:12.00]Third";
        let lines = parse_lrc(lrc);
        assert_eq!(
            texts(&lines),
            [(1000, "First"), (5000, "Chorus"), (10_500, ""), (12_000, "Third"), (20_000, "Chorus")]
        );
        let shifted = parse_lrc("[offset:+500]\n[00:01.00]First");
        assert_eq!(shifted[0].start_ms, 500);
    }

    #[test]
    fn enhanced_lrc_words() {
        let lines = parse_lrc("[00:18.89] <00:18.89>We all <00:19.53>sing <00:20.07>along <00:21.00>");
        assert_eq!(lines[0].text, "We all sing along");
        let w: Vec<(u32, u32)> = lines[0].words.iter().map(|w| (w.start_ms, w.end)).collect();
        assert_eq!(w, [(18_890, 7), (19_530, 12), (20_070, 17)]);
        // Plain lines stay as they are, including angle brackets that aren't times.
        assert_eq!(parse_words("a <b> c"), ("a <b> c".to_string(), Vec::new()));
    }

    const LYRICSFILE: &str = "\
version: '1.0'
metadata:
  title: Made Up Song
  artist: Nobody
  album: Tests
  duration_ms: 200000
  instrumental: false
  language: en
lines:
- text: Hello there
  start_ms: 1000
  end_ms: 2500
  words:
  - text: 'Hello '
    start_ms: 1000
  - text: there
    start_ms: 1600
- text: <00:03.00>Inline <00:03.40>words
  start_ms: 3000
  end_ms: 4000
- text: 'It''s timed by line'
  start_ms: 5000
  end_ms: 6000
- text: Mismatched words
  start_ms: 7000
  words:
  - text: 'Other '
    start_ms: 7000
plain: |-
  Hello there
  Inline words
";

    #[test]
    fn lyricsfile_lines_and_words() {
        let t = parse_lyricsfile(LYRICSFILE).unwrap();
        assert!(!t.instrumental);
        assert_eq!(
            texts(&t.lines),
            [(1000, "Hello there"), (3000, "Inline words"), (5000, "It's timed by line"), (7000, "Mismatched words")]
        );
        let ends = |l: &Line| l.words.iter().map(|w| (w.start_ms, w.end)).collect::<Vec<_>>();
        assert_eq!(ends(&t.lines[0]), [(1000, 6), (1600, 11)]);
        assert_eq!(ends(&t.lines[1]), [(3000, 7), (3400, 12)]);
        assert!(t.lines[2].words.is_empty());
        assert_eq!(t.lines.iter().map(|l| l.end_ms).collect::<Vec<_>>(), [Some(2500), Some(4000), Some(6000), None]);
        assert!(t.lines[3].words.is_empty(), "words that don't spell the line are dropped");
        assert_eq!(t.plain.as_deref(), Some("Hello there\nInline words"));
    }

    #[test]
    fn lyricsfile_instrumental_and_junk() {
        let t = parse_lyricsfile("version: '1.0'\nmetadata:\n  title: X\n  artist: Y\n  instrumental: true\n").unwrap();
        assert!(t.instrumental && t.lines.is_empty());
        assert!(parse_lyricsfile("version: '2.0'\n").is_none());
        assert!(parse_lyricsfile("<html>").is_none());
    }
}
