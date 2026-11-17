// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Files panel's file operations in headless windows: rename with the
//! link preview and Undo, case-only renames, refused names, move, duplicate,
//! trash, new folder, sort, filter, the vault switcher, and files with
//! unsaved changes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, AppContext as _, Entity, Focusable as _, TestAppContext, VisualTestContext,
    WindowOptions,
};
use gv_vault::refactor::Trash;

use super::files_panel::FilesPanel;
use super::vault_ops::TrashCan;
use super::{OpenTarget, Workspace};
use crate::chrome::WindowChrome;
use crate::file_prompt::{FilePrompt, RowStyle, prompt_of};
use crate::files::notice::{NoticeAction, Topic};
use crate::goto::{GotoPopover, goto_of};
use crate::keymap::actions::file as commands;
use crate::{AppSettings, ThemeChoice, ToggleEditMode, ToggleSidebar};

/// Removes files instead of trashing them, and remembers them.
#[derive(Default)]
struct FakeTrash(Mutex<Vec<PathBuf>>);

impl Trash for FakeTrash {
    fn delete(&self, path: &Path) -> Result<(), String> {
        std::fs::remove_file(path)
            .or_else(|_| std::fs::remove_dir_all(path))
            .map_err(|e| e.to_string())?;
        self.0.lock().expect("lock").push(path.to_path_buf());
        Ok(())
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    window: AnyWindowHandle,
    ws: Entity<Workspace>,
    vcx: VisualTestContext,
    trash: Arc<FakeTrash>,
}

impl Fixture {
    /// A vault of `files`, a window for `open` with the sidebar shown,
    /// the path and content indexes built.
    fn new(cx: &mut TestAppContext, files: &[(&str, &str)], open: &str) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        // A marker makes the temp folder the vault root.
        std::fs::create_dir_all(dir.path().join(".obsidian")).expect("marker");
        for (path, text) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().expect("parent")).expect("folders");
            std::fs::write(full, text).expect("write");
        }
        let trash = Arc::new(FakeTrash::default());
        let target = Some(OpenTarget::new(dir.path().join(open)));
        let (window, ws) = cx.update(|cx| {
            crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
            cx.set_global(TrashCan(trash.clone()));
            gpui::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
            })
            .expect("test window opens")
        });
        cx.dispatch_action(window, ToggleSidebar);
        let vcx = VisualTestContext::from_window(window, cx);
        let mut f = Self {
            dir,
            window,
            ws,
            vcx,
            trash,
        };
        f.draw();
        f.wait_for_links();
        f
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn draw(&mut self) {
        for _ in 0..2 {
            self.vcx.run_until_parked();
            self.vcx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    /// Waits (in real time: the content index has its own thread) until
    /// the content index has a snapshot.
    fn wait_for_links(&mut self) {
        let vault = self
            .ws
            .read_with(&self.vcx, |ws, _| ws.vault.clone())
            .expect("vault");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            self.vcx.run_until_parked();
            vault.update(&mut self.vcx, |v, cx| v.poll_content(cx));
            if vault.read_with(&self.vcx, |v, _| v.content().is_some()) {
                self.draw();
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the content index was not built in time");
    }

    fn dispatch(&mut self, action: impl gpui::Action) {
        self.vcx.dispatch_action(action);
        self.draw();
    }

    fn files(&self) -> Entity<FilesPanel> {
        self.ws
            .read_with(&self.vcx, |ws, _| ws.files.clone())
            .expect("files panel")
    }

    fn rows(&self) -> Vec<String> {
        self.files().read_with(&self.vcx, |panel, _| {
            panel
                .visible_rows()
                .into_iter()
                .map(|(depth, name)| format!("{}{name}", "  ".repeat(depth)))
                .collect()
        })
    }

    fn selected(&self) -> Option<String> {
        self.files().read_with(&self.vcx, |p, _| p.selected_name())
    }

    /// Gives the Files panel the keyboard with the row named `name`
    /// selected.
    fn select_row(&mut self, name: &str) {
        let files = self.files();
        let path = self.root().join(name);
        self.vcx.update(|window, cx| {
            files.update(cx, |f, cx| f.reveal_path(&path, cx));
            let focus = files.read(cx).focus_handle(cx);
            focus.focus(window, cx);
        });
        self.draw();
    }

    fn prompt(&mut self) -> Entity<FilePrompt> {
        self.vcx
            .update(|window, cx| prompt_of(window, cx))
            .expect("prompt")
    }

    fn prompt_open(&mut self) -> bool {
        let prompt = self.prompt();
        prompt.read_with(&self.vcx, |p, _| p.is_open())
    }

    fn type_name(&mut self, name: &str) {
        let prompt = self.prompt();
        self.vcx
            .update(|window, cx| prompt.update(cx, |p, cx| p.set_text(name, window, cx)));
        self.draw();
    }

    fn enter(&mut self) {
        self.vcx.simulate_keystrokes("enter");
        self.draw();
    }

    fn goto(&mut self) -> Entity<GotoPopover> {
        self.vcx
            .update(|window, cx| goto_of(window, cx))
            .expect("goto popover")
    }

    fn pick(&mut self, query: &str) {
        let goto = self.goto();
        self.vcx
            .update(|window, cx| goto.update(cx, |g, cx| g.set_query(query, window, cx)));
        self.enter();
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.root().join(path)).expect("read")
    }

    fn active_path(&self) -> Option<PathBuf> {
        self.ws
            .read_with(&self.vcx, |ws, _| ws.active_tab().path.clone())
    }

    fn notice(&self) -> Option<(Topic, String, bool)> {
        self.ws.read_with(&self.vcx, |ws, _| {
            ws.active_tab().notice().map(|n| {
                (
                    n.topic,
                    n.title.to_string(),
                    n.offers(&NoticeAction::UndoFileOp),
                )
            })
        })
    }

    fn press(&mut self, action: &NoticeAction) {
        self.ws.update(&mut self.vcx, |ws, cx| {
            assert!(ws.answer(action, cx));
        });
        self.draw();
    }
}

#[gpui::test]
fn rename_previews_the_link_updates_and_undo_reverts_them(cx: &mut TestAppContext) {
    let mut f = Fixture::new(
        cx,
        &[
            ("Old.md", "# Old\n"),
            ("a.md", "See [[Old]] and [x](Old.md#old).\n"),
        ],
        "Old.md",
    );
    f.dispatch(commands::Rename);
    assert!(f.prompt_open());
    let prompt = f.prompt();
    let (title, selection, text) = f.vcx.update(|_, cx| {
        let p = prompt.read(cx);
        (
            p.title().map(str::to_owned),
            p.selection(cx),
            p.text().to_owned(),
        )
    });
    assert_eq!(title.as_deref(), Some("Rename “Old.md”"));
    assert_eq!(text, "Old.md");
    assert_eq!(selection, 0..3, "the name without its extension");

    f.type_name("New.md");
    f.enter();
    // Links change: the preview.
    assert!(f.prompt_open(), "a preview shows before links change");
    let rows = prompt.read_with(&f.vcx, |p, _| p.rows());
    assert_eq!(rows[0], ("a.md (2)".to_owned(), RowStyle::Heading));
    assert_eq!(
        rows[1..],
        [
            ("1: [[Old]]".to_owned(), RowStyle::Removed),
            ("1: [[New]]".to_owned(), RowStyle::Added),
            ("1: [x](Old.md#old)".to_owned(), RowStyle::Removed),
            ("1: [x](New.md#old)".to_owned(), RowStyle::Added),
        ]
    );
    f.enter();
    assert!(!f.prompt_open());
    assert!(f.root().join("New.md").exists());
    assert!(!f.root().join("Old.md").exists());
    assert_eq!(f.read("a.md"), "See [[New]] and [x](New.md#old).\n");
    assert_eq!(
        f.active_path(),
        Some(f.root().join("New.md")),
        "the tab follows"
    );
    assert_eq!(
        f.notice(),
        Some((
            Topic::FileOp,
            "Renamed “Old.md” to “New.md”.".to_owned(),
            true
        ))
    );
    assert!(f.rows().contains(&"New.md".to_owned()));

    f.press(&NoticeAction::UndoFileOp);
    assert!(f.root().join("Old.md").exists());
    assert!(!f.root().join("New.md").exists());
    assert_eq!(f.read("a.md"), "See [[Old]] and [x](Old.md#old).\n");
    assert_eq!(f.active_path(), Some(f.root().join("Old.md")));
    assert_eq!(f.notice().map(|n| n.1), Some("Undone.".to_owned()));
    assert!(!f.ws.read_with(&f.vcx, |ws, _| ws.can_undo_file_op()));
}

#[gpui::test]
fn a_rename_without_links_runs_at_once_and_case_only_works(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx, &[("note.md", "x\n"), ("other.md", "y\n")], "other.md");
    f.select_row("note.md");
    f.dispatch(commands::Rename);
    f.type_name("Note.md");
    let problem = f
        .prompt()
        .read_with(&f.vcx, |p, _| p.problem().map(str::to_owned));
    assert_eq!(problem, None, "a case-only rename is allowed");
    f.enter();
    assert!(!f.prompt_open(), "no preview without link changes");
    let names: Vec<String> = std::fs::read_dir(f.root())
        .expect("list")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"Note.md".to_owned()), "{names:?}");
    assert!(!names.contains(&"note.md".to_owned()), "{names:?}");
    assert_eq!(f.selected().as_deref(), Some("Note.md"));
}

