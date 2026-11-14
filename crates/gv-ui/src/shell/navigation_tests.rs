// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! History, closing and reopening tabs, the Outline and the status bar, in
//! headless GPUI windows.

use std::path::Path;
use std::sync::Arc;

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Modifiers, MouseButton, NavigationDirection,
    Pixels, Point, ScrollDelta, ScrollEvent, TestAppContext, VisualTestContext, WindowBounds,
    WindowOptions, point, px, size,
};
use gv_model::LinkKind;

use super::outline_panel::OutlinePanel;
use super::status_bar;
use super::{OpenTarget, ViewMode, Workspace};
use crate::chrome::WindowChrome;
use crate::reader::follow::{self, Cause};
use crate::reader::{Disposition, LinkRequest, Reader, ReaderEvent, ScrollTarget};
use crate::{AppSettings, ThemeChoice, ToggleEditMode, TogglePanel};

const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 800.0;

fn temp(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).expect("write");
    }
    dir
}

fn open(cx: &mut TestAppContext, file: &Path) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let target = Some(OpenTarget::new(file));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(WIDTH), px(HEIGHT)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    })
}

fn draw(vcx: &mut VisualTestContext) {
    // Loading and word counts finish on the executor; a jump lands while
    // the view is laid out and the window follows on the next frame.
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn drawn(window: AnyWindowHandle, cx: &mut TestAppContext) -> VisualTestContext {
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    vcx
}

fn reader(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> Entity<Reader> {
    workspace
        .read_with(vcx, |ws, _| ws.active_tab().reader().cloned())
        .expect("the active tab has a reader")
}

fn file_of(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> String {
    workspace.read_with(vcx, |ws, _| {
        ws.active_tab()
            .path()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

fn tab_names(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> Vec<String> {
    workspace.read_with(vcx, |ws, _| {
        ws.tabs()
            .iter()
            .map(|t| {
                t.path()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect()
    })
}

/// (can go back, can go forward)
fn can_travel(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> (bool, bool) {
    workspace.read_with(vcx, |ws, _| (ws.can_go_back(), ws.can_go_forward()))
}

/// The source offset of the block at the top of the active Reading view.
fn top_offset(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> Option<usize> {
    let reader = reader(workspace, vcx);
    match reader.read_with(vcx, |r, _| r.position()) {
        Some(ScrollTarget::Offset(offset)) => Some(offset),
        _ => None,
    }
}

/// Follows a link from the active Reading view as a click would.
fn follow(
    workspace: &Entity<Workspace>,
    vcx: &mut VisualTestContext,
    destination: &str,
    disposition: Disposition,
) {
    let reader = reader(workspace, vcx);
    let request = LinkRequest {
        destination: Arc::from(destination),
        kind: LinkKind::Inline,
        disposition,
    };
    reader.update(vcx, |_, cx| cx.emit(ReaderEvent::FollowLink(request)));
    draw(vcx);
}

fn scroll_reader_to(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext, offset: usize) {
    let reader = reader(workspace, vcx);
    reader.update(vcx, |r, cx| r.scroll_to(ScrollTarget::Offset(offset), cx));
    draw(vcx);
}

fn center(vcx: &mut VisualTestContext, selector: &'static str) -> Point<Pixels> {
    vcx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"))
        .center()
}

/// A document with a heading tree and enough text between headings to
/// scroll.
fn outlined_doc() -> String {
    let mut s = String::from("# Guide\n\nIntro text.\n\n");
    let sections = [
        "## Install",
        "### From source",
        "## Usage",
        "### Links",
        "#### Fragments",
        "## Reference",
    ];
    for heading in sections {
        s.push_str(heading);
        s.push_str("\n\n");
        for i in 0..12 {
            s.push_str(&format!(
                "Paragraph {i} of this section has enough words to fill a line or two of the column.\n\n"
            ));
        }
    }
    s
}

fn outline(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> Entity<OutlinePanel> {
    workspace
        .read_with(vcx, |ws, _| ws.outline.clone())
        .expect("the outline exists")
}

/// The heading `text` starts at this offset.
fn heading_offset(doc: &str, text: &str) -> usize {
    doc.find(text).expect("heading in the document")
}

#[gpui::test]
fn back_and_forward_across_a_link_and_a_tab_switch(cx: &mut TestAppContext) {
    let a = outlined_doc();
    let dir = temp(&[("a.md", &a), ("b.md", "# B\n\nText.\n"), ("c.md", "# C\n")]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    assert_eq!(can_travel(&workspace, &mut vcx), (false, false));

    // Read a.md down to "## Usage", then follow a link to b.md.
    let usage = heading_offset(&a, "## Usage");
    scroll_reader_to(&workspace, &mut vcx, usage);
    assert_eq!(top_offset(&workspace, &mut vcx), Some(usage));
    follow(&workspace, &mut vcx, "b.md", Disposition::InPlace);
    assert_eq!(file_of(&workspace, &mut vcx), "b.md");
    assert_eq!(can_travel(&workspace, &mut vcx), (true, false));

    // History is per tab: another tab has none, and switching back finds
    // the first tab's history again.
    vcx.update(|window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.open_tab(OpenTarget::new(dir.path().join("c.md")), window, cx);
        });
    });
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "c.md");
    assert_eq!(can_travel(&workspace, &mut vcx), (false, false));
    cx.dispatch_action(window, crate::GoBack);
    assert_eq!(file_of(&workspace, &mut vcx), "c.md");
    vcx.update(|window, cx| workspace.update(cx, |ws, cx| ws.activate_tab(0, window, cx)));
    draw(&mut vcx);
    assert_eq!(can_travel(&workspace, &mut vcx), (true, false));

    // Back returns to a.md where it was read.
    cx.dispatch_action(window, crate::GoBack);
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "a.md");
    assert_eq!(top_offset(&workspace, &mut vcx), Some(usage));
    assert_eq!(can_travel(&workspace, &mut vcx), (false, true));

    // Forward goes to b.md again.
    cx.dispatch_action(window, crate::GoForward);
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "b.md");
    assert_eq!(can_travel(&workspace, &mut vcx), (true, false));

    // A new place forgets the way forward.
    cx.dispatch_action(window, crate::GoBack);
    draw(&mut vcx);
    follow(&workspace, &mut vcx, "c.md", Disposition::InPlace);
    assert_eq!(file_of(&workspace, &mut vcx), "c.md");
    assert_eq!(can_travel(&workspace, &mut vcx), (true, false));
    let back: Vec<_> = workspace.read_with(&vcx, |ws, _| {
        ws.active_tab()
            .history()
            .iter()
            .map(|e| (e.path.ends_with("a.md"), e.mode))
            .collect()
    });
    assert_eq!(back, [(true, ViewMode::Preview)]);
}

/// The items the active Reading view painted: (source offset, content top
/// below the view's top).
fn painted(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> Vec<(usize, f32)> {
    let reader = reader(workspace, vcx);
    reader.read_with(vcx, |r, _| r.painted_items())
}

#[gpui::test]
fn a_link_to_a_heading_in_another_file_and_back(cx: &mut TestAppContext) {
    let a = outlined_doc();
    let b = outlined_doc().replace("# Guide", "# Other guide");
    let dir = temp(&[("a.md", &a), ("b.md", &b)]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);

    // Read a.md from "### From source".
    let from_source = heading_offset(&a, "### From source");
    scroll_reader_to(&workspace, &mut vcx, from_source);
    assert_eq!(top_offset(&workspace, &mut vcx), Some(from_source));
    let before = painted(&workspace, &mut vcx);
    assert!(before.contains(&(from_source, 0.0)), "{before:?}");

    // The link lands on b.md's "## Usage", a quarter of the view down.
    follow(&workspace, &mut vcx, "b.md#usage", Disposition::InPlace);
    assert_eq!(file_of(&workspace, &mut vcx), "b.md");
    let usage = heading_offset(&b, "## Usage");
    let viewport = reader(&workspace, &mut vcx).read_with(&vcx, |r, _| r.viewport_height());
    assert!(viewport > 0.0);
    let arrived = painted(&workspace, &mut vcx);
    let at = arrived
        .iter()
        .find(|(offset, _)| *offset == usage)
        .map(|&(_, y)| y)
        .unwrap_or_else(|| panic!("## Usage is painted: {arrived:?}"));
    assert!(
        (at - viewport / 4.0).abs() < 0.5,
        "## Usage at {at} px, expected {} px",
        viewport / 4.0
    );
    assert_eq!(can_travel(&workspace, &mut vcx), (true, false));

    // Back shows a.md exactly as it was left.
    cx.dispatch_action(window, crate::GoBack);
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "a.md");
    assert_eq!(top_offset(&workspace, &mut vcx), Some(from_source));
    assert_eq!(painted(&workspace, &mut vcx), before);
    assert_eq!(can_travel(&workspace, &mut vcx), (false, true));
}

/// Each Back or Forward is noted once for the navigation log, and the
/// Reading view that shows it takes the note.
fn timed_once(cause: Option<Cause>) {
    let expected: Vec<Cause> = cause.into_iter().collect();
    assert_eq!(follow::drain_records(), (expected.clone(), expected));
}

#[gpui::test]
fn the_history_buttons_keys_and_mouse_buttons(cx: &mut TestAppContext) {
    let dir = temp(&[("a.md", "# A\n"), ("b.md", "# B\n")]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);

    // Disabled buttons do nothing.
    let back = center(&mut vcx, "history-back");
    let forward = center(&mut vcx, "history-forward");
    follow(&workspace, &mut vcx, "b.md", Disposition::InPlace);
    assert_eq!(can_travel(&workspace, &mut vcx), (true, false));
    follow::drain_records();
    vcx.simulate_click(forward, Modifiers::none());
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "b.md");
    timed_once(None);

    // Enabled, they go.
    vcx.simulate_click(back, Modifiers::none());
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "a.md");
    timed_once(Some(Cause::Back));
    assert_eq!(can_travel(&workspace, &mut vcx), (false, true));
    vcx.simulate_click(forward, Modifiers::none());
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "b.md");
    timed_once(Some(Cause::Forward));

    // The keys.
    let (back_key, forward_key) = if cfg!(target_os = "macos") {
        ("cmd-[", "cmd-]")
    } else {
        ("alt-left", "alt-right")
    };
    vcx.simulate_keystrokes(back_key);
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "a.md");
    timed_once(Some(Cause::Back));
    vcx.simulate_keystrokes(forward_key);
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "b.md");
    timed_once(Some(Cause::Forward));

    // The mouse's back and forward buttons, over the document.
    let doc = point(px(WIDTH / 2.0), px(HEIGHT / 2.0));
    let nav = |d| MouseButton::Navigate(d);
    vcx.simulate_mouse_down(doc, nav(NavigationDirection::Back), Modifiers::none());
    vcx.simulate_mouse_up(doc, nav(NavigationDirection::Back), Modifiers::none());
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "a.md");
    timed_once(Some(Cause::Back));
    vcx.simulate_mouse_down(doc, nav(NavigationDirection::Forward), Modifiers::none());
    vcx.simulate_mouse_up(doc, nav(NavigationDirection::Forward), Modifiers::none());
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "b.md");
    timed_once(Some(Cause::Forward));
}

