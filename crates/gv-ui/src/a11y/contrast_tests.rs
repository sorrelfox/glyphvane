// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! WCAG 2.2 AA contrast of the theme files' foreground/background pairs.
//!
//! Text needs 4.5:1 (SC 1.4.3); focus rings and the outlines that are a
//! control's only boundary need 3:1 (SC 1.4.11). Translucent overlays
//! (`state.hover`, `state.press`) are composited on the surface they are
//! drawn over. Pairs that fall short are listed in [`KNOWN_SHORTFALLS`]
//! for the design review; any other pair below its minimum fails the test,
//! and a listed pair that now passes fails it too, so the list stays true.

use crate::theme::{Color, Theme, ThemeId};

const TEXT: f64 = 4.5;
const NON_TEXT: f64 = 3.0;

/// `(theme, foreground, background)` pairs below their minimum, reported
/// to the design (the colours are the design's to change).
const KNOWN_SHORTFALLS: &[(&str, &str, &str)] = &[
    ("light", "text.tertiary", "selection.text"),
    ("light", "border.strong (control outline)", "surface.page"),
    ("light", "border.strong (control outline)", "surface.raised"),
    ("light", "border.strong (control outline)", "surface.panel"),
    ("light", "border.strong (control outline)", "surface.window"),
    ("light", "border.strong (control outline)", "surface.sunken"),
    ("semi-light", "text.tertiary", "selection.text"),
    (
        "semi-light",
        "border.strong (control outline)",
        "surface.page",
    ),
    (
        "semi-light",
        "border.strong (control outline)",
        "surface.raised",
    ),
    (
        "semi-light",
        "border.strong (control outline)",
        "surface.panel",
    ),
    (
        "semi-light",
        "border.strong (control outline)",
        "surface.window",
    ),
    (
        "semi-light",
        "border.strong (control outline)",
        "surface.sunken",
    ),
    ("dark", "text.tertiary", "accent.tint"),
    ("dark", "text.tertiary", "selection.text"),
    ("dark", "border.strong (control outline)", "surface.page"),
    ("dark", "border.strong (control outline)", "surface.raised"),
    ("dark", "border.strong (control outline)", "surface.panel"),
    ("dark", "border.strong (control outline)", "surface.window"),
    ("dark", "border.strong (control outline)", "surface.sunken"),
];

/// `over` drawn on the opaque `under` (straight alpha).
fn composite(over: Color, under: Color) -> Color {
    let a = f64::from(over.a) / 255.0;
    let mix = |o: u8, u: u8| {
        let v = f64::from(o) * a + f64::from(u) * (1.0 - a);
        // In range by construction.
        v.round().clamp(0.0, 255.0) as u8
    };
    Color {
        r: mix(over.r, under.r),
        g: mix(over.g, under.g),
        b: mix(over.b, under.b),
        a: 0xFF,
    }
}

struct Pair {
    fg: String,
    bg: String,
    ratio: f64,
    min: f64,
}

