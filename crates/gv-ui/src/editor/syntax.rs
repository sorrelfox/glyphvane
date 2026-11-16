// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The editor's syntax tree, kept up to date with its text.
//!
//! Every transaction's [`EditSummary`] is queued by an editor observer
//! ([`EditQueue`]). [`Syntax::advance`] reparses the queue in order on the
//! UI thread until a deadline (about 1 ms); a reparse that runs out of time
//! leaves an interim tree for display and is finished on the background
//! executor by the view, after which the rest of the queue follows.
//!
//! The tree shown is always paired with the text it was parsed from
//! ([`Syntax::shown`]), so highlighting never reads offsets of one text in
//! another. While a background reparse runs, the shown pair can be a few
//! edits behind the buffer; highlighting then applies to the current lines
//! by line number and is clamped to them.
//!
//! The complete tree is also handed to a Reading view that follows the
//! editor (the Split view's preview, [`Syntax::take_handoff`]) with the
//! reparses' [`Changes`] since the last hand-off, composed, so that the
//! Reading view's model and render tree update incrementally.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gv_syntax::{Budget, Changes, Reparse, ReparseTask, SyntaxTree};
use gv_text::{EditSummary, Revision, TextSnapshot};

/// How long a reparse may run on the UI thread per batch of edits
/// (NFR-PERF-06 asks for 1 ms at p95; the measured typing reparse is a few
/// µs).
pub(crate) const UI_BUDGET: Duration = Duration::from_millis(1);

/// Edits waiting for the parser: the text after each transaction and its
/// summary, oldest first.
pub(crate) type EditQueue = Rc<RefCell<VecDeque<(TextSnapshot, EditSummary)>>>;

/// A reparse to finish off the UI thread: the paused task and the text it
/// parses.
pub(crate) struct Background {
    pub task: Box<ReparseTask>,
    pub text: TextSnapshot,
}

impl std::fmt::Debug for Background {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Background")
            .field("revision", &self.text.revision())
            .finish_non_exhaustive()
    }
}

impl Background {
    /// Runs the reparse to the end.
    pub fn finish(mut self) -> (SyntaxTree, TextSnapshot, Changes) {
        self.task.run(&self.text, Budget::Unlimited);
        let (tree, changes) = self.task.finish();
        (tree, self.text, changes)
    }
}

/// Names one text of one document: which document (each document loaded
/// into a view gets its own number) and the text's revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TextStamp {
    pub doc: u64,
    pub revision: Revision,
}

/// The syntax tree of the editor's text, handed to a Reading view that
/// follows the editor.
#[derive(Debug, Clone)]
pub(crate) struct SyntaxHandoff {
    pub tree: SyntaxTree,
    /// The text `tree` is the tree of.
    pub text: TextSnapshot,
    pub stamp: TextStamp,
    /// The text of the previous hand-off and the reparses since, composed;
    /// `None` at the first hand-off of a document.
    pub since: Option<(TextStamp, Changes)>,
}

/// The number of the next document.
static NEXT_DOC: AtomicU64 = AtomicU64::new(1);

/// The syntax tree of the editor's text.
#[derive(Debug)]
pub(crate) struct Syntax {
    /// The last complete tree, and the text it is the tree of.
    tree: SyntaxTree,
    text: TextSnapshot,
    /// What highlighting reads: `tree`, or an interim tree while a reparse
    /// finishes in the background.
    shown: (SyntaxTree, TextSnapshot),
    /// Bumped whenever `shown` changes: highlight caches key on it.
    generation: u64,
    /// A reparse is running in the background.
    running: bool,
    /// The document's number, for hand-offs.
    doc: u64,
    /// The revision last handed off and the reparses since, composed.
    handed: Option<(Revision, Changes)>,
}

impl Syntax {
    /// The syntax of `text`, parsed as `tree`.
    pub fn new(tree: SyntaxTree, text: TextSnapshot) -> Self {
        Self {
            shown: (tree.clone(), text.clone()),
            tree,
            text,
            generation: 0,
            running: false,
            doc: NEXT_DOC.fetch_add(1, Ordering::Relaxed),
            handed: None,
        }
    }

    /// The complete tree and its text, if they changed since the last
    /// hand-off, with the reparses since then.
    pub fn take_handoff(&mut self) -> Option<SyntaxHandoff> {
        let revision = self.text.revision();
        let stamp = |revision| TextStamp {
            doc: self.doc,
            revision,
        };
        let since = match self.handed.take() {
            Some((at, changes)) if at == revision && changes.is_empty() => {
                self.handed = Some((at, changes));
                return None;
            }
            Some((at, changes)) => Some((stamp(at), changes)),
            None => None,
        };
        self.handed = Some((revision, Changes::default()));
        Some(SyntaxHandoff {
            tree: self.tree.clone(),
            text: self.text.clone(),
            stamp: stamp(revision),
            since,
        })
    }

