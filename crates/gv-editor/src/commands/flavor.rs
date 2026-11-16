// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What the Markdown commands need to know about a flavor profile.

use std::fmt;

use gv_syntax::ParseOptions;

/// The flavor profile of a document, as far as editing commands are
/// concerned: which inline and block syntax exists.
///
/// | Flavor | Id | `~~strike~~` | `==highlight==` | Task items | Wikilinks | Tables, footnotes |
/// |---|---|---|---|---|---|---|
/// | CommonMark | `commonmark` | — | — | — | — | — |
/// | GFM (files) | `gfm-file` | yes | — | `[ ]` `[x]` | — | yes |
/// | GFM (comments) | `gfm-comment` | yes | — | `[ ]` `[x]` | — | yes |
/// | Obsidian | `obsidian` | yes | yes | any status character | yes | yes |
/// | Extended | `extended` | yes | yes | `[ ]` `[x]` | yes | yes |
///
/// Tables follow the profile's parser options ([`Flavor::has_tables`]).
///
/// Extended reads a single `~` as subscript, so strikethrough is always
/// written with two tildes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Flavor {
    /// CommonMark 0.31.2, no extensions.
    CommonMark,
    /// GitHub, file rendering.
    GfmFile,
    /// GitHub, comment rendering.
    GfmComment,
    /// Obsidian.
    Obsidian,
    /// The Extended superset.
    Extended,
}

impl Flavor {
    /// Every flavor, in the order of the table above.
    pub const ALL: [Flavor; 5] = [
        Flavor::CommonMark,
        Flavor::GfmFile,
        Flavor::GfmComment,
        Flavor::Obsidian,
        Flavor::Extended,
    ];

    /// The flavor of a profile id (`commonmark`, `gfm-file`, …); `None` for
    /// an unknown id.
    pub fn from_id(id: &str) -> Option<Flavor> {
        Self::ALL.into_iter().find(|flavor| flavor.id() == id)
    }

    /// The flavor of a profile id, falling back to [`Flavor::Extended`] for
    /// an unknown one (the profile documents get before their own ships).
    pub fn from_id_or_extended(id: &str) -> Flavor {
        Self::from_id(id).unwrap_or(Flavor::Extended)
    }

    /// The profile id.
    pub fn id(self) -> &'static str {
        match self {
            Flavor::CommonMark => "commonmark",
            Flavor::GfmFile => "gfm-file",
            Flavor::GfmComment => "gfm-comment",
            Flavor::Obsidian => "obsidian",
            Flavor::Extended => "extended",
        }
    }

    /// The parser options of the profile (used when a command has to parse
    /// the text itself).
    pub fn parse_options(self) -> ParseOptions {
        gv_syntax::ProfileId::from_id(self.id())
            .map_or(ParseOptions::COMMONMARK, ParseOptions::builtin)
    }

    /// Whether `~~text~~` is strikethrough.
    pub fn has_strikethrough(self) -> bool {
        self != Flavor::CommonMark
    }

    /// Whether `==text==` is a highlight.
    pub fn has_highlight(self) -> bool {
        matches!(self, Flavor::Obsidian | Flavor::Extended)
    }

    /// Whether `- [ ]` starts a task item.
    pub fn has_tasks(self) -> bool {
        self != Flavor::CommonMark
    }

    /// Whether any single character between the brackets is a task status
    /// (Obsidian), not only a space, `x` or `X`.
    pub fn any_task_status(self) -> bool {
        self == Flavor::Obsidian
    }

    /// Whether pipe tables exist (the profile parses GFM tables).
    pub fn has_tables(self) -> bool {
        self.parse_options().tables
    }

    /// Whether `[^label]` footnotes exist.
    pub fn has_footnotes(self) -> bool {
        self != Flavor::CommonMark
    }

    /// Whether `[[target]]` is a wikilink.
    pub fn has_wikilinks(self) -> bool {
        matches!(self, Flavor::Obsidian | Flavor::Extended)
    }
}

impl fmt::Display for Flavor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for flavor in Flavor::ALL {
            assert_eq!(Flavor::from_id(flavor.id()), Some(flavor));
            assert!(gv_syntax::ProfileId::from_id(flavor.id()).is_some());
        }
        assert_eq!(Flavor::from_id("mkdocs"), None);
        assert_eq!(Flavor::from_id_or_extended("mkdocs"), Flavor::Extended);
    }
}