fn pairs(theme: &Theme) -> Vec<Pair> {
    let s = &theme.surface;
    let surfaces = [
        ("surface.window", s.window),
        ("surface.panel", s.panel),
        ("surface.page", s.page),
        ("surface.raised", s.raised),
        ("surface.sunken", s.sunken),
    ];
    let t = &theme.text;
    let st = &theme.status;
    let sy = &theme.syntax;
    let mut out = Vec::new();
    let mut add = |fg: &str, fg_c: Color, bg: &str, bg_c: Color, min: f64| {
        out.push(Pair {
            fg: fg.to_owned(),
            bg: bg.to_owned(),
            ratio: fg_c.contrast_ratio(bg_c),
            min,
        });
    };
    for (bg, bg_c) in surfaces {
        for (fg, c) in [
            ("text.primary", t.primary),
            ("text.secondary", t.secondary),
            ("text.tertiary", t.tertiary),
            ("accent.default", theme.accent.default),
            ("status.error", st.error),
            ("status.warning", st.warning),
            ("status.success", st.success),
            ("status.info", st.info),
        ] {
            add(fg, c, bg, bg_c, TEXT);
        }
        // Hovered and pressed rows and buttons.
        for (state, overlay) in [
            ("state.hover", theme.state.hover),
            ("state.press", theme.state.press),
        ] {
            let under = composite(overlay, bg_c);
            for (fg, c) in [("text.primary", t.primary), ("text.secondary", t.secondary)] {
                add(fg, c, &format!("{state} on {bg}"), under, TEXT);
            }
        }
        add(
            "accent.default (focus ring)",
            theme.accent.default,
            bg,
            bg_c,
            NON_TEXT,
        );
        add(
            "border.strong (control outline)",
            theme.border.strong,
            bg,
            bg_c,
            NON_TEXT,
        );
    }
    // Code and syntax colours on the page and in code blocks.
    for (bg, bg_c) in [("surface.page", s.page), ("surface.sunken", s.sunken)] {
        for (fg, c) in [
            ("syntax.key", sy.key),
            ("syntax.string", sy.string),
            ("syntax.number", sy.number),
            ("syntax.tag", sy.tag),
            ("syntax.url", sy.url),
            ("syntax.code", sy.code),
        ] {
            add(fg, c, bg, bg_c, TEXT);
        }
    }
    // Selected rows, tags, chips, matches and selected text.
    for (bg, bg_c) in [
        ("accent.tint", theme.accent.tint),
        ("selection.text", theme.selection.text),
    ] {
        for (fg, c) in [
            ("text.primary", t.primary),
            ("text.secondary", t.secondary),
            ("text.tertiary", t.tertiary),
        ] {
            add(fg, c, bg, bg_c, TEXT);
        }
    }
    add(
        "accent.default (focus ring)",
        theme.accent.default,
        "accent.tint",
        theme.accent.tint,
        NON_TEXT,
    );
    add(
        "accent.on",
        theme.accent.on,
        "accent.default",
        theme.accent.default,
        TEXT,
    );
    // Infobars and callouts: the status colour and body text on its tint.
    for (name, fg_c, bg_c) in [
        ("error", st.error, st.error_tint),
        ("warning", st.warning, st.warning_tint),
        ("success", st.success, st.success_tint),
        ("info", st.info, st.info_tint),
    ] {
        let bg = format!("status.{name}_tint");
        add(&format!("status.{name}"), fg_c, &bg, bg_c, TEXT);
        add("text.primary", t.primary, &bg, bg_c, TEXT);
        add("text.secondary", t.secondary, &bg, bg_c, TEXT);
    }
    add(
        "state.close_hover_fg",
        theme.state.close_hover_fg,
        "state.close_hover",
        theme.state.close_hover,
        TEXT,
    );
    out
}

#[test]
fn theme_pairs_meet_wcag_aa_or_are_reported() {
    let mut report = String::new();
    let mut unexpected = Vec::new();
    let mut now_passing = Vec::new();
    for id in ThemeId::ALL {
        let theme = id.theme();
        let name = theme.id.as_str();
        for pair in pairs(theme) {
            let listed = KNOWN_SHORTFALLS
                .iter()
                .any(|&(t, fg, bg)| t == name && fg == pair.fg && bg == pair.bg);
            let fails = pair.ratio < pair.min;
            if fails {
                report.push_str(&format!(
                    "{name}: {} on {} = {:.2}:1 (needs {}:1)\n",
                    pair.fg, pair.bg, pair.ratio, pair.min
                ));
            }
            if fails && !listed {
                unexpected.push(format!(
                    "{name}: {} on {} {:.2}",
                    pair.fg, pair.bg, pair.ratio
                ));
            }
            if !fails && listed {
                now_passing.push(format!(
                    "{name}: {} on {} {:.2}",
                    pair.fg, pair.bg, pair.ratio
                ));
            }
        }
    }
    // `cargo test contrast -- --nocapture` prints the shortfalls.
    println!("{report}");
    assert!(unexpected.is_empty(), "new shortfalls: {unexpected:#?}");
    assert!(
        now_passing.is_empty(),
        "listed shortfalls that now pass: {now_passing:#?}"
    );
}

#[test]
fn composite_of_an_opaque_or_clear_overlay() {
    let red = Color::from_hex("#FF0000").expect("valid");
    let white = Color::from_hex("#FFFFFF").expect("valid");
    assert_eq!(composite(red, white), red);
    let clear = Color::from_hex("#FF000000").expect("valid");
    assert_eq!(composite(clear, white), white);
}
