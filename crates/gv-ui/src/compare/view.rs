// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The compare view's element tree (not designed; see the module docs).

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, div,
    px,
};

use super::{Comparison, HunkKind, Pick, Region};
use crate::appearance::Appearance;
use crate::editor::EditorView;
use crate::metrics::size::infobar as button_size;
use crate::metrics::spacing::infobar as spacing;
use crate::metrics::{font, radius, size, text};
use crate::theme::{Color, Theme};

/// Unchanged runs longer than this show their first and last
/// [`CONTEXT_LINES`] lines only.
const SHORTEN_AFTER: usize = 8;
const CONTEXT_LINES: usize = 3;

/// The most lines a side of one region shows.
const MAX_SIDE_LINES: usize = 400;

/// Two versions of a document side by side, with a choice per change.
/// Shown in place of the document; the Source view it belongs to applies
/// the result.
pub struct CompareView {
    comparison: Comparison,
    title: SharedString,
    editor: WeakEntity<EditorView>,
}

impl std::fmt::Debug for CompareView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompareView")
            .field("title", &self.title)
            .field("comparison", &self.comparison)
            .finish_non_exhaustive()
    }
}

impl CompareView {
    pub fn new(
        comparison: Comparison,
        title: SharedString,
        editor: WeakEntity<EditorView>,
    ) -> Self {
        Self {
            comparison,
            title,
            editor,
        }
    }

    pub fn comparison(&self) -> &Comparison {
        &self.comparison
    }

    /// Chooses a side for changed region `hunk`.
    pub fn pick(&mut self, hunk: usize, pick: Pick, cx: &mut Context<'_, Self>) {
        self.comparison.pick(hunk, pick);
        cx.notify();
    }

    pub fn pick_all(&mut self, pick: Pick, cx: &mut Context<'_, Self>) {
        self.comparison.pick_all(pick);
        cx.notify();
    }

    /// Hands the result to the Source view, which replaces its text with
    /// it as one undoable edit and closes the view.
    pub fn apply(&mut self, cx: &mut Context<'_, Self>) {
        let result = self.comparison.result();
        if let Some(editor) = self.editor.upgrade() {
            editor.update(cx, |e, cx| e.finish_compare(Some(result), cx));
        }
    }

    /// Closes the view without changing anything.
    pub fn cancel(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(editor) = self.editor.upgrade() {
            editor.update(cx, |e, cx| e.finish_compare(None, cx));
        }
    }
}

/// The lines of `text` without their terminators.
fn lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split_inclusive('\n')
        .flat_map(|l| {
            let l = l.strip_suffix('\n').unwrap_or(l);
            let l = l.strip_suffix('\r').unwrap_or(l);
            // A lone CR ends a line too.
            l.split('\r')
        })
        .collect()
}

/// A bordered button like the infobar's; `on` tints it as chosen.
fn button(
    id: impl Into<SharedString>,
    label: &'static str,
    on: bool,
    theme: &Theme,
) -> gpui::Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.to_string();
    let (hover, press) = (theme.state.hover, theme.state.press);
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .h(px(button_size::BUTTON_HEIGHT))
        .px(px(spacing::BUTTON_PAD_X))
        .flex()
        .items_center()
        .rounded(px(radius::CONTROL))
        .border_1()
        .border_color(theme.border.strong)
        .bg(if on {
            theme.accent.tint
        } else {
            theme.surface.raised
        })
        .text_color(theme.text.primary)
        .font_weight(FontWeight(text::weight::ACTIVE))
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(press))
        .child(label)
}

/// Lines of text in the editor's font.
fn text_lines(lines: &[&str], color: Color) -> Div {
    let shown = lines.len().min(MAX_SIDE_LINES);
    div()
        .flex()
        .flex_col()
        .font_family(font::EDITOR)
        .text_size(px(text::EDITOR.size))
        .line_height(px(text::EDITOR.line))
        .text_color(color)
        .children(lines[..shown].iter().map(|line| {
            // An empty line keeps its height.
            let line: SharedString = if line.is_empty() {
                " ".into()
            } else {
                line.replace('\t', "    ").into()
            };
            div().whitespace_nowrap().child(line)
        }))
        .when(lines.len() > shown, |d| {
            d.child(format!("… {} more lines", lines.len() - shown))
        })
}

impl CompareView {
    fn header(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> Div {
        let changes = self.comparison.hunk_count();
        let unresolved = self.comparison.unresolved();
        let mut summary = match changes {
            0 => "No differences".to_owned(),
            1 => "1 change".to_owned(),
            n => format!("{n} changes"),
        };
        match unresolved {
            0 => {}
            1 => summary.push_str(" · 1 conflict to choose"),
            n => summary.push_str(&format!(" · {n} conflicts to choose")),
        }
        div()
            .flex_none()
            .min_h(px(size::INFOBAR))
            .flex()
            .items_center()
            .gap(px(spacing::GAP))
            .pt(px(spacing::PAD_TOP))
            .pr(px(spacing::PAD_RIGHT))
            .pb(px(spacing::PAD_BOTTOM))
            .pl(px(spacing::PAD_LEFT))
            .bg(theme.surface.panel)
            .border_b_1()
            .border_color(theme.border.default)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .gap(px(spacing::GAP))
                    .child(
                        div()
                            .font_weight(FontWeight(text::weight::STRONG))
                            .child(self.title.clone()),
                    )
                    .child(
                        div()
                            .debug_selector(|| "compare-summary".into())
                            .text_color(theme.text.secondary)
                            .child(summary),
                    ),
            )
            .child(
                button("compare-all-left", "Use all left", false, theme)
                    .on_click(cx.listener(|this, _, _, cx| this.pick_all(Pick::Left, cx))),
            )
            .child(
                button("compare-all-right", "Use all right", false, theme)
                    .on_click(cx.listener(|this, _, _, cx| this.pick_all(Pick::Right, cx))),
            )
            .child(
                button("compare-cancel", "Cancel", false, theme)
                    .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
            )
            .child(
                button("compare-apply", "Apply", false, theme)
                    .bg(theme.accent.default)
                    .border_color(theme.accent.default)
                    .text_color(theme.accent.on)
                    .on_click(cx.listener(|this, _, _, cx| this.apply(cx))),
            )
    }

