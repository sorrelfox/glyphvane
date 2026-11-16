// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Highlighting through the public API: aliases, classes (golden span
//! dumps), the cache, caps and pathological input.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gv_highlight::{
    CodeHighlighter, GialloHighlighter, Highlight, HighlightKey, Language, Limits, Outcome,
    TokenClass,
};

/// The common fence languages, by a typical info string.
const COMMON: &[&str] = &[
    "rust",
    "python",
    "javascript",
    "typescript",
    "tsx",
    "jsx",
    "json",
    "yaml",
    "toml",
    "bash",
    "console",
    "c",
    "cpp",
    "csharp",
    "java",
    "kotlin",
    "swift",
    "go",
    "ruby",
    "php",
    "html",
    "css",
    "scss",
    "sql",
    "diff",
    "dockerfile",
    "makefile",
    "lua",
    "r",
    "julia",
    "haskell",
    "ocaml",
    "elixir",
    "erlang",
    "clojure",
    "scala",
    "dart",
    "zig",
    "nim",
    "perl",
    "powershell",
    "latex",
    "markdown",
    "xml",
    "graphql",
    "protobuf",
    "nix",
    "terraform",
    "ini",
    "csv",
    "vim",
    "asm",
    "fortran",
    "matlab",
    "groovy",
    "objective-c",
    "svelte",
    "vue",
    "mermaid",
];

fn dump(code: &str, h: &Highlight) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "language: {:?}", h.language.map(Language::name));
    let _ = writeln!(out, "outcome: {:?}", h.outcome);
    for s in &h.spans {
        let _ = writeln!(
            out,
            "{}..{} {} {:?}",
            s.start,
            s.end,
            s.class.name(),
            &code[s.start..s.end]
        );
    }
    out
}

/// Compares against `tests/golden/NAME.txt`; `GV_UPDATE_GOLDEN=1` rewrites it.
fn golden(name: &str, info: &str, code: &str) {
    let h = CodeHighlighter::shared().highlight(code, info);
    let actual = dump(code, &h);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/{name}.txt"));
    if std::env::var_os("GV_UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &actual).expect("writing the golden file");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing {}; run with GV_UPDATE_GOLDEN=1", path.display()));
    assert_eq!(actual, expected, "golden {name} differs");
}

fn classes_of(h: &Highlight, code: &str, text: &str) -> Vec<TokenClass> {
    h.spans
        .iter()
        .filter(|s| &code[s.start..s.end] == text)
        .map(|s| s.class)
        .collect()
}

#[test]
fn aliases_resolve() {
    let cases = [
        ("rust", "rust"),
        ("rs", "rust"),
        ("Rust", "rust"),
        ("rust,ignore", "rust"),
        ("{.rust}", "rust"),
        ("rust title=\"main.rs\"", "rust"),
        ("language-python", "python"),
        ("py", "python"),
        ("py3", "python"),
        ("python3", "python"),
        ("golang", "go"),
        ("sh", "shellscript"),
        ("bash", "shellscript"),
        ("zsh", "shellscript"),
        ("shell", "shellscript"),
        ("console", "shellsession"),
        ("shell-session", "shellsession"),
        ("yml", "yaml"),
        ("js", "javascript"),
        ("ts", "typescript"),
        ("c++", "cpp"),
        ("c#", "csharp"),
        ("cs", "csharp"),
        ("ps1", "powershell"),
        ("jl", "julia"),
        ("tf", "terraform"),
        ("hcl", "terraform"),
        ("dockerfile", "docker"),
        ("tex", "latex"),
        ("md", "markdown"),
        ("objc", "objective-c"),
        ("proto", "proto"),
        ("vimscript", "viml"),
        ("f90", "fortran-free-form"),
        ("patch", "diff"),
        ("json5", "json5"),
        ("mmd", "mermaid"),
    ];
    for (info, grammar) in cases {
        let lang = Language::from_info(info).unwrap_or_else(|| panic!("{info} does not resolve"));
        assert_eq!(lang.name(), grammar, "{info}");
    }
    for info in ["", "   ", "nonsense-language", "text", "{}"] {
        assert_eq!(Language::from_info(info), None, "{info:?}");
    }
    assert!(gv_highlight::alias_count() > 500);
}

#[test]
fn common_languages_are_covered() {
    let hl = CodeHighlighter::shared();
    for info in COMMON {
        let lang = Language::from_info(info).unwrap_or_else(|| panic!("{info} does not resolve"));
        let h = hl.highlight("x = 1 # \"two\" // three\n", info);
        assert_eq!(h.outcome, Outcome::Highlighted, "{info} ({})", lang.name());
    }
    assert_eq!(COMMON.len(), 59);
}

#[test]
fn every_bundled_grammar_loads() {
    let engine = GialloHighlighter::new();
    for lang in Language::all() {
        assert!(!lang.licence().is_empty(), "{}", lang.name());
        let spans = gv_highlight::Highlighter::highlight(&engine, "a 1 \"b\"\n", lang);
        assert!(spans.is_ok(), "{}: {spans:?}", lang.name());
    }
}

