// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The find popover of find in page (SPEC § Popovers, "Find and replace").
//!
//! The find row: a field with the match case (`Aa`), whole word (`ab`) and
//! regular expression (`.*`) toggles, the match count, and the previous,
//! next and close buttons. The replace row, in the Source view only: the
//! replacement field, then Replace and Replace all. The popover is a view
//! of its own, [`FindBar`]; the Reading view and the Source view host it at
//! the top right of the document area, run the searches and tell it what
//! to show ([`FindStatus`]). In the Split view each pane has its own.
//!
//! # Keys
//!
//! Mod+F opens find (filled with the selection when there is one),
//! Ctrl+H (Cmd+Alt+F on macOS) find and replace in the Source view. Enter
//! and Shift+Enter in the find field go to the next and previous match, as
//! do F3 and Shift+F3 (Cmd+G and Cmd+Shift+G on macOS) anywhere in the
//! view; Enter in the replace field replaces the current match. Alt+Enter
//! in either field of the Source view's popover selects every match, one
//! caret each. Esc closes the popover and gives the keyboard back to the
//! document.

pub(crate) mod source;

use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, BoxShadow, Context, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
    point, px,
};
use gv_render_tree::FindOptions;

use crate::a11y::{A11yExt as _, AccessibleExt as _, Live, Role};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::metrics::size::{CONTROL, SMALL_ICON_BUTTON, find as size};
use crate::metrics::spacing::find as spacing;
use crate::metrics::{DISABLED_OPACITY, FOCUS_HALO, HAIRLINE, font, icon, radius, text};
use crate::theme::Theme;

/// Open find in page, filled with the selection if there is one
/// (`search.find`); open find and replace (`search.replace`, the Source
/// view's); go to the next and the previous match (`search.find_next`,
/// `search.find_previous`): commands of the keymap.
pub use crate::keymap::actions::search::{Find, FindNext, FindPrevious, Replace};

gpui::actions!(
    find,
    [
        /// Close find in page.
        CloseFind,
        /// Select every match in the Source view, one caret each.
        SelectAllMatches,
    ]
);

/// The key context of the popover.
pub(crate) const KEY_CONTEXT: &str = "FindBar";

/// The popover's own keys: Esc closes it, in the popover and in the
/// Reading view that hosts it (the Source view's Esc closes it first);
/// Alt+Enter selects every match. Find, replace, next and previous are
/// commands, bound by the keymap ([`crate::keymap`]).
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("escape", CloseFind, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", CloseFind, Some(crate::reader::KEY_CONTEXT)),
        KeyBinding::new("alt-enter", SelectAllMatches, Some(KEY_CONTEXT)),
    ]
}

/// What the match count shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FindStatus {
    /// Nothing to search for.
    #[default]
    Empty,
    /// The regular expression does not parse.
    Invalid,
    /// `total` matches (`more`: there are more than that), the current one
    /// 0-based.
    Matches {
        current: Option<usize>,
        total: usize,
        more: bool,
    },
}

impl FindStatus {
    /// The text of the match count: "3 of 17", "1 of 10,000+",
    /// "No results", "Invalid"; nothing for an empty query.
    pub fn text(&self) -> Option<String> {
        match *self {
            Self::Empty => None,
            Self::Invalid => Some("Invalid".to_owned()),
            Self::Matches { total: 0, .. } => Some("No results".to_owned()),
            Self::Matches {
                current,
                total,
                more,
            } => {
                let total = format!("{}{}", grouped(total), if more { "+" } else { "" });
                Some(match current {
                    Some(k) => format!("{} of {total}", grouped(k + 1)),
                    None => total,
                })
            }
        }
    }
}

/// `n` with thousands separators (the interface is English).
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What the popover asks of the view that hosts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindBarEvent {
    /// The query or an option changed.
    Changed,
    Next,
    Previous,
    Close,
    /// Replace the current match, then go to the next one.
    Replace,
    ReplaceAll,
    /// Select every match, one caret each.
    SelectAll,
}

/// The find popover.
pub struct FindBar {
    input: Entity<InputState>,
    replace: Entity<InputState>,
    options: FindOptions,
    status: FindStatus,
    /// The host is still reading what it searches: the count says so.
    searching: bool,
    /// The host edits text: the replace row can show and Alt+Enter selects
    /// every match.
    editing: bool,
    /// The replace row shows.
    replacing: bool,
    _input_events: [Subscription; 2],
}

impl std::fmt::Debug for FindBar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FindBar")
            .field("options", &self.options)
            .field("status", &self.status)
            .field("editing", &self.editing)
            .field("replacing", &self.replacing)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<FindBarEvent> for FindBar {}

