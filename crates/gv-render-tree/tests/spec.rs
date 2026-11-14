// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The render tree on the CommonMark spec examples:
//!
//! - every example builds and satisfies the structural invariants
//!   (`common::check`), and an edit-free update keeps it unchanged;
//! - snapshots of representative examples and of the features the spec
//!   does not show (link statuses, image sizes, front matter, chunking).
//!
//! Snapshots are plain text files in `tests/snapshots/`; run with
//! `UPDATE_SNAPSHOTS=1` to rewrite them after a deliberate change.

mod common;

use std::fmt::Write as _;
use std::path::PathBuf;

use gv_render_tree::{
    CopyOptions, FrontMatterDisplay, ImageSize, LinkStatus, LinkTarget, RenderOptions, Sources,
};
use gv_syntax::{FrontMatterOptions, ParseOptions};

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

/// Compares `actual` with the snapshot file `name`, or rewrites it.
fn snapshot(name: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name);
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().expect("dir")).expect("mkdir");
        std::fs::write(&path, actual).expect("write snapshot");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (run with UPDATE_SNAPSHOTS=1)", path.display()));
    if expected != actual {
        let (mut line, mut first) = (0, String::new());
        for (i, (a, b)) in expected.lines().zip(actual.lines()).enumerate() {
            if a != b {
                line = i + 1;
                first = format!("expected: {a}\n  actual: {b}");
                break;
            }
        }
        panic!(
            "snapshot {name} differs (first difference at line {line}):\n{first}\n\
             run with UPDATE_SNAPSHOTS=1 to accept"
        );
    }
}

#[test]
fn every_example_holds_the_invariants() {
    let examples = spec();
    assert_eq!(examples.len(), 652);
    let mut items = 0;
    for (number, md) in examples {
        let doc = common::build(&md);
        let result = std::panic::catch_unwind(|| common::check(&doc));
        assert!(result.is_ok(), "example {number}: {md:?}");
        items += doc.render.items().len();
        // Copy of the whole document and find never panic.
        let _ = doc.render.plain_text(0..md.len(), CopyOptions::default());
        let _ = doc.render.find("a", Default::default());
        for ix in 0..doc.render.items().len() {
            let _ = doc.render.item_paragraph(ix);
        }
    }
    assert!(items > 800, "{items} items");
}

/// Representative examples: one or more per block construct and per
/// inline construct.
const EXAMPLES: &[u64] = &[
    1, 12, 25, 43, 62, 80, 107, 119, 142, 148, 192, 219, 228, 254, 264, 301, 304, 306, 307, 312,
    328, 350, 482, 571, 572, 594, 613, 633,
];

#[test]
fn spec_snapshots() {
    let all = spec();
    let mut out = String::new();
    for n in EXAMPLES {
        let md = &all[usize::try_from(*n).expect("index") - 1].1;
        let doc = common::build(md);
        let _ = writeln!(out, "=== example {n}: {md:?}");
        out.push_str(&doc.render.dump());
        let _ = writeln!(
            out,
            "copy: {:?}",
            doc.render.plain_text(0..md.len(), CopyOptions::default())
        );
    }
    snapshot("spec.txt", &out);
}

/// Statuses by destination prefix, for the status snapshot.
fn status_of(t: &LinkTarget<'_>) -> LinkStatus {
    let d = t.destination;
    if d.starts_with("exact") {
        LinkStatus::Exact
    } else if d.starts_with("case") {
        LinkStatus::CaseFold
    } else if d.starts_with("loose") {
        LinkStatus::Loose
    } else if d.starts_with("ambiguous") {
        LinkStatus::Ambiguous(2)
    } else if d.starts_with("missing") {
        LinkStatus::Unresolved
    } else if d.starts_with("../") {
        LinkStatus::OutsideRoot
    } else if d.starts_with("https:") {
        LinkStatus::External
    } else if d.starts_with("javascript:") {
        LinkStatus::Blocked
    } else {
        LinkStatus::Pending
    }
}

fn size_of(t: &LinkTarget<'_>) -> ImageSize {
    match t.destination {
        "small.png" => ImageSize::Known {
            width: 32.0,
            height: 16.0,
        },
        "wide.png" => ImageSize::Known {
            width: 1200.0,
            height: 600.0,
        },
        "broken.png" => ImageSize::Failed,
        _ => ImageSize::Pending,
    }
}

#[test]
fn feature_snapshots() {
    let mut out = String::new();
    let sources = Sources {
        links: &status_of,
        images: &size_of,
    };
    let statuses = "[Exact](exact.md) [Case](case.md) [Loose](loose.md) \
                    [Ambiguous](ambiguous.md) [Pending](notes.md) [Missing](missing.md) \
                    [Out](../up.md) [Web](https://example.org) [Bad](javascript:x) \
                    <https://example.org/auto> <me@example.org> [*em* `code`](exact.md)\n";
    let images = "![Small](small.png) ![Wide](wide.png \"title\") ![Broken *alt*](broken.png) \
                  ![Loading](later.png) [![Linked](small.png)](exact.md)\n";
    let mut long = String::from("```text\n");
    for i in 0..120 {
        let _ = writeln!(long, "line {i}");
    }
    long.push_str("```\n");
    let nested = "> - a\n>   > b\n>   >\n>   > c\n> 1. one\n> 2. two\n>\n>    two b\n\n10) ten\n11) eleven\n";
    let headings = "# Dup\n\n## Dup\n\nSetext *x*\n---\n\n#\n";
    for (name, md) in [
        ("statuses", statuses),
        ("images", images),
        ("long code", long.as_str()),
        ("nested containers", nested),
        ("headings", headings),
    ] {
        let doc = common::build_with(
            md,
            ParseOptions::default(),
            RenderOptions::default(),
            sources,
        );
        common::check(&doc);
        let _ = writeln!(out, "=== {name}: {md:?}");
        out.push_str(&doc.render.dump());
        let _ = writeln!(
            out,
            "copy: {:?}",
            doc.render.plain_text(0..md.len(), CopyOptions::default())
        );
    }
    // Front matter: properties placeholder, or hidden.
    let fm = "---\ntitle: A\ntags: [x, y]\n---\nBody\n";
    let parse = ParseOptions {
        front_matter: FrontMatterOptions::YAML,
    };
    for display in [FrontMatterDisplay::Properties, FrontMatterDisplay::Hidden] {
        let opts = RenderOptions {
            front_matter: display,
            ..RenderOptions::default()
        };
        let doc = common::build_with(fm, parse, opts, Sources::default());
        common::check(&doc);
        let _ = writeln!(out, "=== front matter {display:?}: {fm:?}");
        out.push_str(&doc.render.dump());
    }
    snapshot("features.txt", &out);
}
