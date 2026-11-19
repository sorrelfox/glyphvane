// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! PDF pages drawn by the real `glyphvane-helper`: a PDF opened once and
//! drawn page by page at a scale (the PDF viewer), the text of its pages,
//! the caps on images and pages, unreadable and encrypted files, and the
//! deadline. The PDFs are written by the tests themselves ([`tiny_pdf`],
//! [`text_pdf`]).

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use gv_helper::protocol::{MAX_RENDER_PIXELS, MAX_RENDER_SIDE};
use gv_helper::{Helper, HelperConfig, HelperError, PageJob, RenderJob, TextJob};

fn helper() -> Helper {
    Helper::new(HelperConfig::new(env!("CARGO_BIN_EXE_glyphvane-helper")))
}

fn never() -> AtomicBool {
    AtomicBool::new(false)
}

const DEADLINE: Duration = Duration::from_secs(30);

/// What else a test PDF has.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Extra {
    None,
    /// Every page draws a 30,000 × 30,000 image (with no data: the
    /// pre-scan refuses it before anything decodes it).
    ImageBomb,
    /// A standard security handler whose empty user password is wrong.
    Encrypted,
}

/// A PDF of pages `sizes` (points), each with a blue 50 × 50 pt square
/// whose lower-left corner is 10 pt from the page's.
fn tiny_pdf(sizes: &[(f32, f32)], extra: Extra) -> Vec<u8> {
    let mut objects: Vec<String> = Vec::new();
    let n = sizes.len();
    // 1: catalog, 2: pages, then a page and its content per page, then
    // the image.
    let image_id = 3 + 2 * n;
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + 2 * i)).collect();
    objects.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {n} >>",
        kids.join(" ")
    ));
    for (i, (w, h)) in sizes.iter().enumerate() {
        let resources = if extra == Extra::ImageBomb {
            format!("<< /XObject << /Im0 {image_id} 0 R >> >>")
        } else {
            "<< >>".to_owned()
        };
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents {} 0 R /Resources {resources} >>",
            4 + 2 * i
        ));
        let mut content = "0 0 1 rg 10 10 50 50 re f".to_owned();
        if extra == Extra::ImageBomb {
            content.push_str(" q 100 0 0 100 0 0 cm /Im0 Do Q");
        }
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
    if extra == Extra::ImageBomb {
        objects.push(
            "<< /Type /XObject /Subtype /Image /Width 30000 /Height 30000 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 0 >>\nstream\n\nendstream"
                .into(),
        );
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    let encrypt = if extra == Extra::Encrypted {
        let o = "4".repeat(64);
        let u = "7".repeat(64);
        format!(
            " /Encrypt << /Filter /Standard /V 1 /R 2 /O <{o}> /U <{u}> /P -4 >> /ID [<0123456789abcdef0123456789abcdef> <0123456789abcdef0123456789abcdef>]"
        )
    } else {
        String::new()
    };
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R{encrypt} >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// Writes `objects` (numbered from 1; the first is the catalog) as a PDF.
fn write_pdf(objects: &[String]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// A Letter-size PDF with one page per entry of `pages`, each line of it
/// set at 12 pt from 72 pt from the left, the first baseline 92 pt from
/// the top and 20 pt between baselines, in a Type 3 font (defined in the
/// file, so no font has to be installed) whose glyphs are 6 pt wide
/// (500 units) and draw nothing; its glyph names give the text. Letters
/// and spaces only.
fn text_pdf(pages: &[&[&str]]) -> Vec<u8> {
    let n = pages.len();
    // 1: catalog, 2: pages, 3: font, 4: the glyph procedure, then a page
    // and its content per page.
    let mut objects: Vec<String> = vec!["<< /Type /Catalog /Pages 2 0 R >>".into()];
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 5 + 2 * i)).collect();
    objects.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {n} >>",
        kids.join(" ")
    ));
    let upper: Vec<String> = ('A'..='Z').map(|c| format!("/{c}")).collect();
    let lower: Vec<String> = ('a'..='z').map(|c| format!("/{c}")).collect();
    let procs: Vec<String> = std::iter::once("/space 4 0 R".to_owned())
        .chain(('A'..='Z').chain('a'..='z').map(|c| format!("/{c} 4 0 R")))
        .collect();
    let widths = vec!["500"; 122 - 32 + 1].join(" ");
    objects.push(format!(
        "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 500 700] /FontMatrix [0.001 0 0 0.001 0 0] \
         /CharProcs << {} >> /Encoding << /Type /Encoding /Differences [32 /space 65 {} 97 {}] >> \
         /FirstChar 32 /LastChar 122 /Widths [{widths}] /Resources << >> >>",
        procs.join(" "),
        upper.join(" "),
        lower.join(" ")
    ));
    let glyph = "500 0 d0";
    objects.push(format!(
        "<< /Length {} >>\nstream\n{glyph}\nendstream",
        glyph.len()
    ));
    for (i, lines) in pages.iter().enumerate() {
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R \
             /Resources << /Font << /F1 3 0 R >> >> >>",
            6 + 2 * i
        ));
        let mut content = "BT /F1 12 Tf 72 700 Td".to_owned();
        for (k, line) in lines.iter().enumerate() {
            if k > 0 {
                content.push_str(" 0 -20 Td");
            }
            content.push_str(&format!(" ({line}) Tj"));
        }
        content.push_str(" ET");
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
    write_pdf(&objects)
}

