// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Link fragments (`#heading`, `#^block`, `#L10-L20`, `#page=2`) and the
//! trait through which they are matched inside a target document.
//!
//! The resolver only **classifies** a fragment, following a fixed
//! cascade: empty, block id, line range, viewer parameters, otherwise
//! heading text. Matching headings against slugs or heading text needs the
//! document model: [`ModelFragments`](crate::model::ModelFragments)
//! implements [`FragmentResolver`] over it.

use crate::keys::nfc_key;
use crate::link::percent_decode_lossy;
use crate::path::VaultPath;
use crate::profile::ProfileParams;

/// What a fragment refers to, before it is matched in the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FragmentKind {
    /// Empty fragment: the top of the document.
    Top,
    /// `^id`: a block id.
    Block(String),
    /// `L10`, `L10-L20` (GitHub), `L10-20` (GitLab): opens Source mode with
    /// the lines selected. 1-based, inclusive.
    Lines {
        /// First line.
        start: u32,
        /// Last line, if a range.
        end: Option<u32>,
    },
    /// PDF and media parameters (`page=N`, `height=N`, `selection=…`,
    /// `t=…`), passed to the viewer.
    Viewer(Vec<(String, String)>),
    /// Heading text or slug. A chain `#A#B` (Obsidian) has several parts:
    /// "B inside the section of A". The word `top` lands here too: it means
    /// the top of the document only if no element has the id `top`.
    Heading(Vec<String>),
}

/// A parsed fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    /// The text after the first `#`, as written.
    pub raw: String,
    /// Decoded (percent-decoding for Markdown and HTML links) and NFC.
    pub text: String,
    /// The cascade classification.
    pub kind: FragmentKind,
}

impl Fragment {
    /// A fragment of a Markdown or HTML destination: percent-decoded, NFC.
    #[must_use]
    pub fn from_markdown(raw: &str, params: &ProfileParams) -> Self {
        let text = nfc_key(&percent_decode_lossy(raw)).into_owned();
        let kind = classify(&text, params.fragment_chains);
        Self {
            raw: raw.to_owned(),
            text,
            kind,
        }
    }

    /// A wikilink subpath (no percent-decoding), NFC.
    #[must_use]
    pub fn from_wikilink(raw: &str) -> Self {
        let text = nfc_key(raw).into_owned();
        let kind = classify(&text, true);
        Self {
            raw: raw.to_owned(),
            text,
            kind,
        }
    }

    /// `true` for `#top`, which falls back to the top of the document when
    /// no element has the id `top`.
    #[must_use]
    pub fn is_top_keyword(&self) -> bool {
        matches!(&self.kind, FragmentKind::Heading(parts) if parts.len() == 1 && parts[0].eq_ignore_ascii_case("top"))
    }

    /// `true` if the viewer handles it (no lookup in the document).
    #[must_use]
    pub fn is_passthrough(&self) -> bool {
        matches!(self.kind, FragmentKind::Viewer(_))
    }
}

fn classify(text: &str, chains: bool) -> FragmentKind {
    if text.is_empty() {
        return FragmentKind::Top;
    }
    if let Some(id) = text.strip_prefix('^') {
        return FragmentKind::Block(id.to_owned());
    }
    if let Some(lines) = parse_lines(text) {
        return lines;
    }
    if let Some(params) = parse_viewer(text) {
        return FragmentKind::Viewer(params);
    }
    let parts: Vec<String> = if chains {
        text.split('#')
            .filter(|p| !p.trim().is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        vec![text.to_owned()]
    };
    if let Some(last) = parts.last()
        && let Some(id) = last.strip_prefix('^')
        && parts.len() > 1
    {
        return FragmentKind::Block(id.to_owned());
    }
    if parts.is_empty() {
        return FragmentKind::Top;
    }
    FragmentKind::Heading(parts)
}

fn parse_u32(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn parse_lines(text: &str) -> Option<FragmentKind> {
    let rest = text.strip_prefix('L')?;
    match rest.split_once('-') {
        None => Some(FragmentKind::Lines {
            start: parse_u32(rest)?,
            end: None,
        }),
        Some((a, b)) => {
            let b = b.strip_prefix('L').unwrap_or(b);
            Some(FragmentKind::Lines {
                start: parse_u32(a)?,
                end: Some(parse_u32(b)?),
            })
        }
    }
}

fn parse_viewer(text: &str) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    for part in text.split('&') {
        let (k, v) = part.split_once('=')?;
        if !matches!(k, "page" | "height" | "selection" | "t") || v.is_empty() {
            return None;
        }
        out.push((k.to_owned(), v.to_owned()));
    }
    Some(out)
}

/// Where a fragment landed in the target document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FragmentTarget {
    /// The top of the document.
    Top,
    /// A heading. Lines are 0-based.
    Heading {
        /// The heading text as written.
        text: String,
        /// Its line.
        line: u32,
        /// Found only through a fallback (another slug algorithm, a
        /// case-insensitive comparison): the link checker warns.
        loose: bool,
    },
    /// A block id.
    Block {
        /// The id as written in the target.
        id: String,
        /// Its line (0-based).
        line: u32,
        /// Matched only case-insensitively.
        loose: bool,
    },
    /// Lines to select in Source mode (1-based, inclusive).
    Lines {
        /// First line.
        start: u32,
        /// Last line.
        end: u32,
    },
    /// Handled by a viewer (PDF page, media time).
    Passthrough,
    /// Not found: open at the top with "Heading not found" (`MissingAnchor`).
    Missing,
    /// No fragment resolver was available.
    Unchecked,
}

