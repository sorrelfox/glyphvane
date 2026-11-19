// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! A Reading view that follows an editor: the Split view's preview pane.
//!
//! - [`Reader::follow`] takes the editor's syntax tree after a reparse
//!   ([`SyntaxHandoff`]). When the shown document was built from the text
//!   the hand-off's changes start at, the model and the render tree are
//!   updated incrementally off the UI thread ([`load::update_tree`]);
//!   otherwise they are built afresh from the tree, without parsing. One
//!   build runs at a time: the caller hands off again when it is done, and
//!   the editor composes the reparses in between.
//! - Installing an update splices the height index ([`Engine::splice`])
//!   and drops the layouts of the changed items only, so the view stays
//!   where it is and only edited blocks are shaped again.
//! - Scroll sync reads the top of the view as a place in an item
//!   ([`Reader::top_spot`]) and asks for one ([`Reader::sync_to`]),
//!   optionally with the caret's block to be shown whole. The caret's block
//!   gets a `state.hover` band ([`Reader::set_band`]).
//!
//! [`Engine::splice`]: super::engine::Engine::splice

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{AppContext as _, Context, Task};
use gv_render_tree::{ItemKind, RenderOptions, RenderTree, StyleTable, UpdateReport};
use gv_text::TextSnapshot;

use super::items::TreeItems;
use super::links::Resolver;
use super::load::{self, Loaded};
use super::state::{Hover, Press, ReaderLayout, State};
use super::{LoadStatus, Reader, ScrollTarget, index_key, jump};
use crate::editor::{SyntaxHandoff, TextStamp};

/// A place in an item's box, as a fraction, for scroll sync.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Spot {
    /// The top padding above the first item: -1 at its top, up to 0 where
    /// the first item starts.
    Lead(f32),
    /// The gap above the item's content: 0 at its top, 1 at the content.
    Gap(f32),
    /// The item's content: 0 at its top, 1 at its bottom.
    Content(f32),
}

/// What scroll sync asks of the next frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SyncRequest {
    /// The place to put at the top of the view.
    pub ix: usize,
    pub spot: Spot,
    /// A source offset whose block is to be shown whole (the caret).
    pub reveal: Option<usize>,
}

/// The state of following an editor.
#[derive(Default)]
pub(crate) struct Live {
    /// The editor text the shown document was built from.
    shown: Option<TextStamp>,
    /// The build running, if any.
    task: Option<Task<()>>,
    /// Builds installed: (incremental, from scratch).
    #[cfg(test)]
    pub counts: (u64, u64),
    /// The model and render tree times of the last build.
    #[cfg(test)]
    pub last: (std::time::Duration, std::time::Duration),
}

impl std::fmt::Debug for Live {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Live")
            .field("shown", &self.shown)
            .field("building", &self.task.is_some())
            .finish()
    }
}

impl Live {
    /// The shown document no longer comes from the editor (a file was
    /// opened).
    pub fn forget(&mut self) {
        self.shown = None;
        self.task = None;
    }

    /// The editor text the shown document was built from.
    pub fn shown_stamp(&self) -> Option<TextStamp> {
        self.shown
    }

    /// The profile changed: the next hand-off builds from scratch, and a
    /// build still running is dropped.
    pub fn restart(&mut self) {
        self.shown = None;
        self.task = None;
    }
}

/// What a build made: the document and, for an incremental update, the
/// items it replaced.
struct Built {
    doc: Loaded,
    report: Option<UpdateReport>,
    stamp: TextStamp,
}

impl Reader {
    /// A Reading view for the Split view's preview pane: padding `20 28 0`
    /// and the split type scale (body 14.5/23, H1 24/30).
    pub fn for_split(cx: &mut Context<'_, Self>) -> Self {
        // The split scale has the preview's fonts (only sizes differ), so
        // the font table stays.
        let mut reader = Self::new(cx);
        reader.state.borrow_mut().set_layout(ReaderLayout::Pane);
        reader.options = RenderOptions {
            styles: StyleTable::split(),
            ..RenderOptions::commonmark()
        }
        .with_profile(&reader.profile);
        reader
    }

    /// Whether a document from an editor is being built.
    pub(crate) fn is_building(&self) -> bool {
        self.live.task.is_some()
    }

    /// The editor text the shown document was built from.
    pub(crate) fn shown_stamp(&self) -> Option<TextStamp> {
        self.live.shown
    }

