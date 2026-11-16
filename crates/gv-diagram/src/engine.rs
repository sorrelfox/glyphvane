// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The cached, capped Mermaid engine.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use merman::svg::export::{NativeFontContext, NativeFontLimits};
use merman::svg::{RenderResourcePolicy, SvgPipeline};
use merman::{
    MermaidEngine, OperationControl, ParseOptions, RenderError, RenderOutput, RenderRequest,
    Renderer, SourcePresentationConfig, SvgEnvironment, SvgRequest,
};

use crate::card::{CardKind, FallbackCard};
use crate::convert::{self, ConvertError, ListCaps};
use crate::display::Diagram;
use crate::palette;

/// How much a render may cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Trust {
    /// Diagrams in documents: the interactive caps (plan 12).
    Interactive,
    /// After the user chose *Render anyway* on a "too large" card: looser
    /// caps and a longer deadline, still off the UI thread.
    Trusted,
}

/// Caps per trust level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustLimits {
    /// Largest source, in bytes.
    pub max_source_bytes: usize,
    /// Mermaid's `maxEdges`.
    pub max_edges: usize,
    /// Wall-clock deadline for parse, layout and SVG.
    pub deadline: Duration,
    /// Largest display list, in drawing items.
    pub max_items: usize,
    /// Largest display list, in path elements.
    pub max_elements: usize,
}

/// The engine's caps and cache budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub interactive: TrustLimits,
    pub trusted: TrustLimits,
    /// Approximate bytes of cached diagrams kept.
    pub cache_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            interactive: TrustLimits {
                max_source_bytes: 100 << 10,
                max_edges: 500,
                deadline: Duration::from_secs(2),
                max_items: 50_000,
                max_elements: 2_000_000,
            },
            trusted: TrustLimits {
                max_source_bytes: 1 << 20,
                max_edges: 5_000,
                deadline: Duration::from_secs(30),
                max_items: 400_000,
                max_elements: 16_000_000,
            },
            cache_bytes: 64 << 20,
        }
    }
}

impl Limits {
    /// The caps for `trust`.
    pub fn for_trust(&self, trust: Trust) -> &TrustLimits {
        match trust {
            Trust::Interactive => &self.interactive,
            Trust::Trusted => &self.trusted,
        }
    }
}

/// The font labels are measured and drawn with: one family, given as
/// OpenType/TrueType bytes (regular first, then bold and italic faces).
#[derive(Debug, Clone)]
pub struct DiagramFonts {
    /// The family name inside the font files.
    pub family: String,
    pub faces: Vec<Vec<u8>>,
}

/// The engine could not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupError(pub String);

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "diagram engine setup: {}", self.0)
    }
}

impl std::error::Error for SetupError {}

/// The note shown above the highlighted source when a ` ```mermaid ` block
/// holds no diagram type merman knows (no card, ADR-017).
pub const NOT_RECOGNISED_NOTE: &str = "Mermaid: diagram type not recognised";

/// How a render ended.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// The diagram, ready to paint.
    Rendered(Arc<Diagram>),
    /// A fallback card to show instead (the source stays available).
    Card(Arc<FallbackCard>),
    /// Not a diagram merman knows: show the highlighted source with
    /// [`NOT_RECOGNISED_NOTE`], no card.
    NotRecognised,
    /// Cancelled through a [`Canceller`] (the block changed or left the
    /// viewport). Never cached.
    Cancelled,
}

impl Outcome {
    fn cost(&self) -> usize {
        match self {
            Outcome::Rendered(d) => d.approx_bytes(),
            Outcome::Card(c) => std::mem::size_of::<FallbackCard>() + c.message.len(),
            Outcome::NotRecognised | Outcome::Cancelled => 16,
        }
    }
}

/// The cache key of a source: content hash and trust. Theme and scale are
/// not part of it: diagrams are theme-agnostic vectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DiagramKey {
    len: usize,
    hash: u64,
    trust: Trust,
}

impl DiagramKey {
    /// The key of `source` at `trust`: one pass over the text, cheap enough
    /// for the UI thread.
    pub fn new(source: &str, trust: Trust) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        source.hash(&mut h);
        Self {
            len: source.len(),
            hash: h.finish(),
            trust,
        }
    }

    /// The content hash (for logs: never log diagram sources).
    pub fn hash(&self) -> u64 {
        self.hash
    }
}

/// Cancels a running [`Job`] from any thread.
#[derive(Debug, Clone)]
pub struct Canceller {
    control: OperationControl,
    flag: Arc<AtomicBool>,
}

