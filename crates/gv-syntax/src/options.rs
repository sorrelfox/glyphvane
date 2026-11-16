// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Parser switches of a flavor profile.
//!
//! [`ParseOptions`] is the part of a compiled flavor profile
//! ([`gv_profile::CompiledProfile`]) that the parser reads: block switches
//! (front matter, tables, `$$` math blocks) and inline switches
//! (strikethrough, extended autolinks, math; see [`InlineOptions`]). It is
//! small and `Copy`, so a tree can remember the options it was parsed with
//! ([`SyntaxTree::options`]) and incremental reparses keep them. The
//! default is CommonMark exactly.
//!
//! [`ParseOptions::from_profile`] derives the options from a compiled
//! profile's switches, so tuning and `.glyphvane.toml` overrides reach the
//! parser. Switches for constructs the parser does not implement yet are
//! ignored; a construct whose meaning the profile changes is never parsed
//! with another profile's meaning: Extended reads `~x~` as subscript, which
//! is not parsed yet, so a single tilde stays text there rather than
//! becoming GFM strikethrough.
//!
//! | Switch | Option |
//! |---|---|
//! | `front_matter_yaml`, `front_matter_toml` | [`ParseOptions::front_matter`] |
//! | `tables`, `table_min_dashes` | [`ParseOptions::tables`], [`ParseOptions::table_min_dashes`] |
//! | `tilde_double`, `tilde_single = strike` | [`ParseOptions::strikethrough`], [`ParseOptions::strikethrough_single`] |
//! | `extended_autolinks` | [`ParseOptions::autolinks`] |
//! | `math` | [`ParseOptions::math`]: `github` is [`DollarMath::GitHub`] with `` $`…`$ ``, `obsidian` is [`DollarMath::Pandoc`]; `$$` blocks with either |
//! | `math_brackets` | [`ParseOptions::math_brackets`] |
//! | `task_lists` | [`ParseOptions::task_lists`] |
//! | `footnotes`, `footnote_indent` | [`ParseOptions::footnotes`], [`ParseOptions::footnote_indent`] |
//! | `admonitions = alerts` | [`ParseOptions::alerts`] (Obsidian callouts are not parsed yet: their `> [!x]` stays a block quote) |
//!
//! [`InlineOptions`]: crate::inline::InlineOptions
//!
//! [`SyntaxTree::options`]: crate::SyntaxTree::options

use gv_profile::switches::{Admonitions, Math, TaskLists as TaskListSwitch, TildeSingle};
use gv_profile::{CompiledProfile, ProfileId, Switches};

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

/// The rules for `$…$` inline math (see [`crate::inline::InlineKind::InlineMath`]).
///
/// Both rules need a non-blank character right after the opening `$` and
/// right before the closing one, and an unescaped closer; a `$` preceded by
/// an odd number of backslashes is no closer. They differ in what may
/// follow the closer, and in escapes before the opener.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DollarMath {
    /// No `$` math (`$` is text).
    #[default]
    Off,
    /// GitHub: the closer is not followed by an ASCII letter or digit, so
    /// `$5 and $10` is not math. A backslash does not stop an opener:
    /// `\$x$` is a literal backslash and the formula `x`, as GitHub finds
    /// math before Markdown escapes.
    GitHub,
    /// Pandoc's `tex_math_dollars` (Obsidian's rule until checked against
    /// Obsidian): the closer is not followed by an ASCII digit; `\$` is an
    /// escaped dollar.
    Pandoc,
}

/// Which list items are task items (see
/// [`ParseOptions::task_lists`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TaskLists {
    /// No task items: `[ ]` is text.
    #[default]
    Off,
    /// GFM: `[ ]`, `[x]` or `[X]`.
    Standard,
    /// Obsidian: any one character between the brackets.
    AnyCharacter,
}

