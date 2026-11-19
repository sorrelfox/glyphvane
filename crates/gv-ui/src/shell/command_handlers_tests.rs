// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Every command the menus and the command palette offer has a handler
//! somewhere in a window, in headless windows over a temporary vault: a
//! command without one would show as an item that never does anything.

use std::collections::BTreeSet;

use gpui::{
    AppContext as _, Bounds, Entity, Focusable as _, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};
use gv_settings::keymap::CommandRegistry;

use super::{OpenTarget, ViewMode, Workspace};
use crate::chrome::WindowChrome;
use crate::keymap::actions::workspace as commands;
use crate::menu::model::{ContextMenu, app_menus, context_menu};
use crate::{AppSettings, ThemeChoice};

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

/// The commands with a handler where the focus is now.
fn available(vcx: &mut VisualTestContext) -> BTreeSet<&'static str> {
    vcx.update(|window, cx| {
        window
            .available_actions(cx)
            .iter()
            .filter_map(|a| crate::keymap::actions::command_of(a.as_ref()))
            .collect()
    })
}

/// The note the window opens: a task item with links (a note, a web
/// page, a missing note, an ambiguous one), an ordered list and a table.
const NOTE: &str = "# A\n\n- [ ] task [b](b.md) [web](https://example.com/) [[gone]] [[dup]]\n\n1. one\n3. two\n\n| a | b | c |\n|---|---|---|\n| d | e | f |\n| g | h | i |\n| j | k | l |\n";

/// Commands handled only on another platform.
const OTHER_PLATFORMS: &[&str] = &[
    #[cfg(not(target_os = "macos"))]
    "app.install_cli",
];

/// Where the focus goes before the handlers are listed.
#[derive(Clone, Copy)]
enum Focus {
    Reader,
    /// The editor, with the caret at the start of this text in [`NOTE`].
    Editor(&'static str),
    Files,
    Canvas,
}

fn focus(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext, at: Focus) {
    workspace.update_in(vcx, |ws, window, cx| {
        let tab = ws.active_tab();
        let handle = match at {
            Focus::Reader => tab.reader().map(|r| r.read(cx).focus_handle(cx)),
            Focus::Editor(text) => tab.editor().cloned().map(|e| {
                let offset = NOTE.find(text).expect("in the note");
                e.update(cx, |e, cx| e.set_cursor(offset, cx));
                e.read(cx).focus_handle(cx)
            }),
            Focus::Files => ws.files.as_ref().map(|f| f.read(cx).focus_handle(cx)),
            Focus::Canvas => tab.canvas.as_ref().map(|c| c.read(cx).focus_handle(cx)),
        };
        handle.expect("the focus target").focus(window, cx);
    });
    draw(vcx);
}

#[gpui::test]
fn every_offered_command_has_a_handler(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join(".obsidian")).expect("marker");
    std::fs::write(dir.path().join("a.md"), NOTE).expect("write");
    std::fs::write(dir.path().join("b.md"), "# B\n").expect("write");
    for folder in ["x", "y"] {
        std::fs::create_dir(dir.path().join(folder)).expect("folder");
        std::fs::write(dir.path().join(folder).join("dup.md"), "# Dup\n").expect("write");
    }
    std::fs::write(
        dir.path().join("c.canvas"),
        r#"{"nodes":[{"id":"1","type":"text","text":"x","x":0,"y":0,"width":100,"height":60}],"edges":[]}"#,
    )
    .expect("write");
    let file = dir.path().join("a.md");
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1280.0), px(800.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |window, cx| {
            cx.new(|cx| {
                Workspace::with_chrome(
                    Some(OpenTarget::new(&file)),
                    WindowChrome::Linux,
                    window,
                    cx,
                )
            })
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let mut handled = BTreeSet::new();
    // The vault, for the commands about the vault's files.
    vcx.dispatch_action(commands::ShowFiles);
    draw(&mut vcx);
    focus(&workspace, &mut vcx, Focus::Files);
    handled.extend(available(&mut vcx));
    focus(&workspace, &mut vcx, Focus::Reader);
    handled.extend(available(&mut vcx));
    // Edit and Split, with the caret on a task with links and in a table.
    for mode in [ViewMode::Edit, ViewMode::Split] {
        workspace.update(&mut vcx, |ws, cx| ws.show_mode(mode, cx));
        draw(&mut vcx);
        for at in [
            "task", "b](b.md)", "web](", "gone]]", "dup]]", "two", "e |", "h |",
        ] {
            focus(&workspace, &mut vcx, Focus::Editor(at));
            handled.extend(available(&mut vcx));
        }
    }
    // A canvas.
    let canvas = dir.path().join("c.canvas");
    workspace.update_in(&mut vcx, |ws, window, cx| {
        ws.open_tab(OpenTarget::new(canvas), window, cx);
    });
    draw(&mut vcx);
    focus(&workspace, &mut vcx, Focus::Canvas);
    handled.extend(available(&mut vcx));
    handled.extend(OTHER_PLATFORMS);

    let registry = CommandRegistry::builtin();
    let mut offered: BTreeSet<&str> = registry.iter().map(|c| c.id().as_str()).collect();
    for platform in gv_settings::keymap::Platform::ALL {
        for menu in app_menus(platform) {
            offered.extend(menu.items().map(|i| i.command));
        }
    }
    for kind in [
        ContextMenu::Link,
        ContextMenu::Editor,
        ContextMenu::FileTree,
        ContextMenu::Tab,
        ContextMenu::MoreActions,
    ] {
        offered.extend(context_menu(kind).items().map(|i| i.command));
    }
    let missing: Vec<&str> = offered.difference(&handled).copied().collect();
    assert!(missing.is_empty(), "no handler: {missing:#?}");
}
