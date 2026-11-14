// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Heading anchor (slug) algorithms of the tools Glyphvane's profiles
//! follow, as pure functions, and their duplicate handling ([`Slugger`]).
//!
//! Each algorithm is two steps, kept apart so that both can be tested
//! against the tool's own fixtures:
//!
//! 1. [`SlugAlgorithm::slug`]: heading text to a base slug. The text is the
//!    [`Projection`] the tool starts from: the rendered text, the rendered
//!    text with image descriptions, or the Markdown source.
//! 2. [`Slugger::unique`]: the base slug to a unique one, with the tool's
//!    fallback for an empty slug and its suffix scheme.
//!
//! | Algorithm | Used by | Kept | Spaces | Empty | Duplicates | Source checked |
//! |---|---|---|---|---|---|---|
//! | [`GitHub`](SlugAlgorithm::GitHub) | CommonMark, GFM, Extended; Docusaurus | letters (`Alphabetic`), marks, `Nd`, `Nl`, `Pc`, `-` | each → `-` | stays empty | `-1`, `-2` | github-slugger 2.0.0 |
//! | [`GitLab`](SlugAlgorithm::GitLab) | GitLab | `L`, `M`, `N`, `Pc`, `-` | each → `-` | stays empty | `-1`, `-2` | GitLab `HeadingSlug`, comrak anchorizer |
//! | [`Pandoc`](SlugAlgorithm::Pandoc) | Pandoc, Quarto | `L`, `N`, `_`, `-`, `.`; up to the first letter dropped | runs → `-` | `section` | `-1`, `-2` | pandoc `Text.Pandoc.Shared` |
//! | [`PythonMarkdown`](SlugAlgorithm::PythonMarkdown) | MkDocs | ASCII after NFKD (or `\w` with `unicode`), `-` | runs of space and `-` → `-` | `_1` | `_1`, `_2` | Python-Markdown `toc` |
//! | [`Kramdown`](SlugAlgorithm::Kramdown) | kramdown, Jekyll | ASCII letters, digits, `-`; up to the first letter dropped | each → `-` | `section` | `-1`, `-2` (no collision check) | kramdown `Converter::Base` |
//! | [`Hugo`](SlugAlgorithm::Hugo) | Hugo | `L`, `Nd`, `_` (`github`); see [`HugoIds`] | each → `-` | `heading` | `-1`, `-2` | Hugo `markup/goldmark/autoid.go` |
//!
//! Obsidian has no slugs: links name the heading text, matched by
//! [`obsidian::heading_key`].
//!
//! # Fidelity
//!
//! Every algorithm was ported from the tool's source (checked 2026-11-13)
//! and is tested against the tool's own test fixtures where they exist.
//! Known differences:
//!
//! - **Unicode versions.** Classes come from Unicode 18 (general
//!   categories) and the standard library's tables. github-slugger's
//!   pattern was generated from Unicode 13, so characters assigned since
//!   then are kept here and removed there.
//! - **Pandoc**: `gfm_auto_identifiers` replaces emoji with their names;
//!   here they are removed. `ascii_identifiers` transliterates through
//!   pandoc's own table; here accents are stripped by NFD and other
//!   non-ASCII characters dropped.
//! - **kramdown**: `transliterated_header_ids` (Unidecode) is not
//!   supported.
//! - **Lowercasing**: GitHub, GitLab and Python-Markdown use the full
//!   mappings with final sigma; Pandoc the full mappings without it; Hugo
//!   the simple mappings, as their runtimes do.

mod chars;
mod github;
mod gitlab;
mod hugo;
mod kramdown;
pub mod obsidian;
mod pandoc;
mod python_markdown;

use std::collections::{HashMap, HashSet};

/// The heading text an algorithm starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Projection {
    /// The rendered text, as a browser's `textContent` holds it: text with
    /// escapes and entities decoded, code span content, link text; no
    /// image descriptions, no raw HTML tags; line breaks as `\n`.
    Text,
    /// [`Text`](Self::Text) with image descriptions, and line breaks as
    /// spaces (the text of the syntax tree, as comrak and pandoc collect
    /// it).
    TextWithAlt,
    /// The heading's Markdown source (its lines joined with `\n`).
    Source,
    /// The last source line (Goldmark).
    SourceLastLine,
}

