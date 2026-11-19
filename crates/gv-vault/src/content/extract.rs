// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! From a note's text to its [`NoteData`].
//!
//! The text is parsed with the profile's options by the same parser the
//! views use (`gv-syntax`), and the document model (`gv-model`) supplies
//! headings, Markdown links and images, and the front matter block.
//!
//! The Obsidian constructs (wikilinks, embeds, inline tags, block ids,
//! `%%` comments) come from the parser's inline trees of the prose leaves
//! (paragraphs, headings, callout titles, table cells; through the tree's
//! inline cache, which the model filled), so code, math and HTML never
//! yield links or tags. The profile's switches decide which constructs
//! exist.
//!
//! The `href` of `<a>` tags in HTML blocks and inline HTML go to
//! [`NoteData::html_links`], apart from the links (see there why).

use std::sync::Arc;

use gv_model::{DocModel, Projection};
use gv_syntax::inline::{InlineKind, InlineTree};
use gv_syntax::{
    Container, ContainerKind, FrontMatterKind, Leaf, LeafKind, ParseOptions, SyntaxTree, Visitor,
};

use super::frontmatter;
use super::html::a_hrefs;
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
        tree: &tree,
        options,
        lines: &lines,
        data: NoteData::default(),
        open_tasks: Vec::new(),
        pending_task_text: None,
        prev_block: None,
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

    // Markdown links and images (wikilinks and embeds come from the scan,
    // which also sees table cells).
    for l in model.links().iter().filter(|l| !l.kind.is_wiki()) {
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
    data.html_links.sort_by_key(|l| l.span.start);
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
    tree: &'a SyntaxTree,
    options: ParseOptions,
    lines: &'a LineIndex,
    data: NoteData,
    open_tasks: Vec<OpenTask>,
    /// The innermost open task still waits for its text.
    pending_task_text: Option<usize>,
    /// The previous block (a leaf, or a container that ended): a block id
    /// alone in its paragraph names it.
    prev_block: Option<(Span, u32)>,
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
        if c.kind != ContainerKind::Item {
            self.prev_block = Some((
                Span::of(c.range.clone()),
                u32::try_from(c.line).unwrap_or(u32::MAX),
            ));
        }
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
            LeafKind::Paragraph | LeafKind::Heading { .. } | LeafKind::CalloutTitle => {
                self.prose(leaf);
            }
            LeafKind::Table => self.table(leaf),
            LeafKind::Html { .. } => {
                self.data.skip.push(Span::of(leaf.range.clone()));
                self.html(leaf.range.clone());
            }
            LeafKind::FencedCode
            | LeafKind::IndentedCode
            | LeafKind::MathBlock
            | LeafKind::Comment => {
                self.data.skip.push(Span::of(leaf.range.clone()));
            }
            LeafKind::ThematicBreak | LeafKind::LinkRefDef | LeafKind::FrontMatter { .. } => {}
        }
        if leaf.kind != LeafKind::LinkRefDef {
            self.prev_block = Some((
                Span::of(leaf.range.clone()),
                self.lines.line_of(leaf.range.start),
            ));
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
    /// Wikilinks, embeds, tags, comments, code spans and the block id of a
    /// paragraph, heading or callout title.
    fn prose(&mut self, leaf: &Leaf<'_>) {
        // Nothing to find without one of these bytes: no inline parse.
        let interesting = leaf.lines.iter().filter(|l| l.has_content).any(|l| {
            self.text.as_bytes()[l.content.clone()]
                .iter()
                .any(|b| matches!(b, b'[' | b'#' | b'^' | b'`' | b'%' | b'<'))
        });
        if !interesting {
            return;
        }
        let tree = self.tree.inlines(leaf, self.text);
        self.inline_tree(&tree);
        if let Some((id, _)) = tree.block_id() {
            let own_line = tree.nodes().all(|n| {
                !n.kind().is_token()
                    || matches!(
                        n.kind(),
                        InlineKind::Whitespace
                            | InlineKind::SoftBreak
                            | InlineKind::Marker
                            | InlineKind::BlockIdText
                    )
            });
            let here = (
                Span::of(leaf.range.clone()),
                self.lines.line_of(leaf.range.start),
            );
            let (span, line) = if own_line {
                self.prev_block.unwrap_or(here)
            } else {
                here
            };
            self.data.block_ids.push(IndexedBlockId {
                id: id.into(),
                span,
                line,
            });
        }
    }

    /// Each cell of a table, parsed with the profile's inline switches.
    fn table(&mut self, leaf: &Leaf<'_>) {
        let refs = self.tree.ref_map(self.text);
        let options = self.options.inline_for(LeafKind::Table);
        for line in leaf.lines.iter().filter(|l| l.has_content && !l.marker) {
            let row = &self.text.as_bytes()[line.content.clone()];
            let Some(cells) = gv_syntax::table::row_cells(row) else {
                continue;
            };
            for cell in cells {
                let cell = line.content.start + cell.start..line.content.start + cell.end;
                let tree = gv_syntax::inline::parse_table_cell(self.text, cell, &*refs, options);
                self.inline_tree(&tree);
                for l in tree.links() {
                    let kind = if l.is_image {
                        LinkKind::Image
                    } else if l.kind.is_reference() {
                        LinkKind::Reference
                    } else if matches!(
                        l.kind,
                        gv_syntax::inline::LinkKind::UriAutolink
                            | gv_syntax::inline::LinkKind::EmailAutolink
                    ) {
                        if l.text_range.start == l.range.start {
                            // A GFM extended autolink: no link here, as in
                            // the model.
                            continue;
                        }
                        LinkKind::Autolink
                    } else {
                        LinkKind::Inline
                    };
                    let raw: Arc<str> =
                        if matches!(l.kind, gv_syntax::inline::LinkKind::EmailAutolink) {
                            format!("mailto:{}", l.destination).into()
                        } else {
                            l.destination.into()
                        };
                    self.data.links.push(IndexedLink {
                        kind,
                        raw,
                        span: Span::of(l.range.clone()),
                        line: self.lines.line_of(l.range.start),
                        front_matter: false,
                    });
                }
            }
        }
    }

    /// The wikilinks, embeds, tags, code spans and comments of one inline
    /// tree.
    fn inline_tree(&mut self, tree: &InlineTree) {
        for c in tree.code_spans() {
            self.data.skip.push(Span::of(c.range));
        }
        for c in tree.comments() {
            self.data.skip.push(Span::of(c));
        }
        for w in tree.wikilinks() {
            self.data.links.push(IndexedLink {
                kind: if w.embed {
                    LinkKind::Embed
                } else {
                    LinkKind::Wikilink
                },
                raw: w.inner.into(),
                span: Span::of(w.range.clone()),
                line: self.lines.line_of(w.range.start),
                front_matter: false,
            });
        }
        for (name, range) in tree.tags() {
            self.data.tags.push(IndexedTag {
                name: name.into(),
                line: self.lines.line_of(range.start),
                span: Some(Span::of(range)),
            });
        }
        for node in tree.nodes() {
            if node.kind() == InlineKind::RawHtml {
                self.html(node.range());
            }
        }
    }

    /// The `<a href>`s of the raw HTML at `range`.
    fn html(&mut self, range: std::ops::Range<usize>) {
        let Some(html) = self.text.get(range.clone()) else {
            return;
        };
        if !html
            .as_bytes()
            .windows(4)
            .any(|w| w.eq_ignore_ascii_case(b"href"))
        {
            return;
        }
        for href in a_hrefs(html, range.start) {
            self.data.html_links.push(IndexedLink {
                kind: LinkKind::HtmlHref,
                raw: href.value.into(),
                line: self.lines.line_of(href.range.start),
                span: Span::of(href.range),
                front_matter: false,
            });
        }
    }
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
        // The heading's text shows the wikilink's text.
        assert_eq!(d.title.as_deref(), Some("Title In heading"));
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
    fn callouts_block_comments_and_lists() {
        let text = "> [!note] Title [[T]] #t1\n> body [[B]]\n\n%%\n[[Hidden]] #no\n%%\n\n- a\n- b\n\n^list\n";
        let d = obs(text);
        let raws: Vec<&str> = d.links.iter().map(|l| &*l.raw).collect();
        assert_eq!(raws, ["T", "B"]);
        let names: Vec<&str> = d.tags.iter().map(|t| &*t.name).collect();
        assert_eq!(names, ["t1"]);
        // The block id alone names the list before it.
        assert_eq!(d.block_ids.len(), 1);
        assert_eq!(d.block_ids[0].line, 7);
        assert_eq!(text[d.block_ids[0].span.range()].trim_end(), "- a\n- b");
        assert!(
            d.skip
                .iter()
                .any(|s| text[s.range()].starts_with("%%\n[[Hidden]]"))
        );
    }

    #[test]
    fn inline_math_and_links_hide_their_text() {
        let d = obs("$[[NoMath]]$ [x](y.md \"[[NoTitle]]\") [[Yes]]\n");
        let raws: Vec<&str> = d.links.iter().map(|l| &*l.raw).collect();
        assert_eq!(raws, ["y.md", "Yes"]);
    }

    #[test]
    fn html_hrefs_are_kept_apart_from_links() {
        let text = "<div>\n<a href=\"Block.md#Part\">b</a> <a href=\"https://e.com\">e</a>\n</div>\n\n\
                    Inline <a href='My%20Note.md'>m</a> and [md](x.md).\n\n\
                    | <a href=\"Cell.md\">c</a> |\n|---|\n\n\
                    `<a href=\"Code.md\">`\n";
        let d = obs(text);
        let raws: Vec<&str> = d.links.iter().map(|l| &*l.raw).collect();
        assert_eq!(raws, ["x.md"]);
        let html: Vec<(&str, &str, u32)> = d
            .html_links
            .iter()
            .map(|l| (&*l.raw, &text[l.span.range()], l.line))
            .collect();
        assert_eq!(
            html,
            [
                ("Block.md#Part", "Block.md#Part", 1),
                ("My%20Note.md", "My%20Note.md", 4),
                ("Cell.md", "Cell.md", 6),
            ]
        );
        assert!(d.html_links.iter().all(|l| l.kind == LinkKind::HtmlHref));
    }

    #[test]
    fn table_pipes_are_unescaped() {
        let d = obs("| a | b |\n|---|---|\n| [[X\\|shown]] | c |\n");
        assert_eq!(d.links.len(), 1);
        assert_eq!(&*d.links[0].raw, "X|shown");
    }
}
