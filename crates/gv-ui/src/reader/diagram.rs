// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Mermaid diagrams in the Reading view.
//!
//! The rule of code highlighting holds: **the UI thread never lays a
//! diagram out.**
//!
//! - While drawing, a diagram item computes its block's key
//!   ([`DiagramKey`]: content hash and trust) and reads the engine's cache;
//!   neither renders. A pending diagram keeps its estimated height, blank.
//! - The view starts a background job per key ([`Diagrams::take_requests`],
//!   [`run`]) on the low-priority pool and keeps its [`Canceller`]: a job is
//!   cancelled when its block shows another source or leaves the viewport
//!   (a block not drawn in a frame), and asked for again when the block is
//!   drawn again. When a job lands, the items drawn meanwhile are laid out
//!   again.
//! - The outcome decides the item: the diagram, centred (wider ones scroll
//!   sideways); a fallback card above the source (ADR-017: *Diagram
//!   error*, *Diagram too large to render* with *Render anyway*, *Renderer
//!   error*; *Copy source* on each); or the source under a "not
//!   recognised" note.
//!
//! The engine is made once per process ([`diagram_engine`]) on a
//! background thread: its labels are measured and drawn with the system's
//! sans-serif face ([`diagram_fonts`]), KaTeX's sans-serif without one.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use gv_diagram::{
    Canceller, DiagramEngine, DiagramFonts, DiagramKey, FallbackCard, Limits, Outcome, Trust,
};
use gv_render_tree::{CodeBlock, ItemId};
use gv_syntax::BlockId;

/// Families tried, in order, for diagram labels: common UI sans-serif
/// faces with separate regular, bold and italic files.
const FAMILIES: &[&str] = &[
    "Inter",
    "Noto Sans",
    "Open Sans",
    "DejaVu Sans",
    "Liberation Sans",
    "Cantarell",
    "Segoe UI",
    "Arial",
    "Helvetica Neue",
    "Helvetica",
];

/// The shared font database of the engines (SVG text, diagram labels):
/// the system's fonts, scanned once on first use (never on the UI
/// thread). Tests use KaTeX's sans-serif faces only.
pub(crate) fn font_db() -> Arc<fontdb::Database> {
    static DB: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        if cfg!(test) {
            for face in katex_faces() {
                db.load_font_data(face);
            }
        } else {
            db.load_system_fonts();
        }
        Arc::new(db)
    })
    .clone()
}

/// KaTeX's sans-serif faces (regular, bold, italic): bundled, OFL.
fn katex_faces() -> Vec<Vec<u8>> {
    [
        "KaTeX_SansSerif-Regular.ttf",
        "KaTeX_SansSerif-Bold.ttf",
        "KaTeX_SansSerif-Italic.ttf",
    ]
    .iter()
    .filter_map(|name| ratex_katex_fonts::ttf_bytes(name).map(std::borrow::Cow::into_owned))
    .collect()
}

/// The bytes of the face of `family` with `weight` and `style`, when it is
/// a single-face file.
fn face_bytes(
    db: &fontdb::Database,
    family: &str,
    weight: fontdb::Weight,
    style: fontdb::Style,
) -> Option<Vec<u8>> {
    let id = db.query(&fontdb::Query {
        families: &[fontdb::Family::Name(family)],
        weight,
        style,
        ..fontdb::Query::default()
    })?;
    let face = db.face(id)?;
    if face.families.first().is_none_or(|(name, _)| name != family) {
        return None;
    }
    db.with_face_data(id, |data, index| (index == 0).then(|| data.to_vec()))
        .flatten()
}

/// The faces diagram labels are measured and drawn with: the first of
/// [`FAMILIES`] with a regular face, else KaTeX's sans-serif.
pub(crate) fn diagram_fonts(db: &fontdb::Database) -> DiagramFonts {
    for family in FAMILIES {
        let Some(regular) = face_bytes(db, family, fontdb::Weight::NORMAL, fontdb::Style::Normal)
        else {
            continue;
        };
        let mut faces = vec![regular];
        for (w, s) in [
            (fontdb::Weight::BOLD, fontdb::Style::Normal),
            (fontdb::Weight::NORMAL, fontdb::Style::Italic),
        ] {
            if let Some(f) = face_bytes(db, family, w, s).filter(|f| f != &faces[0]) {
                faces.push(f);
            }
        }
        return DiagramFonts {
            family: (*family).to_owned(),
            faces,
        };
    }
    DiagramFonts {
        family: "KaTeX_SansSerif".to_owned(),
        faces: katex_faces(),
    }
}

