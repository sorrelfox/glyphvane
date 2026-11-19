// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Which completion the text before the caret asks for.
//!
//! [`detect`] looks only at the caret's line: the text before the caret,
//! the text after it (for the closers smart pairs typed) and the block the
//! line is in, as the syntax tree says ([`Block`]). Positions are relative
//! to the line's start until [`Trigger::shifted`] moves them into the
//! document.

use std::ops::Range;

/// The block the caret's line is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Block {
    /// Paragraphs, headings, quotes, list items, tables.
    Prose,
    /// Code, HTML, math and comment blocks: nothing completes there.
    Verbatim,
    /// The opening fence line of a fenced code block.
    FenceOpen,
    /// A line between the front matter's delimiters. `list_key` is the key
    /// whose list a `- item` line continues.
    FrontMatter { list_key: Option<String> },
}

/// The syntax the document's profile has, which decides the triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Features {
    /// `[[…]]` wikilinks.
    pub wikilinks: bool,
    /// `![[…]]` embeds.
    pub embeds: bool,
    /// `#tag`s.
    pub tags: bool,
    /// `[^label]` footnotes.
    pub footnotes: bool,
    /// `:shortcode:` emoji.
    pub emoji: bool,
    /// Obsidian callouts or GitHub alerts.
    pub callouts: bool,
    /// Front matter.
    pub front_matter: bool,
}

/// Whose headings or blocks a wikilink lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Scope {
    /// The document being edited (`[[#`, `[[^`).
    Current,
    /// Every note of the vault (`[[##`, `[[^^`).
    Vault,
    /// The note written before the `#` (`[[Note#`, `[[Note#^`, `[[Note^`).
    Note(String),
}

/// What a trigger completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `[[` and `![[`: notes and other files.
    WikiFile,
    /// `[[#`, `[[##`, `[[Note#`: headings.
    WikiHeading(Scope),
    /// `[[^`, `[[^^`, `[[#^`, `[[Note#^`, `[[Note^`: blocks.
    WikiBlock(Scope),
    /// `](` and `![…](`: a path.
    LinkPath,
    /// `](path#`: a heading of that file (the current one when `path` is
    /// empty).
    LinkAnchor { path: String },
    /// `#` and a letter: a tag.
    Tag,
    /// `:` and two letters: an emoji shortcode.
    Emoji,
    /// `> [!`: a callout or alert type.
    Callout,
    /// After an opening fence: a language.
    Fence,
    /// `[^`: a footnote label.
    Footnote,
    /// A front matter key.
    PropertyName,
    /// A front matter value of `key`, after the quote that opened it (if
    /// any), in a flow list (`[a, b]`) or not.
    PropertyValue {
        key: String,
        quote: Option<char>,
        flow: bool,
    },
}

/// A completion the text asks for, and the text an accepted item replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Trigger {
    pub kind: Kind,
    /// The trigger's first byte (`!`, `[`, `#`, `:`, `>`, the fence, the
    /// key): Esc dismisses the trigger that starts there.
    pub start: usize,
    /// Wikilinks: right after `[[`; Markdown links: right after `](`;
    /// otherwise the query's start.
    pub inner: usize,
    /// The typed text the items are matched against; it ends at the caret.
    pub query: Range<usize>,
    /// The caret and the closer after it that accepting consumes (`]]`
    /// that smart pairs typed, `]` after `[^` and `[!`, `:` after an emoji
    /// name).
    pub end: usize,
    /// `![[` and `![…](`.
    pub embed: bool,
    /// A Markdown link's text between `[` and `]`.
    pub text: Option<Range<usize>>,
}

impl Trigger {
    /// The trigger with every position moved by `base` (a line's start).
    pub fn shifted(mut self, base: usize) -> Self {
        let shift = |r: Range<usize>| r.start + base..r.end + base;
        self.start += base;
        self.inner += base;
        self.query = shift(self.query);
        self.end += base;
        self.text = self.text.map(shift);
        self
    }
}

