// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Renders a Mermaid file to a themed SVG, without a window.
//!
//! `cargo run -p gv-diagram --example diagram -- FONT.ttf INPUT.mmd [OUT.svg]`
//!
//! The font is the face labels are measured and drawn with. Also the
//! binary-size probe for the diagram engine.

use std::time::Instant;

use gv_diagram::{DiagramEngine, DiagramFonts, Limits, Outcome, Palette, Trust};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(font), Some(input)) = (args.first(), args.get(1)) else {
        eprintln!("usage: diagram FONT.ttf INPUT.mmd [OUT.svg]");
        std::process::exit(2);
    };
    let face = std::fs::read(font).unwrap_or_else(|e| {
        eprintln!("{font}: {e}");
        std::process::exit(1);
    });
    let family = std::path::Path::new(font)
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.split('-').next())
        .unwrap_or("sans-serif")
        .to_owned();
    let source = std::fs::read_to_string(input).unwrap_or_else(|e| {
        eprintln!("{input}: {e}");
        std::process::exit(1);
    });
    let engine = match DiagramEngine::new(
        DiagramFonts {
            family,
            faces: vec![face],
        },
        Limits::default(),
    ) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let t = Instant::now();
    let outcome = engine.render(&source, Trust::Interactive);
    let ms = t.elapsed().as_secs_f64() * 1e3;
    match outcome {
        Outcome::Rendered(d) => {
            eprintln!(
                "{}: {}x{} px, {} items, {ms:.1} ms",
                d.family,
                d.width.round(),
                d.height.round(),
                d.items.len()
            );
            let svg = d.to_svg(&Palette::markers(), true);
            match args.get(2) {
                Some(out) => {
                    if let Err(e) = std::fs::write(out, svg) {
                        eprintln!("{out}: {e}");
                        std::process::exit(1);
                    }
                }
                None => println!("{svg}"),
            }
        }
        Outcome::Card(c) => {
            eprintln!("{}: {}", c.title(), c.message);
            std::process::exit(1);
        }
        Outcome::NotRecognised => {
            eprintln!("{}", gv_diagram::NOT_RECOGNISED_NOTE);
            std::process::exit(1);
        }
        Outcome::Cancelled => std::process::exit(1),
    }
}
