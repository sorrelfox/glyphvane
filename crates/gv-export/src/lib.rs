// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! HTML exporter, Typst document builder and print preparation.
//!
//! # HTML export
//!
//! [`export_html`] turns a document's [`RenderTree`] into one standalone
//! HTML file:
//!
//! 1. **Emit.** The render tree's nodes and their containers become HTML
//!    (headings with their anchors, lists and tasks, quotes, GitHub alerts,
//!    tables, code with highlighting classes, footnotes, `<details>`,
//!    front matter as a property table). Raw HTML from the document is
//!    passed on as written. Formulas (RaTeX) and diagrams (through a
//!    [`DiagramRenderer`]) become inline SVG, with their text as paths;
//!    task checkboxes and these SVGs are *trusted* fragments, held aside
//!    behind random placeholders.
//! 2. **Sanitize.** The whole body goes through ammonia once, so tags that
//!    raw HTML splits across Markdown blocks pair as a browser pairs them.
//!    The allowlist is the Reading view's (elements, attributes, the
//!    `style` subset, which GitHub profiles drop), and an attribute filter
//!    classifies every URL with [`gv_vault::url::classify`]: links that
//!    are not allowed without asking (network paths, `file://host`,
//!    unknown schemes) lose their target, script and data URLs go, ids get
//!    the `user-content-` prefix and fragment links follow it.
//! 3. **Rewrite and embed.** Relative links to Markdown files point to the
//!    `.html` beside them; unresolved links become plain text with a class.
//!    Local images are read and inlined as `data:` URIs (or listed as
//!    [`Asset`]s for an `assets/` folder) within size caps; remote images
//!    stay plain references and are never fetched.
//! 4. **Wrap.** The trusted fragments replace their placeholders, and the
//!    body goes into a document with a stylesheet built from the light
//!    theme's tokens ([`ExportTheme`]), a Content Security Policy that
//!    allows no script, and no timestamp, so the same input always gives
//!    the same bytes.
//!
//! [`rich_copy`] runs the same pipeline over a selection and returns an
//! HTML fragment and its plain-text alternative, for the clipboard.
//!
//! # PDF export
//!
//! [`export_typst`] turns the render tree into a generated Typst document
//! (see [`pdf`]) with its images and formulas as assets; the separate
//! helper executable compiles it to PDF. Print uses the same PDF.

mod assets;
mod css;
mod emit;
mod escape;
mod math;
pub mod pdf;
mod sanitize;

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gv_render_tree::{CopyOptions, RenderTree};

pub use assets::{Asset, ImageMode, ImageNote, Limits};
pub use css::{ExportTheme, THEME_TOKENS};
pub use emit::wikilink_href;
pub use pdf::{PageOptions, Paper, TypstDocument, export_typst};
pub use sanitize::{ID_PREFIX, rewrite_link};

/// Draws a Mermaid diagram as a standalone SVG (with its text as paths).
/// `None` when it cannot be drawn: the export then shows its source.
pub trait DiagramRenderer: Send + Sync {
    fn svg(&self, source: &str) -> Option<String>;
}

/// Finds the file a wikilink target names.
pub trait WikiResolver: Send + Sync {
    /// The file `target` (without its `#anchor`) resolves to, or `None`.
    fn resolve(&self, target: &str) -> Option<PathBuf>;
}

/// What an export needs besides the document.
#[derive(Clone)]
pub struct ExportOptions {
    /// The page title; by default the first heading, else the file name.
    pub title: Option<String>,
    /// The document's file: relative links and images are resolved from
    /// its folder.
    pub doc_path: Option<PathBuf>,
    /// The vault root: local images outside it are not embedded.
    pub root: Option<PathBuf>,
    pub images: ImageMode,
    pub limits: Limits,
    /// Drop every `style` attribute of raw HTML (GitHub profiles, as
    /// GitHub does); otherwise the validated subset is kept.
    pub strip_style: bool,
    /// Add a table of contents from the headings.
    pub toc: bool,
    pub theme: ExportTheme,
    pub diagrams: Option<Arc<dyn DiagramRenderer>>,
    pub wiki: Option<Arc<dyn WikiResolver>>,
}

