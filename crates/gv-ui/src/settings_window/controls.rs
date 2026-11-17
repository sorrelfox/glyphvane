// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The settings window's controls, drawn from the design's control specs
//! (SPEC § Controls): switch, segmented control, select, number and text
//! fields, stepper, and the rows they sit in. Every colour is a theme role.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, FontWeight, InteractiveElement as _,
    IntoElement as _, ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};

use super::metrics::{field, row, select, stepper};
use crate::a11y::{A11yExt as _, AccessibleExt as _, Role, Toggled};
use crate::metrics::size::{CONTROL, segment, settings_row, switch};
use crate::metrics::spacing::mode_switch;
use crate::metrics::{DISABLED_OPACITY, FOCUS_HALO, HAIRLINE, font, radius, text};
use crate::theme::Theme;

/// A click handler of a control.
pub type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A settings row: the label and an optional description at the left, the
/// controls at the right, a hairline above. At least 36 high, 40 with a
/// description.
pub fn row(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    description: Option<SharedString>,
    error: Option<SharedString>,
    controls: Vec<AnyElement>,
    theme: &Theme,
) -> Stateful<Div> {
    let has_description = description.is_some() || error.is_some();
    let label: SharedString = label.into();
    // The row names the controls in it for assistive technology.
    let about = match (&error, &description) {
        (Some(error), _) => Some(error.clone()),
        (None, Some(description)) => Some(description.clone()),
        (None, None) => None,
    };
    div()
        .id(id)
        .role(Role::Group)
        .aria_label(label.clone())
        .when_some(about, |d, about| d.aria_description(about))
        .min_h(px(if has_description {
            settings_row::WITH_DESCRIPTION
        } else {
            settings_row::PLAIN
        }))
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(row::GAP_X))
        .gap_y(px(row::GAP_Y))
        .py(px(row::PAD_Y))
        .border_t(px(HAIRLINE))
        .border_color(theme.border.default)
        .child(
            div()
                .flex_1()
                .flex_basis(px(row::LABEL_BASIS))
                .min_w_0()
                .child(
                    div()
                        .font_weight(FontWeight(text::weight::ACTIVE))
                        .child(label),
                )
                .when_some(description, |d, description| {
                    d.child(
                        div()
                            .text_size(px(text::UI_SMALL.size))
                            .line_height(px(text::UI_SMALL.line))
                            .text_color(theme.text.secondary)
                            .child(description),
                    )
                })
                .when_some(error, |d, error| {
                    d.child(
                        div()
                            .text_size(px(text::UI_SMALL.size))
                            .line_height(px(text::UI_SMALL.line))
                            .text_color(theme.status.error)
                            .child(error),
                    )
                }),
        )
        .children(controls)
}

/// A group heading: 11.5/600 `text.secondary`, 24 high, 10 above (none
/// above the first group).
pub fn heading(title: impl Into<SharedString>, first: bool, theme: &Theme) -> Div {
    div()
        .h(px(row::HEADING))
        .when(!first, |d| d.mt(px(row::HEADING_GAP)))
        .text_size(px(text::UI_SECTION.size))
        .line_height(px(row::HEADING))
        .font_weight(FontWeight(text::weight::STRONG))
        .text_color(theme.text.secondary)
        .child(title.into())
}

/// A switch, 30 × 18 with a 14 knob. On: `accent.default` track and
/// `accent.on` knob. Off (not designed): `border.strong` track and a
/// `surface.raised` knob.
pub fn switch(
    id: impl Into<ElementId>,
    on: bool,
    enabled: bool,
    on_click: OnClick,
    theme: &Theme,
) -> Stateful<Div> {
    let pad = (switch::HEIGHT - switch::KNOB) / 2.0;
    div()
        .id(id)
        .role(Role::Switch)
        .aria_toggled(if on { Toggled::True } else { Toggled::False })
        .when(enabled, |d| d.keyboard_control(theme))
        .when(!enabled, |d| d.aria_description("Unavailable"))
        .flex_none()
        .w(px(switch::WIDTH))
        .h(px(switch::HEIGHT))
        .p(px(pad))
        .rounded(px(switch::HEIGHT / 2.0))
        .flex()
        .when(on, |d| d.justify_end().bg(theme.accent.default))
        .when(!on, |d| d.justify_start().bg(theme.border.strong))
        .child(div().size(px(switch::KNOB)).rounded_full().bg(if on {
            theme.accent.on
        } else {
            theme.surface.raised
        }))
        .when(enabled, |d| d.on_click(on_click))
        .when(!enabled, |d| d.opacity(DISABLED_OPACITY))
}

/// One segment of a segmented control.
pub struct Segment {
    /// The element id, also the test selector.
    pub id: SharedString,
    pub label: SharedString,
    pub active: bool,
    pub on_click: Option<OnClick>,
}

