// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Shared helpers for the gv-syntax integration tests: spec fixtures, the
//! comrak oracle, random Markdown documents and test-case minimization.

// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

pub mod comrak_outline;

use std::path::PathBuf;

/// The workspace root (two levels above this crate).
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// One spec example from `tests/fixtures/spec/*.json`.
#[derive(Debug, Clone)]
pub struct Example {
    pub number: u64,
    pub section: String,
    pub markdown: String,
    pub html: String,
    pub extensions: Vec<String>,
}

pub const COMMONMARK_SPEC: &str = "commonmark-0.31.2.json";
pub const GFM_SPEC: &str = "gfm-0.29.0.gfm.13.json";

/// Loads a spec fixture from `tests/fixtures/spec/`.
pub fn load_spec(file: &str) -> Vec<Example> {
    let path = workspace_root().join("tests/fixtures/spec").join(file);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let json: serde_json::Value = serde_json::from_str(&text).expect("spec fixture is JSON");
    let field = |e: &serde_json::Value, k: &str| e[k].as_str().unwrap_or_default().to_owned();
    json.as_array()
        .expect("spec fixture is an array")
        .iter()
        .map(|e| Example {
            number: e["example"].as_u64().expect("example number"),
            section: field(e, "section"),
            markdown: field(e, "markdown"),
            html: field(e, "html"),
            extensions: e["extensions"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

/// comrak options matching `cmark --unsafe` (raw HTML is passed through, as
/// the spec examples expect).
fn comrak_options() -> comrak::Options<'static> {
    let mut opts = comrak::Options::default();
    opts.render.r#unsafe = true;
    opts
}

/// comrak's HTML for `md` with the CommonMark profile.
pub fn comrak_commonmark_html(md: &str) -> String {
    comrak::markdown_to_html(md, &comrak_options())
}

/// comrak's HTML for `md` with the named cmark-gfm extensions. comrak 0.59
/// has no `tagfilter` option; that name is ignored (see the known
/// divergences).
pub fn comrak_gfm_html(md: &str, extensions: &[String]) -> String {
    let mut opts = comrak_options();
    for ext in extensions {
        match ext.as_str() {
            "table" => opts.extension.table = true,
            "strikethrough" => opts.extension.strikethrough = true,
            "autolink" => opts.extension.autolink = true,
            "tasklist" => opts.extension.tasklist = true,
            "footnotes" => opts.extension.footnotes = true,
            _ => {}
        }
    }
    comrak::markdown_to_html(md, &opts)
}

/// Every cmark-gfm extension name (the GFM profile).
pub fn gfm_all_extensions() -> Vec<String> {
    [
        "table",
        "strikethrough",
        "autolink",
        "tagfilter",
        "tasklist",
        "footnotes",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// A small deterministic PRNG (xorshift64*), so failures reproduce from a
/// seed without extra dependencies.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next_u64() % n as u64).unwrap_or(0)
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Markdown fragments the random documents are built from, weighted towards
/// the constructs where implementations tend to differ.
const FRAGMENTS: &[&str] = &[
    "# Heading",
    "## Heading *em*",
    "###### h6 #",
    "#5 not",
    "Title\n===",
    "Title\n---",
    "- item",
    "* item",
    "+ item",
    "1. one",
    "2) two",
    "10. ten",
    "- [ ] task",
    "-\n  foo",
    "  - nested",
    "    - deep",
    "- a\n\n- b",
    "1. a\n   1. b",
    "> quote",
    "> > nested",
    ">",
    "> - in quote",
    "lazy line",
    "```\ncode\n```",
    "~~~ info\nx\n~~~",
    "```",
    "    indented",
    "\tTab",
    "<div>\nblock\n</div>",
    "<!-- comment -->",
    "<?pi?>",
    "<!DOCTYPE html>",
    "<![CDATA[x]]>",
    "<pre>\npre\n</pre>",
    "<custom-tag>",
    "<a href=\"x\">",
    "[ref]: /url \"title\"",
    "[ref]",
    "[text](/url)",
    "[text](</a b> 'title')",
    "![img](i.png)",
    "[ref][]",
    "[x][ref]",
    "<https://example.com>",
    "<a@b.c>",
    "*em*",
    "**strong**",
    "_u_",
    "__uu__",
    "***both***",
    "*a **b** c*",
    "a*b*c",
    "_a_b",
    "**",
    "*",
    "_",
    "`code`",
    "``a`b``",
    "`",
    "\\*escaped\\*",
    "\\",
    "&amp;",
    "&copy;",
    "&#35;",
    "&#x41;",
    "&bogus;",
    "line  ",
    "line\\",
    "a\nb",
    "---",
    "***",
    "___",
    "- - -",
    "= =",
    "| a | b |",
    "~~del~~",
    "é ñ 日本",
    "😀",
    "\u{a0}",
    "word",
    "two words",
    "x",
    "",
    " ",
    "   ",
    "\r\n",
    "\r",
];

const SEPARATORS: &[&str] = &["\n", "\n", "\n", "\n\n", "", " ", "\n  ", "\n    ", "\n> "];

/// A random document of up to `max_fragments` fragments.
pub fn random_document(rng: &mut Rng, max_fragments: usize) -> String {
    let mut s = String::new();
    for _ in 0..=rng.below(max_fragments) {
        if rng.below(8) == 0 {
            s.push_str(&" ".repeat(rng.below(5)));
        }
        s.push_str(rng.pick(FRAGMENTS));
        s.push_str(rng.pick(SEPARATORS));
    }
    s
}

/// Shrinks `input` while `fails` stays true: first whole lines, then single
/// characters. Returns the smallest failing input found.
pub fn minimize(input: &str, fails: impl Fn(&str) -> bool) -> String {
    let mut cur = input.to_owned();
    // Lines.
    let mut i = 0;
    loop {
        let lines: Vec<&str> = cur.split_inclusive('\n').collect();
        if i >= lines.len() {
            break;
        }
        let candidate: String = lines
            .iter()
            .enumerate()
            .filter(|&(j, _)| j != i)
            .map(|(_, l)| *l)
            .collect();
        if fails(&candidate) {
            cur = candidate;
        } else {
            i += 1;
        }
    }
    // Characters.
    let mut i = 0;
    while i < cur.len() {
        let len = cur[i..].chars().next().map_or(1, char::len_utf8);
        let candidate = format!("{}{}", &cur[..i], &cur[i + len..]);
        if fails(&candidate) {
            cur = candidate;
        } else {
            i += len;
        }
    }
    cur
}
