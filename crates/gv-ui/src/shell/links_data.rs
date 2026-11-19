// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What the Backlinks and *Links in this note* sections show, computed from
//! a content index snapshot without the interface: groups of links by
//! source note, context lines with the link marked, link findings by
//! severity, and the text the Link action of an unlinked mention writes.

use std::ops::Range;
use std::sync::Arc;

use gpui::SharedString;
use gv_syntax::inline::InlineOptions;
use gv_vault::VaultPath;
use gv_vault::content::{Backlink, ContentSnapshot, FileSource, Mention, file_stem};
use gv_vault::keys::fold_key;
use gv_vault::resolve::Status;

use crate::search_panel::excerpt;

/// Characters kept before the link in a context line; more are cut at a
/// word with "…".
const CONTEXT_BEFORE: usize = 60;
/// Characters kept after the link.
const CONTEXT_AFTER: usize = 140;

/// A context line: the source line with the link (or mention) shown as
/// its text, marked by `highlight`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Snippet {
    pub text: SharedString,
    /// Byte range of the link's text in `text`.
    pub highlight: Range<usize>,
    /// The formulas of the line (byte ranges of `text`), to typeset.
    pub math: Vec<excerpt::Formula>,
}

/// One link, or one unlinked mention, in a source note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Item {
    /// The link or the mentioned words in the source text.
    pub span: Range<usize>,
    /// The innermost heading above it.
    pub heading: Option<Arc<str>>,
    /// "embed", "front matter", "alias" (linked mentions).
    pub note: Option<&'static str>,
    /// The words that mention the note (unlinked mentions).
    pub term: Option<Arc<str>>,
}

/// The links (or mentions) of one source note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Group {
    pub source: VaultPath,
    pub title: SharedString,
    pub items: Vec<Item>,
}

impl Group {
    /// The heading the group's head shows at the right: the one heading all
    /// its items are under; `None` when they differ or there is none.
    pub fn shared_heading(&self) -> Option<&Arc<str>> {
        let first = self.items.first()?.heading.as_ref()?;
        self.items
            .iter()
            .all(|i| i.heading.as_ref() == Some(first))
            .then_some(first)
    }
}

/// How bad a link's status is, as the Link checker counts it (SPEC § Link
/// status).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Severity {
    Error,
    Warning,
}

/// The severity of a link with `status`, `None` for links that are fine.
pub(super) fn severity(status: Status) -> Option<Severity> {
    match status {
        Status::Unresolved | Status::Blocked => Some(Severity::Error),
        Status::CaseFold | Status::Loose | Status::Ambiguous(_) | Status::OutsideRoot => {
            Some(Severity::Warning)
        }
        Status::Exact | Status::Pending | Status::External => None,
    }
}

/// Link findings of one note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Findings {
    pub errors: usize,
    pub warnings: usize,
    /// Every link of the note.
    pub links: usize,
}

/// One link of the active note (*Links in this note*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Outgoing {
    /// The link as its text reads.
    pub label: SharedString,
    /// Where it points, when it resolved: the target's file name.
    pub target_name: Option<SharedString>,
    pub target: Option<VaultPath>,
    pub severity: Option<Severity>,
    pub status: Status,
    /// The link in the note.
    pub span: Range<usize>,
}

/// The links of `path` with their statuses, and the findings among them.
/// Front matter links are listed too.
pub(super) fn outgoing(
    snapshot: &ContentSnapshot,
    path: &VaultPath,
    text: Option<&str>,
) -> (Vec<Outgoing>, Findings) {
    let mut findings = Findings::default();
    let rows = snapshot
        .outgoing(path)
        .into_iter()
        .map(|link| {
            let severity = severity(link.target.status);
            findings.links += 1;
            match severity {
                Some(Severity::Error) => findings.errors += 1,
                Some(Severity::Warning) => findings.warnings += 1,
                None => {}
            }
            let span = link.link.span.range();
            let label = text
                .and_then(|t| t.get(span.clone()))
                .map_or_else(|| link.link.raw.to_string(), link_label);
            Outgoing {
                label: label.into(),
                target_name: link
                    .target
                    .target
                    .as_ref()
                    .and_then(|t| t.file_name().map(|n| SharedString::from(n.to_string()))),
                target: link.target.target.clone(),
                severity,
                status: link.target.status,
                span,
            }
        })
        .collect();
    (rows, findings)
}

