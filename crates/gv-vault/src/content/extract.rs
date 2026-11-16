// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! From a note's text to its [`NoteData`].
//!
//! The text is parsed with the profile's options by the same parser the
//! views use (`gv-syntax`), and the document model (`gv-model`) supplies
//! headings, Markdown links and images, and the front matter block.
//!
//! The Obsidian constructs the model does not record yet (wikilinks,
//! embeds, inline tags, block ids, `%%` comments) are found by a scan of
//! the prose leaves (paragraphs, headings, table rows) that skips code
//! spans, so code blocks, code spans, math and HTML blocks never yield
//! links or tags. The profile's switches decide which constructs exist.
//! When the model gains these records, this scan goes away and the index
//! format version is bumped ([`super::store::EXTRACT_VERSION`]).

use std::sync::Arc;

use gv_model::{DocModel, Projection};
use gv_profile::Switches;
use gv_syntax::{Container, ContainerKind, FrontMatterKind, Leaf, LeafKind, ParseOptions, Visitor};

use super::frontmatter;
use super::record::{
    IndexedBlockId, IndexedHeading, IndexedLink, IndexedTag, IndexedTask, NoteData, Span,
    TitleSource,
};
use crate::link::LinkKind;
use crate::profile::ProfileId;

/// Extracts the index record of `text` under `profile`.
#[must_use]
pub fn extract(text: &str, profile: ProfileId) -> NoteData {
    let options = ParseOptions::builtin(profile);
    let switches = &gv_profile::definition(profile).switches;
    let tree = gv_syntax::parse_with(text, options);
    let model = DocModel::build(&tree, text);
    let lines = LineIndex::new(text);

    let mut scan = Scan {
        text,
        switches,
        lines: &lines,
        data: NoteData::default(),
        open_tasks: Vec::new(),
        pending_task_text: None,
        prev_leaf: None,
    };
    gv_syntax::walk(&tree, &mut scan);
    let mut data = scan.data;

    // Headings.
    for h in model.headings() {
        let text: Arc<str> = h.projection(Projection::Text).into();
        let source = if *h.source == *text {
            text.clone()
        } else {
            h.source.clone()
        };
        data.headings.push(IndexedHeading {
            level: h.level,
            text,
            source,
            span: Span::of(h.range.clone()),
            line: h.line,
        });
    }

    // Markdown links and images.
    for l in model.links() {
        data.links.push(IndexedLink {
            kind: crate::model::link_kind(l),
            raw: l.destination.clone(),
            span: Span::of(l.range.clone()),
            line: lines.line_of(l.range.start),
            front_matter: false,
        });
    }

    // Front matter.
    if let Some(fm) = model.front_matter() {
        data.front_matter = Some(Span::of(fm.range.clone()));
        data.skip.push(Span::of(fm.range.clone()));
        let content = &text[fm.content_range.clone()];
        let facts = match fm.kind {
            FrontMatterKind::Yaml => frontmatter::parse_yaml(content, fm.content_range.start),
            FrontMatterKind::Toml => frontmatter::parse_toml(content),
        };
        let fm_line = lines.line_of(fm.range.start);
        let mut tags: Vec<IndexedTag> = facts
            .tags
            .into_iter()
            .map(|name| IndexedTag {
                name,
                span: None,
                line: fm_line,
            })
            .collect();
        tags.append(&mut data.tags);
        data.tags = tags;
        data.aliases = facts.aliases;
        data.cssclasses = facts.cssclasses;
        data.properties = facts.properties;
        if switches.wikilinks {
            for (raw, span) in facts.links {
                let embed = text.as_bytes().get(span.start as usize) == Some(&b'!');
                data.links.push(IndexedLink {
                    kind: if embed {
                        LinkKind::Embed
                    } else {
                        LinkKind::Wikilink
                    },
                    raw,
                    span,
                    line: lines.line_of(span.start as usize),
                    front_matter: true,
                });
            }
        }
        if let Some(title) = facts.title {
            data.title = Some(title);
            data.title_source = TitleSource::FrontMatter;
        }
    }
    if data.title.is_none()
        && let Some(h1) = data.headings.iter().find(|h| h.level == 1)
    {
        data.title = Some(h1.text.clone());
        data.title_source = TitleSource::Heading;
    }

    data.links
        .sort_by_key(|l| (!l.front_matter, l.span.start, l.span.end));
    data.skip.sort();
    data.tasks.sort_by_key(|t| t.span.start);
    data.shrink();
    data
}

