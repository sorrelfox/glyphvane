// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Files panel inside a window: lazy indexing, rows, clicks and keys.

use std::path::{Path, PathBuf};

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Focusable as _, Modifiers, Pixels,
    TestAppContext, VisualTestContext, WindowOptions,
};

use super::files_panel::FilesPanel;
use super::vault::VaultStatus;
use super::{OpenTarget, Workspace};
use crate::chrome::WindowChrome;
use crate::metrics::spacing::{list, panel_head};
use crate::metrics::{HAIRLINE, size};
use crate::{AppSettings, ThemeChoice, ToggleEditMode, ToggleSidebar};

/// An Obsidian vault with hidden and excluded entries and every file kind.
fn temp_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    for folder in [".obsidian", "Notes/Sub", "Empty folder", "node_modules/pkg"] {
        std::fs::create_dir_all(root.join(folder)).expect("folder");
    }
    for file in [
        "Notes/inner.md",
        "Notes/Sub/deep.md",
        "b.md",
        "a10.md",
        "a2.md",
        ".hidden.md",
        "node_modules/pkg/x.md",
        "img.png",
        "x.canvas",
        "y.base",
    ] {
        std::fs::write(root.join(file), "# x\n").expect("file");
    }
    dir
}

fn open_window(cx: &mut TestAppContext, file: &Path) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let target = Some(OpenTarget::new(file));
        gpui::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    })
}

fn drawn(window: AnyWindowHandle, cx: &mut TestAppContext) -> VisualTestContext {
    let mut vcx = VisualTestContext::from_window(window, cx);
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    vcx
}

fn files(workspace: &Entity<Workspace>, cx: &TestAppContext) -> Entity<FilesPanel> {
    workspace
        .read_with(cx, |ws, _| ws.files.clone())
        .expect("files panel exists")
}

fn rows(workspace: &Entity<Workspace>, cx: &TestAppContext) -> Vec<String> {
    files(workspace, cx).read_with(cx, |panel, _| {
        panel
            .visible_rows()
            .into_iter()
            .map(|(depth, name)| format!("{}{name}", "  ".repeat(depth)))
            .collect()
    })
}

fn selected(workspace: &Entity<Workspace>, cx: &TestAppContext) -> Option<String> {
    files(workspace, cx).read_with(cx, |panel, _| panel.selected_name())
}

fn active_path(workspace: &Entity<Workspace>, cx: &TestAppContext) -> Option<PathBuf> {
    workspace.read_with(cx, |ws, _| ws.active_tab().path.clone())
}

fn row_bounds(vcx: &mut VisualTestContext, row: usize) -> Bounds<Pixels> {
    const ROWS: [&str; 10] = [
        "file-row-0",
        "file-row-1",
        "file-row-2",
        "file-row-3",
        "file-row-4",
        "file-row-5",
        "file-row-6",
        "file-row-7",
        "file-row-8",
        "file-row-9",
    ];
    vcx.debug_bounds(ROWS[row])
        .unwrap_or_else(|| panic!("row {row} is drawn"))
}

fn click(vcx: &mut VisualTestContext, bounds: Bounds<Pixels>) {
    vcx.simulate_click(bounds.center(), Modifiers::none());
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
}

fn px_eq(value: Pixels, expected: f32) -> bool {
    (f32::from(value) - expected).abs() < 0.01
}

/// Opens the sidebar of a window for `Notes/inner.md` and waits for the
/// index.
fn open_with_sidebar(
    cx: &mut TestAppContext,
    vault: &Path,
) -> (AnyWindowHandle, Entity<Workspace>, VisualTestContext) {
    let (window, workspace) = open_window(cx, &vault.join("Notes/inner.md"));
    cx.dispatch_action(window, ToggleSidebar);
    let vcx = drawn(window, cx);
    (window, workspace, vcx)
}

const EXPECTED: [&str; 10] = [
    "Empty folder",
    "Notes",
    "  Sub",
    "  inner.md",
    "a2.md",
    "a10.md",
    "b.md",
    "img.png",
    "x.canvas",
    "y.base",
];

#[gpui::test]
fn a_file_window_indexes_nothing_until_the_sidebar_opens(cx: &mut TestAppContext) {
    let vault = temp_vault();
    let (window, workspace) = open_window(cx, &vault.path().join("Notes/inner.md"));
    cx.dispatch_action(window, ToggleEditMode);
    drawn(window, cx);
    let (has_vault, has_files) =
        workspace.read_with(cx, |ws, _| (ws.vault.is_some(), ws.files.is_some()));
    assert!(
        !has_vault && !has_files,
        "nothing is indexed before the sidebar or the right panel (Backlinks)"
    );

    cx.dispatch_action(window, ToggleSidebar);
    drawn(window, cx);
    let vault_entity = workspace
        .read_with(cx, |ws, _| ws.vault.clone())
        .expect("the sidebar creates the vault");
    let (status, root, built) = vault_entity.read_with(cx, |v, _| {
        (v.status().clone(), v.root().to_path_buf(), v.build_time())
    });
    assert_eq!(status, VaultStatus::Ready);
    assert_eq!(root, vault.path());
    assert!(built.is_some());

    // Hiding and showing the sidebar again does not rebuild anything.
    cx.dispatch_action(window, ToggleSidebar);
    cx.dispatch_action(window, ToggleSidebar);
    let again = workspace.read_with(cx, |ws, _| ws.vault.clone());
    assert_eq!(again.map(|v| v.entity_id()), Some(vault_entity.entity_id()));
}