impl Canceller {
    /// Stops the job at its next checkpoint; it ends with
    /// [`Outcome::Cancelled`].
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
        self.control.cancel();
    }
}

/// One render, prepared on the UI thread and run on a background thread.
#[derive(Debug)]
pub struct Job {
    source: Arc<str>,
    key: DiagramKey,
    control: OperationControl,
    flag: Arc<AtomicBool>,
}

impl Job {
    /// A handle that cancels this job.
    pub fn canceller(&self) -> Canceller {
        Canceller {
            control: self.control.clone(),
            flag: Arc::clone(&self.flag),
        }
    }

    /// The cache key.
    pub fn key(&self) -> DiagramKey {
        self.key
    }

    /// Runs the job: blocking, milliseconds to seconds. Call it on a
    /// background executor.
    pub fn run(self, engine: &DiagramEngine) -> Outcome {
        engine.run(self)
    }

    /// The job as a future, for an executor's background pool (GPUI's
    /// `background_spawn`). It does the work when first polled, blocking
    /// that thread: never poll it on the UI thread.
    pub async fn run_async(self, engine: Arc<DiagramEngine>) -> Outcome {
        engine.run(self)
    }
}

/// Cache statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    pub entries: usize,
    pub bytes: usize,
    pub hits: u64,
    pub misses: u64,
}

#[derive(Debug, Default)]
struct Cache {
    map: HashMap<DiagramKey, (Outcome, u64)>,
    tick: u64,
    stats: CacheStats,
}

impl Cache {
    fn get(&mut self, key: &DiagramKey) -> Option<Outcome> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.map.get_mut(key)?;
        entry.1 = tick;
        Some(entry.0.clone())
    }

    fn insert(&mut self, key: DiagramKey, value: Outcome, budget: usize) {
        self.tick += 1;
        self.stats.bytes += value.cost();
        if let Some((old, _)) = self.map.insert(key, (value, self.tick)) {
            self.stats.bytes -= old.cost();
        }
        if self.stats.bytes > budget {
            let mut by_age: Vec<(u64, DiagramKey)> =
                self.map.iter().map(|(k, (_, t))| (*t, *k)).collect();
            by_age.sort_unstable_by_key(|(t, _)| *t);
            for (_, k) in by_age {
                if self.stats.bytes <= budget / 4 * 3 {
                    break;
                }
                if let Some((old, _)) = self.map.remove(&k) {
                    self.stats.bytes -= old.cost();
                }
            }
        }
        self.stats.entries = self.map.len();
    }
}

/// Mermaid rendering with caps, a cache and fallback cards. `Send + Sync`:
/// share one per process.
///
/// On the UI thread: [`key`](DiagramKey::new), [`cached`](Self::cached),
/// [`job`](Self::job) and [`Canceller::cancel`]. On a background executor:
/// [`Job::run`] or [`render`](Self::render).
pub struct DiagramEngine {
    interactive: Renderer,
    trusted: Renderer,
    fonts: NativeFontContext,
    limits: Limits,
    cache: Mutex<Cache>,
}

