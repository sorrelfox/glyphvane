// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Table tests of the foldable regions of Markdown samples: each region as
//! its first line, kind and the text it folds.

use gv_editor::{FoldKind, foldable_ranges};
use gv_syntax::{FrontMatterOptions, ParseOptions};
use gv_text::TextSnapshot;

/// The regions of `text` as (line, kind, folded text).
fn regions(text: &str, options: ParseOptions) -> Vec<(usize, FoldKind, String)> {
    let snapshot = TextSnapshot::new(text);
    let tree = gv_syntax::parse_with(&snapshot, options);
    let candidates = foldable_ranges(&tree, &snapshot);
    for candidate in &candidates {
        // Every range starts at the end of a line and ends at the end of a
        // later one.
        let point = |offset| snapshot.offset_to_point(offset).expect("in the text");
        let line_end = |line| snapshot.line_range(line).expect("a line").end;
        let (start, end) = (point(candidate.range.start), point(candidate.range.end));
        assert_eq!(line_end(start.line), candidate.range.start);
        assert_eq!(line_end(end.line), candidate.range.end);
        assert!(end.line > start.line, "{candidate:?}");
        assert!(start.line >= candidate.line, "{candidate:?}");
    }
    candidates
        .into_iter()
        .map(|candidate| {
            (
                candidate.line,
                candidate.kind,
                text[candidate.range].to_owned(),
            )
        })
        .collect()
}

fn commonmark(text: &str) -> Vec<(usize, FoldKind, String)> {
    regions(text, ParseOptions::COMMONMARK)
}

fn h(level: u8) -> FoldKind {
    FoldKind::Heading { level }
}

fn r(line: usize, kind: FoldKind, text: &str) -> (usize, FoldKind, String) {
    (line, kind, text.to_owned())
}

#[test]
fn headings_fold_to_the_next_heading_of_the_same_or_a_higher_level() {
    let text = "\
# One
intro

## One.A
a text

### One.A.i
deep

## One.B
b text
# Two
two text
";
    assert_eq!(
        commonmark(text),
        [
            r(
                0,
                h(1),
                "\nintro\n\n## One.A\na text\n\n### One.A.i\ndeep\n\n## One.B\nb text"
            ),
            r(3, h(2), "\na text\n\n### One.A.i\ndeep"),
            r(6, h(3), "\ndeep"),
            r(9, h(2), "\nb text"),
            r(11, h(1), "\ntwo text"),
        ]
    );
}

#[test]
fn a_section_runs_to_the_end_of_the_text_without_trailing_blank_lines() {
    assert_eq!(
        commonmark("## Last\none\n\ntwo\n\n\n"),
        [r(0, h(2), "\none\n\ntwo")]
    );
    // A heading with nothing after it, or only blank lines, does not fold.
    assert_eq!(commonmark("# A\n\n\n# B"), []);
}

#[test]
fn setext_headings_fold_after_their_underline() {
    let text = "Title\n=====\nbody\n\nSub\n---\nmore\n";
    assert_eq!(
        commonmark(text),
        [r(0, h(1), "\nbody\n\nSub\n---\nmore"), r(4, h(2), "\nmore"),]
    );
}

#[test]
fn headings_inside_containers_are_not_sections() {
    let text = "> # Quoted\n> text\n\n# Real\nbody\n";
    assert_eq!(
        commonmark(text),
        [r(0, FoldKind::BlockQuote, "\n> text"), r(3, h(1), "\nbody"),]
    );
}

#[test]
fn list_items_with_nested_content() {
    let text = "\
- one
- two
  - nested
  - more
- three

  second paragraph
- four
  lazy continuation
";
    assert_eq!(
        commonmark(text),
        [
            r(1, FoldKind::ListItem, "\n  - nested\n  - more"),
            r(4, FoldKind::ListItem, "\n\n  second paragraph"),
        ]
    );
}

#[test]
fn ordered_and_nested_items() {
    let text = "1. a\n   1. b\n      - c\n      - d\n2. e\n";
    assert_eq!(
        commonmark(text),
        [
            r(0, FoldKind::ListItem, "\n   1. b\n      - c\n      - d"),
            r(1, FoldKind::ListItem, "\n      - c\n      - d"),
        ]
    );
}

#[test]
fn code_blocks() {
    let text = "\
```rust
fn main() {}
```

    indented
    code

~~~
unclosed
";
    assert_eq!(
        commonmark(text),
        [
            r(0, FoldKind::FencedCode, "\nfn main() {}\n```"),
            r(4, FoldKind::IndentedCode, "\n    code"),
            r(7, FoldKind::FencedCode, "\nunclosed"),
        ]
    );
    // An empty fenced block still folds its closing fence.
    assert_eq!(
        commonmark("```\n```\n"),
        [r(0, FoldKind::FencedCode, "\n```")]
    );
}

#[test]
fn block_quotes_and_html_blocks() {
    let text = "\
> quote
> more
>
> - item

<div>
  <p>x</p>
</div>
";
    assert_eq!(
        commonmark(text),
        [
            r(0, FoldKind::BlockQuote, "\n> more\n>\n> - item"),
            r(5, FoldKind::Html, "\n  <p>x</p>\n</div>"),
        ]
    );
}

#[test]
fn front_matter() {
    let text = "---\ntitle: x\ntags: [a]\n---\n# Heading\nbody\n";
    let yaml = ParseOptions {
        front_matter: FrontMatterOptions::YAML,
        ..ParseOptions::default()
    };
    assert_eq!(
        regions(text, yaml),
        [
            r(0, FoldKind::FrontMatter, "\ntitle: x\ntags: [a]\n---"),
            r(4, h(1), "\nbody"),
        ]
    );
    // Without the option it is a thematic break and a setext heading,
    // whose section ends at once (the next heading is of a higher level).
    assert_eq!(commonmark(text), [r(4, h(1), "\nbody")]);
}

#[test]
fn nested_regions_share_a_line_largest_first() {
    let text = "- ```\n  code\n  ```\n  after\n";
    assert_eq!(
        commonmark(text),
        [
            r(0, FoldKind::ListItem, "\n  code\n  ```\n  after"),
            r(0, FoldKind::FencedCode, "\n  code\n  ```"),
        ]
    );
}

#[test]
fn crlf_line_endings() {
    let text = "# A\r\none\r\n\r\n# B\r\ntwo\r\n";
    assert_eq!(
        commonmark(text),
        [r(0, h(1), "\r\none"), r(3, h(1), "\r\ntwo")]
    );
}