/// The process-wide diagram engine, made on first use: blocking (it may
/// scan the system's fonts), so only on a background thread. `None` when
/// it cannot start (logged once): diagrams then show a renderer error.
pub(crate) fn diagram_engine() -> Option<Arc<DiagramEngine>> {
    static ENGINE: OnceLock<Option<Arc<DiagramEngine>>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let fonts = if cfg!(test) {
                DiagramFonts {
                    family: "KaTeX_SansSerif".to_owned(),
                    faces: katex_faces(),
                }
            } else {
                diagram_fonts(&font_db())
            };
            match DiagramEngine::new(fonts, Limits::default()) {
                Ok(e) => Some(Arc::new(e)),
                Err(e) => {
                    tracing::error!("diagrams are not drawn: {e}");
                    None
                }
            }
        })
        .clone()
}

/// The engine if a job made it already (for the UI thread: never makes
/// it).
fn engine_if_ready() -> Option<Arc<DiagramEngine>> {
    ENGINE_READY.get().cloned().flatten()
}

/// Set once the engine exists, so the UI thread can read its cache.
static ENGINE_READY: OnceLock<Option<Arc<DiagramEngine>>> = OnceLock::new();

/// A job's cancellation, shared with the background task: the canceller
/// arrives when the task has made its job.
#[derive(Debug, Default)]
pub(crate) struct CancelSlot {
    cancelled: AtomicBool,
    canceller: Mutex<Option<Canceller>>,
}

