// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The raw HTML blocks the Reading view renders natively before the HTML
//! pipeline exists: `<details>` (a summary row that folds the blocks up
//! to `</details>`), `<picture>` (a source picked by the app theme) and
//! one `<audio>`, `<video>` or `<iframe>` element (a media card,
//! [`crate::live`]).
//!
//! A small tag scanner reads the tags of one HTML block: names and
//! attributes, nothing else. Blocks it does not recognise stay raw HTML.

use std::ops::Range;
use std::sync::Arc;

use crate::node::Picture;
use crate::status::ImageSize;

/// A start or end tag of an HTML block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tag {
    /// The name in lower case.
    pub name: String,
    pub end: bool,
    /// Attribute names in lower case, values unquoted (no entity
    /// decoding).
    pub attrs: Vec<(String, String)>,
    /// The tag's bytes in the block's text.
    pub range: Range<usize>,
}

impl Tag {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// The tags of `s`, in order (comments, declarations and malformed tags
/// are skipped).
pub(crate) fn tags(s: &str) -> Vec<Tag> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(k) = memchr_lt(b, i) {
        i = k + 1;
        if s[i..].starts_with("!--") {
            i = s[i..].find("-->").map_or(b.len(), |e| i + e + 3);
            continue;
        }
        let end = b.get(i) == Some(&b'/');
        let mut j = i + usize::from(end);
        let name_start = j;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'-') {
            j += 1;
        }
        if j == name_start || !b[name_start].is_ascii_alphabetic() {
            continue;
        }
        let name = s[name_start..j].to_ascii_lowercase();
        let mut attrs = Vec::new();
        loop {
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            match b.get(j) {
                None => break,
                Some(b'>') => {
                    j += 1;
                    break;
                }
                Some(b'/') => {
                    j += 1;
                    continue;
                }
                _ => {}
            }
            let a = j;
            while j < b.len() && !b[j].is_ascii_whitespace() && !matches!(b[j], b'=' | b'>' | b'/')
            {
                j += 1;
            }
            if j == a {
                j += 1;
                continue;
            }
            let attr = s[a..j].to_ascii_lowercase();
            let mut value = String::new();
            if b.get(j) == Some(&b'=') {
                j += 1;
                match b.get(j) {
                    Some(&q @ (b'"' | b'\'')) => {
                        let v = j + 1;
                        let e = s[v..].find(char::from(q)).map_or(b.len(), |e| v + e);
                        value = s[v..e].to_owned();
                        j = (e + 1).min(b.len());
                    }
                    _ => {
                        let v = j;
                        while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                            j += 1;
                        }
                        value = s[v..j].to_owned();
                    }
                }
            }
            attrs.push((attr, value));
        }
        out.push(Tag {
            name,
            end,
            attrs,
            range: k..j,
        });
        i = j;
    }
    out
}

fn memchr_lt(b: &[u8], from: usize) -> Option<usize> {
    b.get(from..)?
        .iter()
        .position(|&c| c == b'<')
        .map(|p| from + p)
}

/// What an HTML block does for `<details>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DetailsPart {
    /// It opens a `<details>` that a later block closes: whether it is
    /// open, and its summary text with its range in the block's text.
    Open {
        open: bool,
        summary: Option<(String, Range<usize>)>,
    },
    /// It closes the innermost open `<details>`; `only`: it holds nothing
    /// else.
    Close { only: bool },
}