/// Pandoc's identifier extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PandocIds {
    /// `gfm_auto_identifiers`: GitHub's rules (no leading-character
    /// stripping, marks kept, spaces become `-` one by one).
    pub gfm: bool,
    /// `ascii_identifiers`: accents stripped, other non-ASCII dropped.
    pub ascii: bool,
}

/// Hugo's `markup.goldmark.parser.autoHeadingIDType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HugoIds {
    /// `github` (the default).
    GitHub,
    /// `github-ascii`: accents removed, other non-ASCII dropped.
    GitHubAscii,
    /// `blackfriday`: runs of non-alphanumerics become one `-`.
    Blackfriday,
}

/// A heading slug algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlugAlgorithm {
    /// GitHub's heading anchors, as github-slugger 2.0.0 computes them.
    GitHub,
    /// GitLab's heading anchors (comrak's anchorizer).
    GitLab,
    /// Pandoc's `auto_identifiers`.
    Pandoc(PandocIds),
    /// Python-Markdown's `toc` extension (MkDocs): `slugify`, or
    /// `slugify_unicode` with `unicode`.
    PythonMarkdown {
        /// Keep non-ASCII word characters (`slugify_unicode`).
        unicode: bool,
    },
    /// kramdown's `auto_ids` (from the raw heading text).
    Kramdown,
    /// Hugo's Goldmark heading ids.
    Hugo(HugoIds),
}

impl SlugAlgorithm {
    /// Every algorithm and variant, primary ones first: the order in which
    /// fallbacks are tried when a fragment does not match the primary
    /// algorithm.
    pub const ALL: [SlugAlgorithm; 11] = [
        SlugAlgorithm::GitHub,
        SlugAlgorithm::GitLab,
        SlugAlgorithm::Pandoc(PandocIds {
            gfm: false,
            ascii: false,
        }),
        SlugAlgorithm::Pandoc(PandocIds {
            gfm: true,
            ascii: false,
        }),
        SlugAlgorithm::Pandoc(PandocIds {
            gfm: false,
            ascii: true,
        }),
        SlugAlgorithm::PythonMarkdown { unicode: false },
        SlugAlgorithm::PythonMarkdown { unicode: true },
        SlugAlgorithm::Kramdown,
        SlugAlgorithm::Hugo(HugoIds::GitHub),
        SlugAlgorithm::Hugo(HugoIds::GitHubAscii),
        SlugAlgorithm::Hugo(HugoIds::Blackfriday),
    ];

    /// Position of this algorithm in [`ALL`](Self::ALL) (`None` for the
    /// combined pandoc variant `gfm` + `ascii`).
    pub fn index(self) -> Option<usize> {
        Self::ALL.iter().position(|&a| a == self)
    }

    /// A short stable name (`github`, `pandoc-gfm`, …) for traces and the
    /// content index.
    pub fn name(self) -> &'static str {
        match self {
            SlugAlgorithm::GitHub => "github",
            SlugAlgorithm::GitLab => "gitlab",
            SlugAlgorithm::Pandoc(p) => match (p.gfm, p.ascii) {
                (false, false) => "pandoc",
                (true, false) => "pandoc-gfm",
                (false, true) => "pandoc-ascii",
                (true, true) => "pandoc-gfm-ascii",
            },
            SlugAlgorithm::PythonMarkdown { unicode: false } => "python-markdown",
            SlugAlgorithm::PythonMarkdown { unicode: true } => "python-markdown-unicode",
            SlugAlgorithm::Kramdown => "kramdown",
            SlugAlgorithm::Hugo(HugoIds::GitHub) => "hugo-github",
            SlugAlgorithm::Hugo(HugoIds::GitHubAscii) => "hugo-github-ascii",
            SlugAlgorithm::Hugo(HugoIds::Blackfriday) => "hugo-blackfriday",
        }
    }

    /// The heading text this algorithm starts from.
    pub fn projection(self) -> Projection {
        match self {
            SlugAlgorithm::GitHub | SlugAlgorithm::PythonMarkdown { .. } => Projection::Text,
            SlugAlgorithm::GitLab | SlugAlgorithm::Pandoc(_) => Projection::TextWithAlt,
            SlugAlgorithm::Kramdown => Projection::Source,
            SlugAlgorithm::Hugo(_) => Projection::SourceLastLine,
        }
    }

    /// The base slug of `text` (no duplicate handling, no fallback for an
    /// empty result). `text` is the algorithm's [`projection`](Self::projection).
    pub fn slug(self, text: &str) -> String {
        match self {
            SlugAlgorithm::GitHub => github::slug(text),
            SlugAlgorithm::GitLab => gitlab::slug(text),
            SlugAlgorithm::Pandoc(p) => pandoc::slug(text, p),
            SlugAlgorithm::PythonMarkdown { unicode } => python_markdown::slug(text, unicode),
            SlugAlgorithm::Kramdown => kramdown::slug(text),
            SlugAlgorithm::Hugo(t) => hugo::slug(text, t),
        }
    }

    fn dedup(self) -> Dedup {
        match self {
            SlugAlgorithm::GitHub | SlugAlgorithm::GitLab => Dedup::Hyphen { empty: "" },
            SlugAlgorithm::Pandoc(_) => Dedup::Hyphen { empty: "section" },
            SlugAlgorithm::Hugo(_) => Dedup::Hyphen { empty: "heading" },
            SlugAlgorithm::PythonMarkdown { .. } => Dedup::Underscore,
            SlugAlgorithm::Kramdown => Dedup::Counter { empty: "section" },
        }
    }
}

