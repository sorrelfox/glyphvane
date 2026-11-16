// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The built-in profile definitions: metadata and the switch values of the
//! switch matrix, one column per profile.
//!
//! A value marked "verify" in the matrix is the documented behavior, still
//! to be checked against the real tool; changing it is a new revision.

use crate::id::ProfileId;
use crate::switches::{
    Admonitions, FenceMeta, FrontMatter, HtmlMarkdown, Math, RawHtml, Slugs, SoftBreak, Switches,
    TaskLists, TildeSingle, WikiPipe,
};

/// A built-in profile: who it is and its switch values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileDef {
    pub id: ProfileId,
    /// The name the profile has in the interface's lists.
    pub name: &'static str,
    /// The revision, such as `obsidian-1.13`. Behavior changes make a new
    /// revision.
    pub revision: &'static str,
    /// What the profile follows upstream.
    pub upstream: &'static str,
    /// The upstream reference documentation.
    pub reference_url: &'static str,
    pub switches: Switches,
}

const COMMONMARK: ProfileDef = ProfileDef {
    id: ProfileId::CommonMark,
    name: "CommonMark",
    revision: "commonmark-0.31.2",
    upstream: "CommonMark 0.31.2",
    reference_url: "https://spec.commonmark.org/0.31.2/",
    switches: Switches::COMMONMARK,
};

const GFM_FILE_SWITCHES: Switches = Switches {
    raw_html: RawHtml::GitHub,
    tables: true,
    table_min_dashes: 1,
    task_lists: TaskLists::Standard,
    front_matter: FrontMatter {
        yaml: true,
        toml: false,
    },
    admonitions: Admonitions::Alerts,
    footnotes: true,
    fence_meta: FenceMeta::Ignored,
    mermaid: true,
    math: Math::GitHub,
    slugs: Slugs::GitHub,
    tilde_single: TildeSingle::Strike,
    tilde_double: true,
    emoji: true,
    extended_autolinks: true,
    ..Switches::COMMONMARK
};

const GFM_FILE: ProfileDef = ProfileDef {
    id: ProfileId::GfmFile,
    name: "GFM (files)",
    revision: "gfm-file-2026.11",
    upstream: "GFM 0.29 and github.com file rendering as observed",
    reference_url: "https://github.github.com/gfm/",
    switches: GFM_FILE_SWITCHES,
};

const GFM_COMMENT: ProfileDef = ProfileDef {
    id: ProfileId::GfmComment,
    name: "GFM (comments)",
    revision: "gfm-comment-2026.11",
    upstream: "GFM 0.29 and github.com comment rendering as observed",
    reference_url: "https://github.github.com/gfm/",
    switches: Switches {
        soft_break: SoftBreak::Br,
        slugs: Slugs::None,
        github_references: true,
        colour_chips: true,
        ..GFM_FILE_SWITCHES
    },
};

const OBSIDIAN: ProfileDef = ProfileDef {
    id: ProfileId::Obsidian,
    name: "Obsidian",
    revision: "obsidian-1.13",
    upstream: "Obsidian 1.13",
    reference_url: "https://help.obsidian.md/obsidian-flavored-markdown",
    switches: Switches {
        // `strictLineBreaks` in `app.json` turns this back to a space.
        soft_break: SoftBreak::Br,
        raw_html: RawHtml::Sanitized,
        html_markdown: HtmlMarkdown::Never,
        tables: true,
        // Verify.
        table_min_dashes: 2,
        task_lists: TaskLists::AnyCharacter,
        refdef_max_indent: 0,
        front_matter: FrontMatter {
            yaml: true,
            toml: false,
        },
        admonitions: Admonitions::Callouts,
        footnotes: true,
        footnote_indent: 2,
        inline_footnotes: true,
        math: Math::Obsidian,
        slugs: Slugs::HeadingText,
        // Verify.
        tilde_single: TildeSingle::Literal,
        tilde_double: true,
        highlight: true,
        wikilinks: true,
        embeds: true,
        wiki_pipe: WikiPipe::TargetTitle,
        comments: true,
        tags: true,
        block_ids: true,
        // Verify.
        emoji: false,
        extended_autolinks: true,
        ..Switches::COMMONMARK
    },
};

const EXTENDED: ProfileDef = ProfileDef {
    id: ProfileId::Extended,
    name: "Extended",
    revision: "extended-1",
    upstream: "Markdig's extensions, Pandoc's inline footnotes and spans, GitHub alerts",
    reference_url: "https://github.com/xoofx/markdig/tree/master/src/Markdig.Tests/Specs",
    switches: Switches {
        tables: true,
        table_min_dashes: 1,
        task_lists: TaskLists::Standard,
        front_matter: FrontMatter {
            yaml: true,
            toml: true,
        },
        admonitions: Admonitions::Alerts,
        footnotes: true,
        inline_footnotes: true,
        definition_lists: true,
        abbreviations: true,
        fenced_divs: true,
        attributes: true,
        toc_marker: true,
        fence_meta: FenceMeta::Rich,
        mermaid: true,
        math: Math::GitHub,
        math_brackets: false,
        slugs: Slugs::GitHub,
        tilde_single: TildeSingle::Subscript,
        tilde_double: true,
        superscript: true,
        highlight: true,
        insert: true,
        wikilinks: true,
        embeds: true,
        wiki_pipe: WikiPipe::TargetTitle,
        emoji: true,
        extended_autolinks: true,
        smart_punctuation: false,
        ..Switches::COMMONMARK
    },
};

/// The definition of built-in profile `id`.
#[must_use]
pub fn definition(id: ProfileId) -> &'static ProfileDef {
    match id {
        ProfileId::CommonMark => &COMMONMARK,
        ProfileId::GfmFile => &GFM_FILE,
        ProfileId::GfmComment => &GFM_COMMENT,
        ProfileId::Obsidian => &OBSIDIAN,
        ProfileId::Extended => &EXTENDED,
    }
}