fn render(key: u64, page: u32, scale: f32) -> RenderJob {
    RenderJob {
        key,
        page,
        scale,
        max_pixels: MAX_RENDER_PIXELS,
    }
}

/// The RGBA pixel at (`x`, `y`) of an image `width` wide.
fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

#[test]
fn a_pdf_opens_once_and_draws_page_by_page_at_a_scale() {
    let helper = helper();
    let pdf = tiny_pdf(
        &[(200.0, 100.0), (100.0, 300.0), (612.0, 792.0)],
        Extra::None,
    );
    let info = helper.open(1, pdf, DEADLINE, &never()).expect("opens");
    let sizes: Vec<(f32, f32)> = info.pages.iter().map(|p| (p.width, p.height)).collect();
    assert_eq!(sizes, [(200.0, 100.0), (100.0, 300.0), (612.0, 792.0)]);

    // Page 1 at 2 px per point: 400 × 200, the square blue, the rest white.
    let image = helper
        .render(render(1, 0, 2.0), DEADLINE, &never())
        .expect("drawn")
        .expect("the helper holds the PDF");
    assert_eq!((image.width, image.height, image.pages), (400, 200, 3));
    assert_eq!(image.rgba.len(), 400 * 200 * 4);
    // 10 pt from the bottom-left: the square spans x 20..120, y 80..180.
    assert_eq!(pixel(&image.rgba, 400, 70, 130), [0, 0, 255, 255]);
    assert_eq!(pixel(&image.rgba, 400, 300, 20), [255, 255, 255, 255]);

    // Page 2 from the same session, at another scale.
    let pid = helper.pid();
    let image = helper
        .render(render(1, 1, 0.5), DEADLINE, &never())
        .expect("drawn")
        .expect("held");
    assert_eq!((image.width, image.height), (50, 150));
    assert_eq!(helper.pid(), pid, "the same helper");

    // No such page: an error, and the helper keeps the PDF.
    let err = helper
        .render(render(1, 9, 1.0), DEADLINE, &never())
        .expect_err("no page 10");
    assert!(
        matches!(&err, HelperError::Failed(m) if m.contains("no page 10")),
        "{err}"
    );
    assert!(
        helper
            .render(render(1, 2, 1.0), DEADLINE, &never())
            .expect("drawn")
            .is_some()
    );

    // Another key is not held: open it again.
    assert!(
        helper
            .render(render(2, 0, 1.0), DEADLINE, &never())
            .expect("answered")
            .is_none()
    );
    helper.shutdown();
}

