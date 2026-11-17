// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Hover previews of links, in headless windows over a temporary Obsidian
//! vault: the delay and dismissal, contents by target (note, unresolved,
//! ambiguous, web address), Esc and Enter, *Preview link under caret*,
//! Mod+hover in the Source view, previews inside previews and links
//! followed from a preview.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Bounds, Entity, EntityInputHandler as _, Focusable as _, Modifiers, Pixels,
    Point, TestAppContext, VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use gv_vault::files::journal::JournalStore;

use super::link_preview::CLOSE_DELAY;
use super::{OpenTarget, Workspace};
use crate::chrome::WindowChrome;
use crate::editor::EditorView;
use crate::keymap::actions::links as commands;
use crate::reader::Reader;
use crate::reader::link_info::TOOLTIP_DELAY;
use crate::{AppSettings, ThemeChoice, TogglePanel};

/// The default `preview.hover_preview_delay_ms`.
const DELAY: Duration = Duration::from_millis(300);

const HUB: &str = "# Hub

Exact [[A]].

Missing [[Missing]].

Ambiguous [[Dup]].

Site <https://example.com/page>.
";

struct Fixture {
    dir: tempfile::TempDir,
    _journal: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

fn fixture(cx: &mut TestAppContext) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join(".obsidian")).expect("marker");
    let files = [
        ("Hub.md", HUB),
        ("A.md", "# A\n\nThe note A, which links to [[B]].\n"),
        ("B.md", "# B\n\nThe note B.\n"),
        ("x/Dup.md", "one\n"),
        ("y/Dup.md", "two\n"),
    ];
    for (name, text) in files {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("folder");
        }
        std::fs::write(path, text).expect("write");
    }
    let target = Some(OpenTarget::new(dir.path().join("Hub.md")));
    let journal = tempfile::tempdir().expect("journal dir");
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let store = JournalStore::open(journal.path().join("journal")).expect("journal store");
        crate::files::install_store(cx, Some(store), false);
        crate::files::override_autosave(cx, Some(false));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1280.0), px(800.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let mut f = Fixture {
        dir,
        _journal: journal,
        workspace,
        vcx,
    };
    // The right panel starts the window's vault.
    f.vcx.dispatch_action(TogglePanel);
    f.draw();
    f.until("the vault's indexes", |f| {
        let vault = f.workspace.read_with(&f.vcx, |ws, _| ws.vault.clone());
        vault.is_some_and(|v| {
            v.read_with(&f.vcx, |v, _| {
                v.snapshot().is_complete() && v.content().is_some()
            })
        })
    });
    f.until("the statuses from the index", |f| {
        let statuses = f
            .reader()
            .read_with(&f.vcx, |r, _| r.link_statuses_for_tests());
        !statuses.is_empty()
            && !statuses
                .iter()
                .any(|(_, s)| *s == gv_render_tree::LinkStatus::Pending)
    });
    f
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..2 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

impl Fixture {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn draw(&mut self) {
        draw(&mut self.vcx);
    }

    fn until(&mut self, what: &str, f: impl Fn(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            let vault = self
                .workspace
                .read_with(&self.vcx, |ws, _| ws.vault.clone());
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

    fn has(&mut self, selector: &'static str) -> bool {
        self.vcx.debug_bounds(selector).is_some()
    }

    fn click(&mut self, selector: &'static str) {
        let at = self
            .vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center();
        self.vcx.simulate_click(at, Modifiers::none());
        self.draw();
    }

    fn reader(&self) -> Entity<Reader> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.active_tab().reader().cloned())
            .expect("a Reading view")
    }

    fn previewed(&self) -> Vec<String> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.previewed_links())
    }

    fn active_path(&self) -> Option<PathBuf> {
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.active_tab().path().map(Path::to_path_buf)
        })
    }

    fn link_point_in(&mut self, reader: &Entity<Reader>, needle: &str) -> Point<Pixels> {
        self.draw();
        reader
            .read_with(&self.vcx, |r, _| r.link_point_for_tests(needle))
            .unwrap_or_else(|| panic!("the link of {needle:?} is drawn"))
    }

    fn link_point(&mut self, needle: &str) -> Point<Pixels> {
        let reader = self.reader();
        self.link_point_in(&reader, needle)
    }

    /// Moves the pointer to `at` and lets `wait` pass.
    fn rest(&mut self, at: Point<Pixels>, modifiers: Modifiers, wait: Duration) {
        self.vcx.simulate_mouse_move(at, None, modifiers);
        self.draw();
        self.vcx.executor().advance_clock(wait);
        self.draw();
    }

    /// A point of the document with nothing on it.
    fn empty_point(&mut self) -> Point<Pixels> {
        let at = self.link_point("Exact");
        point(at.x, at.y + px(400.0))
    }

    fn edit(&mut self) -> Entity<EditorView> {
        self.workspace.update_in(&mut self.vcx, |ws, window, cx| {
            ws.edit_active_tab(None, window, cx);
        });
        self.until("the Source view", |f| {
            f.workspace.read_with(&f.vcx, |ws, cx| {
                ws.active_tab()
                    .editor()
                    .is_some_and(|e| e.read(cx).editor().is_some())
            })
        });
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.active_tab().editor().cloned())
            .expect("a Source view")
    }

    fn editor_point(&mut self, editor: &Entity<EditorView>, offset: usize) -> Point<Pixels> {
        self.draw();
        editor
            .update_in(&mut self.vcx, |e, window, cx| {
                e.bounds_for_range(offset..offset + 1, Bounds::default(), window, cx)
            })
            .expect("drawn")
            .center()
    }
}

