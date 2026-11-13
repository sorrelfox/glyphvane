// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Ordering of several candidates for one link.
//!
//! ORACLE-PENDING: Obsidian's real basename tie-break is undocumented. The
//! order below is the documented best guess; it is a profile setting
//! ([`TieBreak`]) and lives in this one function ([`standard_order`]) so the
//! oracle results can replace it without touching the resolver.

use std::cmp::Ordering;

use crate::path::VaultPath;

/// The tie-break rule set of a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TieBreak {
    /// ORACLE-PENDING. Exact case match first, then fewer path segments,
    /// then the shorter path, then lexical order on the NFC + case-fold
    /// key; byte order last, so the order is total.
    #[default]
    Standard,
}

/// One candidate as the tie-break sees it.
#[derive(Debug, Clone, Copy)]
pub struct TieBreakKey<'a> {
    /// The candidate path.
    pub path: &'a VaultPath,
    /// NFC of the path.
    pub nfc: &'a str,
    /// Canonical caseless key of the path.
    pub fold: &'a str,
    /// The part the link names matches with exact case (after NFC).
    pub exact_case: bool,
    /// Matched by `userIgnoreFilters`: always ranked last.
    pub user_ignored: bool,
}

/// Which rule separated two candidates (for traces).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TieBreakRule {
    /// The other one is excluded by `userIgnoreFilters`.
    UserIgnored,
    /// Exact case match.
    ExactCase,
    /// Fewer path segments.
    FewerSegments,
    /// Shorter path.
    ShorterPath,
    /// Lexical order on NFC + case fold.
    Lexical,
    /// Byte order (only twins that fold equal).
    Bytes,
}

impl TieBreakRule {
    /// A short explanation.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::UserIgnored => "others are in userIgnoreFilters",
            Self::ExactCase => "exact case",
            Self::FewerSegments => "fewer segments",
            Self::ShorterPath => "shorter path",
            Self::Lexical => "lexical order",
            Self::Bytes => "byte order",
        }
    }
}

impl TieBreak {
    /// Orders two candidates: `Less` means `a` is preferred.
    #[must_use]
    pub fn compare(self, a: &TieBreakKey<'_>, b: &TieBreakKey<'_>) -> Ordering {
        self.decide(a, b).0
    }

    /// Orders two candidates and names the deciding rule.
    #[must_use]
    pub fn decide(self, a: &TieBreakKey<'_>, b: &TieBreakKey<'_>) -> (Ordering, TieBreakRule) {
        match self {
            Self::Standard => standard_order(a, b),
        }
    }
}

/// ORACLE-PENDING: the standard tie-break order, with `userIgnoreFilters`
/// matches ranked last.
#[must_use]
pub fn standard_order(a: &TieBreakKey<'_>, b: &TieBreakKey<'_>) -> (Ordering, TieBreakRule) {
    let steps = [
        (
            a.user_ignored.cmp(&b.user_ignored),
            TieBreakRule::UserIgnored,
        ),
        (b.exact_case.cmp(&a.exact_case), TieBreakRule::ExactCase),
        (
            a.path.depth().cmp(&b.path.depth()),
            TieBreakRule::FewerSegments,
        ),
        (
            a.nfc.chars().count().cmp(&b.nfc.chars().count()),
            TieBreakRule::ShorterPath,
        ),
        (a.fold.cmp(b.fold), TieBreakRule::Lexical),
        (a.path.as_str().cmp(b.path.as_str()), TieBreakRule::Bytes),
    ];
    steps
        .into_iter()
        .find(|(o, _)| *o != Ordering::Equal)
        .unwrap_or((Ordering::Equal, TieBreakRule::Bytes))
}
