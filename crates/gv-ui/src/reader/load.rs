// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Loading a document for the Reading view: read, decode, parse, model and
//! render tree, all off the UI thread. The UI thread only lays out what is
//! on screen.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gv_model::DocModel;
use gv_render_tree::{LinkTarget, NoImageSizes, RenderOptions, RenderTree, Sources};
use gv_syntax::{ParseOptions, SyntaxTree};
use gv_text::DecodeStatus;

use super::links::{PROFILE, Resolver};

/// A loaded document. Cloning is cheap except for the render tree's item
/// list.
#[derive(Debug, Clone)]
pub(crate) struct Loaded {
    pub path: PathBuf,
    /// The text exactly as decoded.
    pub text: Arc<str>,
    pub syntax: SyntaxTree,
    pub model: DocModel,
    pub tree: RenderTree,
    /// Why the text is not an exact decode, if it is not (undecodable bytes
    /// show as U+FFFD).
    pub lossy: Option<String>,
    pub timings: Timings,
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

/// Reads and builds the document at `path`. Link statuses come from
/// `links`. Errors are messages for the reader.
pub(crate) fn load(
    path: &Path,
    links: &Resolver,
    options: RenderOptions,
) -> Result<Loaded, String> {
    let mut timings = Timings::default();
    let t = Instant::now();
    let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
    timings.read = t.elapsed();
    timings.bytes = bytes.len();

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

    let t = Instant::now();
    let parse_options = ParseOptions::for_profile(PROFILE.as_str()).unwrap_or_default();
    let syntax = gv_syntax::parse_with(&*text, parse_options);
    timings.parse = t.elapsed();

    let t = Instant::now();
    let model = DocModel::build(&syntax, &*text);
    timings.model = t.elapsed();

    let t = Instant::now();
    let sources = Sources {
        links,
        images: &NoImageSizes,
    };
    let tree = RenderTree::build(&syntax, &model, &*text, options, sources);
    timings.tree = t.elapsed();

    Ok(Loaded {
        path: path.to_path_buf(),
        text,
        syntax,
        model,
        tree,
        lossy,
        timings,
    })
}

/// Re-asks the status of every link the index can change (the index
/// changed); image sizes are not known yet and URLs keep their status.
/// Returns the ids of the items that changed.
pub(crate) fn refresh(doc: &mut Loaded, links: &Resolver) -> Vec<gv_render_tree::ItemId> {
    let sources = Sources {
        links,
        images: &NoImageSizes,
    };
    let which = |t: &LinkTarget<'_>| !t.image && links.depends_on_index(t);
    doc.tree
        .refresh_where(&doc.syntax, &*doc.text, sources, &which)
        .changed
}
