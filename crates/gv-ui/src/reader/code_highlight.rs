// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Fenced code highlighting for the Reading view and the Source view.
//!
//! Both views follow the same rule: **the UI thread never highlights.**
//!
//! - While drawing, a view computes a block's cache key ([`key_for`]:
//!   none without a language or over the size cap) and reads the shared
//!   cache ([`cached`], [`CodeHighlighter::get`]); neither highlights.
//! - A pending block is drawn plain, or with the block's previous result
//!   mapped line by line ([`LineSpans`]), and the view starts a background
//!   job ([`highlight`]) once per key ([`Jobs`]). When the job lands, the
//!   view drops what it drew from the old state and draws again.
//!
//! Results are token classes ([`TokenClass`]); [`token_of`] gives the
//! design token that colours each, so a theme change needs no new
//! highlighting. Only blocks the views draw are highlighted, so large-file
//! mode's "fenced-code highlighting only for visible blocks" holds in both.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use gv_highlight::{CodeHighlighter, Highlight, HighlightKey, Limits, Outcome, TokenClass};

use crate::inline::Token;

/// The design token that colours `class` ([`TokenClass::token`]).
pub(crate) fn token_of(class: TokenClass) -> Token {
    static TABLE: OnceLock<Vec<Token>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        TokenClass::ALL
            .iter()
            .map(|c| {
                Token::ALL
                    .into_iter()
                    .find(|t| t.name() == c.token())
                    .unwrap_or(Token::TextPrimary)
            })
            .collect()
    });
    table
        .get(class as usize)
        .copied()
        .unwrap_or(Token::TextPrimary)
}

/// The cache key of `code` (a block's lines joined with `\n`) highlighted
/// as the fence info string `info`, or `None` when it stays plain (no
/// info string, no bundled grammar, over the size cap). One pass over the
/// text: cheap enough for the UI thread.
pub(crate) fn key_for(code: &str, info: &str) -> Option<HighlightKey> {
    #[cfg(test)]
    if tests::DISABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    if info.trim().is_empty() || code.is_empty() || code.len() > Limits::default().max_bytes {
        return None;
    }
    let key = HighlightKey::new(code, info);
    key.language().is_some().then_some(key)
}

/// The cached result for `key` (a cache lookup, for the UI thread).
pub(crate) fn cached(key: &HighlightKey) -> Option<Arc<Highlight>> {
    CodeHighlighter::shared().get(key)
}

/// Highlights `code` under `key` into the shared cache. Blocking: only on
/// the background executor.
pub(crate) fn highlight(key: HighlightKey, code: &str) -> Arc<Highlight> {
    #[cfg(test)]
    let started = std::time::Instant::now();
    let result = CodeHighlighter::shared().highlight_keyed(key, code);
    #[cfg(test)]
    tests::count_job(code, started.elapsed());
    result
}

/// The keys with a job running, so that one key starts one job.
#[derive(Debug, Default)]
pub(crate) struct Jobs {
    running: HashSet<HighlightKey>,
}

impl Jobs {
    /// Whether a job for `key` should start now (none is running).
    pub fn start(&mut self, key: HighlightKey) -> bool {
        self.running.insert(key)
    }

    /// The job for `key` ended.
    pub fn finish(&mut self, key: &HighlightKey) {
        self.running.remove(key);
    }
}

/// A result with the line ranges of the text it was made from, so that it
/// can colour a newer version of the block line by line while that one is
/// highlighted (the previous structure shifted into place).
#[derive(Debug, Clone)]
pub(crate) struct LineSpans {
    highlight: Arc<Highlight>,
    lines: Arc<[Range<usize>]>,
}

impl LineSpans {
    /// `highlight` of a text whose lines are `lines`.
    pub fn new(highlight: Arc<Highlight>, lines: Arc<[Range<usize>]>) -> Self {
        Self { highlight, lines }
    }

    /// Whether there is anything to colour.
    pub fn is_empty(&self) -> bool {
        self.highlight.outcome != Outcome::Highlighted || self.highlight.spans.is_empty()
    }