/// The trigger the caret's line asks for, if any: `before` is the line up
/// to the caret, `after` the rest of it.
pub(crate) fn detect(
    before: &str,
    after: &str,
    block: &Block,
    features: Features,
) -> Option<Trigger> {
    match block {
        Block::Verbatim => None,
        Block::FenceOpen => fence(before),
        Block::FrontMatter { list_key } => property(before, after, list_key.as_deref()),
        Block::Prose => {
            if fence(before).is_some() {
                // An opening fence the tree has not seen yet: only the
                // language completes.
                return fence(before);
            }
            if in_code_span(before) {
                return None;
            }
            wiki(before, after, features)
                .or_else(|| link(before))
                .or_else(|| footnote(before, after, features))
                .or_else(|| callout(before, after, features))
                .or_else(|| tag(before, features))
                .or_else(|| emoji(before, after, features))
        }
    }
}

/// An odd number of backticks before the caret: inside a code span.
fn in_code_span(before: &str) -> bool {
    before.bytes().filter(|&b| b == b'`').count() % 2 == 1
}

fn plain(kind: Kind, start: usize, query: Range<usize>, closer: usize) -> Trigger {
    Trigger {
        kind,
        start,
        inner: query.start,
        end: query.end + closer,
        query,
        embed: false,
        text: None,
    }
}

/// `[[…`, `![[…`.
fn wiki(before: &str, after: &str, features: Features) -> Option<Trigger> {
    if !features.wikilinks && !features.embeds {
        return None;
    }
    let open = before.rfind("[[")?;
    let inner_start = open + 2;
    let inner = &before[inner_start..];
    if inner.contains(['[', ']', '|']) {
        return None;
    }
    let embed = before[..open].ends_with('!');
    if (embed && !features.embeds) || (!embed && !features.wikilinks) {
        return None;
    }
    let start = if embed { open - 1 } else { open };
    let caret = before.len();
    let closer = if after.starts_with("]]") { 2 } else { 0 };
    let (kind, query_start) = if inner.starts_with("#^") {
        (Kind::WikiBlock(Scope::Current), inner_start + 2)
    } else if inner.starts_with("##") {
        (Kind::WikiHeading(Scope::Vault), inner_start + 2)
    } else if inner.starts_with('#') {
        (Kind::WikiHeading(Scope::Current), inner_start + 1)
    } else if inner.starts_with("^^") {
        (Kind::WikiBlock(Scope::Vault), inner_start + 2)
    } else if inner.starts_with('^') {
        (Kind::WikiBlock(Scope::Current), inner_start + 1)
    } else if let Some(hash) = inner.find('#') {
        let note = inner[..hash].to_owned();
        let rest = &inner[hash + 1..];
        if rest.contains('#') {
            return None;
        }
        if rest.starts_with('^') {
            (Kind::WikiBlock(Scope::Note(note)), inner_start + hash + 2)
        } else {
            (Kind::WikiHeading(Scope::Note(note)), inner_start + hash + 1)
        }
    } else if let Some(caret) = inner.find('^') {
        // `[[Note^`: Obsidian's short form of `[[Note#^`.
        let note = inner[..caret].to_owned();
        if inner[caret + 1..].contains('^') {
            return None;
        }
        (Kind::WikiBlock(Scope::Note(note)), inner_start + caret + 1)
    } else {
        (Kind::WikiFile, inner_start)
    };
    Some(Trigger {
        kind,
        start,
        inner: inner_start,
        query: query_start..caret,
        end: caret + closer,
        embed,
        text: None,
    })
}

/// `[text](…` and `![alt](…`.
fn link(before: &str) -> Option<Trigger> {
    let open = before.rfind("](")?;
    let dest_start = open + 2;
    let dest = &before[dest_start..];
    // A destination with spaces is written `<…>`; a URL has a scheme.
    if dest.contains([' ', '\t', '(', ')', '<', '>', ':']) {
        return None;
    }
    let text_open = before[..open].rfind('[')?;
    if before[text_open + 1..open].contains(']') || before[..text_open].ends_with('[') {
        return None;
    }
    let embed = before[..text_open].ends_with('!');
    let start = if embed { text_open - 1 } else { text_open };
    let caret = before.len();
    let (kind, query_start) = match dest.find('#') {
        Some(hash) => (
            Kind::LinkAnchor {
                path: dest[..hash].to_owned(),
            },
            dest_start + hash + 1,
        ),
        None => (Kind::LinkPath, dest_start),
    };
    Some(Trigger {
        kind,
        start,
        inner: dest_start,
        query: query_start..caret,
        end: caret,
        embed,
        text: Some(text_open + 1..open),
    })
}

