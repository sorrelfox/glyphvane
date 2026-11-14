// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Parser → document model → resolver → fragment matching, on a small
//! vault of real Markdown files: links come from the model
//! ([`links_from_model`]) and fragments are matched in the target's model
//! ([`ModelFragments`]).

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use gv_model::DocModel;
use gv_vault::{
    FragmentTarget, IndexConfig, LinkKind, ModelFragments, PathIndex, ProfileId, ProfileParams,
    Status, VaultPath, links_from_model, resolve,
};

fn parse(text: &str) -> DocModel {
    DocModel::build(&gv_syntax::parse(text), text)
}

fn write_vault(root: &Path, files: &[(&str, &str)]) {
    for (p, t) in files {
        let path = root.join(p);
        fs::create_dir_all(path.parent().expect("test setup")).expect("test setup");
        fs::write(path, t).expect("test setup");
    }
}

/// Loads target models from disk once each (Markdown files only).
fn loader(root: &Path) -> impl Fn(&VaultPath) -> Option<DocModel> + '_ {
    let cache: RefCell<HashMap<VaultPath, DocModel>> = RefCell::new(HashMap::new());
    move |p: &VaultPath| {
        if !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("md")) {
            return None;
        }
        if let Some(m) = cache.borrow().get(p) {
            return Some(m.clone());
        }
        let text = fs::read_to_string(p.to_fs_path(root)).ok()?;
        let m = parse(&text);
        cache.borrow_mut().insert(p.clone(), m.clone());
        Some(m)
    }
}

/// One link of a source note, resolved.
#[derive(Debug)]
struct Resolved {
    /// The link as written.
    original: String,
    kind: LinkKind,
    status: Status,
    target: Option<String>,
    fragment: Option<FragmentTarget>,
}

/// Every link of `source` with its status, target and fragment target.
fn resolve_all(root: &Path, source: &str, profile: ProfileId) -> Vec<Resolved> {
    let (idx, _) = PathIndex::build(root, IndexConfig::default()).expect("test setup");
    let params = ProfileParams::for_profile(profile);
    let fragments = ModelFragments::new(loader(root));
    let src = VaultPath::new(source).expect("test setup");
    let text = fs::read_to_string(src.to_fs_path(root)).expect("test setup");
    let model = parse(&text);
    let refs = links_from_model(&model, &src, profile);
    assert_eq!(refs.len(), model.links().len());
    model
        .links()
        .iter()
        .zip(refs)
        .map(|(l, link)| {
            let r = resolve(&idx, &link, &params);
            Resolved {
                original: text[l.range.clone()].to_owned(),
                kind: link.kind,
                status: r.status,
                target: r.target_path.as_ref().map(|p| p.as_str().to_owned()),
                fragment: r.resolve_fragment(&fragments, &params),
            }
        })
        .collect()
}

fn heading(text: &str, line: u32, loose: bool) -> Option<FragmentTarget> {
    Some(FragmentTarget::Heading {
        text: text.to_owned(),
        line,
        loose,
    })
}

#[test]
fn github_relative_links_and_slugs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_vault(
        root,
        &[
            (
                "docs/guide.md",
                "# Guide\n\n## Getting started\n\n## FAQ\n### Why?\n\n## FAQ\n",
            ),
            (
                "docs/README.md",
                "Intro\n=====\n\
                 [start](guide.md#getting-started) [faq2](./guide.md#faq-1) \
                 [why](guide.md#why) [up](../README.md#top) [ref][g] \
                 [case](guide.md#Getting-Started) [gone](guide.md#nope) \
                 [self](#intro) [lines](guide.md#L2-L4) ![logo](img/logo.png) \
                 <https://example.com> [missing](nothere.md)\n\n\
                 [g]: <guide.md#faq>\n",
            ),
            ("README.md", "# Home\n"),
            ("docs/img/logo.png", ""),
        ],
    );
    let got = resolve_all(root, "docs/README.md", ProfileId::GfmFile);
    let by = |orig: &str| {
        got.iter()
            .find(|g| g.original == orig)
            .unwrap_or_else(|| panic!("no link {orig} in {got:#?}"))
    };
    let start = by("[start](guide.md#getting-started)");
    assert_eq!(start.kind, LinkKind::Inline);
    assert_eq!(start.status, Status::Exact);
    assert_eq!(start.target.as_deref(), Some("docs/guide.md"));
    assert_eq!(start.fragment, heading("Getting started", 2, false));
    assert_eq!(
        by("[faq2](./guide.md#faq-1)").fragment,
        heading("FAQ", 7, false)
    );
    assert_eq!(
        by("[why](guide.md#why)").fragment,
        heading("Why?", 5, false)
    );
    let up = by("[up](../README.md#top)");
    assert_eq!(up.target.as_deref(), Some("README.md"));
    assert_eq!(up.fragment, Some(FragmentTarget::Top));
    let r = by("[ref][g]");
    assert_eq!(r.kind, LinkKind::Reference);
    assert_eq!(r.fragment, heading("FAQ", 4, false));
    assert_eq!(
        by("[case](guide.md#Getting-Started)").fragment,
        heading("Getting started", 2, true)
    );
    assert_eq!(
        by("[gone](guide.md#nope)").fragment,
        Some(FragmentTarget::Missing)
    );
    let me = by("[self](#intro)");
    assert_eq!(me.target.as_deref(), Some("docs/README.md"));
    assert_eq!(me.fragment, heading("Intro", 0, false));
    assert_eq!(
        by("[lines](guide.md#L2-L4)").fragment,
        Some(FragmentTarget::Lines { start: 2, end: 4 })
    );
    let img = by("![logo](img/logo.png)");
    assert_eq!(img.kind, LinkKind::Image);
    assert_eq!(img.target.as_deref(), Some("docs/img/logo.png"));
    assert_eq!(by("<https://example.com>").status, Status::External);
    assert_eq!(by("[missing](nothere.md)").status, Status::Unresolved);
}

#[test]
fn obsidian_heading_text_through_markdown_links() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_vault(
        root,
        &[
            (
                "Notes/Topic.md",
                "# Topic\n## Part 1\n### Detail\n## Part 2\n### Detail\n## Time: 10:30\n",
            ),
            (
                "Notes/Source.md",
                "[a](Topic.md#Part%202#Detail) [b](<Topic.md#Time 10 30>) \
                 [c](Notes/Topic.md#part%201) [d](Topic.md#part-2) [e](Topic.md#Nope) \
                 [f](Topic.md#^blk)\n",
            ),
        ],
    );
    let got = resolve_all(root, "Notes/Source.md", ProfileId::Obsidian);
    let frags: Vec<Option<FragmentTarget>> = got.iter().map(|g| g.fragment.clone()).collect();
    assert!(
        got.iter()
            .all(|g| g.target.as_deref() == Some("Notes/Topic.md")),
        "{got:#?}"
    );
    assert_eq!(
        frags,
        [
            heading("Detail", 4, false),
            heading("Time: 10:30", 5, false),
            heading("Part 1", 1, false),
            // A GitHub slug is a loose match in Obsidian.
            heading("Part 2", 3, true),
            Some(FragmentTarget::Missing),
            // The CommonMark model has no block ids yet.
            Some(FragmentTarget::Missing),
        ]
    );
}
