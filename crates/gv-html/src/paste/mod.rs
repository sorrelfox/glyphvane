// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! HTML → Markdown for paste.
//!
//! [`html_to_markdown`] converts clipboard HTML (browser selections,
//! Google Docs, Word, GitHub-rendered pages) into Markdown for one
//! profile. The input is untrusted: it is checked by the pre-parse guards,
//! parsed by html5ever into our own arena, and walked by an allowlist that
//! keeps text, structure and sanitized link and image URLs, and nothing
//! else.
//!
//! | HTML | Markdown |
//! |---|---|
//! | `h1`–`h6` | ATX heading (one line; bold implied, so not marked) |
//! | `p`, `div` and other blocks | paragraphs |
//! | `b strong`, `i em`, `font-weight` / `font-style` styles | `**strong**`, `*em*` |
//! | `s del strike`, `line-through` | `~~strike~~`; `<del>` in CommonMark |
//! | `sub sup`, `vertical-align` | `~x~` / `^x^` in the extended profile, else `<sub>` / `<sup>` |
//! | `mark` | `==x==` where highlights exist, else plain text |
//! | `code kbd samp tt`, monospace fonts on inline elements | code spans |
//! | `pre` | fenced code; language from `language-x`, `lang-x`, `highlight-source-x`, `brush: x`, `pre[lang]` |
//! | `a[href]` | `[text](url "title")`, URL sanitized, relative ones resolved against the base URL |
//! | `img` | `![alt](src "title")`; a source that may not load becomes its alt text |
//! | `ul ol li`, `start`, nesting; Word `mso-list` paragraphs | lists (tight unless an item holds several blocks) |
//! | `input[type=checkbox]` at the start of an item | `[ ]` / `[x]` task (escaped text in CommonMark) |
//! | `blockquote` | `>` quote; GitHub alerts become `> [!NOTE]` |
//! | `br`, `hr` | `\` hard break (two in a row split the paragraph), `***` |
//! | `table` | GFM pipe table; CommonMark gets a sanitized HTML table; single-cell and nested (layout) tables are unwrapped |
//! | MathML with a TeX annotation | `$tex$` where math exists, else a code span |
//! | `script style template head meta iframe object svg form controls …`, comments, `hidden`, `display:none` | dropped |
//!
//! Formatting that would not parse as Markdown in its context
//! (`foo**bar**baz`) is written as the equivalent inline HTML tag.

mod build;
mod emit;
mod escape;
mod ir;

use crate::guard::{self, GuardTrip};
use crate::profile::Profile;
use crate::url::parse_base;

use build::Builder;
use emit::Emitter;
use ir::{Block, Inline, Run, Style};

/// Options for [`html_to_markdown`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PasteOptions<'a> {
    /// The profile of the target document.
    pub profile: Profile,
    /// The page the HTML came from (the clipboard's source URL, when the
    /// platform provides one). Relative links and images are resolved
    /// against it; only an `http(s)` base is used.
    pub base_url: Option<&'a str>,
}

impl<'a> PasteOptions<'a> {
    /// Options for `profile`, without a base URL.
    #[must_use]
    pub fn new(profile: Profile) -> Self {
        PasteOptions {
            profile,
            base_url: None,
        }
    }

    /// Sets the base URL.
    #[must_use]
    pub fn with_base_url(mut self, base_url: Option<&'a str>) -> Self {
        self.base_url = base_url;
        self
    }
}

/// The result of [`convert_html`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Conversion {
    /// The Markdown, without a trailing newline; empty if nothing in the
    /// HTML could be kept.
    pub markdown: String,
    /// `Some` when the input exceeded a guard and only its text was kept.
    pub guard: Option<GuardTrip>,
}

/// Converts clipboard HTML to Markdown for `opts.profile`.
#[must_use]
pub fn html_to_markdown(html: &str, opts: PasteOptions<'_>) -> String {
    convert_html(html, opts).markdown
}

/// [`html_to_markdown`] with a report of whether the guards fired.
#[must_use]
pub fn convert_html(html: &str, opts: PasteOptions<'_>) -> Conversion {
    let html = strip_cf_html_header(html);
    if let Err(trip) = guard::check(html) {
        return Conversion {
            markdown: plain_text_markdown(&guard::plain_text(html), opts.profile),
            guard: Some(trip),
        };
    }
    let dom = crate::dom::Dom::parse(html);
    let (blocks, links) = Builder::new(&dom, parse_base(opts.base_url)).run();
    Conversion {
        markdown: Emitter::new(opts.profile, &links).document(&blocks),
        guard: None,
    }
}

/// Escapes plain text as Markdown paragraphs (one per non-empty line).
#[must_use]
pub fn plain_text_markdown(text: &str, profile: Profile) -> String {
    let blocks: Vec<Block> = text
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .map(|l| {
            Block::Para(vec![Run {
                item: Inline::Text(l),
                style: Style::default(),
            }])
        })
        .collect();
    Emitter::new(profile, &[]).document(&blocks)
}

/// A link destination for pasted text (`<…>` form when needed).
pub(crate) fn destination_for_paste(url: &str) -> String {
    escape::destination(url, escape::Mode::Para)
}

/// Windows puts a `Version:0.9` / `StartHTML:` header before CF_HTML.
fn strip_cf_html_header(html: &str) -> &str {
    if html.trim_start().starts_with("Version:")
        && let Some(lt) = html.find('<')
    {
        return &html[lt..];
    }
    html
}
