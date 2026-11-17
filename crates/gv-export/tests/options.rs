// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The export options as the export dialog sets them: the page of a PDF
//! (paper, orientation, margins, header, page numbers, page breaks before
//! level-1 headings) and the table of contents, in the generated Typst
//! source. Two golden files (`fixtures/options-*.typ`) hold the whole
//! source for the defaults and for every option changed; run with
//! `GV_BLESS=1` to write them again after an intended change.

use std::path::{Path, PathBuf};

use gv_export::{ExportOptions, ExportTheme, PageOptions, Paper, export_typst};
use gv_model::DocModel;
use gv_profile::ProfileId;
use gv_render_tree::{AllPending, NoImageSizes, RenderOptions, RenderTree, Sources};
use gv_syntax::ParseOptions;

const DOC: &str = "# First part\n\nSome *text* and `code`.\n\n## A section\n\nMore text.\n\n# Second part\n\nThe end.\n";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tree(src: &str) -> RenderTree {
    let id = ProfileId::Extended;
    let syntax = gv_syntax::parse_with(src, ParseOptions::builtin(id));
    let model = DocModel::build(&syntax, src);
    RenderTree::build(
        &syntax,
        &model,
        src,
        RenderOptions::builtin(id),
        Sources {
            links: &AllPending,
            images: &NoImageSizes,
        },
    )
}

/// A theme with made-up colours: token `i` is grey `i`.
fn theme() -> ExportTheme {
    let mut i = 0u8;
    ExportTheme::from_fn(|_| {
        i += 1;
        Some([i, i, i, 255])
    })
}

fn source(toc: bool, page: &PageOptions) -> String {
    let mut opts = ExportOptions::new(Some(fixtures().join("doc.md")), theme());
    opts.toc = toc;
    export_typst(&tree(DOC), &opts, page).source
}

/// Every option changed from its default.
fn custom() -> PageOptions {
    PageOptions {
        paper: Paper::Letter,
        landscape: true,
        margin_mm: 35.0,
        header: false,
        page_numbers: false,
        break_before_h1: true,
        ..PageOptions::default()
    }
}

fn golden(name: &str, source: &str) {
    let path = fixtures().join(format!("{name}.typ"));
    if std::env::var_os("GV_BLESS").is_some() {
        std::fs::write(&path, source).expect("write golden");
    }
    let expected = std::fs::read_to_string(&path).expect("golden file (GV_BLESS=1 writes it)");
    assert!(
        source == expected,
        "source differs from {}; run with GV_BLESS=1 and review the diff",
        path.display()
    );
}

#[test]
fn golden_default_options() {
    golden("options-default", &source(false, &PageOptions::default()));
}

#[test]
fn golden_custom_options() {
    golden("options-custom", &source(true, &custom()));
}

#[test]
fn each_option_reaches_the_page_set_up() {
    let base = source(false, &PageOptions::default());
    assert!(base.contains("paper: \"a4\", flipped: false, margin: 20mm"));
    assert!(base.contains("counter(page).display"));
    assert!(base.contains("header: context"));
    assert!(!base.contains("#outline()"));
    assert!(!base.contains("pagebreak(weak: true)"));

    for (paper, typst) in [
        (Paper::A3, "a3"),
        (Paper::A5, "a5"),
        (Paper::Letter, "us-letter"),
        (Paper::Legal, "us-legal"),
    ] {
        let page = PageOptions {
            paper,
            ..PageOptions::default()
        };
        assert!(
            source(false, &page).contains(&format!("paper: \"{typst}\"")),
            "{paper:?}"
        );
    }

    let s = source(true, &custom());
    assert!(s.contains("paper: \"us-letter\", flipped: true, margin: 35mm"));
    assert!(s.contains("header: none, footer: none"));
    assert!(s.contains("#outline()"));
    assert!(s.contains("pagebreak(weak: true)"));
}

#[test]
fn margins_are_clamped() {
    for (asked, used) in [(1.0, "5mm"), (500.0, "60mm"), (f32::NAN, "20mm")] {
        let page = PageOptions {
            margin_mm: asked,
            ..PageOptions::default()
        };
        assert!(
            source(false, &page).contains(&format!("margin: {used}")),
            "{asked}"
        );
    }
}

#[test]
fn the_bundled_faces_come_first() {
    let opts = ExportOptions::new(None, theme());
    let doc = export_typst(&tree(DOC), &opts, &PageOptions::default());
    assert_eq!(
        doc.fonts.first().map(String::as_str),
        Some("Inter Variable")
    );
    assert!(doc.fonts.iter().any(|f| f == "JetBrains Mono"));
    assert!(
        doc.source
            .contains("#set text(font: (\"Inter Variable\", \"Inter\",")
    );
    assert!(
        doc.source
            .contains("#show raw: set text(font: (\"JetBrains Mono\",")
    );
}
