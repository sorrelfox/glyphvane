// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Reading view's Obsidian constructs in the render tree: callouts
//! (titles, folds, nesting), wikilinks, image and note embeds (budgets and
//! cycles), highlights, hidden comments and block ids, tag pills, and
//! line breaks.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::{Doc, build_with, check};
use gv_model::LinkKind;
use gv_render_tree::{
    CalloutType, EmbedFetch, FrameKind, ImageSize, ImageSizeSource, InlineRole, LinkStatus,
    LinkStatusSource, LinkTarget, NodeKind, RenderOptions, RenderTree, Sources,
};
use gv_syntax::{CalloutFold, ParseOptions, ProfileId};

/// Notes by name, and their links' statuses: an Obsidian vault.
#[derive(Default)]
struct Vault {
    notes: HashMap<&'static str, &'static str>,
}

impl LinkStatusSource for Vault {
    fn status(&self, link: &LinkTarget<'_>) -> LinkStatus {
        let name = link.destination.split(['#', '|']).next().unwrap_or("");
        if link.kind == LinkKind::Wikilink && !self.notes.contains_key(name) {
            LinkStatus::Unresolved
        } else {
            LinkStatus::Exact
        }
    }

    fn embed(&self, destination: &str, _from: Option<&str>) -> EmbedFetch {
        let name = destination.split(['#', '|']).next().unwrap_or("").trim();
        match self.notes.get(name) {
            Some(text) => EmbedFetch::Note {
                key: Arc::from(format!("{name}.md")),
                title: Arc::from(name),
                text: Arc::from(*text),
            },
            None => EmbedFetch::Unavailable,
        }
    }
}

/// Every image is 200 × 100.
struct Images;

impl ImageSizeSource for Images {
    fn size(&self, _: &LinkTarget<'_>) -> ImageSize {
        ImageSize::Known {
            width: 200.0,
            height: 100.0,
        }
    }
}

fn ob_with(text: &str, vault: &Vault) -> Doc {
    let d = build_with(
        text,
        ParseOptions::builtin(ProfileId::Obsidian),
        RenderOptions::builtin(ProfileId::Obsidian),
        Sources {
            links: vault,
            images: &Images,
        },
    );
    check(&d);
    d
}

fn ob(text: &str) -> Doc {
    ob_with(text, &Vault::default())
}

/// Each item's text and its innermost frame.
fn outline(rt: &RenderTree) -> Vec<String> {
    (0..rt.items().len())
        .map(|ix| {
            let it = &rt.items()[ix];
            let frames: Vec<String> = it
                .context
                .iter()
                .filter_map(|f| match &f.kind {
                    FrameKind::Callout { name, fold } => {
                        Some(format!("callout({name}{})", fold.name()))
                    }
                    FrameKind::Details => Some("fold".to_owned()),
                    FrameKind::Embed { depth } => Some(format!("embed({depth})")),
                    _ => None,
                })
                .collect();
            format!("{} {}", rt.item_text(ix).text, frames.join(">"))
        })
        .collect()
}

#[test]
fn callouts_have_a_title_row_and_fold() {
    let d = ob("> [!tip]- My *title*\n> Body one.\n>\n> Body two.\n\nAfter.\n");
    assert_eq!(
        outline(&d.render),
        [
            "My title callout(tip-)",
            "Body one. callout(tip-)>fold",
            "Body two. callout(tip-)>fold",
            "After. "
        ]
    );
    let n = &d.render.nodes()[d.render.items()[0].node];
    assert!(matches!(
        n.kind,
        NodeKind::CalloutTitle {
            fold: CalloutFold::Closed,
            ..
        }
    ));
    // The body's fold frame is keyed by the title's block.
    let fold = d.render.items()[1].context.last().expect("frame");
    assert_eq!(fold.id, n.id);
    // Not foldable: no fold frame.
    let d = ob("> [!note] T\n> b\n");
    assert_eq!(outline(&d.render), ["T callout(note)", "b callout(note)"]);
}

#[test]
fn callouts_without_a_title_show_their_type() {
    let d = ob("> [!faq]+\n> Answer.\n\n> [!my-type]\n> x\n");
    assert_eq!(
        outline(&d.render),
        [
            "Faq callout(faq+)",
            "Answer. callout(faq+)>fold",
            "My-type callout(my-type)",
            "x callout(my-type)"
        ]
    );
    assert_eq!(CalloutType::of("FAQ"), CalloutType::Question);
    assert_eq!(CalloutType::of("my-type"), CalloutType::Note);
    assert_eq!(CalloutType::of("cite"), CalloutType::Quote);
    assert!(!CalloutType::is_known("my-type") && CalloutType::is_known("Note"));
}

