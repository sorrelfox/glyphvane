// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Engine behaviour: caps, cards, cancellation, cache, host theme lock and
//! malformed input.

use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use gv_diagram::{
    CardKind, DiagramEngine, DiagramFonts, DiagramKey, Limits, Outcome, Paint, Trust,
};

fn fonts() -> DiagramFonts {
    let face = |name: &str| {
        ratex_katex_fonts::ttf_bytes(name)
            .unwrap_or_else(|| panic!("{name} is bundled"))
            .into_owned()
    };
    DiagramFonts {
        family: "KaTeX_SansSerif".into(),
        faces: vec![face("KaTeX_SansSerif-Regular.ttf")],
    }
}

fn engine() -> &'static DiagramEngine {
    static ENGINE: OnceLock<DiagramEngine> = OnceLock::new();
    ENGINE.get_or_init(|| DiagramEngine::new(fonts(), Limits::default()).expect("engine"))
}

fn card(o: &Outcome) -> &gv_diagram::FallbackCard {
    match o {
        Outcome::Card(c) => c,
        other => panic!("expected a card, got {other:?}"),
    }
}

#[test]
fn oversized_source_gets_a_too_large_card_and_render_anyway_works() {
    let mut src = String::from("flowchart LR\n");
    let mut i = 0;
    while src.len() <= 100 << 10 {
        let _ = writeln!(
            src,
            "  %% padding comment line number {i} to grow the source"
        );
        i += 1;
    }
    src.push_str("  A --> B\n");
    let o = engine().render(&src, Trust::Interactive);
    let c = card(&o);
    assert_eq!(c.kind, CardKind::TooLarge);
    assert!(c.render_anyway);
    assert!(c.message.contains("KB"), "{}", c.message);
    assert_eq!(&*c.source, src.as_str());

    // Render anyway: the trusted caps allow it.
    match engine().render(&src, Trust::Trusted) {
        Outcome::Rendered(d) => assert!(!d.items.is_empty()),
        other => panic!("trusted render: {other:?}"),
    }
}

#[test]
fn too_many_edges_gets_a_too_large_card() {
    let mut src = String::from("flowchart LR\n");
    for i in 0..520 {
        let _ = writeln!(src, "  n{i} --> n{}", i + 1);
    }
    let o = engine().render(&src, Trust::Interactive);
    let c = card(&o);
    assert_eq!(c.kind, CardKind::TooLarge, "{}", c.message);
    assert!(c.render_anyway);
}

#[test]
fn parse_errors_get_an_error_card_with_a_position() {
    let src = "sequenceDiagram\n  Alice->>Bob: hi\n  Bob-->>: \n  ??? !!!\n";
    let o = engine().render(src, Trust::Interactive);
    let c = card(&o);
    assert_eq!(c.kind, CardKind::Error);
    assert_eq!(c.title(), "Diagram error");
    assert!(!c.message.is_empty());
    assert!(!c.message.contains('\n'));
    if let Some((line, col)) = c.line_col {
        assert!(line >= 1 && col >= 1);
    }
}

#[test]
fn unknown_types_are_not_recognised_without_a_card() {
    for src in ["", "   \n", "not a diagram at all", "plantuml\nA -> B"] {
        let o = engine().render(src, Trust::Interactive);
        assert!(matches!(o, Outcome::NotRecognised), "{src:?}: {o:?}");
    }
}

#[test]
fn elk_layout_requests_render_with_dagre() {
    let src = "---\nconfig:\n  layout: elk\n---\nflowchart TD\n  A --> B --> C\n";
    assert!(matches!(
        engine().render(src, Trust::Interactive),
        Outcome::Rendered(_)
    ));
}

