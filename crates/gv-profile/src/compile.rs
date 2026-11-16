// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The profile compiler: a built-in profile, tool-config tuning and switch
//! overrides become a [`CompiledProfile`], the flag set the parser and the
//! renderer read.
//!
//! The order is fixed: the definition's switches, then [`Tuning`] from the
//! tool's own configuration, then the user's [`SwitchOverrides`] from
//! `.glyphvane.toml`. From the switches the compiler derives the enabled
//! extension rules in priority order and the inline trigger table.

use std::fmt;

use crate::builtin::definition;
use crate::id::ProfileId;
use crate::switches::{Admonitions, Math, SwitchError, SwitchValue, Switches, TildeSingle};

/// Switch overrides, validated and in file order. A later value for the
/// same key replaces an earlier one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SwitchOverrides(Vec<(String, SwitchValue)>);

impl SwitchOverrides {
    /// No overrides.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `key = value` after checking it against the switch table.
    ///
    /// # Errors
    ///
    /// The switch table's reason when the key is unknown or the value
    /// invalid; nothing is added then.
    pub fn insert(&mut self, key: &str, value: SwitchValue) -> Result<(), SwitchError> {
        let mut check = Switches::COMMONMARK;
        check.set(key, &value)?;
        self.0.retain(|(k, _)| k != key);
        self.0.push((key.to_owned(), value));
        Ok(())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The overrides, as `(key, value)`.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &SwitchValue)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    fn apply(&self, switches: &mut Switches) {
        for (key, value) in &self.0 {
            // Checked on insert, so this cannot fail.
            let _ = switches.set(key, value);
        }
    }
}

/// What a tool's own configuration changes in its profile.
///
/// This is the extension point for profiles tuned from tool configs: the
/// Obsidian profile reads `.obsidian/app.json` (milestone 5), MkDocs reads
/// `mkdocs.yml` `markdown_extensions` (1.2). Reading those files belongs to
/// their profiles; the compiler only applies the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tuning {
    /// Obsidian `app.json` `strictLineBreaks`: soft breaks render as spaces.
    pub strict_line_breaks: Option<bool>,
}

impl Tuning {
    fn apply(self, id: ProfileId, switches: &mut Switches) {
        if id == ProfileId::Obsidian
            && let Some(strict) = self.strict_line_breaks
        {
            switches.soft_break = if strict {
                crate::switches::SoftBreak::Space
            } else {
                crate::switches::SoftBreak::Br
            };
        }
    }
}

/// A block-level extension rule. CommonMark's own block rules are always
/// on and are not listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockRule {
    /// YAML or TOML front matter at byte 0.
    FrontMatter,
    /// `:::` fenced divs.
    FencedDiv,
    /// Pipe tables (interrupt a paragraph with a delimiter row).
    Table,
    /// `[^x]:` footnote definitions.
    FootnoteDefinition,
    /// `Term` / `: definition`.
    DefinitionList,
    /// `*[X]: …`.
    Abbreviation,
    /// A `[TOC]` paragraph.
    TocMarker,
}

/// A block-quote transform (`> [!x]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContainerTransform {
    Alert,
    Callout,
}

/// An inline extension rule. CommonMark's inlines are always on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InlineRule {
    /// `~x~` and/or `~~x~~` as strikethrough.
    Strikethrough,
    /// `~x~` as subscript.
    Subscript,
    /// `^x^`.
    Superscript,
    /// `==x==`.
    Highlight,
    /// `++x++`.
    Insert,
    /// `[^x]` references.
    FootnoteReference,
    /// `^[…]`.
    InlineFootnote,
    /// `[[…]]`.
    Wikilink,
    /// `![[…]]`.
    Embed,
    /// `%%…%%`.
    Comment,
    /// `#tag`.
    Tag,
    /// `^block-id`.
    BlockId,
    /// Dollar math.
    Math,
    /// `\(…\)` and `\[…\]`.
    BracketMath,
    /// `:shortcode:`.
    Emoji,
    /// `{…}` attributes.
    Attributes,
    /// `@user`, `#123`, commit SHAs.
    GitHubReference,
    /// Curly quotes, dashes, ellipses.
    SmartPunctuation,
    /// Bare URLs (a post-processor over text).
    ExtendedAutolink,
    /// `[ ]` task boxes (a property of list items).
    TaskListItem,
}