#[test]
fn nested_callouts() {
    let d = ob("> [!note] Outer\n> > [!warning]- Inner\n> > text\n");
    assert_eq!(
        outline(&d.render),
        [
            "Outer callout(note)",
            "Inner callout(note)>callout(warning-)",
            "text callout(note)>callout(warning-)>fold"
        ]
    );
}

#[test]
fn wikilinks_are_links_with_a_status() {
    let mut vault = Vault::default();
    vault.notes.insert("Note", "# Note\n");
    let d = ob_with(
        "See [[Note#Part|the part]], [[Missing]] and [[Note#H]].\n",
        &vault,
    );
    assert_eq!(
        d.render.item_text(0).text,
        "See the part, Missing and Note > H."
    );
    let c = d.render.nodes()[0].inline().expect("inline");
    let links: Vec<(LinkKind, &str, LinkStatus)> = c
        .links
        .iter()
        .map(|l| (l.kind, &*l.destination, l.status))
        .collect();
    assert_eq!(
        links,
        [
            (LinkKind::Wikilink, "Note#Part|the part", LinkStatus::Exact),
            (LinkKind::Wikilink, "Missing", LinkStatus::Unresolved),
            (LinkKind::Wikilink, "Note#H", LinkStatus::Exact),
        ]
    );
}

#[test]
fn highlights_comments_block_ids_and_tags() {
    let d = ob("A ==mark== b %%hidden%% c #tag/x d ^block-1\n");
    assert_eq!(d.render.item_text(0).text, "A mark b  c #tag/x d");
    let c = d.render.nodes()[0].inline().expect("inline");
    let dump = d.render.dump();
    assert!(dump.contains("tag"), "{dump}");
    let tag = c
        .spans
        .iter()
        .position(|s| s.role == InlineRole::Tag)
        .expect("a tag pill");
    let link = c.spans[tag].link.expect("click target") as usize;
    assert_eq!(
        (c.links[link].kind, &*c.links[link].destination),
        (LinkKind::Tag, "tag/x")
    );
    // The highlight's background.
    assert!(
        dump.lines()
            .any(|l| l.contains("\"mark\"") && l.contains("chip")),
        "{dump}"
    );
    // A block id alone is not shown; a block comment neither.
    let d = ob("- a\n\n^list\n\n%%\nhidden\n%%\nEnd\n");
    assert_eq!(outline(&d.render), ["a ", "End "]);
}