/// The linked mentions of `target` grouped by source, in path order.
pub(super) fn backlink_groups(snapshot: &ContentSnapshot, target: &VaultPath) -> Vec<Group> {
    let aliases: Vec<String> = snapshot
        .note(target)
        .map(|r| r.data.aliases.iter().map(|a| fold_key(a)).collect())
        .unwrap_or_default();
    let stem = fold_key(file_stem(target));
    let mut groups: Vec<Group> = Vec::new();
    for link in snapshot.backlinks(target) {
        let note = backlink_note(&link, &stem, &aliases);
        let item = Item {
            span: link.link.span.range(),
            heading: link.heading_path.last().cloned(),
            note,
            term: None,
        };
        match groups.last_mut() {
            Some(group) if group.source == link.source => group.items.push(item),
            _ => groups.push(Group {
                title: title_of(snapshot, &link.source),
                source: link.source,
                items: vec![item],
            }),
        }
    }
    groups
}

/// What a backlink is labelled with: an embed, a front matter link, a link
/// through an alias.
fn backlink_note(link: &Backlink, stem: &str, aliases: &[String]) -> Option<&'static str> {
    if link.link.front_matter {
        return Some("front matter");
    }
    if link.link.is_embed() {
        return Some("embed");
    }
    if link.link.kind.is_wiki() {
        let name = link.link.raw.split(['|', '#']).next().unwrap_or("").trim();
        let name = name.rsplit('/').next().unwrap_or(name);
        let key = fold_key(name);
        if key != stem && aliases.contains(&key) {
            return Some("alias");
        }
    }
    None
}

/// Unlinked mentions grouped by source, in path order, then text order.
pub(super) fn mention_groups(snapshot: &ContentSnapshot, mut mentions: Vec<Mention>) -> Vec<Group> {
    mentions.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then(a.span.start.cmp(&b.span.start))
    });
    let mut groups: Vec<Group> = Vec::new();
    for m in mentions {
        let heading = snapshot
            .note(&m.source)
            .and_then(|r| r.heading_path(m.span.start as usize).pop());
        let item = Item {
            span: m.span.range(),
            heading,
            note: None,
            term: Some(m.term.clone()),
        };
        match groups.last_mut() {
            Some(group) if group.source == m.source => group.items.push(item),
            _ => groups.push(Group {
                title: title_of(snapshot, &m.source),
                source: m.source,
                items: vec![item],
            }),
        }
    }
    groups
}

fn title_of(snapshot: &ContentSnapshot, path: &VaultPath) -> SharedString {
    snapshot.note(path).map_or_else(
        || SharedString::from(file_stem(path).to_string()),
        |r| SharedString::from(r.title().to_string()),
    )
}

/// The context lines of `groups`, each read once (unsaved buffers from
/// memory). `link_text` shows links as their text; mentions stay as
/// written.
pub(super) fn snippets(
    snapshot: &ContentSnapshot,
    source: &dyn FileSource,
    groups: &[(VaultPath, Vec<Range<usize>>)],
    link_text: bool,
) -> Vec<Vec<Option<Snippet>>> {
    let options = excerpt::inline_options(snapshot.options().profile);
    groups
        .iter()
        .map(|(path, spans)| {
            let text = snapshot.note_text(path, source);
            spans
                .iter()
                .map(|span| {
                    let text = text.as_deref()?;
                    snippet(text, span.clone(), link_text, options)
                })
                .collect()
        })
        .collect()
}

