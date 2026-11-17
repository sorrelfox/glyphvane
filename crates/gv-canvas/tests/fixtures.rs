// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Fixture tests: lossless round trips, per-item validation, the view model
//! and rename patches.

use gv_canvas::patch::{PathKey, apply_patches, path_refs, plan_path_patches};
use gv_canvas::{
    BackgroundStyle, Canvas, CanvasColor, CanvasView, Detail, Direction, End, FitOptions,
    IssueItem, IssueKind, ItemId, LodThresholds, NodeKind, Point, Preset, Rect, Severity, Side,
    Size, Subpath,
};

const OBSIDIAN: &str = include_str!("fixtures/obsidian.canvas");
const BROKEN: &str = include_str!("fixtures/broken.canvas");

fn parse(src: &str) -> Canvas {
    Canvas::parse(src).expect("fixture parses")
}

#[test]
fn obsidian_round_trip_is_byte_identical() {
    let c = parse(OBSIDIAN);
    assert!(c.issues().is_empty(), "{:?}", c.issues());
    assert_eq!(c.to_json_string(), OBSIDIAN);
}

#[test]
fn broken_round_trip_keeps_everything() {
    let c = parse(BROKEN);
    let before: serde_json::Value = serde_json::from_str(BROKEN).expect("json");
    let after: serde_json::Value = serde_json::from_str(&c.to_json_string()).expect("json");
    assert_eq!(before, after);
    let compact: serde_json::Value = serde_json::from_str(&c.to_json_compact()).expect("json");
    assert_eq!(before, compact);
}

#[test]
fn node_types_and_fields() {
    let c = parse(OBSIDIAN);
    let ids: Vec<&str> = c.nodes().iter().map(|n| n.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "g-outer", "t-intro", "g-inner", "f-note", "f-block", "l-site", "f-img"
        ]
    );
    let n = &c.nodes()[0];
    assert_eq!(n.rect, Rect::new(-400.0, -300.0, 1400.0, 900.0));
    assert_eq!(
        n.kind,
        NodeKind::Group {
            label: Some("Project".into()),
            background: Some("assets/paper.png".into()),
            background_style: Some(BackgroundStyle::Repeat),
        }
    );
    assert_eq!(c.nodes()[1].color, Some(CanvasColor::Preset(Preset::Green)));
    assert!(matches!(&c.nodes()[1].kind, NodeKind::Text { text } if text.starts_with("# Intro\n")));
    assert_eq!(
        c.nodes()[2].color,
        Some(CanvasColor::Rgba {
            r: 0x8a,
            g: 0x2b,
            b: 0xe2,
            a: 255
        })
    );
    match &c.nodes()[3].kind {
        NodeKind::File {
            file,
            subpath,
            subpath_raw,
        } => {
            assert_eq!(file, "Notes/Plan.md");
            assert_eq!(
                subpath,
                &Some(Subpath::Heading(vec!["Goals".into(), "Q4".into()]))
            );
            assert_eq!(subpath_raw.as_deref(), Some("#Goals#Q4"));
        }
        other => panic!("not a file node: {other:?}"),
    }
    assert!(matches!(
        &c.nodes()[4].kind,
        NodeKind::File { subpath: Some(Subpath::Block(b)), file, .. } if b == "abc123" && file == "Notes/Log é.md"
    ));
    assert_eq!(
        c.nodes()[5].kind,
        NodeKind::Link {
            url: "https://jsoncanvas.org/".into()
        }
    );
}

#[test]
fn edges_and_defaults() {
    let c = parse(OBSIDIAN);
    let e = c.edges();
    assert_eq!(e.len(), 4);
    assert_eq!((e[0].from, e[0].to), (1, 3));
    assert_eq!(e[0].from_side, Some(Side::Right));
    assert_eq!(e[0].label.as_deref(), Some("plans"));
    assert_eq!(e[0].color, Some(CanvasColor::Preset(Preset::Orange)));
    assert_eq!((e[0].from_end, e[0].to_end), (End::None, End::Arrow));
    assert_eq!((e[1].from_end, e[1].to_end), (End::Arrow, End::None));
    assert_eq!(e[2].from_side, None);
    assert_eq!(
        e[2].color,
        Some(CanvasColor::Rgba {
            r: 0,
            g: 255,
            b: 0,
            a: 255
        })
    );
}