#[gpui::test]
fn names_that_cannot_be_used_are_refused_as_typed(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx, &[("a.md", ""), ("B.md", "")], "a.md");
    f.dispatch(commands::Rename);
    let prompt = f.prompt();
    let problem = |f: &mut Fixture| prompt.read_with(&f.vcx, |p, _| p.problem().map(str::to_owned));
    f.type_name("b.md");
    assert_eq!(
        problem(&mut f).as_deref(),
        Some("“b.md” already exists in this folder")
    );
    f.type_name("a|b.md");
    assert!(problem(&mut f).is_some_and(|p| p.contains("breaks links")));
    f.type_name("con.md");
    assert!(problem(&mut f).is_some_and(|p| p.contains("reserved")));
    f.type_name("");
    assert!(problem(&mut f).is_some());
    // Enter does nothing while the name cannot be used.
    f.enter();
    assert!(f.prompt_open());
    f.vcx.simulate_keystrokes("escape");
    f.draw();
    assert!(!f.prompt_open());
    assert!(f.root().join("a.md").exists());
}

#[gpui::test]
fn move_to_lists_the_folders_and_updates_links(cx: &mut TestAppContext) {
    let mut f = Fixture::new(
        cx,
        &[
            ("n.md", "[a](a.md)\n"),
            ("a.md", "[n](./n.md)\n"),
            ("Archive/old.md", ""),
        ],
        "a.md",
    );
    f.select_row("n.md");
    f.dispatch(commands::Move);
    let listed = f.goto().read_with(&f.vcx, |g, _| g.listed());
    assert_eq!(listed, ["Archive"], "the folders, without the current one");
    f.pick("Archive");
    // Preview of the link changes, then apply.
    assert!(f.prompt_open());
    f.enter();
    assert!(f.root().join("Archive/n.md").exists());
    assert_eq!(f.read("a.md"), "[n](./Archive/n.md)\n");
    // In an Obsidian vault a Markdown link by name keeps resolving.
    assert_eq!(f.read("Archive/n.md"), "[a](a.md)\n");
    assert_eq!(f.selected().as_deref(), Some("n.md"));
}