#[test]
fn soft_breaks_are_line_breaks_unless_strict() {
    let d = ob("one\ntwo\n");
    let c = d.render.nodes()[0].inline().expect("inline");
    assert!(c.spans.iter().any(|s| s.role == InlineRole::Break));
    let strict = gv_profile::compile(
        ProfileId::Obsidian,
        gv_profile::Tuning::from_obsidian_app_json(r#"{"strictLineBreaks": true}"#),
        &gv_profile::SwitchOverrides::new(),
    );
    let d = build_with(
        "one\ntwo\n",
        ParseOptions::from_profile(&strict),
        RenderOptions::for_profile(&strict),
        Sources::default(),
    );
    let c = d.render.nodes()[0].inline().expect("inline");
    assert!(!c.spans.iter().any(|s| s.role == InlineRole::Break));
}

#[test]
fn image_embeds_take_their_size() {
    let d = ob("![[pic.png|100]] ![[pic.png|50x40]] ![[pic.png]]\n");
    let c = d.render.nodes()[0].inline().expect("inline");
    let images: Vec<(LinkKind, &str)> =
        c.images.iter().map(|i| (i.kind, &*i.destination)).collect();
    assert_eq!(
        images,
        [
            (LinkKind::Embed, "pic.png"),
            (LinkKind::Embed, "pic.png"),
            (LinkKind::Embed, "pic.png")
        ]
    );
    let dump = d.render.dump();
    // 100 wide keeps the ratio; 50x40 is both; none: the image's own.
    for size in ["100x50", "50x40", "200x100"] {
        assert!(dump.contains(size), "{size} in {dump}");
    }
}

#[test]
fn note_embeds_show_the_note_section_or_block() {
    let mut vault = Vault::default();
    vault.notes.insert(
        "Other",
        "---\ntitle: x\n---\n# Top\nIntro **bold**.\n\n## Part\nIn part.\n\n## Next\nNot here.\n\n- item ^b1\n  - child\n- other\n\nPara ^p2\n",
    );
    let d = ob_with("Before\n\n![[Other#Part]]\n\nAfter\n", &vault);
    assert_eq!(
        outline(&d.render),
        [
            "Before ",
            "Other > Part embed(0)",
            "Part embed(0)",
            "In part. embed(0)",
            "After "
        ]
    );
    let d = ob_with("![[Other#^b1]]\n", &vault);
    assert_eq!(
        outline(&d.render),
        [
            "Other > ^b1 embed(0)",
            "• item embed(0)",
            "• child embed(0)"
        ]
    );
    let d = ob_with("![[Other]]\n", &vault);
    let texts = outline(&d.render);
    assert_eq!(texts[1], "Top embed(0)");
    assert_eq!(texts[2], "Intro bold. embed(0)");
    // A missing section says so.
    let d = ob_with("![[Other#Nowhere]]\n", &vault);
    assert_eq!(
        outline(&d.render)[1],
        "\u{201c}Nowhere\u{201d} is not in Other. embed(0)"
    );
    // A missing note: only the link.
    let d = ob_with("![[Gone]]\n", &vault);
    assert_eq!(outline(&d.render), ["Gone embed(0)"]);
}

#[test]
fn embed_cycles_and_depth_are_guarded() {
    let mut vault = Vault::default();
    vault.notes.insert("A", "A text\n\n![[B]]\n");
    vault.notes.insert("B", "B text\n\n![[A]]\n");
    let d = ob_with("![[A]]\n", &vault);
    let texts = outline(&d.render);
    assert_eq!(
        texts,
        [
            "A embed(0)",
            "A text embed(0)",
            "\u{29c9} B embed(0)>embed(1)",
            "B text embed(0)>embed(1)",
            "\u{29c9} A embed(0)>embed(1)>embed(2)",
            "Embed cycle: A \u{2192} B \u{2192} A embed(0)>embed(1)>embed(2)"
        ]
    );
    // A chain deeper than the budget stops with a notice.
    let mut vault = Vault::default();
    let names = ["N0", "N1", "N2", "N3", "N4", "N5", "N6", "N7"];
    let texts: Vec<&'static str> = (0..7)
        .map(|i| &*Box::leak(format!("x{i}\n\n![[N{}]]\n", i + 1).into_boxed_str()))
        .collect();
    for i in 0..7 {
        vault.notes.insert(names[i], texts[i]);
    }
    vault.notes.insert("N7", "end\n");
    let d = ob_with("![[N0]]\n", &vault);
    let texts = outline(&d.render);
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("Embeds nested more than 5 deep are not shown.")),
        "{texts:#?}"
    );
    assert!(!texts.iter().any(|t| t.starts_with("end ")));
}

#[test]
fn edits_next_to_embeds_and_callouts_keep_the_tree_valid() {
    let mut vault = Vault::default();
    vault.notes.insert("Other", "# Head\nText\n");
    let mut d = ob_with("![[Other]]\n\n> [!tip]- T\n> b\n\nEnd\n", &vault);
    let sources = Sources {
        links: &vault,
        images: &Images,
    };
    let at = d.text.find("End").expect("text");
    d.edit(at..at, "x", sources);
    check(&d);
    let fresh = ob_with(&d.text.clone(), &vault);
    assert_eq!(outline(&d.render), outline(&fresh.render));
    let at = d.text.find("b\n").expect("text");
    d.edit(at..at + 1, "body", sources);
    check(&d);
    let fresh = ob_with(&d.text.clone(), &vault);
    assert_eq!(outline(&d.render), outline(&fresh.render));
}

fn ob_unchecked(text: &str) -> Doc {
    build_with(
        text,
        ParseOptions::builtin(ProfileId::Obsidian),
        RenderOptions::builtin(ProfileId::Obsidian),
        Sources::default(),
    )
}

