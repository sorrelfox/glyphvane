// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! New block ids in other notes from autocomplete, in headless windows:
//! the file of a note that is not open, the buffer of one that is (saved
//! when it was, kept unsaved when it was not), a note changed since its
//! blocks were listed, and one with unsaved changes in another window.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Entity, Focusable as _, TestAppContext, VisualTestContext, WindowOptions,
};

use super::{OpenTarget, Workspace};
use crate::chrome::WindowChrome;
use crate::editor::EditorView;
use crate::{AppSettings, ThemeChoice, ToggleEditMode, ToggleSidebar};

const PLAN: &str = "# Plan\n\nThe paragraph.\n\n- An item\n";

struct Fixture {
    dir: tempfile::TempDir,
    _journal: tempfile::TempDir,
    ws: Entity<Workspace>,
    vcx: VisualTestContext,
}

impl Fixture {
    /// A vault with `Home.md` (empty) and `Plan.md`, `Home.md` in the
    /// Source view with the keyboard, the content index built.
    fn new(cx: &mut TestAppContext) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join(".obsidian")).expect("marker");
        std::fs::write(dir.path().join("Home.md"), "").expect("write");
        std::fs::write(dir.path().join("Plan.md"), PLAN).expect("write");
        let journal = tempfile::tempdir().expect("journal dir");
        let target = Some(OpenTarget::new(dir.path().join("Home.md")));
        let (window, ws) = cx.update(|cx| {
            crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
            let store = gv_vault::files::journal::JournalStore::open(journal.path().join("j"))
                .expect("journal store");
            crate::files::install_store(cx, Some(store), false);
            crate::files::override_autosave(cx, Some(false));
            gpui::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
            })
            .expect("test window opens")
        });
        // The sidebar shown, as in the Files panel tests: the vault opens.
        cx.dispatch_action(window, ToggleSidebar);
        let vcx = VisualTestContext::from_window(window, cx);
        let mut f = Self {
            dir,
            _journal: journal,
            ws,
            vcx,
        };
        f.until("the content index", |f| {
            let vault = f.ws.read_with(&f.vcx, |ws, _| ws.vault.clone());
            vault.is_some_and(|v| v.read_with(&f.vcx, |v, _| v.content().is_some()))
        });
        f.vcx.dispatch_action(ToggleEditMode);
        f.until("the Source view", |f| f.text("Home.md").is_some());
        f.focus("Home.md");
        f
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn draw(&mut self) {
        for _ in 0..2 {
            self.vcx.run_until_parked();
            self.vcx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    /// Waits (in real time: the index and the files have their own
    /// threads) until `f` holds.
    fn until(&mut self, what: &str, f: impl Fn(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let vault = self.ws.read_with(&self.vcx, |ws, _| ws.vault.clone());
            if let Some(vault) = vault {
                vault.update(&mut self.vcx, |v, cx| v.poll_content(cx));
            }
            self.draw();
            if f(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{what} did not happen in time");
    }

    fn editor(&self, name: &str) -> Option<Entity<EditorView>> {
        let path = self.path(name);
        self.ws.read_with(&self.vcx, |ws, _| {
            ws.tabs()
                .iter()
                .find(|t| t.path() == Some(path.as_path()))
                .and_then(|t| t.editor().cloned())
        })
    }

    fn text(&self, name: &str) -> Option<String> {
        let editor = self.editor(name)?;
        editor.read_with(&self.vcx, |e, _| e.editor().map(|e| e.text().to_string()))
    }

    fn focus(&mut self, name: &str) {
        let editor = self.editor(name).expect("editor");
        self.vcx.update(|window, cx| {
            let focus = editor.read(cx).focus_handle(cx);
            focus.focus(window, cx);
        });
        self.draw();
    }

    /// Opens `name` in a background tab with a loaded Source view.
    fn open_in_background(&mut self, name: &str) -> Entity<EditorView> {
        let path = self.path(name);
        self.ws.update_in(&mut self.vcx, |ws, window, cx| {
            ws.insert_tab(OpenTarget::new(path.clone()), None, false, window, cx);
            let index = ws
                .tabs
                .iter()
                .position(|t| t.path.as_deref() == Some(path.as_path()))
                .expect("the tab");
            ws.ensure_editor(index, cx);
        });
        self.until("the note loaded", |f| f.text(name).is_some());
        self.editor(name).expect("editor")
    }

    /// Types `text` into the focused Source view, a character at a time.
    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.vcx.simulate_input(&c.to_string());
        }
        self.draw();
    }

    fn rows(&self, name: &str) -> Vec<String> {
        let editor = self.editor(name).expect("editor");
        editor.read_with(&self.vcx, |e, _| {
            e.autocomplete_rows().map(|(r, _)| r).unwrap_or_default()
        })
    }

    /// Types `[[Plan#^` and `query` in `Home.md` and waits for `row`.
    fn list(&mut self, query: &str, row: &str) {
        self.type_text(&format!("[[Plan#^{query}"));
        self.until("Plan's blocks", |f| {
            f.rows("Home.md").first().map(String::as_str) == Some(row)
        });
    }

    fn accept(&mut self) {
        self.vcx.simulate_keystrokes("enter");
        self.draw();
    }

    fn disk(&self, name: &str) -> String {
        std::fs::read_to_string(self.path(name)).expect("read")
    }

    /// The id the link in `Home.md` names.
    fn linked_id(&self) -> String {
        let home = self.text("Home.md").expect("Home");
        let id = home
            .strip_prefix("[[Plan#^")
            .and_then(|r| r.strip_suffix("]]"))
            .unwrap_or_else(|| panic!("a link to a block: {home}"));
        assert_eq!(id.len(), 6, "{home}");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase()),
            "{home}"
        );
        id.to_owned()
    }

    fn notice(&self, name: &str) -> Option<(String, String)> {
        let editor = self.editor(name)?;
        editor.read_with(&self.vcx, |e, _| {
            e.notice().map(|n| {
                (
                    n.title.to_string(),
                    n.detail
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                )
            })
        })
    }
}

