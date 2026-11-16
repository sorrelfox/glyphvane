// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Decodes an image or SVG and prints what came out, without a window.
//!
//! `cargo run -p gv-media --example media -- FILE [MAX_W MAX_H]`
//!
//! Also the binary-size probe for the media engine.

use std::path::Path;
use std::time::Instant;

use gv_media::{CancelFlag, DecodeRequest, Limits, MediaEngine, SvgContext};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(file) = args.first() else {
        eprintln!("usage: media FILE [MAX_W MAX_H]");
        std::process::exit(2);
    };
    let fit = match (args.get(1), args.get(2)) {
        (Some(w), Some(h)) => w.parse().ok().zip(h.parse().ok()),
        _ => None,
    };
    let engine = MediaEngine::new(Limits::default(), SvgContext::default());
    let req = DecodeRequest { fit, animate: true };
    let t = Instant::now();
    match engine.load_path(Path::new(file), req, &CancelFlag::new()) {
        Ok(img) => {
            let ms = t.elapsed().as_secs_f64() * 1e3;
            let f = &img.frames[0];
            println!(
                "{}: {}x{} -> {}x{}, {} frame(s){}, {} KB, {ms:.1} ms",
                img.format.name(),
                img.source_width,
                img.source_height,
                f.width,
                f.height,
                img.frames.len(),
                if img.truncated { " (cut)" } else { "" },
                img.bytes() / 1024
            );
        }
        Err(e) => {
            eprintln!("{}", e.card_message());
            std::process::exit(1);
        }
    }
}
