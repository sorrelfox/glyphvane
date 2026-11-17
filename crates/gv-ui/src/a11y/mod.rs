// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Accessibility (NFR-A11Y-01 to -05): what assistive technology is told
//! about the controls, and how the keyboard reaches them.
//!
//! GPUI reports an element to AccessKit when it has an id and a role; its
//! name, states and actions come from the `aria_*` builders. This module
//! adds:
//!
//! - [`A11yExt`]: one call per kind of control (button, toggle, tab, list
//!   option, tree item…) so the same control gets the same role, name and
//!   state everywhere;
//! - [`A11yExt::keyboard_control`]: a Tab stop that Enter and Space click
//!   (GPUI's keyboard click), that a mouse press does not take the
//!   keyboard from the document, and that shows the focus ring
//!   (`accent.default` border, `accent.tint` fill) while focused from the
//!   keyboard. Tab and Shift+Tab are GPUI Base's root keys; F6 cycles the
//!   panes ([`crate::shell`]);
//! - [`A11yExt::roving_tab`] and [`RovingTabs`]: the tab-list pattern.
//!   One tab of a list is a Tab stop (the focused one, else the selected
//!   one); Left and Right (Up and Down in a vertical list) move between
//!   the tabs, wrapping round, and Home and End go to the first and the
//!   last. Each list says whether moving also shows the tab (automatic
//!   activation) or waits for Enter or Space (manual);
//! - [`Accessible`]: a thin wrapper for the states the `aria_*` builders do
//!   not cover (live regions, disabled, modal); in tests it also records
//!   each wrapped element's role, name and states for headless queries
//!   ([`testing`]).
//!
//! The editor and the Reading view are custom elements; they describe
//! their content with synthetic nodes ([`crate::reader`],
//! [`crate::editor`]).

pub(crate) mod document;

use gpui::{
    AnyElement, App, Bounds, BoxShadow, Element, ElementId, FocusHandle, GlobalElementId,
    InspectorElementId, InteractiveElement, Interactivity, IntoElement, KeyDownEvent, LayoutId,
    MouseButton, ParentElement, Pixels, SharedString, StatefulInteractiveElement, StyleRefinement,
    Styled, Window, accesskit, point, px,
};

pub use gpui::accesskit::Live;
pub use gpui::{Role, Toggled};

use crate::metrics::{FOCUS_HALO, HAIRLINE};
use crate::theme::Theme;

/// Names, roles and states for the app's controls. Every method sets the
/// element's role, so it must have an id (`div().id(…)`).
pub trait A11yExt: StatefulInteractiveElement + Sized {
    /// A push button named `label` (for an icon button, what it does).
    fn a11y_button(self, label: impl Into<SharedString>) -> Self {
        self.role(Role::Button).aria_label(label)
    }

    /// A check box or switch.
    fn a11y_check_box(self, label: impl Into<SharedString>, checked: bool) -> Self {
        self.role(Role::CheckBox)
            .aria_label(label)
            .aria_toggled(toggled(checked))
    }

    /// Tab `index` (from 0) of `count` in a tab list.
    fn a11y_tab(
        self,
        label: impl Into<SharedString>,
        selected: bool,
        index: usize,
        count: usize,
    ) -> Self {
        self.role(Role::Tab)
            .aria_label(label)
            .aria_selected(selected)
            .aria_position_in_set(index + 1)
            .aria_size_of_set(count)
    }

    /// Option `index` (from 0) of `count` in a list box (the palette, the
    /// quick switcher, a picker).
    fn a11y_option(
        self,
        label: impl Into<SharedString>,
        selected: bool,
        index: usize,
        count: usize,
    ) -> Self {
        self.role(Role::ListBoxOption)
            .aria_label(label)
            .aria_selected(selected)
            .aria_position_in_set(index + 1)
            .aria_size_of_set(count)
    }

    /// A menu item; `checked` for an item with a check mark.
    fn a11y_menu_item(self, label: impl Into<SharedString>, checked: Option<bool>) -> Self {
        match checked {
            Some(on) => self
                .role(Role::MenuItemCheckBox)
                .aria_label(label)
                .aria_toggled(toggled(on)),
            None => self.role(Role::MenuItem).aria_label(label),
        }
    }

