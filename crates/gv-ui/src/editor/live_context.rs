// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The note's context for Live Preview block widgets: what a block's
//! rendering depends on outside its own source, so that it looks as in
//! the Reading view of the whole note.
//!
//! A block is rendered by a Reading view of its source
//! ([`crate::reader::Reader::open_block`]) with text around it that is
//! built but not shown:
//!
//! - **before it**, the footnotes numbered before the block (a reference
//!   to each, in number order: the block's footnotes get the numbers they
//!   have in the whole note), and the formulas before it that define math
//!   macros (MathJax's document-wide scope);
//! - **after it**, the link reference definitions and the footnote
//!   definitions the block refers to (a footnote defined inside a list or
//!   a quote, or only numbered before the block, gets a stand-in
//!   definition).
//!
//! Only the context a block uses goes into its text, and the text is part
//! of the block's key: editing a definition draws again the blocks that
//! refer to it, and only those.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use gv_render_tree::FootnoteIndex;
use gv_syntax::inline::{RefLookup as _, normalize_label};
use gv_syntax::{Container, ContainerKind, Leaf, LeafKind, RefMap, SyntaxTree, Visitor, walk};
use gv_text::{TextSnapshot, TextSource as _};

use super::live_blocks::BlockWidget;

/// The commands that define math macros (see `gv_math::macros`).
const MACRO_COMMANDS: [&[u8]; 5] = [
    b"\\newcommand",
    b"\\renewcommand",
    b"\\providecommand",
    b"\\def",
    b"\\gdef",
];

/// Notes at most this long get their context (footnote numbers included)
/// on the UI thread when the background pass has not given it yet.
pub(crate) const SYNC_CONTEXT_BYTES: usize = 64 * 1024;

/// What a block's text has around it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub(crate) struct BlockContext {
    /// Built before the block's source (ends with a blank line).
    pub prefix: String,
    /// Built after it (starts with a blank line).
    pub suffix: String,
}

impl BlockContext {
    /// The text to build: the prefix, `source` and the suffix; and where
    /// `source` is in it.
    pub fn text(&self, source: &str) -> (String, Range<usize>) {
        let mut text = String::with_capacity(self.prefix.len() + source.len() + self.suffix.len());
        text.push_str(&self.prefix);
        text.push_str(source);
        text.push_str(&self.suffix);
        (text, self.prefix.len()..self.prefix.len() + source.len())
    }
}

/// The note-level context of one text revision.
#[derive(Debug, Default)]
pub(crate) struct NoteContext {
    refs: Option<Arc<RefMap>>,
    /// Footnote definitions at the top level: normalized label to their
    /// source.
    footnotes: HashMap<String, String>,
    /// The footnotes by number: where each is first referenced and its
    /// label; `None` when not worked out (a long note, before the
    /// background pass).
    firsts: Option<Vec<(usize, String)>>,
    /// The formulas that define macros, in document order: where each
    /// starts and its TeX.
    macros: Vec<(usize, String)>,
}

/// Collects footnote definitions and macro formulas.
struct Collect<'a> {
    tree: &'a SyntaxTree,
    text: &'a TextSnapshot,
    depth: usize,
    footnotes: bool,
    /// Offsets of macro commands in the text, sorted.
    commands: &'a [usize],
    out: NoteContext,
    buf: Vec<u8>,
    /// The top-level footnote definition being walked: its label and the
    /// source seen so far.
    note: Option<(String, Range<usize>)>,
}

impl Collect<'_> {
    fn has_command(&self, range: &Range<usize>) -> bool {
        let i = self.commands.partition_point(|&o| o < range.start);
        self.commands.get(i).is_some_and(|&o| o < range.end)
    }
}

