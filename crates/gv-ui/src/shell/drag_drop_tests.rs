// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Drag and drop, attachments and pasted images, in headless GPUI windows:
//! a Files panel entry dragged onto a folder moves (preview, links,
//! Undo); one dragged into the Source view becomes a link (Alt: an embed);
//! files from the OS are linked, copied into the attachment folder or
//! linked where they are; an Outline heading becomes a link to it; a
//! pasted image is saved and embedded. Every insertion is one undo step.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, AppContext as _, ClipboardItem, Entity, ExternalPaths, FileDropEvent,
    Focusable as _, Image, ImageFormat, Modifiers, MouseButton, Pixels, Point, TestAppContext,
    VisualTestContext, WindowOptions, point, px,
};
use gv_settings::{CURRENT_VERSION, LayerKind, SettingsFile};

use super::drag_drop::DraggedHeading;
use super::{OpenTarget, Workspace};
use crate::chrome::WindowChrome;
use crate::editor::EditorView;
use crate::file_prompt::prompt_of;
use crate::files::notice::NoticeAction;
use crate::goto::goto_of;
use crate::keymap::actions::editor as editor_commands;
use crate::{AppSettings, ThemeChoice, ToggleEditMode, ToggleSidebar};

struct Fixture {
    dir: tempfile::TempDir,
    ws: Entity<Workspace>,
    vcx: VisualTestContext,
    _window: AnyWindowHandle,
}