#[test]
fn golden_rust() {
    golden(
        "rust",
        "rust",
        "/// Adds one.\n#[inline]\npub fn add_one(x: u32) -> u32 {\n    let s = \"a\\n\";\n    x + 1 // done\n}\n",
    );
}

#[test]
fn golden_python() {
    golden(
        "python",
        "py",
        "import os\n\n@cache\ndef f(n: int) -> str:\n    \"\"\"Doc.\"\"\"\n    if n is None:\n        return f\"{n!r}\"  # comment\n    return os.sep * 3\n",
    );
}

#[test]
fn golden_json_yaml_toml() {
    golden(
        "json",
        "json",
        "{\n  \"name\": \"gv\",\n  \"n\": 1.5,\n  \"ok\": true,\n  \"x\": null\n}\n",
    );
    golden(
        "yaml",
        "yml",
        "name: gv\nitems:\n  - 1\n  - \"two\" # note\n",
    );
    golden("toml", "toml", "[package]\nname = \"gv\"\nversion = 3\n");
}

#[test]
fn golden_markup() {
    golden(
        "html",
        "html",
        "<a href=\"x.html\" class=c>Link &amp; text</a><!-- c -->\n",
    );
    golden(
        "markdown",
        "md",
        "# Title\n\nSome *em* and **strong** and `code` and [a](https://x.org).\n\n```rust\nfn main() {}\n```\n",
    );
    golden(
        "diff",
        "diff",
        "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n",
    );
}

#[test]
fn golden_shell_and_js() {
    golden(
        "bash",
        "bash",
        "#!/bin/sh\nfor f in *.md; do echo \"$f\" | wc -l; done\n",
    );
    golden(
        "javascript",
        "js",
        "const re = /a+b/g;\nexport function f(a) { return `x ${a}` + 0x1f; }\n",
    );
}

#[test]
fn classes_make_sense() {
    let hl = CodeHighlighter::shared();
    let code = "fn main() { let s = \"hi\"; let n = 42; } // end\n";
    let h = hl.highlight(code, "rust");
    assert!(classes_of(&h, code, "fn").contains(&TokenClass::Keyword));
    assert!(classes_of(&h, code, "main").contains(&TokenClass::Function));
    assert!(classes_of(&h, code, "\"hi\"").contains(&TokenClass::String));
    assert!(classes_of(&h, code, "42").contains(&TokenClass::Number));
    assert!(h.spans.iter().any(|s| s.class == TokenClass::Comment));
    // Spans are sorted, disjoint, on char boundaries, and never plain.
    for pair in h.spans.windows(2) {
        assert!(pair[0].end <= pair[1].start);
    }
    for s in &h.spans {
        assert!(s.start < s.end && s.class != TokenClass::Plain);
        assert!(code.is_char_boundary(s.start) && code.is_char_boundary(s.end));
    }
}

#[test]
fn crlf_and_unicode_offsets() {
    let hl = CodeHighlighter::shared();
    let lf = "let a = \"é\";\nlet b = 1;\n";
    let crlf = lf.replace('\n', "\r\n");
    let h_lf = hl.highlight(lf, "rust");
    let h_crlf = hl.highlight(&crlf, "rust");
    let texts = |code: &str, h: &Highlight| -> Vec<String> {
        h.spans
            .iter()
            .map(|s| code[s.start..s.end].to_owned())
            .collect()
    };
    assert!(!h_lf.spans.is_empty());
    assert_eq!(texts(lf, &h_lf), texts(&crlf, &h_crlf));
    for s in &h_crlf.spans {
        assert!(!crlf[s.start..s.end].contains(['\r', '\n']));
    }
}

#[test]
fn embedded_languages_are_classified() {
    let hl = CodeHighlighter::shared();
    let code = "Text\n\n```python\ndef f():\n    return 1\n```\n";
    let h = hl.highlight(code, "markdown");
    assert!(
        classes_of(&h, code, "def").contains(&TokenClass::Keyword),
        "{}",
        dump(code, &h)
    );
    let html = "<script>let x = 1;</script>\n";
    let h = hl.highlight(html, "html");
    assert!(
        classes_of(&h, html, "let").contains(&TokenClass::Keyword),
        "{}",
        dump(html, &h)
    );
}

#[test]
fn unknown_language_is_plain() {
    let h = CodeHighlighter::shared().highlight("whatever", "no-such-lang");
    assert_eq!(h.outcome, Outcome::UnknownLanguage);
    assert!(h.spans.is_empty());
    assert_eq!(h.language, None);
}