/// The bytes that start CommonMark inlines.
const COMMONMARK_TRIGGERS: &[u8] = b"\\`*_[]!<&\n";

/// A profile ready for the parser: switch values, the extension rules it
/// enables, and the inline trigger table.
#[derive(Clone, PartialEq, Eq)]
pub struct CompiledProfile {
    pub id: ProfileId,
    pub revision: &'static str,
    pub switches: Switches,
    /// The switches whose values differ from the definition (tuning and
    /// overrides), in matrix order.
    pub changed: Vec<&'static str>,
    block_rules: Vec<BlockRule>,
    transform: Option<ContainerTransform>,
    inline_rules: Vec<InlineRule>,
    triggers: [bool; 256],
}

impl fmt::Debug for CompiledProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompiledProfile")
            .field("id", &self.id)
            .field("revision", &self.revision)
            .field("changed", &self.changed)
            .field("block_rules", &self.block_rules)
            .field("transform", &self.transform)
            .field("inline_rules", &self.inline_rules)
            .finish_non_exhaustive()
    }
}

impl CompiledProfile {
    /// The block extension rules, in the order the parser tries them.
    #[must_use]
    pub fn block_rules(&self) -> &[BlockRule] {
        &self.block_rules
    }

    /// The `> [!x]` transform, if any.
    #[must_use]
    pub fn container_transform(&self) -> Option<ContainerTransform> {
        self.transform
    }

    /// The inline extension rules.
    #[must_use]
    pub fn inline_rules(&self) -> &[InlineRule] {
        &self.inline_rules
    }

    #[must_use]
    pub fn has_block(&self, rule: BlockRule) -> bool {
        self.block_rules.contains(&rule)
    }

    #[must_use]
    pub fn has_inline(&self, rule: InlineRule) -> bool {
        self.inline_rules.contains(&rule)
    }

    /// Whether byte `b` can start an inline in this profile.
    #[must_use]
    pub fn is_trigger(&self, b: u8) -> bool {
        self.triggers[usize::from(b)]
    }

    /// The trigger bytes, ascending.
    #[must_use]
    pub fn trigger_bytes(&self) -> Vec<u8> {
        (0..=255u8).filter(|&b| self.is_trigger(b)).collect()
    }
}

/// Compiles built-in profile `id` with no tuning and no overrides.
#[must_use]
pub fn compile_builtin(id: ProfileId) -> CompiledProfile {
    compile(id, Tuning::default(), &SwitchOverrides::new())
}