/// [`segmented`] as a radio group named `label` (usually the row's label),
/// so assistive technology hears the choices as one set: "Light, radio
/// button, 1 of 2" inside "In light appearance, use, radio group".
pub fn radio_group(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    segments: Vec<Segment>,
    enabled: bool,
    theme: &Theme,
) -> AnyElement {
    segmented(segments, enabled, theme)
        .id(id)
        .a11y_group(Role::RadioGroup, label)
        .accessible()
        .into_any_element()
}

/// A segmented control: a 24 frame with 20 high segments, the active one
/// on `surface.page` with a `border.strong` ring, as the mode switch.
pub fn segmented(segments: Vec<Segment>, enabled: bool, theme: &Theme) -> Div {
    div()
        .h(px(segment::FRAME))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(mode_switch::FRAME_GAP))
        .p(px(mode_switch::FRAME_PAD))
        .border_1()
        .border_color(theme.border.strong)
        .rounded(px(radius::SEGMENT_FRAME))
        .when(!enabled, |d| d.opacity(DISABLED_OPACITY))
        .children(segments.into_iter().map(|segment| {
            let hover = theme.state.hover;
            let selector = segment.id.clone();
            div()
                .id(segment.id)
                .role(Role::RadioButton)
                .aria_label(segment.label.clone())
                .aria_toggled(if segment.active {
                    Toggled::True
                } else {
                    Toggled::False
                })
                .when(enabled && segment.on_click.is_some(), |seg| {
                    seg.keyboard_control(theme)
                })
                .debug_selector(move || selector.to_string())
                .h(px(segment::INNER))
                .px(px(row::SEGMENT_PAD_X - HAIRLINE))
                .flex()
                .items_center()
                .rounded(px(radius::SEGMENT))
                .border_1()
                .map(|seg| {
                    if segment.active {
                        seg.bg(theme.surface.page)
                            .border_color(theme.border.strong)
                            .text_color(theme.text.primary)
                            .font_weight(FontWeight(text::weight::ACTIVE))
                    } else {
                        seg.border_color(gpui::transparent_black())
                            .text_color(theme.text.secondary)
                            .when(enabled, |s| s.hover(move |style| style.bg(hover)))
                    }
                })
                .when_some(segment.on_click.filter(|_| enabled), |seg, f| {
                    seg.on_click(f)
                })
                .child(segment.label)
        }))
}

/// A select: a secondary button with the value at the left and
/// `chevrons-up-down` at the right, at least 168 wide.
pub fn select_button(
    id: impl Into<ElementId>,
    value: impl Into<SharedString>,
    enabled: bool,
    on_click: OnClick,
    theme: &Theme,
) -> Stateful<Div> {
    let (hover, press) = (theme.state.hover, theme.state.press);
    let value: SharedString = value.into();
    div()
        .id(id)
        .role(Role::ComboBox)
        .aria_value(value.clone())
        .when(enabled, |d| d.keyboard_control(theme))
        .h(px(CONTROL))
        .flex_none()
        .min_w(px(select::MIN_WIDTH))
        .pl(px(select::PAD_LEFT))
        .pr(px(select::PAD_RIGHT))
        .rounded(px(radius::CONTROL))
        .border_1()
        .border_color(theme.border.strong)
        .bg(theme.surface.raised)
        .flex()
        .items_center()
        .gap(px(select::GAP))
        .child(div().flex_1().min_w_0().truncate().child(value))
        .child(div().text_color(theme.text.secondary).child(super::icon(
            gpui::assets::IconName::ChevronsUpDown,
            select::ICON,
        )))
        .when(enabled, |d| {
            d.hover(move |s| s.bg(hover))
                .active(move |s| s.bg(press))
                .on_click(on_click)
        })
        .when(!enabled, |d| d.opacity(DISABLED_OPACITY))
}

/// The list of a select, under its button: `surface.raised`, a
/// `border.strong` hairline and the popover shadow, rows 24 high, the
/// chosen one in weight 500 on `accent.tint` (not designed). Each option
/// carries its test selector.
pub fn select_list(
    id: impl Into<ElementId>,
    options: Vec<(SharedString, String, bool, OnClick)>,
    width: f32,
    theme: &Theme,
) -> Stateful<Div> {
    let hover = theme.state.hover;
    let count = options.len();
    div()
        .id(id)
        .role(Role::ListBox)
        .occlude()
        .w(px(width))
        .max_h(px(select::LIST_MAX_HEIGHT))
        .overflow_y_scroll()
        .p(px(select::LIST_PAD))
        .rounded(px(radius::POPOVER))
        .border_1()
        .border_color(theme.border.strong)
        .bg(theme.surface.raised)
        .shadow(popover_shadow(theme))
        .children(options.into_iter().enumerate().map(
            |(index, (label, selector, chosen, on_click))| {
                div()
                    .id(("option", index))
                    .a11y_option(label.clone(), chosen, index, count)
                    .keyboard_control(theme)
                    .debug_selector(move || selector)
                    .h(px(crate::metrics::size::MENU_ROW))
                    .px(px(select::PAD_LEFT))
                    .flex()
                    .items_center()
                    .rounded(px(radius::CONTROL))
                    .truncate()
                    .when(chosen, |d| {
                        d.bg(theme.accent.tint)
                            .font_weight(FontWeight(text::weight::ACTIVE))
                    })
                    .hover(move |s| s.bg(hover))
                    .on_click(on_click)
                    .child(label)
            },
        ))
}