#[test]
fn inline_footnotes_are_numbered_into_the_footnote_section() {
    let md = "Text^[note *one*] and[^a] more^[two].\n\n[^a]: Def a.\n";
    let d = ob_unchecked(md);
    let rt = &d.render;
    // One sequence with the references, in document order.
    let c = rt.nodes()[0].inline().expect("paragraph");
    let numbers: Vec<u32> = c.footnotes.iter().map(|f| f.number).collect();
    assert_eq!(numbers, [1, 2, 3]);
    assert_eq!(rt.item_text(0).text, "Text1 and2 more3.");
    let section = rt.footnote_section_start();
    let texts: Vec<String> = (section..rt.items().len())
        .map(|ix| rt.item_text(ix).text.clone())
        .collect();
    assert_eq!(texts, ["", "note one", "Def a.", "two"]);
    // The definition is styled like the reference's text (emphasis kept).
    let def = rt.item_inline(section + 1).expect("definition");
    assert!(
        def.spans
            .iter()
            .any(|s| matches!(s.role, InlineRole::FootnoteBackref(0)))
    );
    // Jumps both ways go through the inline footnote's label.
    let label = c.footnotes[0].label.to_string();
    assert_eq!(rt.footnote_item(&label), Some(section + 1));
    let back = rt.footnote_refs(&label);
    assert_eq!(back.len(), 1);
    assert_eq!(&md[back[0].1.clone()], "^[note *one*]");
    // A position inside the note still finds the paragraph.
    let inside = md.find("one").expect("one");
    assert_eq!(rt.item_at(inside), Some(0));
}

#[test]
fn a_document_with_only_inline_footnotes_gets_a_section() {
    let d = ob_unchecked("# Head^[in heading]\n\n| a |\n|---|\n| cell^[in cell] |\n");
    let rt = &d.render;
    let section = rt.footnote_section_start();
    assert!(section < rt.items().len());
    let texts: Vec<String> = (section + 1..rt.items().len())
        .map(|ix| rt.item_text(ix).text.clone())
        .collect();
    assert_eq!(texts, ["in heading", "in cell"]);
    // Profiles without inline footnotes keep the text.
    let gfm = common::build_with(
        "A^[b].\n",
        ParseOptions::builtin(ProfileId::GfmFile),
        RenderOptions::builtin(ProfileId::GfmFile),
        Sources::default(),
    );
    assert_eq!(gfm.render.item_text(0).text, "A^[b].");
    assert_eq!(
        gfm.render.footnote_section_start(),
        gfm.render.items().len()
    );
}

#[test]
fn typing_an_inline_footnote_rebuilds_the_document() {
    let mut d = ob_unchecked("one\n\ntwo\n");
    let at = d.text.find("two").expect("two") + 3;
    let (_, report) = d.edit(at..at, "^[x]", Sources::default());
    assert!(report.full);
    let fresh = RenderTree::build(
        &d.tree,
        &d.model,
        d.text.as_str(),
        RenderOptions::builtin(ProfileId::Obsidian),
        Sources::default(),
    );
    assert_eq!(common::canonical(&d.render), common::canonical(&fresh));
    assert_eq!(d.render.item_text(1).text, "two1");
    // Removing it drops the section again.
    let (_, report) = d.edit(at..at + 4, "", Sources::default());
    assert!(report.full);
    assert_eq!(d.render.footnote_section_start(), d.render.items().len());
}

#[test]
fn markdown_images_take_a_size_from_their_alt_text() {
    let d = ob("![cat|60](a.png) ![dog | 30x20](https://x.test/b.png) ![plain|wide](c.png)\n");
    let c = d.render.nodes()[0].inline().expect("inline");
    let alts: Vec<&str> = c.images.iter().map(|i| &*i.alt).collect();
    // The size is not part of the alt text; a non-size suffix stays.
    assert_eq!(alts, ["cat", "dog", "plain|wide"]);
    let dump = d.render.dump();
    for size in ["60x30", "30x20", "200x100"] {
        assert!(dump.contains(size), "{size} in {dump}");
    }
    // GitHub reads the alt text as written.
    let gfm = common::build_with(
        "![cat|60](a.png)\n",
        ParseOptions::builtin(ProfileId::GfmFile),
        RenderOptions::builtin(ProfileId::GfmFile),
        Sources {
            links: &Vault::default(),
            images: &Images,
        },
    );
    let c = gfm.render.nodes()[0].inline().expect("inline");
    assert_eq!(&*c.images[0].alt, "cat|60");
    assert!(gfm.render.dump().contains("200x100"));
}
