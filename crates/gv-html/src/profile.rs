// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The flavor profiles the paste converter writes for.

/// A built-in flavor profile. It decides which syntax the converter may
/// produce (and which characters it must escape).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Profile {
    /// CommonMark 0.31.2, no extensions.
    CommonMark,
    /// GitHub Flavored Markdown as rendered for repository files.
    GfmFile,
    /// GitHub Flavored Markdown as rendered in issues and comments.
    GfmComment,
    /// Obsidian Flavored Markdown.
    Obsidian,
    /// The extended superset (the default for loose folders).
    #[default]
    Extended,
}

impl Profile {
    /// The profile with this id (`commonmark`, `gfm-file`, `gfm-comment`,
    /// `obsidian`, `extended`).
    #[must_use]
    pub fn from_id(id: &str) -> Option<Profile> {
        Some(match id {
            "commonmark" => Profile::CommonMark,
            "gfm-file" => Profile::GfmFile,
            "gfm-comment" => Profile::GfmComment,
            "obsidian" => Profile::Obsidian,
            "extended" => Profile::Extended,
            _ => return None,
        })
    }

    /// The profile id.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Profile::CommonMark => "commonmark",
            Profile::GfmFile => "gfm-file",
            Profile::GfmComment => "gfm-comment",
            Profile::Obsidian => "obsidian",
            Profile::Extended => "extended",
        }
    }

    /// GFM pipe tables.
    pub(crate) fn tables(self) -> bool {
        self != Profile::CommonMark
    }

    /// `- [ ]` / `- [x]` task list items.
    pub(crate) fn task_lists(self) -> bool {
        self != Profile::CommonMark
    }

    /// `~~strike~~`.
    pub(crate) fn strikethrough(self) -> bool {
        self != Profile::CommonMark
    }

    /// `~sub~` and `^sup^`.
    pub(crate) fn sub_sup(self) -> bool {
        self == Profile::Extended
    }

    /// `==highlight==`.
    pub(crate) fn highlight(self) -> bool {
        matches!(self, Profile::Obsidian | Profile::Extended)
    }

    /// `$math$`.
    pub(crate) fn dollar_math(self) -> bool {
        self != Profile::CommonMark
    }

    /// `> [!note]` callouts or GitHub alerts.
    pub(crate) fn alerts(self) -> bool {
        self != Profile::CommonMark
    }

    /// `~` is a delimiter (strike, or subscript in the extended profile).
    pub(crate) fn tilde_delimiter(self) -> bool {
        self.strikethrough() || self.sub_sup()
    }

    /// `++insert++` (extended).
    pub(crate) fn plus_delimiter(self) -> bool {
        self == Profile::Extended
    }

    /// `%%comment%%` and `^block-id` (Obsidian).
    pub(crate) fn obsidian_syntax(self) -> bool {
        self == Profile::Obsidian
    }

    /// `:::` fenced divs (extended).
    pub(crate) fn fenced_divs(self) -> bool {
        self == Profile::Extended
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for p in [
            Profile::CommonMark,
            Profile::GfmFile,
            Profile::GfmComment,
            Profile::Obsidian,
            Profile::Extended,
        ] {
            assert_eq!(Profile::from_id(p.id()), Some(p));
        }
        assert_eq!(Profile::from_id("pandoc"), None);
    }
}