#[test]
fn the_text_of_a_page_comes_with_each_characters_box() {
    let helper = helper();
    let pdf = text_pdf(&[&["Hello world", "Second line"], &["Page two"]]);
    helper.open(11, pdf, DEADLINE, &never()).expect("opens");
    let text = helper
        .text(TextJob { key: 11, page: 0 }, DEADLINE, &never())
        .expect("read")
        .expect("the helper holds the PDF");
    assert_eq!(text.text, "Hello world\nSecond line");
    assert_eq!(text.boxes.len(), text.text.chars().count());
    // "H": 72 pt from the left, 6 pt wide; the baseline 92 pt from the
    // top, the box from 0.8 em above it to 0.2 em below.
    let h = text.boxes[0];
    let near = |a: f32, b: f32| (a - b).abs() < 0.01;
    assert!(near(h.x0, 72.0) && near(h.x1, 78.0), "{h:?}");
    assert!(near(h.y0, 92.0 - 9.6) && near(h.y1, 92.0 + 2.4), "{h:?}");
    // "S" starts the second line, 20 pt lower.
    let s = text.boxes[12];
    assert!(near(s.x0, 72.0) && near(s.y0, 112.0 - 9.6), "{s:?}");

    // Another page of the same session, while it also draws.
    let pid = helper.pid();
    assert!(
        helper
            .render(render(11, 1, 1.0), DEADLINE, &never())
            .expect("drawn")
            .is_some()
    );
    let two = helper
        .text(TextJob { key: 11, page: 1 }, DEADLINE, &never())
        .expect("read")
        .expect("held");
    assert_eq!(two.text, "Page two");
    assert_eq!(helper.pid(), pid, "the same helper");

    // No such page: an error; another key is not held.
    let err = helper
        .text(TextJob { key: 11, page: 5 }, DEADLINE, &never())
        .expect_err("no page 6");
    assert!(
        matches!(&err, HelperError::Failed(m) if m.contains("no page 6")),
        "{err}"
    );
    assert!(
        helper
            .text(TextJob { key: 12, page: 0 }, DEADLINE, &never())
            .expect("answered")
            .is_none()
    );
    // A page without text has none.
    let square = tiny_pdf(&[(100.0, 100.0)], Extra::None);
    helper.open(13, square, DEADLINE, &never()).expect("opens");
    let empty = helper
        .text(TextJob { key: 13, page: 0 }, DEADLINE, &never())
        .expect("read")
        .expect("held");
    assert!(empty.text.is_empty() && empty.boxes.is_empty());
    helper.shutdown();
    // A new helper holds nothing.
    assert!(
        helper
            .text(TextJob { key: 13, page: 0 }, DEADLINE, &never())
            .expect("answered")
            .is_none()
    );
    helper.shutdown();
}

#[test]
fn another_job_ends_the_session_and_a_new_helper_holds_nothing() {
    let helper = helper();
    let pdf = tiny_pdf(&[(100.0, 100.0)], Extra::None);
    helper
        .open(7, pdf.clone(), DEADLINE, &never())
        .expect("opens");
    // A preview of another PDF in between.
    let preview = helper
        .page(
            PageJob {
                pdf: pdf.clone(),
                page: 0,
                max_width: 64,
                max_height: 64,
            },
            DEADLINE,
            &never(),
        )
        .expect("a preview");
    assert_eq!((preview.width, preview.height), (64, 64));
    assert!(
        helper
            .render(render(7, 0, 1.0), DEADLINE, &never())
            .expect("answered")
            .is_none(),
        "the session ended"
    );
    // Opened again, it draws.
    helper.open(7, pdf, DEADLINE, &never()).expect("opens");
    assert!(
        helper
            .render(render(7, 0, 1.0), DEADLINE, &never())
            .expect("drawn")
            .is_some()
    );
    // A new helper (after a stop) holds nothing.
    helper.shutdown();
    assert!(
        helper
            .render(render(7, 0, 1.0), DEADLINE, &never())
            .expect("answered")
            .is_none()
    );
    helper.shutdown();
}

