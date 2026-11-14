// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The keymap model's contexts as GPUI key contexts.
//!
//! Each context name of the model is a GPUI key context identifier that the
//! view which owns that state sets with `key_context`. A binding's context
//! predicate becomes a GPUI predicate of the same shape.
//!
//! GPUI evaluates a predicate against the chain of key contexts from the
//! window's root to the focused element, deepest first, and `&&` holds only
//! when both sides hold on the same element (`!` looks at the whole chain).
//! The focus contexts name the focused view, so they are on the deepest
//! element that has one; state contexts (`autocomplete`, `split`) are set on
//! the focused view's own key context, beside its focus context, so that
//! `editor && autocomplete` holds.

use gpui::{KeyBindingContextPredicate, SharedString};
use gv_settings::keymap::ContextExpr;

/// The GPUI key context of each model context name.
pub const KEY_CONTEXTS: &[(&str, &str)] = &[
    ("editor", "Editor"),
    ("reader", crate::reader::KEY_CONTEXT),
    ("tree", "FileTree"),
    ("outline", "Outline"),
    ("search", "VaultSearch"),
    ("palette", "CommandPalette"),
    ("switcher", "QuickSwitcher"),
    ("menu", MENU),
    ("infobar", "Infobar"),
    ("settings", "SettingsWindow"),
    ("autocomplete", "Autocomplete"),
    ("split", "Split"),
];

/// The key context of the application menu and, later, of context menus.
pub const MENU: &str = "Menu";

/// The GPUI key context for a model context name. Names the app never sets
/// (the model warns about them) are used as they are, so they never match.
pub fn gpui_context(name: &str) -> SharedString {
    KEY_CONTEXTS
        .iter()
        .find(|(model, _)| *model == name)
        .map_or_else(
            || SharedString::from(name.to_owned()),
            |(_, gpui)| (*gpui).into(),
        )
}

/// The GPUI predicate for a model context predicate.
pub fn predicate(expr: &ContextExpr) -> KeyBindingContextPredicate {
    use KeyBindingContextPredicate as P;
    let join = |parts: &[ContextExpr], op: fn(Box<P>, Box<P>) -> P| {
        parts
            .iter()
            .map(predicate)
            .reduce(|left, right| op(Box::new(left), Box::new(right)))
            // The model never builds an empty `&&` or `||`. No element has
            // an empty key context, so this would hold nowhere.
            .unwrap_or_else(|| P::Identifier(SharedString::default()))
    };
    match expr {
        ContextExpr::Name(name) => P::Identifier(gpui_context(name)),
        ContextExpr::Not(inner) => P::Not(Box::new(predicate(inner))),
        ContextExpr::And(parts) => join(parts, P::And),
        ContextExpr::Or(parts) => join(parts, P::Or),
    }
}