#[gpui::test]
fn resting_on_a_link_previews_the_note_until_the_pointer_leaves(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let at = f.link_point("Exact");
    // Not before the delay.
    f.rest(at, Modifiers::none(), DELAY / 2);
    assert!(f.previewed().is_empty());
    f.vcx.executor().advance_clock(DELAY);
    f.draw();
    assert_eq!(f.previewed(), ["A"]);
    assert!(f.has("hover-preview-0"));
    assert!(f.has("hover-preview-note-0"));
    assert!(f.has("hover-preview-head-0"));
    // The note shows in the preview's own Reading view.
    let preview = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.preview_reader_for_tests(0))
        .expect("a preview reader");
    f.until("the previewed note", |f| {
        preview.read_with(&f.vcx, |r, _| {
            r.path() == Some(f.path("A.md").as_path())
                && r.plain_text().is_some_and(|t| t.contains("The note A"))
        })
    });

    // Moving onto the preview keeps it.
    let inside = f
        .vcx
        .debug_bounds("hover-preview-head-0")
        .expect("drawn")
        .center();
    f.rest(inside, Modifiers::none(), CLOSE_DELAY * 2);
    assert_eq!(f.previewed(), ["A"]);
    // Leaving both the link and the preview closes it after a moment.
    let away = f.empty_point();
    f.rest(away, Modifiers::none(), CLOSE_DELAY / 2);
    assert_eq!(f.previewed(), ["A"], "not at once");
    f.vcx.executor().advance_clock(CLOSE_DELAY);
    f.draw();
    assert!(f.previewed().is_empty());
    assert!(!f.has("hover-preview-0"));
}

#[gpui::test]
fn esc_closes_and_enter_opens(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let at = f.link_point("Exact");
    f.rest(at, Modifiers::none(), DELAY);
    assert_eq!(f.previewed(), ["A"]);
    // Esc closes it without the keyboard in it; resting on still does not
    // bring it back.
    f.vcx.simulate_keystrokes("escape");
    f.draw();
    assert!(f.previewed().is_empty());
    f.vcx.executor().advance_clock(DELAY * 2);
    f.reader().update(&mut f.vcx, |_, cx| cx.notify());
    f.draw();
    assert!(f.previewed().is_empty());

    // A click on the head opens the note in place.
    let away = f.empty_point();
    f.rest(away, Modifiers::none(), DELAY);
    f.rest(at, Modifiers::none(), DELAY);
    assert_eq!(f.previewed(), ["A"]);
    f.click("hover-preview-head-0");
    assert!(f.previewed().is_empty());
    assert_eq!(f.active_path(), Some(f.path("A.md")));
}

