// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Canvases written the way Obsidian writes them: the compact layout of
//! older versions (one item per line), the expanded layout of current ones
//! with every node type, keys from plugins, fractional coordinates and an
//! edge to a deleted node, and an empty file.

use gv_canvas::patch::{PathKey, apply_patches, path_refs, plan_path_patches};
use gv_canvas::{
    BackgroundStyle, Canvas, CanvasColor, CanvasView, End, FitOptions, IssueItem, IssueKind,
    ItemId, NodeKind, Preset, Severity, Side, Size, Subpath,
};

const COMPACT: &str = include_str!("fixtures/real/compact.canvas");
const FULL: &str = include_str!("fixtures/real/full.canvas");
const EMPTY: &str = include_str!("fixtures/real/empty.canvas");

fn parse(src: &str) -> Canvas {
    Canvas::parse(src).expect("fixture parses")
}

fn json(s: &str) -> serde_json::Value {
    serde_json::from_str(s).expect("json")
}

#[test]
fn compact_layout_reads_and_keeps_its_bytes_through_a_rename() {
    let c = parse(COMPACT);
    assert!(c.issues().is_empty(), "{:?}", c.issues());
    assert_eq!((c.nodes().len(), c.edges().len()), (5, 1));
    // Not Obsidian's current layout, so a rewrite is equal but not identical.
    assert_eq!(json(&c.to_json_string()), json(COMPACT));
    // A rename patches only the path's bytes.
    let patches = plan_path_patches(COMPACT, |r| {
        (r.value == "spec/1.0.md").then(|| "spec/v1.md".to_owned())
    })
    .expect("plan");
    assert_eq!(patches.len(), 1);
    let out = apply_patches(COMPACT, &patches).expect("apply");
    assert_eq!(out, COMPACT.replace("\"spec/1.0.md\"", "\"spec/v1.md\""));
    // The link in the text card is Markdown, not a path ref.
    assert!(out.contains("[Spec](spec/1.0.md)"));
}

#[test]
fn compact_layout_reading_order_and_edge() {
    let c = parse(COMPACT);
    let v = CanvasView::new(&c);
    let ids: Vec<&str> = v
        .reading_order()
        .iter()
        .map(|&n| c.nodes()[n].id.as_str())
        .collect();
    // Rows top to bottom, left to right; the group is not a card.
    assert_eq!(
        ids,
        [
            "c3b8f1a2d5e6970b",
            "e4d2a6c8b0f13579",
            "f19a3b5c7d2e4608",
            "a07c2e9d1f4b6830"
        ]
    );
    let e = &c.edges()[0];
    assert_eq!(
        (e.from_side, e.to_side),
        (Some(Side::Right), Some(Side::Left))
    );
    assert_eq!(e.to_end, End::Arrow);
    assert_eq!(e.from_end, End::None);
    assert_eq!(v.parent_group(2), Some(0), "the logo sits in the group");
}

#[test]
fn full_layout_round_trips_byte_for_byte() {
    let c = parse(FULL);
    assert_eq!(c.to_json_string(), FULL);
}

#[test]
fn full_layout_nodes() {
    let c = parse(FULL);
    let kinds: Vec<(&str, &str)> = c
        .nodes()
        .iter()
        .map(|n| {
            let k = match &n.kind {
                NodeKind::Text { .. } => "text",
                NodeKind::File { .. } => "file",
                NodeKind::Link { .. } => "link",
                NodeKind::Group { .. } => "group",
                NodeKind::Unknown { .. } => "unknown",
            };
            (n.id.as_str(), k)
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("grp-research", "group"),
            ("grp-unlabelled", "group"),
            ("txt-question", "text"),
            ("file-note", "file"),
            ("file-block", "file"),
            ("file-pdf", "file"),
            ("file-image", "file"),
            ("file-canvas", "file"),
            ("file-missing", "file"),
            ("link-wiki", "link"),
            ("portal-1", "unknown"),
        ]
    );
    let g = &c.nodes()[0];
    assert!((g.rect.x + 620.5).abs() < 1e-9, "fractional coordinates");
    assert_eq!(g.color, Some(CanvasColor::Preset(Preset::Cyan)));
    assert_eq!(
        g.kind,
        NodeKind::Group {
            label: Some("Research ☕".into()),
            background: Some("Attachments/paper.png".into()),
            background_style: Some(BackgroundStyle::Ratio),
        }
    );
    assert!(matches!(
        &c.nodes()[1].kind,
        NodeKind::Group { label: None, .. }
    ));
    match &c.nodes()[2].kind {
        NodeKind::Text { text } => {
            assert!(text.starts_with("## Question\r\n"), "CR LF kept");
            assert!(text.contains("[[Tides#Causes]]"));
        }
        other => panic!("text: {other:?}"),
    }
    assert!(matches!(
        c.nodes()[2].color,
        Some(CanvasColor::Rgba {
            r: 0xe0,
            g: 0xf2,
            b: 0xfe,
            a: 255
        })
    ));
    let subpath = |i: usize| match &c.nodes()[i].kind {
        NodeKind::File { subpath, .. } => subpath.clone(),
        other => panic!("file: {other:?}"),
    };
    assert_eq!(
        subpath(3),
        Some(Subpath::Heading(vec!["Causes".into(), "Moon".into()]))
    );
    assert_eq!(subpath(4), Some(Subpath::Block("a1b2c3".into())));
    // A PDF page subpath is not a heading; it is kept as written.
    match &c.nodes()[5].kind {
        NodeKind::File { subpath_raw, .. } => {
            assert_eq!(subpath_raw.as_deref(), Some("#page=3"));
        }
        other => panic!("file: {other:?}"),
    }
    assert!(matches!(
        &c.nodes()[9].kind,
        NodeKind::Link { url } if url == "https://en.wikipedia.org/wiki/Tide"
    ));
}