#[test]
fn document_themes_cannot_override_the_host_mapping() {
    let src = "%%{init: {\"theme\": \"dark\", \"themeVariables\": {\"primaryColor\": \"#ff0000\"}}}%%\n\
               ---\nconfig:\n  theme: forest\n---\nflowchart TD\n  A --> B\n";
    let src2 = "---\nconfig:\n  theme: forest\n  themeVariables:\n    primaryColor: '#ff0000'\n---\nflowchart TD\n  A --> B\n";
    for s in [src, src2] {
        let Outcome::Rendered(d) = engine().render(s, Trust::Interactive) else {
            // The first form may not parse (init before front matter); the
            // second must.
            continue;
        };
        assert!(
            d.literal_colors().is_empty(),
            "document theme leaked: {:?}",
            d.literal_colors()
        );
    }
    assert!(matches!(
        engine().render(src2, Trust::Interactive),
        Outcome::Rendered(_)
    ));
}

#[test]
fn author_colours_stay_literal() {
    let src =
        "flowchart TD\n  A --> B\n  classDef hot fill:#ff3300,stroke:#00aa00\n  class A hot\n";
    let Outcome::Rendered(d) = engine().render(src, Trust::Interactive) else {
        panic!("render")
    };
    let lits = d.literal_colors();
    assert!(lits.contains(&[0xFF, 0x33, 0x00]), "{lits:?}");
    assert!(
        d.items.iter().any(|i| matches!(i.paint, Paint::Role(_))),
        "the rest is still themed"
    );
}

#[test]
fn cancelled_jobs_end_cancelled_and_are_not_cached() {
    let e = DiagramEngine::new(fonts(), Limits::default()).expect("engine");
    let src = "flowchart TD\n  A --> B\n";
    let job = e.job(src, Trust::Interactive);
    let key = job.key();
    job.canceller().cancel();
    assert!(matches!(job.run(&e), Outcome::Cancelled));
    assert!(e.cached(&key).is_none());
    // A fresh job renders.
    assert!(matches!(
        e.render(src, Trust::Interactive),
        Outcome::Rendered(_)
    ));
}

#[test]
fn a_missed_deadline_is_a_too_large_card() {
    let mut limits = Limits::default();
    limits.interactive.deadline = Duration::ZERO;
    let e = DiagramEngine::new(fonts(), limits).expect("engine");
    let o = e.render("flowchart TD\n  A --> B --> C\n", Trust::Interactive);
    let c = card(&o);
    assert_eq!(c.kind, CardKind::TooLarge);
    assert!(c.message.contains("longer than"), "{}", c.message);
}

#[test]
fn display_list_caps_apply() {
    let mut limits = Limits::default();
    limits.interactive.max_items = 5;
    let e = DiagramEngine::new(fonts(), limits).expect("engine");
    let o = e.render("flowchart TD\n  A --> B --> C --> D\n", Trust::Interactive);
    assert_eq!(card(&o).kind, CardKind::TooLarge);
}

#[test]
fn results_are_cached_by_content() {
    let e = DiagramEngine::new(fonts(), Limits::default()).expect("engine");
    let src = "pie\n  \"a\" : 1\n  \"b\" : 2\n";
    let key = DiagramKey::new(src, Trust::Interactive);
    assert!(e.cached(&key).is_none());
    let Outcome::Rendered(first) = e.render(src, Trust::Interactive) else {
        panic!("render")
    };
    let Some(Outcome::Rendered(again)) = e.cached(&key) else {
        panic!("cached")
    };
    assert!(Arc::ptr_eq(&first, &again));
    let stats = e.cache_stats();
    assert_eq!(stats.entries, 1);
    assert!(stats.hits >= 1);
    // Errors are cached too (the same source fails the same way).
    let bad = "sequenceDiagram\n  ->>: x\n";
    let _ = e.render(bad, Trust::Interactive);
    assert!(
        e.cached(&DiagramKey::new(bad, Trust::Interactive))
            .is_some()
    );
    e.clear_cache();
    assert!(e.cached(&key).is_none());
}

#[test]
fn jobs_run_on_other_threads() {
    let e = Arc::new(DiagramEngine::new(fonts(), Limits::default()).expect("engine"));
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let e = Arc::clone(&e);
            std::thread::spawn(move || {
                let src = format!("flowchart LR\n  A{i} --> B{i}\n");
                matches!(e.render(&src, Trust::Interactive), Outcome::Rendered(_))
            })
        })
        .collect();
    for h in handles {
        assert!(h.join().expect("thread"));
    }
}