#[gpui::test]
fn rows_are_sorted_and_the_active_file_is_revealed(cx: &mut TestAppContext) {
    let vault = temp_vault();
    let (_, workspace, mut vcx) = open_with_sidebar(cx, vault.path());
    assert_eq!(rows(&workspace, cx), EXPECTED);
    assert_eq!(selected(&workspace, cx).as_deref(), Some("inner.md"));

    let sidebar = vcx.debug_bounds("sidebar").expect("sidebar is drawn");
    let title = vcx
        .debug_bounds("vault-switcher")
        .expect("vault name is drawn");
    let head_middle = f32::from(sidebar.origin.y) + size::TITLE_ROW + size::PANEL_HEADER / 2.0;
    assert!(px_eq(title.center().y, head_middle), "{title:?}");
    assert!(px_eq(
        title.origin.x - sidebar.origin.x,
        panel_head::PAD_LEFT
    ));
    let top = row_bounds(&mut vcx, 0);
    let nested = row_bounds(&mut vcx, 2);
    assert!(px_eq(top.size.height, size::LIST_ROW));
    assert!(px_eq(top.origin.x - sidebar.origin.x, size::LIST_ROW_INSET));
    assert!(px_eq(
        top.right(),
        f32::from(sidebar.right()) - size::LIST_ROW_INSET - HAIRLINE
    ));
    // Rows touch; nesting indents the content, not the row.
    assert!(px_eq(nested.origin.x, f32::from(top.origin.x)));
    assert!(px_eq(
        row_bounds(&mut vcx, 1).origin.y,
        f32::from(top.origin.y) + size::LIST_ROW
    ));
    assert!(px_eq(
        top.origin.y,
        f32::from(sidebar.origin.y) + size::TITLE_ROW + size::PANEL_HEADER + list::PAD_Y
    ));
}

#[gpui::test]
fn clicking_a_folder_opens_it_and_collapse_all_closes_everything(cx: &mut TestAppContext) {
    let vault = temp_vault();
    let (_, workspace, mut vcx) = open_with_sidebar(cx, vault.path());

    let sub = row_bounds(&mut vcx, 2);
    click(&mut vcx, sub);
    assert_eq!(
        rows(&workspace, cx)[..5],
        [
            "Empty folder",
            "Notes",
            "  Sub",
            "    deep.md",
            "  inner.md"
        ]
    );
    assert_eq!(selected(&workspace, cx).as_deref(), Some("Sub"));

    let collapse = vcx
        .debug_bounds("collapse-all")
        .expect("collapse all is drawn");
    click(&mut vcx, collapse);
    assert_eq!(
        rows(&workspace, cx),
        [
            "Empty folder",
            "Notes",
            "a2.md",
            "a10.md",
            "b.md",
            "img.png",
            "x.canvas",
            "y.base"
        ]
    );
    assert_eq!(selected(&workspace, cx).as_deref(), Some("Notes"));
}

#[gpui::test]
fn clicking_a_file_opens_it_as_a_tab(cx: &mut TestAppContext) {
    let vault = temp_vault();
    let (_, workspace, mut vcx) = open_with_sidebar(cx, vault.path());

    let b = row_bounds(&mut vcx, 6);
    click(&mut vcx, b);
    assert_eq!(active_path(&workspace, cx), Some(vault.path().join("b.md")));
    assert_eq!(workspace.read_with(cx, |ws, _| ws.tabs().len()), 2);
    assert_eq!(selected(&workspace, cx).as_deref(), Some("b.md"));

    // A file that is already open activates its tab.
    let inner = row_bounds(&mut vcx, 3);
    click(&mut vcx, inner);
    assert_eq!(
        active_path(&workspace, cx),
        Some(vault.path().join("Notes/inner.md"))
    );
    assert_eq!(workspace.read_with(cx, |ws, _| ws.tabs().len()), 2);
    // The panels stay as they are.
    assert!(workspace.read_with(cx, |ws, _| ws.sidebar_open && !ws.panel_open));
}