/// How an algorithm makes slugs unique.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dedup {
    /// `x`, `x-1`, `x-2`, … skipping every id already taken (github-slugger,
    /// comrak, pandoc, Hugo). An empty base becomes `empty` first.
    Hyphen { empty: &'static str },
    /// Python-Markdown's `unique`: `x_1`, `x_2`, …; an id that already ends
    /// in `_N` continues from `N`; an empty id becomes `_1`.
    Underscore,
    /// kramdown: a counter per base id, with no check against ids taken
    /// otherwise.
    Counter { empty: &'static str },
}

/// Pandoc stops looking for a free suffix after this many and repeats the
/// base id.
const PANDOC_SUFFIX_LIMIT: usize = 60_000;

/// Assigns unique slugs to the headings of one document, in order.
#[derive(Debug, Clone)]
pub struct Slugger {
    algorithm: SlugAlgorithm,
    taken: HashSet<String>,
    /// Hyphen: the next suffix to try per base. Underscore: the highest `n`
    /// such that `base_1` to `base_n` are known to be taken. Counter: the
    /// last suffix per base.
    counters: HashMap<String, usize>,
}

impl Slugger {
    /// A slugger with no ids taken.
    pub fn new(algorithm: SlugAlgorithm) -> Self {
        Self {
            algorithm,
            taken: HashSet::new(),
            counters: HashMap::new(),
        }
    }

    /// The algorithm.
    pub fn algorithm(&self) -> SlugAlgorithm {
        self.algorithm
    }

    /// Marks `id` as taken without generating it: explicit ids that exist
    /// in the document before slugs are assigned (Python-Markdown collects
    /// every `id` attribute first; pandoc every explicit identifier).
    pub fn reserve(&mut self, id: &str) {
        self.taken.insert(id.to_owned());
    }

    /// Slugs `text` (the algorithm's projection) and makes the result
    /// unique.
    pub fn slug(&mut self, text: &str) -> String {
        let base = self.algorithm.slug(text);
        self.unique(base)
    }

    /// Makes `base` unique with the algorithm's duplicate scheme and records
    /// it.
    pub fn unique(&mut self, base: String) -> String {
        match self.algorithm.dedup() {
            Dedup::Hyphen { empty } => self.unique_hyphen(base, empty),
            Dedup::Underscore => self.unique_underscore(base),
            Dedup::Counter { empty } => self.unique_counter(base, empty),
        }
    }

    fn unique_hyphen(&mut self, base: String, empty: &str) -> String {
        let base = if base.is_empty() {
            empty.to_owned()
        } else {
            base
        };
        if !self.taken.contains(&base) {
            self.taken.insert(base.clone());
            return base;
        }
        // Every suffix below the remembered one was taken when it was
        // tried, and ids are never released, so starting there gives the
        // same result as starting at 1.
        let mut n = self.counters.get(&base).copied().unwrap_or(1).max(1);
        let pandoc = matches!(self.algorithm, SlugAlgorithm::Pandoc(_));
        let chosen = loop {
            if pandoc && n > PANDOC_SUFFIX_LIMIT {
                break base.clone();
            }
            let candidate = format!("{base}-{n}");
            if !self.taken.contains(&candidate) {
                break candidate;
            }
            n += 1;
        };
        self.counters.insert(base, n + 1);
        self.taken.insert(chosen.clone());
        chosen
    }

    fn unique_underscore(&mut self, id: String) -> String {
        if !id.is_empty() && !self.taken.contains(&id) {
            self.taken.insert(id.clone());
            return id;
        }
        let (base, count) = python_markdown::split_idcount(&id);
        let known = self.counters.get(base).copied().unwrap_or(0);
        // `base_1` to `base_known` are taken; so is `id` itself.
        let skip = count <= known || id == format!("{base}_{}", known + 1);
        let mut n = if skip { count.max(known) } else { count };
        n += 1;
        let mut candidate = format!("{base}_{n}");
        while self.taken.contains(&candidate) {
            n += 1;
            candidate = format!("{base}_{n}");
        }
        if skip {
            self.counters.insert(base.to_owned(), n);
        }
        self.taken.insert(candidate.clone());
        candidate
    }

    fn unique_counter(&mut self, base: String, empty: &str) -> String {
        let base = if base.is_empty() {
            empty.to_owned()
        } else {
            base
        };
        let id = match self.counters.get_mut(&base) {
            Some(n) => {
                *n += 1;
                format!("{base}-{n}")
            }
            None => {
                self.counters.insert(base.clone(), 0);
                base
            }
        };
        self.taken.insert(id.clone());
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(alg: SlugAlgorithm, inputs: &[&str]) -> Vec<String> {
        let mut s = Slugger::new(alg);
        inputs.iter().map(|t| s.slug(t)).collect()
    }

    #[test]
    fn names_and_indices_are_distinct() {
        let mut names = HashSet::new();
        for (i, a) in SlugAlgorithm::ALL.iter().enumerate() {
            assert_eq!(a.index(), Some(i));
            assert!(names.insert(a.name()));
        }
    }

    #[test]
    fn hyphen_duplicates() {
        // github-slugger's README and fixtures: a literal "Dup 1" after two
        // "Dup" headings.
        assert_eq!(
            run(SlugAlgorithm::GitHub, &["Dup", "Dup", "Dup 1", "Dup"]),
            ["dup", "dup-1", "dup-1-1", "dup-2"]
        );
        assert_eq!(
            run(SlugAlgorithm::GitHub, &["a-1", "a", "a", "a"]),
            ["a-1", "a", "a-2", "a-3"]
        );
    }

    #[test]
    fn empty_fallbacks() {
        let pandoc = SlugAlgorithm::Pandoc(PandocIds::default());
        assert_eq!(run(pandoc, &["33", "33"]), ["section", "section-1"]);
        assert_eq!(
            run(SlugAlgorithm::Hugo(HugoIds::GitHub), &["!", "?"]),
            ["heading", "heading-1"]
        );
        assert_eq!(
            run(
                SlugAlgorithm::PythonMarkdown { unicode: false },
                &["!", "?"]
            ),
            ["_1", "_2"]
        );
        assert_eq!(
            run(SlugAlgorithm::Kramdown, &["123", "456"]),
            ["section", "section-1"]
        );
        assert_eq!(run(SlugAlgorithm::GitHub, &["!", "?"]), ["", "-1"]);
    }

    #[test]
    fn kramdown_counter_does_not_check_collisions() {
        // kramdown's generate_id: a literal "a-1" is not seen by the counter.
        assert_eq!(
            run(SlugAlgorithm::Kramdown, &["a 1", "a", "a"]),
            ["a-1", "a", "a-1"]
        );
    }

    #[test]
    fn pandoc_suffix_limit() {
        let mut s = Slugger::new(SlugAlgorithm::Pandoc(PandocIds::default()));
        for i in 1..=PANDOC_SUFFIX_LIMIT {
            s.reserve(&format!("x-{i}"));
        }
        assert_eq!(s.slug("x"), "x");
        assert_eq!(s.slug("x"), "x");
    }

    #[test]
    fn many_duplicates_are_linear() {
        let mut s = Slugger::new(SlugAlgorithm::GitHub);
        for i in 0..20_000 {
            let id = s.slug("Header");
            if i > 0 {
                assert_eq!(id, format!("header-{i}"));
            }
        }
        let mut s = Slugger::new(SlugAlgorithm::PythonMarkdown { unicode: false });
        for i in 0..20_000 {
            let id = s.slug("Header");
            if i > 0 {
                assert_eq!(id, format!("header_{i}"));
            }
        }
    }
}