    /// A tree item at `level` (from 1). `expanded` is `None` for a leaf.
    fn a11y_tree_item(
        self,
        label: impl Into<SharedString>,
        level: usize,
        expanded: Option<bool>,
        selected: bool,
    ) -> Self {
        let item = self
            .role(Role::TreeItem)
            .aria_label(label)
            .aria_level(level.max(1))
            .aria_selected(selected);
        match expanded {
            Some(expanded) => item.aria_expanded(expanded),
            None => item,
        }
    }

    /// Item `index` (from 0) of `count` in a plain list (search results,
    /// backlinks, history, bookmarks).
    fn a11y_list_item(self, label: impl Into<SharedString>, index: usize, count: usize) -> Self {
        self.role(Role::ListItem)
            .aria_label(label)
            .aria_position_in_set(index + 1)
            .aria_size_of_set(count)
    }

    /// A container with `role` and a name (a panel, a toolbar, a list).
    fn a11y_group(self, role: Role, label: impl Into<SharedString>) -> Self {
        self.role(role).aria_label(label)
    }

    /// A Tab stop: Enter and Space click it, a mouse press leaves the
    /// keyboard where it was, and keyboard focus shows the focus ring.
    fn keyboard_control(self, theme: &Theme) -> Self {
        let ring = focus_ring(theme);
        let fill = theme.accent.tint;
        self.tab_index(0)
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .focus_visible(move |style| style.bg(fill).shadow(ring))
    }

    /// Tab `index` of the list whose focus handles are `tabs`, keyed as a
    /// tab list ([`tab_list_step`]): a Tab stop only when `stop` (see
    /// [`RovingTabs::stop`]), Enter and Space click it, the arrow keys,
    /// Home and End move the keyboard to another tab after calling
    /// `on_move` with its index (which shows that tab for automatic
    /// activation, or does nothing for manual). A mouse press leaves the
    /// keyboard where it was; keyboard focus shows the focus ring. Give
    /// the tab its role with [`A11yExt::a11y_tab`].
    fn roving_tab(
        self,
        theme: &Theme,
        tabs: &RovingTabs,
        index: usize,
        stop: bool,
        on_move: impl Fn(usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        let ring = focus_ring(theme);
        let fill = theme.accent.tint;
        let handles = tabs.handles.clone();
        let axis = tabs.axis;
        let el = match handles.get(index) {
            Some(handle) => self.track_focus(&handle.clone().tab_index(0).tab_stop(stop)),
            None => self.tab_index(0),
        };
        el.on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .focus_visible(move |style| style.bg(fill).shadow(ring))
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                let m = &event.keystroke.modifiers;
                if m.control || m.alt || m.platform || m.shift || m.function {
                    return;
                }
                let key = event.keystroke.key.as_str();
                let Some(to) = tab_list_step(key, axis, index, handles.len()) else {
                    return;
                };
                cx.stop_propagation();
                on_move(to, window, cx);
                if let Some(handle) = handles.get(to) {
                    handle.focus(window, cx);
                }
            })
    }
}

impl<E: StatefulInteractiveElement> A11yExt for E {}

/// Which arrow keys move along a tab list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Axis {
    /// Left and Right (a row of tabs).
    #[default]
    Horizontal,
    /// Up and Down (a column of tabs).
    Vertical,
}

/// The tab that `key` moves the keyboard to from tab `index` of `count`
/// (ARIA tab-list pattern): the next or previous one along `axis`,
/// wrapping round, or the first (Home) or the last (End). `None` for any
/// other key or an empty list.
pub fn tab_list_step(key: &str, axis: Axis, index: usize, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let (back, forth) = match axis {
        Axis::Horizontal => ("left", "right"),
        Axis::Vertical => ("up", "down"),
    };
    let index = index.min(count - 1);
    match key {
        k if k == back => Some((index + count - 1) % count),
        k if k == forth => Some((index + 1) % count),
        "home" => Some(0),
        "end" => Some(count - 1),
        _ => None,
    }
}

/// The focus handles of a tab list's tabs, kept by the view that draws
/// the list so the arrow keys can move the keyboard from tab to tab
/// ([`A11yExt::roving_tab`]).
#[derive(Debug, Clone, Default)]
pub struct RovingTabs {
    handles: Vec<FocusHandle>,
    axis: Axis,
}