impl CancelSlot {
    /// Cancels the job, now or as soon as it starts.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(c) = self
            .canceller
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            c.cancel();
        }
    }

    fn set(&self, c: Canceller) {
        *self
            .canceller
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(c.clone());
        if self.cancelled.load(Ordering::Acquire) {
            c.cancel();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// A job to start: the source, at a trust level, with its cancellation.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub key: DiagramKey,
    pub source: Arc<str>,
    pub trust: Trust,
    pub slot: Arc<CancelSlot>,
}

/// Renders a request (blocking: the background executor only). `None`
/// when the engine cannot start.
pub(crate) fn run(request: &Request) -> Option<Outcome> {
    let engine = diagram_engine()?;
    let _ = ENGINE_READY.set(Some(Arc::clone(&engine)));
    if request.slot.is_cancelled() {
        return Some(Outcome::Cancelled);
    }
    #[cfg(test)]
    let started = std::time::Instant::now();
    let job = engine.job(Arc::clone(&request.source), request.trust);
    request.slot.set(job.canceller());
    let outcome = job.run(&engine);
    #[cfg(test)]
    tests::count_job(started.elapsed());
    Some(outcome)
}

/// What a diagram item shows now.
#[derive(Debug, Clone)]
pub(crate) enum Shown {
    /// Waiting for its job.
    Pending,
    Rendered(Arc<gv_diagram::Diagram>),
    Card(Arc<FallbackCard>),
    NotRecognised,
    /// The engine could not start.
    Unavailable,
}

/// A diagram block as last drawn.
#[derive(Debug)]
struct Entry {
    /// The node it was computed for.
    node: Arc<CodeBlock>,
    key: DiagramKey,
    /// The user chose *Render anyway* for this source.
    trusted: bool,
    /// Its running job.
    job: Option<Arc<CancelSlot>>,
    /// Drawn in the current frame.
    seen: bool,
}

/// Diagrams in one Reading view: the blocks drawn, their jobs, and the
/// items to lay out again when a job lands.
#[derive(Debug, Default)]
pub(crate) struct Diagrams {
    blocks: HashMap<BlockId, Entry>,
    /// Items laid out while their key was pending.
    waiting: HashMap<DiagramKey, Vec<ItemId>>,
    requests: Vec<Request>,
    /// Outcomes that landed while the engine's cache could not hold them
    /// (the engine was not ready on the UI thread yet), and the cards of
    /// an engine that cannot start.
    landed: HashMap<DiagramKey, Outcome>,
    /// The engine failed to start.
    unavailable: bool,
}

/// Blocks remembered at most (all are forgotten beyond).
const BLOCKS_CAP: usize = 4_096;

impl Diagrams {
    /// What item `item` of diagram block `block` (`code`) shows, asking for
    /// a job when it is pending and none runs.
    pub fn shown(&mut self, block: BlockId, code: &Arc<CodeBlock>, item: ItemId) -> Shown {
        if self.blocks.len() > BLOCKS_CAP {
            self.forget_blocks();
        }
        let entry = self.blocks.get(&block);
        let same = entry.is_some_and(|e| Arc::ptr_eq(&e.node, code) || e.node.text == code.text);
        if !same {
            // Another source: its job (if any) is of no use now.
            if let Some(job) = entry.and_then(|e| e.job.as_ref()) {
                job.cancel();
            }
            self.blocks.insert(
                block,
                Entry {
                    node: Arc::clone(code),
                    key: DiagramKey::new(&code.text, Trust::Interactive),
                    trusted: false,
                    job: None,
                    seen: true,
                },
            );
        }
        let Some(entry) = self.blocks.get_mut(&block) else {
            return Shown::Pending;
        };
        entry.seen = true;
        entry.node = Arc::clone(code);
        let key = entry.key;
        if self.unavailable {
            return Shown::Unavailable;
        }
        let cached = engine_if_ready()
            .and_then(|e| e.cached(&key))
            .or_else(|| self.landed.get(&key).cloned());
        match cached {
            Some(Outcome::Rendered(d)) => return Shown::Rendered(d),
            Some(Outcome::Card(c)) => return Shown::Card(c),
            Some(Outcome::NotRecognised) => return Shown::NotRecognised,
            Some(Outcome::Cancelled) | None => {}
        }
        let waiting = self.waiting.entry(key).or_default();
        if !waiting.contains(&item) {
            waiting.push(item);
        }
        if entry.job.is_none() {
            let slot = Arc::new(CancelSlot::default());
            entry.job = Some(Arc::clone(&slot));
            let trust = if entry.trusted {
                Trust::Trusted
            } else {
                Trust::Interactive
            };
            self.requests.push(Request {
                key,
                source: Arc::from(code.text.as_str()),
                trust,
                slot,
            });
        }
        #[cfg(test)]
        tests::count_pending_draw();
        Shown::Pending
    }

    /// *Render anyway*: block `block` renders with the trusted caps from
    /// now on (until its source changes). Returns whether it was a known
    /// block.
    pub fn trust(&mut self, block: BlockId) -> bool {
        let Some(e) = self.blocks.get_mut(&block) else {
            return false;
        };
        if let Some(job) = e.job.take() {
            job.cancel();
        }
        e.trusted = true;
        e.key = DiagramKey::new(&e.node.text, Trust::Trusted);
        true
    }

    /// The jobs to start.
    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    /// A job ended with `outcome` (`None`: the engine cannot start): the
    /// items to lay out again.
    pub fn landed(&mut self, request: &Request, outcome: Option<Outcome>) -> Vec<ItemId> {
        for e in self.blocks.values_mut() {
            if e.job
                .as_ref()
                .is_some_and(|j| Arc::ptr_eq(j, &request.slot))
            {
                e.job = None;
            }
        }
        match outcome {
            None => self.unavailable = true,
            Some(Outcome::Cancelled) => {
                // Asked for again when its block is drawn again.
                return Vec::new();
            }
            Some(o) => {
                if engine_if_ready().is_none() {
                    self.landed.insert(request.key, o);
                }
            }
        }
        self.waiting.remove(&request.key).unwrap_or_default()
    }

    /// Starts a frame: no block is seen yet.
    pub fn begin_frame(&mut self) {
        for e in self.blocks.values_mut() {
            e.seen = false;
        }
    }

    /// Ends a frame: the jobs of blocks not drawn in it (they left the
    /// viewport) are cancelled. Returns how many.
    pub fn end_frame(&mut self) -> usize {
        let mut n = 0;
        for e in self.blocks.values_mut() {
            if !e.seen
                && let Some(job) = e.job.take()
            {
                job.cancel();
                n += 1;
            }
        }
        n
    }

    /// Forgets the blocks (another document is shown), cancelling their
    /// jobs.
    pub fn forget_blocks(&mut self) {
        for e in self.blocks.values() {
            if let Some(job) = &e.job {
                job.cancel();
            }
        }
        self.blocks.clear();
        self.waiting.clear();
        self.landed.clear();
    }

    /// Whether block `block` has a job running.
    #[cfg(test)]
    pub fn running(&self, block: BlockId) -> bool {
        self.blocks.get(&block).is_some_and(|e| e.job.is_some())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;

    thread_local! {
        static JOBS: Cell<usize> = const { Cell::new(0) };
        static PENDING: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn count_job(_took: std::time::Duration) {
        assert_eq!(
            crate::reader::code_highlight::tests::in_frame(),
            0,
            "a diagram job ran while a frame was drawn"
        );
        JOBS.with(|j| j.set(j.get() + 1));
    }

    pub(super) fn count_pending_draw() {
        PENDING.with(|p| p.set(p.get() + 1));
    }

    /// Diagram jobs run on this thread so far.
    pub(crate) fn jobs_run() -> usize {
        JOBS.with(Cell::get)
    }

    /// Diagram items drawn while pending on this thread so far.
    pub(crate) fn pending_draws() -> usize {
        PENDING.with(Cell::get)
    }

    use std::sync::Arc;

    use gv_render_tree::{CodeBlock, ItemId, NodeKind, RenderTree};

    use super::{Diagrams, Shown};

    /// The diagram block of a one-fence document in the GFM profile.
    fn fence(source: &str) -> (gv_syntax::BlockId, Arc<CodeBlock>) {
        let md = format!("```mermaid\n{source}\n```\n");
        let profile = gv_profile::ProfileId::GfmFile;
        let syntax = gv_syntax::parse_with(&md, gv_syntax::ParseOptions::builtin(profile));
        let model = gv_model::DocModel::build(&syntax, &*md);
        let t = RenderTree::build(
            &syntax,
            &model,
            &*md,
            gv_render_tree::RenderOptions::builtin(profile),
            gv_render_tree::Sources::default(),
        );
        let node = &t.nodes()[0];
        let NodeKind::Diagram(code) = &node.kind else {
            panic!("a diagram: {:?}", node.kind);
        };
        (node.id, Arc::clone(code))
    }

    #[test]
    fn a_changed_block_cancels_its_job_and_asks_again() {
        let mut d = Diagrams::default();
        let (block, a) = fence("graph TD\n  cancel_on_change_a --> B");
        let (_, b) = fence("graph TD\n  cancel_on_change_b --> B");
        let item = ItemId { block, chunk: 0 };
        assert!(matches!(d.shown(block, &a, item), Shown::Pending));
        let first = d.take_requests();
        assert_eq!(first.len(), 1);
        // Drawn again while it runs: no second job.
        assert!(matches!(d.shown(block, &a, item), Shown::Pending));
        assert!(d.take_requests().is_empty());
        // The block shows another source: the job stops, a new one starts.
        assert!(matches!(d.shown(block, &b, item), Shown::Pending));
        assert!(first[0].slot.is_cancelled());
        let second = d.take_requests();
        assert_eq!(second.len(), 1);
        assert!(!second[0].slot.is_cancelled());
        assert_ne!(first[0].key, second[0].key);
        // A cancelled job that lands lays nothing out again.
        let cancelled = Some(gv_diagram::Outcome::Cancelled);
        assert!(d.landed(&first[0], cancelled).is_empty());
        assert!(d.running(block));
    }

    #[test]
    fn a_block_that_leaves_the_viewport_cancels_its_job() {
        let mut d = Diagrams::default();
        let (block, a) = fence("graph TD\n  leaves_viewport --> B");
        let item = ItemId { block, chunk: 0 };
        d.begin_frame();
        let _ = d.shown(block, &a, item);
        assert_eq!(d.end_frame(), 0, "drawn this frame: kept");
        let job = d.take_requests().remove(0);
        // A frame that does not draw it.
        d.begin_frame();
        assert_eq!(d.end_frame(), 1);
        assert!(job.slot.is_cancelled());
        assert!(!d.running(block));
        // Back on screen: asked for again.
        d.begin_frame();
        let _ = d.shown(block, &a, item);
        assert_eq!(d.take_requests().len(), 1);
    }

    #[test]
    fn render_anyway_asks_with_the_trusted_caps() {
        let mut d = Diagrams::default();
        let (block, a) = fence("graph TD\n  render_anyway_unit --> B");
        let item = ItemId { block, chunk: 0 };
        let _ = d.shown(block, &a, item);
        let first = d.take_requests().remove(0);
        assert_eq!(first.trust, gv_diagram::Trust::Interactive);
        assert!(d.trust(block));
        assert!(first.slot.is_cancelled());
        let _ = d.shown(block, &a, item);
        let again = d.take_requests().remove(0);
        assert_eq!(again.trust, gv_diagram::Trust::Trusted);
    }
}
