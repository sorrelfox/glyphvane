// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Golden tests, one per Mermaid family the plan lists: a summary line per
//! diagram (exact, as text) and a CPU raster of the display list in a light
//! and a dark test palette (PNG, with a tolerance), so a merman, usvg or
//! font bump that changes output shows up.
//!
//! The fonts are the KaTeX sans-serif faces (bundled with the math engine),
//! so the output does not depend on the machine's fonts.
//!
//! `GV_UPDATE_GOLDEN=1 cargo test -p gv-diagram --test golden` rewrites
//! the files under `tests/golden/`; look at the PNGs before committing them.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;

use gv_diagram::{
    Diagram, DiagramEngine, DiagramFonts, FillRule, Limits, Outcome, Paint, Palette, PathEl, Role,
    Trust,
};
use tiny_skia::{
    Color, FillRule as SkFill, Mask, Paint as SkPaint, PathBuilder, Pixmap, Transform,
};

/// The families of plan 08 / the M0 spike, with their sample file.
const FAMILIES: &[&str] = &[
    "flowchart",
    "sequence",
    "class",
    "state",
    "er",
    "gantt",
    "pie",
    "journey",
    "gitgraph",
    "mindmap",
    "timeline",
    "quadrant",
    "sankey",
    "xychart",
    "block",
    "packet",
    "kanban",
    "architecture",
    "c4",
    "requirement",
];

/// Families whose output keeps literal colours by design: C4 has its own
/// palette, sankey colours links by node, architecture draws its own icons.
const LITERAL_OK: &[&str] = &["c4", "sankey", "architecture"];

pub fn test_fonts() -> DiagramFonts {
    let face = |name: &str| {
        ratex_katex_fonts::ttf_bytes(name)
            .unwrap_or_else(|| panic!("{name} is bundled"))
            .into_owned()
    };
    DiagramFonts {
        family: "KaTeX_SansSerif".into(),
        faces: vec![
            face("KaTeX_SansSerif-Regular.ttf"),
            face("KaTeX_SansSerif-Bold.ttf"),
            face("KaTeX_SansSerif-Italic.ttf"),
        ],
    }
}

fn engine() -> &'static DiagramEngine {
    static ENGINE: OnceLock<DiagramEngine> = OnceLock::new();
    ENGINE.get_or_init(|| DiagramEngine::new(test_fonts(), Limits::default()).expect("engine"))
}

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn update() -> bool {
    std::env::var_os("GV_UPDATE_GOLDEN").is_some_and(|v| v == "1")
}

fn sample(name: &str) -> String {
    std::fs::read_to_string(dir().join("samples").join(format!("{name}.mmd")))
        .unwrap_or_else(|e| panic!("sample {name}: {e}"))
}

fn render(name: &str) -> std::sync::Arc<Diagram> {
    match engine().render(&sample(name), Trust::Interactive) {
        Outcome::Rendered(d) => d,
        other => panic!("{name}: {other:?}"),
    }
}

fn rgba(hex: u32) -> [u8; 4] {
    let [_, r, g, b] = hex.to_be_bytes();
    [r, g, b, 0xFF]
}

/// A light palette from the Light theme tokens, plus test category colours.
fn light() -> Palette {
    Palette::from_fn(|role| match role {
        Role::Background => rgba(0xFDFDFE),
        Role::NodeFill => rgba(0xCCEEEF),
        Role::NodeBorder => rgba(0x007378),
        Role::SecondaryFill => rgba(0xEFF2F4),
        Role::TertiaryFill => rgba(0xF3F5F7),
        Role::Border => rgba(0xC0C5C8),
        Role::Grid => rgba(0xD9DDDF),
        Role::Text => rgba(0x191F23),
        Role::Line => rgba(0x4A5156),
        Role::NoteFill => rgba(0xFDEFCF),
        Role::NoteBorder => rgba(0x8F5300),
        Role::Critical => rgba(0xB51F1F),
        Role::CriticalFill => rgba(0xFFE7E4),
        Role::Category(i) => rgba(
            [
                0x7AA6DA, 0xE6A157, 0x7BC08A, 0xE07B7B, 0xA98BD4, 0xC9A27A, 0xE59BC8, 0x9DA3A8,
                0xC7C25A, 0x6CC2C5, 0xF0C75E, 0x8FB0E8,
            ][usize::from(i % 12)],
        ),
    })
}

