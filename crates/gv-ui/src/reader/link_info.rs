// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The link a command or a click acts on, in either view, and what its
//! status means (09 § Unresolved, ambiguous and pending links).
//!
//! - [`LinkAt`]: a link of a document, with the construct's place in the
//!   text when it is known (rewrites need it).
//! - The **link menu's link**: a right-click on a link in the Reading view
//!   or the Source view records it ([`set_menu_link`]) before the link menu
//!   opens; the `links.*` commands the menu runs act on it.
//! - [`source_link_at`] finds the link at a byte offset of the Source
//!   view's text: inline links, images, autolinks, wikilinks, embeds and
//!   the destination of a reference definition.
//! - [`status_tooltip`]: the sentence a link's tooltip shows for its
//!   status (nothing for exact matches and URLs).

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Global, InteractiveElement as _, IntoElement as _, ParentElement as _, Pixels,
    Point, SharedString, Styled as _, anchored, deferred, div, point, px,
};
use gv_render_tree::LinkStatus;
use gv_syntax::inline::{InlineKind, LinkKind as SyntaxLinkKind};
use gv_syntax::{Leaf, LeafKind, SyntaxTree, Visitor, walk_range};
use gv_text::TextSnapshot;
use gv_vault::Resolution;

use crate::appearance::Appearance;
use crate::metrics::{radius, size, spacing, text};

/// A link of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkAt {
    /// The document the link is in.
    pub file: PathBuf,
    pub kind: gv_model::LinkKind,
    pub image: bool,
    /// As the resolver takes it (wikilinks: everything between the
    /// brackets).
    pub destination: Arc<str>,
    /// The whole construct in the document's text, when known.
    pub range: Option<Range<usize>>,
    /// The construct as written, when known.
    pub construct: Option<Arc<str>>,
    /// The document's profile, which the link is resolved in.
    pub profile: gv_vault::ProfileId,
}

/// The link the link menu was opened on.
#[derive(Debug, Default)]
struct MenuLink(Option<LinkAt>);

impl Global for MenuLink {}

/// Records the link a right-click opens the link menu on.
pub fn set_menu_link(link: Option<LinkAt>, cx: &mut App) {
    cx.set_global(MenuLink(link));
}

/// The link the link menu was last opened on.
pub fn menu_link(cx: &App) -> Option<&LinkAt> {
    cx.try_global::<MenuLink>().and_then(|m| m.0.as_ref())
}

/// The resolver's kind of a syntax link.
pub(crate) fn model_kind(kind: SyntaxLinkKind) -> gv_model::LinkKind {
    match kind {
        SyntaxLinkKind::Inline => gv_model::LinkKind::Inline,
        SyntaxLinkKind::Full => gv_model::LinkKind::Full,
        SyntaxLinkKind::Collapsed => gv_model::LinkKind::Collapsed,
        SyntaxLinkKind::Shortcut => gv_model::LinkKind::Shortcut,
        SyntaxLinkKind::UriAutolink => gv_model::LinkKind::UriAutolink,
        SyntaxLinkKind::EmailAutolink => gv_model::LinkKind::EmailAutolink,
    }
}

/// A link found in the Source view's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLink {
    pub kind: gv_model::LinkKind,
    pub image: bool,
    pub destination: String,
    /// The whole construct.
    pub range: Range<usize>,
}

