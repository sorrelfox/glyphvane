// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The comparison model: regions, default choices, choices per region and
//! the result text. The view in a window is tested with the documents it
//! compares (`files::safety_tests`).

use gv_text::LineEnding;

use super::{Comparison, HunkKind, Pick, Region, Sides};

fn sides() -> Sides {
    Sides {
        left: "mine".into(),
        right: "theirs".into(),
    }
}

#[test]
fn three_way_starts_with_the_side_that_changed() {
    let base = "a\nb\nc\nd\ne\n";
    let mine = "A\nb\nc\nd\ne\n";
    let theirs = "a\nb\nc\nd\nE\n";
    let mut c = Comparison::three_way(base, mine, theirs, sides(), LineEnding::Lf);
    let hunks: Vec<_> = c.hunks().map(|h| (h.kind, h.pick)).collect();
    assert_eq!(
        hunks,
        [
            (HunkKind::Left, Some(Pick::Left)),
            (HunkKind::Right, Some(Pick::Right))
        ]
    );
    assert_eq!(c.unresolved(), 0);
    assert_eq!(c.result(), "A\nb\nc\nd\nE\n", "a clean merge");
    assert!(matches!(c.regions()[1], Region::Unchanged(_)));

    c.pick(0, Pick::Right);
    assert_eq!(c.result(), "a\nb\nc\nd\nE\n");
    c.pick_all(Pick::Left);
    assert_eq!(c.result(), mine);
    c.pick_all(Pick::Right);
    assert_eq!(c.result(), theirs);
}

#[test]
fn conflicts_start_unresolved_and_are_marked() {
    let c = Comparison::three_way(
        "t\r\nbody\r\nend",
        "t\r\nmine\r\nend",
        "t\r\ntheirs\r\nend",
        sides(),
        LineEnding::CrLf,
    );
    assert_eq!(c.unresolved(), 1);
    assert_eq!(c.hunks().next().map(|h| h.kind), Some(HunkKind::Conflict));
    assert_eq!(
        c.result(),
        "t\r\n<<<<<<< mine\r\nmine\r\n=======\r\ntheirs\r\n>>>>>>> theirs\r\nend"
    );
}

#[test]
fn both_writes_left_then_right() {
    let mut c = Comparison::three_way("x\nend", "x\nmine", "x\ntheirs", sides(), LineEnding::Lf);
    c.pick(0, Pick::Both);
    // The left side had no line ending: one is added between the two.
    assert_eq!(c.result(), "x\nmine\ntheirs");
    assert_eq!(c.unresolved(), 0);
}

#[test]
fn the_same_change_on_both_sides_is_written_once() {
    let c = Comparison::three_way("a\nb\n", "a\nB\n", "a\nB\n", sides(), LineEnding::Lf);
    let kinds: Vec<_> = c.hunks().map(|h| h.kind).collect();
    assert_eq!(kinds, [HunkKind::Same]);
    assert_eq!(c.result(), "a\nB\n");
}

#[test]
fn two_way_keeps_the_left_side_until_told() {
    let mine = "# Note\n\nRecovered line.\nkept\n";
    let disk = "# Note\n\nkept\nOn disk only.\n";
    let mut c = Comparison::two_way(mine, disk, sides(), LineEnding::Lf);
    assert!(c.hunks().all(|h| h.kind == HunkKind::Left));
    assert!(c.hunk_count() >= 2);
    assert_eq!(c.result(), mine);
    c.pick_all(Pick::Right);
    assert_eq!(c.result(), disk);
    // No differences: nothing to choose.
    let same = Comparison::two_way(mine, mine, sides(), LineEnding::Lf);
    assert_eq!(same.hunk_count(), 0);
    assert_eq!(same.result(), mine);
}