/// Compiles built-in profile `id`: its switches, then `tuning`, then
/// `overrides`.
#[must_use]
pub fn compile(id: ProfileId, tuning: Tuning, overrides: &SwitchOverrides) -> CompiledProfile {
    let def = definition(id);
    let mut switches = def.switches;
    tuning.apply(id, &mut switches);
    overrides.apply(&mut switches);
    let changed = switches.differences(&def.switches);

    let s = &switches;
    let mut block_rules = Vec::new();
    let mut push_block = |on: bool, rule| {
        if on {
            block_rules.push(rule);
        }
    };
    push_block(
        s.front_matter.yaml || s.front_matter.toml,
        BlockRule::FrontMatter,
    );
    push_block(s.fenced_divs, BlockRule::FencedDiv);
    push_block(s.tables, BlockRule::Table);
    push_block(s.footnotes, BlockRule::FootnoteDefinition);
    push_block(s.definition_lists, BlockRule::DefinitionList);
    push_block(s.abbreviations, BlockRule::Abbreviation);
    push_block(s.toc_marker, BlockRule::TocMarker);

    let transform = match s.admonitions {
        Admonitions::None => None,
        Admonitions::Alerts => Some(ContainerTransform::Alert),
        Admonitions::Callouts => Some(ContainerTransform::Callout),
    };

    let mut inline_rules = Vec::new();
    let mut triggers = [false; 256];
    for &b in COMMONMARK_TRIGGERS {
        triggers[usize::from(b)] = true;
    }
    let mut push = |on: bool, rule, bytes: &[u8]| {
        if on {
            inline_rules.push(rule);
            for &b in bytes {
                triggers[usize::from(b)] = true;
            }
        }
    };
    push(
        s.tilde_double || s.tilde_single == TildeSingle::Strike,
        InlineRule::Strikethrough,
        b"~",
    );
    push(
        s.tilde_single == TildeSingle::Subscript,
        InlineRule::Subscript,
        b"~",
    );
    push(s.superscript, InlineRule::Superscript, b"^");
    push(s.highlight, InlineRule::Highlight, b"=");
    push(s.insert, InlineRule::Insert, b"+");
    push(s.footnotes, InlineRule::FootnoteReference, b"[");
    push(s.inline_footnotes, InlineRule::InlineFootnote, b"^");
    push(s.wikilinks, InlineRule::Wikilink, b"[");
    push(s.embeds, InlineRule::Embed, b"!");
    push(s.comments, InlineRule::Comment, b"%");
    push(s.tags, InlineRule::Tag, b"#");
    push(s.block_ids, InlineRule::BlockId, b"^");
    push(s.math != Math::Off, InlineRule::Math, b"$");
    push(s.math_brackets, InlineRule::BracketMath, b"\\");
    push(s.emoji, InlineRule::Emoji, b":");
    push(s.attributes, InlineRule::Attributes, b"{");
    push(s.github_references, InlineRule::GitHubReference, b"@#");
    push(s.smart_punctuation, InlineRule::SmartPunctuation, b"\"'-.");
    // A post-processor over text nodes: no trigger byte.
    push(s.extended_autolinks, InlineRule::ExtendedAutolink, b"");
    push(
        s.task_lists != crate::switches::TaskLists::Off,
        InlineRule::TaskListItem,
        b"",
    );

    CompiledProfile {
        id,
        revision: def.revision,
        switches,
        changed,
        block_rules,
        transform,
        inline_rules,
        triggers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::switches::{FenceMeta, HtmlMarkdown, RawHtml, Slugs, SoftBreak, TaskLists};

    #[test]
    fn commonmark_has_no_extensions() {
        let p = compile_builtin(ProfileId::CommonMark);
        assert_eq!(p.switches, Switches::COMMONMARK);
        assert!(p.block_rules().is_empty());
        assert!(p.inline_rules().is_empty());
        assert_eq!(p.container_transform(), None);
        assert_eq!(p.trigger_bytes(), b"\n!&*<[\\]_`".to_vec());
        assert!(p.changed.is_empty());
    }

    #[test]
    fn gfm_file_is_commonmark_plus_github() {
        let p = compile_builtin(ProfileId::GfmFile);
        let s = p.switches;
        assert_eq!(s.soft_break, SoftBreak::Space);
        assert_eq!(s.raw_html, RawHtml::GitHub);
        assert!(s.tables && s.table_min_dashes == 1);
        assert_eq!(s.task_lists, TaskLists::Standard);
        assert_eq!(s.tilde_single, TildeSingle::Strike);
        assert!(s.tilde_double && s.footnotes && s.emoji && s.extended_autolinks);
        assert!(!s.superscript && !s.highlight && !s.wikilinks && !s.inline_footnotes);
        assert_eq!(s.math, Math::GitHub);
        assert_eq!(s.slugs, Slugs::GitHub);
        assert_eq!(s.fence_meta, FenceMeta::Ignored);
        assert!(s.front_matter.yaml && !s.front_matter.toml);
        assert_eq!(
            p.block_rules(),
            [
                BlockRule::FrontMatter,
                BlockRule::Table,
                BlockRule::FootnoteDefinition
            ]
        );
        assert_eq!(p.container_transform(), Some(ContainerTransform::Alert));
        assert!(p.has_inline(InlineRule::Strikethrough));
        assert!(!p.has_inline(InlineRule::Subscript));
        assert!(p.is_trigger(b'~') && p.is_trigger(b'$') && p.is_trigger(b':'));
        assert!(!p.is_trigger(b'=') && !p.is_trigger(b'^'));
    }

    #[test]
    fn gfm_comment_differs_from_gfm_file_only_where_the_matrix_says() {
        let file = compile_builtin(ProfileId::GfmFile).switches;
        let comment = compile_builtin(ProfileId::GfmComment).switches;
        assert_eq!(
            comment.differences(&file),
            ["soft_break", "slugs", "github_references", "colour_chips"]
        );
        assert_eq!(comment.soft_break, SoftBreak::Br);
        assert_eq!(comment.slugs, Slugs::None);
    }

    #[test]
    fn extended_is_the_superset() {
        let p = compile_builtin(ProfileId::Extended);
        let s = p.switches;
        assert_eq!(s.tilde_single, TildeSingle::Subscript);
        assert!(s.tilde_double && s.superscript && s.highlight && s.insert);
        assert!(s.fenced_divs && s.attributes && s.definition_lists && s.abbreviations);
        assert!(s.wikilinks && s.embeds && s.inline_footnotes && s.toc_marker);
        assert!(!s.comments && !s.tags && !s.block_ids);
        assert!(!s.smart_punctuation, "an option, default off");
        assert!(s.front_matter.yaml && s.front_matter.toml);
        assert_eq!(s.html_markdown, HtmlMarkdown::CommonMark);
        assert_eq!(s.fence_meta, FenceMeta::Rich);
        assert!(p.has_inline(InlineRule::Strikethrough) && p.has_inline(InlineRule::Subscript));
        assert_eq!(
            p.block_rules(),
            [
                BlockRule::FrontMatter,
                BlockRule::FencedDiv,
                BlockRule::Table,
                BlockRule::FootnoteDefinition,
                BlockRule::DefinitionList,
                BlockRule::Abbreviation,
                BlockRule::TocMarker,
            ]
        );
        for b in *b"~^=+${:" {
            assert!(p.is_trigger(b), "{}", char::from(b));
        }
    }

    #[test]
    fn obsidian_is_defined_and_tuned_by_app_json() {
        let p = compile_builtin(ProfileId::Obsidian);
        assert_eq!(p.switches.soft_break, SoftBreak::Br);
        assert_eq!(p.switches.html_markdown, HtmlMarkdown::Never);
        assert_eq!(p.switches.refdef_max_indent, 0);
        assert_eq!(p.container_transform(), Some(ContainerTransform::Callout));
        let strict = compile(
            ProfileId::Obsidian,
            Tuning {
                strict_line_breaks: Some(true),
            },
            &SwitchOverrides::new(),
        );
        assert_eq!(strict.switches.soft_break, SoftBreak::Space);
        assert_eq!(strict.changed, ["soft_break"]);
        // Tuning is per tool: it does not touch other profiles.
        let ext = compile(
            ProfileId::Extended,
            Tuning {
                strict_line_breaks: Some(false),
            },
            &SwitchOverrides::new(),
        );
        assert_eq!(ext.switches, compile_builtin(ProfileId::Extended).switches);
    }

    #[test]
    fn overrides_apply_last_and_recompute_rules() {
        let mut overrides = SwitchOverrides::new();
        overrides
            .insert("tilde_single", SwitchValue::Word("strike".into()))
            .expect("valid");
        overrides
            .insert("smart_punctuation", SwitchValue::Bool(true))
            .expect("valid");
        overrides
            .insert("smart_punctuation", SwitchValue::Bool(true))
            .expect("valid");
        assert!(overrides.insert("nope", SwitchValue::Bool(true)).is_err());
        let p = compile(ProfileId::Extended, Tuning::default(), &overrides);
        assert_eq!(p.switches.tilde_single, TildeSingle::Strike);
        assert!(!p.has_inline(InlineRule::Subscript));
        assert!(p.has_inline(InlineRule::SmartPunctuation));
        assert!(p.is_trigger(b'"'));
        assert_eq!(p.changed, ["tilde_single", "smart_punctuation"]);

        let mut tables = SwitchOverrides::new();
        tables
            .insert("tables", SwitchValue::Bool(true))
            .expect("valid");
        let cm = compile(ProfileId::CommonMark, Tuning::default(), &tables);
        assert_eq!(cm.block_rules(), [BlockRule::Table]);
    }
}