impl std::fmt::Debug for DiagramEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiagramEngine")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl DiagramEngine {
    /// An engine that measures and draws labels with `fonts`.
    pub fn new(fonts: DiagramFonts, limits: Limits) -> Result<Self, SetupError> {
        let ctx = NativeFontContext::from_font_bytes(
            fonts.faces,
            NativeFontLimits::default(),
            &OperationControl::new(),
        )
        .map_err(|e| SetupError(e.to_string()))?;
        let renderer = |t: &TrustLimits| {
            let config = palette::site_config(&fonts.family, t.max_edges, t.max_source_bytes);
            Renderer::new()
                .with_parse_options(ParseOptions::strict())
                .with_presentation_engine(
                    MermaidEngine::new()
                        .with_site_config(SourcePresentationConfig::from_value(config)),
                )
        };
        Ok(Self {
            interactive: renderer(&limits.interactive),
            trusted: renderer(&limits.trusted),
            fonts: ctx,
            limits,
            cache: Mutex::new(Cache::default()),
        })
    }

    /// The caps in force.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// The cached outcome for `key`, without rendering (UI thread).
    pub fn cached(&self, key: &DiagramKey) -> Option<Outcome> {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let hit = cache.get(key);
        if hit.is_some() {
            cache.stats.hits += 1;
        }
        hit
    }

    /// Prepares a render of `source` (UI thread; cheap). The deadline
    /// starts now.
    pub fn job(&self, source: impl Into<Arc<str>>, trust: Trust) -> Job {
        let source = source.into();
        let key = DiagramKey::new(&source, trust);
        let control = OperationControl::new().with_deadline(self.limits.for_trust(trust).deadline);
        Job {
            source,
            key,
            control,
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Renders `source` now (blocking; background thread).
    pub fn render(&self, source: &str, trust: Trust) -> Outcome {
        self.job(source, trust).run(self)
    }

    /// Cache statistics.
    pub fn cache_stats(&self) -> CacheStats {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stats
    }

    /// Empties the cache.
    pub fn clear_cache(&self) {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let stats = cache.stats;
        *cache = Cache::default();
        cache.stats.hits = stats.hits;
        cache.stats.misses = stats.misses;
    }

    /// merman's SVG for `source`, with marker colours, before conversion:
    /// for diagnosing colour mapping. Not for display.
    #[doc(hidden)]
    pub fn debug_svg(&self, source: &str) -> Result<String, String> {
        let control = OperationControl::new().with_deadline(self.limits.interactive.deadline);
        let env = SvgEnvironment::deterministic()
            .with_resource_policy(RenderResourcePolicy::interactive())
            .with_native_font_context(self.fonts.for_operation(&control))
            .map_err(|e| e.to_string())?;
        let request = SvgRequest {
            environment: env,
            pipeline: Some(SvgPipeline::resvg_safe()),
            ..SvgRequest::default()
        };
        match self
            .interactive
            .render(RenderRequest::svg(source, control, request))
        {
            Ok(RenderOutput::Svg(Some(svg))) => Ok(svg.svg().to_owned()),
            Ok(_) => Err("no diagram".into()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn run(&self, job: Job) -> Outcome {
        {
            let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(hit) = cache.get(&job.key) {
                cache.stats.hits += 1;
                return hit;
            }
            cache.stats.misses += 1;
        }
        let key = job.key;
        let outcome = match catch_unwind(AssertUnwindSafe(|| self.compute(&job))) {
            Ok(o) => o,
            Err(_) => {
                // Not retried until the source changes: the outcome is
                // cached under this source's key.
                log::error!(
                    "gv-diagram: renderer panicked (source hash {:016x})",
                    key.hash
                );
                Outcome::Card(Arc::new(FallbackCard::new(
                    CardKind::RendererError,
                    "The diagram renderer failed on this diagram.".into(),
                    &job.source,
                )))
            }
        };
        if !matches!(outcome, Outcome::Cancelled) {
            self.cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, outcome.clone(), self.limits.cache_bytes);
        }
        outcome
    }

    fn compute(&self, job: &Job) -> Outcome {
        let trust = job.key.trust;
        let caps = *self.limits.for_trust(trust);
        let source: &str = &job.source;
        if source.len() > caps.max_source_bytes {
            return too_large(
                source,
                trust,
                format!(
                    "The source is {} KB; the limit is {} KB.",
                    source.len().div_ceil(1024),
                    caps.max_source_bytes / 1024
                ),
            );
        }
        let (renderer, policy) = match trust {
            Trust::Interactive => (&self.interactive, RenderResourcePolicy::interactive()),
            Trust::Trusted => (&self.trusted, RenderResourcePolicy::trusted_native()),
        };
        let env = match SvgEnvironment::deterministic()
            .with_resource_policy(policy)
            .with_native_font_context(self.fonts.for_operation(&job.control))
        {
            Ok(env) => env,
            Err(e) => return renderer_error(source, &e.to_string()),
        };
        let request = SvgRequest {
            environment: env,
            pipeline: Some(SvgPipeline::resvg_safe()),
            ..SvgRequest::default()
        };
        let result = renderer.render(RenderRequest::svg(source, job.control.clone(), request));
        let svg = match result {
            Ok(RenderOutput::Svg(Some(svg))) => svg,
            Ok(_) | Err(RenderError::NoDiagram) => return Outcome::NotRecognised,
            Err(e) => return self.error_outcome(job, e),
        };
        if job.flag.load(Ordering::Acquire) {
            return Outcome::Cancelled;
        }
        let text = svg.svg();
        let family = role_description(text).unwrap_or_default();
        let fixed = convert::normalise_hardcoded(&convert::clamp_negative_stroke_widths(text));
        let mut options = usvg::Options::default();
        self.fonts.apply_to_usvg_options(&mut options);
        let list_caps = ListCaps {
            max_items: caps.max_items,
            max_elements: caps.max_elements,
        };
        match convert::convert(&fixed, &options, list_caps) {
            Ok((width, height, items, skipped)) => {
                if skipped > 0 {
                    log::debug!("gv-diagram: {family}: {skipped} unsupported SVG nodes skipped");
                }
                Outcome::Rendered(Arc::new(Diagram {
                    family,
                    width,
                    height,
                    items,
                }))
            }
            Err(ConvertError::TooLarge { what, max }) => too_large(
                source,
                trust,
                format!("The drawing has more than {max} {what}."),
            ),
            Err(ConvertError::Svg(e)) => renderer_error(source, &e),
        }
    }

    fn error_outcome(&self, job: &Job, e: RenderError) -> Outcome {
        let source: &str = &job.source;
        let trust = job.key.trust;
        match e {
            RenderError::Cancelled(_) => {
                if job.flag.load(Ordering::Acquire) {
                    Outcome::Cancelled
                } else {
                    let secs = self.limits.for_trust(trust).deadline.as_secs();
                    too_large(
                        source,
                        trust,
                        format!("Rendering took longer than {secs} s."),
                    )
                }
            }
            RenderError::ResourceLimitExceeded(r) => too_large(
                source,
                trust,
                format!("Limit `{}` exceeded ({} > {}).", r.id, r.actual, r.maximum),
            ),
            RenderError::Parse(d) => {
                let message = d.terminal_safe_message();
                // Mermaid's own caps (maxEdges, maxTextSize) are reported
                // as parse errors.
                if d.is_resource_limit()
                    || message.contains("limit exceeded")
                    || message.contains("maxEdges")
                    || message.contains("maxTextSize")
                {
                    return too_large(source, trust, first_line(&message));
                }
                let details = d.terminal_diagnostic_details();
                if is_unrecognised(&details.code) {
                    return Outcome::NotRecognised;
                }
                let mut card = FallbackCard::new(CardKind::Error, first_line(&message), source);
                if let Some(span) = details.span {
                    card = card.with_span(span.start, span.end);
                }
                Outcome::Card(Arc::new(card))
            }
            // The model merman built is not drawable: a problem with the
            // diagram, not with the renderer.
            RenderError::Svg(e) => Outcome::Card(Arc::new(FallbackCard::new(
                CardKind::Error,
                first_line(&e.to_string()),
                source,
            ))),
            other => {
                let message = other.to_string();
                renderer_error(source, &message)
            }
        }
    }
}

/// Diagnostic codes that mean "no diagram type matched".
fn is_unrecognised(code: &str) -> bool {
    let c = code.to_ascii_lowercase();
    c.contains("no_diagram_detected") || c.contains("unsupported_diagram")
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    let mut out: String = line.chars().take(300).collect();
    if out.is_empty() {
        out = "The diagram could not be parsed.".into();
    }
    out
}

fn too_large(source: &str, trust: Trust, limit: String) -> Outcome {
    let card = FallbackCard::new(CardKind::TooLarge, limit, source)
        .with_render_anyway(trust == Trust::Interactive);
    Outcome::Card(Arc::new(card))
}

fn renderer_error(source: &str, message: &str) -> Outcome {
    log::warn!("gv-diagram: renderer error: {}", first_line(message));
    Outcome::Card(Arc::new(FallbackCard::new(
        CardKind::RendererError,
        first_line(message),
        source,
    )))
}

/// The `aria-roledescription` of the root `<svg>`: merman's diagram type.
fn role_description(svg: &str) -> Option<String> {
    const KEY: &str = "aria-roledescription=\"";
    let start = svg.find(KEY)? + KEY.len();
    let rest = &svg[start..];
    Some(rest[..rest.find('"')?].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DiagramEngine>();
        assert_send_sync::<Outcome>();
        assert_send_sync::<Job>();
        assert_send_sync::<Canceller>();
    }

    #[test]
    fn job_futures_are_send() {
        fn assert_send_static<T: Send + 'static>(_: &T) {}
        let face = ratex_katex_fonts::ttf_bytes("KaTeX_SansSerif-Regular.ttf")
            .expect("bundled font")
            .into_owned();
        let fonts = DiagramFonts {
            family: "KaTeX_SansSerif".into(),
            faces: vec![face],
        };
        let e = Arc::new(DiagramEngine::new(fonts, Limits::default()).expect("engine"));
        let job = e.job("graph TD; A-->B", Trust::Interactive);
        let fut = job.run_async(Arc::clone(&e));
        assert_send_static(&fut);
    }

    #[test]
    fn keys_depend_on_text_and_trust() {
        let a = DiagramKey::new("graph TD; A-->B", Trust::Interactive);
        assert_eq!(a, DiagramKey::new("graph TD; A-->B", Trust::Interactive));
        assert_ne!(a, DiagramKey::new("graph TD; A-->C", Trust::Interactive));
        assert_ne!(a, DiagramKey::new("graph TD; A-->B", Trust::Trusted));
    }
}