impl FindBar {
    /// The popover of a view that only reads (find, no replace).
    pub fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        let replace = cx.new(|cx| InputState::new(window, cx));
        let find_events =
            cx.subscribe_in(
                &input,
                window,
                |_, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => cx.emit(FindBarEvent::Changed),
                    InputEvent::PressEnter { shift: false, .. } => cx.emit(FindBarEvent::Next),
                    InputEvent::PressEnter { shift: true, .. } => cx.emit(FindBarEvent::Previous),
                    InputEvent::Focus | InputEvent::Blur => cx.notify(),
                },
            );
        let replace_events = cx.subscribe_in(
            &replace,
            window,
            |_, _, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {}
                InputEvent::PressEnter { .. } => cx.emit(FindBarEvent::Replace),
                InputEvent::Focus | InputEvent::Blur => cx.notify(),
            },
        );
        Self {
            input,
            replace,
            options: FindOptions::default(),
            status: FindStatus::Empty,
            searching: false,
            editing: false,
            replacing: false,
            _input_events: [find_events, replace_events],
        }
    }

    /// The popover of the Source view, which can replace.
    pub fn for_editor(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        Self {
            editing: true,
            ..Self::new(window, cx)
        }
    }

    /// Whether the replace row shows.
    pub fn is_replacing(&self) -> bool {
        self.replacing
    }

    /// Shows or hides the replace row (never shown in a view that only
    /// reads).
    pub fn set_replacing(&mut self, replacing: bool, cx: &mut Context<'_, Self>) {
        let replacing = replacing && self.editing;
        if self.replacing != replacing {
            self.replacing = replacing;
            cx.notify();
        }
    }

    /// The replacement text.
    pub fn replacement(&self, cx: &App) -> String {
        self.replace.read(cx).value().to_string()
    }

    /// Replaces the text of the replace field.
    pub fn set_replacement(&mut self, text: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.replace
            .update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
        cx.notify();
    }

    /// Gives the keyboard to the replace field, with its text selected.
    pub fn focus_replace(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.replace.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
    }

    /// Whether the replace field has the keyboard.
    pub fn replace_focused(&self, window: &Window, cx: &App) -> bool {
        self.replace.read(cx).focus_handle(cx).is_focused(window)
    }

    /// Whether either field has the keyboard.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.field_focused(window, cx) || self.replace_focused(window, cx)
    }

    /// Whether there are matches to replace or select.
    fn has_matches(&self) -> bool {
        matches!(self.status, FindStatus::Matches { total, .. } if total > 0)
    }

    /// The text being searched for.
    pub fn query(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    pub fn options(&self) -> FindOptions {
        self.options
    }

    pub fn status(&self) -> FindStatus {
        self.status
    }

    /// Replaces the text of the field (no [`FindBarEvent::Changed`]).
    pub fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(query.to_owned(), window, cx)
        });
        cx.notify();
    }

    /// Gives the keyboard to the field, with its text selected.
    pub fn focus_field(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
    }

    /// Whether the field has the keyboard.
    pub fn field_focused(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    pub fn set_status(&mut self, status: FindStatus, cx: &mut Context<'_, Self>) {
        if self.status != status {
            self.status = status;
            cx.notify();
        }
    }

    /// Whether the host is still reading what it searches (the PDF
    /// viewer's pages): the match count adds " (searching…)".
    pub fn set_searching(&mut self, searching: bool, cx: &mut Context<'_, Self>) {
        if self.searching != searching {
            self.searching = searching;
            cx.notify();
        }
    }

    /// The match count as shown and announced: the status's text, with
    /// " (searching…)" while the host is still reading.
    pub fn count_text(&self) -> Option<String> {
        let text = self.status.text()?;
        Some(
            if self.searching && matches!(self.status, FindStatus::Matches { .. }) {
                format!("{text} (searching\u{2026})")
            } else {
                text
            },
        )
    }

    /// Turns an option on or off, as its toggle does.
    pub fn toggle(&mut self, option: FindToggle, cx: &mut Context<'_, Self>) {
        let o = &mut self.options;
        match option {
            FindToggle::MatchCase => o.case_sensitive = !o.case_sensitive,
            FindToggle::WholeWord => o.whole_word = !o.whole_word,
            FindToggle::Regex => o.regex = !o.regex,
        }
        cx.emit(FindBarEvent::Changed);
        cx.notify();
    }
}

impl Focusable for FindBar {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

/// The toggles of the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindToggle {
    MatchCase,
    WholeWord,
    Regex,
}

impl FindToggle {
    const ALL: [Self; 3] = [Self::MatchCase, Self::WholeWord, Self::Regex];

