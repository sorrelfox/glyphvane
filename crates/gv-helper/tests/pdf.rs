// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! PDF export end to end, in process: Markdown → render tree → generated
//! Typst (gv-export) → PDF (the helper's engine), read back with lopdf.
//!
//! The PDFs depend on the installed fonts, so the checks are on structure
//! (pages, outline, links, images, page size, determinism) and on the
//! extracted text, not on bytes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gv_export::{
    DiagramRenderer, ExportOptions, ExportTheme, PageOptions, Paper, TypstDocument, export_typst,
};
use gv_helper::{PdfJob, PdfOutput};
use gv_model::DocModel;
use gv_profile::ProfileId;
use gv_render_tree::{AllPending, NoImageSizes, RenderOptions, RenderTree, Sources};
use gv_syntax::ParseOptions;
use lopdf::{Document, Object};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../gv-export/tests/fixtures")
}

fn tree(src: &str, id: ProfileId) -> RenderTree {
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

/// A diagram as a box with a label drawn as a path.
struct Diagrams;

impl DiagramRenderer for Diagrams {
    fn svg(&self, _: &str) -> Option<String> {
        Some(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"120\" height=\"60\" viewBox=\"0 0 120 60\">\
             <rect x=\"1\" y=\"1\" width=\"118\" height=\"58\" fill=\"none\" stroke=\"#333\"/>\
             <path d=\"M10 30 L110 30\" stroke=\"#333\"/></svg>"
                .to_owned(),
        )
    }
}

fn theme() -> ExportTheme {
    ExportTheme::from_fn(|name| match name {
        "text.primary" => Some([20, 20, 20, 255]),
        "accent.default" => Some([30, 90, 200, 255]),
        _ => Some([200, 200, 200, 255]),
    })
}

fn document(src: &str, doc: &Path, page: &PageOptions) -> TypstDocument {
    let mut opts = ExportOptions::new(Some(doc.to_path_buf()), theme());
    opts.diagrams = Some(Arc::new(Diagrams));
    export_typst(&tree(src, ProfileId::Extended), &opts, page)
}

fn compile(doc: &TypstDocument, max_pages: u32) -> Result<PdfOutput, String> {
    gv_helper::engine::pdf(&PdfJob {
        source: doc.source.clone(),
        assets: doc.assets.clone(),
        fonts: doc.fonts.clone(),
        max_pages,
        ident: doc.ident.clone(),
    })
}

fn fixture(page: &PageOptions) -> (TypstDocument, PdfOutput) {
    let path = fixtures().join("all.md");
    let src = std::fs::read_to_string(&path).expect("fixture");
    let doc = document(&src, &path, page);
    let pdf = compile(&doc, 100).unwrap_or_else(|e| panic!("{e}\n{}", doc.source));
    (doc, pdf)
}

/// Whether a text font the document names is installed (text checks need
/// one).
fn fonts_installed() -> bool {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    db.faces().any(|f| {
        f.families
            .iter()
            .any(|(n, _)| gv_export::pdf::SANS_FONTS.contains(&n.as_str()))
    })
}

fn text(pdf: &Document) -> String {
    let pages: Vec<u32> = pdf.get_pages().keys().copied().collect();
    pdf.extract_text(&pages).unwrap_or_default()
}

/// Every dictionary of the file, resolved streams included.
fn dictionaries(pdf: &Document) -> Vec<&lopdf::Dictionary> {
    pdf.objects
        .values()
        .filter_map(|o| match o {
            Object::Dictionary(d) => Some(d),
            Object::Stream(s) => Some(&s.dict),
            _ => None,
        })
        .collect()
}

fn name_is(d: &lopdf::Dictionary, key: &[u8], value: &[u8]) -> bool {
    d.get(key)
        .ok()
        .and_then(|o| o.as_name().ok())
        .is_some_and(|n| n == value)
}

/// The link annotations: external URIs and the number of internal ones.
fn links(pdf: &Document) -> (BTreeSet<String>, usize) {
    let mut uris = BTreeSet::new();
    let mut internal = 0;
    for d in dictionaries(pdf) {
        if !name_is(d, b"Subtype", b"Link") {
            continue;
        }
        let action = d.get(b"A").ok().and_then(|a| match a {
            Object::Dictionary(a) => Some(a.clone()),
            Object::Reference(r) => pdf.get_dictionary(*r).ok().cloned(),
            _ => None,
        });
        match action {
            Some(a) if name_is(&a, b"S", b"URI") => {
                if let Ok(Object::String(bytes, _)) = a.get(b"URI") {
                    uris.insert(String::from_utf8_lossy(bytes).into_owned());
                }
            }
            Some(a) if name_is(&a, b"S", b"GoTo") => internal += 1,
            _ if d.has(b"Dest") => internal += 1,
            _ => {}
        }
    }
    (uris, internal)
}

/// Image XObjects, without the soft masks (alpha channels) of others.
fn image_count(pdf: &Document) -> usize {
    let images: Vec<(&lopdf::ObjectId, &lopdf::Stream)> = pdf
        .objects
        .iter()
        .filter_map(|(id, o)| match o {
            Object::Stream(s) if name_is(&s.dict, b"Subtype", b"Image") => Some((id, s)),
            _ => None,
        })
        .collect();
    let masks: BTreeSet<lopdf::ObjectId> = images
        .iter()
        .filter_map(|(_, s)| s.dict.get(b"SMask").and_then(Object::as_reference).ok())
        .collect();
    images.iter().filter(|(id, _)| !masks.contains(id)).count()
}

/// The number of outline items (bookmarks), at every level.
fn outline_count(pdf: &Document) -> usize {
    fn items(pdf: &Document, first: Option<lopdf::ObjectId>) -> usize {
        let mut n = 0;
        let mut next = first;
        while let Some(id) = next {
            let Ok(item) = pdf.get_dictionary(id) else {
                break;
            };
            n += 1 + items(pdf, item.get(b"First").and_then(Object::as_reference).ok());
            next = item.get(b"Next").and_then(Object::as_reference).ok();
            if n > 10_000 {
                break;
            }
        }
        n
    }
    let first = pdf
        .catalog()
        .and_then(|c| c.get(b"Outlines"))
        .and_then(Object::as_reference)
        .and_then(|r| pdf.get_dictionary(r))
        .and_then(|o| o.get(b"First"))
        .and_then(Object::as_reference)
        .ok();
    items(pdf, first)
}

fn media_box(pdf: &Document) -> Vec<f32> {
    let (_, page) = pdf.get_pages().into_iter().next().expect("a page");
    let dict = pdf.get_dictionary(page).expect("page dictionary");
    dict.get(b"MediaBox")
        .and_then(Object::as_array)
        .map(|a| a.iter().filter_map(|o| o.as_float().ok()).collect())
        .unwrap_or_default()
}

#[test]
fn the_fixture_becomes_a_pdf_with_outline_links_and_images() {
    let (doc, out) = fixture(&PageOptions::default());
    assert!(out.pdf.starts_with(b"%PDF-"));
    let pdf = Document::load_mem(&out.pdf).expect("a readable PDF");
    assert_eq!(pdf.get_pages().len() as u32, out.pages);
    assert!(out.pages >= 1);

    // A4 by default.
    let mb = media_box(&pdf);
    assert_eq!(mb.len(), 4);
    assert!(
        (mb[2] - 595.28).abs() < 1.0 && (mb[3] - 841.89).abs() < 1.0,
        "{mb:?}"
    );

    // Bookmarks from the headings: the H1 and the eight H2s.
    assert_eq!(outline_count(&pdf), 9, "outline");

    // External links keep their URL; anchors become internal links.
    let (uris, internal) = links(&pdf);
    for url in [
        "https://example.com",
        "https://autolink.example",
        "mailto:me@example.com",
    ] {
        assert!(uris.contains(url), "{url} in {uris:?}");
    }
    // `[anchor](#tables)`, `[x](#lists)` and two footnote references.
    assert!(internal >= 2, "internal links: {internal}");
    // The relative note link is plain text, never a file link.
    assert!(uris.iter().all(|u| !u.contains("other")), "{uris:?}");

    // One raster image (dot.png, used twice: embedded once); the formulas
    // and the diagram are vector graphics.
    assert_eq!(image_count(&pdf), 1, "images");
    let svgs = doc
        .assets
        .iter()
        .filter(|(p, _)| p.ends_with(".svg"))
        .count();
    assert!(svgs >= 4, "math and diagram assets: {svgs}");
    // Remote, missing and outside images are reported.
    let noted: Vec<&str> = doc.notes.iter().map(|n| n.src.as_str()).collect();
    assert!(noted.contains(&"https://example.com/x.png"), "{noted:?}");
    assert!(noted.contains(&"img/missing.png"), "{noted:?}");
    assert!(noted.contains(&"../outside.png"), "{noted:?}");

    // No script, no launch actions, no dates.
    for d in dictionaries(&pdf) {
        for key in [&b"JS"[..], b"JavaScript", b"Launch", b"OpenAction", b"AA"] {
            assert!(!d.has(key), "{}", String::from_utf8_lossy(key));
        }
        assert!(!d.has(b"CreationDate") && !d.has(b"ModDate"));
    }

    if fonts_installed() {
        let text = text(&pdf);
        for want in [
            "Export fixture",
            "emphasis",
            "strong",
            "The footnote.",
            "Second footnote",
            "\"<b>\"",
            "indented <code>",
            "Careful.",
            "A nested quote.",
            "bold",
        ] {
            assert!(text.contains(want), "{want:?} in:\n{text}");
        }
        // Raw HTML keeps its text, never its script.
        assert!(!text.contains("alert(1)"), "{text}");
    } else {
        eprintln!("no font of the document is installed: text not checked");
    }
}

#[test]
fn the_same_document_gives_the_same_bytes() {
    let (_, a) = fixture(&PageOptions::default());
    let (_, b) = fixture(&PageOptions::default());
    assert_eq!(a.pdf, b.pdf);
}

#[test]
fn page_options_reach_the_pdf() {
    let page = PageOptions {
        paper: Paper::Letter,
        landscape: true,
        ..PageOptions::default()
    };
    let (_, out) = fixture(&page);
    let pdf = Document::load_mem(&out.pdf).expect("PDF");
    let mb = media_box(&pdf);
    assert!(
        (mb[2] - 792.0).abs() < 1.0 && (mb[3] - 612.0).abs() < 1.0,
        "{mb:?}"
    );

    let page = PageOptions {
        size_mm: Some((100.0, 150.0)),
        ..PageOptions::default()
    };
    let (_, out) = fixture(&page);
    let mb = media_box(&Document::load_mem(&out.pdf).expect("PDF"));
    assert!((mb[2] - 283.46).abs() < 1.0, "{mb:?}");
}

#[test]
fn page_breaks_and_the_page_cap() {
    let src = "# One\n\ntext\n\n# Two\n\ntext\n\n# Three\n\ntext\n";
    let page = PageOptions {
        break_before_h1: true,
        ..PageOptions::default()
    };
    let doc = document(src, &fixtures().join("x.md"), &page);
    let out = compile(&doc, 10).expect("compiles");
    assert_eq!(out.pages, 3);
    let err = compile(&doc, 2).expect_err("too many pages");
    assert!(err.contains("more than 2 pages"), "{err}");
}

#[test]
fn typst_in_the_text_is_only_text() {
    let hostile = [
        "#read(\"/etc/passwd\")",
        "#import \"@preview/cetz:0.3.0\": *",
        "\"; #read(\"/etc/passwd\"); \"",
        "#{ while true {} }",
        "@ref = heading // comment /* block <x",
        "#set page(fill: red) #show: none",
        "x^2 \\backslash ] ) } #sys.inputs",
    ];
    let mut src = String::from("# Hostile\n\n");
    for h in hostile {
        src.push_str(&format!("{h}\n\n`{h}`\n\n"));
    }
    src.push_str("```typ\" + read(\"/etc/passwd\") + \"\n#read(\"/etc/passwd\")\n```\n");
    let doc = document(&src, &fixtures().join("x.md"), &PageOptions::default());
    let out = compile(&doc, 10).unwrap_or_else(|e| panic!("{e}\n{}", doc.source));
    if fonts_installed() {
        let text = text(&Document::load_mem(&out.pdf).expect("PDF"));
        let squash = |s: &str| s.split_whitespace().collect::<String>();
        let all = squash(&text);
        for h in hostile {
            assert!(all.contains(&squash(h)), "{h:?} in\n{text}");
        }
        assert!(!text.contains("root:"), "{text}");
    }
}

#[test]
fn a_hostile_source_cannot_read_files() {
    // As if escaping had failed: the World denies every file.
    for source in [
        "#read(\"/etc/passwd\")",
        "#image(\"/etc/hostname\")",
        "#import \"@preview/cetz:0.3.0\": *",
        "#include \"/etc/hostname\"",
    ] {
        let job = PdfJob {
            source: source.to_owned(),
            max_pages: 10,
            ..PdfJob::default()
        };
        let err = gv_helper::engine::pdf(&job).expect_err(source);
        assert!(
            err.contains("denied") || err.contains("not found") || err.contains("failed"),
            "{source}: {err}"
        );
    }
}
