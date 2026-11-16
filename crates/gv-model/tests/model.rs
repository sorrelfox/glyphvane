// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The document model on hand-written documents: headings, outline, links,
//! definitions, slugs and fragment matching.

use gv_model::{
    Anchoring, BlockMatch, DocModel, FragmentMatch, HugoIds, LinkKind, PandocIds, Projection,
    SlugAlgorithm,
};

fn model(src: &str) -> DocModel {
    let tree = gv_syntax::parse(src);
    DocModel::build(&tree, src)
}

fn slugs(m: &DocModel, alg: SlugAlgorithm) -> Vec<String> {
    m.slugs(alg).iter().map(|s| s.to_string()).collect()
}

const GITHUB: Anchoring = Anchoring::Slug(SlugAlgorithm::GitHub);

#[test]
fn headings_and_ranges() {
    let src = "# Foo *bar* `x` ##\n\n> Set\n> ext\n> ===\n\n  ## a ![img](x.png) <b>B</b> &amp; \\* [l](u)\n#\n";
    let m = model(src);
    let h = m.headings();
    assert_eq!(h.len(), 4);

    assert_eq!((h[0].level, h[0].setext, h[0].line), (1, false, 0));
    assert_eq!(&*h[0].text, "Foo bar x");
    assert_eq!(&*h[0].source, "Foo *bar* `x`");
    assert_eq!(&src[h[0].content_range.clone()], "Foo *bar* `x`");
    assert_eq!(&src[h[0].range.clone()], "# Foo *bar* `x` ##\n");

    assert_eq!(
        (h[1].level, h[1].setext, h[1].line, h[1].end_line),
        (1, true, 2, 4)
    );
    assert_eq!(h[1].container_depth, 1);
    assert_eq!(&*h[1].text, "Set\next");
    assert_eq!(h[1].projection(Projection::TextWithAlt), "Set ext");
    assert_eq!(&*h[1].source, "Set\next");
    assert_eq!(h[1].projection(Projection::SourceLastLine), "ext");
    assert_eq!(&src[h[1].range.clone()], "Set\n> ext\n> ===\n");

    assert_eq!(&*h[2].text, "a  B & * l");
    assert_eq!(h[2].projection(Projection::TextWithAlt), "a img B & * l");

    assert_eq!(&*h[3].text, "");
    assert!(h[3].content_range.is_empty());

    assert_eq!(
        slugs(&m, SlugAlgorithm::GitHub),
        ["foo-bar-x", "setext", "a--b---l", ""]
    );
    assert_eq!(
        slugs(&m, SlugAlgorithm::GitLab),
        ["foo-bar-x", "set-ext", "a-img-b---l", ""]
    );
    assert_eq!(
        slugs(&m, SlugAlgorithm::Pandoc(PandocIds::default())),
        ["foo-bar-x", "set-ext", "a-img-b-l", "section"]
    );
    assert_eq!(
        slugs(&m, SlugAlgorithm::PythonMarkdown { unicode: false }),
        ["foo-bar-x", "set-ext", "a-b-l", "_1"]
    );
    assert_eq!(
        slugs(&m, SlugAlgorithm::Kramdown)[2],
        "a-imgxpng-bbb-amp--lu"
    );
    assert_eq!(
        slugs(&m, SlugAlgorithm::Hugo(HugoIds::GitHub))[0],
        "foo-bar-x"
    );
}

