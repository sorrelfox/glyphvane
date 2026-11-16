// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Highlights a file and prints its spans, or times pathological inputs.
//!
//! ```text
//! cargo run -p gv-highlight --example highlight -- LANG FILE
//! cargo run -p gv-highlight --example highlight -- --pathological
//! ```

use std::time::Instant;

use gv_highlight::{CodeHighlighter, GialloHighlighter, Limits};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--pathological" => pathological(),
        [lang, file] => {
            let code = match std::fs::read_to_string(file) {
                Ok(code) => code,
                Err(err) => {
                    eprintln!("{file}: {err}");
                    std::process::exit(1);
                }
            };
            let start = Instant::now();
            let h = CodeHighlighter::shared().highlight(&code, lang);
            let took = start.elapsed();
            for s in &h.spans {
                println!(
                    "{}..{} {} {:?}",
                    s.start,
                    s.end,
                    s.class.name(),
                    &code[s.start..s.end]
                );
            }
            eprintln!("{:?}: {} spans in {took:?}", h.outcome, h.spans.len());
        }
        _ => eprintln!("usage: highlight LANG FILE | --pathological"),
    }
}

fn pathological() {
    let no_caps = Limits::UNLIMITED;
    for n in [1_000, 4_000, 16_000, 64_000] {
        let cases: Vec<(&str, &str, String)> = vec![
            (
                "long line js",
                "js",
                format!("var a = {};\n", "a+".repeat(n)),
            ),
            (
                "nested parens js",
                "js",
                format!("{}{}\n", "(".repeat(n), ")".repeat(n)),
            ),
            (
                "unterminated string py",
                "python",
                format!("x = \"{}\n", "a\\".repeat(n)),
            ),
            ("unclosed comments c", "c", "x /* y\n".repeat(n / 4)),
            (
                "spaces then tab sh",
                "bash",
                format!("{}\t\n", " ".repeat(n)),
            ),
            ("brackets yaml", "yaml", format!("a: {}\n", "[".repeat(n))),
            (
                "emphasis md",
                "markdown",
                format!("{}\n", "*a ".repeat(n / 3)),
            ),
            (
                "underscores md",
                "markdown",
                format!("{}\n", "_a ".repeat(n / 3)),
            ),
            (
                "backticks md",
                "markdown",
                format!("{}\n", "`a ".repeat(n / 3)),
            ),
            (
                "links md",
                "markdown",
                format!("{}\n", "[a](".repeat(n / 4)),
            ),
            ("tags html", "html", format!("{}\n", "<a ".repeat(n / 3))),
        ];
        for (name, lang, code) in cases {
            let hl = CodeHighlighter::with_engine(GialloHighlighter::new(), no_caps, 1 << 20);
            hl.preload(lang);
            let start = Instant::now();
            let h = hl.highlight(&code, lang);
            println!(
                "{n}\t{name}\t{} bytes\t{:.1} ms\t{} spans",
                code.len(),
                start.elapsed().as_secs_f64() * 1000.0,
                h.spans.len()
            );
        }
    }
}