#[gpui::test]
fn unresolved_ambiguous_and_web_links_explain_themselves(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    // Unresolved: the explanation and Create note.
    let at = f.link_point("Missing");
    f.rest(at, Modifiers::none(), DELAY);
    assert_eq!(f.previewed(), ["Missing"]);
    assert!(f.has("hover-preview-unresolved-0"));
    assert!(!f.has("link-tooltip"), "the preview replaces the tooltip");
    f.click("hover-preview-create-0");
    let created = f.path("Missing.md");
    f.until("the new note", |f| {
        f.active_path().as_deref() == Some(created.as_path())
    });
    assert!(created.exists());
    std::fs::remove_file(&created).expect("delete");
    f.workspace
        .update_in(&mut f.vcx, |ws, window, cx| ws.go_back(window, cx));
    f.draw();
    assert_eq!(f.active_path(), Some(f.path("Hub.md")));

    // Ambiguous: the candidates, a click opens one.
    let at = f.link_point("Ambiguous");
    f.rest(at, Modifiers::none(), DELAY);
    assert!(f.has("hover-preview-candidates-0"));
    f.click("hover-preview-candidate-1");
    assert_eq!(f.active_path(), Some(f.path("y/Dup.md")));
    f.workspace
        .update_in(&mut f.vcx, |ws, window, cx| ws.go_back(window, cx));
    f.draw();

    // A web address: the address and its domain, nothing fetched.
    let at = f.link_point("Site");
    f.rest(at, Modifiers::none(), DELAY);
    assert!(f.has("hover-preview-text-0"));
    assert_eq!(f.previewed(), ["https://example.com/page"]);
}

#[gpui::test]
fn a_link_in_a_preview_has_its_own_preview_and_follows(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let at = f.link_point("Exact");
    f.rest(at, Modifiers::none(), DELAY);
    let preview = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.preview_reader_for_tests(0))
        .expect("a preview reader");
    f.until("the previewed note", |f| {
        preview.read_with(&f.vcx, |r, _| {
            r.plain_text().is_some_and(|t| t.contains("The note A"))
        })
    });
    // Resting on the link inside the preview: a second preview.
    let inner = f.link_point_in(&preview, "which links");
    f.rest(inner, Modifiers::none(), DELAY);
    assert_eq!(f.previewed(), ["A", "B"]);
    assert!(f.has("hover-preview-1"));
    // Esc closes the inner one only.
    f.vcx.simulate_keystrokes("escape");
    f.draw();
    assert_eq!(f.previewed(), ["A"]);
    // A click on the link inside the preview follows it from note A, in
    // the tab.
    f.vcx.simulate_click(inner, Modifiers::none());
    f.draw();
    assert!(f.previewed().is_empty());
    assert_eq!(f.active_path(), Some(f.path("B.md")));
}

#[gpui::test]
fn preview_link_under_caret_and_mod_hover(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let editor = f.edit();
    // Resting on a link without Mod previews nothing in the Source view.
    let offset = HUB.find("[[A]]").expect("link") + 2;
    let at = f.editor_point(&editor, offset);
    f.rest(at, Modifiers::none(), TOOLTIP_DELAY);
    assert!(f.previewed().is_empty());
    // Pressing Mod while it rests there previews it.
    let secondary = f.vcx.update(|_, cx| Modifiers::secondary_key(cx));
    f.rest(at, secondary, Duration::ZERO);
    assert_eq!(f.previewed(), ["A"]);
    let away = f.editor_point(&editor, 2);
    f.rest(away, Modifiers::none(), CLOSE_DELAY * 2);
    assert!(f.previewed().is_empty());

    // The command previews the link under the caret, with the keyboard;
    // Enter opens it.
    editor.update(&mut f.vcx, |e, cx| e.set_cursor_for_tests(offset, cx));
    editor.update_in(&mut f.vcx, |e, window, cx| {
        e.focus_handle(cx).focus(window, cx)
    });
    f.draw();
    f.vcx.dispatch_action(commands::Preview);
    f.draw();
    assert_eq!(f.previewed(), ["A"]);
    // The pointer moving elsewhere leaves a keyboard preview open.
    f.rest(away, Modifiers::none(), CLOSE_DELAY * 2);
    assert_eq!(f.previewed(), ["A"]);
    f.vcx.simulate_keystrokes("enter");
    f.draw();
    assert!(f.previewed().is_empty());
    assert_eq!(f.active_path(), Some(f.path("A.md")));
    crate::files::overlay::clean(&f.path("Hub.md"));
}

