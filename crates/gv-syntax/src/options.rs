// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Parser switches that change the block structure.
//!
//! [`ParseOptions`] is the part of a flavor profile that the block pass
//! reads. It is deliberately small: every field is a switch some built-in
//! profile sets differently, and the default is CommonMark exactly. A
//! tree remembers the options it was parsed with ([`SyntaxTree::options`]),
//! and incremental reparses keep them.
//!
//! | Profile id | Front matter |
//! |---|---|
//! | `commonmark` | none (`---` at the top is a thematic break) |
//! | `gfm-file`, `gfm-comment` | YAML |
//! | `obsidian` | YAML (JSON objects use the same `---` delimiters) |
//! | `extended` | YAML and TOML |
//!
//! [`SyntaxTree::options`]: crate::SyntaxTree::options

/// Which front matter syntaxes the parser recognizes at byte 0 (after an
/// optional BOM). See [`crate::BlockKind::FrontMatter`].
///
/// The first line must be the opening delimiter followed only by spaces or
/// tabs, and a later line the closing delimiter (also at column 0 and
/// followed only by spaces or tabs). Without a closing line there is no
/// front matter. Leading blank lines are not accepted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FrontMatterOptions {
    /// YAML: opened by `---`, closed by `---` or `...`.
    pub yaml: bool,
    /// TOML: opened and closed by `+++`.
    pub toml: bool,
}

impl FrontMatterOptions {
    /// No front matter.
    pub const NONE: FrontMatterOptions = FrontMatterOptions {
        yaml: false,
        toml: false,
    };

    /// YAML front matter only.
    pub const YAML: FrontMatterOptions = FrontMatterOptions {
        yaml: true,
        toml: false,
    };

    /// Whether any syntax is enabled.
    pub fn any(self) -> bool {
        self.yaml || self.toml
    }
}

/// Switches of the block pass (see the [module documentation](self)).
///
/// The default parses CommonMark 0.31.2 exactly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ParseOptions {
    /// Front matter at the start of the document.
    pub front_matter: FrontMatterOptions,
}

impl ParseOptions {
    /// CommonMark exactly (the default).
    pub const COMMONMARK: ParseOptions = ParseOptions {
        front_matter: FrontMatterOptions::NONE,
    };

    /// The options of a built-in profile, by profile id (`commonmark`,
    /// `gfm-file`, `gfm-comment`, `obsidian`, `extended`); `None` for an
    /// unknown id.
    pub fn for_profile(id: &str) -> Option<ParseOptions> {
        let front_matter = match id {
            "commonmark" => FrontMatterOptions::NONE,
            "gfm-file" | "gfm-comment" | "obsidian" => FrontMatterOptions::YAML,
            "extended" => FrontMatterOptions {
                yaml: true,
                toml: true,
            },
            _ => return None,
        };
        Some(ParseOptions { front_matter })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles() {
        assert_eq!(ParseOptions::default(), ParseOptions::COMMONMARK);
        assert_eq!(
            ParseOptions::for_profile("commonmark"),
            Some(ParseOptions::COMMONMARK)
        );
        let ext = ParseOptions::for_profile("extended").expect("known");
        assert!(ext.front_matter.yaml && ext.front_matter.toml);
        let gfm = ParseOptions::for_profile("gfm-file").expect("known");
        assert_eq!(gfm.front_matter, FrontMatterOptions::YAML);
        assert_eq!(ParseOptions::for_profile("nope"), None);
    }
}