/// A dark palette from the Dark theme tokens, plus darker test categories.
fn dark() -> Palette {
    Palette::from_fn(|role| match role {
        Role::Background => rgba(0x1A1E20),
        Role::NodeFill => rgba(0x104042),
        Role::NodeBorder => rgba(0x65D2D2),
        Role::SecondaryFill => rgba(0x121517),
        Role::TertiaryFill => rgba(0x14181A),
        Role::Border => rgba(0x40464A),
        Role::Grid => rgba(0x30363A),
        Role::Text => rgba(0xE5E8EA),
        Role::Line => rgba(0xACB2B5),
        Role::NoteFill => rgba(0x3A2B0D),
        Role::NoteBorder => rgba(0xEBBC63),
        Role::Critical => rgba(0xF08080),
        Role::CriticalFill => rgba(0x4A1A1A),
        Role::Category(i) => rgba(
            [
                0x2F5A8A, 0x8A5A20, 0x2F6E3E, 0x8A3434, 0x5A3F86, 0x6E5434, 0x86416E, 0x50565A,
                0x6E6A1F, 0x206E70, 0x8A6E1F, 0x3F5F96,
            ][usize::from(i % 12)],
        ),
    })
}

fn sk_path(els: &[PathEl]) -> Option<tiny_skia::Path> {
    let mut b = PathBuilder::new();
    for el in els {
        match *el {
            PathEl::MoveTo(x, y) => b.move_to(x, y),
            PathEl::LineTo(x, y) => b.line_to(x, y),
            PathEl::QuadTo(x1, y1, x, y) => b.quad_to(x1, y1, x, y),
            PathEl::CubicTo(x1, y1, x2, y2, x, y) => b.cubic_to(x1, y1, x2, y2, x, y),
            PathEl::Close => b.close(),
        }
    }
    b.finish()
}

/// Paints the display list the way the Reading view does: ground, then
/// every item filled with its resolved colour.
pub fn raster(d: &Diagram, palette: &Palette) -> Pixmap {
    let w = d.width.ceil().max(1.0) as u32;
    let h = d.height.ceil().max(1.0) as u32;
    let mut pm = Pixmap::new(w, h).expect("pixmap");
    let [r, g, b, a] = palette.get(Role::Background);
    pm.fill(Color::from_rgba8(r, g, b, a));
    for item in &d.items {
        let Some(path) = sk_path(item.path.elements()) else {
            continue;
        };
        let [r, g, b, a] = Diagram::resolve(item.paint, palette);
        let mut paint = SkPaint::default();
        let alpha = (f32::from(a) * item.opacity).round().clamp(0.0, 255.0) as u8;
        paint.set_color(Color::from_rgba8(r, g, b, alpha));
        paint.anti_alias = true;
        let rule = match item.rule {
            FillRule::NonZero => SkFill::Winding,
            FillRule::EvenOdd => SkFill::EvenOdd,
        };
        let mask = item.clip.and_then(|c| {
            let mut m = Mask::new(w, h)?;
            let rect = tiny_skia::Rect::from_xywh(c.x, c.y, c.width, c.height)?;
            m.fill_path(
                &PathBuilder::from_rect(rect),
                SkFill::Winding,
                true,
                Transform::identity(),
            );
            Some(m)
        });
        pm.fill_path(&path, &paint, rule, Transform::identity(), mask.as_ref());
    }
    pm
}

/// Share of pixels whose channels differ by more than 48.
fn differing(a: &Pixmap, b: &Pixmap) -> f64 {
    let bad = a
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.data().as_chunks::<4>().0)
        .filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 48))
        .count();
    bad as f64 / f64::from((a.width() * a.height()).max(1))
}