#[test]
fn unknown_keys_are_reachable() {
    let c = parse(OBSIDIAN);
    let keys: Vec<&str> = c.unknown_root_keys().iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, ["metadata"]);
    let note = &c.nodes()[3];
    let extra = c.unknown_node_keys(note);
    assert_eq!(extra.len(), 1);
    assert_eq!(extra[0].0, "styleAttributes");
    assert_eq!(extra[0].1["border"], "dashed");
    assert!(c.unknown_node_keys(&c.nodes()[0]).is_empty());
    let e3 = &c.edges()[2];
    assert_eq!(c.unknown_edge_keys(e3)[0].0, "styleAttributes");
    // Key order inside an object is the file's.
    let order: Vec<&str> = c
        .node_json(1)
        .expect("node 1")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        order,
        ["id", "type", "text", "x", "y", "width", "height", "color"]
    );
}

fn has(c: &Canvas, item: &IssueItem, sev: Severity, kind: &IssueKind) -> bool {
    c.issues()
        .iter()
        .any(|i| &i.item == item && i.severity == sev && &i.kind == kind)
}

fn node(index: usize, id: &str) -> IssueItem {
    IssueItem::Node {
        index,
        id: Some(id.into()),
    }
}

#[test]
fn per_item_validation() {
    let c = parse(BROKEN);
    for i in c.issues() {
        // Every issue renders as a readable message.
        assert!(!i.to_string().is_empty());
    }
    let kept: Vec<(usize, &str)> = c.nodes().iter().map(|n| (n.index, n.id.as_str())).collect();
    assert_eq!(
        kept,
        [
            (0, "ok"),
            (4, "bad-colour"),
            (6, "future"),
            (7, "ok"),
            (9, "grp")
        ]
    );
    use IssueKind as K;
    use Severity::{Error, Warning};
    assert!(has(&c, &node(1, "no-x"), Error, &K::MissingKey("x")));
    assert!(has(
        &c,
        &IssueItem::Node { index: 2, id: None },
        Error,
        &K::NotAnObject
    ));
    assert!(has(&c, &node(3, "neg"), Error, &K::NonPositiveSize));
    assert!(has(
        &c,
        &node(4, "bad-colour"),
        Warning,
        &K::InvalidValue {
            key: "color",
            value: "\"9\"".into()
        }
    ));
    assert!(has(&c, &node(5, "no-file"), Error, &K::MissingKey("file")));
    assert!(has(
        &c,
        &node(6, "future"),
        Warning,
        &K::UnknownNodeType("sticker".into())
    ));
    assert!(has(
        &c,
        &node(7, "ok"),
        Warning,
        &K::DuplicateId("ok".into())
    ));
    assert!(has(&c, &node(7, "ok"), Warning, &K::MissingKey("text")));
    assert!(has(&c, &node(8, "big"), Error, &K::OutOfRange("x")));
    assert!(has(
        &c,
        &node(9, "grp"),
        Warning,
        &K::WrongType {
            key: "label",
            expected: "a string"
        }
    ));
    match &c.nodes()[4].kind {
        NodeKind::Group {
            label,
            background_style,
            ..
        } => assert!(label.is_none() && background_style.is_none()),
        other => panic!("not a group: {other:?}"),
    }

    // Edges: one survives with defaults in place of bad values.
    assert_eq!(c.edges().len(), 1);
    let e = &c.edges()[0];
    assert_eq!(e.id, "e-ok");
    assert_eq!((e.from_side, e.to_end), (None, End::Arrow));
    // Duplicate ids resolve to the first node.
    assert_eq!(e.from, 0);
    let edge = |index, id: Option<&str>| IssueItem::Edge {
        index,
        id: id.map(str::to_owned),
    };
    assert!(has(
        &c,
        &edge(1, Some("e-dangling")),
        Error,
        &K::DanglingEdge {
            key: "toNode",
            target: "no-file".into()
        }
    ));
    assert!(has(&c, &edge(2, None), Error, &K::MissingKey("id")));
    assert!(has(&c, &edge(3, None), Error, &K::NotAnObject));
}

#[test]
fn paint_order_puts_groups_behind() {
    let c = parse(OBSIDIAN);
    let v = CanvasView::new(&c);
    use ItemId::{Edge, Node};
    assert_eq!(
        v.paint_order(),
        [
            Node(0),
            Node(2),
            Edge(0),
            Edge(1),
            Edge(2),
            Edge(3),
            Node(1),
            Node(3),
            Node(4),
            Node(5),
            Node(6)
        ]
    );
    assert_eq!(v.parent_group(3), Some(2));
    assert_eq!(v.parent_group(1), Some(0));
    assert_eq!(v.parent_group(2), Some(0));
    assert_eq!(v.parent_group(0), None);
}

