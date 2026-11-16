// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The bridge from the document model (`gv-model`) to the resolver.
//!
//! - [`links_from_model`] turns a [`DocModel`]'s links and images into
//!   [`LinkRef`]s, one per [`DocModel::links`] entry, in the same order.
//! - [`ModelFragments`] implements [`FragmentResolver`] over the model of
//!   the target document: heading fragments through
//!   [`DocModel::match_fragment`] under the profile's
//!   [`Anchoring`](crate::Anchoring), block ids through
//!   [`DocModel::match_block`]. The top, line ranges and viewer parameters
//!   need no document and resolve as [`StructuralFragments`] resolves them.
//!
//! The dependency points one way: this crate depends on `gv-model`, never
//! the reverse.

use gv_model::{BlockMatch, DocModel, FragmentMatch, Link};

use crate::fragment::{
    Fragment, FragmentKind, FragmentResolver, FragmentTarget, StructuralFragments,
};
use crate::link::{LinkKind, LinkRef};
use crate::path::VaultPath;
use crate::profile::{ProfileId, ProfileParams};

/// The resolver's kind of a model link. Images (inline or reference) are
/// [`LinkKind::Image`]; otherwise reference links (full, collapsed,
/// shortcut) are [`LinkKind::Reference`], autolinks (URI and email)
/// [`LinkKind::Autolink`], and inline links [`LinkKind::Inline`].
///
/// The model's kind is non-exhaustive: a kind added there without a
/// mapping here is treated as inline until it gets one.
#[must_use]
pub fn link_kind(link: &Link) -> LinkKind {
    if link.image {
        LinkKind::Image
    } else if link.kind.is_reference() {
        LinkKind::Reference
    } else if link.kind.is_autolink() {
        LinkKind::Autolink
    } else {
        LinkKind::Inline
    }
}

/// The [`LinkRef`] of one model link in `source`. The destination is the
/// model's: escapes and entities decoded, angle brackets removed, a
/// reference link's taken from its definition, `mailto:` added to email
/// autolinks.
#[must_use]
pub fn link_ref(link: &Link, source: VaultPath, profile: ProfileId) -> LinkRef {
    LinkRef::new(link_kind(link), &*link.destination, source, profile)
}

/// The [`LinkRef`] of every link and image in `model`, in the order of
/// [`DocModel::links`] (so the `i`-th result belongs to the `i`-th link and
/// its source ranges).
#[must_use]
pub fn links_from_model(model: &DocModel, source: &VaultPath, profile: ProfileId) -> Vec<LinkRef> {
    model
        .links()
        .iter()
        .map(|l| link_ref(l, source.clone(), profile))
        .collect()
}

/// Resolves `fragment` inside the document `model` under `params`.
///
/// Heading fragments use the profile's [`anchoring`](ProfileParams::anchoring)
/// as the primary rule and the model's fallbacks (other anchorings,
/// ignoring case) as loose matches; `top` is the top of the document
/// unless a heading has that id. Block ids match exactly, then ignoring
/// case (loose). The target heading's text is its Markdown source, as
/// Obsidian reports it.
#[must_use]
pub fn fragment_in_model(
    model: &DocModel,
    fragment: &Fragment,
    params: &ProfileParams,
) -> FragmentTarget {
    match &fragment.kind {
        FragmentKind::Heading(parts) => match model.match_fragment(parts, params.anchoring) {
            FragmentMatch::Top => FragmentTarget::Top,
            FragmentMatch::Heading { index, loose } => match model.headings().get(index) {
                Some(h) => FragmentTarget::Heading {
                    text: h.source.to_string(),
                    line: h.line,
                    loose,
                },
                None => FragmentTarget::Missing,
            },
            FragmentMatch::Missing => FragmentTarget::Missing,
        },
        FragmentKind::Block(id) => match model.match_block(id) {
            BlockMatch::Found { index, loose } => match model.block_anchors().get(index) {
                Some(a) => FragmentTarget::Block {
                    id: a.id.to_string(),
                    line: a.line,
                    loose,
                },
                None => FragmentTarget::Missing,
            },
            BlockMatch::Missing => FragmentTarget::Missing,
        },
        FragmentKind::Top | FragmentKind::Lines { .. } | FragmentKind::Viewer(_) => {
            structural(fragment, params)
        }
    }
}

fn structural(fragment: &Fragment, params: &ProfileParams) -> FragmentTarget {
    StructuralFragments.resolve_fragment(&VaultPath::root(), fragment, params)
}

/// A [`FragmentResolver`] over document models.
///
/// `load` returns the model of a target document (an open buffer's current
/// model, or one built from the file); `None` when there is none (not a
/// Markdown document, not readable), and heading and block fragments then
/// stay [`FragmentTarget::Unchecked`]. [`DocModel`] is an `Arc` snapshot,
/// so returning a clone is cheap.
pub struct ModelFragments<F> {
    load: F,
}

