// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The engine: results, errors, caps, colours, fallback text and the cache.

use std::sync::Arc;

use gv_math::{
    FallbackMetrics, GlyphMetrics, Limits, MacroSet, MathColor, MathEngine, MathError, MathItem,
    document_macros,
};

fn engine() -> MathEngine {
    MathEngine::default()
}

#[test]
fn lays_out_with_metrics_in_em() {
    let l = engine()
        .render(r"x^2", false, &MacroSet::empty())
        .expect("ok");
    assert!(!l.display);
    assert!((0.5..2.0).contains(&l.width), "{}", l.width);
    assert!(l.height > 0.5 && l.depth >= 0.0);
    let ink = l.ink.expect("ink");
    assert!(ink.min_y >= -0.1 && ink.max_y <= l.total_height() + 0.1);
    assert!(l.items.iter().all(|i| matches!(i, MathItem::Fill { .. })));
}

#[test]
fn display_style_is_taller_than_text_style() {
    let e = engine();
    let tex = r"\sum_{i=1}^n \frac{1}{i}";
    let d = e.render(tex, true, &MacroSet::empty()).expect("display");
    let t = e.render(tex, false, &MacroSet::empty()).expect("text");
    assert!(d.total_height() > t.total_height());
}

#[test]
fn colours_follow_the_theme_unless_explicit() {
    let l = engine()
        .render(r"a \color{red} b", false, &MacroSet::empty())
        .expect("ok");
    let colors: Vec<MathColor> = l
        .items
        .iter()
        .map(|i| match i {
            MathItem::Fill { color, .. }
            | MathItem::Stroke { color, .. }
            | MathItem::Text { color, .. } => *color,
        })
        .collect();
    assert_eq!(colors[0], MathColor::Current);
    let MathColor::Rgba([r, g, b, a]) = colors[1] else {
        panic!("{colors:?}")
    };
    assert!(r > 0.9 && g < 0.1 && b < 0.1 && a > 0.99);
}

#[test]
fn parse_errors_carry_message_and_span() {
    let e = engine();
    let Err(MathError::Parse { message, span }) =
        e.render(r"x + \foo{y}", false, &MacroSet::empty())
    else {
        panic!("expected a parse error")
    };
    assert!(message.contains("\\foo"), "{message}");
    assert_eq!(span.map(|s| s.start), Some(4));
    assert!(matches!(
        e.render(r"\frac{1}{2", true, &MacroSet::empty()),
        Err(MathError::Parse { .. })
    ));
    assert!(matches!(
        e.render(r"\begin{matrix} a \end{pmatrix}", true, &MacroSet::empty()),
        Err(MathError::Parse { .. })
    ));
}

#[test]
fn ratex_limits_become_errors_not_crashes() {
    let e = engine();
    let deep = format!("{}x{}", "{".repeat(5000), "}".repeat(5000));
    assert!(matches!(
        e.render(&deep[..4000], true, &MacroSet::empty()),
        Err(MathError::Parse { .. })
    ));
    let bomb = r"\def\a{xx}\def\b{\a\a\a\a\a\a\a\a}\def\c{\b\b\b\b\b\b\b\b}\def\d{\c\c\c\c\c\c\c\c}\def\e{\d\d\d\d\d\d\d\d}\def\f{\e\e\e\e\e\e\e\e}\f\f\f\f";
    assert!(e.render(bomb, true, &MacroSet::empty()).is_err());
    assert!(e.render(r"\def\a{\a}\a", true, &MacroSet::empty()).is_err());
}

#[test]
fn source_length_caps() {
    let e = engine();
    let long = "x+".repeat(3000);
    assert!(matches!(
        e.render(&long, true, &MacroSet::empty()),
        Err(MathError::TooLong {
            chemistry: false,
            len: 6000,
            max: 4096
        })
    ));
    let chem = format!(r"\ce{{{}}}", "H2O + ".repeat(200));
    assert!(matches!(
        e.render(&chem, true, &MacroSet::empty()),
        Err(MathError::TooLong {
            chemistry: true,
            ..
        })
    ));
    // The same length without mhchem renders.
    let plain = format!("x{}", "{}".repeat(550));
    assert!(e.render(&plain, true, &MacroSet::empty()).is_ok());
}