    /// Shows the editor's text from `handoff` as the document at `path`.
    /// The first document is scrolled to `target`, unless a jump was asked
    /// for already; later ones stay where the view is.
    pub(crate) fn follow(
        &mut self,
        path: PathBuf,
        handoff: SyntaxHandoff,
        target: Option<ScrollTarget>,
        cx: &mut Context<'_, Self>,
    ) {
        let SyntaxHandoff {
            tree,
            text,
            stamp,
            since,
        } = handoff;
        let (revision, base) = {
            let st = self.state.borrow();
            let base = match (since, self.live.shown, &st.doc) {
                (Some((from, changes)), Some(shown), Some(doc))
                    if from == shown && self.status == LoadStatus::Ready =>
                {
                    Some((doc.clone(), changes))
                }
                _ => None,
            };
            (st.revision, base)
        };
        if self.status != LoadStatus::Ready && self.target.is_none() {
            self.target = target;
        }
        self.path = Some(path.clone());
        let profile = self.profile.id;
        let resolver = Resolver::statuses(&path, self.index.as_ref(), profile);
        self.statuses_from = index_key(self.index.as_ref());
        let options = self.build_options();
        self.live.task = Some(cx.spawn(async move |this, cx| {
            let built = cx
                .background_spawn(async move {
                    let (doc, report) = match base {
                        Some((base, changes)) => {
                            let (doc, report) =
                                load::update_tree(&base, tree, text, &changes, &resolver);
                            (doc, Some(report))
                        }
                        None => (
                            load::build_tree(&path, tree, text, &resolver, options, profile),
                            None,
                        ),
                    };
                    Built { doc, report, stamp }
                })
                .await;
            this.update(cx, |reader, cx| reader.finish_follow(revision, built, cx))
                .ok();
        }));
    }

    fn finish_follow(&mut self, revision: u64, built: Built, cx: &mut Context<'_, Self>) {
        self.live.task = None;
        let Built {
            mut doc,
            report,
            stamp,
        } = built;
        #[cfg(test)]
        {
            if report.is_some() {
                self.live.counts.0 += 1;
            } else {
                self.live.counts.1 += 1;
            }
            self.live.last = (doc.timings.model, doc.timings.tree);
        }
        {
            let mut st = self.state.borrow_mut();
            if st.revision != revision {
                // Another document was opened meanwhile.
                return;
            }
            // A refresh of link statuses still running is for the old tree.
            st.revision += 1;
            let first = st.doc.is_none() || self.status != LoadStatus::Ready;
            if first {
                st.install(doc);
                if let Some(target) = self.target.take() {
                    jump(&mut st, &target);
                }
            } else {
                if let Some(current) = &st.doc {
                    // Code blocks whose wrap was toggled meanwhile.
                    for (block, wrap) in current.tree.code_wrap_settings() {
                        Arc::make_mut(&mut doc.tree).set_code_wrap(block, wrap);
                    }
                }
                splice(&mut st, doc, report);
            }
        }
        self.live.shown = Some(stamp);
        self.watch_embeds(cx);
        self.status = LoadStatus::Ready;
        if self.typography_stale() {
            self.rebuild(cx);
        }
        if index_key(self.index.as_ref()) != self.statuses_from {
            self.refresh_statuses(cx);
        }
        self.find_document_changed(cx);
        self.sync_remote(cx);
        cx.notify();
    }

    /// The shown document's render tree and the editor text it was built
    /// from (its line index), when it follows an editor.
    pub(crate) fn live_doc(&self) -> Option<(Arc<RenderTree>, TextSnapshot)> {
        let st = self.state.borrow();
        let doc = st.doc.as_ref()?;
        Some((doc.tree.clone(), doc.lines.clone()?))
    }

    /// Bumped whenever another document or an update of it is shown.
    pub(crate) fn doc_version(&self) -> u64 {
        self.state.borrow().doc_version
    }

    /// Where the top of the view is, as of the last frame: the item there
    /// and the place in its box.
    pub(crate) fn top_spot(&self) -> Option<(usize, Spot)> {
        let st = self.state.borrow();
        let doc = st.doc.as_ref()?;
        let anchor = st.engine.anchor()?;
        let ix = doc.tree.index_of(anchor.id)?;
        let item = &doc.tree.items()[ix];
        if anchor.offset < 0.0 {
            let lead = st.engine.lead().max(1.0);
            return Some((ix, Spot::Lead((anchor.offset / lead).max(-1.0))));
        }
        let height = st.engine.height_of(ix)?;
        let gap = item.gap_before;
        if anchor.offset < gap {
            return Some((ix, Spot::Gap(anchor.offset / gap)));
        }
        let content = (height - gap).max(1.0);
        Some((
            ix,
            Spot::Content(((anchor.offset - gap) / content).clamp(0.0, 1.0)),
        ))
    }

    /// The top of the view as a fractional line of the code block the
    /// document is (the text viewer's: [`crate::file_viewer`]): line `n`
    /// of the code plus the part of it above the top edge, as scroll sync
    /// measures it ([`crate::split::sync`]); -1 to 0 in the top padding.
    /// `None` when no code is at the top. As of the last frame.
    pub(crate) fn top_code_line(&self) -> Option<f64> {
        let (ix, spot) = self.top_spot()?;
        let st = self.state.borrow();
        let item = st.doc.as_ref()?.tree.items().get(ix)?;
        let ItemKind::Code { lines, .. } = &item.kind else {
            return None;
        };
        let (start, end) = (f64::from(lines.start), f64::from(lines.end));
        Some(match spot {
            Spot::Lead(f) => f64::from(f).clamp(-1.0, 0.0),
            Spot::Gap(_) => start,
            Spot::Content(c) => start + f64::from(c.clamp(0.0, 1.0)) * (end - start),
        })
    }