    /// The classified spans of line `line`, line-relative and clipped to
    /// `len` bytes (the length of the line as it is now).
    pub fn line(
        &self,
        line: usize,
        len: usize,
    ) -> impl Iterator<Item = (Range<usize>, TokenClass)> {
        let range = self.lines.get(line).cloned().unwrap_or(0..0);
        let spans = if range.is_empty() {
            &[][..]
        } else {
            self.highlight.spans_in(range.clone())
        };
        spans.iter().filter_map(move |s| {
            let start = s.start.max(range.start) - range.start;
            let end = (s.end.min(range.end) - range.start).min(len);
            (start < end).then_some((start..end, s.class))
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;
    use std::sync::atomic::AtomicBool;

    use super::*;

    /// Turns highlighting off (for the before/after timing runs only:
    /// it applies to the whole process).
    pub(crate) static DISABLED: AtomicBool = AtomicBool::new(false);

    thread_local! {
        static JOBS: Cell<usize> = const { Cell::new(0) };
        static TEXTS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
        static IN_FRAME: Cell<u32> = const { Cell::new(0) };
        static PENDING: Cell<usize> = const { Cell::new(0) };
        static TIME: Cell<std::time::Duration> = const { Cell::new(std::time::Duration::ZERO) };
    }

    pub(super) fn count_job(code: &str, took: std::time::Duration) {
        TIME.with(|t| t.set(t.get() + took));
        assert_eq!(
            IN_FRAME.with(Cell::get),
            0,
            "a highlighting job ran while a frame was drawn"
        );
        JOBS.with(|j| j.set(j.get() + 1));
        TEXTS.with(|t| t.borrow_mut().push(code.to_owned()));
    }

    /// Frames being drawn on this thread now (engine jobs must see 0).
    pub(crate) fn in_frame() -> u32 {
        IN_FRAME.with(Cell::get)
    }

    /// Highlighting jobs run on this thread so far. GPUI's test executor
    /// runs background tasks on the test's thread, so a test sees its own.
    pub(crate) fn jobs_run() -> usize {
        JOBS.with(Cell::get)
    }

    /// The time spent in highlighting jobs on this thread so far.
    pub(crate) fn job_time() -> std::time::Duration {
        TIME.with(Cell::get)
    }

    /// The texts highlighted by the jobs run on this thread, from the
    /// `from`th on.
    pub(crate) fn job_texts(from: usize) -> Vec<String> {
        TEXTS.with(|t| {
            t.borrow()
                .get(from..)
                .map(<[_]>::to_vec)
                .unwrap_or_default()
        })
    }

    /// Blocks drawn while their highlighting was pending, without a
    /// previous result, on this thread so far.
    pub(crate) fn pending_draws() -> usize {
        PENDING.with(Cell::get)
    }

    pub(crate) fn count_pending_draw() {
        PENDING.with(|p| p.set(p.get() + 1));
    }

    /// Marks a frame being drawn on this thread until dropped: a
    /// highlighting job that runs meanwhile fails the test.
    pub(crate) struct FrameGuard;

    impl FrameGuard {
        pub(crate) fn new() -> Self {
            IN_FRAME.with(|f| f.set(f.get() + 1));
            Self
        }
    }

    impl Drop for FrameGuard {
        fn drop(&mut self) {
            IN_FRAME.with(|f| f.set(f.get() - 1));
        }
    }

    #[test]
    fn every_class_has_its_design_token() {
        for class in TokenClass::ALL {
            assert_eq!(token_of(class).name(), class.token(), "{class:?}");
        }
    }

    #[test]
    fn lookups_never_highlight() {
        let code = "fn lookups_never_highlight() -> u32 { 1 }";
        let before = jobs_run();
        let key = key_for(code, "rust").expect("rust is bundled");
        assert!(cached(&key).is_none());
        assert_eq!(jobs_run(), before);
        assert!(key_for(code, "no-such-language").is_none());
        assert!(key_for(code, "").is_none());
        let h = highlight(key, code);
        assert_eq!(jobs_run(), before + 1);
        assert!(!h.spans.is_empty());
        assert!(cached(&key).is_some());
    }

    #[test]
    fn line_spans_clip_to_the_line() {
        let code = "let line_spans_clip = 1;\nlet b = \"x\";";
        let key = key_for(code, "js").expect("js is bundled");
        let lines: Arc<[Range<usize>]> = vec![0..24, 25..code.len()].into();
        let spans = LineSpans::new(highlight(key, code), lines);
        assert!(!spans.is_empty());
        let first: Vec<_> = spans.line(0, 24).collect();
        assert!(
            first
                .iter()
                .any(|(r, c)| *r == (0..3) && *c == TokenClass::Keyword)
        );
        let second: Vec<_> = spans.line(1, 100).collect();
        assert!(
            second
                .iter()
                .any(|(r, c)| *r == (8..11) && *c == TokenClass::String)
        );
        // A shorter line now: clipped.
        assert!(spans.line(1, 9).all(|(r, _)| r.end <= 9));
        assert_eq!(spans.line(5, 10).count(), 0);
    }
}
