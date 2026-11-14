// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The model on every CommonMark spec example:
//!
//! - building never panics, every heading gets a slug under every
//!   algorithm, and every slug finds its heading again;
//! - the GitLab algorithm equals comrak's anchorizer over comrak's own
//!   heading text (GitLab renders with comrak), which also checks the
//!   text-with-descriptions projection;
//! - every link the model reports is a link in comrak's tree too (same
//!   count and destinations per example).

use std::path::PathBuf;

use comrak::nodes::{AstNode, NodeValue};
use gv_model::{Anchoring, DocModel, FragmentMatch, SlugAlgorithm};

fn spec() -> Vec<(u64, String)> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/spec/commonmark-0.31.2.json");
    let text = std::fs::read_to_string(&path).expect("spec fixture");
    let json: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    json.as_array()
        .expect("array")
        .iter()
        .map(|e| {
            (
                e["example"].as_u64().expect("number"),
                e["markdown"].as_str().expect("markdown").to_owned(),
            )
        })
        .collect()
}

fn model(src: &str) -> DocModel {
    DocModel::build(&gv_syntax::parse(src), src)
}

fn comrak_walk<'a>(n: &'a AstNode<'a>, f: &mut impl FnMut(&'a AstNode<'a>)) {
    f(n);
    for c in n.children() {
        comrak_walk(c, f);
    }
}

/// comrak's heading ids (its anchorizer over `collect_text`) and link
/// destinations, in document order.
fn comrak_facts(src: &str) -> (Vec<String>, Vec<String>) {
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, src, &comrak::Options::default());
    let mut anchorizer = comrak::Anchorizer::new();
    let mut ids = Vec::new();
    let mut links = Vec::new();
    comrak_walk(root, &mut |n| match &n.data().value {
        NodeValue::Heading(_) => ids.push(anchorizer.anchorize(&n.collect_text())),
        NodeValue::Link(l) | NodeValue::Image(l) => links.push(l.url.clone()),
        _ => {}
    });
    (ids, links)
}

#[test]
fn every_heading_gets_a_slug() {
    let mut headings = 0;
    for (number, md) in spec() {
        let m = model(&md);
        headings += m.headings().len();
        assert_eq!(m.outline().len(), m.headings().len(), "example {number}");
        for alg in SlugAlgorithm::ALL {
            let slugs = m.slugs(alg);
            assert_eq!(slugs.len(), m.headings().len(), "example {number}");
            for (i, slug) in slugs.iter().enumerate() {
                // kramdown's counter can repeat an id: the first heading with
                // the slug is found.
                match m.match_fragment(&[&**slug], Anchoring::Slug(alg)) {
                    FragmentMatch::Heading {
                        index,
                        loose: false,
                    } => {
                        assert_eq!(slugs[index], *slug, "example {number} {alg:?} heading {i}");
                    }
                    FragmentMatch::Top if slug.eq_ignore_ascii_case("top") => {}
                    other => panic!("example {number} {alg:?} heading {i} {slug:?}: {other:?}"),
                }
            }
        }
        let anchoring = Anchoring::HeadingText;
        for (i, h) in m.headings().iter().enumerate() {
            if gv_model::slug::obsidian::heading_key(&h.source).is_empty() {
                continue;
            }
            match m.match_fragment(&[&*h.source], anchoring) {
                FragmentMatch::Heading {
                    index,
                    loose: false,
                } => assert!(index <= i),
                other => panic!("example {number} heading {i}: {other:?}"),
            }
        }
    }
    // 0.31.2 has 62 headings across its examples.
    assert!(headings >= 62, "{headings} headings");
}

/// Examples where gv-syntax and comrak 0.59 disagree on inlines (see the
/// parser's conformance notes); excluded from the link comparison.
const COMRAK_INLINE_DIVERGENCES: &[u64] = &[];

#[test]
fn gitlab_slugs_and_links_match_comrak() {
    let mut compared = 0;
    let mut mismatches = Vec::new();
    for (number, md) in spec() {
        let m = model(&md);
        let (ids, links) = comrak_facts(&md);
        let ours: Vec<String> = m
            .slugs(SlugAlgorithm::GitLab)
            .iter()
            .map(|s| s.to_string())
            .collect();
        if ours.len() == ids.len() {
            compared += ours.len();
            if ours != ids {
                mismatches.push(format!(
                    "example {number}: slugs {ours:?} vs comrak {ids:?}"
                ));
            }
        } else {
            mismatches.push(format!(
                "example {number}: {} headings vs comrak {}",
                ours.len(),
                ids.len()
            ));
        }
        if COMRAK_INLINE_DIVERGENCES.contains(&number) {
            continue;
        }
        // comrak stores URLs percent-encoded (cmark's normalization) and
        // `mailto:` on email autolinks; compare the counts, and the
        // destinations where nothing needed encoding.
        if m.links().len() != links.len() {
            mismatches.push(format!(
                "example {number}: {} links vs comrak {}",
                m.links().len(),
                links.len()
            ));
            continue;
        }
        for (ours, theirs) in m.links().iter().zip(&links) {
            let d = &*ours.destination;
            let plain = d
                .bytes()
                .all(|b| b.is_ascii_graphic() && !b"%\\\"<>`[]^{|}".contains(&b));
            if plain && d != theirs {
                mismatches.push(format!("example {number}: link {d:?} vs comrak {theirs:?}"));
            }
        }
    }
    assert!(compared >= 62, "{compared} headings compared");
    assert!(
        mismatches.is_empty(),
        "{} mismatches:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}