impl Visitor for Collect<'_> {
    fn enter(&mut self, container: &Container) {
        if self.footnotes
            && self.depth == 0
            && container.kind == ContainerKind::FootnoteDefinition
            && let Some(label) = container.footnote_label()
        {
            self.buf.clear();
            self.text.copy_range(label, &mut self.buf);
            let key = normalize_label(String::from_utf8_lossy(&self.buf).trim());
            let start = container
                .marker
                .as_ref()
                .map_or(container.range.start, |m| m.start);
            let end = container.marker.as_ref().map_or(start, |m| m.end);
            self.note = Some((key, start..end));
        }
        self.depth += 1;
    }

    fn exit(&mut self, _: &Container) {
        self.depth = self.depth.saturating_sub(1);
        if self.depth == 0
            && let Some((key, range)) = self.note.take()
        {
            let source = self.text.text_for_range(range).unwrap_or_default();
            self.out
                .footnotes
                .entry(key)
                .or_insert_with(|| source.trim_end().to_owned());
        }
    }

    fn leaf(&mut self, leaf: &Leaf<'_>) {
        if let Some((_, range)) = self.note.as_mut() {
            range.end = range.end.max(leaf.range.end);
        }
        if !self.has_command(&leaf.range) {
            return;
        }
        match leaf.kind {
            LeafKind::MathBlock => {
                let mut tex = String::new();
                for (i, line) in leaf.lines.iter().enumerate() {
                    if i > 0 {
                        tex.push('\n');
                    }
                    tex.push_str(
                        &self
                            .text
                            .text_for_range(line.content.clone())
                            .unwrap_or_default(),
                    );
                }
                self.out.macros.push((leaf.range.start, tex));
            }
            LeafKind::Paragraph | LeafKind::Heading { .. } => {
                let inlines = self.tree.inlines(leaf, self.text);
                for node in inlines.nodes() {
                    if let Some(tex) = node.math()
                        && MACRO_COMMANDS
                            .iter()
                            .any(|c| tex.as_bytes().windows(c.len()).any(|w| w == *c))
                    {
                        self.out.macros.push((node.range().start, tex.to_owned()));
                    }
                }
            }
            _ => {}
        }
    }
}

/// The offsets of the macro commands in `text`.
fn macro_commands(text: &TextSnapshot) -> Vec<usize> {
    let mut bytes = Vec::with_capacity(text.len());
    text.copy_range(0..text.len(), &mut bytes);
    let mut out = Vec::new();
    for (i, _) in bytes.iter().enumerate().filter(|(_, b)| **b == b'\\') {
        let rest = &bytes[i..];
        if MACRO_COMMANDS.iter().any(|c| {
            rest.starts_with(c) && rest.get(c.len()).is_none_or(|b| !b.is_ascii_alphabetic())
        }) {
            out.push(i);
        }
    }
    out
}

impl NoteContext {
    /// The context of `text` (parsed as `tree`). The footnote numbers
    /// need the inlines of every paragraph: they are left out with
    /// `numbers` false.
    pub fn compute(tree: &SyntaxTree, text: &TextSnapshot, numbers: bool) -> Self {
        let refs = tree.ref_map(text);
        let footnotes = refs.footnote_count() > 0;
        let commands = macro_commands(text);
        let mut out = if footnotes || !commands.is_empty() {
            let mut collect = Collect {
                tree,
                text,
                depth: 0,
                footnotes,
                commands: &commands,
                out: NoteContext::default(),
                buf: Vec::new(),
                note: None,
            };
            walk(tree, &mut collect);
            collect.out
        } else {
            NoteContext::default()
        };
        out.firsts = numbers.then(|| FootnoteIndex::first_references(tree, text, &refs));
        out.refs = (!refs.is_empty() || footnotes).then_some(refs);
        out
    }

