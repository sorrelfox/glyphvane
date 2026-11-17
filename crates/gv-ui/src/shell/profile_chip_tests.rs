// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The profile chip, its menu and the dialect hint.

use std::path::Path;

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Modifiers, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};
use gv_profile::{ProfileId, Reason};

use super::{OpenTarget, Workspace};
use crate::chrome::WindowChrome;
use crate::files::notice::Topic;
use crate::profile::Scope;
use crate::{AppSettings, ThemeChoice, ToggleEditMode};

fn open(cx: &mut TestAppContext, file: &Path) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let target = Some(OpenTarget::new(file));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(900.0), px(600.0)));
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
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn click(vcx: &mut VisualTestContext, selector: &str) {
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    let at = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"))
        .center();
    vcx.simulate_click(at, Modifiers::none());
    draw(vcx);
}

fn drawn(vcx: &mut VisualTestContext, selector: &'static str) -> bool {
    vcx.debug_bounds(selector).is_some()
}

/// The active tab's profile and why.
fn profile(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> (ProfileId, Reason) {
    workspace.read_with(vcx, |ws, _| {
        let p = ws.active_tab().profile.as_ref().expect("detected");
        (p.id(), p.detection.reason.clone())
    })
}

fn notice_topic(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> Option<Topic> {
    workspace.read_with(vcx, |ws, _| ws.active_tab().notice().map(|n| n.topic))
}

#[gpui::test]
fn the_chip_shows_the_detected_profile_and_switches_it(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join(".glyphvane.toml"),
        "profile = \"gfm-file\"\n[switches]\nfront_matter_toml = true\n",
    )
    .expect("write");
    let note = dir.path().join("note.md");
    std::fs::write(&note, "# Notes\n\nSome ~~old~~ text.\n").expect("write");
    let (window, workspace) = open(cx, &note);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);

    assert!(drawn(&mut vcx, "profile-chip"));
    let (id, reason) = profile(&workspace, &mut vcx);
    assert_eq!(id, ProfileId::GfmFile);
    assert!(matches!(reason, Reason::FolderConfig(_)));
    let tooltip = workspace.read_with(&vcx, |ws, _| {
        ws.active_tab()
            .profile
            .as_ref()
            .expect("detected")
            .tooltip()
    });
    assert!(tooltip.starts_with("Set in `"), "{tooltip}");
    assert_eq!(super::status_bar::profile_name(id), "GitHub");

    // The Source view gets the compiled profile: the toml's TOML front
    // matter switch reaches the parser options.
    cx.dispatch_action(window, ToggleEditMode);
    draw(&mut vcx);
    let editor = workspace.read_with(&vcx, |ws, _| ws.active_tab().editor().cloned());
    let editor = editor.expect("Edit mode made the Source view");
    let commands = editor.read_with(&vcx, |e, _| e.markdown_commands());
    assert_eq!(commands.flavor, gv_editor::Flavor::GfmFile);

    // The menu: pick CommonMark for this file.
    click(&mut vcx, "profile-chip");
    assert!(workspace.read_with(&vcx, |ws, _| ws.profile_menu_open()));
    assert!(drawn(&mut vcx, "profile-menu"));
    click(&mut vcx, "profile-menu-item:commonmark");
    assert!(!workspace.read_with(&vcx, |ws, _| ws.profile_menu_open()));
    assert_eq!(
        profile(&workspace, &mut vcx),
        (ProfileId::CommonMark, Reason::FileOverride)
    );
    let commands = editor.read_with(&vcx, |e, _| e.markdown_commands());
    assert_eq!(commands.flavor, gv_editor::Flavor::CommonMark);

    // Obsidian has shipped: picking it switches the file's profile.
    click(&mut vcx, "profile-chip");
    click(&mut vcx, "profile-menu-item:obsidian");
    assert_eq!(
        profile(&workspace, &mut vcx),
        (ProfileId::Obsidian, Reason::FileOverride)
    );
    let commands = editor.read_with(&vcx, |e, _| e.markdown_commands());
    assert_eq!(commands.flavor, gv_editor::Flavor::Obsidian);

    // Back to Automatic: the toml decides again.
    click(&mut vcx, "profile-chip");
    click(&mut vcx, "profile-menu-item:auto");
    assert_eq!(profile(&workspace, &mut vcx).0, ProfileId::GfmFile);

    // A folder override sits below the toml in precedence.
    click(&mut vcx, "profile-chip");
    click(&mut vcx, "profile-menu-scope:folder");
    click(&mut vcx, "profile-menu-item:extended");
    assert_eq!(profile(&workspace, &mut vcx).0, ProfileId::GfmFile);
    let folder =
        vcx.update(|_, cx| crate::profile::ProfileOverrides::get(cx, &note, Scope::Folder));
    assert_eq!(folder, Some(ProfileId::Extended));

    // Explain detection shows the reasons in the infobar.
    click(&mut vcx, "profile-chip");
    click(&mut vcx, "profile-menu-command:Explain");
    assert_eq!(
        notice_topic(&workspace, &mut vcx),
        Some(Topic::ProfileExplained)
    );
    let title = workspace.read_with(&vcx, |ws, _| {
        ws.active_tab().notice().map(|n| n.title.to_string())
    });
    assert_eq!(title.as_deref(), Some("Profile: GFM (files) (gfm-file)."));
}

