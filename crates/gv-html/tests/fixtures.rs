// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Representative clipboard captures (hand-written in the shape that
//! Chrome, Firefox, Google Docs, Word, GitHub and Windows CF_HTML produce)
//! against expected Markdown, per profile.
//!
//! Each `fixtures/NAME.html` is compared with every `fixtures/NAME.PROFILE.md`
//! next to it. Set `GV_HTML_BLESS=1` to rewrite the expected files.

use std::path::Path;

use gv_html::{PasteOptions, Profile, html_to_markdown};

const PROFILES: &[Profile] = &[
    Profile::CommonMark,
    Profile::GfmFile,
    Profile::GfmComment,
    Profile::Obsidian,
    Profile::Extended,
];

#[test]
fn clipboard_fixtures() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let bless = std::env::var_os("GV_HTML_BLESS").is_some();
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .expect("fixtures directory")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "html"))
        .collect();
    names.sort();
    assert!(!names.is_empty());
    let mut failures = Vec::new();
    let mut compared = 0;
    for html_path in names {
        let html = std::fs::read_to_string(&html_path).expect("read fixture");
        let stem = html_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        for &profile in PROFILES {
            let md_path = dir.join(format!("{stem}.{}.md", profile.id()));
            let got = html_to_markdown(&html, PasteOptions::new(profile)) + "\n";
            if bless && (md_path.exists() || matches!(profile, Profile::GfmFile)) {
                std::fs::write(&md_path, &got).expect("write expected");
                continue;
            }
            let Ok(want) = std::fs::read_to_string(&md_path) else {
                continue;
            };
            compared += 1;
            if got != want {
                failures.push(format!(
                    "{}:\n--- want\n{want}--- got\n{got}",
                    md_path.display()
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(bless || compared > 0);
}