/// Parser switches (see the [module documentation](self)).
///
/// The default parses CommonMark 0.31.2 exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ParseOptions {
    /// Front matter at the start of the document.
    pub front_matter: FrontMatterOptions,
    /// GFM tables (the `table` extension of cmark-gfm): a delimiter row
    /// under a paragraph's last line turns that line into a table header
    /// ([`LeafKind::Table`](crate::LeafKind::Table)).
    pub tables: bool,
    /// The fewest dashes each cell of a delimiter row needs (1 in GFM).
    pub table_min_dashes: u8,
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
    /// `$…$` inline math. Any rule other than [`DollarMath::Off`] also
    /// turns on `$$…$$` display math inside paragraphs and `$$` math
    /// blocks ([`LeafKind::MathBlock`](crate::LeafKind::MathBlock)).
    pub math: DollarMath,
    /// `` $`…`$ `` inline math (GitHub).
    pub math_code: bool,
    /// `\(…\)` inline and `\[…\]` display math.
    pub math_brackets: bool,
    /// Task list items: a list item whose first line starts with `[ ]`,
    /// `[x]` or `[X]` (any one character with
    /// [`TaskLists::AnyCharacter`]) followed by a space, a tab or the end
    /// of the line. The brackets become an
    /// [`ExtMarker`](crate::TokenKind::ExtMarker) token of the item
    /// ([`Container::marker`](crate::Container::marker)).
    pub task_lists: TaskLists,
    /// GFM footnotes: `[^label]:` definitions
    /// ([`BlockKind::FootnoteDefinition`](crate::BlockKind::FootnoteDefinition))
    /// and `[^label]` references to a defined label.
    pub footnotes: bool,
    /// The indentation that continues a footnote definition (4, or 2 in
    /// Obsidian).
    pub footnote_indent: u8,
    /// GitHub alerts ([`BlockKind::Alert`](crate::BlockKind::Alert)).
    pub alerts: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self::COMMONMARK
    }
}

impl ParseOptions {
    /// CommonMark exactly (the default).
    pub const COMMONMARK: ParseOptions = ParseOptions {
        front_matter: FrontMatterOptions::NONE,
        tables: false,
        table_min_dashes: 1,
        strikethrough: false,
        strikethrough_single: false,
        autolinks: false,
        math: DollarMath::Off,
        math_code: false,
        math_brackets: false,
        task_lists: TaskLists::Off,
        footnotes: false,
        footnote_indent: 4,
        alerts: false,
    };

    /// The GFM extensions of the GFM spec (tables, strikethrough, extended
    /// autolinks), without front matter or math: what cmark-gfm parses
    /// with `-e table -e strikethrough -e autolink`.
    pub const GFM: ParseOptions = ParseOptions {
        tables: true,
        strikethrough: true,
        strikethrough_single: true,
        autolinks: true,
        ..ParseOptions::COMMONMARK
    };

    /// The options of a compiled profile (its switches after tuning and
    /// overrides).
    pub fn from_profile(profile: &CompiledProfile) -> ParseOptions {
        Self::from_switches(&profile.switches)
    }

    /// The options of built-in profile `id`, without tuning or overrides.
    pub fn builtin(id: ProfileId) -> ParseOptions {
        Self::from_switches(&gv_profile::definition(id).switches)
    }

    /// The options of a switch set.
    pub fn from_switches(s: &Switches) -> ParseOptions {
        let (math, math_code) = match s.math {
            Math::Off => (DollarMath::Off, false),
            Math::GitHub => (DollarMath::GitHub, true),
            Math::Obsidian => (DollarMath::Pandoc, false),
        };
        let strike_single = s.tilde_single == TildeSingle::Strike;
        ParseOptions {
            front_matter: FrontMatterOptions {
                yaml: s.front_matter.yaml,
                toml: s.front_matter.toml,
            },
            tables: s.tables,
            table_min_dashes: s.table_min_dashes.max(1),
            strikethrough: s.tilde_double || strike_single,
            strikethrough_single: strike_single,
            autolinks: s.extended_autolinks,
            math,
            math_code,
            math_brackets: s.math_brackets,
            task_lists: match s.task_lists {
                TaskListSwitch::Off => TaskLists::Off,
                TaskListSwitch::Standard => TaskLists::Standard,
                TaskListSwitch::AnyCharacter => TaskLists::AnyCharacter,
            },
            footnotes: s.footnotes,
            footnote_indent: if s.footnote_indent == 2 { 2 } else { 4 },
            alerts: s.admonitions == Admonitions::Alerts,
        }
    }

