// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Byte patches for rename refactors.
//!
//! Canvas files are never rewritten as a whole: a rename replaces only the
//! bytes of the affected path strings, so formatting, key order, unknown keys
//! and everything else stay exactly as they were. [`path_refs`] finds every
//! vault path a canvas holds (a file node's `file`, a group's `background`)
//! with its byte range in the source; [`plan_path_patches`] turns a rename
//! mapping into [`Patch`]es; [`apply_patches`] applies them.

use std::fmt;
use std::ops::Range;

/// Nesting deeper than this is refused (the same limit serde_json uses).
const MAX_DEPTH: usize = 128;

/// Which key a [`PathRef`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathKey {
    /// `file` of a node whose `type` is `file`.
    File,
    /// `background` of a node whose `type` is `group`.
    Background,
}

/// One vault path inside a canvas file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathRef {
    /// Position in the `nodes` array.
    pub node_index: usize,
    pub key: PathKey,
    /// Byte range of the JSON string token, quotes included.
    pub range: Range<usize>,
    /// The decoded string value.
    pub value: String,
}

/// The source could not be scanned. Byte offset of the problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanError {
    pub offset: usize,
    pub message: &'static str,
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

impl std::error::Error for ScanError {}

/// Replace `range` of the source with `replacement`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Patch {
    pub range: Range<usize>,
    pub replacement: String,
}

/// Patches that cannot be applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatchError {
    /// A range ends past the source or is reversed.
    OutOfBounds(Range<usize>),
    /// A range does not start or end on a character boundary.
    NotCharBoundary(Range<usize>),
    /// Two ranges overlap.
    Overlap(Range<usize>, Range<usize>),
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfBounds(r) => write!(f, "patch range {r:?} is out of bounds"),
            Self::NotCharBoundary(r) => {
                write!(f, "patch range {r:?} splits a character")
            }
            Self::Overlap(a, b) => write!(f, "patch ranges {a:?} and {b:?} overlap"),
        }
    }
}

impl std::error::Error for PatchError {}

/// Finds every vault path in a canvas source, in file order. Only the
/// top-level `nodes` array is searched. A key that appears twice in one node
/// yields two refs; both get patched.
pub fn path_refs(src: &str) -> Result<Vec<PathRef>, ScanError> {
    let mut s = Scanner {
        b: src.as_bytes(),
        pos: 0,
    };
    let mut out = Vec::new();
    s.ws();
    if src.as_bytes().get(s.pos..s.pos + 3) == Some("\u{feff}".as_bytes()) {
        s.pos += 3;
        s.ws();
    }
    s.expect(b'{')?;
    s.ws();
    if s.eat(b'}') {
        return Ok(out);
    }
    loop {
        s.ws();
        let (_, key) = s.string()?;
        s.ws();
        s.expect(b':')?;
        s.ws();
        if key == "nodes" && s.peek() == Some(b'[') {
            s.nodes(&mut out)?;
        } else {
            s.skip_value(1)?;
        }
        s.ws();
        if s.eat(b',') {
            continue;
        }
        s.expect(b'}')?;
        return Ok(out);
    }
}

/// Encodes `s` as a JSON string token (with quotes) the way
/// `JSON.stringify` does: `"` and `\` escaped, control characters as short
/// escapes or `\u00XX`, everything else literal.
pub fn encode_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Asks `rename` about every path ref and returns a patch for each one it
/// maps to a new value. Unchanged values yield no patch.
pub fn plan_path_patches(
    src: &str,
    mut rename: impl FnMut(&PathRef) -> Option<String>,
) -> Result<Vec<Patch>, ScanError> {
    Ok(path_refs(src)?
        .into_iter()
        .filter_map(|r| {
            let new = rename(&r)?;
            (new != r.value).then(|| Patch {
                range: r.range.clone(),
                replacement: encode_json_string(&new),
            })
        })
        .collect())
}

/// Applies non-overlapping patches (in any order) to `src`.
pub fn apply_patches(src: &str, patches: &[Patch]) -> Result<String, PatchError> {
    let mut sorted: Vec<&Patch> = patches.iter().collect();
    sorted.sort_by_key(|p| (p.range.start, p.range.end));
    for p in &sorted {
        let r = &p.range;
        if r.start > r.end || r.end > src.len() {
            return Err(PatchError::OutOfBounds(r.clone()));
        }
        if !src.is_char_boundary(r.start) || !src.is_char_boundary(r.end) {
            return Err(PatchError::NotCharBoundary(r.clone()));
        }
    }
    for w in sorted.windows(2) {
        if w[0].range.end > w[1].range.start {
            return Err(PatchError::Overlap(w[0].range.clone(), w[1].range.clone()));
        }
    }
    let mut out = String::with_capacity(src.len());
    let mut at = 0;
    for p in sorted {
        out.push_str(&src[at..p.range.start]);
        out.push_str(&p.replacement);
        at = p.range.end;
    }
    out.push_str(&src[at..]);
    Ok(out)
}