    fn side_names(&self, theme: &Theme) -> Div {
        let sides = self.comparison.sides();
        div()
            .flex_none()
            .flex()
            .gap(px(spacing::GAP))
            .px(px(spacing::PAD_LEFT))
            .py(px(spacing::PAD_TOP))
            .border_b_1()
            .border_color(theme.border.default)
            .text_size(px(text::UI_SMALL.size))
            .line_height(px(text::UI_SMALL.line))
            .text_color(theme.text.secondary)
            .child(div().flex_1().min_w_0().child(sides.left.clone()))
            .child(div().flex_1().min_w_0().child(sides.right.clone()))
    }

    fn unchanged(text: &str, theme: &Theme) -> Div {
        let all = lines(text);
        let block = div().px(px(spacing::PAD_LEFT)).py(px(spacing::PAD_TOP));
        if all.len() <= SHORTEN_AFTER {
            return block.child(text_lines(&all, theme.text.secondary));
        }
        let hidden = all.len() - 2 * CONTEXT_LINES;
        block
            .child(text_lines(&all[..CONTEXT_LINES], theme.text.secondary))
            .child(
                div()
                    .text_size(px(text::UI_SMALL.size))
                    .text_color(theme.text.tertiary)
                    .child(format!("⋯ {hidden} unchanged lines")),
            )
            .child(text_lines(
                &all[all.len() - CONTEXT_LINES..],
                theme.text.secondary,
            ))
    }

    fn hunk(
        &self,
        index: usize,
        hunk: &super::Hunk,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> Div {
        let what = match hunk.kind {
            HunkKind::Left => format!("Changed in {}", self.comparison.sides().left),
            HunkKind::Right => format!("Changed in {}", self.comparison.sides().right),
            HunkKind::Same => "The same change on both sides".to_owned(),
            HunkKind::Conflict => "Changed on both sides".to_owned(),
        };
        let pick = hunk.pick;
        let left_on = matches!(pick, Some(Pick::Left | Pick::Both));
        let right_on = matches!(pick, Some(Pick::Right | Pick::Both));
        let side = |text: &str, on: bool| {
            let all = lines(text);
            let body = if all.is_empty() {
                div()
                    .text_size(px(text::UI_SMALL.size))
                    .text_color(theme.text.tertiary)
                    .child("Nothing")
            } else {
                text_lines(
                    &all,
                    if on {
                        theme.text.primary
                    } else {
                        theme.text.tertiary
                    },
                )
            };
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .px(px(spacing::BUTTON_PAD_X))
                .py(px(spacing::PAD_TOP))
                .rounded(px(radius::CONTROL))
                .when(on, |d| d.bg(theme.accent.tint))
                .child(body)
        };
        let choose = |which: Pick, label: &'static str, on: bool| {
            let id = format!(
                "compare-hunk-{index}-{}",
                match which {
                    Pick::Left => "left",
                    Pick::Right => "right",
                    Pick::Both => "both",
                }
            );
            button(id, label, on, theme).on_click(cx.listener(move |this, _, _, cx| {
                this.pick(index, which, cx);
            }))
        };
        let border = if pick.is_none() {
            theme.status.error
        } else {
            theme.border.strong
        };
        div()
            .debug_selector(move || format!("compare-hunk-{index}"))
            .mx(px(spacing::PAD_LEFT))
            .my(px(spacing::PAD_TOP))
            .p(px(spacing::PAD_TOP))
            .flex()
            .flex_col()
            .gap(px(spacing::PAD_TOP))
            .rounded(px(radius::BLOCK))
            .border_1()
            .border_color(border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(spacing::GAP))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(theme.text.secondary)
                            .child(what),
                    )
                    .child(choose(Pick::Left, "Use left", pick == Some(Pick::Left)))
                    .child(choose(Pick::Right, "Use right", pick == Some(Pick::Right)))
                    .child(choose(Pick::Both, "Use both", pick == Some(Pick::Both))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(spacing::GAP))
                    .child(side(&hunk.left, left_on))
                    .child(side(&hunk.right, right_on)),
            )
    }
}

impl Render for CompareView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let mut index = 0;
        let regions: Vec<AnyElement> = self
            .comparison
            .regions()
            .to_vec()
            .iter()
            .map(|region| match region {
                Region::Unchanged(text) => Self::unchanged(text, theme).into_any_element(),
                Region::Changed(hunk) => {
                    let element = self.hunk(index, hunk, theme, cx).into_any_element();
                    index += 1;
                    element
                }
            })
            .collect();
        div()
            .debug_selector(|| "compare-view".into())
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.surface.page)
            .text_color(theme.text.primary)
            .text_size(px(text::UI.size))
            .line_height(px(text::UI.line))
            .child(self.header(theme, cx))
            .child(self.side_names(theme))
            .child(
                div()
                    .id("compare-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .pb(px(spacing::GAP))
                    .children(regions),
            )
    }
}
