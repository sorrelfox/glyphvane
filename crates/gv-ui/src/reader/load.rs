// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Loading a document for the Reading view: read, decode, parse, model and
//! render tree, all off the UI thread. The UI thread only lays out what is
//! on screen.
//!
//! A Reading view that follows an editor (the Split view's preview) builds
//! from the editor's syntax tree instead of parsing ([`build_tree`]), and
//! then updates the model and the render tree incrementally from the
//! reparses' changes ([`update_tree`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gv_model::DocModel;
use gv_profile::{CompiledProfile, ProfileId};
use gv_render_tree::{LinkTarget, RenderOptions, RenderTree, Sources, UpdateReport};
use gv_syntax::{Changes, ParseOptions, SyntaxTree};
use gv_text::{DecodeStatus, TextSnapshot};

use super::links::Resolver;
use super::media::{ImageSizes, ImageTargets};
use super::startup;

/// A loaded document. Cloning is cheap.
#[derive(Debug, Clone)]
pub(crate) struct Loaded {
    pub path: PathBuf,
    /// The text exactly as decoded.
    pub text: Arc<str>,
    pub syntax: SyntaxTree,
    pub model: DocModel,
    /// Shared with find in page's background searches.
    pub tree: Arc<RenderTree>,
    /// Why the text is not an exact decode, if it is not (undecodable bytes
    /// show as U+FFFD).
    pub lossy: Option<String>,
    pub timings: Timings,
    /// The editor's text the document was built from, when it follows an
    /// editor: its line index serves scroll sync.
    pub lines: Option<TextSnapshot>,
    /// The profile it was read with.
    pub profile: ProfileId,
    /// What its images resolved to (shared with the builds that follow an
    /// editor).
    pub images: Arc<ImageTargets>,
}

/// Where loading spent its time.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Timings {
    pub bytes: usize,
    pub read: Duration,
    pub decode: Duration,
    pub parse: Duration,
    pub model: Duration,
    pub tree: Duration,
}

impl Timings {
    pub fn total(&self) -> Duration {
        self.read + self.decode + self.parse + self.model + self.tree
    }
}