/// Matches fragments inside target documents. Implemented by the document
/// model (slug algorithms, Obsidian heading-text rules, block ids); the
/// resolver stays free of document contents.
pub trait FragmentResolver {
    /// Resolves `fragment` inside `target` under `params` (whose
    /// [`anchoring`](ProfileParams::anchoring) is the primary one).
    fn resolve_fragment(
        &self,
        target: &VaultPath,
        fragment: &Fragment,
        params: &ProfileParams,
    ) -> FragmentTarget;
}

/// A [`FragmentResolver`] that resolves only what needs no document:
/// the top, line ranges and viewer parameters. Everything else is
/// [`FragmentTarget::Unchecked`].
#[derive(Debug, Clone, Copy, Default)]
pub struct StructuralFragments;

impl FragmentResolver for StructuralFragments {
    fn resolve_fragment(
        &self,
        _: &VaultPath,
        fragment: &Fragment,
        _: &ProfileParams,
    ) -> FragmentTarget {
        match &fragment.kind {
            FragmentKind::Top => FragmentTarget::Top,
            FragmentKind::Lines { start, end } => FragmentTarget::Lines {
                start: *start,
                end: end.unwrap_or(*start),
            },
            FragmentKind::Viewer(_) => FragmentTarget::Passthrough,
            FragmentKind::Block(_) | FragmentKind::Heading(_) => FragmentTarget::Unchecked,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{ProfileId, ProfileParams};

    #[test]
    fn cascade() {
        let gh = ProfileParams::for_profile(ProfileId::GfmFile);
        let k = |s: &str| Fragment::from_markdown(s, &gh).kind;
        assert_eq!(k(""), FragmentKind::Top);
        assert_eq!(k("^abc"), FragmentKind::Block("abc".into()));
        assert_eq!(k("%5Eabc"), FragmentKind::Block("abc".into()));
        assert_eq!(
            k("L5"),
            FragmentKind::Lines {
                start: 5,
                end: None
            }
        );
        assert_eq!(
            k("L10-L20"),
            FragmentKind::Lines {
                start: 10,
                end: Some(20)
            }
        );
        assert_eq!(
            k("L10-20"),
            FragmentKind::Lines {
                start: 10,
                end: Some(20)
            }
        );
        assert_eq!(
            k("page=2"),
            FragmentKind::Viewer(vec![("page".into(), "2".into())])
        );
        assert_eq!(k("Lx"), FragmentKind::Heading(vec!["Lx".into()]));
        assert_eq!(k("Part%202"), FragmentKind::Heading(vec!["Part 2".into()]));
        assert_eq!(k("a#b"), FragmentKind::Heading(vec!["a#b".into()]));
        assert!(Fragment::from_markdown("top", &gh).is_top_keyword());
    }

    #[test]
    fn wikilink_chains_and_nfc() {
        let f = Fragment::from_wikilink("Part 1#Detail");
        assert_eq!(
            f.kind,
            FragmentKind::Heading(vec!["Part 1".into(), "Detail".into()])
        );
        let f = Fragment::from_wikilink("Re\u{301}sume\u{301}");
        assert_eq!(f.text, "R\u{e9}sum\u{e9}");
        assert_eq!(
            Fragment::from_wikilink("^Item-1").kind,
            FragmentKind::Block("Item-1".into())
        );
        // No percent-decoding in wikilinks.
        assert_eq!(
            Fragment::from_wikilink("Part%202").kind,
            FragmentKind::Heading(vec!["Part%202".into()])
        );
    }
}