impl<F> ModelFragments<F>
where
    F: Fn(&VaultPath) -> Option<DocModel>,
{
    /// A resolver that loads target models with `load`.
    pub fn new(load: F) -> Self {
        Self { load }
    }
}

impl<F> std::fmt::Debug for ModelFragments<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelFragments").finish_non_exhaustive()
    }
}

impl<F> FragmentResolver for ModelFragments<F>
where
    F: Fn(&VaultPath) -> Option<DocModel>,
{
    fn resolve_fragment(
        &self,
        target: &VaultPath,
        fragment: &Fragment,
        params: &ProfileParams,
    ) -> FragmentTarget {
        match &fragment.kind {
            FragmentKind::Heading(_) | FragmentKind::Block(_) => match (self.load)(target) {
                Some(model) => fragment_in_model(&model, fragment, params),
                None => FragmentTarget::Unchecked,
            },
            FragmentKind::Top | FragmentKind::Lines { .. } | FragmentKind::Viewer(_) => {
                structural(fragment, params)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use gv_model::{Anchoring, SlugAlgorithm};

    use super::*;

    fn model(text: &str) -> DocModel {
        DocModel::build(&gv_syntax::parse(text), text)
    }

    /// A note parsed with the options of `profile` (front matter).
    fn model_for(profile: ProfileId, text: &str) -> DocModel {
        let options = gv_syntax::ParseOptions::builtin(profile);
        DocModel::build(&gv_syntax::parse_with(text, options), text)
    }

    fn src() -> VaultPath {
        VaultPath::new("dir/Source.md").unwrap()
    }

    #[test]
    fn every_link_kind_maps() {
        let text = "[i](a.md) [f][r] [r][] [r] <https://x.org> <me@x.org>\n\
                    ![img](p.png) ![ri][r] ![r]\n\n[r]: <b c.md#Part%202>\n";
        let m = model(text);
        let got: Vec<(LinkKind, String)> = links_from_model(&m, &src(), ProfileId::CommonMark)
            .into_iter()
            .map(|l| (l.kind, l.raw_destination))
            .collect();
        let want = [
            (LinkKind::Inline, "a.md"),
            (LinkKind::Reference, "b c.md#Part%202"),
            (LinkKind::Reference, "b c.md#Part%202"),
            (LinkKind::Reference, "b c.md#Part%202"),
            (LinkKind::Autolink, "https://x.org"),
            (LinkKind::Autolink, "mailto:me@x.org"),
            (LinkKind::Image, "p.png"),
            (LinkKind::Image, "b c.md#Part%202"),
            (LinkKind::Image, "b c.md#Part%202"),
        ];
        assert_eq!(
            got,
            want.map(|(k, d)| (k, d.to_owned())),
            "model kinds: {:?}",
            m.links().iter().map(|l| l.kind).collect::<Vec<_>>()
        );
        let one = link_ref(&m.links()[0], src(), ProfileId::Obsidian);
        assert_eq!(one.source_file, src());
        assert_eq!(one.profile, ProfileId::Obsidian);
    }

    #[test]
    fn escapes_and_entities_are_decoded_by_the_model() {
        let m = model("[a](<my file.md>) [b](x\\_y.md) [c](caf&eacute;.md)\n");
        let raw: Vec<String> = links_from_model(&m, &src(), ProfileId::GfmFile)
            .into_iter()
            .map(|l| l.raw_destination)
            .collect();
        assert_eq!(raw, ["my file.md", "x_y.md", "caf\u{e9}.md"]);
    }

    const DOC: &str =
        "# Intro\n\n## Part 2\n### Detail\n\n## Dup\n## Dup\n\n# Time: 10:30\n## **Bold** text\n";

    fn frag_md(s: &str, params: &ProfileParams) -> Fragment {
        Fragment::from_markdown(s, params)
    }

    fn heading(text: &str, line: u32, loose: bool) -> FragmentTarget {
        FragmentTarget::Heading {
            text: text.to_owned(),
            line,
            loose,
        }
    }

    #[test]
    fn github_slugs() {
        let m = model(DOC);
        let p = ProfileParams::for_profile(ProfileId::GfmFile);
        assert_eq!(p.anchoring, Anchoring::Slug(SlugAlgorithm::GitHub));
        let r = |s: &str| fragment_in_model(&m, &frag_md(s, &p), &p);
        assert_eq!(r("part-2"), heading("Part 2", 2, false));
        assert_eq!(r("user-content-detail"), heading("Detail", 3, false));
        assert_eq!(r("dup-1"), heading("Dup", 6, false));
        assert_eq!(r("time-1030"), heading("Time: 10:30", 8, false));
        assert_eq!(r("bold-text"), heading("**Bold** text", 9, false));
        // Fallbacks are loose: case, Obsidian heading text.
        assert_eq!(r("Part-2"), heading("Part 2", 2, true));
        assert_eq!(r("Part%202"), heading("Part 2", 2, true));
        assert_eq!(r("nope"), FragmentTarget::Missing);
        assert_eq!(r("top"), FragmentTarget::Top);
        assert_eq!(r(""), FragmentTarget::Top);
        assert_eq!(r("L3-L4"), FragmentTarget::Lines { start: 3, end: 4 });
        assert_eq!(r("page=2"), FragmentTarget::Passthrough);
        // No block ids in the CommonMark model.
        assert_eq!(r("^abc"), FragmentTarget::Missing);
    }

    #[test]
    fn obsidian_heading_text() {
        let m = model(DOC);
        let p = ProfileParams::for_profile(ProfileId::Obsidian);
        assert_eq!(p.anchoring, Anchoring::HeadingText);
        let w = |s: &str| fragment_in_model(&m, &Fragment::from_wikilink(s), &p);
        assert_eq!(w("Part 2"), heading("Part 2", 2, false));
        assert_eq!(w("part   2"), heading("Part 2", 2, false));
        assert_eq!(w("Part 2#Detail"), heading("Detail", 3, false));
        assert_eq!(w("Intro#Detail"), heading("Detail", 3, false));
        assert_eq!(w("Dup#Detail"), FragmentTarget::Missing);
        assert_eq!(w("Time 10 30"), heading("Time: 10:30", 8, false));
        assert_eq!(w("Bold text"), heading("**Bold** text", 9, false));
        // A GitHub slug in an Obsidian note still works, loosely.
        assert_eq!(w("part-2"), heading("Part 2", 2, true));
        // Markdown-link fragments are percent-decoded first.
        let md = frag_md("Part%202#Detail", &p);
        assert_eq!(fragment_in_model(&m, &md, &p), heading("Detail", 3, false));
    }

    #[test]
    fn front_matter_is_not_a_heading() {
        let text = "---\ntitle: Part 2\nlink: \"[x](Other.md)\"\n---\n# Part 2\n[y](Note.md)\n";
        let m = model_for(ProfileId::Obsidian, text);
        assert!(m.front_matter().is_some());
        let p = ProfileParams::for_profile(ProfileId::Obsidian);
        let w = |s: &str| fragment_in_model(&m, &Fragment::from_wikilink(s), &p);
        assert_eq!(w("Part 2"), heading("Part 2", 4, false));
        assert_eq!(w("title: Part 2"), FragmentTarget::Missing);
        let raw: Vec<String> = links_from_model(&m, &src(), ProfileId::Obsidian)
            .into_iter()
            .map(|l| l.raw_destination)
            .collect();
        assert_eq!(raw, ["Note.md"]);
    }

    #[test]
    fn no_anchors() {
        let m = model(DOC);
        let p = ProfileParams::for_profile(ProfileId::GfmComment);
        assert_eq!(p.anchoring, Anchoring::None);
        let r = |s: &str| fragment_in_model(&m, &frag_md(s, &p), &p);
        // Every heading match is loose; `top` is still the top.
        assert_eq!(r("part-2"), heading("Part 2", 2, true));
        assert_eq!(r("top"), FragmentTarget::Top);
        assert_eq!(r("nope"), FragmentTarget::Missing);
    }

    #[test]
    fn loader_is_used_only_when_needed() {
        let calls = Cell::new(0);
        let doc = model(DOC);
        let resolver = ModelFragments::new(|p: &VaultPath| {
            calls.set(calls.get() + 1);
            (p.as_str() == "Note.md").then(|| doc.clone())
        });
        let p = ProfileParams::for_profile(ProfileId::Obsidian);
        let note = VaultPath::new("Note.md").unwrap();
        let other = VaultPath::new("pic.png").unwrap();
        let f = |s: &str| Fragment::from_wikilink(s);
        assert_eq!(
            resolver.resolve_fragment(&note, &f("Detail"), &p),
            heading("Detail", 3, false)
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(
            resolver.resolve_fragment(&other, &f("Detail"), &p),
            FragmentTarget::Unchecked
        );
        assert_eq!(
            resolver.resolve_fragment(&other, &f("^id"), &p),
            FragmentTarget::Unchecked
        );
        assert_eq!(calls.get(), 3);
        assert_eq!(
            resolver.resolve_fragment(&note, &f(""), &p),
            FragmentTarget::Top
        );
        assert_eq!(
            resolver.resolve_fragment(&other, &f("page=3"), &p),
            FragmentTarget::Passthrough
        );
        assert_eq!(calls.get(), 3);
        assert!(format!("{resolver:?}").starts_with("ModelFragments"));
    }
}
