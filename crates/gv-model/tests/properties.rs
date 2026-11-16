// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Property tests:
//!
//! - slugs are deterministic, and unique within a document (except where
//!   the tool itself repeats ids: kramdown's counter);
//! - the outline is consistent with heading order and levels;
//! - [`DocModel::update`] after an edit equals [`DocModel::build`] of the
//!   same tree, on `str` and on a rope snapshot, with front matter off
//!   (CommonMark) and on (YAML and TOML), and after skipping revisions
//!   (their changes composed);
//! - nothing is recorded inside front matter;
//! - the GitLab slugs of random headings equal comrak's anchorizer over
//!   comrak's heading text.
//!
//! Case counts: `GV_PROP_CASES` (default 256).

use std::collections::HashSet;

use comrak::nodes::{AstNode, NodeValue};
use gv_model::{DocModel, SlugAlgorithm, Slugger};
use gv_syntax::{Change, Changes, ParseOptions, SyntaxTree};
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

/// A random document that may start with front matter (closed or not).
fn document_with_front_matter() -> impl Strategy<Value = String> {
    let head = prop_oneof![
        Just(""),
        Just("---\ntitle: x\n---\n"),
        Just("---\n# not a heading [x](y)\n[ref]: /fm\n...\n"),
        Just("---\n---\n"),
        Just("+++\na = \"[x](y)\"\n+++\n"),
        Just("---\nunclosed: [x](y)\n"),
    ];
    (head, document()).prop_map(|(h, d)| format!("{h}{d}"))
}