#[gpui::test]
fn closed_tabs_reopen_with_their_history(cx: &mut TestAppContext) {
    let dir = temp(&[("a.md", "# A\n"), ("b.md", "# B\n"), ("c.md", "# C\n")]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    vcx.update(|window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.open_tab(OpenTarget::new(dir.path().join("b.md")), window, cx);
        });
    });
    draw(&mut vcx);
    follow(&workspace, &mut vcx, "c.md", Disposition::InPlace);
    assert_eq!(tab_names(&workspace, &mut vcx), ["a.md", "c.md"]);

    // The close button closes its tab; the neighbour becomes active.
    let close = center(&mut vcx, "close-tab-1");
    vcx.simulate_click(close, Modifiers::none());
    draw(&mut vcx);
    assert_eq!(tab_names(&workspace, &mut vcx), ["a.md"]);
    assert_eq!(file_of(&workspace, &mut vcx), "a.md");

    // Mod+Shift+T brings it back where it was, with its history.
    vcx.simulate_keystrokes("secondary-shift-t");
    draw(&mut vcx);
    assert_eq!(tab_names(&workspace, &mut vcx), ["a.md", "c.md"]);
    assert_eq!(file_of(&workspace, &mut vcx), "c.md");
    assert_eq!(can_travel(&workspace, &mut vcx), (true, false));
    cx.dispatch_action(window, crate::GoBack);
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "b.md");

    // Mod+W closes the active tab; closing the last leaves an empty tab.
    vcx.simulate_keystrokes("secondary-w");
    draw(&mut vcx);
    assert_eq!(tab_names(&workspace, &mut vcx), ["a.md"]);
    vcx.simulate_keystrokes("secondary-w");
    draw(&mut vcx);
    assert_eq!(tab_names(&workspace, &mut vcx), [""]);
    vcx.simulate_keystrokes("secondary-w");
    draw(&mut vcx);
    assert_eq!(tab_names(&workspace, &mut vcx), [""]);

    // Reopening fills the empty tab, then inserts.
    vcx.simulate_keystrokes("secondary-shift-t");
    draw(&mut vcx);
    assert_eq!(tab_names(&workspace, &mut vcx), ["a.md"]);
    vcx.simulate_keystrokes("secondary-shift-t");
    draw(&mut vcx);
    assert_eq!(tab_names(&workspace, &mut vcx), ["a.md", "b.md"]);
    // b.md came back with the way forward to c.md.
    assert_eq!(can_travel(&workspace, &mut vcx), (false, true));
    let reopened =
        vcx.update(|window, cx| workspace.update(cx, |ws, cx| ws.reopen_closed_tab(window, cx)));
    assert!(!reopened, "nothing is left to reopen");
}