#[test]
fn cache_hits_and_eviction() {
    let hl = CodeHighlighter::with_engine(GialloHighlighter::new(), Limits::default(), 64 << 10);
    let code = "let x = 1;\n";
    let key = HighlightKey::new(code, "rust");
    assert!(hl.get(&key).is_none());
    let first = hl.highlight(code, "rust");
    let second = hl.highlight(code, "rs");
    assert!(
        Arc::ptr_eq(&first, &second),
        "aliases share one cache entry"
    );
    assert!(Arc::ptr_eq(&first, &hl.get(&key).unwrap()));
    let stats = hl.cache_stats();
    assert_eq!((stats.entries, stats.misses), (1, 1));
    assert!(stats.hits >= 2);
    assert_ne!(key, HighlightKey::new("let x = 2;\n", "rust"));
    assert_ne!(key, HighlightKey::new(code, "python"));

    // A small budget evicts the oldest entries but keeps the newest.
    for i in 0..2000 {
        hl.highlight(&format!("let x{i} = {i}; // {i}\n"), "rust");
    }
    let stats = hl.cache_stats();
    assert!(stats.bytes <= 64 << 10, "{stats:?}");
    assert!(stats.entries < 2000);
    assert!(
        hl.get(&HighlightKey::new("let x1999 = 1999; // 1999\n", "rust"))
            .is_some()
    );
    assert!(hl.get(&key).is_none());
    hl.clear_cache();
    assert_eq!(hl.cache_stats().entries, 0);
}

#[test]
fn caps_leave_text_plain() {
    let limits = Limits {
        max_bytes: 1000,
        max_line_bytes: 100,
        max_line_work: 5000,
    };
    let hl = CodeHighlighter::with_engine(GialloHighlighter::new(), limits, 1 << 20);
    let big = "let x = 1;\n".repeat(200);
    let h = hl.highlight(&big, "rust");
    assert_eq!(h.outcome, Outcome::TooLarge);
    assert!(h.spans.is_empty());
    assert_eq!(h.language.map(Language::name), Some("rust"));
    let long_line = format!("let s = \"{}\";\n", "a".repeat(200));
    assert_eq!(hl.highlight(&long_line, "rust").outcome, Outcome::TooLarge);
    // 6 lines of 90 bytes: under the size caps, over the line work.
    let wide = format!("// {}\n", "a".repeat(87)).repeat(6);
    assert_eq!(hl.highlight(&wide, "rust").outcome, Outcome::TooLarge);
    assert_eq!(
        hl.highlight("let x = 1;\n", "rust").outcome,
        Outcome::Highlighted
    );

    let defaults = Limits::default();
    assert!(defaults.allows(&"x\n".repeat(1000)));
    assert!(!defaults.allows(&"x".repeat(5000)));
    assert!(!defaults.allows(&"x".repeat(600 << 10)));
    // Ordinary code within the size cap passes the line-work cap.
    assert!(defaults.allows(&format!("{}\n", "x".repeat(99)).repeat(5000)));
    assert!(Limits::UNLIMITED.allows(&"x".repeat(1 << 20)));
}

/// Pathological inputs: each must be refused by the caps or finish within
/// a bound (loose for unoptimised test builds).
#[test]
fn pathological_inputs_are_bounded() {
    let hl = CodeHighlighter::new();
    let cases: Vec<(&str, &str, String)> = vec![
        (
            "200k-char line",
            "js",
            format!("var a = {};\n", "a+".repeat(100_000)),
        ),
        (
            "4k-char line js",
            "js",
            format!("var a = {};\n", "a+".repeat(2_000)),
        ),
        (
            "nested parens",
            "js",
            format!("{}{}\n", "(".repeat(2_000), ")".repeat(2_000)),
        ),
        (
            "unterminated string",
            "python",
            format!("x = \"{}\n", "a\\".repeat(2_000)),
        ),
        ("10k unclosed comments", "c", "x /* y\n".repeat(10_000)),
        (
            "spaces then tab",
            "bash",
            format!("{}\t\n", " ".repeat(4_000)),
        ),
        (
            "nested brackets yaml",
            "yaml",
            format!("a: {}\n", "[".repeat(4_000)),
        ),
        (
            "emphasis run 90 KB",
            "markdown",
            format!("{}\n", "*a ".repeat(30_000)),
        ),
        (
            "emphasis run 2 KB",
            "markdown",
            format!("{}\n", "*a ".repeat(680)),
        ),
        (
            "emphasis runs 1 KB x 30",
            "markdown",
            format!("{}\n", "*a ".repeat(340)).repeat(30),
        ),
    ];
    for (name, info, code) in cases {
        let start = Instant::now();
        let h = hl.highlight(&code, info);
        let took = start.elapsed();
        eprintln!(
            "pathological {name}: {took:?}, {:?}, {} spans",
            h.outcome,
            h.spans.len()
        );
        assert!(
            matches!(h.outcome, Outcome::Highlighted | Outcome::TooLarge),
            "{name}"
        );
        assert!(took < Duration::from_secs(10), "{name} took {took:?}");
    }
}

#[test]
fn parallel_highlighting() {
    let hl = Arc::new(CodeHighlighter::new());
    let threads: Vec<_> = ["rust", "python", "markdown", "html", "go", "sql"]
        .into_iter()
        .map(|info| {
            let hl = Arc::clone(&hl);
            std::thread::spawn(move || {
                let code = "x = 1 \"s\" // c\n".repeat(50);
                hl.highlight(&code, info).outcome.clone()
            })
        })
        .collect();
    for t in threads {
        assert_eq!(t.join().unwrap(), Outcome::Highlighted);
    }
}