    /// The inline part of the options.
    pub fn inline(self) -> crate::inline::InlineOptions {
        crate::inline::InlineOptions {
            strikethrough: self.strikethrough,
            strikethrough_single: self.strikethrough_single,
            autolinks: self.autolinks,
            math: self.math,
            math_code: self.math_code,
            math_brackets: self.math_brackets,
            footnotes: self.footnotes,
        }
    }

    /// Whether `$$` math blocks open.
    pub fn math_blocks(self) -> bool {
        self.math != DollarMath::Off
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_profiles() {
        assert_eq!(ParseOptions::default(), ParseOptions::COMMONMARK);
        assert_eq!(
            ParseOptions::builtin(ProfileId::CommonMark),
            ParseOptions::COMMONMARK
        );
        let ext = ParseOptions::builtin(ProfileId::Extended);
        assert!(ext.front_matter.yaml && ext.front_matter.toml);
        assert!(ext.tables && ext.strikethrough && !ext.strikethrough_single);
        assert_eq!((ext.math, ext.math_code), (DollarMath::GitHub, true));
        let gfm = ParseOptions::builtin(ProfileId::GfmFile);
        assert_eq!(gfm.front_matter, FrontMatterOptions::YAML);
        assert!(gfm.tables && gfm.strikethrough && gfm.strikethrough_single && gfm.autolinks);
        assert_eq!(gfm.math, DollarMath::GitHub);
        assert_eq!(
            ParseOptions {
                front_matter: FrontMatterOptions::NONE,
                math: DollarMath::Off,
                math_code: false,
                task_lists: TaskLists::Off,
                footnotes: false,
                alerts: false,
                ..gfm
            },
            ParseOptions::GFM
        );
        assert_eq!(gfm.task_lists, TaskLists::Standard);
        assert!(gfm.footnotes && gfm.alerts && gfm.footnote_indent == 4);
        assert_eq!(ParseOptions::builtin(ProfileId::GfmComment), gfm);
        let ob = ParseOptions::builtin(ProfileId::Obsidian);
        assert!(ob.tables && ob.table_min_dashes == 2);
        assert!(ob.strikethrough && !ob.strikethrough_single && ob.autolinks);
        assert_eq!((ob.math, ob.math_code), (DollarMath::Pandoc, false));
        assert_eq!(ob.task_lists, TaskLists::AnyCharacter);
        assert!(ob.footnotes && ob.footnote_indent == 2 && !ob.alerts);
        assert!(ext.alerts && ext.footnotes);
        let cm = ParseOptions::COMMONMARK;
        assert!(!cm.tables && !cm.strikethrough && !cm.autolinks && !cm.math_blocks());
    }

    #[test]
    fn overrides_reach_the_options() {
        use gv_profile::{SwitchOverrides, SwitchValue, Tuning};
        let mut overrides = SwitchOverrides::new();
        for (key, value) in [
            ("front_matter_toml", SwitchValue::Bool(true)),
            ("tables", SwitchValue::Bool(true)),
            ("tilde_single", SwitchValue::Word("strike".into())),
            ("math", SwitchValue::Word("obsidian".into())),
            ("math_brackets", SwitchValue::Bool(true)),
        ] {
            overrides.insert(key, value).expect("valid");
        }
        let profile = gv_profile::compile(ProfileId::CommonMark, Tuning::default(), &overrides);
        let o = ParseOptions::from_profile(&profile);
        assert!(o.front_matter.toml && !o.front_matter.yaml);
        assert!(o.tables && o.strikethrough && o.strikethrough_single && !o.autolinks);
        assert_eq!(o.math, DollarMath::Pandoc);
        assert!(o.math_brackets && o.math_blocks());
    }
}
