// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Checking whole documents: what is skipped per construct, incremental
//! re-checks through the block cache, and a 1 MB document.

use std::sync::Arc;
use std::time::Instant;

use gv_spell::{BlockCache, Checker, Library, Rules, WordSet, check_document};
use gv_syntax::{DollarMath, FrontMatterOptions, ParseOptions, SyntaxTree, TaskLists};

fn options() -> ParseOptions {
    ParseOptions {
        front_matter: FrontMatterOptions {
            yaml: true,
            toml: true,
        },
        math: DollarMath::Pandoc,
        task_lists: TaskLists::Standard,
        footnotes: true,
        alerts: true,
        ..ParseOptions::GFM
    }
}

fn checker(words: &[&str], revision: u64) -> Checker {
    let library = Library::new(None);
    let en = library.backend("en_US").expect("bundled");
    Checker::new(
        vec![en],
        vec![Arc::new(WordSet::new(words.iter().copied()))],
        Rules::default(),
        revision,
    )
}

fn misspelled(text: &str) -> Vec<String> {
    let tree = SyntaxTree::parse_with(text, options());
    let check = check_document(
        &tree,
        text,
        &checker(&[], 0),
        &mut BlockCache::default(),
        None,
    );
    check
        .misspellings
        .iter()
        .map(|m| {
            assert_eq!(&text[m.range.clone()], &*m.word);
            m.word.to_string()
        })
        .collect()
}