#[gpui::test]
fn the_outline_lists_headings_and_follows_the_reading(cx: &mut TestAppContext) {
    let doc = outlined_doc();
    let dir = temp(&[("guide.md", &doc)]);
    let (window, workspace) = open(cx, &dir.path().join("guide.md"));
    let mut vcx = drawn(window, cx);
    // The panel is not there until it is called.
    assert!(workspace.read_with(&vcx, |ws, _| ws.outline.is_none()));
    cx.dispatch_action(window, TogglePanel);
    draw(&mut vcx);

    let outline = outline(&workspace, &mut vcx);
    let rows = outline.read_with(&vcx, |o, _| o.rows());
    let expected = [
        (0, "Guide"),
        (1, "Install"),
        (2, "From source"),
        (1, "Usage"),
        (2, "Links"),
        (3, "Fragments"),
        (1, "Reference"),
    ];
    let expected: Vec<_> = expected.iter().map(|&(d, t)| (d, t.to_owned())).collect();
    assert_eq!(rows, expected);
    assert_eq!(outline.read_with(&vcx, |o, _| o.current()), Some(0));
    let row = vcx.debug_bounds("outline-row-2").expect("rows are drawn");
    assert_eq!(
        f32::from(row.size.height),
        crate::metrics::size::OUTLINE_ROW
    );

    // Wheel down into "### Links": the highlight and the breadcrumb follow.
    let target = heading_offset(&doc, "### Links");
    let at = point(px(WIDTH / 2.0 - 124.0), px(HEIGHT / 2.0));
    for _ in 0..200 {
        if top_offset(&workspace, &mut vcx).is_some_and(|o| o >= target) {
            break;
        }
        vcx.simulate_event(ScrollEvent {
            position: at,
            delta: ScrollDelta::Pixels(point(px(0.0), px(-60.0))),
            ..ScrollEvent::default()
        });
        draw(&mut vcx);
    }
    let top = top_offset(&workspace, &mut vcx).expect("scrolled");
    assert!(top >= target, "{top} < {target}");
    let current = outline.read_with(&vcx, |o, _| o.current());
    let expected = if top >= heading_offset(&doc, "#### Fragments") {
        5
    } else {
        4
    };
    assert_eq!(current, Some(expected));
    let crumbs = workspace.read_with(&vcx, |ws, _| {
        status_bar::breadcrumb_items(ws.active_tab(), ws.active_doc())
    });
    let last = crumbs.last().map(ToString::to_string);
    assert_eq!(last.as_deref(), Some(expected_text(expected)));
    // Scrolling is not history.
    assert_eq!(can_travel(&workspace, &mut vcx), (false, false));

    // The section head folds the list.
    let head = center(&mut vcx, "outline-head");
    vcx.simulate_click(head, Modifiers::none());
    draw(&mut vcx);
    assert!(!outline.read_with(&vcx, |o, _| o.is_open()));
    assert!(vcx.debug_bounds("outline-row-0").is_none());
}