/// The frame of a text or number field: 24 high, radius 5, a
/// `border.strong` hairline on `surface.raised`; focused, an
/// `accent.default` border and a 2 px `accent.tint` halo. An error draws
/// the border in `status.error` (not designed).
pub fn field_frame(focused: bool, error: bool, width: Option<f32>, theme: &Theme) -> Div {
    let border = if error {
        theme.status.error
    } else if focused {
        theme.accent.default
    } else {
        theme.border.strong
    };
    div()
        .h(px(CONTROL))
        .flex_none()
        .map(|d| match width {
            Some(w) => d.w(px(w)),
            None => d.min_w(px(select::MIN_WIDTH)),
        })
        .px(px(field::PAD_X))
        .flex()
        .items_center()
        .gap(px(field::UNIT_GAP))
        .rounded(px(radius::CONTROL))
        .border_1()
        .border_color(border)
        .bg(theme.surface.raised)
        .when(focused, |d| {
            d.shadow(vec![gpui::BoxShadow {
                color: theme.accent.tint.into(),
                offset: gpui::point(px(0.0), px(0.0)),
                blur_radius: px(0.0),
                spread_radius: px(FOCUS_HALO),
                inset: false,
            }])
        })
}

/// A unit after a number field's value: 11 px `text.secondary`.
pub fn unit(text_: &'static str, theme: &Theme) -> Div {
    div()
        .flex_none()
        .text_size(px(text::UI_SMALL.size))
        .text_color(theme.text.secondary)
        .child(text_)
}

/// A stepper: minus, the value in mono 11.5, plus; 24 high, cells split by
/// `border.strong` hairlines.
pub fn stepper(
    id: &'static str,
    value: impl Into<SharedString>,
    on_minus: Option<OnClick>,
    on_plus: Option<OnClick>,
    theme: &Theme,
) -> Div {
    let cell = |id: (&'static str, &'static str),
                icon: gpui::assets::IconName,
                handler: Option<OnClick>| {
        let (hover, press) = (theme.state.hover, theme.state.press);
        div()
            .id(ElementId::NamedChild(
                std::sync::Arc::new(id.0.into()),
                id.1.into(),
            ))
            .a11y_button(if id.1 == "minus" {
                "Decrease"
            } else {
                "Increase"
            })
            .when(handler.is_some(), |d| d.keyboard_control(theme))
            .debug_selector(move || format!("{}-{}", id.0, id.1))
            .w(px(stepper::BUTTON))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .child(super::icon(icon, stepper::ICON))
            .map(|d| match handler {
                Some(handler) => d
                    .hover(move |s| s.bg(hover))
                    .active(move |s| s.bg(press))
                    .on_click(handler),
                None => d.opacity(DISABLED_OPACITY),
            })
    };
    div()
        .h(px(CONTROL))
        .flex_none()
        .flex()
        .items_stretch()
        .rounded(px(radius::CONTROL))
        .border_1()
        .border_color(theme.border.strong)
        .bg(theme.surface.raised)
        .overflow_hidden()
        .child(cell((id, "minus"), gpui::assets::IconName::Minus, on_minus))
        .child(
            div()
                .w(px(stepper::VALUE))
                .border_x(px(HAIRLINE))
                .border_color(theme.border.strong)
                .flex()
                .items_center()
                .justify_center()
                .font_family(font::EDITOR)
                .text_size(px(stepper::VALUE_TEXT))
                .child(value.into()),
        )
        .child(cell((id, "plus"), gpui::assets::IconName::Plus, on_plus))
}

/// A quiet text button (not designed): no border, the hover overlay, 11 px
/// `text.secondary`.
pub fn quiet_button(
    id: impl Into<ElementId>,
    label: &'static str,
    on_click: OnClick,
    theme: &Theme,
) -> Stateful<Div> {
    let (hover, press) = (theme.state.hover, theme.state.press);
    div()
        .id(id)
        .a11y_button(label)
        .keyboard_control(theme)
        .flex_none()
        .h(px(CONTROL))
        .px(px(field::PAD_X))
        .flex()
        .items_center()
        .rounded(px(radius::CONTROL))
        .text_size(px(text::UI_SMALL.size))
        .text_color(theme.text.secondary)
        .hover(move |s| s.bg(hover))
        .active(move |s| s.bg(press))
        .on_click(on_click)
        .child(label)
}

/// `shadow.popover`, the application's only shadow.
fn popover_shadow(theme: &Theme) -> Vec<gpui::BoxShadow> {
    theme
        .shadow
        .popover
        .iter()
        .map(|l| gpui::BoxShadow {
            color: l.color.into(),
            offset: gpui::point(px(l.x), px(l.y)),
            blur_radius: px(l.blur),
            spread_radius: px(l.spread),
            inset: false,
        })
        .collect()
}
