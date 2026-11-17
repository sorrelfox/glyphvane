// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Wall-clock timing of the scan over generated in-memory notes
//! (NFR-PERF-11: first results within 100 ms on 10k notes). Not a gate:
//! it prints numbers. `GV_SEARCH_NOTES` sets the note count (default
//! 10,000).
//!
//! ```sh
//! cargo bench -p gv-search --profile measure --bench scan
//! ```

#![allow(clippy::unwrap_used)]

use std::time::Instant;

use gv_profile::ProfileId;
use gv_search::{CancelToken, MemorySource, Query, SearchOptions, search};

fn note(i: usize) -> String {
    let mut s = format!(
        "---\nstatus: {}\nprio: {}\n---\n# Note {i}\n\n",
        if i.is_multiple_of(3) { "done" } else { "draft" },
        i % 10
    );
    for p in 0..12 {
        s.push_str(&format!(
            "Paragraph {p} of note {i} talks about lorem ipsum dolor sit amet, \
             consectetur adipiscing elit, sed do eiusmod tempor #topic/{}.\n\n",
            (i + p) % 50
        ));
        if (i + p).is_multiple_of(997) {
            s.push_str("A rare needle appears here.\n\n");
        }
    }
    s.push_str("- [ ] follow up\n- [x] done item\n");
    s
}

fn main() {
    // `cargo bench` passes `--bench`; ignore arguments.
    let n: usize = std::env::var("GV_SEARCH_NOTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000);
    let t = Instant::now();
    let mut src = MemorySource::new();
    let mut bytes = 0;
    for i in 0..n {
        let text = note(i);
        bytes += text.len();
        src.add(
            &format!("folder{}/note {i}.md", i % 40),
            &text,
            ProfileId::Obsidian,
        );
    }
    println!(
        "setup: {n} notes, {:.1} MB, indexed in {:?}",
        bytes as f64 / 1e6,
        t.elapsed()
    );
    let opts = SearchOptions::default();
    for q in [
        "needle",
        "lorem ipsum",
        "\"rare needle\"",
        "/ne+dle/",
        "line:(rare appears)",
        "tag:#topic/7",
        "[status:done] [prio:>5]",
        "file:\"note 99\"",
        "task-todo:follow",
        "zzzznothing",
    ] {
        let query = Query::new(q, false).unwrap();
        let mut runs = Vec::new();
        let mut first = Vec::new();
        let mut matched = 0;
        for _ in 0..5 {
            let s = search(&query, &src, &opts, &CancelToken::new(), None, |_| {});
            runs.push(s.elapsed);
            if let Some(f) = s.first_batch {
                first.push(f);
            }
            matched = s.files_matched;
        }
        runs.sort();
        first.sort();
        println!(
            "{q:<28} matched {matched:>5}  median {:>10.2?}  first batch {:>10.2?}",
            runs[runs.len() / 2],
            first.get(first.len() / 2).copied().unwrap_or_default()
        );
    }
}