/// Whether the HTML block with text `s` opens or closes a `<details>`.
/// A block that opens and closes one is neither (it stays raw HTML).
pub(crate) fn details(s: &str) -> Option<DetailsPart> {
    let t = tags(s);
    let first = t.first()?;
    if first.name == "details" && !first.end && s[..first.range.start].trim().is_empty() {
        if t.iter().any(|x| x.name == "details" && x.end) {
            return None;
        }
        let open = first.attr("open").is_some();
        let summary = t
            .iter()
            .position(|x| x.name == "summary" && !x.end)
            .and_then(|i| {
                let start = t[i].range.end;
                let close = t[i + 1..]
                    .iter()
                    .find(|x| x.name == "summary" && x.end)
                    .map_or(s.len(), |x| x.range.start);
                // The text between the summary's tags, without tags.
                let mut text = String::new();
                let mut at = start;
                for x in t[i + 1..].iter().filter(|x| x.range.start < close) {
                    text.push_str(&s[at..x.range.start]);
                    at = x.range.end;
                }
                text.push_str(&s[at.min(close)..close]);
                let text = collapse(&decode_basic(&text));
                (!text.is_empty()).then_some((text, start..close))
            });
        return Some(DetailsPart::Open { open, summary });
    }
    let close = t.iter().position(|x| x.name == "details" && x.end)?;
    if t[..close].iter().any(|x| x.name == "details") {
        return None;
    }
    let tag = &t[close];
    let only = s[..tag.range.start].trim().is_empty() && s[tag.range.end..].trim().is_empty();
    Some(DetailsPart::Close { only })
}

/// The `<picture>` of an HTML block that is one, else `None`.
pub(crate) fn picture(s: &str) -> Option<Picture> {
    let t = tags(s);
    let first = t.first()?;
    if first.name != "picture" || first.end || !s[..first.range.start].trim().is_empty() {
        return None;
    }
    let mut sources = Vec::new();
    let mut fallback = None;
    let mut alt = String::new();
    for x in t.iter().skip(1) {
        if x.end {
            if x.name == "picture" {
                break;
            }
            continue;
        }
        match x.name.as_str() {
            "source" => {
                let Some(url) = x.attr("srcset").and_then(first_url) else {
                    continue;
                };
                let media = x
                    .attr("media")
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .map(Arc::from);
                sources.push((media, Arc::from(url)));
            }
            "img" if fallback.is_none() => {
                fallback = x
                    .attr("src")
                    .or_else(|| x.attr("srcset").and_then(first_url))
                    .map(Arc::from);
                alt = decode_basic(x.attr("alt").unwrap_or(""));
            }
            _ => {}
        }
    }
    Some(Picture {
        sources: sources.into_boxed_slice(),
        fallback,
        alt: Arc::from(alt),
        from: None,
        sizes: [ImageSize::Pending; 2],
    })
}

/// An `<audio>`, `<video>` or `<iframe>` HTML block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MediaTag {
    pub kind: crate::live::LiveKind,
    /// The element's `src`, else its first `<source src>`.
    pub src: String,
    pub title: Option<String>,
    pub poster: Option<String>,
}

/// The media element of an HTML block that is only one `<audio>`,
/// `<video>` or `<iframe>` with a source (text inside it, such as a
/// fallback message, is allowed), else `None`.
pub(crate) fn media(s: &str) -> Option<MediaTag> {
    use crate::live::LiveKind;
    let t = tags(s);
    let first = t.first()?;
    let kind = match first.name.as_str() {
        "audio" => LiveKind::Audio,
        "video" => LiveKind::Video,
        "iframe" => LiveKind::Frame,
        _ => return None,
    };
    if first.end || !s[..first.range.start].trim().is_empty() {
        return None;
    }
    // Nothing after the element's end tag; without one, nothing but
    // `<source>` and `<track>` tags after the start tag.
    let close = t.iter().position(|x| x.end && x.name == first.name);
    let inside = &t[1..close.unwrap_or(t.len())];
    match close {
        Some(i) if !s[t[i].range.end..].trim().is_empty() => return None,
        None if inside
            .iter()
            .any(|x| x.name != "source" && x.name != "track") =>
        {
            return None;
        }
        _ => {}
    }
    let non_empty = |v: &str| {
        let v = decode_basic(v.trim());
        (!v.is_empty()).then_some(v)
    };
    let src = first.attr("src").and_then(non_empty).or_else(|| {
        inside
            .iter()
            .find(|x| x.name == "source" && !x.end)
            .and_then(|x| x.attr("src"))
            .and_then(non_empty)
    })?;
    Some(MediaTag {
        kind,
        src,
        title: first.attr("title").and_then(non_empty),
        poster: first
            .attr("poster")
            .filter(|_| kind == LiveKind::Video)
            .and_then(non_empty),
    })
}

