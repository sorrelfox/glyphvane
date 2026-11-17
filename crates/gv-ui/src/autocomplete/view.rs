// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The popover: rows, footer and placement (SPEC § Popovers).

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Bounds, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    Pixels, Point, SharedString, Size, Styled as _, Window, div, point, px, size,
};

use super::Open;
use super::context::Kind;
use super::items::ItemIcon;
use crate::icons::Icon;
use crate::metrics::size::{AUTOCOMPLETE_WIDTH, MENU_ROW};
use crate::metrics::spacing::menu::EDGE;
use crate::metrics::{HAIRLINE, icon, radius, text};
use crate::switcher::{hint, label_text, popover_shadow};
use crate::theme::Theme;

/// Rows shown at once; the list scrolls with the highlight (not designed).
pub(crate) const VISIBLE_ROWS: usize = 8;
/// The list's padding.
const LIST_PAD: f32 = 4.0;
/// A row's padding `0 8`, gap 7, radius 5.
const ROW_PAD_X: f32 = 8.0;
const ROW_GAP: f32 = 7.0;
const ROW_RADIUS: f32 = 5.0;
/// The footer's padding `5 12 6` and gap 12.
const FOOTER_PAD_TOP: f32 = 5.0;
const FOOTER_PAD_BOTTOM: f32 = 6.0;
const FOOTER_PAD_X: f32 = 12.0;
const FOOTER_GAP: f32 = 12.0;
/// Between the caret's row and the popover (not designed).
const CARET_GAP: f32 = 2.0;
/// From the popover's left edge to its labels: the border, the list's
/// padding, the row's padding, the icon and the gap after it. The labels
/// line up with the typed text.
pub(crate) const LABEL_INSET: f32 = HAIRLINE + LIST_PAD + ROW_PAD_X + icon::ROW + ROW_GAP;

/// The popover's size for `rows` rows.
pub(crate) fn panel_size(rows: usize) -> Size<Pixels> {
    let rows = rows.min(VISIBLE_ROWS) as f32;
    let footer = FOOTER_PAD_TOP + text::UI_SMALL.line + FOOTER_PAD_BOTTOM + HAIRLINE;
    size(
        px(AUTOCOMPLETE_WIDTH),
        px(2.0 * HAIRLINE + 2.0 * LIST_PAD + rows * MENU_ROW + footer),
    )
}

/// Where the popover of `size` goes for the typed text starting at
/// `anchor` (its row's bounds) in a window of `viewport`: labels over the
/// text, below the row, or above it when there is no room below and more
/// room above; inside the window sideways.
pub(crate) fn place(
    anchor: Bounds<Pixels>,
    size: Size<Pixels>,
    viewport: Size<Pixels>,
) -> Point<Pixels> {
    let (w, h) = (f32::from(size.width), f32::from(size.height));
    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
    let below = f32::from(anchor.bottom()) + CARET_GAP;
    let above = f32::from(anchor.top()) - CARET_GAP - h;
    let fits_below = below + h <= vh - EDGE;
    let y = if fits_below || vh - below >= f32::from(anchor.top()) {
        below
    } else {
        above.max(EDGE)
    };
    let x = (f32::from(anchor.left()) - LABEL_INSET)
        .min(vw - w - EDGE)
        .max(EDGE);
    point(px(x), px(y))
}

fn icon_of(icon: ItemIcon) -> Result<Icon, &'static str> {
    Ok(match icon {
        ItemIcon::Note => Icon::Note,
        ItemIcon::Alias => Icon::SwitcherAlias,
        ItemIcon::Image => Icon::ImageFile,
        ItemIcon::Canvas => Icon::Canvas,
        ItemIcon::Base => Icon::Base,
        ItemIcon::OtherFile => Icon::OtherFile,
        ItemIcon::Heading => Icon::SwitcherHeading,
        ItemIcon::Block => Icon::SwitcherBlock,
        ItemIcon::Tag => Icon::Tags,
        ItemIcon::Emoji(glyph) => return Err(glyph),
        ItemIcon::Callout => Icon::NoticeInformation,
        ItemIcon::Language => Icon::CopyCode,
        ItemIcon::Footnote => Icon::MoreActions,
        ItemIcon::Property => Icon::Profile,
        ItemIcon::Create => Icon::NotCreated,
    })
}