#[gpui::test]
fn arrow_keys_and_enter_drive_the_tree(cx: &mut TestAppContext) {
    let vault = temp_vault();
    let (_, workspace, mut vcx) = open_with_sidebar(cx, vault.path());
    let panel = files(&workspace, cx);
    vcx.update(|window, cx| {
        let focus = panel.read(cx).focus_handle(cx);
        focus.focus(window, cx);
    });

    // From inner.md: up to Sub, open it, into deep.md, open the file.
    vcx.simulate_keystrokes("up");
    assert_eq!(selected(&workspace, cx).as_deref(), Some("Sub"));
    vcx.simulate_keystrokes("right");
    assert_eq!(rows(&workspace, cx)[3], "    deep.md");
    vcx.simulate_keystrokes("right");
    assert_eq!(selected(&workspace, cx).as_deref(), Some("deep.md"));
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(
        active_path(&workspace, cx),
        Some(vault.path().join("Notes/Sub/deep.md"))
    );

    // Left goes to the parent, then closes it.
    vcx.simulate_keystrokes("left");
    assert_eq!(selected(&workspace, cx).as_deref(), Some("Sub"));
    vcx.simulate_keystrokes("left");
    assert_eq!(rows(&workspace, cx)[3], "  inner.md");
    vcx.simulate_keystrokes("down down");
    assert_eq!(selected(&workspace, cx).as_deref(), Some("a2.md"));
    // Enter on a folder opens or closes it.
    vcx.simulate_keystrokes("up up up enter");
    assert_eq!(selected(&workspace, cx).as_deref(), Some("Notes"));
    assert_eq!(rows(&workspace, cx).len(), 8);
}

#[gpui::test]
fn a_new_snapshot_redraws_the_tree(cx: &mut TestAppContext) {
    let vault = temp_vault();
    let (_, workspace, _vcx) = open_with_sidebar(cx, vault.path());
    let vault_entity = workspace
        .read_with(cx, |ws, _| ws.vault.clone())
        .expect("vault");
    let next = vault_entity.read_with(cx, |v, _| {
        v.snapshot()
            .apply(&[gv_vault::Change::Remove {
                path: gv_vault::VaultPath::new("b.md").expect("path"),
            }])
            .expect("apply")
    });
    vault_entity.update(cx, |v, cx| v.publish(next, cx));
    cx.run_until_parked();
    let now = rows(&workspace, cx);
    assert!(!now.iter().any(|r| r == "b.md"), "{now:?}");
    assert_eq!(now.len(), EXPECTED.len() - 1);
    assert_eq!(selected(&workspace, cx).as_deref(), Some("inner.md"));
}

#[gpui::test]
fn a_window_without_a_file_has_no_vault(cx: &mut TestAppContext) {
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        gpui::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(None, WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    });
    cx.dispatch_action(window, ToggleSidebar);
    let mut vcx = drawn(window, cx);
    assert!(workspace.read_with(cx, |ws, _| ws.vault.is_none()));
    assert!(vcx.debug_bounds("files-message").is_some());
}

/// Ways a size or colour can bypass the metrics and the theme: GPUI's
/// fixed-step helpers (`.px_2()`, `.h_8()`, `.text_sm()`, `.rounded_md()`),
/// colour constructors and literals, and `px(` around a number.
pub(super) fn untokened(code: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (i, _) in code.match_indices('.') {
        let call: String = code[i + 1..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let fixed_step = call.rsplit_once('_').is_some_and(|(helper, step)| {
            let spacing = [
                "p", "px", "py", "pt", "pb", "pl", "pr", "m", "mx", "my", "mt", "mb", "ml", "mr",
                "w", "h", "size", "gap", "min_w", "min_h", "max_w", "max_h",
            ];
            (spacing.contains(&helper)
                && step.starts_with(|c: char| c.is_ascii_digit())
                && step != "0")
                || (["text", "rounded"].contains(&helper)
                    && ["xs", "sm", "md", "lg", "xl", "base"].contains(&step))
        });
        if fixed_step {
            found.push(format!(".{call}()"));
        }
    }
    for banned in ["rgb(", "rgba(", "hsla(", "Hsla {", "0x", "\"#"] {
        if code.contains(banned) {
            found.push(banned.to_owned());
        }
    }
    for (i, _) in code.match_indices("px(") {
        if code[i + 3..]
            .trim_start()
            .starts_with(|c: char| c.is_ascii_digit())
        {
            found.push(code[i..(i + 12).min(code.len())].to_owned());
        }
    }
    found
}

/// The panel's code takes every colour from the theme and every size from
/// the metrics.
#[test]
fn the_files_panel_uses_tokens_only() {
    let bad = "div().px_2().h(px(24.0)).text_sm().bg(rgb(0xffffff)).rounded_md().min_w_0()";
    assert_eq!(untokened(bad).len(), 6, "{:?}", untokened(bad));

    let sources = [
        ("files_panel.rs", include_str!("files_panel.rs")),
        ("tree.rs", include_str!("tree.rs")),
        ("vault.rs", include_str!("vault.rs")),
    ];
    for (name, source) in sources {
        let code: String = source
            .lines()
            .take_while(|line| !line.starts_with("#[cfg(test)]"))
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(untokened(&code), Vec::<String>::new(), "{name}");
    }
}