/// Small broken inputs, generated here: truncations and byte mutations of
/// valid sources. None may panic or hang; each ends in a defined outcome.
#[test]
fn malformed_inputs_end_cleanly() {
    let seeds = [
        "flowchart TD\n  A[Start] -->|yes| B{Check}\n  B --> C((End))\n",
        "sequenceDiagram\n  Alice->>+Bob: hi\n  Note over Alice,Bob: x\n  Bob-->>-Alice: ok\n",
        "classDiagram\n  class A { +int x\n +f() bool }\n  A <|-- B\n",
        "gantt\n  dateFormat YYYY-MM-DD\n  section S\n  T :a, 2026-01-01, 3d\n",
        "pie\n  \"a\" : 1\n  \"b\" : 2\n",
        "stateDiagram-v2\n  [*] --> A\n  state A { [*] --> B }\n",
        "mindmap\n  root\n    a\n      b\n",
        "xychart-beta\n  x-axis [a, b]\n  bar [1, 2]\n",
    ];
    let mut cases: Vec<String> = Vec::new();
    for s in seeds {
        for cut in (1..s.len()).step_by(7) {
            if s.is_char_boundary(cut) {
                cases.push(s[..cut].to_owned());
            }
        }
        let bytes = s.as_bytes();
        for (k, sub) in b"{[\"\n->:%".iter().enumerate() {
            let mut b = bytes.to_vec();
            let at = (k * 13 + 5) % b.len();
            b[at] = *sub;
            if let Ok(t) = String::from_utf8(b) {
                cases.push(t);
            }
        }
        cases.push(s.repeat(3));
        cases.push(s.replace('\n', " "));
    }
    cases.push("flowchart TD\n".to_owned() + &"A[".repeat(500));
    cases.push("graph TD\n  A-->".to_owned() + &"(".repeat(200));
    cases.push(format!(
        "flowchart TD\n  A[\"{}\"] --> B\n",
        "x".repeat(20_000)
    ));
    cases.push("\u{0}\u{1}\u{7f}flowchart TD\n A-->B".into());
    for c in &cases {
        let o = engine().render(c, Trust::Interactive);
        if let Outcome::Card(card) = &o {
            assert_ne!(
                card.kind,
                CardKind::RendererError,
                "{c:?}: {}",
                card.message
            );
        }
        assert!(!matches!(o, Outcome::Cancelled));
    }
}

#[test]
fn links_and_internal_links_are_clickable_parts() {
    let src = "graph TD\n  A[Web] --> B[My Note]\n  B --> C[Plain]\n  click A href \"https://example.com/x\"\n  class B internal-link;\n";
    let Outcome::Rendered(d) = engine().render(src, Trust::Interactive) else {
        panic!("rendered");
    };
    let targets: Vec<&gv_diagram::LinkTarget> = d.links.iter().map(|l| &l.target).collect();
    assert_eq!(
        targets,
        [
            &gv_diagram::LinkTarget::Href("https://example.com/x".into()),
            &gv_diagram::LinkTarget::Note("My Note".into()),
        ],
        "{:?}",
        d.links
    );
    for l in &d.links {
        let r = l.rect;
        assert!(r.width > 0.0 && r.height > 0.0);
        assert!(r.x >= 0.0 && r.y >= 0.0);
        assert!(r.x + r.width <= d.width + 0.5 && r.y + r.height <= d.height + 0.5);
    }
    // The web node is above the note node (top to bottom).
    assert!(d.links[0].rect.y < d.links[1].rect.y);
    // Unsafe links are dropped by merman's sanitizer, never surfaced.
    let Outcome::Rendered(d) = engine().render(
        "graph TD\n  A --> B\n  click A href \"javascript:alert(1)\"\n",
        Trust::Interactive,
    ) else {
        panic!("rendered");
    };
    assert!(
        d.links.iter().all(
            |l| !matches!(&l.target, gv_diagram::LinkTarget::Href(h) if h.contains("javascript"))
        ),
        "{:?}",
        d.links
    );
}