#[test]
fn outline_and_sections() {
    let src = "# A\ntext\n### B\n## C\n# D\n#### E\n";
    let m = model(src);
    let parents: Vec<_> = m.outline().iter().map(|e| e.parent).collect();
    assert_eq!(parents, [None, Some(0), Some(0), None, Some(3)]);
    assert_eq!(m.outline_children(None).collect::<Vec<_>>(), [0, 3]);
    assert_eq!(m.outline_children(Some(0)).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(
        &src[m.section(0).expect("section")],
        "# A\ntext\n### B\n## C\n"
    );
    assert_eq!(&src[m.section(2).expect("section")], "## C\n");
    assert_eq!(&src[m.section(4).expect("section")], "#### E\n");
    assert_eq!(m.heading_at(5), Some(0));
    assert_eq!(m.heading_at(src.find("## C").expect("C") + 3), Some(2));
    assert!(m.section(9).is_none());
}

#[test]
fn links_and_definitions() {
    let src = "\
See [inline](a.md#Part%202 \"T\") and [full][Ref], [Ref][], [ref], [none].

<https://example.org/x> <me@example.org> [![logo](img.png)](b.md)

> [ref]: <my target.md> 'the title'
> [Ref]: second.md

# Heading [x](h.md)
";
    let m = model(src);
    let links = m.links();
    let kinds: Vec<_> = links
        .iter()
        .map(|l| (l.kind, l.image, &*l.destination))
        .collect();
    assert_eq!(
        kinds,
        [
            (LinkKind::Inline, false, "a.md#Part%202"),
            (LinkKind::Full, false, "my target.md"),
            (LinkKind::Collapsed, false, "my target.md"),
            (LinkKind::Shortcut, false, "my target.md"),
            (LinkKind::UriAutolink, false, "https://example.org/x"),
            (LinkKind::EmailAutolink, false, "mailto:me@example.org"),
            (LinkKind::Inline, false, "b.md"),
            (LinkKind::Inline, true, "img.png"),
            (LinkKind::Inline, false, "h.md"),
        ]
    );
    let first = &links[0];
    assert_eq!(&src[first.range.clone()], "[inline](a.md#Part%202 \"T\")");
    assert_eq!(&src[first.text_range.clone()], "inline");
    assert_eq!(
        &src[first.destination_range.clone().expect("raw")],
        "a.md#Part%202"
    );
    assert_eq!(first.title.as_deref(), Some("T"));
    assert_eq!(links[1].label.as_deref(), Some("ref"));
    assert_eq!(&src[links[1].label_range.clone().expect("label")], "Ref");
    assert!(links[1].destination_range.is_none());
    assert!(!links[0].in_heading && links[8].in_heading);

    // Hit testing: the image inside the link is innermost.
    let img = src.find("logo").expect("logo");
    assert!(m.link_at(img).expect("link").image);
    assert_eq!(&*m.link_at(img - 3).expect("link").destination, "b.md");
    assert!(m.link_at(src.find("See").expect("See")).is_none());

    let defs = m.definitions();
    assert_eq!(defs.len(), 2);
    assert_eq!(&*defs[0].label, "ref");
    assert_eq!(&src[defs[0].destination_range.clone()], "<my target.md>");
    assert_eq!(
        &src[defs[0].title_range.clone().expect("title")],
        "'the title'"
    );
    assert_eq!(&src[defs[0].label_range.clone()], "ref");
    assert_eq!(
        &src[defs[0].range.clone()],
        "[ref]: <my target.md> 'the title'"
    );
    assert!(defs[0].first);
    assert!(!defs[1].first);
    assert_eq!(defs[1].line, 5);
}

#[test]
fn fragments_github_profile() {
    let src = "# Intro\n## Café Θ\n## Dup\n## Dup\n## Dup 1\n# Top\n";
    let m = model(src);
    assert_eq!(
        slugs(&m, SlugAlgorithm::GitHub),
        ["intro", "café-θ", "dup", "dup-1", "dup-1-1", "top"]
    );
    let f = |s: &str| m.match_fragment(&[s], GITHUB);
    assert_eq!(
        f("intro"),
        FragmentMatch::Heading {
            index: 0,
            loose: false
        }
    );
    assert_eq!(
        f("user-content-intro"),
        FragmentMatch::Heading {
            index: 0,
            loose: false
        }
    );
    assert_eq!(
        f("café-θ"),
        FragmentMatch::Heading {
            index: 1,
            loose: false
        }
    );
    // NFD input matches the NFC slug.
    assert_eq!(
        f("cafe\u{301}-θ"),
        FragmentMatch::Heading {
            index: 1,
            loose: false
        }
    );
    assert_eq!(
        f("dup-1"),
        FragmentMatch::Heading {
            index: 3,
            loose: false
        }
    );
    assert_eq!(
        f("dup-1-1"),
        FragmentMatch::Heading {
            index: 4,
            loose: false
        }
    );
    // Heading text (Obsidian style) in a GitHub note: loose.
    assert_eq!(
        f("Café Θ"),
        FragmentMatch::Heading {
            index: 1,
            loose: true
        }
    );
    // Case differs: loose.
    assert_eq!(
        f("INTRO"),
        FragmentMatch::Heading {
            index: 0,
            loose: true
        }
    );
    assert_eq!(f("nope"), FragmentMatch::Missing);
    // A heading with the id `top` wins over the top of the document.
    assert_eq!(
        f("top"),
        FragmentMatch::Heading {
            index: 5,
            loose: false
        }
    );
    let m2 = model("# Intro\n");
    assert_eq!(m2.match_fragment(&["top"], GITHUB), FragmentMatch::Top);
    assert_eq!(m2.match_fragment(&["TOP"], GITHUB), FragmentMatch::Top);
}

#[test]
fn fragments_obsidian_profile() {
    let src = "# Part 1\n## Detail\n# Part 2\n## Detail\n### Some **bold** `code`: x\n";
    let m = model(src);
    let ob = Anchoring::HeadingText;
    let f = |parts: &[&str]| m.match_fragment(parts, ob);
    assert_eq!(
        f(&["Detail"]),
        FragmentMatch::Heading {
            index: 1,
            loose: false
        }
    );
    assert_eq!(
        f(&["Part 2", "Detail"]),
        FragmentMatch::Heading {
            index: 3,
            loose: false
        }
    );
    assert_eq!(
        f(&["part 2", "detail"]),
        FragmentMatch::Heading {
            index: 3,
            loose: false
        }
    );
    assert_eq!(
        f(&["Part 1", "Detail"]),
        FragmentMatch::Heading {
            index: 1,
            loose: false
        }
    );
    assert_eq!(f(&["Part 1", "Some bold code x"]), FragmentMatch::Missing);
    assert_eq!(
        f(&["Some bold code x"]),
        FragmentMatch::Heading {
            index: 4,
            loose: false
        }
    );
    assert_eq!(
        f(&["Some **bold** `code`: x"]),
        FragmentMatch::Heading {
            index: 4,
            loose: false
        }
    );
    // A GitHub slug in an Obsidian note: loose.
    assert_eq!(
        f(&["part-2"]),
        FragmentMatch::Heading {
            index: 2,
            loose: true
        }
    );
    assert_eq!(f(&["Part 3"]), FragmentMatch::Missing);
    // No block ids in the CommonMark profile.
    assert_eq!(m.match_block("abc"), BlockMatch::Missing);
}

#[test]
fn fragments_without_primary_anchors() {
    let m = model("# Intro\n");
    assert_eq!(
        m.match_fragment(&["intro"], Anchoring::None),
        FragmentMatch::Heading {
            index: 0,
            loose: true
        }
    );
}

#[test]
fn github_pipeline_on_hugo_cases() {
    // Hugo's TestSanitizeAnchorName cases (Apache-2.0), "generated manually
    // on github.com", through the whole pipeline (parse, project, slug).
    let cases: &[(&str, &str)] = &[
        ("God is good: 神真美好", "god-is-good-神真美好"),
        ("Number 32", "number-32"),
        ("Question?", "question"),
        ("1+2=3", "123"),
        ("Special !\"#$%&(parens)=?´* chars", "special-parens-chars"),
        ("Resumé", "resumé"),
        ("One-Hyphen", "one-hyphen"),
        ("Multiple--Hyphens", "multiple--hyphens"),
        ("Trailing hyphen-", "trailing-hyphen-"),
        ("Many   spaces  here", "many---spaces--here"),
        ("Forward/slash", "forwardslash"),
        ("Backward\\slash", "backwardslash"),
        ("Under_score", "under_score"),
        ("Nonbreaking\u{a0}Space", "nonbreakingspace"),
        ("Tab\tSpace", "tabspace"),
        ("Trailing Space ", "trailing-space"),
    ];
    for &(heading, want) in cases {
        let m = model(&format!("# {heading}\n"));
        assert_eq!(&*m.slugs(SlugAlgorithm::GitHub)[0], want, "{heading:?}");
    }
}

#[test]
fn reference_links_follow_definitions() {
    // A shortcut reference with no definition is text.
    let m = model("[foo]\n");
    assert!(m.links().is_empty());
    let m = model("[foo]\n\n[foo]: /url\n");
    assert_eq!(m.links().len(), 1);
    assert_eq!(&*m.links()[0].destination, "/url");
}

#[test]
fn empty_and_tiny_documents() {
    for src in [
        "",
        "\n",
        "#",
        "[",
        "<",
        "[a]:",
        "> #\n>",
        "- # x\n  y\n  ===",
    ] {
        let m = model(src);
        assert_eq!(m.len(), src.len());
        assert_eq!(m.outline().len(), m.headings().len());
        for alg in SlugAlgorithm::ALL {
            assert_eq!(m.slugs(alg).len(), m.headings().len());
        }
    }
}

fn model_with(profile: &str, src: &str) -> DocModel {
    let id = gv_syntax::ProfileId::from_id(profile).expect("built-in profile");
    let options = gv_syntax::ParseOptions::builtin(id);
    DocModel::build(&gv_syntax::parse_with(src, options), src)
}

#[test]
fn front_matter() {
    let src = "---\ntitle: \"# x\"\nlink: \"[a](b.md)\"\n---\n# Heading\n\n[c](d.md)\n";
    let m = model_with("gfm-file", src);
    let fm = m.front_matter().expect("front matter");
    assert_eq!(fm.kind, gv_model::FrontMatterKind::Yaml);
    assert_eq!(
        &src[fm.range.clone()],
        "---\ntitle: \"# x\"\nlink: \"[a](b.md)\"\n---\n"
    );
    assert_eq!(
        &src[fm.content_range.clone()],
        "title: \"# x\"\nlink: \"[a](b.md)\"\n"
    );
    // Nothing from inside it: one heading, one link, after it.
    assert_eq!(m.headings().len(), 1);
    assert_eq!(
        (&*m.headings()[0].text, m.headings()[0].line),
        ("Heading", 4)
    );
    assert_eq!(m.links().len(), 1);
    assert_eq!(&*m.links()[0].destination, "d.md");

    // CommonMark has none: a thematic break, then a setext heading.
    let cm = model(src);
    assert!(cm.front_matter().is_none());
    assert_eq!(
        &*cm.headings()[0].source,
        "title: \"# x\"\nlink: \"[a](b.md)\""
    );

    // TOML in the extended profile only; `...` closes YAML; empty content.
    let toml = "+++\na = 1\n+++\ntext\n";
    let fm = model_with("extended", toml).front_matter().expect("toml");
    assert_eq!(fm.kind, gv_model::FrontMatterKind::Toml);
    assert_eq!(&toml[fm.content_range.clone()], "a = 1\n");
    assert!(model_with("obsidian", toml).front_matter().is_none());
    let dots = "---\na: b\n...\n";
    let fm = model_with("obsidian", dots).front_matter().expect("dots");
    assert_eq!(fm.range, 0..dots.len());
    let empty = "---\n---\n# H\n";
    let fm = model_with("gfm-file", empty).front_matter().expect("empty");
    assert_eq!((fm.range.clone(), fm.content_range.clone()), (0..8, 4..4));
    assert_eq!(model_with("gfm-file", empty).headings().len(), 1);

    // Unclosed front matter is not front matter: a thematic break here,
    // and its links count.
    let open = "---\ntitle: [x](y)\n";
    let m = model_with("gfm-file", open);
    assert!(m.front_matter().is_none());
    assert_eq!(&*m.links()[0].destination, "y");
}

#[test]
fn nul_in_heading_text_is_replaced() {
    let m = model("# a\0b &#0;\n");
    assert_eq!(&*m.headings()[0].text, "a\u{fffd}b \u{fffd}");
}

#[test]
fn obsidian_wikilinks_tags_and_block_ids() {
    let src = "# Head #h\n\nSee [[Note#Sec|alias]] and ![[img.png|20]] #a/b ^p1\n\n- item ^i1\n\n^own\n\n| [[c\\|d]] |\n| -- |\n";
    let tree = gv_syntax::SyntaxTree::parse_with(
        src,
        gv_syntax::ParseOptions::builtin(gv_syntax::ProfileId::Obsidian),
    );
    let m = DocModel::build(&tree, src);
    let links: Vec<(LinkKind, &str, &str)> = m
        .links()
        .iter()
        .map(|l| (l.kind, &*l.destination, &src[l.text_range.clone()]))
        .collect();
    assert_eq!(
        links,
        [
            (LinkKind::Wikilink, "Note#Sec|alias", "alias"),
            (LinkKind::Embed, "img.png|20", "20"),
        ]
    );
    assert_eq!(m.links()[0].range, 15..33);
    let tags: Vec<(&str, u32)> = m.tags().iter().map(|t| (&*t.name, t.line)).collect();
    assert_eq!(tags, [("h", 0), ("a/b", 2)]);
    let anchors: Vec<(&str, bool, &str)> = m
        .block_anchors()
        .iter()
        .map(|a| (&*a.id, a.own_line, &src[a.id_range.clone()]))
        .collect();
    assert_eq!(
        anchors,
        [
            ("p1", false, "^p1"),
            ("i1", false, "^i1"),
            ("own", true, "^own")
        ]
    );
    let m2 = DocModel::build(&gv_syntax::SyntaxTree::parse(src), src);
    assert!(m2.tags().is_empty() && m2.block_anchors().is_empty());
    assert!(m2.links().iter().all(|l| !l.kind.is_wiki()));
}
