// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Live Preview inline widgets (FR-EDIT-17, phase 2): what a construct
//! shows in place of its source while no selection touches it.
//!
//! - **Images** `![alt](src)`, `![[img.png]]`, `![[img.png|200]]`: a
//!   thumbnail one line high (a size in the alias or the alt text caps its
//!   width; the column width caps it too), decoded on the background
//!   executor through the reader's media engine. Only local files below
//!   the vault (or the document's folder) are shown; remote, `data:`,
//!   missing and undecodable images, and images still loading, show their
//!   alt text (or file name) on a chip.
//! - **Inline math** `$…$`: typeset by the reader's math engine and drawn
//!   as paths, scaled down to fit the row, with the math macros the
//!   note's formulas define before it (as the Reading view). A formula
//!   that does not typeset shows its source in the error colour.
//! - **Task checkboxes** `[ ]` / `[x]`: a box as wide as the source, which
//!   a click toggles (one undoable edit).
//! - **Emoji shortcodes** `:smile:` (profiles with emoji): the emoji.
//! - **Tags** `#tag`: the tag on a chip; a click runs the tag's search.
//!
//! A widget is a replacement of the display map's replace layer
//! ([`Widget`]): boxes (images, math, checkboxes) take their pixel width
//! in soft wrap and in the row (a space spaced out to it), text widgets
//! (emoji, tags, placeholders, math errors) are shaped as text in a style
//! of their own. Not designed: chips, the checkbox and the placeholder are
//! built from existing tokens and metrics.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash as _, Hasher as _};
use std::path::PathBuf;
use std::sync::Arc;

use gpui::RenderImage;
use gv_editor::display_map::{StyleId, Widget, WidgetId};
use gv_math::{MacroSet, MathLayout};
use gv_media::{CancelFlag, DecodeRequest};

use super::highlight::Style;
use crate::inline::Token;
use crate::reader::Resolver;

/// What a widget shows, from the syntax tree alone (no offsets: kept in
/// the per-leaf decoration cache).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum WidgetSpec {
    /// An image: its destination, whether it is an embed (`![[…]]`), its
    /// alt text and a width from its size (`|200`).
    Image {
        destination: Arc<str>,
        embed: bool,
        alt: Arc<str>,
        width: Option<u32>,
    },
    /// Inline math: the TeX and the source it replaces.
    Math { tex: Arc<str>, source: Arc<str> },
    /// A task's checkbox (`[ ]`, `[x]`).
    Task { checked: bool },
    /// An emoji for a shortcode.
    Emoji { glyph: &'static str },
    /// A tag (`#name`, as written).
    Tag { text: Arc<str> },
}

/// Text widget styles: ids above any row's highlight table.
pub(crate) const TAG_STYLE: StyleId = StyleId(u32::MAX - 1);
pub(crate) const PLACEHOLDER_STYLE: StyleId = StyleId(u32::MAX - 2);
pub(crate) const MATH_ERROR_STYLE: StyleId = StyleId(u32::MAX - 3);
pub(crate) const EMOJI_STYLE: StyleId = StyleId(u32::MAX - 4);

/// The style a widget style id stands for.
pub(crate) fn widget_style(id: StyleId) -> Option<Style> {
    Some(match id {
        TAG_STYLE => Style::of(Token::AccentDefault),
        PLACEHOLDER_STYLE => Style {
            italic: true,
            ..Style::of(Token::TextSecondary)
        },
        MATH_ERROR_STYLE => Style::of(Token::StatusError),
        EMOJI_STYLE => Style::PLAIN,
        _ => return None,
    })
}

/// What the view draws for a widget.
#[derive(Debug, Clone)]
pub(crate) enum WidgetPaint {
    /// A decoded image, fitted in the box.
    Image(Arc<RenderImage>),
    /// A formula, `em` px per em, its baseline `baseline` px below the
    /// row's top.
    Math {
        layout: Arc<MathLayout>,
        em: f32,
        baseline: f32,
    },
    /// A task's checkbox.
    Task { checked: bool },
    /// Text on a chip (tags, image placeholders).
    Chip,
    /// Nothing besides its text (emoji, math errors).
    Text,
}