#[test]
fn bounds_fit_and_culling() {
    let c = parse(OBSIDIAN);
    let v = CanvasView::new(&c);
    let b = v.bounds().expect("non-empty");
    assert_eq!(b, Rect::new(-400.0, -300.0, 1400.0, 900.0));
    let t = v.fit(Size::new(1480.0, 980.0), &FitOptions::default());
    assert!((t.zoom - 1.0).abs() < 1e-9);
    assert_eq!(t.world_to_screen(b.center()), Point::new(740.0, 490.0));
    assert_eq!(
        v.visible(&Rect::new(-400.0, -300.0, 100.0, 100.0)),
        [ItemId::Node(0), ItemId::Node(1)]
    );
    assert!(v.visible(&Rect::new(5000.0, 5000.0, 10.0, 10.0)).is_empty());
    assert_eq!(v.visible(&b).len(), v.paint_order().len());

    let empty = CanvasView::new(&Canvas::empty());
    assert!(empty.bounds().is_none());
    assert_eq!(
        empty
            .fit(Size::new(100.0, 100.0), &FitOptions::default())
            .zoom,
        1.0
    );
}

#[test]
fn hit_testing() {
    let c = parse(OBSIDIAN);
    let v = CanvasView::new(&c);
    assert_eq!(
        v.hit_test(Point::new(390.0, -130.0), 0.0),
        Some(ItemId::Node(3))
    );
    assert_eq!(
        v.hit_test(Point::new(220.0, 100.0), 0.0),
        Some(ItemId::Node(2))
    );
    assert_eq!(
        v.hit_test(Point::new(-380.0, -280.0), 0.0),
        Some(ItemId::Node(0))
    );
    assert_eq!(v.hit_test(Point::new(3000.0, 0.0), 4.0), None);
    let mid = v.edge_geometry(0).label_anchor;
    assert_eq!(v.hit_test(mid, 2.0), Some(ItemId::Edge(0)));
    // Tolerance grows node borders.
    assert_eq!(
        v.hit_test(Point::new(42.0, -200.0), 0.0),
        Some(ItemId::Node(0))
    );
    assert_eq!(
        v.hit_test(Point::new(42.0, -200.0), 3.0),
        Some(ItemId::Node(1))
    );
}

#[test]
fn edge_geometry_from_file() {
    let c = parse(OBSIDIAN);
    let v = CanvasView::new(&c);
    let g = v.edge_geometry(0);
    assert_eq!(g.from, Point::new(40.0, -140.0));
    assert_eq!(g.to, Point::new(240.0, -130.0));
    assert!(g.to_arrow.is_some() && g.from_arrow.is_none());
    // Missing sides are picked from the layout: t-intro is above l-site.
    let g3 = v.edge_geometry(2);
    assert_eq!((g3.from_side, g3.to_side), (Side::Bottom, Side::Top));
    let g2 = v.edge_geometry(1);
    assert!(g2.from_arrow.is_some() && g2.to_arrow.is_none());
}

#[test]
fn level_of_detail() {
    let c = parse(OBSIDIAN);
    let r = c.nodes()[1].rect;
    let lod = LodThresholds::default();
    assert_eq!(lod.detail(&r, 1.0), Detail::Full);
    assert_eq!(lod.detail(&r, 0.2), Detail::Blocks);
    assert_eq!(lod.detail(&r, 0.001), Detail::Hidden);
    assert_eq!(
        lod.detail(&Rect::new(0.0, 0.0, 400.0, 10.0), 1.0),
        Detail::Blocks
    );
    assert_eq!(LodThresholds::SHAPES_ONLY.detail(&r, 1.0), Detail::Blocks);
    assert_eq!(lod.edge_detail(0.1), Detail::Blocks);
    assert_eq!(lod.edge_detail(1.0), Detail::Full);
}