#[test]
fn page_images_stay_within_the_caps() {
    let helper = helper();
    let pdf = tiny_pdf(&[(14_400.0, 14_400.0), (100.0, 14_400.0)], Extra::None);
    helper.open(3, pdf, DEADLINE, &never()).expect("opens");
    let big = helper
        .render(render(3, 0, 8.0), DEADLINE, &never())
        .expect("drawn")
        .expect("held");
    assert!(u64::from(big.width) * u64::from(big.height) <= MAX_RENDER_PIXELS);
    assert_eq!(big.width, big.height, "in proportion");
    let tall = helper
        .render(render(3, 1, 4.0), DEADLINE, &never())
        .expect("drawn")
        .expect("held");
    assert!(tall.height <= MAX_RENDER_SIDE, "{}", tall.height);
    let small = helper
        .render(
            RenderJob {
                max_pixels: 10_000,
                ..render(3, 0, 1.0)
            },
            DEADLINE,
            &never(),
        )
        .expect("drawn")
        .expect("held");
    assert!(u64::from(small.width) * u64::from(small.height) <= 10_000);
    helper.shutdown();
}

#[test]
fn hostile_and_unreadable_pdfs_are_refused_with_a_reason() {
    let helper = helper();
    // An image that would decode to 2.7 GB: refused before it is drawn.
    let bomb = tiny_pdf(&[(100.0, 100.0)], Extra::ImageBomb);
    helper
        .open(4, bomb.clone(), DEADLINE, &never())
        .expect("opens");
    let err = helper
        .render(render(4, 0, 1.0), DEADLINE, &never())
        .expect_err("refused");
    assert!(
        matches!(&err, HelperError::Failed(m) if m.contains("image too large")),
        "{err}"
    );
    let err = helper
        .page(
            PageJob {
                pdf: bomb,
                page: 0,
                max_width: 100,
                max_height: 100,
            },
            DEADLINE,
            &never(),
        )
        .expect_err("refused too");
    assert!(
        matches!(&err, HelperError::Failed(m) if m.contains("image too large")),
        "{err}"
    );

    let err = helper
        .open(5, b"%PDF-1.7\nnothing here".to_vec(), DEADLINE, &never())
        .expect_err("unreadable");
    assert!(
        matches!(&err, HelperError::Failed(m) if m.contains("not a readable PDF")),
        "{err}"
    );
    let locked = tiny_pdf(&[(100.0, 100.0)], Extra::Encrypted);
    let err = helper
        .open(6, locked, DEADLINE, &never())
        .expect_err("encrypted");
    assert!(
        matches!(&err, HelperError::Failed(m) if m.contains("password") || m.contains("encrypted")),
        "{err}"
    );
    helper.shutdown();
}

#[test]
fn a_page_past_its_deadline_stops_the_helper_and_the_next_one_reopens() {
    let helper = helper();
    let sizes = vec![(612.0, 792.0); 2];
    let pdf = tiny_pdf(&sizes, Extra::None);
    helper
        .open(8, pdf.clone(), DEADLINE, &never())
        .expect("opens");
    let first = helper.pid().expect("running");
    // No page draws in a microsecond.
    let err = helper
        .render(render(8, 0, 4.0), Duration::from_micros(1), &never())
        .expect_err("late");
    assert!(matches!(err, HelperError::Timeout(_)), "{err}");
    assert_eq!(helper.pid(), None, "stopped");
    // The next helper does not hold the PDF until it is opened again.
    assert!(
        helper
            .render(render(8, 1, 1.0), DEADLINE, &never())
            .expect("answered")
            .is_none()
    );
    assert_ne!(helper.pid(), Some(first));
    helper.open(8, pdf, DEADLINE, &never()).expect("opens");
    assert!(
        helper
            .render(render(8, 1, 1.0), DEADLINE, &never())
            .expect("drawn")
            .is_some()
    );
    // Cancelled before it starts: nothing runs.
    let cancel = AtomicBool::new(true);
    let err = helper
        .render(render(8, 0, 1.0), DEADLINE, &cancel)
        .expect_err("cancelled");
    assert!(matches!(err, HelperError::Cancelled), "{err}");
    helper.shutdown();
}