#[gpui::test]
fn duplicate_copies_next_to_the_original(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx, &[("Note.md", "text\n")], "Note.md");
    f.select_row("Note.md");
    f.dispatch(commands::Duplicate);
    assert_eq!(f.read("Note 1.md"), "text\n");
    assert_eq!(f.selected().as_deref(), Some("Note 1.md"));
}

#[gpui::test]
fn move_to_trash_asks_with_the_links_that_break(cx: &mut TestAppContext) {
    let mut f = Fixture::new(
        cx,
        &[
            ("Gone.md", "bye\n"),
            ("a.md", "[[Gone]]\n"),
            ("b.md", "x\n"),
        ],
        "Gone.md",
    );
    f.dispatch(commands::Delete);
    let prompt = f.prompt();
    let (title, message, rows) = prompt.read_with(&f.vcx, |p, _| {
        (
            p.title().map(str::to_owned),
            p.message().map(str::to_owned),
            p.rows(),
        )
    });
    assert_eq!(title.as_deref(), Some("Move “Gone.md” to the trash?"));
    assert_eq!(message.as_deref(), Some("1 link will break."));
    assert_eq!(rows, [("a.md:1 → Gone.md".to_owned(), RowStyle::Note)]);
    f.enter();
    assert!(!f.root().join("Gone.md").exists());
    assert_eq!(f.trash.0.lock().expect("lock").len(), 1);
    assert_ne!(
        f.active_path(),
        Some(f.root().join("Gone.md")),
        "its tab closed"
    );
    assert!(!f.rows().contains(&"Gone.md".to_owned()));
}

