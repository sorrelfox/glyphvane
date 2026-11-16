// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Property tests on random documents and edits:
//!
//! - every built tree satisfies the invariants (`common::check`): items in
//!   order and disjoint, every rendered content byte in one item, chunks
//!   tiling their block, estimates non-increasing in the width, item texts
//!   mapping into their items;
//! - an incremental update equals a full build of the new text;
//! - items outside the reparsed window keep their ids (and sources,
//!   shifted);
//! - a refresh after the sources changed equals a build with the new
//!   sources.

mod common;

use proptest::prelude::*;

use gv_render_tree::{ImageSize, LinkStatus, LinkTarget, RenderOptions, RenderTree, Sources};
use gv_syntax::{FrontMatterOptions, ParseOptions};

const LINES: &[&str] = &[
    "",
    "",
    "",
    "para *em* **strong** `code` text",
    "more text with [link](exact.md) and [ref]",
    "# Heading",
    "## Heading",
    "Setext",
    "---",
    "===",
    "- item",
    "- [link](missing.md) x",
    "  - nested",
    "    - deeper",
    "1. one",
    "2) two",
    "7. seven",
    "> quote",
    "> > deep",
    ">",
    "> - quoted item",
    "```rust",
    "```",
    "~~~",
    "    indented code",
    "<div>",
    "</div>",
    "<!-- c -->",
    "[ref]: /url",
    "[ref] use",
    "![img](small.png) ![other](later.png)",
    "<https://x.org> <a@b.c>",
    "a\\*b &amp; c  ",
    "tab\there",
    "* * *",
    "  continuation",
    "日本語 text é",
    "-",
    "10. ten",
    "    ",
    "- > quote in item",
    "1. - mixed",
    "   ```",
    "<pre>",
    "</pre>",
    "***bold italic***",
    "`` co`de ``",
    "nul\u{0}x &#0;",
    "crlf line\r",
    "\ttabbed",
    ">\t> tab quote",
    "-\tx",
    "[![a](small.png)](exact.md)",
    "<span>raw *x*</span>",
];

const INSERTS: &[&str] = &[
    "x",
    "\n",
    "\n\n",
    "- ",
    "> ",
    "```\n",
    "# ",
    "*",
    "[ref]: /other\n",
    "    ",
    "",
    "1. ",
    "\\",
    "`",
    "<div>\n",
    "日",
    "---\n",
    "  ",
    "\r\n",
    "\t",
    "]",
    "(",
];

fn status_of(t: &LinkTarget<'_>) -> LinkStatus {
    match t.destination {
        "exact.md" => LinkStatus::Exact,
        "missing.md" => LinkStatus::Unresolved,
        d if d.starts_with("https:") => LinkStatus::External,
        d if d.starts_with("/") => LinkStatus::Ambiguous(3),
        _ => LinkStatus::Pending,
    }
}

fn size_of(t: &LinkTarget<'_>) -> ImageSize {
    match t.destination {
        "small.png" => ImageSize::Known {
            width: 40.0,
            height: 30.0,
        },
        _ => ImageSize::Pending,
    }
}

fn sources() -> Sources<'static> {
    Sources {
        links: &status_of,
        images: &size_of,
    }
}

fn doc() -> impl Strategy<Value = String> {
    prop::collection::vec(0..LINES.len(), 0..40).prop_map(|v| {
        let mut s = v.iter().map(|&i| LINES[i]).collect::<Vec<_>>().join("\n");
        s.push('\n');
        s
    })
}

/// An edit as fractions of the text, made concrete against a text.
#[derive(Debug, Clone)]
struct Edit {
    at: u32,
    del: usize,
    insert: usize,
}

fn edit() -> impl Strategy<Value = Edit> {
    (0u32..10_000, 0usize..16, 0..INSERTS.len()).prop_map(|(at, del, insert)| Edit {
        at,
        del,
        insert,
    })
}