// ----- Links in this note, Copy link to heading ------------------------------------------

fn goto_listed(f: &mut Fixture) -> Vec<String> {
    let popover = f
        .vcx
        .update(|window, cx| crate::goto::goto_of(window, cx))
        .expect("the window has a Go popover");
    popover.read_with(
        &f.vcx,
        |p, _| if p.is_open() { p.listed() } else { Vec::new() },
    )
}

#[gpui::test]
fn links_in_this_note_lists_and_follows(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.vcx.dispatch_action(commands::List);
    f.draw();
    assert_eq!(
        goto_listed(&mut f),
        [
            "[[A]]",
            "[[Missing]]",
            "[[Dup]]",
            "<https://example.com/page>"
        ]
        .map(super::links_data::link_label)
    );
    // Typing filters; Enter follows as a click would: an unresolved link
    // offers to create the note.
    f.vcx.simulate_input("Missing");
    f.draw();
    f.vcx.simulate_keystrokes("enter");
    f.draw();
    assert!(goto_listed(&mut f).is_empty(), "the popover closed");
    assert!(matches!(
        f.workspace
            .read_with(&f.vcx, |ws, _| ws.link_popover().cloned()),
        Some(super::link_actions::PopoverKind::Create { .. })
    ));
    f.vcx.simulate_keystrokes("escape");
    f.draw();
    // A resolved link opens its note.
    f.vcx.dispatch_action(commands::List);
    f.draw();
    f.vcx.simulate_keystrokes("enter");
    f.draw();
    assert_eq!(f.active_path(), Some(f.path("A.md")));
}

#[gpui::test]
fn copy_link_to_heading_or_block(cx: &mut TestAppContext) {
    use super::link_list::link_to_for_tests as link_to;
    assert_eq!(
        link_to("Notes/Big plan.md", Ok("Next steps"), false),
        "[[Big plan#Next steps]]"
    );
    assert_eq!(
        link_to("Notes/Big plan.md", Err("abc"), false),
        "[[Big plan#^abc]]"
    );
    assert_eq!(
        link_to("Notes/Big plan.md", Ok("Next [steps]"), false),
        "[[Big plan#Next steps]]"
    );
    assert_eq!(
        link_to("Notes/Big plan.md", Ok("Next steps"), true),
        "[Next steps](Big%20plan.md#Next%20steps)"
    );
    assert_eq!(
        link_to("Big plan.md", Err("abc"), true),
        "[Big plan ^abc](Big%20plan.md#^abc)"
    );

    // In the Source view: the heading the caret is in.
    let mut f = fixture(cx);
    let editor = f.edit();
    let offset = HUB.find("Exact").expect("text");
    editor.update(&mut f.vcx, |e, cx| e.set_cursor_for_tests(offset, cx));
    editor.update_in(&mut f.vcx, |e, window, cx| {
        e.focus_handle(cx).focus(window, cx)
    });
    f.draw();
    f.vcx.dispatch_action(commands::CopyLinkToHeading);
    f.draw();
    let copied = f
        .vcx
        .update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert_eq!(copied.as_deref(), Some("[[Hub#Hub]]"));
    crate::files::overlay::clean(&f.path("Hub.md"));
}
