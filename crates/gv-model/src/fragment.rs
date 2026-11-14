// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Matching link fragments against a document's headings and block ids.
//!
//! The link resolver classifies a fragment (top, block id, line range,
//! viewer parameters, heading) and decodes it (percent-decoding for
//! Markdown links, NFC); this module finds the heading or block. For a
//! heading, the first rule that matches wins:
//!
//! 1. an explicit id (none in the CommonMark profile);
//! 2. the profile's primary [`Anchoring`]: a slug algorithm, or Obsidian
//!    heading text;
//! 3. the other anchorings (a GitHub-style anchor in an Obsidian note still
//!    works): **loose**;
//! 4. a case-insensitive comparison against all of the above: **loose**.
//!
//! A `user-content-` prefix (GitHub's id prefix) is accepted wherever a
//! slug is compared. `top` is the top of the document unless a heading has
//! the id `top`. A chain (`#A#B`, Obsidian) looks for each part inside the
//! section of the previous one.

use crate::model::{DocModel, nfc};
use crate::slug::{SlugAlgorithm, obsidian};

/// How a profile names headings in fragments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Anchoring {
    /// Slugs of this algorithm.
    Slug(SlugAlgorithm),
    /// Heading text (Obsidian).
    HeadingText,
    /// No heading anchors (GitHub comments); every match is loose.
    None,
}

/// Where a heading fragment landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentMatch {
    /// `top`, with no heading of that id: the top of the document.
    Top,
    /// A heading.
    Heading {
        /// Its index in [`DocModel::headings`].
        index: usize,
        /// Found only through a fallback (another anchoring, or ignoring
        /// case): the link checker warns.
        loose: bool,
    },
    /// No such heading (`MissingAnchor`).
    Missing,
}

/// Where a block-id fragment landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockMatch {
    /// A block anchor.
    Found {
        /// Its index in [`DocModel::block_anchors`].
        index: usize,
        /// Matched only ignoring case.
        loose: bool,
    },
    /// No such block id.
    Missing,
}

/// GitHub prefixes the ids of user content.
const USER_CONTENT: &str = "user-content-";

impl DocModel {
    /// Finds the heading a fragment names. `parts` is the decoded fragment
    /// (one part, or an Obsidian chain `A#B` split at `#`); `primary` is
    /// the profile's anchoring.
    pub fn match_fragment<P: AsRef<str>>(&self, parts: &[P], primary: Anchoring) -> FragmentMatch {
        if let [only] = parts
            && only.as_ref().eq_ignore_ascii_case("top")
        {
            return match self.find(0..self.headings().len(), "top", primary, true) {
                Some((index, loose)) => FragmentMatch::Heading { index, loose },
                None => FragmentMatch::Top,
            };
        }
        let mut range = 0..self.headings().len();
        let mut found = None;
        let mut loose = false;
        for part in parts {
            let Some((i, l)) = self.find(range.clone(), part.as_ref(), primary, false) else {
                return FragmentMatch::Missing;
            };
            loose |= l;
            found = Some(i);
            range = i + 1..self.outline()[i].section_end;
        }
        match found {
            Some(index) => FragmentMatch::Heading { index, loose },
            None => FragmentMatch::Missing,
        }
    }

    /// Finds a block id (`^id` without the `^`): exact first, then ignoring
    /// case (loose). ORACLE-PENDING: whether Obsidian's block ids are
    /// case-sensitive.
    pub fn match_block(&self, id: &str) -> BlockMatch {
        let anchors = self.block_anchors();
        if let Some(index) = anchors.iter().position(|a| &*a.id == id) {
            return BlockMatch::Found {
                index,
                loose: false,
            };
        }
        let lower = id.to_lowercase();
        match anchors.iter().position(|a| a.id.to_lowercase() == lower) {
            Some(index) => BlockMatch::Found { index, loose: true },
            None => BlockMatch::Missing,
        }
    }

    /// The first heading in `range` that `part` names, and whether the
    /// match is loose. With `exact_only`, only explicit ids and the primary
    /// anchoring are tried, case-sensitively.
    fn find(
        &self,
        range: std::ops::Range<usize>,
        part: &str,
        primary: Anchoring,
        exact_only: bool,
    ) -> Option<(usize, bool)> {
        let part = nfc(part);
        let part = part.as_ref();
        let stripped = part.strip_prefix(USER_CONTENT).filter(|s| !s.is_empty());
        let is_slug = |s: &str| s == part || stripped == Some(s);
        let headings = self.headings();
        let in_range = || range.clone().filter(|&i| i < headings.len());

        // 1. Explicit ids.
        if let Some(i) =
            in_range().find(|&i| headings[i].explicit_id.as_deref().is_some_and(is_slug))
        {
            return Some((i, false));
        }
        // 2. The primary anchoring.
        if let Some(i) = self.find_by(primary, in_range(), part, &is_slug) {
            return Some((i, false));
        }
        if exact_only {
            return None;
        }
        // 3. The other anchorings, loose.
        let others = std::iter::once(Anchoring::HeadingText)
            .chain(SlugAlgorithm::ALL.iter().map(|&a| Anchoring::Slug(a)))
            .filter(|&a| a != primary);
        for a in others.clone() {
            if let Some(i) = self.find_by(a, in_range(), part, &is_slug) {
                return Some((i, true));
            }
        }
        // 4. Ignoring case, loose (heading-text keys ignore case already).
        let lower = part.to_lowercase();
        let lower_stripped = stripped.map(str::to_lowercase);
        let is_lower_slug = |s: &str| s == lower || lower_stripped.as_deref() == Some(s);
        if let Some(i) = in_range().find(|&i| {
            headings[i]
                .explicit_id
                .as_deref()
                .is_some_and(|id| is_lower_slug(&id.to_lowercase()))
        }) {
            return Some((i, true));
        }
        for a in std::iter::once(primary).chain(others) {
            if let Anchoring::Slug(alg) = a {
                let keys = self.slug_keys(alg);
                if let Some(i) = in_range().find(|&i| is_lower_slug(&keys[i].1)) {
                    return Some((i, true));
                }
            }
        }
        None
    }

    /// The first heading in `range` that `part` names under `anchoring`.
    fn find_by(
        &self,
        anchoring: Anchoring,
        mut range: impl Iterator<Item = usize>,
        part: &str,
        is_slug: &dyn Fn(&str) -> bool,
    ) -> Option<usize> {
        match anchoring {
            Anchoring::None => None,
            Anchoring::Slug(alg) => {
                let keys = self.slug_keys(alg);
                range.find(|&i| is_slug(&keys[i].0))
            }
            Anchoring::HeadingText => {
                let want = obsidian::heading_key(part);
                if want.is_empty() {
                    return None;
                }
                let keys = self.heading_keys();
                range.find(|&i| *keys[i].0 == *want || *keys[i].1 == *want)
            }
        }
    }
}