/// The options of the `extended` profile: YAML and TOML front matter.
fn front_matter_on() -> ParseOptions {
    ParseOptions::builtin(gv_syntax::ProfileId::Extended)
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
        // Obsidian (text in other profiles).
        heading_text().prop_map(|t| format!(
            "See [[Note#{t}|alias]] and ![[img.png]] #tag/x ^id-{}\n",
            t.len()
        )),
        Just("- item ^b1\n\n^own\n".to_owned()),
        Just("> [!note] T [[x]]\n> #t body\n".to_owned()),
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
    "---\n",
    "+++\n",
    "...\n",
    "[[",
    "]]",
    "#t",
    " ^a",
    "^",
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
        "{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n",
        m.headings(),
        m.outline(),
        m.links(),
        m.definitions(),
        m.front_matter(),
        m.tags(),
        m.block_anchors()
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

/// Nothing is recorded inside the front matter.
fn check_front_matter(m: &DocModel) -> Result<(), TestCaseError> {
    let Some(fm) = m.front_matter() else {
        return Ok(());
    };
    prop_assert_eq!(fm.range.start, 0);
    prop_assert!(fm.range.start < fm.content_range.start);
    prop_assert!(fm.content_range.end < fm.range.end);
    let after = |r: &std::ops::Range<usize>| r.start >= fm.range.end;
    prop_assert!(m.headings().iter().all(|h| after(&h.range)));
    prop_assert!(m.links().iter().all(|l| after(&l.range)));
    prop_assert!(m.definitions().iter().all(|d| after(&d.range)));
    Ok(())
}

/// Applies `edits` one by one, updating the model after each reparse and
/// comparing it with a full build; then checks one update over all the
/// revisions (the changes composed). Returns the blocks reused.
fn run_edits<S: TextSource>(
    mut text: String,
    edits: &[EditSpec],
    options: ParseOptions,
    source: impl Fn(&str) -> S,
) -> Result<usize, TestCaseError> {
    let src = source(&text);
    let mut tree = SyntaxTree::parse_with(&src, options);
    let mut model = DocModel::build(&tree, &src);
    let first = model.clone();
    let mut all = Vec::new();
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
        check_front_matter(&full)?;
        reused += updated.stats().reused;
        all.push(changes);
        tree = next;
        model = updated;
    }
    let src = source(&text);
    let skipped = first.update(&tree, &src, &Changes::compose_all(&all));
    prop_assert_eq!(
        snapshot(&skipped),
        snapshot(&model),
        "composed {:?} on {:?}",
        all,
        text
    );
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
        run_edits(doc, &edits, ParseOptions::default(), |s| s.to_owned())?;
    }

    #[test]
    fn update_equals_build_on_a_rope(doc in document(), edits in edits()) {
        run_edits(doc, &edits, ParseOptions::default(), TextSnapshot::new)?;
    }

    #[test]
    fn update_equals_build_obsidian(doc in document(), edits in edits()) {
        run_edits(doc, &edits, ParseOptions::builtin(gv_syntax::ProfileId::Obsidian), |s| s.to_owned())?;
    }

    #[test]
    fn update_equals_build_with_front_matter(
        doc in document_with_front_matter(),
        edits in edits(),
    ) {
        run_edits(doc, &edits, front_matter_on(), |s| s.to_owned())?;
    }

    #[test]
    fn update_equals_build_with_front_matter_on_a_rope(
        doc in document_with_front_matter(),
        edits in edits(),
    ) {
        run_edits(doc, &edits, front_matter_on(), TextSnapshot::new)?;
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
    // Only the reparsed window is walked.
    assert!(stats.leaves <= 4, "{stats:?}");
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

/// Cases that once broke [`DocModel::update`].
#[test]
fn update_regressions() {
    let e = |at: f64, del: usize, insert: &[usize]| EditSpec {
        at,
        del,
        insert: insert.to_vec(),
    };
    let cases: [(&str, Vec<EditSpec>); 4] = [
        // A text block before the window now ends where the window starts.
        (
            "Para  [link](a.md#Dup) and [ref].\n```\n# not a heading [x](y)\n```\n```\n# not a heading [x](y)\n```\n[Ref]:\n  <other target>\n[ref]: /target \"title\"\n### </b>e\u{301}Θ<b><b><https://x.y/z>\n",
            vec![
                e(0.747_342_500_312_087_2, 3, &[]),
                e(0.604_410_981_722_262_6, 0, &[1]),
            ],
        ),
        (
            "- item\n  ## **strong**`code`Dup²[link](url)\\*\n- [a][ref]\nPara [ref][link](url)*em*Θ*em* [link](a.md#Dup) and [ref].\n[Ref]:\n  <other target>\nplain text line\n> # <b>&amp;wordCafé😄e\u{301}\n> text [x](y)\nPara &amp;[ref] [link](a.md#Dup) and [ref].\n<div>\n# html\n</div>\nxe\u{301}e\u{301}e\u{301}<b>\n---\nx\n===\n[ref]: /target \"title\"\n```\n# not a heading [x](y)\n```\n",
            vec![
                e(0.878_367_883_042_208_4, 9, &[]),
                e(0.379_930_013_097_309_8, 0, &[0]),
            ],
        ),
        // Front matter, and an edit that changes nothing.
        (
            "---\ntitle: x\n---\n# 😄Caféwordwordword\nplain text line\n# ![alt *x*](i.png)&amp;&amp;word\n[Ref]:\n  <other target>\n",
            vec![e(0.0, 0, &[]), e(0.872_847_363_420_378_5, 0, &[1])],
        ),
        (
            "+++\na = \"[x](y)\"\n+++\n[Ref]:\n  <other target>\n<div>\n# html\n</div>\n[ref]: /target \"title\"\n[ref]: /target \"title\"\nx&amp;</b>word\n---\n# *em***strong**\n<div>\n# html\n</div>\n",
            vec![e(0.174_321_687_825_168, 0, &[0])],
        ),
    ];
    for (doc, edits) in cases {
        for options in [ParseOptions::default(), front_matter_on()] {
            run_edits(doc.to_owned(), &edits, options, |s| s.to_owned()).expect("update == build");
            run_edits(doc.to_owned(), &edits, options, TextSnapshot::new).expect("on a rope");
        }
    }
}