/// `[^label`.
fn footnote(before: &str, after: &str, features: Features) -> Option<Trigger> {
    if !features.footnotes {
        return None;
    }
    let open = before.rfind("[^")?;
    let label = &before[open + 2..];
    if label.contains([']', '[', ' ', '\t']) || before[..open].ends_with('[') {
        return None;
    }
    let closer = usize::from(after.starts_with(']'));
    Some(plain(Kind::Footnote, open, open + 2..before.len(), closer))
}

/// `> [!type` at the start of a quote.
fn callout(before: &str, after: &str, features: Features) -> Option<Trigger> {
    if !features.callouts {
        return None;
    }
    let indent = before.len() - before.trim_start_matches([' ', '\t']).len();
    let mut rest = &before[indent..];
    if !rest.starts_with('>') {
        return None;
    }
    while let Some(r) = rest.strip_prefix('>') {
        rest = r.trim_start_matches([' ', '\t']);
    }
    let name = rest.strip_prefix("[!")?;
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let closer = usize::from(after.starts_with(']'));
    let caret = before.len();
    Some(plain(
        Kind::Callout,
        indent,
        caret - name.len()..caret,
        closer,
    ))
}

/// Characters a tag name may have.
fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '/')
}

/// `#name` after a space or at the line's start.
fn tag(before: &str, features: Features) -> Option<Trigger> {
    if !features.tags {
        return None;
    }
    let hash = before.rfind('#')?;
    let name = &before[hash + 1..];
    if name.is_empty() || !name.chars().all(is_tag_char) {
        return None;
    }
    let allowed = before[..hash]
        .chars()
        .next_back()
        .is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '[' | ',' | ';'));
    if !allowed {
        return None;
    }
    Some(plain(Kind::Tag, hash, hash + 1..before.len(), 0))
}

/// `:` and at least two shortcode characters, not after a letter or digit
/// (`10:30`, `https:`).
fn emoji(before: &str, after: &str, features: Features) -> Option<Trigger> {
    if !features.emoji {
        return None;
    }
    let colon = before.rfind(':')?;
    let code = &before[colon + 1..];
    if code.len() < 2
        || !code.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'+' | b'-')
        })
    {
        return None;
    }
    let allowed = before[..colon]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_alphanumeric() && !matches!(c, ':' | '/' | '\\'));
    if !allowed {
        return None;
    }
    let closer = usize::from(after.starts_with(':'));
    Some(plain(Kind::Emoji, colon, colon + 1..before.len(), closer))
}

/// An opening fence (up to three spaces, then three or more backticks or
/// tildes) and the info string typed so far.
fn fence(before: &str) -> Option<Trigger> {
    let indent = before.len() - before.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &before[indent..];
    let marker = rest.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let run = rest.len() - rest.trim_start_matches(marker).len();
    if run < 3 {
        return None;
    }
    let info = &rest[run..];
    if info.contains(|c: char| c.is_whitespace() || c == '`' || c == '~') {
        return None;
    }
    let caret = before.len();
    Some(plain(Kind::Fence, indent, caret - info.len()..caret, 0))
}

/// A key at the start of a front matter line, its value after `key: `, or
/// an item of the list `list_key` starts.
fn property(before: &str, after: &str, list_key: Option<&str>) -> Option<Trigger> {
    let caret = before.len();
    if before
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
        && before
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
    {
        // A `:` already there is written again with the key.
        let closer = if after.starts_with(": ") {
            2
        } else {
            usize::from(after.starts_with(':'))
        };
        return Some(plain(Kind::PropertyName, 0, 0..caret, closer));
    }
    let trimmed = before.trim_start_matches([' ', '\t']);
    if trimmed.len() != before.len() || before.starts_with('-') {
        // An item of a block list.
        let item = trimmed.strip_prefix('-')?;
        let value = item.trim_start_matches([' ', '\t']);
        if value.len() == item.len() {
            return None;
        }
        let key = list_key?.to_owned();
        let quote = opening_quote(value);
        let value = value.trim_start_matches(['"', '\'']);
        return Some(plain(
            Kind::PropertyValue {
                key,
                quote,
                flow: false,
            },
            0,
            caret - value.len()..caret,
            0,
        ));
    }
    let colon = before.find(':')?;
    let key = before[..colon].trim();
    if key.is_empty() || key.starts_with('#') {
        return None;
    }
    let rest = &before[colon + 1..];
    let value = rest.trim_start_matches([' ', '\t']);
    if value.len() == rest.len() {
        return None;
    }
    // In a flow list (`[a, b`), the item being typed.
    let flow = value.starts_with('[');
    let value = match value.strip_prefix('[') {
        Some(list) => list
            .rsplit(',')
            .next()
            .unwrap_or(list)
            .trim_start_matches(' '),
        None => value,
    };
    let quote = opening_quote(value);
    let value = value.trim_start_matches(['"', '\'']);
    Some(plain(
        Kind::PropertyValue {
            key: key.to_owned(),
            quote,
            flow,
        },
        0,
        caret - value.len()..caret,
        0,
    ))
}