impl RovingTabs {
    pub fn new(axis: Axis) -> Self {
        Self {
            handles: Vec::new(),
            axis,
        }
    }

    /// Makes one handle per tab (call before drawing the list).
    pub fn sync(&mut self, count: usize, cx: &mut App) {
        self.handles.truncate(count);
        while self.handles.len() < count {
            self.handles.push(cx.focus_handle());
        }
    }

    /// The tab that has the keyboard, itself or in a control inside it
    /// (a tab's close button).
    pub fn focused(&self, window: &Window, cx: &App) -> Option<usize> {
        self.handles
            .iter()
            .position(|h| h.contains_focused(window, cx))
    }

    /// Gives tab `index` the keyboard (nothing when it has no handle).
    pub fn focus(&self, index: usize, window: &mut Window, cx: &mut App) {
        if let Some(handle) = self.handles.get(index) {
            handle.focus(window, cx);
        }
    }

    /// The one tab that is a Tab stop: the focused one, else `selected`,
    /// else the first.
    pub fn stop(&self, window: &Window, cx: &App, selected: Option<usize>) -> usize {
        self.focused(window, cx)
            .or(selected.filter(|&s| s < self.handles.len()))
            .unwrap_or(0)
    }
}

/// The focus ring of a control focused from the keyboard, as text fields
/// draw it: a 1 px `accent.default` line around the control and a 2 px
/// `accent.tint` halo outside it. Drawn as shadows, so it takes no room
/// and moves nothing.
pub fn focus_ring(theme: &Theme) -> Vec<BoxShadow> {
    let ring = |color: crate::theme::Color, spread: f32| BoxShadow {
        color: color.into(),
        offset: point(px(0.0), px(0.0)),
        blur_radius: px(0.0),
        spread_radius: px(spread),
        inset: false,
    };
    vec![
        ring(theme.accent.tint, HAIRLINE + FOCUS_HALO),
        ring(theme.accent.default, HAIRLINE),
    ]
}

/// A live region with no size that says `text` politely: the option a
/// list popover has selected while its text field keeps the keyboard
/// (GPUI reports the active descendant only under a focused ancestor, and
/// the field is the rows' sibling).
pub fn announcer(
    id: &'static str,
    text: impl Into<SharedString>,
) -> Accessible<gpui::Stateful<gpui::Div>> {
    gpui::div()
        .id(id)
        .role(Role::Status)
        .aria_label(text)
        .absolute()
        .size_0()
        .overflow_hidden()
        .accessible()
        .live(Live::Polite)
}

/// What a list popover announces: the selected option and where it is,
/// or `empty`.
pub fn option_announcement(
    selected: Option<&str>,
    index: usize,
    count: usize,
    empty: &str,
) -> String {
    match selected {
        Some(name) if count > 0 => format!("{name}, {} of {count}", index + 1),
        _ => empty.to_owned(),
    }
}

fn toggled(on: bool) -> Toggled {
    if on { Toggled::True } else { Toggled::False }
}

/// Wraps an element to give it the accessibility states the `aria_*`
/// builders do not set.
pub trait AccessibleExt: Element + InteractiveElement + Sized {
    fn accessible(self) -> Accessible<Self> {
        Accessible {
            inner: self,
            extra: Extra::default(),
        }
    }
}

impl<E: Element + InteractiveElement> AccessibleExt for E {}

#[derive(Debug, Default, Clone, Copy)]
struct Extra {
    live: Option<Live>,
    disabled: bool,
    modal: bool,
}

/// An element with the extra states of [`AccessibleExt::accessible`]. It
/// adds no layout node: layout, painting and events are the inner
/// element's.
pub struct Accessible<E> {
    inner: E,
    extra: Extra,
}

impl<E> std::fmt::Debug for Accessible<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Accessible")
            .field("extra", &self.extra)
            .finish_non_exhaustive()
    }
}

impl<E> Accessible<E> {
    /// A live region: assistive technology announces changes to its
    /// content (`Polite` for status, `Assertive` for errors).
    pub fn live(mut self, live: Live) -> Self {
        self.extra.live = Some(live);
        self
    }