/// `wrongg` sits in every construct that is not prose; `badd`, `baddtwo`
/// and so on sit where prose is.
#[test]
fn only_prose_is_checked() {
    let text = "\
---
title: A baddone title
tags: [wrongg, wrongg]
wrongg: plain
lang: wrongg
---

# Heading with baddtwo

Text `wrongg code` and $wrongg + math$ and <span class=\"wrongg\">baddthree</span>.
A [baddfour link](https://wrongg.example/wrongg \"title wrongg\") and <https://wrongg.org>.
Bare https://wrongg.org/x and www.wrongg.com and someone@wrongg.org and ./wrongg/path.md.
An ![baddfive alt](wrongg.png) and [ref][wrongg] and [[Wrongg note|baddsix alias]].
Obsidian #wrongg tag, :wrongg: code, ![[wrongg.png]] and &amp; entity.
Footnote[^wrongg]. NASAA and h2oo stay. Snake wrongg_case and wrongg.md.
**Strong baddseven** and *emphasis baddeight* and ~~strike baddnine~~.

> [!wrongg] Note baddten
> Quoted baddeleven ^wrongg

```rust
let wrongg = 1;
```

    indented wrongg

$$
\\wrongg
$$

<div>
wrongg block
</div>

| head baddtwelve | `wrongg` |
|---|---|
| cell baddthirteen | [x](wrongg) |

[wrongg]: https://wrongg.org \"wrongg\"
[^wrongg]: Footnote baddfourteen.
";
    let found = misspelled(text);
    assert!(
        !found.iter().any(|w| w.to_lowercase().contains("wrongg")),
        "{found:?}"
    );
    let expected = [
        "baddone",
        "baddtwo",
        "baddthree",
        "baddfour",
        "baddfive",
        "baddsix",
        "baddseven",
        "baddeight",
        "baddnine",
        "baddten",
        "baddeleven",
        "baddtwelve",
        "baddthirteen",
        "baddfourteen",
    ];
    assert_eq!(found, expected);
}

#[test]
fn words_in_word_sets_are_not_flagged() {
    let text = "Glyphvane and zettelkasten and teh.\n";
    let tree = SyntaxTree::parse_with(text, options());
    let mut cache = BlockCache::default();
    let all = check_document(&tree, text, &checker(&[], 0), &mut cache, None);
    assert_eq!(all.misspellings.len(), 3);
    let some = check_document(
        &tree,
        text,
        &checker(&["glyphvane", "zettelkasten"], 1),
        &mut cache,
        None,
    );
    let words: Vec<&str> = some.misspellings.iter().map(|m| &*m.word).collect();
    assert_eq!(words, ["teh"]);
    // A new revision of the words is a new checker: nothing reused.
    assert_eq!(some.stats.reused, 0);
}

fn paragraphs(n: usize) -> String {
    (0..n)
        .map(|i| format!("Paragraph {i} has a mistaek and some ordinary words in it.\n\n"))
        .collect()
}

#[test]
fn a_recheck_after_an_edit_checks_only_changed_blocks() {
    let text = paragraphs(50);
    let c = checker(&[], 0);
    let tree = SyntaxTree::parse_with(text.as_str(), options());
    let mut cache = BlockCache::default();
    let first = check_document(&tree, text.as_str(), &c, &mut cache, None);
    assert_eq!(first.stats.blocks, 50);
    assert_eq!(first.stats.checked, 50);
    assert_eq!(first.misspellings.len(), 50);

    // Fix the third paragraph's typo, and add a paragraph at the start
    // (every other block moves).
    let edited = format!(
        "New text with wrnog words.\n\n{}",
        text.replacen("Paragraph 2 has a mistaek", "Paragraph 2 has a mistake", 1)
    );
    let tree = SyntaxTree::parse_with(edited.as_str(), options());
    let second = check_document(&tree, edited.as_str(), &c, &mut cache, None);
    assert_eq!(second.stats.blocks, 51);
    assert_eq!(second.stats.checked, 2);
    assert_eq!(second.stats.reused, 49);
    let words: Vec<&str> = second.misspellings.iter().map(|m| &*m.word).collect();
    assert_eq!(words.len(), 50);
    assert_eq!(words[0], "wrnog");
    // Moved results point at the new offsets.
    for m in &second.misspellings {
        assert_eq!(&edited[m.range.clone()], &*m.word);
    }
    // The blocks around a range only.
    let near = check_document(&tree, edited.as_str(), &c, &mut cache, Some(0..10));
    assert_eq!(near.stats.blocks, 1);
    assert_eq!(near.misspellings.len(), 1);
}

/// A 1 MB document: the first check and a re-check after a one-block edit.
/// Run with `--nocapture` for the times (debug builds are much slower than
/// release; the numbers of record come from `--profile measure`).
#[test]
fn a_one_megabyte_document() {
    let mut text = String::new();
    let mut i = 0;
    while text.len() < 1 << 20 {
        text.push_str(&format!(
            "## Section {i}\n\nThe quick brown fox jumps over the lazy dog, recieving a [link](https://example.org/{i}) \
             and `code {i}` while *emphasis* and **strong text** run on. Several ordinary sentences follow here, \
             with punctuation; quotes \u{201C}like these\u{201D} and it\u{2019}s fine.\n\n\
             - a list item with a link [[Note {i}]] and a #tag\n- another item\n\n"
        ));
        i += 1;
    }
    let c = checker(&[], 0);
    let started = Instant::now();
    let tree = SyntaxTree::parse_with(text.as_str(), options());
    let parsed = started.elapsed();
    let mut cache = BlockCache::default();
    let started = Instant::now();
    let first = check_document(&tree, text.as_str(), &c, &mut cache, None);
    let checked = started.elapsed();
    assert_eq!(first.misspellings.len(), i);
    assert!(first.misspellings.iter().all(|m| &*m.word == "recieving"));

    let at = text.len() / 2;
    let at = text[at..].find("over").map_or(at, |k| at + k);
    let mut edited = text.clone();
    edited.insert_str(at, "ovr ");
    let tree = SyntaxTree::parse_with(edited.as_str(), options());
    let started = Instant::now();
    let second = check_document(&tree, edited.as_str(), &c, &mut cache, None);
    let rechecked = started.elapsed();
    assert_eq!(second.stats.checked, 1);
    assert_eq!(second.misspellings.len(), i + 1);
    println!(
        "1 MB ({} bytes, {} blocks, {} words): parse {parsed:?}, first check {checked:?}, \
         re-check after one edit {rechecked:?}",
        text.len(),
        first.stats.blocks,
        first.stats.words
    );
}