/// What a click on a widget does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WidgetClick {
    /// Toggles the task whose `[ ]` the widget replaces.
    Task,
    /// Runs the tag search for this tag (`#name`).
    Tag(Arc<str>),
    /// Puts the caret there: the construct shows its source.
    Reveal,
}

/// The sizes widgets are made for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Sizes {
    /// The row height.
    pub line: f32,
    /// The editor font size.
    pub font: f32,
    /// The advance of a character of the (monospace) editor font.
    pub advance: f32,
    /// The text column's width.
    pub column: f32,
    /// Device pixels per px.
    pub scale: f32,
}

impl Sizes {
    fn key(&self) -> [u32; 5] {
        [
            self.line.to_bits(),
            self.font.to_bits(),
            self.advance.to_bits(),
            self.column.to_bits(),
            self.scale.to_bits(),
        ]
    }
}

/// An image file found for a destination, decoded or not.
#[derive(Debug, Clone)]
enum ImageState {
    /// The job runs.
    Loading,
    /// Decoded: the image and its size as displayed.
    Ready {
        image: Arc<RenderImage>,
        width: u32,
        height: u32,
    },
    /// Not shown (remote, missing, not an image, not decodable).
    Failed,
}

/// An image to find and decode, for the background executor.
#[derive(Debug, Clone)]
pub(crate) struct ImageJob {
    pub key: ImageKey,
    resolver: Resolver,
    /// The box to decode for, in device pixels.
    fit: (u32, u32),
    pub cancel: CancelFlag,
}

/// An image destination as written, and whether it is an embed.
pub(crate) type ImageKey = (Arc<str>, bool);

/// What an image job found.
#[derive(Debug)]
pub(crate) struct ImageResult {
    image: Option<(Arc<RenderImage>, u32, u32)>,
}

/// The widgets of a view: images by destination, and what each widget id
/// draws and does.
#[derive(Debug, Default)]
pub(crate) struct Widgets {
    images: HashMap<ImageKey, ImageState>,
    /// Bumped when an image lands or the sizes change: the widgets are
    /// worked out again.
    generation: u64,
    sizes: Option<[u32; 5]>,
    jobs: Vec<ImageJob>,
    /// The images being decoded, with their jobs' cancel flags.
    loading: HashMap<ImageKey, CancelFlag>,
    paints: HashMap<WidgetId, (WidgetPaint, WidgetClick)>,
}

/// A widget id for `spec` in its current state (`state` tells states
/// apart).
fn id_of(spec: &WidgetSpec, state: u64) -> WidgetId {
    let mut hasher = DefaultHasher::new();
    spec.hash(&mut hasher);
    state.hash(&mut hasher);
    WidgetId(hasher.finish())
}

