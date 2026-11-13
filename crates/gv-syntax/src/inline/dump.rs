// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The S-expression dump of an inline tree (part of the deterministic CST
//! dump): one line per node or token, indented two spaces per level, with
//! document ranges; tokens show their gathered text, links their kind,
//! destination and title.
//!
//! ```text
//! (Emphasis 4..9
//!   (Marker 4..5 "*")
//!   (Text 5..8 "foo")
//!   (Marker 8..9 "*"))
//! ```

use std::fmt::Write as _;

use super::api::InlineTree;
use super::tree::InlineKind as K;

/// Appends the dump of `tree`, every line prefixed with `indent` spaces.
pub(super) fn dump(tree: &InlineTree, indent: usize, out: &mut String) {
    let mut open: Vec<u32> = Vec::new();
    for (i, n) in tree.nodes.iter().enumerate() {
        let depth = indent + 2 * open.len();
        out.extend(std::iter::repeat_n(' ', depth));
        let r = tree.src_range(n.start, n.end);
        let _ = write!(out, "({} {}..{}", n.kind.name(), r.start, r.end);
        if n.kind.is_token() {
            let _ = write!(out, " {:?}", tree.slice(n.start, n.end));
        } else if matches!(n.kind, K::Link | K::Image | K::Autolink)
            && let Some(l) = tree.links.get(n.data as usize)
        {
            let _ = write!(out, " {} {:?}", l.kind.name(), tree.str(l.dest));
            if let Some(t) = l.title {
                let _ = write!(out, " {:?}", tree.str(t));
            }
        }
        if n.kind.is_token() || n.next as usize == i + 1 {
            out.push(')');
            while open.last().is_some_and(|&end| end as usize == i + 1) {
                out.push(')');
                open.pop();
            }
        } else {
            open.push(n.next);
        }
        out.push('\n');
    }
}