    fn label(self) -> &'static str {
        match self {
            Self::MatchCase => "Aa",
            Self::WholeWord => "ab",
            Self::Regex => ".*",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::MatchCase => "find-match-case",
            Self::WholeWord => "find-whole-word",
            Self::Regex => "find-regex",
        }
    }

    fn is_on(self, o: FindOptions) -> bool {
        match self {
            Self::MatchCase => o.case_sensitive,
            Self::WholeWord => o.whole_word,
            Self::Regex => o.regex,
        }
    }
}

/// `shadow.popover`, the application's only shadow.
fn popover_shadow(theme: &Theme) -> Vec<BoxShadow> {
    theme
        .shadow
        .popover
        .iter()
        .map(|l| BoxShadow {
            color: l.color.into(),
            offset: point(px(l.x), px(l.y)),
            blur_radius: px(l.blur),
            spread_radius: px(l.spread),
            inset: false,
        })
        .collect()
}

/// The focus halo: 2 px of `accent.tint` around the border.
fn focus_halo(theme: &Theme) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: theme.accent.tint.into(),
        offset: point(px(0.0), px(0.0)),
        blur_radius: px(0.0),
        spread_radius: px(FOCUS_HALO),
        inset: false,
    }]
}

/// A 22 × 22 icon button: `text.secondary`, the hover overlay on hover.
fn icon_button(
    id: &'static str,
    icon: Icon,
    icon_size: f32,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let (hover, press) = (theme.state.hover, theme.state.press);
    let label = match id {
        "find-previous" => "Previous match",
        "find-next" => "Next match",
        _ => "Close find",
    };
    div()
        .id(id)
        .a11y_button(label)
        .keyboard_control(theme)
        .debug_selector(move || id.into())
        .flex_none()
        .size(px(SMALL_ICON_BUTTON))
        .rounded(px(radius::CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.text.secondary)
        .hover(move |s| s.bg(hover))
        .active(move |s| s.bg(press))
        .child(icon.sized(icon_size))
}

impl FindBar {
    fn toggle_button(
        &self,
        t: FindToggle,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let on = t.is_on(self.options);
        let hover = theme.state.hover;
        let id = t.id();
        let name = match t {
            FindToggle::MatchCase => "Match case",
            FindToggle::WholeWord => "Whole word",
            FindToggle::Regex => "Regular expression",
        };
        div()
            .id(ElementId::from(SharedString::from(id)))
            .a11y_check_box(name, on)
            .keyboard_control(theme)
            .debug_selector(move || id.into())
            .flex_none()
            .w(px(size::TOGGLE_WIDTH))
            .h(px(size::TOGGLE_HEIGHT))
            .rounded(px(radius::SMALL_CONTROL))
            .flex()
            .items_center()
            .justify_center()
            .font_family(font::EDITOR)
            .text_size(px(text::field_toggle_size()))
            .when(t == FindToggle::WholeWord, |d| d.underline())
            .map(|d| {
                if on {
                    d.bg(theme.accent.tint).text_color(theme.text.primary)
                } else {
                    d.text_color(theme.text.secondary)
                        .hover(move |s| s.bg(hover))
                }
            })
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(t, cx)))
            .child(t.label())
    }
}

/// A field: `surface.page`, a 1 px `border.strong` border, `accent.default`
/// with the focus halo while it has the keyboard.
fn field(
    input: &Entity<InputState>,
    label: &'static str,
    focused: bool,
    (pad_left, pad_right): (f32, f32),
    theme: &Theme,
) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .h(px(CONTROL))
        .flex()
        .items_center()
        .gap(px(spacing::FIELD_GAP))
        .pl(px(pad_left))
        .pr(px(pad_right))
        .rounded(px(radius::CONTROL))
        .bg(theme.surface.page)
        .border(px(HAIRLINE))
        .border_color(if focused {
            theme.accent.default
        } else {
            theme.border.strong
        })
        .when(focused, |d| d.shadow(focus_halo(theme)))
        .child(
            div().flex_1().min_w_0().h_full().child(
                Input::new(input)
                    .aria_label(label)
                    .appearance(false)
                    .h_full()
                    .px_0()
                    .py_0()
                    .text_size(px(text::UI.size))
                    .text_color(theme.text.primary),
            ),
        )
}

/// A secondary button 24 high: 1 px `border.strong`, `surface.raised`,
/// 12/500; half opacity and inert while `enabled` is false.
fn text_button(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let (hover, press) = (theme.state.hover, theme.state.press);
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .h(px(CONTROL))
        .px(px(spacing::BUTTON_PAD_X))
        .flex()
        .items_center()
        .rounded(px(radius::CONTROL))
        .border(px(HAIRLINE))
        .border_color(theme.border.strong)
        .bg(theme.surface.raised)
        .text_color(theme.text.primary)
        .font_weight(FontWeight(text::weight::ACTIVE))
        .map(|d| {
            if enabled {
                d.hover(move |s| s.bg(hover)).active(move |s| s.bg(press))
            } else {
                d.opacity(DISABLED_OPACITY)
            }
        })
        .child(label)
}