/// The file name of an image destination, for its placeholder.
fn file_name(destination: &str) -> &str {
    let path = destination
        .split(['#', '?', '|'])
        .next()
        .unwrap_or(destination);
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

impl Widgets {
    /// Bumped when what widgets show may have changed.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// New sizes (a zoom, a resize): widgets are measured again.
    pub fn set_sizes(&mut self, sizes: &Sizes) {
        let key = sizes.key();
        if self.sizes != Some(key) {
            if self
                .sizes
                .is_some_and(|old| old[4] != key[4] || old[0] != key[0])
            {
                // Decoded for another box.
                self.images.clear();
            }
            self.sizes = Some(key);
            self.generation += 1;
        }
    }

    /// Forgets everything (another document).
    pub fn reset(&mut self) {
        for job in &self.jobs {
            job.cancel.cancel();
        }
        for cancel in self.loading.values() {
            cancel.cancel();
        }
        *self = Self {
            generation: self.generation + 1,
            ..Self::default()
        };
    }

    /// What widget `id` draws and what a click on it does.
    pub fn paint(&self, id: WidgetId) -> Option<&(WidgetPaint, WidgetClick)> {
        self.paints.get(&id)
    }

    /// Images to decode, taken.
    pub fn take_jobs(&mut self) -> Vec<ImageJob> {
        std::mem::take(&mut self.jobs)
    }

    /// Cancels the image jobs for images not in `keys` (the images near
    /// the lines on screen): they start again when those images come back.
    pub fn keep_loading(&mut self, keys: &std::collections::HashSet<ImageKey>) {
        self.loading.retain(|key, cancel| {
            let keep = keys.contains(key);
            if !keep {
                cancel.cancel();
                self.images.remove(key);
            }
            keep
        });
    }

    /// Images being decoded.
    #[cfg(test)]
    pub fn loading_count(&self) -> usize {
        self.loading.len()
    }

    /// An image job finished.
    pub fn landed(&mut self, key: &ImageKey, result: ImageResult) {
        // A job cancelled since (its image left the screen) lands nothing.
        if self.loading.remove(key).is_none_or(|c| c.is_cancelled()) {
            return;
        }
        let state = match result.image {
            Some((image, width, height)) => ImageState::Ready {
                image,
                width,
                height,
            },
            None => ImageState::Failed,
        };
        self.images.insert(key.clone(), state);
        self.generation += 1;
    }

    /// The widget `spec` shows now, measured for `sizes`; an image not
    /// known yet starts a job when `load` (with `resolver`; without one
    /// it fails), else shows its placeholder. Inline math is typeset with
    /// `macros` (the note's, defined before it), part of the widget's id.
    pub fn widget(
        &mut self,
        spec: &WidgetSpec,
        sizes: &Sizes,
        resolver: Option<&Resolver>,
        load: bool,
        macros: Option<&MacroSet>,
    ) -> Widget {
        let (widget, paint, click) = match spec {
            WidgetSpec::Task { checked } => {
                let id = id_of(spec, 0);
                (
                    Widget::boxed(id, sizes.advance * 3.0, None),
                    WidgetPaint::Task { checked: *checked },
                    WidgetClick::Task,
                )
            }
            WidgetSpec::Emoji { glyph } => {
                let id = id_of(spec, 0);
                (
                    Widget::text(id, glyph, Some(EMOJI_STYLE)),
                    WidgetPaint::Text,
                    WidgetClick::Reveal,
                )
            }
            WidgetSpec::Tag { text } => {
                let id = id_of(spec, 0);
                (
                    Widget::text(id, text, Some(TAG_STYLE)),
                    WidgetPaint::Chip,
                    WidgetClick::Tag(Arc::clone(text)),
                )
            }
            WidgetSpec::Math { tex, source } => {
                let empty = MacroSet::empty();
                let macros = macros.unwrap_or(&empty);
                let salt = macros.hash_value();
                match crate::reader::math_engine().render(tex, false, macros) {
                    Ok(layout) => {
                        let ink = (layout.height + layout.depth).max(f32::EPSILON);
                        let em = sizes.font.min(sizes.line * 0.9 / ink);
                        // The baseline where the body text's is, unless
                        // the formula would leave the row.
                        let natural = (sizes.line + sizes.font * 0.7) / 2.0;
                        let baseline = natural
                            .max(layout.height * em)
                            .min(sizes.line - layout.depth * em);
                        let width = (layout.width * em).max(1.0);
                        let id = id_of(spec, u64::from(width.to_bits()) ^ salt.rotate_left(32));
                        (
                            Widget::boxed(id, width, None),
                            WidgetPaint::Math {
                                layout,
                                em,
                                baseline,
                            },
                            WidgetClick::Reveal,
                        )
                    }
                    Err(_) => {
                        let id = id_of(spec, 1 ^ salt.rotate_left(32));
                        (
                            Widget::text(id, source, Some(MATH_ERROR_STYLE)),
                            WidgetPaint::Text,
                            WidgetClick::Reveal,
                        )
                    }
                }
            }
            WidgetSpec::Image {
                destination,
                embed,
                alt,
                width,
            } => self.image(
                spec,
                destination,
                *embed,
                alt,
                *width,
                sizes,
                resolver.filter(|_| load),
                load,
            ),
        };
        self.paints.insert(widget.id, (paint, click));
        widget
    }

    #[allow(clippy::too_many_arguments)]
    fn image(
        &mut self,
        spec: &WidgetSpec,
        destination: &Arc<str>,
        embed: bool,
        alt: &str,
        width: Option<u32>,
        sizes: &Sizes,
        resolver: Option<&Resolver>,
        load: bool,
    ) -> (Widget, WidgetPaint, WidgetClick) {
        let key: ImageKey = (Arc::clone(destination), embed);
        let cap = sizes.column.max(sizes.line);
        let max_width = width.map_or(cap, |w| (w as f32).min(cap));
        let state = match self.images.get(&key) {
            Some(state) => state.clone(),
            // Away from the screen: the placeholder until it comes near.
            None if !load => ImageState::Loading,
            None => match resolver {
                Some(resolver) => {
                    let device = |px: f32| (px * sizes.scale).ceil().max(1.0) as u32;
                    let cancel = CancelFlag::new();
                    self.jobs.push(ImageJob {
                        key: key.clone(),
                        resolver: resolver.clone(),
                        fit: (device(max_width), device(sizes.line)),
                        cancel: cancel.clone(),
                    });
                    self.loading.insert(key.clone(), cancel);
                    self.images.insert(key, ImageState::Loading);
                    ImageState::Loading
                }
                None => ImageState::Failed,
            },
        };
        match state {
            ImageState::Ready {
                image,
                width: w,
                height: h,
            } if w > 0 && h > 0 => {
                let shown = (sizes.line * w as f32 / h as f32).min(max_width).max(1.0);
                let id = id_of(spec, u64::from(shown.to_bits()));
                (
                    Widget::boxed(id, shown, None),
                    WidgetPaint::Image(image),
                    WidgetClick::Reveal,
                )
            }
            ImageState::Loading | ImageState::Ready { .. } | ImageState::Failed => {
                let label = if alt.trim().is_empty() {
                    file_name(destination)
                } else {
                    alt.trim()
                };
                let label = if label.is_empty() { "image" } else { label };
                let id = id_of(spec, 2);
                (
                    Widget::text(id, label, Some(PLACEHOLDER_STYLE)),
                    WidgetPaint::Chip,
                    WidgetClick::Reveal,
                )
            }
        }
    }
}

/// Finds and decodes an image (blocking: the background executor only).
/// Local files below the resolver's root only.
pub(crate) fn load_image(job: &ImageJob) -> ImageResult {
    let failed = ImageResult { image: None };
    let resolver = job.resolver.for_images();
    let root = resolver.root().to_path_buf();
    let (destination, embed) = (&*job.key.0, job.key.1);
    let path: PathBuf = if embed {
        let res = resolver.resolve_embed(destination, None);
        match (&res.target_path, res.target_kind) {
            (Some(p), Some(gv_vault::TargetKind::File)) if res.status.is_resolved() => {
                p.to_fs_path(&root)
            }
            _ => return failed,
        }
    } else {
        match crate::reader::media::image_target(destination, None, &resolver, &root) {
            crate::reader::media::ImageTarget::Local { path, probe: Ok(_) } => path,
            _ => return failed,
        }
    };
    let inside = std::path::absolute(&path)
        .ok()
        .zip(std::path::absolute(&root).ok())
        .is_some_and(|(p, r)| p.starts_with(r));
    if !inside || !path.is_file() {
        return failed;
    }
    let engine = crate::reader::media::media_engine(&root);
    let Ok(probe) = crate::reader::media::probe(&engine, &path) else {
        return failed;
    };
    let request = DecodeRequest {
        fit: Some(job.fit),
        animate: false,
    };
    let Ok(decoded) = engine.load_path(&path, request, &job.cancel) else {
        return failed;
    };
    match crate::reader::media::render_image(&decoded) {
        Some(image) => ImageResult {
            image: Some((image, probe.width, probe.height)),
        },
        None => failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sizes() -> Sizes {
        Sizes {
            line: 20.0,
            font: 14.0,
            advance: 8.0,
            column: 600.0,
            scale: 1.0,
        }
    }

    #[test]
    fn task_boxes_are_as_wide_as_their_source() {
        let mut widgets = Widgets::default();
        let widget = widgets.widget(
            &WidgetSpec::Task { checked: true },
            &sizes(),
            None,
            true,
            None,
        );
        assert_eq!(
            widget.content,
            gv_editor::display_map::WidgetContent::Box { width: 24.0 }
        );
        assert!(matches!(
            widgets.paint(widget.id),
            Some((WidgetPaint::Task { checked: true }, WidgetClick::Task))
        ));
    }

    #[test]
    fn math_fits_the_row_and_errors_show_the_source() {
        let mut widgets = Widgets::default();
        let spec = WidgetSpec::Math {
            tex: "\\frac{a}{b}".into(),
            source: "$\\frac{a}{b}$".into(),
        };
        let widget = widgets.widget(&spec, &sizes(), None, true, None);
        let Some((WidgetPaint::Math { layout, em, .. }, _)) = widgets.paint(widget.id) else {
            panic!("no formula");
        };
        assert!((layout.height + layout.depth) * em <= 20.0 * 0.9 + 0.01);
        let bad = WidgetSpec::Math {
            tex: "\\frac{".into(),
            source: "$\\frac{$".into(),
        };
        let widget = widgets.widget(&bad, &sizes(), None, true, None);
        assert_eq!(widget.display_text(), "$\\frac{$");
        assert_eq!(widget.style, Some(MATH_ERROR_STYLE));
    }

    #[test]
    fn math_uses_the_notes_macros_and_they_are_part_of_its_id() {
        let mut widgets = Widgets::default();
        let spec = WidgetSpec::Math {
            tex: "\\RR^2".into(),
            source: "$\\RR^2$".into(),
        };
        // Unknown without the note's macros: the source, as an error.
        let without = widgets.widget(&spec, &sizes(), None, true, None);
        assert_eq!(without.style, Some(MATH_ERROR_STYLE));
        let macros = MacroSet::from_pairs([("\\RR", "\\mathbb{R}")]);
        let with = widgets.widget(&spec, &sizes(), None, true, Some(&macros));
        assert!(matches!(
            widgets.paint(with.id),
            Some((WidgetPaint::Math { .. }, _))
        ));
        assert_ne!(with.id, without.id);
        let other = MacroSet::from_pairs([("\\RR", "\\mathbf{R}")]);
        let changed = widgets.widget(&spec, &sizes(), None, true, Some(&other));
        assert_ne!(changed.id, with.id);
    }

    #[test]
    fn images_without_a_file_show_their_alt_text() {
        let mut widgets = Widgets::default();
        let spec = |alt: &str| WidgetSpec::Image {
            destination: "pics/cat.png".into(),
            embed: false,
            alt: alt.into(),
            width: None,
        };
        let widget = widgets.widget(&spec("A cat"), &sizes(), None, true, None);
        assert_eq!(widget.display_text(), "A cat");
        assert_eq!(widget.style, Some(PLACEHOLDER_STYLE));
        let widget = widgets.widget(&spec(""), &sizes(), None, true, None);
        assert_eq!(widget.display_text(), "cat.png");
    }

    #[test]
    fn images_decode_only_near_the_screen_and_jobs_that_leave_it_stop() {
        let mut widgets = Widgets::default();
        let resolver = Resolver::statuses(
            std::path::Path::new("/vault/note.md"),
            None,
            gv_profile::ProfileId::Obsidian,
        );
        let spec = WidgetSpec::Image {
            destination: "cat.png".into(),
            embed: false,
            alt: "A cat".into(),
            width: None,
        };
        // Away from the screen: its placeholder, no job.
        let widget = widgets.widget(&spec, &sizes(), Some(&resolver), false, None);
        assert_eq!(widget.display_text(), "A cat");
        assert!(widgets.take_jobs().is_empty());
        assert_eq!(widgets.loading_count(), 0);
        // Near it: a job.
        widgets.widget(&spec, &sizes(), Some(&resolver), true, None);
        let jobs = widgets.take_jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(widgets.loading_count(), 1);
        // It left the screen: cancelled, and what it decodes is dropped.
        let generation = widgets.generation();
        widgets.keep_loading(&std::collections::HashSet::new());
        assert!(jobs[0].cancel.is_cancelled());
        assert_eq!(widgets.loading_count(), 0);
        widgets.landed(&jobs[0].key, ImageResult { image: None });
        assert_eq!(widgets.generation(), generation);
        // Back near the screen: a new job.
        widgets.widget(&spec, &sizes(), Some(&resolver), true, None);
        assert_eq!(widgets.take_jobs().len(), 1);
        // Kept while it stays near.
        let keys = std::iter::once((Arc::<str>::from("cat.png"), false)).collect();
        widgets.keep_loading(&keys);
        assert_eq!(widgets.loading_count(), 1);
    }
}