#[gpui::test]
fn a_long_outline_scrolls_inside_the_panel(cx: &mut TestAppContext) {
    let doc: String = (0..200)
        .map(|i| format!("## Heading {i}\n\nText.\n\n"))
        .collect();
    let dir = temp(&[("long.md", &doc)]);
    let (window, _) = open(cx, &dir.path().join("long.md"));
    let mut vcx = drawn(window, cx);
    cx.dispatch_action(window, TogglePanel);
    draw(&mut vcx);
    let panel = vcx.debug_bounds("outline-panel").expect("outline is drawn");
    let backlinks = vcx
        .debug_bounds("backlinks-head")
        .expect("backlinks head is drawn");
    let bottom = HEIGHT - crate::metrics::size::STATUS_BAR;
    assert!(
        f32::from(backlinks.bottom()) <= bottom + 0.5,
        "{backlinks:?}"
    );
    assert!(f32::from(panel.bottom()) <= f32::from(backlinks.top()) + 0.5);
    assert!(vcx.debug_bounds("outline-row-0").is_some());
    assert!(vcx.debug_bounds("outline-row-199").is_none());
}

fn expected_text(heading: usize) -> &'static str {
    [
        "Guide",
        "Install",
        "From source",
        "Usage",
        "Links",
        "Fragments",
        "Reference",
    ][heading]
}