/// The link at byte `offset` of `text` (whose tree is `tree`): the
/// innermost link, image, autolink, wikilink or embed whose construct
/// holds the offset (its end included, so a caret just after `]]` is on
/// the link), or the destination of a reference definition.
pub fn source_link_at(tree: &SyntaxTree, text: &TextSnapshot, offset: usize) -> Option<SourceLink> {
    if tree.len() != text.len() || offset > text.len() {
        return None;
    }
    struct Find<'a> {
        tree: &'a SyntaxTree,
        text: &'a TextSnapshot,
        offset: usize,
        found: Option<SourceLink>,
    }
    impl Visitor for Find<'_> {
        fn leaf(&mut self, leaf: &Leaf<'_>) {
            if self.found.is_some() {
                return;
            }
            match leaf.kind {
                LeafKind::LinkRefDef => {
                    if let Some(def) = leaf.link_ref_def(self.text)
                        && def.destination_range.start <= self.offset
                        && self.offset <= def.destination_range.end
                    {
                        self.found = Some(SourceLink {
                            kind: gv_model::LinkKind::Full,
                            image: false,
                            destination: def.def.destination.clone(),
                            range: def.range,
                        });
                    }
                }
                LeafKind::Paragraph
                | LeafKind::Heading { .. }
                | LeafKind::CalloutTitle
                | LeafKind::Table => {
                    let inlines = self.tree.inlines(leaf, self.text);
                    let mut best: Option<SourceLink> = None;
                    for node in inlines.nodes() {
                        let range = node.range();
                        if !(range.start <= self.offset && self.offset <= range.end) {
                            continue;
                        }
                        let found = match node.kind() {
                            InlineKind::Wikilink | InlineKind::Embed => {
                                node.wikilink().map(|w| SourceLink {
                                    kind: if w.embed {
                                        gv_model::LinkKind::Embed
                                    } else {
                                        gv_model::LinkKind::Wikilink
                                    },
                                    image: false,
                                    destination: w.inner.into_owned(),
                                    range: w.range,
                                })
                            }
                            InlineKind::Link | InlineKind::Image | InlineKind::Autolink => {
                                node.link().map(|l| SourceLink {
                                    kind: model_kind(l.kind),
                                    image: l.is_image,
                                    destination: if l.kind == SyntaxLinkKind::EmailAutolink {
                                        format!("mailto:{}", l.destination)
                                    } else {
                                        l.destination.to_owned()
                                    },
                                    range: l.range,
                                })
                            }
                            _ => None,
                        };
                        // Preorder: a later match is inside an earlier one.
                        if found.is_some() {
                            best = found;
                        }
                    }
                    self.found = best;
                }
                _ => {}
            }
        }
    }
    let mut find = Find {
        tree,
        text,
        offset,
        found: None,
    };
    let start = offset.saturating_sub(1);
    walk_range(
        tree,
        start..(offset + 1).min(text.len()).max(start),
        &mut find,
    );
    find.found
}

/// The name a link's tooltip uses for its target: the path part of the
/// destination (before the fragment, the query or the display text).
pub fn link_name(kind: gv_model::LinkKind, destination: &str) -> String {
    let wiki = matches!(
        kind,
        gv_model::LinkKind::Wikilink | gv_model::LinkKind::Embed
    );
    let end = if wiki {
        destination.find(['#', '|'])
    } else {
        destination.find(['#', '?'])
    };
    let name = end.map_or(destination, |e| &destination[..e]).trim();
    if wiki {
        name.to_owned()
    } else {
        gv_vault::link::percent_decode_lossy(name)
    }
}

/// What a link's tooltip says about `status` (`resolution`: the link
/// resolved again, for its target and candidates; `follow`: how the view
/// follows a link, "click" or "Ctrl+click"). `None` for exact matches and
/// URLs, which need no explanation.
pub fn status_tooltip(
    status: LinkStatus,
    name: &str,
    resolution: Option<&Resolution>,
    follow: &str,
) -> Option<String> {
    let target = resolution
        .and_then(|r| r.target_path.as_ref())
        .map(|p| p.as_str().to_owned());
    let target = target.as_deref().unwrap_or(name);
    let text = match status {
        LinkStatus::Exact | LinkStatus::External => return None,
        LinkStatus::CaseFold => format!(
            "Matches “{target}” only when letter case is ignored: it may break on Linux and GitHub."
        ),
        LinkStatus::Loose => {
            format!("Matches “{target}” only loosely: the profile's own tool may not find it.")
        }
        LinkStatus::Ambiguous(n) => {
            let mut text = format!("{n} notes match “{name}”; {follow} opens “{target}”.");
            if let Some(r) = resolution {
                for c in &r.candidates {
                    text.push_str("\n• ");
                    text.push_str(c.path.as_str());
                }
            }
            text
        }
        LinkStatus::Pending => {
            "The vault is still being indexed: where this link goes is not known yet.".to_owned()
        }
        LinkStatus::Unresolved => format!("No note “{name}” — {follow} to create it."),
        LinkStatus::OutsideRoot => {
            let at = resolution
                .and_then(|r| r.outside_path.clone())
                .unwrap_or_else(|| name.to_owned());
            format!("Points outside the vault: {at}")
        }
        LinkStatus::Blocked => {
            let why = resolution
                .and_then(|r| r.block)
                .map_or_else(|| "not allowed".to_owned(), |b| b.to_string());
            format!("Blocked: {why}")
        }
    };
    Some(text)
}

/// How long the pointer rests on a link before its tooltip shows (not
/// designed).
pub const TOOLTIP_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