/// Reads and builds the document at `path` in `profile`. Link statuses
/// come from `links`. Errors are messages for the reader.
pub(crate) fn load(
    path: &Path,
    links: &Resolver,
    options: RenderOptions,
    profile: &CompiledProfile,
) -> Result<Loaded, String> {
    // The start-up trace follows the documents loaded while it runs.
    let trace = startup::enabled().then(|| {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let mut timings = Timings::default();
    let t = Instant::now();
    let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
    timings.read = t.elapsed();
    timings.bytes = bytes.len();
    mark(
        trace.as_deref(),
        format_args!("read, {} bytes", bytes.len()),
    );

    let t = Instant::now();
    let decoded = gv_text::decode(&bytes).map_err(|err| err.to_string())?;
    drop(bytes);
    let lossy = match decoded.status {
        DecodeStatus::Exact => None,
        DecodeStatus::Lossy(reason) => Some(format!(
            "decoded as {} with losses ({reason:?})",
            decoded.format.encoding
        )),
    };
    let text: Arc<str> = Arc::from(decoded.text);
    timings.decode = t.elapsed();
    mark(trace.as_deref(), format_args!("decoded"));
    Ok(build(
        path,
        text,
        lossy,
        timings,
        trace.as_deref(),
        links,
        options,
        profile,
    ))
}

/// Builds the document at `path` from text that is already decoded: the
/// editor's text, handed to the Reading view when the tab returns to
/// Preview before the text is saved. With `crop`, only the items of that
/// part of the text are shown, the rest being its context (a Live Preview
/// block: [`RenderTree::keep_items_in`]).
pub(crate) fn load_text(
    path: &Path,
    text: Arc<str>,
    links: &Resolver,
    options: RenderOptions,
    profile: &CompiledProfile,
    crop: Option<std::ops::Range<usize>>,
) -> Loaded {
    let timings = Timings {
        bytes: text.len(),
        ..Timings::default()
    };
    let mut loaded = build(path, text, None, timings, None, links, options, profile);
    if let Some(crop) = crop
        && let Some(tree) = Arc::get_mut(&mut loaded.tree)
    {
        tree.keep_items_in(crop);
    }
    loaded
}

/// Parses `text` in `profile` and builds its model and render tree.
#[allow(clippy::too_many_arguments)]
fn build(
    path: &Path,
    text: Arc<str>,
    lossy: Option<String>,
    mut timings: Timings,
    trace: Option<&str>,
    links: &Resolver,
    options: RenderOptions,
    profile: &CompiledProfile,
) -> Loaded {
    let t = Instant::now();
    let parse_options = ParseOptions::from_profile(profile);
    let syntax = gv_syntax::parse_with(&*text, parse_options);
    timings.parse = t.elapsed();
    mark(trace, format_args!("parsed"));

    let t = Instant::now();
    let model = DocModel::build(&syntax, &*text);
    timings.model = t.elapsed();
    mark(trace, format_args!("modelled"));

    let t = Instant::now();
    let images = Arc::new(ImageTargets::new(links.root().to_path_buf()));
    let sizes = ImageSizes {
        resolver: links.for_images(),
        targets: &images,
    };
    let sources = Sources {
        links,
        images: &sizes,
    };
    let tree = RenderTree::build(&syntax, &model, &*text, options, sources);
    timings.tree = t.elapsed();
    mark(trace, format_args!("render tree built"));

    Loaded {
        path: path.to_path_buf(),
        text,
        syntax,
        model,
        tree: Arc::new(tree),
        lossy,
        timings,
        lines: None,
        profile: profile.id,
        images,
    }
}

/// Builds the document at `path` (in `profile`) from an editor's text and
/// its syntax tree: the model and the render tree, without parsing.
pub(crate) fn build_tree(
    path: &Path,
    syntax: SyntaxTree,
    text: TextSnapshot,
    links: &Resolver,
    options: RenderOptions,
    profile: ProfileId,
) -> Loaded {
    let mut timings = Timings {
        bytes: text.len(),
        ..Timings::default()
    };
    let t = Instant::now();
    let model = DocModel::build(&syntax, &text);
    timings.model = t.elapsed();
    let t = Instant::now();
    let images = Arc::new(ImageTargets::new(links.root().to_path_buf()));
    let sizes = ImageSizes {
        resolver: links.for_images(),
        targets: &images,
    };
    let sources = Sources {
        links,
        images: &sizes,
    };
    let tree = RenderTree::build(&syntax, &model, &text, options, sources);
    warm_index(&tree);
    timings.tree = t.elapsed();
    Loaded {
        path: path.to_path_buf(),
        text: Arc::from(text.text_for_range(0..text.len()).unwrap_or_default()),
        syntax,
        model,
        tree: Arc::new(tree),
        lossy: None,
        timings,
        lines: Some(text),
        profile,
        images,
    }
}

/// `base` updated to an editor's text and its syntax tree, which the
/// reparses `changes` made from the tree `base` was built from: the blocks
/// outside the reparsed window keep their records and render nodes. The
/// report says which items were replaced.
pub(crate) fn update_tree(
    base: &Loaded,
    syntax: SyntaxTree,
    text: TextSnapshot,
    changes: &Changes,
    links: &Resolver,
) -> (Loaded, UpdateReport) {
    let mut timings = Timings {
        bytes: text.len(),
        ..Timings::default()
    };
    let t = Instant::now();
    let model = base.model.update(&syntax, &text, changes);
    timings.model = t.elapsed();
    let t = Instant::now();
    let sizes = ImageSizes {
        resolver: links.for_images(),
        targets: &base.images,
    };
    let sources = Sources {
        links,
        images: &sizes,
    };
    let mut tree = RenderTree::clone(&base.tree);
    let report = tree.update(&syntax, &model, &text, changes, sources);
    warm_index(&tree);
    timings.tree = t.elapsed();
    let loaded = Loaded {
        path: base.path.clone(),
        text: Arc::from(text.text_for_range(0..text.len()).unwrap_or_default()),
        syntax,
        model,
        tree: Arc::new(tree),
        lossy: None,
        timings,
        lines: Some(text),
        profile: base.profile,
        images: Arc::clone(&base.images),
    };
    (loaded, report)
}

/// Builds the tree's item index here, off the UI thread, rather than at
/// its first lookup.
fn warm_index(tree: &RenderTree) {
    if let Some(first) = tree.items().first() {
        let _ = tree.index_of(first.id);
    }
}

/// A start-up trace milestone of loading the document `name`.
fn mark(name: Option<&str>, step: std::fmt::Arguments<'_>) {
    if let Some(name) = name {
        startup::mark(&format!("document {step} ({name})"));
    }
}

/// Re-asks the status of every link the index can change (the index
/// changed); image sizes are not known yet and URLs keep their status.
/// Returns the ids of the items that changed.
pub(crate) fn refresh(doc: &mut Loaded, links: &Resolver) -> Vec<gv_render_tree::ItemId> {
    let sizes = ImageSizes {
        resolver: links.for_images(),
        targets: &doc.images,
    };
    let sources = Sources {
        links,
        images: &sizes,
    };
    let which = |t: &LinkTarget<'_>| !t.image && links.depends_on_index(t);
    Arc::make_mut(&mut doc.tree)
        .refresh_where(&doc.syntax, &*doc.text, sources, &which)
        .changed
}