impl Render for FindBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let focused = self.field_focused(window, cx);
        let find_field = field(
            &self.input,
            "Find",
            focused,
            (spacing::FIELD_PAD_LEFT, spacing::FIELD_PAD_RIGHT),
            theme,
        )
        .children(
            FindToggle::ALL
                .into_iter()
                .map(|t| self.toggle_button(t, theme, cx).into_any_element()),
        );
        let count_text = self.count_text();
        let count = div()
            .id("find-count")
            .role(Role::Status)
            .aria_label("Matches")
            .when_some(count_text.clone(), |d, t| d.aria_value(t))
            .accessible()
            .live(Live::Polite)
            .debug_selector(|| "find-count".into())
            .flex_none()
            .min_w(px(size::COUNT_MIN_WIDTH))
            .flex()
            .justify_center()
            .text_size(px(text::UI_SMALL.size))
            .line_height(px(text::UI_SMALL.line))
            .font_weight(FontWeight(text::weight::REGULAR))
            .text_color(if self.status == FindStatus::Invalid {
                theme.status.error
            } else {
                theme.text.secondary
            })
            .children(count_text);
        let row = div()
            .h(px(CONTROL))
            .flex()
            .items_center()
            .gap(px(spacing::GAP))
            .child(find_field)
            .child(count)
            .child(
                icon_button("find-previous", Icon::FindPrevious, icon::INLINE, theme)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FindBarEvent::Previous))),
            )
            .child(
                icon_button("find-next", Icon::FindNext, icon::INLINE, theme)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FindBarEvent::Next))),
            )
            .child(
                icon_button("find-close", Icon::CloseFind, icon::SMALL, theme)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FindBarEvent::Close))),
            );
        let replace_row = self.replacing.then(|| {
            let enabled = self.has_matches();
            let focused = self.replace_focused(window, cx);
            let pad = spacing::REPLACE_FIELD_PAD_X;
            div()
                .debug_selector(|| "find-replace-row".into())
                .h(px(CONTROL))
                .flex()
                .items_center()
                .gap(px(spacing::REPLACE_GAP))
                .child(field(
                    &self.replace,
                    "Replace with",
                    focused,
                    (pad, pad),
                    theme,
                ))
                .child(
                    text_button("find-replace", "Replace", enabled, theme).when(enabled, |b| {
                        b.on_click(cx.listener(|_, _, _, cx| cx.emit(FindBarEvent::Replace)))
                    }),
                )
                .child(
                    text_button("find-replace-all", "Replace all", enabled, theme).when(
                        enabled,
                        |b| {
                            b.on_click(cx.listener(|_, _, _, cx| cx.emit(FindBarEvent::ReplaceAll)))
                        },
                    ),
                )
        });
        div()
            .id("find-bar")
            .debug_selector(|| "find-bar".into())
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &SelectAllMatches, _, cx| {
                if this.editing {
                    cx.emit(FindBarEvent::SelectAll);
                } else {
                    // Alt+Enter keeps its meaning in a view that only reads.
                    cx.propagate();
                }
            }))
            .occlude()
            .w(px(size::WIDTH))
            .p(px(spacing::PAD))
            .flex()
            .flex_col()
            .gap(px(spacing::ROW_GAP))
            .rounded(px(radius::POPOVER))
            .bg(theme.surface.raised)
            .border(px(HAIRLINE))
            .border_color(theme.border.strong)
            .shadow(popover_shadow(theme))
            .text_color(theme.text.primary)
            .text_size(px(text::UI.size))
            .line_height(px(text::UI.line))
            .child(row)
            .children(replace_row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_count_reads_like_the_design() {
        let m = |current, total, more| FindStatus::Matches {
            current,
            total,
            more,
        };
        assert_eq!(FindStatus::Empty.text(), None);
        assert_eq!(FindStatus::Invalid.text().as_deref(), Some("Invalid"));
        assert_eq!(m(None, 0, false).text().as_deref(), Some("No results"));
        assert_eq!(m(Some(2), 17, false).text().as_deref(), Some("3 of 17"));
        assert_eq!(
            m(Some(0), 10_000, true).text().as_deref(),
            Some("1 of 10,000+")
        );
        assert_eq!(
            m(Some(1233), 1_234_567, false).text().as_deref(),
            Some("1,234 of 1,234,567")
        );
        assert_eq!(m(None, 5, false).text().as_deref(), Some("5"));
    }

    #[test]
    fn numbers_are_grouped_in_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(100_000), "100,000");
    }
}
