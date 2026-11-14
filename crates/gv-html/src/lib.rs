// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Raw HTML regions, sanitizer, HTML to render nodes, HTML to Markdown for paste.
//!
//! # Paste
//!
//! - [`html_to_markdown`] converts clipboard HTML to Markdown for a
//!   [`Profile`] ([`convert_html`] also reports whether the pre-parse
//!   guards fired). See the [`paste`] module for the conversion rules.
//! - [`smart_paste_text`] turns a URL pasted over a selection into a link;
//!   [`clipboard_url`] and [`url_autolink`] cover a URL pasted alone.
//!
//! HTML is parsed by html5ever into an arena of our own (no reference
//! counted DOM). Before parsing, a linear scan bounds the nesting depth,
//! the attributes per tag and the tag and input sizes, because html5ever
//! has quadratic paths for deep nesting and for many attributes; input
//! over a limit is reduced to its text instead.

mod dom;
mod guard;
pub mod paste;
mod profile;
mod smart;
mod url;

pub use guard::GuardTrip;
pub use paste::{Conversion, PasteOptions, convert_html, html_to_markdown, plain_text_markdown};
pub use profile::Profile;
pub use smart::{clipboard_url, smart_paste_text, url_autolink};