/// The context line of `span` in `text`: its line without block markers
/// and inline markup (as the profile of `options` reads it), shortened
/// around the span, with the span shown as its link text.
pub(super) fn snippet(
    text: &str,
    span: Range<usize>,
    link_text: bool,
    options: InlineOptions,
) -> Option<Snippet> {
    let construct = text.get(span.clone())?;
    let line_start = text[..span.start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[span.end..]
        .find('\n')
        .map_or(text.len(), |i| i + span.end);
    let before = strip_block_markers(&text[line_start..span.start]);
    let content = span.start - before.len();
    let line = text[content..line_end].trim_end();
    let mark = span.start - content..span.end - content;
    let stripped = excerpt::strip(line, std::slice::from_ref(&mark), options);
    let (before, shown, after, mark, formulas) = match stripped.marks.first() {
        Some(m) if link_text || &stripped.text[m.clone()] == construct => (
            stripped.text[..m.start].to_string(),
            stripped.text[m.clone()].to_string(),
            stripped.text[m.end..].to_string(),
            m.clone(),
            stripped.math,
        ),
        // The span is not one piece of text (a mention across markup):
        // the line as written, its formulas not typeset.
        _ => (
            before,
            if link_text {
                link_label(construct)
            } else {
                construct.to_string()
            },
            text[span.end..line_end].trim_end().to_string(),
            0..0,
            Vec::new(),
        ),
    };
    // The replacements keep the lengths, so the formulas' ranges hold.
    let shown = shown.replace(['\n', '\t'], " ");
    let (before, kept_from) = shorten_start(&before.replace('\t', " "), CONTEXT_BEFORE);
    let (after, kept_to) = shorten_end(&after.replace('\t', " "), CONTEXT_AFTER);
    let start = before.len();
    // Where the kept text of the stripped line starts in `out`, before,
    // in and after the link.
    let lead = (start + kept_from).saturating_sub(mark.start);
    let math = formulas
        .into_iter()
        .filter_map(|f| {
            let r = f.out.clone();
            let out = if r.end <= mark.start && r.start >= kept_from {
                r.start - kept_from + lead..r.end - kept_from + lead
            } else if r.start >= mark.start && r.end <= mark.end {
                r.start - mark.start + start..r.end - mark.start + start
            } else if r.start >= mark.end && r.end <= mark.end + kept_to {
                let at = start + shown.len();
                r.start - mark.end + at..r.end - mark.end + at
            } else {
                // Cut by the shortening, or across the link's edge.
                return None;
            };
            Some(excerpt::Formula { out, ..f })
        })
        .collect();
    let out = format!("{before}{shown}{after}");
    Some(Snippet {
        highlight: start..start + shown.len(),
        text: out.into(),
        math,
    })
}

/// `before` without the line's leading Markdown block markers: quote
/// markers, heading hashes, list bullets or numbers, task boxes.
fn strip_block_markers(before: &str) -> String {
    let mut s = before.trim_start();
    loop {
        let start = s;
        if let Some(rest) = s.strip_prefix('>') {
            s = rest.trim_start();
        }
        let hashes = s.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && s[hashes..].starts_with(' ') {
            s = s[hashes..].trim_start();
        }
        for bullet in ["- ", "* ", "+ "] {
            if let Some(rest) = s.strip_prefix(bullet) {
                s = rest.trim_start();
            }
        }
        let digits = s.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 && (s[digits..].starts_with(". ") || s[digits..].starts_with(") ")) {
            s = s[digits + 1..].trim_start();
        }
        if s.len() >= 4 && s.starts_with('[') && s.as_bytes()[2] == b']' && s.is_char_boundary(3) {
            s = s[3..].trim_start();
        }
        if s == start {
            return s.to_string();
        }
    }
}

/// The last `max` characters of `s`, cut after a space, behind "…"; and
/// where in `s` the kept text starts.
fn shorten_start(s: &str, max: usize) -> (String, usize) {
    let count = s.chars().count();
    if count <= max {
        return (s.to_string(), 0);
    }
    let cut = s
        .char_indices()
        .nth(count - max)
        .map_or(s.len(), |(i, _)| i);
    let tail = &s[cut..];
    let tail = tail.find(' ').map_or(tail, |i| &tail[i + 1..]);
    (format!("…{tail}"), s.len() - tail.len())
}

/// The first `max` characters of `s`, cut before a space, then "…"; and
/// how many bytes of `s` are kept.
fn shorten_end(s: &str, max: usize) -> (String, usize) {
    if s.chars().count() <= max {
        return (s.to_string(), s.len());
    }
    let cut = s.char_indices().nth(max).map_or(s.len(), |(i, _)| i);
    let head = &s[..cut];
    let head = head.rfind(' ').map_or(head, |i| &head[..i]);
    (format!("{head}…"), head.len())
}

pub(super) use crate::search_panel::excerpt::link_label;

/// The link the Link action writes in `source` for the words `shown`
/// mentioning `target`: a wikilink where the profile has them (with the
/// words as its alias when they differ from the name), a Markdown link
/// with a path relative to `source` otherwise. `unique_stem`: no other
/// note shares the target's name, so a wikilink can use the name alone.
pub(super) fn link_for_mention(
    shown: &str,
    source: &VaultPath,
    target: &VaultPath,
    wikilinks: bool,
    unique_stem: bool,
) -> String {
    if wikilinks {
        let name = if unique_stem {
            file_stem(target).to_string()
        } else {
            let path = target.as_str();
            match target.extension() {
                Some(ext) if ext.eq_ignore_ascii_case("md") => {
                    path[..path.len() - ext.len() - 1].to_string()
                }
                _ => path.to_string(),
            }
        };
        if name == shown {
            format!("[[{name}]]")
        } else {
            format!("[[{name}|{shown}]]")
        }
    } else {
        format!("[{shown}]({})", relative_url(source, target))
    }
}

/// Whether no other note of the index has `target`'s file name, ignoring
/// case.
pub(super) fn stem_is_unique(snapshot: &ContentSnapshot, target: &VaultPath) -> bool {
    let key = fold_key(file_stem(target));
    snapshot
        .notes()
        .iter()
        .filter(|r| fold_key(file_stem(&r.path)) == key)
        .count()
        <= 1
}

/// The path of `to` relative to the folder of `from`, percent-encoded for
/// a Markdown link destination.
pub(super) fn relative_url(from: &VaultPath, to: &VaultPath) -> String {
    let from_dir: Vec<&str> = {
        let mut s: Vec<&str> = from.segments().collect();
        s.pop();
        s
    };
    let to_segments: Vec<&str> = to.segments().collect();
    let common = from_dir
        .iter()
        .zip(&to_segments)
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<String> = vec!["..".into(); from_dir.len() - common];
    parts.extend(to_segments[common..].iter().map(|s| encode_segment(s)));
    parts.join("/")
}

fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for c in segment.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3F"),
            '%' => out.push_str("%25"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(s: &str) -> VaultPath {
        VaultPath::new(s).expect("path")
    }

    fn obsidian() -> gv_syntax::inline::InlineOptions {
        excerpt::inline_options(gv_vault::ProfileId::Obsidian)
    }

    #[test]
    fn a_snippet_reads_without_markup() {
        let text =
            "The curve within $\\pm 4$ cm, see **[[Sensor calibration]]** and [[Hub]]. #tides\n";
        let start = text.find("[[Hub").expect("link");
        let s = snippet(text, start..start + 7, true, obsidian()).expect("snippet");
        assert_eq!(
            s.text.as_ref(),
            "The curve within \\pm 4 cm, see Sensor calibration and Hub. #tides"
        );
        assert_eq!(&s.text[s.highlight.clone()], "Hub");
        // A mention marks its words.
        let at = text.find("curve").expect("word");
        let s = snippet(text, at..at + 5, false, obsidian()).expect("snippet");
        assert_eq!(&s.text[s.highlight.clone()], "curve");
        assert!(!s.text.contains("**"), "{}", s.text);
    }

    #[test]
    fn a_snippet_keeps_its_formulas_through_the_shortening() {
        let tex = |s: &Snippet| -> Vec<(String, String)> {
            s.math
                .iter()
                .map(|f| (s.text[f.out.clone()].to_owned(), f.tex.clone()))
                .collect()
        };
        let text = "Area $\\pi r^2$ near [[Hub]] and $x$ costs $5 and $6.\n";
        let start = text.find("[[Hub").expect("link");
        let s = snippet(text, start..start + 7, true, obsidian()).expect("snippet");
        assert_eq!(
            s.text.as_ref(),
            "Area \\pi r^2 near Hub and x costs $5 and $6."
        );
        assert_eq!(&s.text[s.highlight.clone()], "Hub");
        assert_eq!(
            tex(&s),
            [
                ("\\pi r^2".to_owned(), "\\pi r^2".to_owned()),
                ("x".to_owned(), "x".to_owned()),
            ]
        );
        // Shortened on both sides: a formula cut off is dropped, the ones
        // kept move with the text.
        let words = "word ".repeat(40);
        let text = format!("$a$ {words}$b$ near [[Hub]] then $c$ {words}$d$ end\n");
        let start = text.find("[[Hub").expect("link");
        let s = snippet(&text, start..start + 7, true, obsidian()).expect("snippet");
        assert!(
            s.text.starts_with('…') && s.text.ends_with('…'),
            "{}",
            s.text
        );
        assert_eq!(&s.text[s.highlight.clone()], "Hub");
        assert_eq!(
            tex(&s),
            [
                ("b".to_owned(), "b".to_owned()),
                ("c".to_owned(), "c".to_owned()),
            ]
        );
    }

    #[test]
    fn links_read_as_their_text() {
        assert_eq!(link_label("[[Note]]"), "Note");
        assert_eq!(link_label("[[Note|shown]]"), "shown");
        assert_eq!(link_label("![[Note#Part]]"), "Note > Part");
        assert_eq!(link_label("[[#^block]]"), "block");
        assert_eq!(link_label("[text](a%20b.md)"), "text");
        assert_eq!(link_label("<https://x.org>"), "https://x.org");
    }

    #[test]
    fn a_snippet_marks_the_link_and_drops_block_markers() {
        let text = "# Title\n\n- [ ] Then read [[Following links|Following links]] to see.\n";
        let start = text.find("[[").expect("link");
        let end = text.find("]]").expect("end") + 2;
        let s = snippet(text, start..end, true, obsidian()).expect("snippet");
        assert_eq!(s.text.as_ref(), "Then read Following links to see.");
        assert_eq!(&s.text[s.highlight.clone()], "Following links");

        let quoted = "> 1. see [x](y.md)";
        let start = quoted.find('[').expect("link");
        let s = snippet(quoted, start..quoted.len(), true, obsidian()).expect("snippet");
        assert_eq!(s.text.as_ref(), "see x");
    }

    #[test]
    fn long_lines_are_shortened_around_the_link() {
        let before = "word ".repeat(40);
        let after = " tail".repeat(60);
        let text = format!("{before}[[Hub]]{after}");
        let start = before.len();
        let s = snippet(&text, start..start + 7, true, obsidian()).expect("snippet");
        assert!(s.text.starts_with('…'), "{}", s.text);
        assert!(s.text.ends_with('…'), "{}", s.text);
        assert_eq!(&s.text[s.highlight.clone()], "Hub");
        assert!(s.text.chars().count() <= CONTEXT_BEFORE + CONTEXT_AFTER + 5);
    }

    #[test]
    fn severities_follow_the_link_checker() {
        assert_eq!(severity(Status::Unresolved), Some(Severity::Error));
        assert_eq!(severity(Status::Blocked), Some(Severity::Error));
        assert_eq!(severity(Status::Ambiguous(2)), Some(Severity::Warning));
        assert_eq!(severity(Status::CaseFold), Some(Severity::Warning));
        assert_eq!(severity(Status::OutsideRoot), Some(Severity::Warning));
        assert_eq!(severity(Status::Exact), None);
        assert_eq!(severity(Status::Pending), None);
    }

    #[test]
    fn the_link_action_writes_wikilinks_or_relative_links() {
        let target = path("notes/Rust Book.md");
        let source = path("daily/2026/today.md");
        assert_eq!(
            link_for_mention("Rust Book", &source, &target, true, true),
            "[[Rust Book]]"
        );
        assert_eq!(
            link_for_mention("rust book", &source, &target, true, true),
            "[[Rust Book|rust book]]"
        );
        assert_eq!(
            link_for_mention("Rust Book", &source, &target, true, false),
            "[[notes/Rust Book|Rust Book]]"
        );
        assert_eq!(
            link_for_mention("Rust Book", &source, &target, false, true),
            "[Rust Book](../../notes/Rust%20Book.md)"
        );
        assert_eq!(relative_url(&path("a.md"), &path("b.md")), "b.md");
    }
}