    /// The context of a block of kind `kind` at `range` whose source is
    /// `source`; `None` when it needs none.
    pub fn for_block(
        &self,
        kind: BlockWidget,
        range: &Range<usize>,
        source: &str,
    ) -> Option<BlockContext> {
        // Code, diagrams, rules and front matter have no references (and
        // front matter must start the text); embeds show another note.
        if !matches!(
            kind,
            BlockWidget::Table | BlockWidget::Callout | BlockWidget::Math | BlockWidget::Image
        ) {
            return None;
        }
        let mut cx = BlockContext::default();
        // Footnotes this block refers to, and the numbers before it.
        let notes = if kind == BlockWidget::Math {
            Vec::new()
        } else {
            self.footnotes_in(source)
        };
        let inline_notes = kind != BlockWidget::Math && source.contains("^[");
        let mut stubs: Vec<&str> = Vec::new();
        let refs = self.refs.as_deref();
        if (!notes.is_empty() || inline_notes)
            && let Some(firsts) = &self.firsts
        {
            let before: Vec<&(usize, String)> = firsts
                .iter()
                .take_while(|(at, _)| *at < range.start)
                .collect();
            if !before.is_empty() {
                let mut line = String::new();
                for (_, label) in before {
                    if label.starts_with('\u{1}') {
                        line.push_str("^[.] ");
                    } else if let Some(written) = refs.and_then(|r| r.footnote(label)) {
                        line.push_str("[^");
                        line.push_str(written);
                        line.push_str("] ");
                        if !notes.contains(label) {
                            stubs.push(written);
                        }
                    }
                }
                cx.prefix.push_str(line.trim_end());
                cx.prefix.push_str("\n\n");
            }
        }
        // Macros defined before it, for its formulas.
        if kind == BlockWidget::Math || source.contains('$') || source.contains("\\(") {
            for (_, tex) in self.macros.iter().take_while(|(at, _)| *at < range.start) {
                cx.prefix.push_str("$$\n");
                cx.prefix.push_str(tex);
                cx.prefix.push_str("\n$$\n\n");
            }
        }
        if kind != BlockWidget::Math {
            let mut suffix = String::new();
            if let Some(refs) = refs {
                for label in reference_labels(source) {
                    if let Some(def) = refs.lookup(&label) {
                        suffix.push_str(&definition(
                            &label,
                            &def.destination,
                            def.title.as_deref(),
                        ));
                    }
                }
                for label in &notes {
                    match self.footnotes.get(label) {
                        Some(def) => {
                            suffix.push_str(def);
                            suffix.push_str("\n\n");
                        }
                        None => {
                            if let Some(written) = refs.footnote(label) {
                                stubs.push(written);
                            }
                        }
                    }
                }
            }
            for written in stubs {
                suffix.push_str("[^");
                suffix.push_str(written);
                suffix.push_str("]: …\n\n");
            }
            if !suffix.is_empty() {
                cx.suffix.push_str("\n\n");
                cx.suffix.push_str(&suffix);
            }
        }
        (!cx.prefix.is_empty() || !cx.suffix.is_empty()).then_some(cx)
    }

    /// The normalized labels of the defined footnotes `source` refers to,
    /// without repeats.
    fn footnotes_in(&self, source: &str) -> Vec<String> {
        let Some(refs) = self.refs.as_deref() else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        for label in bracketed(source) {
            if let Some(label) = label.strip_prefix('^') {
                let key = normalize_label(label);
                if refs.footnote(&key).is_some() && !out.contains(&key) {
                    out.push(key);
                }
            }
        }
        out
    }
}

/// The text inside each `[…]` of `source` that holds no bracket.
fn bracketed(source: &str) -> impl Iterator<Item = &str> {
    let mut rest = source;
    std::iter::from_fn(move || {
        loop {
            let open = rest.find('[')?;
            let after = &rest[open + 1..];
            let close = after.find([']', '['])?;
            if after.as_bytes()[close] == b'[' {
                rest = &after[close..];
                continue;
            }
            rest = &after[close + 1..];
            let inner = &after[..close];
            if !inner.trim().is_empty() && inner.len() <= 999 {
                return Some(inner);
            }
        }
    })
}

