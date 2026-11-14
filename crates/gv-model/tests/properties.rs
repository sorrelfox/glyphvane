// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Property tests:
//!
//! - slugs are deterministic, and unique within a document (except where
//!   the tool itself repeats ids: kramdown's counter);
//! - the outline is consistent with heading order and levels;
//! - [`DocModel::update`] after an edit equals [`DocModel::build`] of the
//!   same tree, on `str` and on a rope snapshot;
//! - the GitLab slugs of random headings equal comrak's anchorizer over
//!   comrak's heading text.
//!
//! Case counts: `GV_PROP_CASES` (default 256).

use std::collections::HashSet;

use comrak::nodes::{AstNode, NodeValue};
use gv_model::{DocModel, SlugAlgorithm, Slugger};
use gv_syntax::{Change, SyntaxTree};
use gv_text::{TextSnapshot, TextSource};
use proptest::prelude::*;

fn cases() -> u32 {
    std::env::var("GV_PROP_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
}

/// Heading-ish text: words, punctuation, Unicode, inline syntax.
fn heading_text() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        Just("word".to_owned()),
        Just("Dup".to_owned()),
        Just(" ".to_owned()),
        Just("  ".to_owned()),
        Just("-".to_owned()),
        Just("_".to_owned()),
        Just("1".to_owned()),
        Just("Café".to_owned()),
        Just("Θ".to_owned()),
        Just("İ".to_owned()),
        Just("😄".to_owned()),
        Just("中文".to_owned()),
        Just("e\u{301}".to_owned()),
        Just("²".to_owned()),
        Just(".".to_owned()),
        Just("&amp;".to_owned()),
        Just("\\*".to_owned()),
        Just("*em*".to_owned()),
        Just("**strong**".to_owned()),
        Just("`code`".to_owned()),
        Just("[link](url)".to_owned()),
        Just("![alt *x*](i.png)".to_owned()),
        Just("<b>".to_owned()),
        Just("</b>".to_owned()),
        Just("<https://x.y/z>".to_owned()),
        Just("[ref]".to_owned()),
        "[a-zA-Z0-9 !?:()_-]{0,6}",
    ];
    prop::collection::vec(piece, 0..8).prop_map(|v| v.concat())
}

/// A random Markdown document: blocks from a small grammar.
fn document() -> impl Strategy<Value = String> {
    let block = prop_oneof![
        (1usize..=6, heading_text()).prop_map(|(l, t)| format!("{} {t}\n", "#".repeat(l))),
        heading_text().prop_map(|t| format!("{t}x\n===\n")),
        heading_text().prop_map(|t| format!("x{t}\n---\n")),
        heading_text().prop_map(|t| format!("Para {t} [link](a.md#Dup) and [ref].\n")),
        Just("[ref]: /target \"title\"\n".to_owned()),
        Just("[Ref]:\n  <other target>\n".to_owned()),
        heading_text().prop_map(|t| format!("> # {t}\n> text [x](y)\n")),
        heading_text().prop_map(|t| format!("- item\n  ## {t}\n- [a][ref]\n")),
        Just("```\n# not a heading [x](y)\n```\n".to_owned()),
        Just("<div>\n# html\n</div>\n".to_owned()),
        Just("\n".to_owned()),
        Just("plain text line\n".to_owned()),
    ];
    prop::collection::vec(block, 0..14).prop_map(|v| v.concat())
}

/// Snippets an edit inserts.
const SNIPPETS: &[&str] = &[
    "#",
    "# ",
    "\n",
    "\n\n",
    "=",
    "===\n",
    "-",
    "> ",
    "[",
    "]",
    "(",
    ")",
    "x",
    " ",
    "`",
    "```\n",
    "[ref]: /u\n",
    "[ref]",
    "<",
    "![i](p)",
    "é",
    "Dup",
];

#[derive(Debug, Clone)]
struct EditSpec {
    at: f64,
    del: usize,
    insert: Vec<usize>,
}