/// A link's tooltip at window point `at` (SPEC § Popovers, Tooltip:
/// inverted, 11.5 px, padding `0 8`, radius 5, 22 high per line; a list of
/// candidates makes it taller, which is not designed).
pub fn tooltip(text: String, at: Point<Pixels>, cx: &App) -> AnyElement {
    let theme = Appearance::theme(cx);
    let lines: Vec<SharedString> = text
        .lines()
        .map(|l| SharedString::from(l.to_owned()))
        .collect();
    let multi = lines.len() > 1;
    deferred(
        anchored()
            .position(at + point(px(0.0), px(TOOLTIP_OFFSET)))
            .child(
                div()
                    .debug_selector(|| "link-tooltip".to_owned())
                    .min_h(px(size::TOOLTIP))
                    .max_w(px(TOOLTIP_MAX_WIDTH))
                    .px(px(spacing::tooltip::PAD_X))
                    .when(multi, |d| d.py(px(TOOLTIP_PAD_Y)))
                    .flex()
                    .flex_col()
                    .justify_center()
                    .rounded(px(radius::CONTROL))
                    .bg(theme.text.primary)
                    .text_color(theme.surface.page)
                    .text_size(px(text::TOOLTIP_SIZE))
                    .children(lines.into_iter().map(|l| div().child(l))),
            ),
    )
    .with_priority(2)
    .into_any_element()
}

/// Below the pointer, clear of the cursor (not designed).
const TOOLTIP_OFFSET: f32 = 20.0;
const TOOLTIP_MAX_WIDTH: f32 = 420.0;
const TOOLTIP_PAD_Y: f32 = 3.0;

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> (SyntaxTree, TextSnapshot) {
        let snapshot = TextSnapshot::new(text);
        let options = gv_syntax::ParseOptions::builtin(gv_profile::ProfileId::Obsidian);
        (gv_syntax::parse_with(&snapshot, options), snapshot)
    }

    #[test]
    fn finds_the_link_at_an_offset() {
        let text = "a [x](b.md#h) [[Note#H|alias]] <https://e.x> ![i](p.png)\n\n[r]: other.md\n";
        let (tree, snapshot) = parse(text);
        let at = |needle: &str| {
            let offset = text.find(needle).expect("needle");
            source_link_at(&tree, &snapshot, offset)
        };
        let link = at("x]").expect("inline link");
        assert_eq!(link.kind, gv_model::LinkKind::Inline);
        assert_eq!(link.destination, "b.md#h");
        assert_eq!(&text[link.range.clone()], "[x](b.md#h)");
        let wiki = at("Note#").expect("wikilink");
        assert_eq!(wiki.kind, gv_model::LinkKind::Wikilink);
        assert_eq!(wiki.destination, "Note#H|alias");
        assert_eq!(&text[wiki.range], "[[Note#H|alias]]");
        let auto = at("e.x").expect("autolink");
        assert_eq!(auto.kind, gv_model::LinkKind::UriAutolink);
        let image = at("p.png").expect("image");
        assert!(image.image);
        let def = at("other.md").expect("definition");
        assert_eq!(def.destination, "other.md");
        assert_eq!(at("a ["), None);
        // The end of the construct counts (a caret after `]]`).
        let end = text.find("]] ").expect("end") + 2;
        assert!(source_link_at(&tree, &snapshot, end).is_some());
    }

    #[test]
    fn tooltips_explain_each_status() {
        assert_eq!(status_tooltip(LinkStatus::Exact, "x", None, "click"), None);
        assert_eq!(
            status_tooltip(LinkStatus::External, "x", None, "click"),
            None
        );
        let missing = status_tooltip(LinkStatus::Unresolved, "New", None, "click").expect("text");
        assert_eq!(missing, "No note “New” — click to create it.");
        for status in [
            LinkStatus::CaseFold,
            LinkStatus::Loose,
            LinkStatus::Ambiguous(2),
            LinkStatus::Pending,
            LinkStatus::OutsideRoot,
            LinkStatus::Blocked,
        ] {
            assert!(
                status_tooltip(status, "x", None, "click").is_some(),
                "{status}"
            );
        }
        assert_eq!(link_name(gv_model::LinkKind::Wikilink, "A b#c|d"), "A b");
        assert_eq!(
            link_name(gv_model::LinkKind::Inline, "A%20b.md#c"),
            "A b.md"
        );
    }
}
