// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The helper's bundled fonts and its page raster, in process: a PDF is
//! set in the bundled Inter and JetBrains Mono whatever is installed, and
//! its first page is drawn to fit a box for the export dialog's preview.

use std::collections::BTreeSet;

use gv_export::{ExportOptions, ExportTheme, PageOptions, Paper, export_typst};
use gv_helper::{PageJob, PdfJob, PdfOutput};
use gv_model::DocModel;
use gv_profile::ProfileId;
use gv_render_tree::{AllPending, NoImageSizes, RenderOptions, RenderTree, Sources};
use gv_syntax::ParseOptions;
use lopdf::{Document, Object};

const DOC: &str = "# Bundled fonts\n\nBody text in *italic* and **bold**, \
                   with `inline code`.\n\n```rust\nfn main() {}\n```\n";

fn compile(src: &str, page: &PageOptions) -> PdfOutput {
    let id = ProfileId::Extended;
    let syntax = gv_syntax::parse_with(src, ParseOptions::builtin(id));
    let model = DocModel::build(&syntax, src);
    let tree = RenderTree::build(
        &syntax,
        &model,
        src,
        RenderOptions::builtin(id),
        Sources {
            links: &AllPending,
            images: &NoImageSizes,
        },
    );
    let opts = ExportOptions::new(None, ExportTheme::from_fn(|_| Some([40, 40, 40, 255])));
    let doc = export_typst(&tree, &opts, page);
    gv_helper::engine::pdf(&PdfJob {
        source: doc.source,
        assets: doc.assets,
        fonts: doc.fonts,
        max_pages: 10,
        ident: doc.ident,
    })
    .expect("compiles")
}

/// The base names of the fonts the PDF embeds, without subset tags.
fn embedded_fonts(pdf: &[u8]) -> BTreeSet<String> {
    let doc = Document::load_mem(pdf).expect("a PDF");
    doc.objects
        .values()
        .filter_map(|o| match o {
            Object::Dictionary(d) => Some(d),
            Object::Stream(s) => Some(&s.dict),
            _ => None,
        })
        .filter_map(|d| d.get(b"BaseFont").ok()?.as_name().ok())
        .map(|n| {
            let n = String::from_utf8_lossy(n).into_owned();
            // `ABCDEF+Name` → `Name`.
            match n.split_once('+') {
                Some((_, name)) => name.to_owned(),
                None => n,
            }
        })
        .collect()
}

#[test]
fn the_bundled_faces_are_found() {
    let families = gv_helper::engine::bundled_families();
    assert!(
        families.contains(&"inter variable".to_owned()),
        "{families:?}"
    );
    assert!(
        families.contains(&"jetbrains mono".to_owned()),
        "{families:?}"
    );
}

#[test]
fn text_and_code_are_set_in_the_bundled_faces() {
    let out = compile(DOC, &PageOptions::default());
    let fonts = embedded_fonts(&out.pdf);
    assert!(
        fonts.iter().any(|f| f.starts_with("InterVariable")),
        "{fonts:?}"
    );
    assert!(
        fonts.iter().any(|f| f.starts_with("JetBrainsMono")),
        "{fonts:?}"
    );
    // Nothing else is needed for Latin text and code.
    assert!(
        fonts
            .iter()
            .all(|f| f.starts_with("InterVariable") || f.starts_with("JetBrainsMono")),
        "{fonts:?}"
    );
    // The same note gives the same bytes.
    assert_eq!(compile(DOC, &PageOptions::default()).pdf, out.pdf);
}

#[test]
fn the_first_page_is_drawn_to_fit_a_box() {
    let out = compile(DOC, &PageOptions::default());
    let image = gv_helper::raster::page(&PageJob {
        pdf: out.pdf.clone(),
        page: 0,
        max_width: 336,
        max_height: 476,
    })
    .expect("drawn");
    // A4 (210 × 297) in a 336 × 476 box: 336 wide.
    assert_eq!(image.width, 336);
    assert!((474..=476).contains(&image.height), "{}", image.height);
    assert_eq!(image.rgba.len(), (image.width * image.height * 4) as usize);
    assert_eq!(image.pages, 1);
    // White paper with dark text on it.
    let dark = image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] < 128 && p[1] < 128 && p[2] < 128)
        .count();
    let white = image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > 250 && p[1] > 250 && p[2] > 250)
        .count();
    assert!(dark > 100, "text is drawn ({dark} dark pixels)");
    assert!(white > image.rgba.len() / 4 / 2, "mostly paper");

    // Landscape Letter in the same box: limited by its width.
    let page = PageOptions {
        paper: Paper::Letter,
        landscape: true,
        ..PageOptions::default()
    };
    let wide = gv_helper::raster::page(&PageJob {
        pdf: compile(DOC, &page).pdf,
        page: 0,
        max_width: 336,
        max_height: 476,
    })
    .expect("drawn");
    assert_eq!(wide.width, 336);
    assert!(wide.height < 300, "{}", wide.height);

    // A page that is not there.
    let missing = gv_helper::raster::page(&PageJob {
        pdf: out.pdf,
        page: 3,
        max_width: 100,
        max_height: 100,
    });
    assert!(missing.is_err());
}

/// The dark pixels of `src`'s first page drawn 1000 px wide.
fn ink(src: &str) -> usize {
    let image = gv_helper::raster::page(&PageJob {
        pdf: compile(src, &PageOptions::default()).pdf,
        page: 0,
        max_width: 1000,
        max_height: 1500,
    })
    .expect("drawn");
    image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] < 128 && p[1] < 128 && p[2] < 128)
        .count()
}

#[test]
fn bold_text_is_heavier_than_regular() {
    // The bundled Inter is a variable font: **bold** must reach the bold
    // instance, not the default (regular) one.
    let words = "Heavy strokes make bold words stand out ".repeat(6);
    let regular = ink(&format!("{words}\n"));
    let bold = ink(&format!("**{}**\n", words.trim_end()));
    assert!(regular > 1000, "text is drawn ({regular} dark pixels)");
    assert!(
        bold * 10 > regular * 11,
        "bold ({bold} dark pixels) should be heavier than regular ({regular})"
    );
}
