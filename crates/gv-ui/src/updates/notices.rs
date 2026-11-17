// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Application notices: infobars about the application rather than a
//! document, shown in every main window above the document's own infobar
//! until dismissed. The update notification and the crash report found at
//! start-up are the two so far.
//!
//! Not designed (NFR-UI-01): they are drawn as the document infobar (SPEC
//! § Infobars), the information kind, with their buttons and a dismiss
//! button. One shows at a time, the oldest first.

use std::rc::Rc;

use gpui::{
    AnyElement, App, FontWeight, Global, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::a11y::A11yExt as _;
use crate::files::notice::NoticeKind;
use crate::icons::Icon;
use crate::metrics::size::infobar as size;
use crate::metrics::spacing::infobar as spacing;
use crate::metrics::{icon, radius, size as sizes, text};
use crate::theme::Theme;

/// What a notice is about; a newer notice on the same topic replaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppTopic {
    /// A newer release is available (FR-PLAT-07).
    Update,
    /// Glyphvane stopped unexpectedly last time and saved a report
    /// (FR-PLAT-08).
    CrashReport,
}

/// A click handler of a notice button.
pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// What runs when a notice is dismissed.
pub type OnDismiss = Rc<dyn Fn(&mut App)>;

/// One button.
#[derive(Clone)]
pub struct AppButton {
    pub label: SharedString,
    pub run: Handler,
    /// Whether the notice goes away after the click.
    pub dismisses: bool,
}

impl std::fmt::Debug for AppButton {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppButton")
            .field("label", &self.label)
            .field("dismisses", &self.dismisses)
            .finish_non_exhaustive()
    }
}

/// One notice.
#[derive(Clone)]
pub struct AppNotice {
    pub topic: AppTopic,
    pub kind: NoticeKind,
    pub title: SharedString,
    pub detail: Option<SharedString>,
    pub buttons: Vec<AppButton>,
    /// Runs when the notice is dismissed with its dismiss button.
    pub on_dismiss: Option<OnDismiss>,
}

impl std::fmt::Debug for AppNotice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppNotice")
            .field("topic", &self.topic)
            .field("title", &self.title)
            .field("detail", &self.detail)
            .field(
                "buttons",
                &self.buttons.iter().map(|b| &b.label).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// The notices waiting, oldest first.
#[derive(Default)]
struct AppNotices(Vec<AppNotice>);

impl Global for AppNotices {}

/// Shows `notice` in every main window, replacing one on the same topic.
pub fn show(cx: &mut App, notice: AppNotice) {
    let notices = &mut cx.default_global::<AppNotices>().0;
    match notices.iter_mut().find(|n| n.topic == notice.topic) {
        Some(slot) => *slot = notice,
        None => notices.push(notice),
    }
    cx.refresh_windows();
}

/// Removes the notice on `topic`, if any.
pub fn remove(cx: &mut App, topic: AppTopic) {
    if let Some(notices) = cx.try_global::<AppNotices>()
        && !notices.0.iter().any(|n| n.topic == topic)
    {
        return;
    }
    cx.default_global::<AppNotices>()
        .0
        .retain(|n| n.topic != topic);
    cx.refresh_windows();
}

/// The notice shown now.
pub fn current(cx: &App) -> Option<AppNotice> {
    cx.try_global::<AppNotices>()
        .and_then(|n| n.0.first().cloned())
}

/// Every notice waiting, oldest first.
pub fn all(cx: &App) -> Vec<AppNotice> {
    cx.try_global::<AppNotices>()
        .map(|n| n.0.clone())
        .unwrap_or_default()
}

/// Clicks button `index` of the current notice, as the mouse does.
pub fn click(index: usize, window: &mut Window, cx: &mut App) {
    let Some(notice) = current(cx) else {
        return;
    };
    let Some(button) = notice.buttons.get(index) else {
        return;
    };
    if button.dismisses {
        remove(cx, notice.topic);
    }
    (button.run)(window, cx);
}

/// Dismisses the current notice, as its dismiss button does.
pub fn dismiss(cx: &mut App) {
    let Some(notice) = current(cx) else {
        return;
    };
    remove(cx, notice.topic);
    if let Some(on_dismiss) = &notice.on_dismiss {
        on_dismiss(cx);
    }
}

/// The infobar of the current application notice, for a main window to
/// place above the document's own infobar.
pub fn render(theme: &Theme, cx: &App) -> Option<AnyElement> {
    let notice = current(cx)?;
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
    let (hover, press) = (theme.state.hover, theme.state.press);
    let buttons = notice.buttons.iter().enumerate().map(|(index, button)| {
        let id = format!("app-notice-action-{index}");
        div()
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
            .hover(move |s| s.bg(hover))
            .active(move |s| s.bg(press))
            .on_click(move |_, window, cx| click(index, window, cx))
            .child(button.label.clone())
            .into_any_element()
    });
    Some(
        div()
            .debug_selector(|| "app-notice".into())
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
            .child(
                div()
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
                            .children(notice.detail.clone()),
                    ),
            )
            .children(buttons)
            .child(
                div()
                    .id("app-notice-dismiss")
                    .a11y_button("Dismiss")
                    .keyboard_control(theme)
                    .debug_selector(|| "app-notice-dismiss".into())
                    .flex_none()
                    .size(px(size::DISMISS))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(radius::CONTROL))
                    .text_color(theme.text.secondary)
                    .hover(move |s| s.bg(hover))
                    .active(move |s| s.bg(press))
                    .on_click(|_, _, cx| dismiss(cx))
                    .child(Icon::Dismiss.sized(icon::ROW)),
            )
            .into_any_element(),
    )
}