/// The quote a front matter value being typed starts with.
fn opening_quote(value: &str) -> Option<char> {
    value.chars().next().filter(|c| matches!(c, '"' | '\''))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Features = Features {
        wikilinks: true,
        embeds: true,
        tags: true,
        footnotes: true,
        emoji: true,
        callouts: true,
        front_matter: true,
    };

    fn prose(before: &str, after: &str) -> Option<Trigger> {
        detect(before, after, &Block::Prose, ALL)
    }

    fn kind(before: &str) -> Option<Kind> {
        prose(before, "").map(|t| t.kind)
    }

    fn query<'a>(before: &'a str, t: &Trigger) -> &'a str {
        &before[t.query.clone()]
    }

    #[test]
    fn wikilinks_and_their_modes() {
        let t = prose("See [[Fo", "]]").expect("trigger");
        assert_eq!(t.kind, Kind::WikiFile);
        assert_eq!((t.start, t.inner, t.end), (4, 6, 10));
        assert_eq!(query("See [[Fo", &t), "Fo");
        assert!(!t.embed);
        let t = prose("![[pic", "").expect("trigger");
        assert!(t.embed);
        assert_eq!((t.start, t.end), (0, 6));
        assert_eq!(kind("[[#He"), Some(Kind::WikiHeading(Scope::Current)));
        assert_eq!(kind("[[##He"), Some(Kind::WikiHeading(Scope::Vault)));
        assert_eq!(kind("[[^ab"), Some(Kind::WikiBlock(Scope::Current)));
        assert_eq!(kind("[[^^ab"), Some(Kind::WikiBlock(Scope::Vault)));
        assert_eq!(kind("[[#^ab"), Some(Kind::WikiBlock(Scope::Current)));
        assert_eq!(
            kind("[[Note#He"),
            Some(Kind::WikiHeading(Scope::Note("Note".into())))
        );
        let t = prose("[[Note#^ab", "").expect("trigger");
        assert_eq!(t.kind, Kind::WikiBlock(Scope::Note("Note".into())));
        assert_eq!(query("[[Note#^ab", &t), "ab");
        let t = prose("[[Note^ab", "").expect("trigger");
        assert_eq!(t.kind, Kind::WikiBlock(Scope::Note("Note".into())));
        assert_eq!(query("[[Note^ab", &t), "ab");
        // Display text, closed links.
        assert_eq!(kind("[[Note|Al"), None);
        assert_eq!(kind("[[Note]] and"), None);
    }

    #[test]
    fn wikilinks_follow_the_profile() {
        let gfm = Features {
            footnotes: true,
            ..Features::default()
        };
        assert_eq!(detect("[[Fo", "", &Block::Prose, gfm), None);
        let no_embeds = Features {
            wikilinks: true,
            ..Features::default()
        };
        assert_eq!(detect("![[Fo", "", &Block::Prose, no_embeds), None);
        assert!(detect("[[Fo", "", &Block::Prose, no_embeds).is_some());
    }

    #[test]
    fn markdown_link_paths_and_anchors() {
        let t = prose("A [x](docs/re", ")").expect("trigger");
        assert_eq!(t.kind, Kind::LinkPath);
        assert_eq!(query("A [x](docs/re", &t), "docs/re");
        assert_eq!(t.text, Some(3..4));
        assert_eq!(t.end, 13, "the `)` stays");
        let t = prose("[](Note.md#Int", "").expect("trigger");
        assert_eq!(
            t.kind,
            Kind::LinkAnchor {
                path: "Note.md".into()
            }
        );
        assert_eq!(query("[](Note.md#Int", &t), "Int");
        assert!(prose("![alt](img", "").expect("trigger").embed);
        assert_eq!(kind("[x](https://e"), None);
        assert_eq!(kind("[x](a b"), None);
    }

    #[test]
    fn footnotes_callouts_tags_emoji() {
        let t = prose("text[^no", "]").expect("trigger");
        assert_eq!(t.kind, Kind::Footnote);
        assert_eq!(t.end, 9);
        let t = prose("> [!ti", "]").expect("trigger");
        assert_eq!(t.kind, Kind::Callout);
        assert_eq!(query("> [!ti", &t), "ti");
        assert_eq!(t.end, 7);
        assert_eq!(kind("> > [!"), Some(Kind::Callout));
        assert_eq!(kind("[!no"), None);
        assert_eq!(kind("#pro"), Some(Kind::Tag));
        assert_eq!(kind("a #pro/su"), Some(Kind::Tag));
        assert_eq!(kind("#"), None);
        assert_eq!(kind("a#b"), None);
        assert_eq!(kind("# Head"), None);
        assert_eq!(kind("hi :sm"), Some(Kind::Emoji));
        assert_eq!(kind(":s"), None);
        assert_eq!(kind("at 10:30"), None);
        assert_eq!(kind("https://x"), None);
        let t = prose(":smi", ":").expect("trigger");
        assert_eq!(t.end, 5);
    }

    #[test]
    fn nothing_in_code() {
        assert_eq!(kind("`[[Fo"), None);
        assert_eq!(detect("[[Fo", "", &Block::Verbatim, ALL), None);
        let t = detect("```ru", "", &Block::FenceOpen, ALL).expect("trigger");
        assert_eq!(t.kind, Kind::Fence);
        assert_eq!(query("```ru", &t), "ru");
        assert_eq!(kind("~~~~"), Some(Kind::Fence));
        assert_eq!(kind("``"), None);
        assert_eq!(kind("```a b"), None);
    }

    #[test]
    fn front_matter_keys_and_values() {
        let fm = |key: Option<&str>| Block::FrontMatter {
            list_key: key.map(str::to_owned),
        };
        let t = detect("sta", "", &fm(None), ALL).expect("trigger");
        assert_eq!(t.kind, Kind::PropertyName);
        assert_eq!(detect("sta", ": x", &fm(None), ALL).map(|t| t.end), Some(5));
        assert_eq!(detect("- x", "", &fm(None), ALL), None);
        let t = detect("status: dr", "", &fm(None), ALL).expect("trigger");
        assert_eq!(
            t.kind,
            Kind::PropertyValue {
                key: "status".into(),
                quote: None,
                flow: false,
            }
        );
        assert_eq!(query("status: dr", &t), "dr");
        let t = detect("tags: [a, b", "", &fm(None), ALL).expect("trigger");
        assert_eq!(query("tags: [a, b", &t), "b");
        assert!(matches!(t.kind, Kind::PropertyValue { flow: true, .. }));
        let t = detect("title: 'My", "", &fm(None), ALL).expect("trigger");
        assert_eq!(query("title: 'My", &t), "My");
        assert!(matches!(
            t.kind,
            Kind::PropertyValue {
                quote: Some('\''),
                ..
            }
        ));
        let t = detect("  - wo", "", &fm(Some("tags")), ALL).expect("trigger");
        assert_eq!(
            t.kind,
            Kind::PropertyValue {
                key: "tags".into(),
                quote: None,
                flow: false,
            }
        );
        assert_eq!(query("  - wo", &t), "wo");
        assert_eq!(detect("  - wo", "", &fm(None), ALL), None);
        assert_eq!(detect("status:", "", &fm(None), ALL), None);
    }

    #[test]
    fn shifting_moves_every_position() {
        let t = prose("[](a", "").expect("trigger").shifted(10);
        assert_eq!((t.start, t.inner, t.query, t.end), (10, 13, 13..14, 14));
        assert_eq!(t.text, Some(11..11));
    }
}
