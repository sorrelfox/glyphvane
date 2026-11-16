// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The infobar: one strip under the title row, across the document column
//! (SPEC § Infobars), showing the active tab's notice: its document's
//! (save errors, conflicts, recovery, a deleted file) or the tab's own (a
//! file deleted or a sync-conflict copy seen while only the Reading view
//! is open). One at a time; the document's comes first.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Div, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};

use super::Workspace;
use crate::appearance::Appearance;
use crate::files::notice::{Notice, NoticeAction, NoticeKind};
use crate::icons::Icon;
use crate::metrics::size::infobar as size;
use crate::metrics::spacing::{infobar as spacing, tooltip};
use crate::metrics::{DISABLED_OPACITY, icon, radius, size as sizes, text};
use crate::theme::Theme;

/// Where the notice on screen comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NoticeSource {
    /// The active tab's Source view.
    Document,
    /// The tab itself (no Source view, or none of its own notices).
    Tab,
}

impl Workspace {
    /// The notice the active tab shows, and where it comes from.
    pub(super) fn active_notice(&self, cx: &App) -> Option<(Notice, NoticeSource)> {
        let tab = self.active_tab();
        if let Some(notice) = tab
            .editor
            .as_ref()
            .and_then(|e| e.read(cx).notice().cloned())
        {
            return Some((notice, NoticeSource::Document));
        }
        tab.notice.clone().map(|n| (n, NoticeSource::Tab))
    }

    /// The active tab's infobar, if it has a notice.
    pub(super) fn render_infobar(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> Option<Div> {
        let (notice, source) = self.active_notice(cx)?;
        let (fill, tint, kind_icon) = match notice.kind {
            NoticeKind::Warning => (
                theme.status.warning_tint,
                theme.status.warning,
                Icon::NoticeWarning,
            ),
            NoticeKind::Information => (
                theme.status.info_tint,
                theme.status.info,
                Icon::NoticeInformation,
            ),
            NoticeKind::Error => (
                theme.status.error_tint,
                theme.status.error,
                Icon::NoticeError,
            ),
            NoticeKind::Neutral => (
                theme.surface.sunken,
                theme.text.secondary,
                Icon::NoticeInformation,
            ),
        };
        let message = div()
            .flex_1()
            .min_w_0()
            .text_color(theme.text.primary)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x(px(spacing::GAP / 2.0))
                    .child(
                        div()
                            .font_weight(FontWeight(text::weight::STRONG))
                            .child(notice.title.clone()),
                    )
                    .when_some(notice.detail.clone(), |row, detail| row.child(detail)),
            );
        let buttons: Vec<AnyElement> = notice
            .buttons
            .iter()
            .enumerate()
            .map(|(index, button)| {
                let id = format!("infobar-action-{index}");
                let base = div()
                    .id(SharedString::from(id.clone()))
                    .debug_selector(move || id.clone())
                    .flex_none()
                    .h(px(size::BUTTON_HEIGHT))
                    .px(px(spacing::BUTTON_PAD_X))
                    .flex()
                    .items_center()
                    .rounded(px(radius::CONTROL))
                    .border_1()
                    .border_color(theme.border.strong)
                    .bg(theme.surface.raised)
                    .text_color(theme.text.primary)
                    .font_weight(FontWeight(text::weight::ACTIVE))
                    .child(button.label.clone());
                match &button.disabled {
                    Some(why) => {
                        let why = why.clone();
                        base.opacity(DISABLED_OPACITY)
                            .tooltip(move |_, cx| {
                                let why = why.clone();
                                cx.new(|_| Tooltip(why)).into()
                            })
                            .into_any_element()
                    }
                    None => {
                        let (hover, press) = (theme.state.hover, theme.state.press);
                        let action = button.action.clone();
                        base.hover(move |style| style.bg(hover))
                            .active(move |style| style.bg(press))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.run_notice_action(&action, source, window, cx);
                            }))
                            .into_any_element()
                    }
                }
            })
            .collect();
        let (hover, press) = (theme.state.hover, theme.state.press);
        Some(
            div()
                .debug_selector(|| "infobar".into())
                .flex_none()
                .min_h(px(sizes::INFOBAR))
                .flex()
                .items_center()
                .gap(px(spacing::GAP))
                .pt(px(spacing::PAD_TOP))
                .pr(px(spacing::PAD_RIGHT))
                .pb(px(spacing::PAD_BOTTOM))
                .pl(px(spacing::PAD_LEFT))
                .bg(fill)
                .border_b_1()
                .border_color(theme.border.default)
                .text_size(px(text::UI.size))
                .line_height(px(text::UI.line))
                .child(
                    div()
                        .flex_none()
                        .text_color(tint)
                        .child(kind_icon.sized(icon::ROW)),
                )
                .child(message)
                .children(buttons)
                .child(
                    div()
                        .id("infobar-dismiss")
                        .debug_selector(|| "infobar-dismiss".into())
                        .flex_none()
                        .size(px(size::DISMISS))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(radius::CONTROL))
                        .text_color(theme.text.secondary)
                        .hover(move |style| style.bg(hover))
                        .active(move |style| style.bg(press))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.dismiss_notice(source, cx);
                        }))
                        .child(Icon::Dismiss.sized(icon::ROW)),
                ),
        )
    }

    /// An infobar button was clicked.
    pub(super) fn run_notice_action(
        &mut self,
        action: &NoticeAction,
        source: NoticeSource,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match action {
            NoticeAction::Close => {
                self.close_tab(self.active, window, cx);
            }
            NoticeAction::SwitchProfile => {
                self.toggle_profile_menu(cx);
            }
            NoticeAction::OpenCopy(path) => {
                let path = path.clone();
                self.tabs[self.active].notice = None;
                self.open_tab(super::OpenTarget::new(path), window, cx);
            }
            action => {
                let editor = match source {
                    NoticeSource::Document => self.active_tab().editor.clone(),
                    NoticeSource::Tab => None,
                };
                if let Some(editor) = editor {
                    editor.update(cx, |e, cx| e.notice_action(action, cx));
                }
            }
        }
        cx.notify();
    }

    /// The infobar's dismiss button.
    fn dismiss_notice(&mut self, source: NoticeSource, cx: &mut Context<'_, Self>) {
        match source {
            NoticeSource::Document => {
                if let Some(editor) = self.active_tab().editor.clone() {
                    editor.update(cx, |e, cx| e.dismiss_notice(cx));
                }
            }
            NoticeSource::Tab => {
                let notice = self.tabs[self.active].notice.take();
                self.tab_notice_dismissed(notice.as_ref(), cx);
            }
        }
        cx.notify();
    }
}

/// A tooltip (SPEC § Popovers): inverted colours, 22 high.
pub(super) struct Tooltip(pub(super) SharedString);

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        div()
            .h(px(sizes::TOOLTIP))
            .px(px(tooltip::PAD_X))
            .flex()
            .items_center()
            .rounded(px(radius::CONTROL))
            .bg(theme.text.primary)
            .text_color(theme.surface.page)
            .text_size(px(text::TOOLTIP_SIZE))
            .child(self.0.clone())
    }
}