struct Scanner<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Scanner<'_> {
    fn err(&self, message: &'static str) -> ScanError {
        ScanError {
            offset: self.pos,
            message,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), ScanError> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(self.err("unexpected character"))
        }
    }

    fn hex4(&mut self) -> Result<u32, ScanError> {
        let digits = self
            .b
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| self.err("truncated escape"))?;
        let s = std::str::from_utf8(digits).map_err(|_| self.err("bad escape"))?;
        let v = u32::from_str_radix(s, 16).map_err(|_| self.err("bad escape"))?;
        self.pos += 4;
        Ok(v)
    }

    /// Reads a string token; returns its byte range (quotes included) and the
    /// decoded value.
    fn string(&mut self) -> Result<(Range<usize>, String), ScanError> {
        let start = self.pos;
        self.expect(b'"')?;
        let mut out = String::new();
        let mut run = self.pos;
        loop {
            let Some(c) = self.peek() else {
                return Err(self.err("unterminated string"));
            };
            match c {
                b'"' => {
                    out.push_str(self.slice(run, self.pos)?);
                    self.pos += 1;
                    return Ok((start..self.pos, out));
                }
                b'\\' => {
                    out.push_str(self.slice(run, self.pos)?);
                    self.pos += 1;
                    let e = self.peek().ok_or_else(|| self.err("truncated escape"))?;
                    self.pos += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi)
                                && self.b.get(self.pos..self.pos + 2) == Some(b"\\u")
                            {
                                let save = self.pos;
                                self.pos += 2;
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                } else {
                                    self.pos = save;
                                    hi
                                }
                            } else {
                                hi
                            };
                            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                        }
                        _ => return Err(self.err("bad escape")),
                    }
                    run = self.pos;
                }
                0..0x20 => return Err(self.err("control character in string")),
                _ => self.pos += 1,
            }
        }
    }

    fn slice(&self, a: usize, b: usize) -> Result<&str, ScanError> {
        std::str::from_utf8(&self.b[a..b]).map_err(|_| self.err("invalid UTF-8"))
    }

    fn skip_value(&mut self, depth: usize) -> Result<(), ScanError> {
        if depth > MAX_DEPTH {
            return Err(self.err("nesting too deep"));
        }
        match self.peek() {
            Some(b'"') => self.string().map(|_| ()),
            Some(b'{') => {
                self.pos += 1;
                self.ws();
                if self.eat(b'}') {
                    return Ok(());
                }
                loop {
                    self.ws();
                    self.string()?;
                    self.ws();
                    self.expect(b':')?;
                    self.ws();
                    self.skip_value(depth + 1)?;
                    self.ws();
                    if self.eat(b',') {
                        continue;
                    }
                    return self.expect(b'}');
                }
            }
            Some(b'[') => {
                self.pos += 1;
                self.ws();
                if self.eat(b']') {
                    return Ok(());
                }
                loop {
                    self.ws();
                    self.skip_value(depth + 1)?;
                    self.ws();
                    if self.eat(b',') {
                        continue;
                    }
                    return self.expect(b']');
                }
            }
            Some(b't') => self.literal(b"true"),
            Some(b'f') => self.literal(b"false"),
            Some(b'n') => self.literal(b"null"),
            Some(b'-' | b'0'..=b'9') => {
                let start = self.pos;
                while matches!(
                    self.peek(),
                    Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                ) {
                    self.pos += 1;
                }
                if self.pos == start {
                    Err(self.err("bad number"))
                } else {
                    Ok(())
                }
            }
            _ => Err(self.err("unexpected character")),
        }
    }

    fn literal(&mut self, lit: &[u8]) -> Result<(), ScanError> {
        if self.b.get(self.pos..self.pos + lit.len()) == Some(lit) {
            self.pos += lit.len();
            Ok(())
        } else {
            Err(self.err("bad literal"))
        }
    }

    fn nodes(&mut self, out: &mut Vec<PathRef>) -> Result<(), ScanError> {
        self.expect(b'[')?;
        self.ws();
        if self.eat(b']') {
            return Ok(());
        }
        let mut index = 0;
        loop {
            self.ws();
            if self.peek() == Some(b'{') {
                self.node(index, out)?;
            } else {
                self.skip_value(2)?;
            }
            index += 1;
            self.ws();
            if self.eat(b',') {
                continue;
            }
            return self.expect(b']');
        }
    }

    fn node(&mut self, node_index: usize, out: &mut Vec<PathRef>) -> Result<(), ScanError> {
        self.expect(b'{')?;
        let mut type_name: Option<String> = None;
        let mut found: Vec<PathRef> = Vec::new();
        self.ws();
        if !self.eat(b'}') {
            loop {
                self.ws();
                let (_, key) = self.string()?;
                self.ws();
                self.expect(b':')?;
                self.ws();
                let wanted = match key.as_str() {
                    "file" => Some(PathKey::File),
                    "background" => Some(PathKey::Background),
                    _ => None,
                };
                if (wanted.is_some() || key == "type") && self.peek() == Some(b'"') {
                    let (range, value) = self.string()?;
                    match wanted {
                        Some(key) => found.push(PathRef {
                            node_index,
                            key,
                            range,
                            value,
                        }),
                        // Like JSON.parse, the last duplicate key wins.
                        None => type_name = Some(value),
                    }
                } else {
                    self.skip_value(3)?;
                }
                self.ws();
                if self.eat(b',') {
                    continue;
                }
                self.expect(b'}')?;
                break;
            }
        }
        let keep = |k: PathKey| match k {
            PathKey::File => type_name.as_deref() == Some("file"),
            PathKey::Background => type_name.as_deref() == Some("group"),
        };
        out.extend(found.into_iter().filter(|r| keep(r.key)));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_paths_with_ranges() {
        let src = "{\"edges\":[{\"file\":\"x\"}],\"nodes\":[\n\t{\"id\":\"a\",\"file\":\"A\\u00e9.md\",\"type\":\"file\"},\n\t{\"type\":\"group\",\"background\":\"img/b.png\",\"file\":\"ignored\"},\n\t{\"type\":\"text\",\"text\":\"{\\\"file\\\":1}\"}]}";
        let refs = path_refs(src).expect("scan");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].value, "A\u{e9}.md");
        assert_eq!(&src[refs[0].range.clone()], "\"A\\u00e9.md\"");
        assert_eq!(refs[0].key, PathKey::File);
        assert_eq!(refs[1].node_index, 1);
        assert_eq!(refs[1].key, PathKey::Background);
    }

    #[test]
    fn surrogate_pairs_and_errors() {
        let refs = path_refs(r#"{"nodes":[{"type":"file","file":"😀"}]}"#).expect("scan");
        assert_eq!(refs[0].value, "\u{1F600}");
        assert!(path_refs(r#"{"nodes":[{"type":"file","file":"x}"#).is_err());
        assert!(path_refs("[]").is_err());
        let deep = format!("{{\"a\":{}{}}}", "[".repeat(200), "]".repeat(200));
        assert!(path_refs(&deep).is_err());
    }

    #[test]
    fn rename_round_trip() {
        let src = "{\n\t\"nodes\":[\n\t\t{\"id\":\"1\",\"type\":\"file\",\"file\":\"old.md\",\"x\":0},\n\t\t{\"id\":\"2\",\"type\":\"file\",\"file\":\"keep.md\"}\n\t]\n}";
        let patches = plan_path_patches(src, |r| {
            (r.value == "old.md").then(|| "dir/new \"q\".md".to_owned())
        })
        .expect("plan");
        assert_eq!(patches.len(), 1);
        let out = apply_patches(src, &patches).expect("apply");
        assert_eq!(out, src.replace("\"old.md\"", "\"dir/new \\\"q\\\".md\""));
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(v["nodes"][0]["file"], "dir/new \"q\".md");
    }

    #[test]
    fn apply_rejects_bad_patches() {
        let p = |a, b| Patch {
            range: a..b,
            replacement: String::new(),
        };
        assert!(matches!(
            apply_patches("abc", &[p(0, 2), p(1, 3)]),
            Err(PatchError::Overlap(..))
        ));
        assert!(matches!(
            apply_patches("abc", &[p(2, 9)]),
            Err(PatchError::OutOfBounds(..))
        ));
        assert!(matches!(
            apply_patches("é", &[p(1, 2)]),
            Err(PatchError::NotCharBoundary(..))
        ));
        assert_eq!(
            apply_patches("abc", &[p(2, 3), p(0, 1)]).as_deref(),
            Ok("b")
        );
    }

    #[test]
    fn encoding_matches_serde() {
        let s = "a\"b\\c\nd\u{1}é/";
        let ours = encode_json_string(s);
        let back: String = serde_json::from_str(&ours).expect("valid");
        assert_eq!(back, s);
        assert_eq!(ours, "\"a\\\"b\\\\c\\nd\\u0001é/\"");
    }
}