    /// A reparse to `self.tree` reported `changes`.
    fn note(&mut self, changes: &Changes) {
        if let Some((_, since)) = &mut self.handed {
            *since = since.compose(changes);
        }
    }

    /// A full parse of `text`.
    #[cfg(test)]
    pub fn parse(text: TextSnapshot, options: gv_syntax::ParseOptions) -> Self {
        let tree = SyntaxTree::parse_with(&text, options);
        Self::new(tree, text)
    }

    /// The tree to highlight with and the text it was parsed from.
    pub fn shown(&self) -> (&SyntaxTree, &TextSnapshot) {
        (&self.shown.0, &self.shown.1)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether a reparse is running in the background.
    #[cfg(test)]
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Whether the shown tree is the complete tree of `text`.
    pub fn is_current(&self, text: &TextSnapshot) -> bool {
        !self.running && self.text.revision() == text.revision() && !self.shown.0.is_interim()
    }

    /// Reparses the queued edits in order until `deadline`. Returns the
    /// reparse to finish in the background when time ran out; the rest of
    /// the queue waits for it ([`Self::finished`]).
    pub fn advance(
        &mut self,
        queue: &mut VecDeque<(TextSnapshot, EditSummary)>,
        deadline: Instant,
    ) -> Option<Background> {
        if self.running {
            return None;
        }
        while let Some((text, summary)) = queue.pop_front() {
            match self.tree.reparse(&text, &summary, Budget::Until(deadline)) {
                Reparse::Done { tree, changes } => {
                    self.note(&changes);
                    self.tree = tree;
                    self.text = text;
                    self.shown = (self.tree.clone(), self.text.clone());
                    self.generation += 1;
                }
                Reparse::Partial { interim, task } => {
                    self.shown = (interim, text.clone());
                    self.generation += 1;
                    self.running = true;
                    return Some(Background { task, text });
                }
            }
        }
        None
    }

    /// Replaces the tree with a full parse of the current text (another
    /// profile's options). Returns `false`, changing nothing, while a
    /// reparse is running.
    pub fn reparse_all(&mut self, options: gv_syntax::ParseOptions) -> bool {
        if self.running {
            return false;
        }
        self.tree = SyntaxTree::parse_with(&self.text, options);
        self.shown = (self.tree.clone(), self.text.clone());
        self.generation += 1;
        // The next hand-off starts over, as for a new document.
        self.handed = None;
        true
    }

    /// Hands the whole tree off next time, as for a new document (the
    /// profile changed: the follower builds everything again).
    pub fn restart_handoff(&mut self) {
        self.handed = None;
    }

    /// A background reparse finished with the tree of `text`.
    pub fn finished(&mut self, tree: SyntaxTree, text: TextSnapshot, changes: &Changes) {
        self.note(changes);
        self.running = false;
        self.tree = tree;
        self.text = text;
        self.shown = (self.tree.clone(), self.text.clone());
        self.generation += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gv_text::Buffer;

    #[test]
    fn queued_edits_reparse_in_order() {
        let mut buffer = Buffer::new("# Title\n\ntext\n");
        let mut syntax = Syntax::parse(buffer.snapshot(), Default::default());
        let mut queue = VecDeque::new();
        let a = buffer.edit([(9..9, "more ")]).unwrap();
        queue.push_back((buffer.snapshot(), a));
        let b = buffer.edit([(0..0, "> ")]).unwrap();
        queue.push_back((buffer.snapshot(), b));
        let far = Instant::now() + Duration::from_secs(5);
        assert!(syntax.advance(&mut queue, far).is_none());
        assert!(queue.is_empty());
        assert!(syntax.is_current(buffer.text()));
        let full = SyntaxTree::parse(buffer.text());
        let (tree, text) = syntax.shown();
        assert_eq!(text.revision(), buffer.revision());
        tree.same_structure(&full).unwrap();
    }

    #[test]
    fn an_expired_budget_continues_in_the_background() {
        let mut text = String::new();
        for i in 0..2000 {
            text.push_str(&format!("line {i} with *some* text\n\n"));
        }
        let mut buffer = Buffer::new(&text);
        let mut syntax = Syntax::parse(buffer.snapshot(), Default::default());
        // Opening a fence at the top changes every line below.
        let summary = buffer.edit([(0..0, "```\n")]).unwrap();
        let mut queue = VecDeque::from([(buffer.snapshot(), summary)]);
        let past = Instant::now();
        let background = syntax.advance(&mut queue, past).expect("runs out of time");
        assert!(syntax.is_running());
        assert!(syntax.shown().0.is_interim());
        let (tree, text, changes) = background.finish();
        syntax.finished(tree, text, &changes);
        assert!(syntax.is_current(buffer.text()));
        let full = SyntaxTree::parse(buffer.text());
        syntax.shown().0.same_structure(&full).unwrap();
    }
}