#[test]
fn navigation() {
    let c = parse(OBSIDIAN);
    let v = CanvasView::new(&c);
    assert_eq!(v.reading_order(), [1, 3, 4, 5, 6]);
    assert_eq!(v.next_node(None), Some(1));
    assert_eq!(v.next_node(Some(4)), Some(5));
    assert_eq!(v.next_node(Some(6)), Some(1));
    assert_eq!(v.prev_node(Some(1)), Some(6));
    assert_eq!(v.prev_node(None), Some(6));
    // A group is not in reading order: start from the beginning.
    assert_eq!(v.next_node(Some(0)), Some(1));
    assert_eq!(v.neighbor(1, Direction::Right), Some(3));
    assert_eq!(v.neighbor(1, Direction::Down), Some(5));
    assert_eq!(v.neighbor(4, Direction::Right), None);
    assert_eq!(v.neighbor(6, Direction::Up), Some(3));
    let vp = Size::new(800.0, 600.0);
    let t = v.focus_node(3, 2.0, vp, &FitOptions::default());
    assert_eq!(
        t.world_to_screen(v.node_rect(3).center()),
        Point::new(400.0, 300.0)
    );
    assert!(t.zoom <= 1.0 + 1e-9);
}

#[test]
fn rename_patch_on_fixture() {
    let refs = path_refs(OBSIDIAN).expect("scan");
    let files: Vec<(&str, PathKey)> = refs.iter().map(|r| (r.value.as_str(), r.key)).collect();
    assert_eq!(
        files,
        [
            ("assets/paper.png", PathKey::Background),
            ("Notes/Plan.md", PathKey::File),
            ("Notes/Log é.md", PathKey::File),
            ("assets/diagram.png", PathKey::File),
        ]
    );
    let patches = plan_path_patches(OBSIDIAN, |r| {
        r.value
            .strip_prefix("Notes/")
            .map(|rest| format!("Archive/{rest}"))
    })
    .expect("plan");
    assert_eq!(patches.len(), 2);
    let out = apply_patches(OBSIDIAN, &patches).expect("apply");
    let expected = OBSIDIAN
        .replace("\"Notes/Plan.md\"", "\"Archive/Plan.md\"")
        .replace("\"Notes/Log é.md\"", "\"Archive/Log é.md\"");
    assert_eq!(out, expected);
    let c = parse(&out);
    assert!(c.issues().is_empty());
    // Everything else, including formatting, is untouched.
    assert_eq!(c.to_json_string(), out);
}

/// Grid culling and hit-testing agree with brute force on a larger canvas.
#[test]
fn grid_matches_brute_force() {
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut rnd = |m: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % m) as i64
    };
    let mut nodes = Vec::new();
    for i in 0..600 {
        let (x, y) = (rnd(20_000) - 10_000, rnd(20_000) - 10_000);
        let (w, h) = (rnd(800) + 20, rnd(600) + 20);
        let ty = if i % 50 == 0 { "group" } else { "text" };
        nodes.push(serde_json::json!({
            "id": format!("n{i}"), "type": ty, "text": "", "x": x, "y": y, "width": w, "height": h
        }));
    }
    // One huge node lands in the grid's "large" list.
    nodes.push(serde_json::json!({
        "id": "huge", "type": "group", "x": -10_000, "y": -10_000, "width": 20_000, "height": 20_000
    }));
    let mut edges = Vec::new();
    for i in 0..300 {
        edges.push(serde_json::json!({
            "id": format!("e{i}"), "fromNode": format!("n{}", rnd(600)), "toNode": format!("n{}", rnd(600))
        }));
    }
    let src = serde_json::json!({ "nodes": nodes, "edges": edges }).to_string();
    let c = parse(&src);
    assert!(c.issues().is_empty());
    let v = CanvasView::new(&c);
    for _ in 0..50 {
        let area = Rect::new(
            (rnd(24_000) - 12_000) as f64,
            (rnd(24_000) - 12_000) as f64,
            rnd(3000) as f64,
            rnd(3000) as f64,
        );
        let got = v.visible(&area);
        let want: Vec<ItemId> = v
            .paint_order()
            .iter()
            .copied()
            .filter(|&it| v.item_bounds(it).intersects(&area))
            .collect();
        assert_eq!(got, want);

        let p = Point::new(area.x, area.y);
        let brute = v.paint_order().iter().rev().copied().find(|&it| match it {
            ItemId::Node(i) => v.node_rect(i).contains(p),
            ItemId::Edge(i) => v.edge_geometry(i).distance_to(p) <= 0.0,
        });
        assert_eq!(v.hit_test(p, 0.0), brute);
    }
}