/// Line starts, for offsets to lines.
pub(crate) struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    pub(crate) fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            text.bytes()
                .enumerate()
                .filter(|&(_, b)| b == b'\n')
                .map(|(i, _)| i + 1),
        );
        Self { starts }
    }

    /// 0-based line of `offset`.
    pub(crate) fn line_of(&self, offset: usize) -> u32 {
        let line = self
            .starts
            .partition_point(|&s| s <= offset)
            .saturating_sub(1);
        u32::try_from(line).unwrap_or(u32::MAX)
    }
}

struct OpenTask {
    status: char,
    start: usize,
    line: u32,
    text: Option<Arc<str>>,
}

struct Scan<'a> {
    text: &'a str,
    switches: &'a Switches,
    lines: &'a LineIndex,
    data: NoteData,
    open_tasks: Vec<OpenTask>,
    /// The innermost open task still waits for its text.
    pending_task_text: Option<usize>,
    /// The previous prose leaf: a block id alone on a line names it.
    prev_leaf: Option<(Span, u32)>,
}

impl Visitor for Scan<'_> {
    fn enter(&mut self, c: &Container) {
        if c.is_task()
            && let Some(marker) = &c.marker
        {
            let status = self.text[marker.clone()].chars().nth(1).unwrap_or(' ');
            self.open_tasks.push(OpenTask {
                status,
                start: c.range.start,
                line: u32::try_from(c.line).unwrap_or(u32::MAX),
                text: None,
            });
            self.pending_task_text = Some(self.open_tasks.len() - 1);
        } else if c.kind == ContainerKind::FootnoteDefinition
            && let Some(label) = c.footnote_label()
        {
            self.data.footnotes.push(self.text[label].into());
        }
    }

    fn exit(&mut self, c: &Container) {
        if c.is_task()
            && let Some(task) = self.open_tasks.pop()
        {
            self.data.tasks.push(IndexedTask {
                status: task.status,
                text: task.text.unwrap_or_else(|| Arc::from("")),
                span: Span::of(task.start..c.range.end),
                line: task.line,
            });
            if self
                .pending_task_text
                .is_some_and(|i| i >= self.open_tasks.len())
            {
                self.pending_task_text = None;
            }
        }
    }

    fn leaf(&mut self, leaf: &Leaf<'_>) {
        if let Some(i) = self.pending_task_text.take()
            && let Some(first) = leaf.lines.iter().find(|l| l.has_content)
        {
            let line = self.text[first.content.clone()].trim();
            let line = strip_checkbox(line);
            self.open_tasks[i].text = Some(line.into());
        }
        match leaf.kind {
            LeafKind::Paragraph | LeafKind::Heading { .. } | LeafKind::Table => {
                self.prose(leaf);
            }
            LeafKind::FencedCode
            | LeafKind::IndentedCode
            | LeafKind::Html { .. }
            | LeafKind::MathBlock => {
                self.data.skip.push(Span::of(leaf.range.clone()));
            }
            LeafKind::ThematicBreak | LeafKind::LinkRefDef | LeafKind::FrontMatter { .. } => {}
        }
    }
}

fn strip_checkbox(line: &str) -> &str {
    let b = line.as_bytes();
    if b.len() >= 3
        && b[0] == b'['
        && let Some(c) = line[1..].chars().next()
        && b.get(1 + c.len_utf8()) == Some(&b']')
    {
        return line[1 + c.len_utf8() + 1..].trim_start();
    }
    line
}