#[gpui::test]
fn a_click_outside_closes_the_menu(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let note = dir.path().join("note.md");
    std::fs::write(&note, "# Notes\n").expect("write");
    let (window, workspace) = open(cx, &note);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    // Nothing detected: the default profile.
    assert_eq!(
        profile(&workspace, &mut vcx),
        (ProfileId::Extended, Reason::Default)
    );
    click(&mut vcx, "profile-chip");
    assert!(workspace.read_with(&vcx, |ws, _| ws.profile_menu_open()));
    vcx.simulate_click(point(px(20.0), px(60.0)), Modifiers::none());
    draw(&mut vcx);
    assert!(!workspace.read_with(&vcx, |ws, _| ws.profile_menu_open()));
}

#[gpui::test]
fn another_dialect_shows_a_hint_until_dismissed_in_the_folder(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let obsidian_like = "# Notes\n\nText %%hidden%%.\n\n> [!tip]- Folded\n> body\n";
    let first = dir.path().join("first.md");
    let second = dir.path().join("second.md");
    std::fs::write(&first, obsidian_like).expect("write");
    std::fs::write(&second, obsidian_like).expect("write");
    let (window, workspace) = open(cx, &first);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);

    assert_eq!(notice_topic(&workspace, &mut vcx), Some(Topic::DialectHint));
    let (title, detail) = workspace.read_with(&vcx, |ws, _| {
        let n = ws.active_tab().notice().expect("hint");
        (
            n.title.to_string(),
            n.detail.as_ref().map(ToString::to_string),
        )
    });
    assert_eq!(title, "This looks like Obsidian.");
    assert_eq!(detail.as_deref(), Some("Switch profile?"));

    // Its button opens the chip's menu.
    click(&mut vcx, "infobar-action-0");
    assert!(workspace.read_with(&vcx, |ws, _| ws.profile_menu_open()));
    vcx.simulate_click(point(px(20.0), px(60.0)), Modifiers::none());
    draw(&mut vcx);

    click(&mut vcx, "infobar-dismiss");
    assert_eq!(notice_topic(&workspace, &mut vcx), None);

    // Dismissed for the folder: the next file there shows no hint.
    workspace.update_in(&mut vcx, |ws, window, cx| {
        ws.open_tab(OpenTarget::new(&second), window, cx);
    });
    draw(&mut vcx);
    let shows_second = workspace.read_with(&vcx, |ws, _| {
        ws.active_tab()
            .path()
            .is_some_and(|p| p.ends_with("second.md"))
    });
    assert!(shows_second);
    assert_eq!(notice_topic(&workspace, &mut vcx), None);
}

#[test]
fn the_chip_and_menu_use_tokens_only() {
    let code: String = include_str!("profile_chip.rs")
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        super::files_panel_tests::untokened(&code),
        Vec::<String>::new()
    );
}

#[test]
fn syntax_differences_list_where_the_profile_differs() {
    let obsidian = gv_profile::compile_builtin(ProfileId::Obsidian);
    let rows = super::profile_chip::syntax_differences(&obsidian);
    let wikilinks = rows
        .iter()
        .find(|r| r.label.as_ref() == "wikilinks: on")
        .expect("a wikilinks row");
    let detail = wikilinks.detail.as_ref().expect("the others' values");
    assert!(detail.contains("CommonMark off"), "{detail}");
    assert!(!detail.contains("Obsidian"), "{detail}");
    // A switch on which every shipped profile agrees is not listed.
    let commonmark = gv_profile::compile_builtin(ProfileId::CommonMark);
    let all = gv_profile::Switches::KEYS.len();
    assert!(super::profile_chip::syntax_differences(&commonmark).len() < all);
}

#[gpui::test]
fn show_syntax_differences_opens_the_list(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let note = dir.path().join("note.md");
    std::fs::write(&note, "# Notes\n").expect("write");
    let (window, _workspace) = open(cx, &note);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    click(&mut vcx, "profile-chip");
    click(&mut vcx, "profile-menu-command:Differences");
    draw(&mut vcx);
    assert!(vcx.debug_bounds("goto-popover").is_some());
}