    /// A control that cannot be used now.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.extra.disabled = disabled;
        self
    }

    /// A dialog that keeps the keyboard until it closes.
    pub fn modal(mut self) -> Self {
        self.extra.modal = true;
        self
    }

    fn write_extra(&self, node: &mut accesskit::Node) {
        if let Some(live) = self.extra.live {
            node.set_live(live);
        }
        if self.extra.disabled {
            node.set_disabled();
        }
        if self.extra.modal {
            node.set_modal();
        }
    }
}

impl<E: Element + InteractiveElement> IntoElement for Accessible<E> {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl<E: Element + InteractiveElement> Element for Accessible<E> {
    type RequestLayoutState = E::RequestLayoutState;
    type PrepaintState = E::PrepaintState;

    fn id(&self) -> Option<ElementId> {
        Element::id(&self.inner)
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        self.inner.source_location()
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.inner.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        #[cfg(test)]
        testing::record(self, id, bounds, window);
        #[cfg(test)]
        testing::listen_for_probe(self);
        self.inner
            .prepaint(id, inspector_id, bounds, request_layout, window, cx)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.inner.paint(
            id,
            inspector_id,
            bounds,
            request_layout,
            prepaint,
            window,
            cx,
        );
    }

    fn a11y_role(&self) -> Option<Role> {
        self.inner.a11y_role()
    }

    fn write_a11y_info(&self, node: &mut accesskit::Node) {
        self.inner.write_a11y_info(node);
        self.write_extra(node);
    }

    fn a11y_synthetic_children(
        &mut self,
        prepaint: &mut Self::PrepaintState,
        builder: &mut gpui::A11ySubtreeBuilder<'_>,
    ) {
        self.inner.a11y_synthetic_children(prepaint, builder);
    }
}

impl<E: Element + InteractiveElement + Styled> Styled for Accessible<E> {
    fn style(&mut self) -> &mut StyleRefinement {
        self.inner.style()
    }
}

impl<E: Element + InteractiveElement> InteractiveElement for Accessible<E> {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.inner.interactivity()
    }
}

impl<E: Element + StatefulInteractiveElement> StatefulInteractiveElement for Accessible<E> {}

impl<E: Element + InteractiveElement + ParentElement> ParentElement for Accessible<E> {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.inner.extend(elements);
    }
}

/// Headless queries over the accessibility facts of the wrapped elements
/// ([`AccessibleExt::accessible`]) drawn in a window's last frame.
#[cfg(test)]
pub(crate) mod testing {
    use std::cell::{Cell, RefCell};
    use std::rc::{Rc, Weak};

    use gpui::{
        Bounds, Element, GlobalElementId, InteractiveElement, Pixels, Role, Toggled,
        VisualTestContext, Window, WindowId, accesskit,
    };

    use super::Accessible;

    /// What one wrapped element told assistive technology in the last
    /// frame it was drawn.
    #[derive(Debug, Clone)]
    pub(crate) struct Node {
        pub role: Option<Role>,
        pub label: Option<String>,
        pub description: Option<String>,
        pub value: Option<String>,
        pub selected: Option<bool>,
        pub expanded: Option<bool>,
        pub toggled: Option<Toggled>,
        pub level: Option<usize>,
        pub position: Option<(usize, usize)>,
        pub live: Option<accesskit::Live>,
        pub disabled: bool,
        pub modal: bool,
        /// GPUI lets assistive technology focus it (it is focusable).
        pub focusable: bool,
        /// It has a click action.
        pub clickable: bool,
        pub bounds: Bounds<Pixels>,
        /// Paint order within the frame.
        order: u64,
        window: WindowId,
    }

    struct Entry(RefCell<Node>);

    thread_local! {
        static ENTRIES: RefCell<Vec<Weak<Entry>>> = const { RefCell::new(Vec::new()) };
        static ORDER: Cell<u64> = const { Cell::new(0) };
    }