#[gpui::test]
fn a_click_on_an_outline_row_scrolls_there(cx: &mut TestAppContext) {
    let doc = outlined_doc();
    let dir = temp(&[("guide.md", &doc)]);
    let (window, workspace) = open(cx, &dir.path().join("guide.md"));
    let mut vcx = drawn(window, cx);
    cx.dispatch_action(window, TogglePanel);
    draw(&mut vcx);

    let row = center(&mut vcx, "outline-row-3");
    vcx.simulate_click(row, Modifiers::none());
    draw(&mut vcx);
    let usage = heading_offset(&doc, "## Usage");
    assert_eq!(top_offset(&workspace, &mut vcx), Some(usage));
    let outline = outline(&workspace, &mut vcx);
    assert_eq!(outline.read_with(&vcx, |o, _| o.current()), Some(3));
    // The jump is history: Back returns to the top.
    assert_eq!(can_travel(&workspace, &mut vcx), (true, false));
    cx.dispatch_action(window, crate::GoBack);
    draw(&mut vcx);
    assert_eq!(top_offset(&workspace, &mut vcx), Some(0));
    assert_eq!(outline.read_with(&vcx, |o, _| o.current()), Some(0));
}

#[gpui::test]
fn the_status_bar_shows_the_place_words_and_profile(cx: &mut TestAppContext) {
    let dir = temp(&[(
        "note.md",
        "# Hello world\n\nOne two, three — four.\n\n- five\n- six\n\n```\nseven eight\n```\n",
    )]);
    let (window, workspace) = open(cx, &dir.path().join("note.md"));
    let mut vcx = drawn(window, cx);
    let folder = dir
        .path()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .expect("folder name");
    let texts = |vcx: &mut VisualTestContext| {
        workspace.read_with(vcx, |ws, cx| {
            let strings = |v: Vec<gpui::SharedString>| {
                v.into_iter().map(|s| s.to_string()).collect::<Vec<_>>()
            };
            let editor = ws.editor_facts(cx);
            (
                strings(status_bar::breadcrumb_items(
                    ws.active_tab(),
                    ws.active_doc(),
                )),
                strings(status_bar::facts(
                    ws.active_tab(),
                    ws.active_doc(),
                    editor.as_ref(),
                )),
            )
        })
    };
    let (crumbs, facts) = texts(&mut vcx);
    assert_eq!(crumbs, [folder.as_str(), "note.md", "Hello world"]);
    assert_eq!(facts, ["10 words · 1 min read"]);
    assert_eq!(
        status_bar::profile_name(crate::reader::PROFILE),
        "CommonMark"
    );

    cx.dispatch_action(window, ToggleEditMode);
    draw(&mut vcx);
    // The editor counts the words as Preview does.
    let (_, facts) = texts(&mut vcx);
    assert_eq!(facts, ["Ln 1, Col 1", "10 words", "UTF-8", "LF"]);
}

#[test]
fn the_navigation_chrome_uses_tokens_only() {
    let sources = [
        ("history.rs", include_str!("history.rs")),
        ("outline_panel.rs", include_str!("outline_panel.rs")),
        ("panels.rs", include_str!("panels.rs")),
        ("status_bar.rs", include_str!("status_bar.rs")),
        ("title_row.rs", include_str!("title_row.rs")),
    ];
    for (name, source) in sources {
        let code: String = source
            .lines()
            .take_while(|line| !line.starts_with("#[cfg(test)]"))
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            super::files_panel_tests::untokened(&code),
            Vec::<String>::new(),
            "{name}"
        );
    }
}
