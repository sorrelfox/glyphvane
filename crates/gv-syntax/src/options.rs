// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Parser switches of a flavor profile.
//!
//! [`ParseOptions`] is the part of a flavor profile that the parser
//! reads: block switches (front matter, tables) and inline switches
//! (strikethrough, extended autolinks, see [`InlineOptions`]). It is
//! deliberately small: every field is a switch some built-in profile sets
//! differently, and the default is CommonMark exactly. A tree remembers the
//! options it was parsed with ([`SyntaxTree::options`]), and incremental
//! reparses keep them.
//!
//! | Profile id | Front matter | GFM tables, strikethrough, extended autolinks |
//! |---|---|---|
//! | `commonmark` | none (`---` at the top is a thematic break) | no |
//! | `gfm-file`, `gfm-comment` | YAML | yes |
//! | `obsidian` | YAML (JSON objects use the same `---` delimiters) | not yet |
//! | `extended` | YAML and TOML | not yet |
//!
//! [`InlineOptions`]: crate::inline::InlineOptions
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

/// Parser switches (see the [module documentation](self)).
///
/// The default parses CommonMark 0.31.2 exactly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ParseOptions {
    /// Front matter at the start of the document.
    pub front_matter: FrontMatterOptions,
    /// GFM tables (the `table` extension of cmark-gfm): a delimiter row
    /// under a paragraph's last line turns that line into a table header
    /// ([`LeafKind::Table`](crate::LeafKind::Table)).
    pub tables: bool,
    /// GFM strikethrough with two tildes: `~~x~~` (the `strikethrough`
    /// extension).
    pub strikethrough: bool,
    /// With [`strikethrough`](Self::strikethrough), a single tilde strikes
    /// through too (`~x~`), as in GFM; without, a lone `~` is text (the
    /// Obsidian rule, cmark-gfm's `STRIKETHROUGH_DOUBLE_TILDE`).
    pub strikethrough_single: bool,
    /// GFM extended autolinks: `www.`, `http://`, `https://`, `ftp://` and
    /// bare email addresses (the `autolink` extension).
    pub autolinks: bool,
}

impl ParseOptions {
    /// CommonMark exactly (the default).
    pub const COMMONMARK: ParseOptions = ParseOptions {
        front_matter: FrontMatterOptions::NONE,
        tables: false,
        strikethrough: false,
        strikethrough_single: false,
        autolinks: false,
    };

    /// The GFM extensions of the GFM spec (tables, strikethrough, extended
    /// autolinks), without front matter: what cmark-gfm parses with
    /// `-e table -e strikethrough -e autolink`.
    pub const GFM: ParseOptions = ParseOptions {
        front_matter: FrontMatterOptions::NONE,
        tables: true,
        strikethrough: true,
        strikethrough_single: true,
        autolinks: true,
    };

    /// The inline part of the options.
    pub fn inline(self) -> crate::inline::InlineOptions {
        crate::inline::InlineOptions {
            strikethrough: self.strikethrough,
            strikethrough_single: self.strikethrough_single,
            autolinks: self.autolinks,
        }
    }

    /// The options of a built-in profile, by profile id (`commonmark`,
    /// `gfm-file`, `gfm-comment`, `obsidian`, `extended`); `None` for an
    /// unknown id.
    pub fn for_profile(id: &str) -> Option<ParseOptions> {
        Some(match id {
            "commonmark" => ParseOptions::COMMONMARK,
            "gfm-file" | "gfm-comment" => ParseOptions {
                front_matter: FrontMatterOptions::YAML,
                ..ParseOptions::GFM
            },
            "obsidian" => ParseOptions {
                front_matter: FrontMatterOptions::YAML,
                ..ParseOptions::COMMONMARK
            },
            "extended" => ParseOptions {
                front_matter: FrontMatterOptions {
                    yaml: true,
                    toml: true,
                },
                ..ParseOptions::COMMONMARK
            },
            _ => return None,
        })
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
        assert!(gfm.tables && gfm.strikethrough && gfm.autolinks);
        let cm = ParseOptions::COMMONMARK;
        assert!(!cm.tables && !cm.strikethrough && !cm.autolinks);
        assert_eq!(ParseOptions::for_profile("nope"), None);
    }
}