#[test]
fn every_family_renders_and_matches_the_summary() {
    let mut out = String::new();
    let mut unmapped = Vec::new();
    for name in FAMILIES {
        let d = render(name);
        let roles = {
            let mut r: Vec<Role> = d
                .items
                .iter()
                .filter_map(|i| match i.paint {
                    Paint::Role(r) => Some(r),
                    Paint::Rgb(_) => None,
                })
                .collect();
            r.sort();
            r.dedup();
            r.len()
        };
        let literals = d.literal_colors();
        let _ = writeln!(
            out,
            "{name:<13} family={:<14} size={}x{} items={} roles={roles} literals={}",
            d.family,
            d.width.round(),
            d.height.round(),
            d.items.len(),
            literals.len(),
        );
        if !LITERAL_OK.contains(name) && !literals.is_empty() {
            unmapped.push(format!(
                "{name}: {:?}",
                literals
                    .iter()
                    .map(|[r, g, b]| format!("#{r:02x}{g:02x}{b:02x}"))
                    .collect::<Vec<_>>()
            ));
        }
        assert!(d.items.len() > 3, "{name}: almost nothing drawn");
    }
    assert!(
        unmapped.is_empty(),
        "colours not mapped to a role:\n{}",
        unmapped.join("\n")
    );
    let file = dir().join("golden").join("summary.txt");
    if update() {
        std::fs::create_dir_all(file.parent().expect("dir")).expect("mkdir");
        std::fs::write(&file, &out).expect("write summary");
        return;
    }
    let want =
        std::fs::read_to_string(&file).expect("tests/golden/summary.txt (GV_UPDATE_GOLDEN=1)");
    assert_eq!(
        out, want,
        "diagram summaries changed; rerun with GV_UPDATE_GOLDEN=1 and review"
    );
}

#[test]
fn rasters_match_the_golden_images() {
    let mut failures = Vec::new();
    for name in FAMILIES {
        let d = render(name);
        for (scheme, palette) in [("light", light()), ("dark", dark())] {
            let pm = raster(&d, &palette);
            let file = dir().join("golden").join(format!("{name}-{scheme}.png"));
            if update() {
                std::fs::create_dir_all(file.parent().expect("dir")).expect("mkdir");
                pm.save_png(&file).expect("save png");
                continue;
            }
            let want = Pixmap::load_png(&file)
                .unwrap_or_else(|e| panic!("{}: {e} (GV_UPDATE_GOLDEN=1)", file.display()));
            if (want.width(), want.height()) != (pm.width(), pm.height()) {
                failures.push(format!(
                    "{name}-{scheme}: size {}x{} != golden {}x{}",
                    pm.width(),
                    pm.height(),
                    want.width(),
                    want.height()
                ));
                continue;
            }
            let share = differing(&pm, &want);
            if share > 0.01 {
                failures.push(format!(
                    "{name}-{scheme}: {:.2} % of pixels differ",
                    share * 100.0
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn themed_svg_export_parses() {
    let d = render("flowchart");
    let svg = d.to_svg(&light(), true);
    assert!(svg.starts_with("<svg"));
    assert!(svg.contains("#007378"), "node border resolved to the token");
    assert!(!svg.contains("<text"), "text is exported as outlines");
    let tree = usvg_parse(&svg);
    assert!(tree > 0);
}

fn usvg_parse(svg: &str) -> usize {
    // A cheap well-formedness check without another SVG parser dependency.
    let opens = svg.matches("<path").count();
    assert_eq!(svg.matches("<svg").count(), 1);
    assert!(svg.ends_with("</svg>"));
    opens
}

#[test]
#[ignore = "diagnostic: dumps merman's SVG for GV_DUMP=<family> into the target dir"]
fn dump_svg() {
    let name = std::env::var("GV_DUMP").unwrap_or_else(|_| "er".into());
    let svg = engine().debug_svg(&sample(&name)).expect("svg");
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.svg"));
    std::fs::write(&out, svg).expect("write");
    eprintln!("wrote {}", out.display());
}