fn edits() -> impl Strategy<Value = Vec<EditSpec>> {
    prop::collection::vec(
        (
            0.0f64..=1.0,
            0usize..12,
            prop::collection::vec(0..SNIPPETS.len(), 0..3),
        )
            .prop_map(|(at, del, insert)| EditSpec { at, del, insert }),
        1..6,
    )
}

/// Applies `e` to `text` at char boundaries; returns the change.
fn apply(text: &mut String, e: &EditSpec) -> Change {
    let mut start = ((text.len() as f64) * e.at) as usize;
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (start + e.del).min(text.len());
    while !text.is_char_boundary(end) {
        end += 1;
    }
    let ins: String = e.insert.iter().map(|&i| SNIPPETS[i]).collect();
    text.replace_range(start..end, &ins);
    Change {
        start,
        old_end: end,
        new_len: ins.len(),
    }
}

/// Everything a model holds, as comparable values.
fn snapshot(m: &DocModel) -> String {
    let mut s = format!(
        "{:?}\n{:?}\n{:?}\n{:?}\n",
        m.headings(),
        m.outline(),
        m.links(),
        m.definitions()
    );
    for alg in SlugAlgorithm::ALL {
        s.push_str(&format!("{:?}\n", m.slugs(alg)));
    }
    s
}

fn check_outline(m: &DocModel) -> Result<(), TestCaseError> {
    let h = m.headings();
    let o = m.outline();
    prop_assert_eq!(h.len(), o.len());
    for (i, e) in o.iter().enumerate() {
        if i > 0 {
            prop_assert!(h[i - 1].range.start < h[i].range.start, "headings in order");
        }
        match e.parent {
            Some(p) => {
                prop_assert!(p < i);
                prop_assert!(h[p].level < h[i].level);
                prop_assert_eq!(e.depth, o[p].depth + 1);
                prop_assert!(o[p].section_end >= e.section_end);
                // No heading between the parent and this one closes the
                // parent's section or would be a closer parent.
                for j in p + 1..i {
                    prop_assert!(h[j].level >= h[i].level || h[j].level > h[p].level);
                    prop_assert!(h[j].level > h[p].level);
                }
            }
            None => {
                prop_assert_eq!(e.depth, 0);
                for j in 0..i {
                    prop_assert!(h[j].level >= h[i].level);
                }
            }
        }
        prop_assert!(e.section_end > i && e.section_end <= h.len());
        for j in i + 1..e.section_end {
            prop_assert!(h[j].level > h[i].level);
        }
        if let Some(next) = h.get(e.section_end) {
            prop_assert!(next.level <= h[i].level);
        }
    }
    Ok(())
}

fn comrak_walk<'a>(n: &'a AstNode<'a>, f: &mut impl FnMut(&'a AstNode<'a>)) {
    f(n);
    for c in n.children() {
        comrak_walk(c, f);
    }
}