#[test]
fn full_layout_problems_are_per_item() {
    let c = parse(FULL);
    let issues: Vec<(IssueItem, Severity, IssueKind)> = c
        .issues()
        .iter()
        .map(|i| (i.item.clone(), i.severity, i.kind.clone()))
        .collect();
    assert_eq!(
        issues,
        [
            (
                IssueItem::Node {
                    index: 10,
                    id: Some("portal-1".into())
                },
                Severity::Warning,
                IssueKind::UnknownNodeType("portal".into())
            ),
            (
                IssueItem::Edge {
                    index: 4,
                    id: Some("e-dangling".into())
                },
                Severity::Error,
                IssueKind::DanglingEdge {
                    key: "toNode",
                    target: "node-that-was-deleted".into()
                }
            ),
        ]
    );
    // The dangling edge is dropped; the others are drawn.
    let edges: Vec<&str> = c.edges().iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        edges,
        ["e-question-note", "e-note-block", "e-auto", "e-to-group"]
    );
}

#[test]
fn full_layout_edges() {
    let c = parse(FULL);
    let e = &c.edges()[1];
    assert_eq!((e.from_end, e.to_end), (End::Arrow, End::None));
    assert_eq!(e.color, Some(CanvasColor::Preset(Preset::Green)));
    let auto = &c.edges()[2];
    assert_eq!((auto.from_side, auto.to_side), (None, None));
    assert_eq!(
        auto.color,
        Some(CanvasColor::Rgba {
            r: 0xff,
            g: 0x66,
            b: 0x00,
            a: 255
        })
    );
    assert_eq!(c.edges()[3].label.as_deref(), Some("context\nfor all"));
    let v = CanvasView::new(&c);
    // Every drawn edge has a curve with finite points.
    for i in 0..c.edges().len() {
        let g = v.edge_geometry(i);
        assert!(g.label_anchor.x.is_finite() && g.label_anchor.y.is_finite());
    }
}

#[test]
fn full_layout_unknown_keys_are_kept() {
    let c = parse(FULL);
    let root: Vec<&str> = c.unknown_root_keys().iter().map(|(k, _)| *k).collect();
    assert_eq!(root, ["metadata"]);
    let text = &c.nodes()[2];
    let keys: Vec<&str> = c.unknown_node_keys(text).iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, ["styleAttributes"]);
    let portal = &c.nodes()[10];
    let keys: Vec<&str> = c
        .unknown_node_keys(portal)
        .iter()
        .map(|(k, _)| *k)
        .collect();
    assert_eq!(keys, ["portalToFile"]);
    let auto = &c.edges()[2];
    assert_eq!(c.unknown_edge_keys(auto)[0].0, "styleAttributes");
}

#[test]
fn full_layout_rename_patches_files_and_backgrounds() {
    let refs = path_refs(FULL).expect("scan");
    let found: Vec<(PathKey, &str)> = refs.iter().map(|r| (r.key, r.value.as_str())).collect();
    assert_eq!(
        found,
        [
            (PathKey::Background, "Attachments/paper.png"),
            (PathKey::File, "Notes/Tides.md"),
            (PathKey::File, "Notes/Tides.md"),
            (PathKey::File, "Papers/Laplace 1775.pdf"),
            (PathKey::File, "Attachments/moon phases.png"),
            (PathKey::File, "Boards/Sub board.canvas"),
            (PathKey::File, "Gone/Deleted note.md"),
        ]
    );
    let patches = plan_path_patches(FULL, |r| {
        r.value
            .strip_prefix("Attachments/")
            .map(|rest| format!("Media/{rest}"))
    })
    .expect("plan");
    let out = apply_patches(FULL, &patches).expect("apply");
    assert_eq!(out, FULL.replace("\"Attachments/", "\"Media/"));
    // The plugin's `portalToFile` is not a JSON Canvas path; it is left.
    assert!(out.contains("\"portalToFile\": \"Boards/Sub board.canvas\""));
    assert_eq!(parse(&out).to_json_string(), out);
}

#[test]
fn full_layout_view_model() {
    let c = parse(FULL);
    let v = CanvasView::new(&c);
    let b = v.bounds().expect("bounds");
    assert!((b.x + 620.5).abs() < 1e-9);
    assert!((b.right() - 1100.0).abs() < 1e-9);
    // Groups are behind everything; the unknown node is still a card.
    assert!(matches!(v.paint_order()[0], ItemId::Node(0)));
    assert!(v.reading_order().contains(&10));
    assert!(!v.reading_order().contains(&0));
    assert_eq!(v.parent_group(3), Some(0));
    assert_eq!(v.parent_group(8), Some(1));
    let t = v.fit(Size::new(1200.0, 800.0), &FitOptions::default());
    assert!(t.zoom > 0.3 && t.zoom < 1.0, "{}", t.zoom);
}

#[test]
fn an_empty_file_is_an_empty_canvas() {
    for src in [EMPTY, "\n", " \t\r\n", "\u{feff}"] {
        let c = Canvas::parse(src).expect("empty canvas");
        assert!(c.nodes().is_empty() && c.edges().is_empty() && c.issues().is_empty());
        assert!(CanvasView::new(&c).bounds().is_none());
    }
    // Something that is not JSON still fails.
    assert!(Canvas::parse("nodes: []").is_err());
}