/// The compare view for assistive technology, in a headless window: a
/// region named by its title, the summary as a polite status, each change
/// a named group whose choices say which is pressed, each side's text
/// whether it is in the result; Tab reaches the buttons and Space picks.
#[gpui::test]
fn the_compare_view_names_its_changes_and_choices(cx: &mut gpui::TestAppContext) {
    use gpui::{
        AppContext as _, Bounds, VisualTestContext, WeakEntity, WindowBounds, WindowOptions, point,
        px, size,
    };

    use crate::a11y::testing::{focused, named, with_role};
    use crate::a11y::{Live, Role, Toggled};
    use crate::{AppSettings, ThemeChoice};

    fn draw(vcx: &mut VisualTestContext) {
        for _ in 0..2 {
            vcx.run_until_parked();
            vcx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    let comparison = Comparison::three_way(
        "t\nbody\nend\n",
        "t\nmine\nend\n",
        "t\ntheirs\nend\n",
        sides(),
        LineEnding::Lf,
    );
    let (window, view) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(900.0), px(600.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        let (window, view) = gpui::open_window(options, cx, |_, cx| {
            cx.new(|_| {
                super::CompareView::new(comparison, "a.md".into(), WeakEntity::new_invalid())
            })
        })
        .expect("test window opens");
        (window, view)
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);

    assert_eq!(named(&mut vcx, "Compare: a.md").role, Some(Role::Region));
    assert_eq!(named(&mut vcx, "Compare").role, Some(Role::Toolbar));
    let summary = with_role(&mut vcx, Role::Status);
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].live, Some(Live::Polite));
    assert_eq!(
        summary[0].label.as_deref(),
        Some("1 change · 1 conflict to choose")
    );
    let change = named(&mut vcx, "Change 1 of 1: Changed on both sides");
    assert_eq!(change.role, Some(Role::Group));
    assert_eq!(change.description.as_deref(), Some("No side chosen yet"));
    let mine = with_role(&mut vcx, Role::Group)
        .into_iter()
        .find(|n| n.label.as_deref() == Some("mine"))
        .expect("the left side's text");
    assert_eq!(mine.description.as_deref(), Some("Not in the result"));
    for label in ["Use all left", "Use all right", "Cancel", "Apply"] {
        let b = named(&mut vcx, label);
        assert_eq!(b.role, Some(Role::Button), "{label}");
        assert!(b.focusable && b.clickable, "{label}");
    }
    assert_eq!(named(&mut vcx, "Use left").toggled, Some(Toggled::False));

    // Tab to Use left; Space picks it.
    vcx.update(|window, cx| window.focus_next(cx));
    draw(&mut vcx);
    for _ in 0..12 {
        if focused(&mut vcx).flatten().as_deref() == Some("Use left") {
            break;
        }
        vcx.simulate_keystrokes("tab");
        draw(&mut vcx);
    }
    assert_eq!(focused(&mut vcx).flatten().as_deref(), Some("Use left"));
    vcx.simulate_keystrokes("space");
    vcx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("space").expect("a key"),
    });
    draw(&mut vcx);
    assert_eq!(named(&mut vcx, "Use left").toggled, Some(Toggled::True));
    let mine = with_role(&mut vcx, Role::Group)
        .into_iter()
        .find(|n| n.label.as_deref() == Some("mine"))
        .expect("the left side's text");
    assert_eq!(mine.description.as_deref(), Some("In the result"));
    assert_eq!(view.read_with(&vcx, |v, _| v.comparison().unresolved()), 0);
}

/// In a narrow window the header's title wraps in its own column and the
/// buttons stay inside the window, clear of the title and the summary.
#[gpui::test]
fn the_header_wraps_clear_of_the_buttons(cx: &mut gpui::TestAppContext) {
    use gpui::{
        AppContext as _, Bounds, VisualTestContext, WeakEntity, WindowBounds, WindowOptions, point,
        px, size,
    };

    use crate::{AppSettings, ThemeChoice};

    cx.update(|cx| crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light))));
    let title = "notes/projects/a rather long folder name/another long folder name/\
                 yet another folder with a long name/meeting notes from the long week.md";
    let controls = [
        "compare-all-left",
        "compare-all-right",
        "compare-cancel",
        "compare-apply",
    ];
    for width in [1200.0, 800.0, 500.0, 320.0] {
        let comparison = Comparison::three_way(
            "t\nbody\nend\n",
            "t\nmine\nend\n",
            "t\ntheirs\nend\n",
            sides(),
            LineEnding::Lf,
        );
        let (window, _) = cx.update(|cx| {
            let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(width), px(600.0)));
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..WindowOptions::default()
            };
            gpui::open_window(options, cx, |_, cx| {
                cx.new(|_| {
                    super::CompareView::new(comparison, title.into(), WeakEntity::new_invalid())
                })
            })
            .expect("test window opens")
        });
        let mut vcx = VisualTestContext::from_window(window, cx);
        for _ in 0..2 {
            vcx.run_until_parked();
            vcx.update(|window, cx| window.draw(cx).clear(cx));
        }
        crate::files::notice::assert_clear_of_buttons(&mut vcx, width, "compare-title", &controls);
        let summary = vcx.debug_bounds("compare-summary").expect("summary drawn");
        assert!(summary.right() <= px(width), "{width}: summary {summary:?}");
        for id in controls {
            let control = vcx.debug_bounds(id).expect("drawn");
            assert!(!summary.intersects(&control), "{width}: summary under {id}");
        }
    }
}

/// A long line wraps in its column instead of running past its edge.
#[gpui::test]
fn long_lines_wrap_in_their_column(cx: &mut gpui::TestAppContext) {
    use gpui::{
        AppContext as _, Bounds, VisualTestContext, WeakEntity, WindowBounds, WindowOptions, point,
        px, size,
    };

    use crate::{AppSettings, ThemeChoice};

    cx.update(|cx| crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light))));
    let long = "word ".repeat(60);
    let mine = format!("t\n{long}mine\nend\n");
    let comparison = Comparison::three_way(
        "t\nbody\nend\n",
        &mine,
        "t\ntheirs\nend\n",
        sides(),
        LineEnding::Lf,
    );
    let (window, _) = cx.update(|cx| {
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(800.0), px(600.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |_, cx| {
            cx.new(|_| {
                super::CompareView::new(comparison, "a.md".into(), WeakEntity::new_invalid())
            })
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    for _ in 0..2 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
    let left = vcx
        .debug_bounds("compare-hunk-0-left-text")
        .expect("the left side");
    let right = vcx
        .debug_bounds("compare-hunk-0-right-text")
        .expect("the right side");
    let line = px(crate::metrics::text::EDITOR.line);
    assert!(left.size.height > line * 3.0, "wrapped: {left:?}");
    assert!(left.right() <= right.left(), "{left:?} {right:?}");
}