fn run_edits<S: TextSource>(
    mut text: String,
    edits: &[EditSpec],
    source: impl Fn(&str) -> S,
) -> Result<usize, TestCaseError> {
    let src = source(&text);
    let mut tree = SyntaxTree::parse(&src);
    let mut model = DocModel::build(&tree, &src);
    let mut reused = 0;
    for e in edits {
        let change = apply(&mut text, e);
        let src = source(&text);
        let (next, changes) = tree.reparse_full(&src, change);
        let updated = model.update(&next, &src, &changes);
        let full = DocModel::build(&next, &src);
        prop_assert_eq!(
            snapshot(&updated),
            snapshot(&full),
            "after {:?} on {:?}",
            e,
            text
        );
        reused += updated.stats().reused;
        tree = next;
        model = updated;
    }
    Ok(reused)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]

    #[test]
    fn slugs_are_deterministic(texts in prop::collection::vec(heading_text(), 0..12)) {
        for alg in SlugAlgorithm::ALL {
            let (mut a, mut b) = (Slugger::new(alg), Slugger::new(alg));
            for t in &texts {
                prop_assert_eq!(alg.slug(t), alg.slug(t));
                prop_assert_eq!(a.slug(t), b.slug(t));
            }
        }
    }

    #[test]
    fn duplicate_suffixes_are_unique(texts in prop::collection::vec(heading_text(), 0..24)) {
        for alg in SlugAlgorithm::ALL {
            if alg == SlugAlgorithm::Kramdown {
                continue;
            }
            let mut s = Slugger::new(alg);
            let mut seen = HashSet::new();
            for t in &texts {
                let id = s.slug(t);
                prop_assert!(seen.insert(id.clone()), "{:?} repeated {:?}", alg, id);
            }
        }
    }

    #[test]
    fn outline_is_consistent(doc in document()) {
        let tree = gv_syntax::parse(&doc);
        let m = DocModel::build(&tree, doc.as_str());
        check_outline(&m)?;
        for alg in SlugAlgorithm::ALL {
            prop_assert_eq!(m.slugs(alg).len(), m.headings().len());
        }
    }

    #[test]
    fn update_equals_build(doc in document(), edits in edits()) {
        run_edits(doc, &edits, |s| s.to_owned())?;
    }

    #[test]
    fn update_equals_build_on_a_rope(doc in document(), edits in edits()) {
        run_edits(doc, &edits, TextSnapshot::new)?;
    }

    #[test]
    fn gitlab_slugs_match_comrak(texts in prop::collection::vec(heading_text(), 1..6)) {
        let doc: String = texts.iter().map(|t| format!("## {t}\n\n")).collect();
        let m = DocModel::build(&gv_syntax::parse(&doc), doc.as_str());
        let arena = comrak::Arena::new();
        let root = comrak::parse_document(&arena, &doc, &comrak::Options::default());
        let mut anchorizer = comrak::Anchorizer::new();
        let mut ids = Vec::new();
        comrak_walk(root, &mut |n| {
            if let NodeValue::Heading(_) = n.data().value {
                ids.push(anchorizer.anchorize(&n.collect_text()));
            }
        });
        let ours: Vec<String> = m.slugs(SlugAlgorithm::GitLab).iter().map(|s| s.to_string()).collect();
        prop_assert_eq!(ours, ids, "{:?}", doc);
    }
}

#[test]
fn update_reuses_untouched_blocks() {
    let mut text = String::new();
    for i in 0..200 {
        text.push_str(&format!(
            "## Section {i}\n\nSee [link {i}](n{i}.md#part) and [ref].\n\n"
        ));
    }
    text.push_str("[ref]: /target\n");
    let tree = SyntaxTree::parse(text.as_str());
    let model = DocModel::build(&tree, text.as_str());
    assert_eq!(model.headings().len(), 200);
    assert_eq!(model.links().len(), 400);
    // Type into the middle heading.
    let at = text.find("Section 100").expect("heading") + 7;
    text.insert(at, 'X');
    let (next, changes) = tree.reparse_full(
        text.as_str(),
        Change {
            start: at,
            old_end: at,
            new_len: 1,
        },
    );
    let updated = model.update(&next, text.as_str(), &changes);
    let stats = updated.stats();
    assert!(stats.reused >= 398, "{stats:?}");
    assert!(stats.inline_parses <= 2, "{stats:?}");
    assert_eq!(
        snapshot(&updated),
        snapshot(&DocModel::build(&next, text.as_str()))
    );
    assert_eq!(&*updated.headings()[100].text, "SectionX 100");
    assert_eq!(updated.headings()[150].line, model.headings()[150].line);
    assert_eq!(
        updated.headings()[150].range.start,
        model.headings()[150].range.start + 1
    );

    // A new definition changes what `[ref]` points to everywhere.
    let at = text.find("[ref]: /target").expect("def");
    text.insert_str(at, "[ref]: /first\n");
    let (next2, changes) = next.reparse_full(
        text.as_str(),
        Change {
            start: at,
            old_end: at,
            new_len: 14,
        },
    );
    let updated2 = updated.update(&next2, text.as_str(), &changes);
    assert_eq!(
        snapshot(&updated2),
        snapshot(&DocModel::build(&next2, text.as_str()))
    );
    assert!(
        updated2
            .links()
            .iter()
            .filter(|l| l.kind.is_reference())
            .all(|l| &*l.destination == "/first")
    );
}