fn plan_with(id: &str) -> String {
    PLAN.replace("The paragraph.", &format!("The paragraph. ^{id}"))
}

#[gpui::test]
fn a_note_that_is_not_open_gets_the_id_in_its_file(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx);
    f.list("the par", "The paragraph.");
    f.accept();
    let id = f.linked_id();
    f.until("the id in the file", |f| {
        f.disk("Plan.md") == plan_with(&id)
    });
    assert!(f.notice("Home.md").is_none());
    // Undo takes the link back; the id stays, as in Obsidian.
    let home = f.editor("Home.md").expect("Home");
    home.update(&mut f.vcx, |e, cx| e.undo_for_tests(cx));
    f.draw();
    assert!(!f.text("Home.md").expect("Home").contains(&id));
    assert_eq!(f.disk("Plan.md"), plan_with(&id));
}

#[gpui::test]
fn the_short_form_and_headings_link_by_text(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx);
    f.type_text("[[Plan^pla");
    f.until("Plan's blocks", |f| {
        f.rows("Home.md").first().map(String::as_str) == Some("Plan")
    });
    f.accept();
    // A heading has no id: it is linked by its text, and Plan is unchanged.
    assert_eq!(f.text("Home.md").as_deref(), Some("[[Plan#Plan]]"));
    assert_eq!(f.disk("Plan.md"), PLAN);
}

#[gpui::test]
fn an_open_note_gets_the_id_in_its_buffer_and_is_saved(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx);
    let plan = f.open_in_background("Plan.md");
    f.focus("Home.md");
    f.list("an it", "An item");
    f.accept();
    let id = f.linked_id();
    let expected = PLAN.replace("- An item", &format!("- An item ^{id}"));
    assert_eq!(f.text("Plan.md").as_deref(), Some(expected.as_str()));
    // It had no unsaved changes: it is saved.
    f.until("Plan saved", |f| f.disk("Plan.md") == expected);
    assert!(!plan.read_with(&f.vcx, |e, _| e.is_modified()));
}

#[gpui::test]
fn an_open_note_with_unsaved_changes_keeps_them_unsaved(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx);
    let plan = f.open_in_background("Plan.md");
    plan.update(&mut f.vcx, |e, cx| {
        let end = e.editor().map(|e| e.text().len()).unwrap_or(0);
        e.apply_checked_replace(end..end, "", |_| "Unsaved line.\n".to_owned(), cx);
    });
    f.focus("Home.md");
    f.list("the par", "The paragraph.");
    f.accept();
    let id = f.linked_id();
    let expected = format!("{}Unsaved line.\n", plan_with(&id));
    assert_eq!(f.text("Plan.md").as_deref(), Some(expected.as_str()));
    // Still unsaved: the file is as it was.
    f.draw();
    assert!(plan.read_with(&f.vcx, |e, _| e.is_modified()));
    assert_eq!(f.disk("Plan.md"), PLAN);
}

#[gpui::test]
fn a_note_changed_since_it_was_listed_is_left_alone(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx);
    f.list("the par", "The paragraph.");
    let changed = PLAN.replace("The paragraph.", "The paragraph, edited elsewhere.");
    std::fs::write(f.path("Plan.md"), &changed).expect("write");
    f.accept();
    f.until("the refusal", |f| f.notice("Home.md").is_some());
    let (title, detail) = f.notice("Home.md").expect("notice");
    assert_eq!(title, "Block ID not added.");
    assert!(
        detail.contains("Plan.md") && detail.contains("changed"),
        "{detail}"
    );
    // The link went back out; the note is as it was.
    let home = f.text("Home.md").expect("Home");
    assert!(home.starts_with("[[Plan#^the par"), "{home}");
    assert_eq!(f.disk("Plan.md"), changed);
}

#[gpui::test]
fn a_note_with_unsaved_changes_in_another_window_is_refused(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx);
    let plan = f.path("Plan.md");
    let unsaved = format!("{PLAN}More, unsaved.\n");
    crate::files::overlay::dirty(&plan, &gv_text::TextSnapshot::new(&unsaved));
    f.list("the par", "The paragraph.");
    f.accept();
    f.until("the refusal", |f| f.notice("Home.md").is_some());
    let (_, detail) = f.notice("Home.md").expect("notice");
    assert!(
        detail.contains("unsaved changes in another window"),
        "{detail}"
    );
    let home = f.text("Home.md").expect("Home");
    assert!(home.starts_with("[[Plan#^the par"), "{home}");
    assert_eq!(f.disk("Plan.md"), PLAN);
    crate::files::overlay::clean(&plan);
}

#[gpui::test]
fn blocks_of_every_note_get_an_id_too(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx);
    f.type_text("[[^^an ite");
    f.until("every note's blocks", |f| {
        f.rows("Home.md").first().map(String::as_str) == Some("An item")
    });
    f.accept();
    let id = f.linked_id();
    let expected = PLAN.replace("- An item", &format!("- An item ^{id}"));
    f.until("the id in the file", |f| f.disk("Plan.md") == expected);
}