impl Fixture {
    /// A vault of `files` (with `.obsidian/app.json` holding `app`), a
    /// window for `open` with the sidebar shown, the indexes built.
    /// `settings` is the global settings file's text after its version.
    fn new(
        cx: &mut TestAppContext,
        files: &[(&str, &str)],
        app: &str,
        settings: &str,
        open: &str,
    ) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("vault/.obsidian")).expect("marker");
        std::fs::write(dir.path().join("vault/.obsidian/app.json"), app).expect("app.json");
        for (path, text) in files {
            let full = dir.path().join("vault").join(path);
            std::fs::create_dir_all(full.parent().expect("parent")).expect("folders");
            std::fs::write(full, text).expect("write");
        }
        let settings_path = dir.path().join("settings.toml");
        std::fs::write(
            &settings_path,
            format!("version = {CURRENT_VERSION}\n{settings}"),
        )
        .expect("settings");
        let file = SettingsFile::load(&settings_path, LayerKind::Global).expect("settings load");
        let target = Some(OpenTarget::new(dir.path().join("vault").join(open)));
        let (window, ws) = cx.update(|cx| {
            crate::init(cx, AppSettings::new(Some(file), Some(ThemeChoice::Light)));
            gpui::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
            })
            .expect("test window opens")
        });
        cx.dispatch_action(window, ToggleSidebar);
        let vcx = VisualTestContext::from_window(window, cx);
        let mut f = Self {
            dir,
            ws,
            vcx,
            _window: window,
        };
        f.draw();
        f.wait_for_content();
        f
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("vault")
    }

    fn draw(&mut self) {
        for _ in 0..3 {
            self.vcx.run_until_parked();
            self.vcx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    /// Waits (in real time: the content index has its own thread) until
    /// the content index has a snapshot.
    fn wait_for_content(&mut self) {
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

    /// Switches the active tab to Edit mode and gives its Source view the
    /// keyboard.
    fn edit(&mut self) -> Entity<EditorView> {
        self.vcx.dispatch_action(ToggleEditMode);
        self.draw();
        let editor = self
            .ws
            .read_with(&self.vcx, |ws, _| ws.active_tab().editor().cloned())
            .expect("a Source view");
        self.vcx.update(|window, cx| {
            let focus = editor.read(cx).focus_handle(cx);
            focus.focus(window, cx);
        });
        self.draw();
        editor
    }

    fn text(&mut self, editor: &Entity<EditorView>) -> String {
        editor.read_with(&self.vcx, |e, _| {
            let text = e.editor().expect("editor").text();
            text.text_for_range(0..text.len()).expect("text")
        })
    }

    /// A point inside the Source view's text, near its top left corner.
    fn editor_point(&mut self, editor: &Entity<EditorView>) -> Point<Pixels> {
        let bounds = editor
            .read_with(&self.vcx, |e, _| e.drawn_bounds())
            .expect("drawn");
        point(bounds.origin.x + px(1.0), bounds.origin.y + px(1.0))
    }

    /// The centre of the Files panel row named `name`.
    fn row_point(&mut self, name: &str) -> Point<Pixels> {
        let files = self
            .ws
            .read_with(&self.vcx, |ws, _| ws.files.clone())
            .expect("files panel");
        let rows = files.read_with(&self.vcx, |p, _| p.visible_rows());
        let ix = rows
            .iter()
            .position(|(_, n)| n == name)
            .unwrap_or_else(|| panic!("{name} in {rows:?}"));
        let selector: &'static str = Box::leak(format!("file-row-{ix}").into_boxed_str());
        self.vcx.debug_bounds(selector).expect("row drawn").center()
    }

    /// Drags from `from` to `to` with the mouse, dropping with
    /// `modifiers`.
    fn drag(&mut self, from: Point<Pixels>, to: Point<Pixels>, modifiers: Modifiers) {
        self.vcx
            .simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
        let step = point(from.x + px(8.0), from.y + px(8.0));
        self.vcx
            .simulate_mouse_move(step, MouseButton::Left, Modifiers::none());
        self.draw();
        self.vcx
            .simulate_mouse_move(to, MouseButton::Left, modifiers);
        self.draw();
        self.vcx.simulate_mouse_up(to, MouseButton::Left, modifiers);
        self.draw();
    }

    /// Drops `paths` from the OS at `at`.
    fn drop_files(&mut self, paths: &[PathBuf], at: Point<Pixels>) {
        self.vcx.simulate_event(FileDropEvent::Entered {
            position: at,
            paths: ExternalPaths(paths.iter().cloned().collect()),
        });
        self.draw();
        self.vcx
            .simulate_event(FileDropEvent::Submit { position: at });
        self.draw();
    }

    fn undo(&mut self) {
        self.vcx.dispatch_action(editor_commands::Undo);
        self.draw();
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.root().join(path)).expect("read")
    }

    fn enter(&mut self) {
        self.vcx.simulate_keystrokes("enter");
        self.draw();
    }

    fn prompt_open(&mut self) -> bool {
        let prompt = self
            .vcx
            .update(|window, cx| prompt_of(window, cx))
            .expect("prompt");
        prompt.read_with(&self.vcx, |p, _| p.is_open())
    }

    fn active_name(&self) -> String {
        self.ws.read_with(&self.vcx, |ws, _| {
            ws.active_tab()
                .path()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
    }

    /// Waits (in real time: copies run off the window's thread) until
    /// `f` holds.
    fn until(&mut self, what: &str, f: impl Fn(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            self.draw();
            if f(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{what} did not happen in time");
    }
}

const WIKI: &str = "{}";
const MARKDOWN: &str = r#"{"useMarkdownLinks": true}"#;

#[gpui::test]
fn dragging_a_file_onto_a_folder_moves_it_with_its_links(cx: &mut TestAppContext) {
    let mut f = Fixture::new(
        cx,
        &[
            ("n.md", "# N\n"),
            ("a.md", "[n](./n.md)\n"),
            ("Archive/old.md", ""),
        ],
        WIKI,
        "",
        "a.md",
    );
    // Onto its own folder: nothing happens.
    let n = f.row_point("n.md");
    let a = f.row_point("a.md");
    f.drag(n, a, Modifiers::none());
    assert!(!f.prompt_open());
    assert!(f.root().join("n.md").exists());

    let archive = f.row_point("Archive");
    f.drag(n, archive, Modifiers::none());
    // The links change: the preview, as for Move to….
    assert!(f.prompt_open(), "a preview before the links change");
    f.enter();
    assert!(f.root().join("Archive/n.md").exists());
    assert!(!f.root().join("n.md").exists());
    assert_eq!(f.read("a.md"), "[n](./Archive/n.md)\n");
    // Undo puts the file and the link back.
    f.ws.update(&mut f.vcx, |ws, cx| {
        assert!(ws.answer(&NoticeAction::UndoFileOp, cx));
    });
    f.draw();
    assert!(f.root().join("n.md").exists());
    assert_eq!(f.read("a.md"), "[n](./n.md)\n");
}

#[gpui::test]
fn a_file_dragged_into_the_source_view_becomes_a_link(cx: &mut TestAppContext) {
    let mut f = Fixture::new(
        cx,
        &[
            ("a.md", "Hello\n"),
            ("Note B.md", "# B\n"),
            ("pic.png", "png"),
        ],
        WIKI,
        "",
        "a.md",
    );
    let editor = f.edit();
    let note = f.row_point("Note B.md");
    let at = f.editor_point(&editor);
    f.drag(note, at, Modifiers::none());
    assert_eq!(f.text(&editor), "[[Note B]]Hello\n");
    f.undo();
    assert_eq!(f.text(&editor), "Hello\n", "one undo step");

    // With Alt, an embed; an image is always embedded.
    f.drag(note, at, Modifiers::alt());
    assert_eq!(f.text(&editor), "![[Note B]]Hello\n");
    f.undo();
    let pic = f.row_point("pic.png");
    f.drag(pic, at, Modifiers::none());
    assert_eq!(f.text(&editor), "![[pic.png]]Hello\n");

    // In the Reading view a dropped note opens.
    f.undo();
    f.vcx.dispatch_action(ToggleEditMode);
    f.draw();
    let reading = f
        .vcx
        .debug_bounds("files-panel")
        .map(|b| point(b.right() + px(200.0), b.top() + px(200.0)))
        .expect("panel");
    f.drag(note, reading, Modifiers::none());
    assert_eq!(f.active_name(), "Note B.md");
}

#[gpui::test]
fn links_follow_the_vaults_markdown_style(cx: &mut TestAppContext) {
    let mut f = Fixture::new(
        cx,
        &[("a.md", "Hello\n"), ("Note B.md", "# B\n")],
        MARKDOWN,
        "",
        "a.md",
    );
    let editor = f.edit();
    let note = f.row_point("Note B.md");
    let at = f.editor_point(&editor);
    f.drag(note, at, Modifiers::none());
    let text = f.text(&editor);
    assert!(
        text.starts_with("[Note B](") && text.contains("Note%20B.md)Hello"),
        "{text}"
    );
}

#[gpui::test]
fn an_outline_heading_dropped_in_the_source_view_links_to_it(cx: &mut TestAppContext) {
    let mut f = Fixture::new(
        cx,
        &[("a.md", "Hello\n"), ("b.md", "# Big [x] idea\n")],
        WIKI,
        "",
        "a.md",
    );
    let editor = f.edit();
    let at = f.editor_point(&editor);
    f.vcx.simulate_mouse_move(at, None, Modifiers::none());
    let root = f.root();
    for (file, expected) in [
        ("b.md", "[[b#Big x idea]]Hello\n"),
        ("a.md", "[[#Hello]]Hello\n"),
    ] {
        let heading = DraggedHeading {
            file: root.join(file),
            text: if file == "a.md" {
                "Hello".into()
            } else {
                "Big [x] idea".into()
            },
        };
        f.ws.update_in(&mut f.vcx, |ws, window, cx| {
            ws.drop_heading(&heading, window, cx);
        });
        f.draw();
        assert_eq!(f.text(&editor), expected);
        f.undo();
    }
}

#[gpui::test]
fn files_from_outside_are_copied_into_the_attachment_folder(cx: &mut TestAppContext) {
    let mut f = Fixture::new(
        cx,
        &[("notes/a.md", "Hello\n"), ("inside.pdf", "pdf")],
        r#"{"attachmentFolderPath": "./att"}"#,
        "[files]\nexternal_files = \"copy\"\n",
        "notes/a.md",
    );
    let outside = f.dir.path().join("outside");
    std::fs::create_dir_all(&outside).expect("mkdir");
    std::fs::write(outside.join("pic one.png"), "png").expect("write");
    std::fs::write(outside.join("doc.zip"), "zip").expect("write");
    // A file of that name is there already.
    std::fs::create_dir_all(f.root().join("notes/att")).expect("mkdir");
    std::fs::write(f.root().join("notes/att/doc.zip"), "old").expect("write");
    let editor = f.edit();
    let at = f.editor_point(&editor);
    f.drop_files(
        &[
            outside.join("pic one.png"),
            outside.join("doc.zip"),
            f.root().join("inside.pdf"),
        ],
        at,
    );
    f.until("the copies", |f| f.text(&editor).contains("doc 1.zip"));
    assert_eq!(
        f.text(&editor),
        "![[inside.pdf]]![[pic one.png]]\n[[doc 1.zip]]Hello\n"
    );
    assert_eq!(f.read("notes/att/pic one.png"), "png");
    assert_eq!(f.read("notes/att/doc 1.zip"), "zip");
    assert_eq!(f.read("notes/att/doc.zip"), "old");
    // The vault's file was linked where it is, in its own undo step.
    f.undo();
    assert_eq!(f.text(&editor), "![[inside.pdf]]Hello\n");
    f.undo();
    assert_eq!(f.text(&editor), "Hello\n");
}

#[gpui::test]
fn files_from_outside_ask_and_can_be_linked_where_they_are(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx, &[("a.md", "Hello\n")], WIKI, "", "a.md");
    let outside = f.dir.path().join("out dir");
    std::fs::create_dir_all(&outside).expect("mkdir");
    std::fs::write(outside.join("x.txt"), "x").expect("write");
    let editor = f.edit();
    let at = f.editor_point(&editor);
    f.drop_files(&[outside.join("x.txt")], at);
    // The default setting asks.
    let goto = f
        .vcx
        .update(|window, cx| goto_of(window, cx))
        .expect("goto popover");
    let listed = goto.read_with(&f.vcx, |g, _| g.listed());
    assert_eq!(
        listed,
        [
            "Copy into the attachment folder",
            "Link to them where they are"
        ]
    );
    f.vcx.simulate_keystrokes("down");
    f.enter();
    let url = crate::attachments::file_url(&outside.join("x.txt"));
    assert_eq!(f.text(&editor), format!("[x.txt]({url})Hello\n"));
    assert!(url.contains("out%20dir"));
    assert!(!f.root().join("x.txt").exists(), "nothing was copied");
}

#[gpui::test]
fn files_dropped_outside_the_source_view_open(cx: &mut TestAppContext) {
    let mut f = Fixture::new(
        cx,
        &[("a.md", "Hello\n"), ("b.md", "# B\n")],
        WIKI,
        "",
        "a.md",
    );
    let reading = f
        .vcx
        .debug_bounds("files-panel")
        .map(|b| point(b.right() + px(200.0), b.top() + px(200.0)))
        .expect("panel");
    let b = f.root().join("b.md");
    f.drop_files(&[b], reading);
    assert_eq!(f.active_name(), "b.md");
    // The title row (the tabs) opens them too.
    let title = f.vcx.debug_bounds("title-row").expect("title row").center();
    f.drop_files(&[f.root().join("a.md")], title);
    assert_eq!(f.active_name(), "a.md");
}

#[gpui::test]
fn a_pasted_image_is_saved_and_embedded(cx: &mut TestAppContext) {
    let mut f = Fixture::new(
        cx,
        &[("notes/a.md", "Hello\n")],
        r#"{"attachmentFolderPath": "Assets"}"#,
        "",
        "notes/a.md",
    );
    let editor = f.edit();
    let image = Image::from_bytes(ImageFormat::Png, b"\x89PNG data".to_vec());
    f.vcx.write_to_clipboard(ClipboardItem::new_image(&image));
    f.vcx.dispatch_action(editor_commands::Paste);
    f.until("the image", |f| f.text(&editor).contains("Pasted image"));
    let text = f.text(&editor);
    let name = text
        .strip_prefix("![[")
        .and_then(|t| t.split_once("]]"))
        .map(|(n, _)| n.to_owned())
        .unwrap_or_else(|| panic!("an embed first: {text}"));
    assert!(
        name.starts_with("Pasted image 2") && name.ends_with(".png"),
        "{name}"
    );
    assert_eq!(name.len(), "Pasted image 20261117090503.png".len());
    assert_eq!(text, format!("![[{name}]]Hello\n"));
    let saved = std::fs::read(f.root().join("Assets").join(&name)).expect("saved");
    assert_eq!(saved, b"\x89PNG data");
    f.undo();
    assert_eq!(f.text(&editor), "Hello\n");
}

/// Files from the OS dropped on the Files panel are copied into the folder
/// under the pointer (a file row's folder, the vault folder below the
/// rows), keeping their names with a number when taken; nothing opens.
#[gpui::test]
fn files_from_the_os_dropped_on_the_files_panel_are_copied_in(cx: &mut TestAppContext) {
    let mut f = Fixture::new(
        cx,
        &[("a.md", "Hello\n"), ("Archive/old.md", "")],
        WIKI,
        "",
        "a.md",
    );
    let outside = f.dir.path().join("out");
    std::fs::create_dir_all(&outside).expect("folder");
    let x = outside.join("x.txt");
    std::fs::write(&x, "x").expect("write");
    let old = outside.join("old.md");
    std::fs::write(&old, "other").expect("write");

    // Onto a folder row: into that folder; a taken name gets a number.
    let archive = f.row_point("Archive");
    f.drop_files(&[x.clone(), old.clone()], archive);
    f.until("the copies", |f| f.root().join("Archive/old 1.md").exists());
    assert_eq!(f.read("Archive/x.txt"), "x");
    assert_eq!(f.read("Archive/old.md"), "", "the vault's file is kept");
    assert_eq!(f.read("Archive/old 1.md"), "other");
    assert!(x.exists(), "copied, not moved");
    assert_eq!(f.active_name(), "a.md", "nothing opens");

    // Onto a file row at the top level: into the vault folder.
    let a = f.row_point("a.md");
    f.drop_files(std::slice::from_ref(&x), a);
    f.until("the copy", |f| f.root().join("x.txt").exists());
    // The copy is in the index and selected.
    f.until("the row", |f| {
        let files =
            f.ws.read_with(&f.vcx, |ws, _| ws.files.clone())
                .expect("panel");
        files.read_with(&f.vcx, |p, _| {
            p.visible_rows().iter().any(|(_, n)| n == "x.txt")
        })
    });

    // A folder is not copied: a notice says so.
    f.drop_files(std::slice::from_ref(&outside), archive);
    f.draw();
    assert!(!f.root().join("Archive/out").exists());
}

#[test]
fn copies_keep_their_names_and_folders_are_refused() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("vault");
    std::fs::create_dir_all(root.join("Sub")).expect("folders");
    std::fs::write(root.join("Sub/a.txt"), "old").expect("write");
    let src = dir.path().join("a.txt");
    std::fs::write(&src, "new").expect("write");
    let folder = gv_vault::VaultPath::new("Sub").expect("path");
    let copied =
        super::drag_drop::copy_files_into(&root, &folder, &[src, dir.path().join("vault")]);
    assert_eq!(
        copied
            .copies
            .iter()
            .map(gv_vault::VaultPath::as_str)
            .collect::<Vec<_>>(),
        ["Sub/a 1.txt"]
    );
    assert_eq!(
        std::fs::read_to_string(root.join("Sub/a 1.txt")).expect("read"),
        "new"
    );
    assert_eq!(copied.failed.len(), 1);
    assert_eq!(copied.failed[0].0, "vault");
    assert_eq!(copied.changes.len(), 1);
}
