// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The `glyphvane-expect/1` format: resolver results in the shape the
//! Obsidian oracle comparator reads.
//!
//! Links are keyed by `(source, kind, original, ordinal)`, never by
//! offsets. Statuses are the [`Status`] strings (`Ambiguous(3)`); subpaths
//! are `heading {heading, line}`, `block {id, line}`, `missing`,
//! `passthrough`, or `null`. Lines are 0-based. A heading or block found
//! only through a fallback carries `loose: true`; the field is omitted
//! (and read as `false`) for exact matches, so older files stay valid.

use serde::{Deserialize, Serialize};

use crate::fragment::FragmentTarget;
use crate::resolve::{Resolution, Status};

/// The schema identifier.
pub const EXPECT_SCHEMA: &str = "glyphvane-expect/1";

/// Where a link sits, in the oracle's terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExpectKind {
    /// A link in the body.
    Link,
    /// An embed (`![[…]]`, `![](…)`).
    Embed,
    /// A link in a front-matter property.
    Frontmatter,
}

/// A resolved subpath.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ExpectSubpath {
    /// A heading.
    Heading {
        /// Heading text.
        heading: String,
        /// 0-based line.
        line: u32,
        /// Found only through a fallback (another slug algorithm, a
        /// case-insensitive comparison).
        #[serde(default, skip_serializing_if = "is_false")]
        loose: bool,
    },
    /// A block id.
    Block {
        /// The id.
        id: String,
        /// 0-based line.
        line: u32,
        /// Matched only case-insensitively.
        #[serde(default, skip_serializing_if = "is_false")]
        loose: bool,
    },
    /// A fragment that was not found in the target.
    Missing,
    /// Handled by a viewer (`page=`, `t=`, image size).
    Passthrough,
}

// serde's `skip_serializing_if` passes a reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// One link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectLink {
    /// Vault-relative source path.
    pub source: String,
    /// Body link, embed or front matter.
    pub kind: ExpectKind,
    /// The link's exact source text.
    pub original: String,
    /// The n-th link with this `kind` and `original` in `source`.
    pub ordinal: u32,
    /// The status string.
    pub status: String,
    /// The chosen target, or `null`.
    pub target: Option<String>,
    /// For `Ambiguous`: all candidates, best first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<String>>,
    /// The subpath result.
    pub subpath: Option<ExpectSubpath>,
}

/// One probe (a link text resolved without being written in a note).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectProbe {
    /// Vault-relative source path.
    pub source: String,
    /// The link text.
    pub linkpath: String,
    /// The status string.
    pub status: String,
    /// The chosen target, or `null`.
    pub target: Option<String>,
}

/// A whole expected-results file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectFile {
    /// Always [`EXPECT_SCHEMA`].
    pub schema: String,
    /// The vault name.
    pub vault: String,
    /// The profile id.
    pub profile: String,
    /// All links.
    pub links: Vec<ExpectLink>,
    /// Probes (optional).
    #[serde(default)]
    pub probes: Vec<ExpectProbe>,
}

/// Whether `status` is a valid `glyphvane-expect/1` status string.
#[must_use]
pub fn is_valid_status(status: &str) -> bool {
    matches!(
        status,
        "Exact"
            | "CaseFold"
            | "Loose"
            | "Unresolved"
            | "OutsideRoot"
            | "External"
            | "Blocked"
            | "Pending"
    ) || status
        .strip_prefix("Ambiguous(")
        .and_then(|s| s.strip_suffix(')'))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

impl Resolution {
    /// The target path for the expect format: only resolved statuses have
    /// one (`OutsideRoot`, `Blocked` and `Unresolved` are `null`, as in
    /// Obsidian's cache).
    #[must_use]
    pub fn expect_target(&self) -> Option<String> {
        if self.status.is_resolved() {
            self.target_path.as_ref().map(|p| p.as_str().to_owned())
        } else {
            None
        }
    }

    /// The candidates for the expect format (`Ambiguous` only).
    #[must_use]
    pub fn expect_candidates(&self) -> Option<Vec<String>> {
        matches!(self.status, Status::Ambiguous(_)).then(|| {
            self.candidates
                .iter()
                .map(|c| c.path.as_str().to_owned())
                .collect()
        })
    }

    /// The subpath for the expect format, from a fragment result. A wikilink
    /// image size counts as `passthrough`.
    #[must_use]
    pub fn expect_subpath(&self, fragment: Option<&FragmentTarget>) -> Option<ExpectSubpath> {
        match fragment {
            None if self.size.is_some() => Some(ExpectSubpath::Passthrough),
            None | Some(FragmentTarget::Top | FragmentTarget::Unchecked) => None,
            Some(FragmentTarget::Heading { text, line, loose }) => Some(ExpectSubpath::Heading {
                heading: text.clone(),
                line: *line,
                loose: *loose,
            }),
            Some(FragmentTarget::Block { id, line, loose }) => Some(ExpectSubpath::Block {
                id: id.clone(),
                line: *line,
                loose: *loose,
            }),
            Some(FragmentTarget::Missing) => Some(ExpectSubpath::Missing),
            Some(FragmentTarget::Lines { .. } | FragmentTarget::Passthrough) => {
                Some(ExpectSubpath::Passthrough)
            }
        }
    }

    /// This resolution as one expect entry.
    #[must_use]
    pub fn to_expect_link(
        &self,
        source: &str,
        kind: ExpectKind,
        original: &str,
        ordinal: u32,
        fragment: Option<&FragmentTarget>,
    ) -> ExpectLink {
        ExpectLink {
            source: source.to_owned(),
            kind,
            original: original.to_owned(),
            ordinal,
            status: self.status.to_string(),
            target: self.expect_target(),
            candidates: self.expect_candidates(),
            subpath: self.expect_subpath(fragment),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses() {
        for s in ["Exact", "Ambiguous(3)", "Pending", "Blocked"] {
            assert!(is_valid_status(s), "{s}");
        }
        for s in ["exact", "Ambiguous()", "Ambiguous(x)", "Ambiguous(3"] {
            assert!(!is_valid_status(s), "{s}");
        }
        assert!(is_valid_status(&Status::Ambiguous(12).to_string()));
    }

    #[test]
    fn loose_is_optional() {
        let exact = ExpectSubpath::Heading {
            heading: "Part 2".into(),
            line: 3,
            loose: false,
        };
        let loose = ExpectSubpath::Heading {
            heading: "Part 2".into(),
            line: 3,
            loose: true,
        };
        assert_eq!(
            serde_json::to_string(&exact).unwrap(),
            r#"{"kind":"heading","heading":"Part 2","line":3}"#
        );
        assert_eq!(
            serde_json::to_string(&loose).unwrap(),
            r#"{"kind":"heading","heading":"Part 2","line":3,"loose":true}"#
        );
        let old: ExpectSubpath =
            serde_json::from_str(r#"{"kind":"block","id":"abc","line":2}"#).unwrap();
        assert_eq!(
            old,
            ExpectSubpath::Block {
                id: "abc".into(),
                line: 2,
                loose: false
            }
        );
        let r: ExpectSubpath =
            serde_json::from_str(r#"{"kind":"heading","heading":"H","line":0,"loose":true}"#)
                .unwrap();
        assert_eq!(
            r,
            ExpectSubpath::Heading {
                heading: "H".into(),
                line: 0,
                loose: true
            }
        );
    }
}