#[test]
fn item_and_extent_caps() {
    let limits = Limits {
        max_items: 50,
        ..Limits::default()
    };
    let e = MathEngine::new(limits);
    let many = r"\underbrace{a+b}_{c}".repeat(40);
    assert!(matches!(
        e.render(&many, true, &MacroSet::empty()),
        Err(MathError::TooComplex { max: 50, .. })
    ));
    let big = engine();
    for tex in [
        r"\rule{10000em}{1em}",
        r"\rule{1em}{10000em}",
        r"a\kern99999em b",
    ] {
        assert!(
            matches!(
                big.render(tex, true, &MacroSet::empty()),
                Err(MathError::TooLarge { .. })
            ),
            "{tex}"
        );
    }
    assert!(
        big.render(r"\rule{10em}{1em}", true, &MacroSet::empty())
            .is_ok()
    );
}

#[test]
fn error_messages_are_short_sentences() {
    let e = MathError::TooLong {
        len: 5000,
        max: 4096,
        chemistry: true,
    };
    assert_eq!(
        e.to_string(),
        "Chemistry formula too long to render (5000 bytes; the limit is 4096)"
    );
}

#[test]
fn document_macros_apply_to_later_formulas() {
    let e = engine();
    let formulas = [
        r"\newcommand{\RR}{\mathbb{R}} \RR",
        r"x \in \RR^n",
        r"\def\sq#1{#1^2} \sq{y}",
        r"\sq{z}",
    ];
    let sets = document_macros(formulas);
    for (tex, set) in formulas.iter().zip(&sets) {
        assert!(e.render(tex, false, set).is_ok(), "{tex}");
    }
    // Without the pass, the second formula fails.
    assert!(e.render(formulas[1], false, &MacroSet::empty()).is_err());
}

#[test]
fn the_cache_keys_on_source_macros_and_style() {
    let e = engine();
    let m = MacroSet::empty();
    assert!(e.cached("x", true, &m).is_none());
    let a = e.render("x", true, &m).expect("ok");
    let b = e.render("x", true, &m).expect("ok");
    assert!(Arc::ptr_eq(&a, &b));
    assert!(e.cached("x", true, &m).is_some());
    assert!(e.cached("x", false, &m).is_none());
    let with = MacroSet::from_pairs([(r"\a", "b")]);
    assert!(e.cached("x", true, &with).is_none());
    // Errors are cached too.
    let _ = e.render(r"\foo", true, &m);
    assert!(matches!(e.cached(r"\foo", true, &m), Some(Err(_))));
    let s = e.stats();
    assert_eq!(s.entries, 2);
    assert!(s.hits >= 2);
    e.clear();
    assert_eq!(e.stats().entries, 0);
}

#[test]
fn the_cache_evicts_the_least_recently_used() {
    let e = MathEngine::new(Limits {
        cache_entries: 8,
        ..Limits::default()
    });
    let m = MacroSet::empty();
    let _ = e.render("keep", true, &m);
    for i in 0..20 {
        let _ = e.render(&format!("x_{{{i}}}"), true, &m);
        let _ = e.render("keep", true, &m);
    }
    assert!(e.stats().entries <= 8);
    assert!(e.cached("keep", true, &m).is_some());
    assert!(e.cached("x_{0}", true, &m).is_none());
}

#[test]
fn characters_outside_katex_become_text_items() {
    struct Wide;
    impl FallbackMetrics for Wide {
        fn metrics(&self, _: char) -> Option<GlyphMetrics> {
            Some(GlyphMetrics {
                width: 2.0,
                height: 0.7,
                depth: 0.1,
            })
        }
    }
    let tex = r"\text{中}";
    let guessed = engine().render(tex, false, &MacroSet::empty()).expect("ok");
    assert!(guessed.has_fallback_text());
    let measured = MathEngine::default()
        .with_fallback(Arc::new(Wide))
        .render(tex, false, &MacroSet::empty())
        .expect("ok");
    assert!((measured.width - 2.0).abs() < 0.01, "{}", measured.width);
    assert!(matches!(measured.items[0], MathItem::Text { ch: '中', .. }));
}

#[test]
fn renders_from_many_threads() {
    let e = Arc::new(engine());
    let handles: Vec<_> = (0..4)
        .map(|t| {
            let e = e.clone();
            std::thread::spawn(move || {
                for i in 0..25 {
                    let tex = format!(r"\frac{{{t}}}{{{i}}} + \ce{{H2O}}");
                    assert!(e.render(&tex, i % 2 == 0, &MacroSet::empty()).is_ok());
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread");
    }
}