impl Edit {
    fn range(&self, text: &str) -> std::ops::Range<usize> {
        let floor = |mut i: usize| {
            i = i.min(text.len());
            while !text.is_char_boundary(i) {
                i -= 1;
            }
            i
        };
        let start = floor(text.len() * self.at as usize / 10_000);
        let end = floor(start + self.del).max(start);
        start..end
    }
}

fn build(text: &str) -> common::Doc {
    common::build_with(
        text,
        ParseOptions::default(),
        RenderOptions::default(),
        sources(),
    )
}

proptest! {
    // 256 cases by default; `PROPTEST_CASES` raises it.
    #![proptest_config(ProptestConfig {
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn invariants_hold(text in doc()) {
        common::check(&build(&text));
    }

    #[test]
    fn update_equals_full_build(text in doc(), edits in prop::collection::vec(edit(), 1..5)) {
        let mut d = build(&text);
        for e in edits {
            let range = e.range(&d.text);
            d.edit(range, INSERTS[e.insert], sources());
            // A full build of the same (incrementally reparsed) syntax tree:
            // BlockIds of a fresh parse differ.
            let full = RenderTree::build(
                &d.tree,
                &d.model,
                d.text.as_str(),
                RenderOptions::default(),
                sources(),
            );
            prop_assert_eq!(
                common::canonical(&d.render),
                common::canonical(&full),
                "text {:?}",
                d.text
            );
            common::check(&d);
        }
    }

    #[test]
    fn ids_outside_the_window_are_stable(text in doc(), e in edit()) {
        let mut d = build(&text);
        let before: Vec<_> = d.render.items().to_vec();
        let old_len = d.text.len();
        let range = e.range(&d.text);
        let (changes, report) = d.edit(range, INSERTS[e.insert], sources());
        let delta = d.text.len() as isize - old_len as isize;
        let w = changes.old_range.clone();
        for it in &before {
            let shifted = if it.source.end < w.start {
                Some(it.source.clone())
            } else if it.source.start >= w.end && !changes.is_empty() {
                Some(
                    it.source.start.saturating_add_signed(delta)
                        ..it.source.end.saturating_add_signed(delta),
                )
            } else {
                None
            };
            let Some(source) = shifted else { continue };
            let ix = d.render.index_of(it.id);
            prop_assert!(ix.is_some(), "item {:?} lost; text {:?}", it.id, d.text);
            let now = &d.render.items()[ix.unwrap_or(0)];
            prop_assert_eq!(&now.source, &source, "item {:?}", it.id);
            prop_assert_eq!(&now.kind, &it.kind);
        }
        // Every inserted item that is new or rebuilt is reported.
        for ix in report.inserted.clone() {
            let id = d.render.items()[ix].id;
            if !before.iter().any(|it| it.id == id) {
                prop_assert!(report.changed.contains(&id), "new item {:?} not reported", id);
            }
        }
    }

    #[test]
    fn update_with_front_matter_equals_full_build(
        text in doc(),
        fm in 0usize..3,
        edits in prop::collection::vec(edit(), 1..4),
    ) {
        let head = ["---\ntitle: x\n---\n", "---\n", ""][fm];
        let text = format!("{head}{text}");
        let parse = ParseOptions { front_matter: FrontMatterOptions::YAML, ..ParseOptions::default() };
        let mut d = common::build_with(&text, parse, RenderOptions::default(), sources());
        for e in edits {
            let range = e.range(&d.text);
            d.edit(range, INSERTS[e.insert], sources());
            let full = RenderTree::build(
                &d.tree,
                &d.model,
                d.text.as_str(),
                RenderOptions::default(),
                sources(),
            );
            prop_assert_eq!(common::canonical(&d.render), common::canonical(&full));
            common::check(&d);
        }
    }

    #[test]
    fn refresh_equals_build_with_new_sources(text in doc()) {
        let mut d = common::build(&text);
        let report = d.render.refresh(&d.tree, d.text.as_str(), sources());
        let full = build(&text);
        prop_assert_eq!(common::canonical(&d.render), common::canonical(&full.render));
        prop_assert!(!report.full);
    }
}