/// The first URL of a `srcset`.
fn first_url(srcset: &str) -> Option<&str> {
    srcset
        .split(',')
        .next()?
        .split_whitespace()
        .next()
        .filter(|u| !u.is_empty())
}

/// `&amp; &lt; &gt; &quot; &#39;` decoded (what summaries and alt texts
/// use).
fn decode_basic(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Runs of whitespace as one space, trimmed.
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_elements_are_found_with_their_source() {
        use crate::live::LiveKind;
        let m = media("<audio controls src=\"a b.mp3\"></audio>\n").expect("audio");
        assert_eq!((m.kind, m.src.as_str()), (LiveKind::Audio, "a b.mp3"));
        let m = media("<video poster=p.png controls>\n<source src=\"v.mp4\" type=video/mp4>\nNo video.\n</video>")
            .expect("video");
        assert_eq!(m.kind, LiveKind::Video);
        assert_eq!(m.src, "v.mp4");
        assert_eq!(m.poster.as_deref(), Some("p.png"));
        let m = media("<iframe width=560 src=\"https://www.youtube.com/embed/abc?x=1&amp;y=2\" title=\"A talk\"></iframe>")
            .expect("iframe");
        assert_eq!(m.kind, LiveKind::Frame);
        assert_eq!(m.src, "https://www.youtube.com/embed/abc?x=1&y=2");
        assert_eq!(m.title.as_deref(), Some("A talk"));
        // No source, other content around it, other elements: raw HTML.
        assert_eq!(media("<audio controls></audio>"), None);
        assert_eq!(media("<p>x</p><audio src=a.mp3></audio>"), None);
        assert_eq!(media("<audio src=a.mp3></audio><p>after</p>"), None);
        assert_eq!(media("<div><iframe src=x></iframe></div>"), None);
    }

    #[test]
    fn scans_tags_and_attributes() {
        let t = tags("<details open><summary class=x>Hi</summary><!-- <b> --><img src='a b'/>");
        let names: Vec<_> = t.iter().map(|x| (x.name.as_str(), x.end)).collect();
        assert_eq!(
            names,
            [
                ("details", false),
                ("summary", false),
                ("summary", true),
                ("img", false)
            ]
        );
        assert_eq!(t[0].attr("open"), Some(""));
        assert_eq!(t[1].attr("class"), Some("x"));
        assert_eq!(t[3].attr("src"), Some("a b"));
    }

    #[test]
    fn details_blocks() {
        assert_eq!(
            details("<details>\n<summary>Click <b>me</b> &amp; more</summary>\n"),
            Some(DetailsPart::Open {
                open: false,
                summary: Some(("Click me & more".to_owned(), 19..45)),
            })
        );
        assert_eq!(
            details("<details open>"),
            Some(DetailsPart::Open {
                open: true,
                summary: None
            })
        );
        assert_eq!(
            details("</details>\n"),
            Some(DetailsPart::Close { only: true })
        );
        assert_eq!(
            details("<p>x</p></details>"),
            Some(DetailsPart::Close { only: false })
        );
        assert_eq!(details("<details><summary>a</summary>b</details>"), None);
        assert_eq!(details("<div>x</div>"), None);
    }

    #[test]
    fn pictures() {
        let p = picture(
            "<picture>\n  <source media=\"(prefers-color-scheme: dark)\" srcset=\"dark.png 2x, d1.png\">\n  \
             <source media=\"(prefers-color-scheme: light)\" srcset=\"light.png\">\n  \
             <img alt=\"Logo &amp; name\" src=\"fallback.png\">\n</picture>",
        )
        .expect("a picture");
        assert_eq!(p.pick(true), Some("dark.png"));
        assert_eq!(p.pick(false), Some("light.png"));
        assert_eq!(&*p.alt, "Logo & name");
        assert_eq!(p.fallback.as_deref(), Some("fallback.png"));
        let only_img = picture("<picture><img src=\"x.png\" alt=\"\"></picture>").expect("picture");
        assert_eq!(only_img.pick(true), Some("x.png"));
        assert!(picture("<p>no</p>").is_none());
    }
}