#[gpui::test]
fn new_folder_is_created_in_the_selected_folder_and_named(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx, &[("Notes/a.md", ""), ("b.md", "")], "b.md");
    f.select_row("Notes/a.md");
    f.dispatch(commands::NewFolder);
    assert!(f.root().join("Notes/Untitled").is_dir());
    assert!(f.prompt_open(), "it asks for the name");
    f.type_name("Projects");
    f.enter();
    assert!(f.root().join("Notes/Projects").is_dir());
    assert!(!f.root().join("Notes/Untitled").exists());
    assert_eq!(f.selected().as_deref(), Some("Projects"));
}

#[gpui::test]
fn sort_and_filter_change_the_rows(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx, &[("a.md", ""), ("b.md", ""), ("Dir/c.md", "")], "a.md");
    assert_eq!(f.rows(), ["Dir", "a.md", "b.md"]);
    f.ws.update_in(&mut f.vcx, |ws, window, cx| ws.show_sort_orders(window, cx));
    f.draw();
    f.pick("name (Z to A)");
    assert_eq!(f.rows(), ["Dir", "b.md", "a.md"]);

    let files = f.files();
    f.vcx
        .update(|window, cx| files.update(cx, |p, cx| p.set_filter_text("C", window, cx)));
    f.draw();
    assert_eq!(f.rows(), ["Dir", "  c.md"]);
}

#[gpui::test]
fn the_vault_switcher_lists_this_folder_and_open_folder(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx, &[("a.md", "")], "a.md");
    f.ws.update_in(&mut f.vcx, |ws, window, cx| {
        ws.show_vault_switcher(window, cx)
    });
    f.draw();
    let goto = f.goto();
    let (title, listed) = goto.read_with(&f.vcx, |g, _| (g.title().map(str::to_owned), g.listed()));
    assert_eq!(title.as_deref(), Some("Switch vault"));
    let name = f
        .root()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .expect("name");
    assert_eq!(listed.first(), Some(&name));
    assert_eq!(
        listed.last().map(String::as_str),
        Some("Open folder\u{2026}")
    );
}

#[gpui::test]
fn files_with_unsaved_changes_are_not_renamed(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx, &[("a.md", "text\n")], "a.md");
    f.dispatch(ToggleEditMode);
    let editor =
        f.ws.read_with(&f.vcx, |ws, _| ws.active_tab().editor().cloned())
            .expect("editor");
    f.vcx.update(|window, cx| {
        let focus = editor.read(cx).focus_handle(cx);
        focus.focus(window, cx);
    });
    f.vcx.simulate_input("more ");
    f.draw();
    f.dispatch(commands::Rename);
    f.type_name("b.md");
    f.enter();
    assert!(f.root().join("a.md").exists());
    assert!(!f.root().join("b.md").exists());
    let notice = f.notice().expect("notice");
    assert_eq!(notice.0, Topic::FileOp);
    assert_eq!(notice.1, "“a.md” was not renamed.");
    let _ = f.window;
}