/// The normalized labels of the reference links `source` may hold,
/// without repeats.
fn reference_labels(source: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for label in bracketed(source).filter(|l| !l.starts_with('^')) {
        let key = normalize_label(label);
        if !key.is_empty() && !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

/// A link reference definition of `label` (normalized: it normalizes to
/// itself) to `destination` with `title`, every punctuation character
/// escaped so the definition reads back the same.
fn definition(label: &str, destination: &str, title: Option<&str>) -> String {
    fn escaped(out: &mut String, s: &str) {
        for c in s.chars() {
            if c.is_ascii_punctuation() {
                out.push('\\');
            }
            if c == '\n' || c == '\r' {
                out.push(' ');
            } else {
                out.push(c);
            }
        }
    }
    let mut out = String::new();
    out.push('[');
    out.push_str(label);
    out.push_str("]: <");
    escaped(&mut out, destination);
    out.push('>');
    if let Some(title) = title {
        out.push_str(" \"");
        escaped(&mut out, title);
        out.push('"');
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gv_syntax::ParseOptions;

    fn obsidian() -> ParseOptions {
        ParseOptions::from_profile(&gv_profile::compile_builtin(
            gv_profile::ProfileId::Obsidian,
        ))
    }

    fn context(text: &str, block: &str, kind: BlockWidget) -> Option<BlockContext> {
        let snapshot = TextSnapshot::new(text);
        let tree = SyntaxTree::parse_with(&snapshot, obsidian());
        let note = NoteContext::compute(&tree, &snapshot, true);
        let start = text.find(block).expect("block in text");
        note.for_block(kind, &(start..start + block.len()), block)
    }

    #[test]
    fn a_block_gets_the_definitions_it_uses() {
        let table = "| a | b |\n|---|---|\n| [x][r] | y[^n] |";
        let text = format!(
            "Before[^m] and [^n].\n\n{table}\n\n[r]: https://e.com/a&b \"T\"\n[s]: unused\n\n[^m]: M note.\n[^n]: N note.\n"
        );
        let cx = context(&text, table, BlockWidget::Table).expect("context");
        // Footnotes numbered before it, in order.
        assert_eq!(cx.prefix, "[^m] [^n]\n\n");
        assert!(
            cx.suffix
                .contains("[r]: <https\\:\\/\\/e\\.com\\/a\\&b> \"T\"\n"),
            "{cx:?}"
        );
        assert!(!cx.suffix.contains("[s]"), "{cx:?}");
        assert!(cx.suffix.contains("[^n]: N note."), "{cx:?}");
        // [^m] is only numbered before it: a stand-in.
        assert!(cx.suffix.contains("[^m]: …"), "{cx:?}");
        // A block that uses nothing has no context.
        let lone = "| c |\n|---|\n| d |";
        let text = format!("{lone}\n\n[r]: /x\n");
        assert_eq!(context(&text, lone, BlockWidget::Table), None);
    }

    #[test]
    fn formulas_before_a_block_give_their_macros() {
        let math = "$$\n\\R^2\n$$";
        let text = format!(
            "$\\newcommand{{\\R}}{{\\mathbb{{R}}}}$ first\n\n{math}\n\n$$\n\\def\\later{{x}}\n$$\n"
        );
        let cx = context(&text, math, BlockWidget::Math).expect("context");
        assert_eq!(cx.prefix, "$$\n\\newcommand{\\R}{\\mathbb{R}}\n$$\n\n");
        assert!(cx.suffix.is_empty());
        // Code has none.
        assert_eq!(context(&text, math, BlockWidget::Code), None);
    }

    #[test]
    fn brackets_are_split_at_the_innermost_pair() {
        let found: Vec<&str> = bracketed("[a] [[b]] [c [d] e] []").collect();
        assert_eq!(found, ["a", "b", "d"]);
    }
}