    pub(super) fn record<E: Element + InteractiveElement>(
        element: &Accessible<E>,
        id: Option<&GlobalElementId>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) {
        let Some(id) = id else {
            return;
        };
        let role = element.a11y_role();
        let mut node = accesskit::Node::new(role.unwrap_or(Role::Unknown));
        element.write_a11y_info(&mut node);
        let order = ORDER.with(|o| {
            o.set(o.get() + 1);
            o.get()
        });
        let facts = Node {
            role,
            label: node.label().map(str::to_owned),
            description: node.description().map(str::to_owned),
            value: node.value().map(str::to_owned),
            selected: node.is_selected(),
            expanded: node.is_expanded(),
            toggled: node.toggled(),
            level: node.level(),
            position: node.position_in_set().zip(node.size_of_set()),
            live: node.live(),
            disabled: node.is_disabled(),
            modal: node.is_modal(),
            focusable: node.supports_action(accesskit::Action::Focus),
            clickable: node.supports_action(accesskit::Action::Click),
            bounds,
            order,
            window: window.window_handle().window_id(),
        };
        // The entry lives in the element's state, so it goes away with the
        // first frame that does not draw the element.
        window.with_element_state(id, |state: Option<Rc<Entry>>, _| {
            let entry = match state {
                Some(entry) => {
                    *entry.0.borrow_mut() = facts;
                    entry
                }
                None => {
                    let entry = Rc::new(Entry(RefCell::new(facts)));
                    ENTRIES.with(|entries| {
                        let mut entries = entries.borrow_mut();
                        entries.retain(|e| e.strong_count() > 0);
                        entries.push(Rc::downgrade(&entry));
                    });
                    entry
                }
            };
            ((), entry)
        });
    }

    /// The key [`focused`] presses: bound to nothing.
    const PROBE_KEY: &str = "f24";

    thread_local! {
        static PROBED: RefCell<Vec<Option<String>>> = const { RefCell::new(Vec::new()) };
    }

    /// Lets [`focused`] see whether the element is on the focus path.
    pub(super) fn listen_for_probe<E: Element + InteractiveElement>(element: &mut Accessible<E>) {
        let role = element.inner.a11y_role();
        let mut node = accesskit::Node::new(role.unwrap_or(Role::Unknown));
        element.inner.write_a11y_info(&mut node);
        let label = node.label().map(str::to_owned);
        element
            .inner
            .interactivity()
            .on_key_down(move |event, _, _| {
                if event.keystroke.key == PROBE_KEY {
                    PROBED.with(|p| p.borrow_mut().push(label.clone()));
                }
            });
    }

    /// The name of the innermost wrapped element that has the keyboard
    /// (`Some(None)` for one without a name), found by pressing a key
    /// nothing is bound to.
    pub(crate) fn focused(vcx: &mut VisualTestContext) -> Option<Option<String>> {
        PROBED.with(|p| p.borrow_mut().clear());
        vcx.simulate_keystrokes(PROBE_KEY);
        PROBED.with(|p| p.borrow().first().cloned())
    }

    /// The wrapped elements of `vcx`'s window, in paint order.
    pub(crate) fn nodes(vcx: &mut VisualTestContext) -> Vec<Node> {
        let window = vcx.update(|window, _| window.window_handle().window_id());
        let mut nodes: Vec<Node> = ENTRIES.with(|entries| {
            entries
                .borrow()
                .iter()
                .filter_map(Weak::upgrade)
                .map(|e| e.0.borrow().clone())
                .filter(|n| n.window == window)
                .collect()
        });
        nodes.sort_by_key(|n| n.order);
        nodes
    }

    /// The elements with `role`, in paint order.
    pub(crate) fn with_role(vcx: &mut VisualTestContext, role: Role) -> Vec<Node> {
        nodes(vcx)
            .into_iter()
            .filter(|n| n.role == Some(role))
            .collect()
    }

    /// The one element named `label`.
    pub(crate) fn named(vcx: &mut VisualTestContext, label: &str) -> Node {
        let found: Vec<Node> = nodes(vcx)
            .into_iter()
            .filter(|n| n.label.as_deref() == Some(label))
            .collect();
        assert_eq!(found.len(), 1, "one element named {label:?}: {found:#?}");
        found.into_iter().next().expect("checked")
    }

    /// Whether an element is named `label`.
    pub(crate) fn has(vcx: &mut VisualTestContext, label: &str) -> bool {
        nodes(vcx).iter().any(|n| n.label.as_deref() == Some(label))
    }
}

#[cfg(test)]
mod contrast_tests;
#[cfg(test)]
mod tests;