    /// Scrolls so that fractional line `line` of the code block the
    /// document is ([`Self::top_code_line`]) is at the top: in the next
    /// frame, or once the document being loaded is shown.
    pub(crate) fn scroll_to_code_line(&mut self, line: f64, cx: &mut Context<'_, Self>) {
        if self.status != LoadStatus::Ready {
            self.code_line = Some(line);
            return;
        }
        let mut st = self.state.borrow_mut();
        let Some(request) = st
            .doc
            .as_ref()
            .and_then(|d| code_line_request(&d.tree, line))
        else {
            return;
        };
        st.sync = Some(request);
        drop(st);
        cx.notify();
    }

    /// Scrolls, in the next frame, to the place `request` asks for.
    pub(crate) fn sync_to(&mut self, request: SyncRequest, cx: &mut Context<'_, Self>) {
        self.state.borrow_mut().sync = Some(request);
        cx.notify();
    }

    /// Gives the block holding source offset `offset` the caret band.
    pub(crate) fn set_band(&mut self, offset: Option<usize>, cx: &mut Context<'_, Self>) {
        let mut st = self.state.borrow_mut();
        if st.band != offset {
            st.band = offset;
            drop(st);
            cx.notify();
        }
    }

    /// The caret band's source offset.
    #[cfg(test)]
    pub(crate) fn band(&self) -> Option<usize> {
        self.state.borrow().band
    }

    /// Builds installed (incremental, from scratch) and the model and
    /// render tree times of the last one.
    #[cfg(test)]
    pub(crate) fn follow_stats(&self) -> ((u64, u64), (std::time::Duration, std::time::Duration)) {
        (self.live.counts, self.live.last)
    }
}

/// The items of the block that holds source offset `offset` (its end
/// included): one, or the chunks of a code block. `None` between blocks.
pub(super) fn block_items_at(tree: &RenderTree, offset: usize) -> Option<std::ops::Range<usize>> {
    let items = tree.items();
    let k = items.partition_point(|it| it.source.end < offset);
    let item = items.get(k)?;
    (item.source.start <= offset).then(|| tree.items_of(item.id.block))
}

/// Shows `doc`, an update of the shown document: the replaced items'
/// heights are spliced into the height index and their layouts dropped;
/// the view stays where it is. Without a report the whole document is
/// replaced in place.
fn splice(st: &mut State, doc: Loaded, report: Option<UpdateReport>) {
    let old = st.doc.as_ref().map_or(0, |d| d.tree.items().len());
    let report = report.unwrap_or_else(|| UpdateReport {
        removed: 0..old,
        inserted: 0..doc.tree.items().len(),
        changed: doc.tree.items().iter().map(|it| it.id).collect(),
        full: true,
        ..UpdateReport::default()
    });
    if report.full {
        st.cache.clear();
    } else {
        st.cache.forget(report.changed.iter().copied());
    }
    let changed: Vec<usize> = report
        .changed
        .iter()
        .filter_map(|id| doc.tree.index_of(*id))
        .collect();
    st.engine.splice(
        &TreeItems(&doc.tree),
        report.removed.clone(),
        report.inserted.clone(),
        changed,
    );
    st.cache.targets = Some(Arc::clone(&doc.images));
    st.cache.live.retain_tree(&doc.tree);
    st.doc = Some(doc);
    st.selection = None;
    st.hover = Hover::None;
    st.press = Press::None;
    st.geometry.items.clear();
    st.geometry.buttons.clear();
    st.doc_version += 1;
}

/// Where fractional line `line` of the code block in `tree` (its code
/// lines, from 0) is, for scroll sync: the chunk holding it and the part
/// of the chunk above it; the top padding for a negative line, the end
/// past the last. `None` without code.
pub(crate) fn code_line_request(tree: &RenderTree, line: f64) -> Option<SyncRequest> {
    let items = tree.items();
    let chunk = |ix: usize| match &items[ix].kind {
        ItemKind::Code { lines, .. } => Some((f64::from(lines.start), f64::from(lines.end))),
        _ => None,
    };
    let first = (0..items.len()).find(|&ix| chunk(ix).is_some())?;
    if line < 0.0 {
        return Some(SyncRequest {
            ix: first,
            spot: Spot::Lead(line.max(-1.0) as f32),
            reveal: None,
        });
    }
    let mut last = first;
    for ix in first..items.len() {
        let Some((start, end)) = chunk(ix) else {
            break;
        };
        last = ix;
        if line < end {
            let c = (line - start) / (end - start).max(1.0);
            return Some(SyncRequest {
                ix,
                spot: Spot::Content(c.clamp(0.0, 1.0) as f32),
                reveal: None,
            });
        }
    }
    Some(SyncRequest {
        ix: last,
        spot: Spot::Content(1.0),
        reveal: None,
    })
}