impl Scan<'_> {
    /// Wikilinks, embeds, tags, comments and block ids in a prose leaf.
    fn prose(&mut self, leaf: &Leaf<'_>) {
        let segments = leaf.segments();
        if segments.is_empty() {
            return;
        }
        let s = self.switches;
        let table = leaf.kind == LeafKind::Table;
        let mut excluded = code_spans(self.text, &segments);
        if s.comments {
            excluded.extend(comments(self.text, &segments, &excluded));
            excluded.sort();
        }
        for span in &excluded {
            self.data.skip.push(*span);
        }
        let skip_to = |at: usize| {
            excluded
                .iter()
                .find(|e| e.contains(at))
                .map(|e| e.end as usize)
        };

        for seg in &segments {
            let bytes = self.text.as_bytes();
            let mut i = seg.start;
            while i < seg.end {
                if let Some(end) = skip_to(i) {
                    i = end.max(i + 1);
                    continue;
                }
                let b = bytes[i];
                if b == b'\\' {
                    i += 2;
                    continue;
                }
                if b == b'['
                    && s.wikilinks
                    && bytes.get(i + 1) == Some(&b'[')
                    && let Some(end) = self.wikilink(i, seg.end, table)
                {
                    i = end;
                    continue;
                }
                if b == b'#'
                    && s.tags
                    && (i == seg.start || is_space_before(self.text, i))
                    && let Some(end) = self.tag(i, seg.end)
                {
                    i = end;
                    continue;
                }
                i += 1;
            }
        }

        let leaf_span = Span::of(leaf.range.clone());
        let leaf_line = self.lines.line_of(leaf.range.start);
        if s.block_ids
            && let Some(last) = segments.last()
        {
            let line = &self.text[last.clone()];
            if let Some((id, alone)) = block_id(line) {
                let (span, at) = if alone && segments.len() == 1 {
                    self.prev_leaf.unwrap_or((leaf_span, leaf_line))
                } else {
                    (leaf_span, leaf_line)
                };
                self.data.block_ids.push(IndexedBlockId {
                    id: id.into(),
                    span,
                    line: at,
                });
            }
        }
        self.prev_leaf = Some((leaf_span, leaf_line));
    }

    /// A `[[…]]` (or `![[…]]`) at `open`; returns where it ends.
    fn wikilink(&mut self, open: usize, seg_end: usize, table: bool) -> Option<usize> {
        let text = &self.text[open + 2..seg_end];
        let close = text.find("]]")?;
        let inner = &text[..close];
        if inner.trim().is_empty() || inner.contains("[[") {
            return None;
        }
        let embed = self.switches.embeds && open > 0 && self.text.as_bytes()[open - 1] == b'!';
        let start = if embed { open - 1 } else { open };
        let end = open + 2 + close + 2;
        let raw: Arc<str> = if table && inner.contains("\\|") {
            inner.replace("\\|", "|").into()
        } else {
            inner.into()
        };
        self.data.links.push(IndexedLink {
            kind: if embed {
                LinkKind::Embed
            } else {
                LinkKind::Wikilink
            },
            raw,
            span: Span::of(start..end),
            line: self.lines.line_of(start),
            front_matter: false,
        });
        Some(end)
    }

    /// A `#tag` at `hash`; returns where it ends.
    fn tag(&mut self, hash: usize, seg_end: usize) -> Option<usize> {
        let rest = &self.text[hash + 1..seg_end];
        let mut len = 0;
        for c in rest.chars() {
            if is_tag_char(c) {
                len += c.len_utf8();
            } else {
                break;
            }
        }
        let name = &rest[..len];
        // At least one character that is not a digit.
        if name.is_empty() || name.chars().all(|c| c.is_ascii_digit() || c == '/') {
            return None;
        }
        let end = hash + 1 + len;
        self.data.tags.push(IndexedTag {
            name: name.into(),
            span: Some(Span::of(hash..end)),
            line: self.lines.line_of(hash),
        });
        Some(end)
    }
}

fn is_space_before(text: &str, at: usize) -> bool {
    text[..at]
        .chars()
        .next_back()
        .is_some_and(char::is_whitespace)
}

/// Obsidian tag characters: letters, digits, `_`, `-`, `/` and any
/// non-ASCII character that is not whitespace or punctuation.
fn is_tag_char(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/')
    } else {
        !c.is_whitespace() && !is_unicode_punctuation(c)
    }
}

fn is_unicode_punctuation(c: char) -> bool {
    matches!(
        c,
        '\u{2000}'..='\u{206F}' | '\u{3000}'..='\u{303F}' | '\u{FF01}'..='\u{FF0F}' | '\u{00A1}'..='\u{00BF}'
    )
}

/// A block id at the end of `line`: `text ^id` or `^id` alone (which then
/// names the previous block).
fn block_id(line: &str) -> Option<(&str, bool)> {
    let line = line.trim_end();
    let caret = line.rfind('^')?;
    let id = &line[caret + 1..];
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    let before = &line[..caret];
    if before.trim().is_empty() {
        return Some((id, true));
    }
    before
        .chars()
        .next_back()
        .is_some_and(char::is_whitespace)
        .then_some((id, false))
}

/// Code spans in a leaf whose content lines are `segments`: backtick runs
/// paired by length (CommonMark), across lines; a backslash before the
/// opening run escapes it.
fn code_spans(text: &str, segments: &[std::ops::Range<usize>]) -> Vec<Span> {
    let bytes = text.as_bytes();
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for seg in segments {
        let mut i = seg.start;
        while i < seg.end {
            if bytes[i] == b'`' {
                let start = i;
                while i < seg.end && bytes[i] == b'`' {
                    i += 1;
                }
                let escaped = start > seg.start && bytes[start - 1] == b'\\';
                if !escaped {
                    runs.push((start, i - start));
                }
                continue;
            }
            i += 1;
        }
    }
    let mut spans = Vec::new();
    let mut k = 0;
    while k < runs.len() {
        let (start, len) = runs[k];
        match runs[k + 1..].iter().position(|&(_, l)| l == len) {
            Some(j) => {
                let (close, _) = runs[k + 1 + j];
                spans.push(Span::of(start..close + len));
                k += j + 2;
            }
            None => k += 1,
        }
    }
    spans
}