impl std::fmt::Debug for ExportOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportOptions")
            .field("title", &self.title)
            .field("doc_path", &self.doc_path)
            .field("root", &self.root)
            .field("images", &self.images)
            .field("strip_style", &self.strip_style)
            .field("toc", &self.toc)
            .finish_non_exhaustive()
    }
}

impl ExportOptions {
    /// Options for the document at `doc_path` (if it has a file), with
    /// `theme`'s colours and no diagram renderer.
    pub fn new(doc_path: Option<PathBuf>, theme: ExportTheme) -> Self {
        Self {
            title: None,
            doc_path,
            root: None,
            images: ImageMode::Embed,
            limits: Limits::default(),
            strip_style: false,
            toc: false,
            theme,
            diagrams: None,
            wiki: None,
        }
    }

    /// The folder relative paths are resolved from.
    fn base_dir(&self) -> Option<&Path> {
        self.doc_path.as_deref().and_then(Path::parent)
    }
}

/// An exported document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    /// The whole HTML file.
    pub html: String,
    /// Files to copy beside the HTML file ([`ImageMode::Assets`]).
    pub assets: Vec<Asset>,
    /// Images that were not embedded, and why.
    pub notes: Vec<ImageNote>,
}

/// A selection copied as rich text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichCopy {
    /// A sanitized HTML fragment.
    pub html: String,
    /// The plain-text alternative.
    pub text: String,
}

/// Exports the document of `tree` as a standalone HTML file.
pub fn export_html(tree: &RenderTree, opts: &ExportOptions) -> Export {
    let body = emit::emit(tree, opts, None);
    let title = opts
        .title
        .clone()
        .or_else(|| body.title.clone())
        .or_else(|| {
            opts.doc_path
                .as_deref()
                .and_then(Path::file_stem)
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "Untitled".to_owned());
    let store = Arc::new(assets::Store::new(opts, opts.images));
    let clean = sanitize::clean(&body.parts(), opts, &store);
    let html_body = body.restore(&clean);
    let (assets, notes) = store.finish();
    let mut html = String::with_capacity(html_body.len() + 8192);
    html.push_str("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    html.push_str("<meta http-equiv=\"Content-Security-Policy\" content=\"");
    html.push_str(sanitize::CSP);
    html.push_str("\">\n<meta name=\"generator\" content=\"Glyphvane\">\n<title>");
    escape::text(&mut html, &title);
    html.push_str("</title>\n<style>\n");
    html.push_str(&css::stylesheet(&opts.theme));
    html.push_str("</style>\n</head>\n<body>\n<main class=\"gv-document\">\n");
    if opts.toc && !body.headings.is_empty() {
        html.push_str(&emit::toc(&body.headings));
    }
    html.push_str(&html_body);
    if !html_body.ends_with('\n') {
        html.push('\n');
    }
    html.push_str("</main>\n</body>\n</html>\n");
    Export {
        html,
        assets,
        notes,
    }
}

/// The source range `range` of the document of `tree` as rich text: a
/// sanitized HTML fragment (images embedded) and the plain text the
/// Reading view copies.
pub fn rich_copy(tree: &RenderTree, range: Range<usize>, opts: &ExportOptions) -> RichCopy {
    let body = emit::emit(tree, opts, Some(range.clone()));
    // The clipboard carries one fragment: images are always inlined.
    let store = Arc::new(assets::Store::new(opts, ImageMode::Embed));
    let clean = sanitize::clean(&body.parts(), opts, &store);
    let html = body.restore(&clean);
    RichCopy {
        html: html.trim_end().to_owned(),
        text: tree.plain_text(range, CopyOptions::default()),
    }
}