/// Accepts row `n`.
pub(crate) type RowClick = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// The popover for `open`.
pub(crate) fn panel(
    open: &Open,
    font: SharedString,
    theme: &Theme,
    click: &RowClick,
) -> AnyElement {
    let hover = theme.state.hover;
    let last = (open.first + VISIBLE_ROWS).min(open.items.len());
    let rows = (open.first..last).map(|n| {
        let item = &open.items[n];
        let selected = n == open.selected;
        let click = click.clone();
        let icon = match icon_of(item.icon) {
            Ok(icon) => div()
                .flex_none()
                .text_color(theme.text.secondary)
                .child(icon.sized(icon::ROW)),
            Err(glyph) => div()
                .flex_none()
                .w(px(icon::ROW))
                .text_size(px(icon::ROW))
                .child(glyph),
        };
        let selector = format!("autocomplete-row:{}", item.label);
        div()
            .id(("autocomplete-row", n))
            .debug_selector(move || selector.clone())
            .w_full()
            .h(px(MENU_ROW))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(ROW_GAP))
            .px(px(ROW_PAD_X))
            .rounded(px(ROW_RADIUS))
            .map(|row| {
                if selected {
                    row.bg(theme.accent.tint)
                } else {
                    row.hover(move |s| s.bg(hover))
                }
            })
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                window.prevent_default();
                click(n, window, cx);
            })
            .child(icon)
            .child(div().flex_1().min_w_0().truncate().child(label_text(
                &item.label,
                &item.matched,
                &item.suffix,
                theme,
            )))
            .when(!item.detail.is_empty(), |row| {
                row.child(
                    div()
                        .flex_none()
                        .max_w(px(AUTOCOMPLETE_WIDTH / 3.0))
                        .truncate()
                        .text_size(px(text::UI_SMALL.size))
                        .text_color(theme.text.secondary)
                        .child(item.detail.clone()),
                )
            })
    });
    let footer = div()
        .flex()
        .items_center()
        .gap(px(FOOTER_GAP))
        .pt(px(FOOTER_PAD_TOP))
        .pb(px(FOOTER_PAD_BOTTOM))
        .px(px(FOOTER_PAD_X))
        .border_t(px(HAIRLINE))
        .border_color(theme.border.default)
        .text_size(px(text::UI_SMALL.size))
        .line_height(px(text::UI_SMALL.line))
        .text_color(theme.text.secondary)
        .child(hint("↩", "insert".to_owned(), theme))
        .map(|footer| match open.trigger.kind {
            Kind::WikiFile => footer
                .child(hint("#", "headings".to_owned(), theme))
                .child(hint("^", "blocks".to_owned(), theme)),
            _ => footer.child(hint("Esc", "close".to_owned(), theme)),
        });
    div()
        .id("autocomplete")
        .debug_selector(|| "autocomplete".into())
        .occlude()
        .w(px(AUTOCOMPLETE_WIDTH))
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(radius::POPOVER))
        .bg(theme.surface.raised)
        .border(px(HAIRLINE))
        .border_color(theme.border.strong)
        .shadow(popover_shadow(theme))
        .font_family(font)
        .text_color(theme.text.primary)
        .text_size(px(text::UI.size))
        .line_height(px(text::UI.line))
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            cx.stop_propagation();
            window.prevent_default();
        })
        .child(div().flex().flex_col().p(px(LIST_PAD)).children(rows))
        .child(footer)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(x: f32, y: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(10.0), px(20.0)))
    }

    #[test]
    fn below_the_row_with_the_labels_over_the_text() {
        let viewport = size(px(1000.0), px(800.0));
        let s = panel_size(5);
        let at = place(anchor(200.0, 100.0), s, viewport);
        assert_eq!(at, point(px(200.0 - LABEL_INSET), px(122.0)));
    }

    #[test]
    fn flips_above_at_the_bottom_and_stays_inside_sideways() {
        let viewport = size(px(1000.0), px(800.0));
        let s = panel_size(8);
        let at = place(anchor(990.0, 760.0), s, viewport);
        assert_eq!(f32::from(at.y), 760.0 - CARET_GAP - f32::from(s.height));
        assert_eq!(f32::from(at.x), 1000.0 - AUTOCOMPLETE_WIDTH - EDGE);
        let at = place(anchor(5.0, 10.0), s, viewport);
        assert_eq!(f32::from(at.x), EDGE);
        assert_eq!(f32::from(at.y), 32.0);
    }
}