/// `%%…%%` comments in a leaf, outside code spans.
fn comments(text: &str, segments: &[std::ops::Range<usize>], code: &[Span]) -> Vec<Span> {
    let mut marks = Vec::new();
    for seg in segments {
        let mut from = seg.start;
        while let Some(i) = text[from..seg.end].find("%%").map(|i| i + from) {
            if !code.iter().any(|c| c.contains(i)) {
                marks.push(i);
            }
            from = i + 2;
        }
    }
    marks
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[open, close]| Span::of(*open..*close + 2))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(text: &str) -> NoteData {
        extract(text, ProfileId::Obsidian)
    }

    #[test]
    fn wikilinks_embeds_and_markdown_links() {
        let d = obs("See [[Alpha]] and ![[img.png|200]] and [b](Beta.md).\n");
        let kinds: Vec<_> = d.links.iter().map(|l| (l.kind, &*l.raw)).collect();
        assert_eq!(
            kinds,
            vec![
                (LinkKind::Wikilink, "Alpha"),
                (LinkKind::Embed, "img.png|200"),
                (LinkKind::Inline, "Beta.md"),
            ]
        );
        assert!(d.links[1].is_embed());
    }

    #[test]
    fn code_never_yields_links_or_tags() {
        let d = obs("`[[No]] #no`\n\n```\n[[No]] #no\n```\n\n    [[No]]\n\n[[Yes]] #yes\n");
        assert_eq!(d.links.len(), 1);
        assert_eq!(&*d.links[0].raw, "Yes");
        assert_eq!(d.tags.len(), 1);
        assert_eq!(&*d.tags[0].name, "yes");
    }

    #[test]
    fn tags_need_a_space_before_and_a_letter() {
        let d = obs("#a a#b #123 #x/y-z #ünï (#paren) #end.\n");
        let names: Vec<&str> = d.tags.iter().map(|t| &*t.name).collect();
        assert_eq!(names, vec!["a", "x/y-z", "ünï", "end"]);
    }

    #[test]
    fn headings_title_and_block_ids() {
        let d = obs("# Title [[In heading]]\n\nPara one ^abc\n\n- item\n\n^list-id\n");
        assert_eq!(d.title.as_deref(), Some("Title [[In heading]]"));
        assert_eq!(d.title_source, TitleSource::Heading);
        let ids: Vec<&str> = d.block_ids.iter().map(|b| &*b.id).collect();
        assert_eq!(ids, vec!["abc", "list-id"]);
        assert_eq!(d.links.len(), 1);
    }

    #[test]
    fn front_matter_title_tags_and_links() {
        let text = "---\ntitle: FM\ntags: [a]\nup: \"[[Parent]]\"\n---\nBody #b\n";
        let d = obs(text);
        assert_eq!(d.title.as_deref(), Some("FM"));
        let names: Vec<&str> = d.tags.iter().map(|t| &*t.name).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert!(d.links[0].front_matter);
        assert_eq!(&text[d.links[0].span.range()], "[[Parent]]");
        assert!(d.front_matter.is_some());
    }

    #[test]
    fn tasks_with_any_status() {
        let d = obs("- [ ] todo\n- [x] done\n- [/] half\n- plain\n");
        let t: Vec<(char, &str)> = d.tasks.iter().map(|t| (t.status, &*t.text)).collect();
        assert_eq!(t, vec![(' ', "todo"), ('x', "done"), ('/', "half")]);
    }

    #[test]
    fn comments_hide_links() {
        let d = obs("a %%[[Hidden]]%% [[Shown]]\n");
        assert_eq!(d.links.len(), 1);
        assert_eq!(&*d.links[0].raw, "Shown");
    }

    #[test]
    fn commonmark_has_no_wikilinks() {
        let d = extract("[[A]] #t\n", ProfileId::CommonMark);
        assert!(d.links.is_empty() && d.tags.is_empty());
    }

    #[test]
    fn table_pipes_are_unescaped() {
        let d = obs("| a | b |\n|---|---|\n| [[X\\|shown]] | c |\n");
        assert_eq!(d.links.len(), 1);
        assert_eq!(&*d.links[0].raw, "X|shown");
    }
}
