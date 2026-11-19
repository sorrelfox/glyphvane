// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Links with no single target and the link commands, in headless windows
//! over temporary Obsidian vaults: the status styles and tooltips in both
//! views, create on click (and its link rewrite, undone in one step), the
//! ambiguous link's infobar, candidates and rewrite, pending links, the
//! resolution trace, every link menu command, Mod+click in the Source view.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Bounds, Entity, EntityInputHandler as _, Focusable as _, Modifiers,
    MouseButton, Pixels, Point, TestAppContext, VisualTestContext, WindowBounds, WindowOptions,
    point, px, size,
};
use gv_inline::UnderlineStyle;
use gv_render_tree::{LinkStatus, LinkSuffix, StatusIcon, link_decor};
use gv_vault::files::journal::JournalStore;

use super::link_actions::{PENDING_WAIT, PopoverKind};
use super::{OpenTarget, Workspace};
use crate::chrome::WindowChrome;
use crate::editor::EditorView;
use crate::files::notice::{NoticeAction, Topic};
use crate::keymap::actions::links as commands;
use crate::menu::ContextMenu;
use crate::reader::Reader;
use crate::reader::link_info::TOOLTIP_DELAY;
use crate::{AppSettings, ThemeChoice, TogglePanel};

struct Fixture {
    dir: tempfile::TempDir,
    _journal: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

const HUB: &str = "# Hub

Exact [[A]].

Case [[a]].

Ambiguous [[Dup]].

Missing [[Missing]].

Odd [[What?]].

Site <https://example.com/page>.

Script [bad](javascript:alert(1)).
";

fn vault_files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Hub.md", HUB),
        ("A.md", "# A\n"),
        ("x/Dup.md", "one\n"),
        ("y/Dup.md", "two\n"),
    ]
}

/// An Obsidian vault with `files` and a window showing `open`. With
/// `vault`, the right panel is shown, which starts the window's vault.
fn fixture(cx: &mut TestAppContext, files: &[(&str, &str)], open: &str, vault: bool) -> Fixture {
    fixture_in(cx, ".obsidian", files, open, vault)
}

/// The same in a folder with root marker `marker` (`.git`: GitHub's
/// rules).
fn fixture_in(
    cx: &mut TestAppContext,
    marker: &str,
    files: &[(&str, &str)],
    open: &str,
    vault: bool,
) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join(marker)).expect("marker");
    for (name, text) in files {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("folder");
        }
        std::fs::write(path, text).expect("write");
    }
    let target = Some(OpenTarget::new(dir.path().join(open)));
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
    if vault {
        f.vcx.dispatch_action(TogglePanel);
        draw(&mut f.vcx);
        f.until("the vault's indexes", |f| {
            let vault = f.workspace.read_with(&f.vcx, |ws, _| ws.vault.clone());
            vault.is_some_and(|v| {
                v.read_with(&f.vcx, |v, _| {
                    v.snapshot().is_complete() && v.content().is_some()
                })
            })
        });
    }
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

    /// Waits (in real time: the indexes have their own threads) until `f`
    /// holds.
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

    fn editor_of(&self, name: &str) -> Option<Entity<EditorView>> {
        let path = self.path(name);
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.tabs()
                .iter()
                .find(|t| t.path() == Some(path.as_path()))
                .and_then(|t| t.editor().cloned())
        })
    }

    fn editor_text(&self, name: &str) -> Option<String> {
        let editor = self.editor_of(name)?;
        editor.read_with(&self.vcx, |e, _| e.editor().map(|e| e.text().to_string()))
    }

    fn undo(&mut self, name: &str) {
        let editor = self.editor_of(name).expect("a Source view");
        editor.update(&mut self.vcx, |e, cx| e.undo_for_tests(cx));
        self.draw();
    }

    fn active_path(&self) -> Option<PathBuf> {
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.active_tab().path().map(Path::to_path_buf)
        })
    }

    fn popover(&self) -> Option<PopoverKind> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.link_popover().cloned())
    }

    fn statuses(&self) -> Vec<LinkStatus> {
        self.reader()
            .read_with(&self.vcx, |r, _| r.link_statuses_for_tests())
            .into_iter()
            .map(|(_, s)| s)
            .collect()
    }

    /// Waits for the Reading view's statuses from the vault index.
    fn resolved(&mut self) {
        self.until("the statuses from the index", |f| {
            !f.statuses().is_empty() && !f.statuses().contains(&LinkStatus::Pending)
        });
    }

    fn link_point(&mut self, needle: &str) -> Point<Pixels> {
        self.draw();
        self.reader()
            .read_with(&self.vcx, |r, _| r.link_point_for_tests(needle))
            .unwrap_or_else(|| panic!("the link of {needle:?} is drawn"))
    }

    fn click_link(&mut self, needle: &str) {
        let at = self.link_point(needle);
        self.vcx.simulate_click(at, Modifiers::none());
        self.draw();
    }

    fn right_click_at(&mut self, at: Point<Pixels>) {
        self.vcx
            .simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
        self.vcx
            .simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
        self.draw();
    }

    fn menu(&self) -> Option<ContextMenu> {
        let popover = self
            .workspace
            .read_with(&self.vcx, |ws, _| ws.context_menu().clone());
        popover.read_with(&self.vcx, |p, _| p.kind())
    }

    /// Closes the hover previews: the last link's covers the lines below
    /// it.
    fn close_previews(&mut self) {
        self.workspace.update_in(&mut self.vcx, |ws, window, cx| {
            ws.close_previews(window, cx)
        });
        self.draw();
    }

    /// Rests the pointer on `at` until the tooltip is due.
    fn hover(&mut self, at: Point<Pixels>) {
        self.close_previews();
        self.vcx.simulate_mouse_move(at, None, Modifiers::none());
        self.draw();
        self.vcx.executor().advance_clock(TOOLTIP_DELAY);
        self.draw();
    }

    fn reader_tooltip(&self) -> Option<(LinkStatus, String)> {
        self.reader()
            .read_with(&self.vcx, |r, _| r.hovered_link_tooltip())
    }

    fn notice(&self) -> Option<crate::files::notice::Notice> {
        self.workspace
            .read_with(&self.vcx, |ws, cx| ws.infobar_notice(cx))
    }

    fn press(&mut self, action: &NoticeAction) {
        let notice = self.notice().expect("an infobar");
        let index = notice
            .buttons
            .iter()
            .position(|b| &b.action == action)
            .unwrap_or_else(|| panic!("the infobar offers {action:?}"));
        let selector: &'static str = Box::leak(format!("infobar-action-{index}").into_boxed_str());
        self.click(selector);
    }

    /// Edit mode on the active tab, its Source view loaded and focused.
    fn edit(&mut self) -> Entity<EditorView> {
        self.workspace.update_in(&mut self.vcx, |ws, window, cx| {
            ws.edit_active_tab(None, window, cx);
        });
        let name = self
            .active_path()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .expect("a file");
        self.until("the Source view", |f| f.editor_text(&name).is_some());
        self.editor_of(&name).expect("a Source view")
    }

    /// Where the Source view draws byte `offset` (ASCII text).
    fn editor_point(&mut self, editor: &Entity<EditorView>, offset: usize) -> Point<Pixels> {
        self.draw();
        let bounds = editor
            .update_in(&mut self.vcx, |e, window, cx| {
                e.bounds_for_range(offset..offset + 1, Bounds::default(), window, cx)
            })
            .expect("drawn");
        bounds.center()
    }
}

fn delete(path: &Path) {
    let _ = std::fs::remove_file(path);
}

// ----- statuses: styles and tooltips ---------------------------------------------------

#[gpui::test]
fn every_status_is_styled_and_explained_in_the_reading_view(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md", true);
    f.resolved();
    assert_eq!(
        f.statuses(),
        [
            LinkStatus::Exact,
            // Obsidian ignores letter case: an exact match (the checker
            // still offers the case fix).
            LinkStatus::Exact,
            LinkStatus::Ambiguous(2),
            LinkStatus::Unresolved,
            LinkStatus::Unresolved,
            LinkStatus::External,
            LinkStatus::Blocked,
        ]
    );
    // The styles the render tree gives each (SPEC § Link status).
    let decor = |s| link_decor(s);
    assert_eq!(decor(LinkStatus::Exact).suffix, None);
    assert_eq!(decor(LinkStatus::CaseFold).suffix, Some(LinkSuffix::Approx));
    assert!(matches!(
        decor(LinkStatus::Ambiguous(2)).suffix,
        Some(LinkSuffix::Icon {
            icon: StatusIcon::TriangleAlert,
            count: Some(2),
            ..
        })
    ));
    let missing = decor(LinkStatus::Unresolved);
    assert_eq!(missing.underline.map(|u| u.0), Some(UnderlineStyle::Dashed));
    assert!(matches!(
        missing.suffix,
        Some(LinkSuffix::Icon {
            icon: StatusIcon::FilePlusDashed,
            ..
        })
    ));
    assert!(decor(LinkStatus::Blocked).strike);
    assert_eq!(decor(LinkStatus::Pending), decor(LinkStatus::Exact));

    // An exact link needs no explanation.
    let at = f.link_point("Exact");
    f.hover(at);
    assert_eq!(f.reader_tooltip(), None);
    assert!(!f.has("link-tooltip"));
    // The others explain their status after a moment.
    for (needle, status, words) in [
        (
            "Ambiguous",
            LinkStatus::Ambiguous(2),
            vec!["2 notes match “Dup”", "x/Dup.md", "y/Dup.md"],
        ),
        (
            "Missing",
            LinkStatus::Unresolved,
            vec!["No note “Missing” — click to create it."],
        ),
        ("Script", LinkStatus::Blocked, vec!["Blocked"]),
    ] {
        f.close_previews();
        let at = f.link_point(needle);
        f.vcx.simulate_mouse_move(at, None, Modifiers::none());
        f.draw();
        f.vcx.executor().advance_clock(TOOLTIP_DELAY);
        f.draw();
        // The hover preview explains it; the window shows no tooltip too.
        assert!(f.has("hover-preview-0"), "{needle}: the preview shows");
        assert!(!f.has("link-tooltip"), "{needle}: no tooltip over it");
        let (shown, text) = f.reader_tooltip().expect("a tooltip");
        assert_eq!(shown, status);
        for w in words {
            assert!(text.contains(w), "{needle}: {text:?} has {w:?}");
        }
    }
}

#[gpui::test]
fn case_only_and_loose_matches_are_marked_and_explained(cx: &mut TestAppContext) {
    let files = [
        (
            "doc.md",
            "# Doc\n\nCase [x](readme.md).\n\nLoose [y](other).\n",
        ),
        ("README.md", "# Readme\n"),
        ("other.md", "# Other\n"),
        (
            ".git/config",
            "[remote \"origin\"]\n\turl = https://github.com/example/docs.git\n",
        ),
    ];
    let mut f = fixture_in(cx, ".git", &files, "doc.md", true);
    f.resolved();
    assert_eq!(f.statuses(), [LinkStatus::CaseFold, LinkStatus::Loose]);
    for status in [LinkStatus::CaseFold, LinkStatus::Loose] {
        assert_eq!(link_decor(status).suffix, Some(LinkSuffix::Approx));
    }
    let at = f.link_point("Case");
    f.hover(at);
    let (status, text) = f.reader_tooltip().expect("a tooltip");
    assert_eq!(status, LinkStatus::CaseFold);
    assert!(
        text.contains("letter case") && text.contains("README.md"),
        "{text}"
    );
    let at = f.link_point("Loose");
    f.hover(at);
    let (status, text) = f.reader_tooltip().expect("a tooltip");
    assert_eq!(status, LinkStatus::Loose);
    assert!(
        text.contains("only loosely") && text.contains("other.md"),
        "{text}"
    );
    // Fix link: the letter case.
    run_from_menu(&mut f, "Case", "links.fix");
    f.until("the case fix", |f| {
        f.editor_text("doc.md")
            .is_some_and(|t| t.contains("[x](README.md)"))
    });
    crate::files::overlay::clean(&f.path("doc.md"));
}

#[gpui::test]
fn without_the_index_links_are_pending_and_say_so(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md", false);
    let statuses = f.statuses();
    assert!(statuses.contains(&LinkStatus::Pending), "{statuses:?}");
    assert!(!statuses.contains(&LinkStatus::Unresolved));
    let at = f.link_point("Missing");
    f.hover(at);
    let (status, text) = f.reader_tooltip().expect("a tooltip");
    assert_eq!(status, LinkStatus::Pending);
    assert!(text.contains("still being indexed"), "{text}");
    // Following a pending link waits for the index, then follows the
    // link as the file system has it: no create offer while pending.
    f.click_link("Ambiguous");
    f.vcx.executor().advance_clock(PENDING_WAIT);
    f.draw();
    assert_eq!(f.popover().map(|_| ()), None);
    let notice = f.notice().expect("an infobar");
    assert_eq!(notice.title.as_ref(), "The vault is still being indexed.");
}

#[gpui::test]
fn the_source_view_explains_statuses_under_the_pointer(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md", true);
    let editor = f.edit();
    f.until("the Source view's statuses", |f| {
        let editor = f.editor_of("Hub.md").expect("editor");
        let offset = HUB.find("[[Missing").expect("link") + 3;
        let at = f.editor_point(&editor, offset);
        f.hover(at);
        editor
            .update(&mut f.vcx, |e, _| e.hovered_link_tooltip())
            .is_some_and(|(s, _)| s == LinkStatus::Unresolved)
    });
    assert!(f.has("link-tooltip"));
    let (_, text) = editor
        .update(&mut f.vcx, |e, _| e.hovered_link_tooltip())
        .expect("a tooltip");
    assert!(
        text.contains("Ctrl+click to create it") || text.contains("Cmd+click"),
        "{text}"
    );
    let offset = HUB.find("[[Dup").expect("link") + 3;
    let at = f.editor_point(&editor, offset);
    f.hover(at);
    let (status, text) = editor
        .update(&mut f.vcx, |e, _| e.hovered_link_tooltip())
        .expect("a tooltip");
    assert_eq!(status, LinkStatus::Ambiguous(2));
    assert!(
        text.contains("x/Dup.md") && text.contains("y/Dup.md"),
        "{text}"
    );
    // An exact link has none.
    let offset = HUB.find("[[A]]").expect("link") + 3;
    let at = f.editor_point(&editor, offset);
    f.hover(at);
    assert_eq!(
        editor.update(&mut f.vcx, |e, _| e.hovered_link_tooltip()),
        None
    );
}

// ----- unresolved: create on click -----------------------------------------------------

#[gpui::test]
fn clicking_an_unresolved_link_offers_to_create_the_note(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md", true);
    f.resolved();
    f.click_link("Missing");
    let Some(PopoverKind::Create { note, .. }) = f.popover() else {
        panic!("the Create note popover");
    };
    assert_eq!(note.path.as_str(), "Missing.md");
    assert_eq!(note.rewrite, None);
    assert!(f.has("link-create"));
    // Cancel creates nothing.
    f.click("link-create-cancel");
    assert!(f.popover().is_none());
    assert!(!f.path("Missing.md").exists());
    // Create makes the note and opens it for editing in place.
    f.click_link("Missing");
    f.click("link-create-confirm");
    let created = f.path("Missing.md");
    f.until("the new note", |f| {
        f.active_path().as_deref() == Some(created.as_path())
    });
    assert!(created.exists());
    assert_eq!(std::fs::read_to_string(&created).expect("read"), "");
    let mode = f.workspace.read_with(&f.vcx, |ws, _| ws.mode());
    assert_eq!(mode, super::ViewMode::Edit);
    // Back in Hub, the link resolves now.
    f.workspace
        .update_in(&mut f.vcx, |ws, window, cx| ws.go_back(window, cx));
    f.until("Hub with the link resolved", |f| {
        f.active_path() == Some(f.path("Hub.md")) && f.statuses().get(3) == Some(&LinkStatus::Exact)
    });
    delete(&created);
}

#[gpui::test]
fn a_name_not_valid_everywhere_is_sanitised_and_the_link_rewritten(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md", true);
    f.resolved();
    f.click_link("Odd");
    let Some(PopoverKind::Create { note, .. }) = f.popover() else {
        panic!("the Create note popover");
    };
    assert_eq!(note.path.as_str(), "What.md");
    assert_eq!(note.rewrite.as_deref(), Some("What"));
    f.vcx.simulate_keystrokes("enter");
    f.draw();
    let created = f.path("What.md");
    f.until("the new note", |f| {
        f.active_path().as_deref() == Some(created.as_path())
            && f.editor_text("Hub.md")
                .is_some_and(|t| t.contains("[[What]]"))
    });
    assert!(created.exists());
    // The rewrite is one undoable edit in Hub's buffer.
    f.undo("Hub.md");
    assert_eq!(f.editor_text("Hub.md").as_deref(), Some(HUB));
    crate::files::overlay::clean(&f.path("Hub.md"));
    delete(&created);
}

#[gpui::test]
fn new_file_location_puts_a_bare_name_where_the_vault_says(cx: &mut TestAppContext) {
    let mut files = vault_files();
    files.push((
        ".obsidian/app.json",
        "{\"newFileLocation\": \"folder\", \"newFileFolderPath\": \"Inbox\"}",
    ));
    files.push(("sub/Note.md", "[[Fresh]] and [[deep/Fresh]]\n"));
    let mut f = fixture(cx, &files, "sub/Note.md", true);
    f.resolved();
    let at = f.link_point("Fresh");
    f.vcx.simulate_click(at, Modifiers::none());
    f.draw();
    let Some(PopoverKind::Create { note, .. }) = f.popover() else {
        panic!("the Create note popover");
    };
    assert_eq!(note.path.as_str(), "Inbox/Fresh.md");
}

// ----- ambiguous ---------------------------------------------------------------------------

#[gpui::test]
fn an_ambiguous_link_opens_the_best_candidate_with_an_infobar(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md", true);
    f.resolved();
    f.click_link("Ambiguous");
    let opened = f.active_path().expect("a file");
    assert!(opened.ends_with("Dup.md"), "{opened:?}");
    let notice = f.notice().expect("the infobar");
    assert_eq!(notice.topic, Topic::AmbiguousLink);
    assert_eq!(notice.title.as_ref(), "2 notes match “Dup”.");
    assert!(notice.offers(&NoticeAction::ChooseLinkTarget));
    assert!(notice.offers(&NoticeAction::RewriteLink));
    // Choose…: the candidates; choosing the other opens it.
    f.press(&NoticeAction::ChooseLinkTarget);
    let Some(PopoverKind::Choose { rows, rewrite }) = f.popover() else {
        panic!("the candidates");
    };
    assert!(!rewrite);
    assert_eq!(
        rows.iter().map(|r| r.label.to_string()).collect::<Vec<_>>(),
        ["x/Dup.md", "y/Dup.md"]
    );
    let other = rows
        .iter()
        .position(|r| r.path != opened)
        .expect("another candidate");
    f.vcx
        .simulate_keystrokes(if other == 1 { "down enter" } else { "enter" });
    f.draw();
    let now = f.active_path().expect("a file");
    assert_ne!(now, opened);
    assert!(now.ends_with("Dup.md"));
}

#[gpui::test]
fn rewrite_link_names_the_note_opened_in_one_undoable_edit(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md", true);
    f.resolved();
    f.click_link("Ambiguous");
    let opened = f.active_path().expect("a file");
    let folder = opened
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .expect("folder");
    f.press(&NoticeAction::RewriteLink);
    let expected = HUB.replace("[[Dup]]", &format!("[[{folder}/Dup]]"));
    f.until("the rewrite", |f| {
        f.editor_text("Hub.md").as_deref() == Some(expected.as_str())
    });
    assert_eq!(f.notice().map(|n| n.topic), None);
    f.undo("Hub.md");
    assert_eq!(f.editor_text("Hub.md").as_deref(), Some(HUB));
    crate::files::overlay::clean(&f.path("Hub.md"));
}

// ----- the link menu -----------------------------------------------------------------------

fn clipboard(f: &mut Fixture) -> Option<String> {
    f.vcx
        .update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))
}

/// Opens the link menu on the link of `needle` and runs `command`.
fn run_from_menu(f: &mut Fixture, needle: &str, command: &'static str) {
    let at = f.link_point(needle);
    f.right_click_at(at);
    assert_eq!(f.menu(), Some(ContextMenu::Link));
    let selector: &'static str = Box::leak(format!("context-menu-item:{command}").into_boxed_str());
    let disabled: &'static str =
        Box::leak(format!("context-menu-disabled:{command}").into_boxed_str());
    assert!(!f.has(disabled), "{command} is offered for {needle}");
    f.click(selector);
}

#[gpui::test]
fn the_link_menu_commands_act_on_the_link_under_the_pointer(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md", true);
    f.resolved();

    // Copy link, copy resolved path, copy URL.
    run_from_menu(&mut f, "Exact", "links.copy_link");
    assert_eq!(clipboard(&mut f).as_deref(), Some("[[A]]"));
    run_from_menu(&mut f, "Exact", "links.copy_resolved_path");
    let a = f.path("A.md");
    assert_eq!(clipboard(&mut f), Some(a.display().to_string()));
    run_from_menu(&mut f, "Site", "links.copy_url");
    assert_eq!(
        clipboard(&mut f).as_deref(),
        Some("https://example.com/page")
    );

    // What a link cannot do is dimmed.
    let at = f.link_point("Site");
    f.right_click_at(at);
    for dimmed in [
        "context-menu-disabled:links.copy_resolved_path",
        "context-menu-disabled:links.reveal_in_tree",
        "context-menu-disabled:links.fix",
        "context-menu-disabled:links.open_in_new_window",
    ] {
        assert!(f.has(dimmed), "{dimmed}");
    }
    f.vcx.simulate_keystrokes("escape");
    f.draw();

    // Show resolution trace: the resolver's steps in a popover.
    run_from_menu(&mut f, "Ambiguous", "links.show_trace");
    let Some(PopoverKind::Trace { title, lines }) = f.popover() else {
        panic!("the trace");
    };
    assert!(title.contains("[[Dup]] from Hub.md"), "{title}");
    assert!(title.contains("obsidian"), "{title}");
    let text = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Ambiguous(2)"), "{text}");
    assert!(f.has("link-trace"));
    f.vcx.simulate_keystrokes("escape");
    f.draw();
    assert!(f.popover().is_none());

    // Reveal in file tree: the Files panel selects the target.
    run_from_menu(&mut f, "Exact", "links.reveal_in_tree");
    let selected = {
        let files = f.workspace.read_with(&f.vcx, |ws, _| ws.files.clone());
        files.and_then(|files| files.read_with(&f.vcx, |p, _| p.selected_name()))
    };
    assert_eq!(selected.as_deref(), Some("A.md"));

    // Open in new tab.
    let tabs = f.workspace.read_with(&f.vcx, |ws, _| ws.tabs().len());
    run_from_menu(&mut f, "Exact", "links.open_in_new_tab");
    let now = f.workspace.read_with(&f.vcx, |ws, _| ws.tabs().len());
    assert_eq!(now, tabs + 1);
    assert_eq!(f.active_path(), Some(a.clone()));
}

#[gpui::test]
fn the_link_menu_opens_follows_fixes_and_creates(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md", true);
    f.resolved();
    // Fix link on a case-only match: the letter case, one undoable edit.
    run_from_menu(&mut f, "Case", "links.fix");
    let fixed = HUB.replace("[[a]]", "[[A]]");
    f.until("the case fix", |f| {
        f.editor_text("Hub.md").as_deref() == Some(fixed.as_str())
    });
    f.undo("Hub.md");
    assert_eq!(f.editor_text("Hub.md").as_deref(), Some(HUB));

    // Fix link on an ambiguous link: a picker of the rewrites.
    run_from_menu(&mut f, "Ambiguous", "links.fix");
    let Some(PopoverKind::Choose { rows, rewrite }) = f.popover() else {
        panic!("the rewrites");
    };
    assert!(rewrite);
    assert_eq!(
        rows.iter().map(|r| r.label.to_string()).collect::<Vec<_>>(),
        ["Use x/Dup.md", "Use y/Dup.md"]
    );
    f.click("link-choice-1");
    let fixed = HUB.replace("[[Dup]]", "[[y/Dup]]");
    f.until("the rewrite", |f| {
        f.editor_text("Hub.md").as_deref() == Some(fixed.as_str())
    });
    f.undo("Hub.md");
    assert_eq!(f.editor_text("Hub.md").as_deref(), Some(HUB));
    crate::files::overlay::clean(&f.path("Hub.md"));

    // Open (follow) and Reveal in file manager stay enabled on a link.
    f.workspace.update_in(&mut f.vcx, |ws, window, cx| {
        ws.activate_tab(0, window, cx);
    });
    f.draw();
    run_from_menu(&mut f, "Exact", "links.follow");
    assert_eq!(f.active_path(), Some(f.path("A.md")));
}

#[gpui::test]
fn open_in_new_window_opens_a_window_for_the_target(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md", true);
    f.resolved();
    let before = f.vcx.update(|_, cx| cx.windows().len());
    run_from_menu(&mut f, "Exact", "links.open_in_new_window");
    f.draw();
    let after = f.vcx.update(|_, cx| cx.windows().len());
    assert_eq!(after, before + 1);
}

// ----- the Source view ---------------------------------------------------------------------

#[gpui::test]
fn mod_click_in_the_source_view_follows_and_a_plain_click_does_not(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md", true);
    f.resolved();
    let editor = f.edit();
    let offset = HUB.find("[[A]]").expect("link") + 2;
    let at = f.editor_point(&editor, offset);
    f.vcx.simulate_click(at, Modifiers::none());
    f.draw();
    assert_eq!(
        f.active_path(),
        Some(f.path("Hub.md")),
        "a plain click stays"
    );
    let secondary = f.vcx.update(|_, cx| Modifiers::secondary_key(cx));
    f.vcx.simulate_click(at, secondary);
    f.draw();
    assert_eq!(f.active_path(), Some(f.path("A.md")));

    // Mod+click on an unresolved link offers to create it.
    f.workspace
        .update_in(&mut f.vcx, |ws, window, cx| ws.go_back(window, cx));
    f.draw();
    let editor = f.edit();
    let offset = HUB.find("[[Missing").expect("link") + 3;
    let at = f.editor_point(&editor, offset);
    let secondary = f.vcx.update(|_, cx| Modifiers::secondary_key(cx));
    f.vcx.simulate_click(at, secondary);
    f.draw();
    assert!(matches!(f.popover(), Some(PopoverKind::Create { .. })));
}

#[gpui::test]
fn the_source_view_link_menu_and_caret_commands(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md", true);
    f.resolved();
    let editor = f.edit();
    // A right-click on a link: the link menu.
    let offset = HUB.find("[[a]]").expect("link") + 2;
    let at = f.editor_point(&editor, offset);
    f.right_click_at(at);
    assert_eq!(f.menu(), Some(ContextMenu::Link));
    f.click("context-menu-item:links.copy_link");
    assert_eq!(clipboard(&mut f).as_deref(), Some("[[a]]"));
    // Elsewhere: the editor's menu.
    let at = f.editor_point(&editor, 2);
    f.right_click_at(at);
    assert_eq!(f.menu(), Some(ContextMenu::Editor));
    f.vcx.simulate_keystrokes("escape");
    f.draw();
    // With the caret on a link: the commands act on it (Fix link here).
    editor.update(&mut f.vcx, |e, cx| e.set_cursor_for_tests(offset, cx));
    editor.update_in(&mut f.vcx, |e, window, cx| {
        e.focus_handle(cx).focus(window, cx)
    });
    f.draw();
    f.vcx.dispatch_action(commands::Fix);
    f.draw();
    let fixed = HUB.replace("[[a]]", "[[A]]");
    f.until("the case fix", |f| {
        f.editor_text("Hub.md").as_deref() == Some(fixed.as_str())
    });
    f.undo("Hub.md");
    // The context menu key on a link opens the link menu.
    editor.update(&mut f.vcx, |e, cx| e.set_cursor_for_tests(offset, cx));
    f.draw();
    f.vcx.simulate_keystrokes("shift-f10");
    f.draw();
    assert_eq!(f.menu(), Some(ContextMenu::Link));
    f.vcx.simulate_keystrokes("escape");
    f.draw();
    // Create note on the unresolved link under the caret.
    let missing = HUB.find("[[Missing").expect("link") + 3;
    editor.update(&mut f.vcx, |e, cx| e.set_cursor_for_tests(missing, cx));
    f.draw();
    f.vcx.dispatch_action(commands::CreateNote);
    f.draw();
    let created = f.path("Missing.md");
    f.until("the new note", |f| {
        f.active_path().as_deref() == Some(created.as_path())
    });
    assert!(created.exists());
    delete(&created);
    f.workspace
        .update_in(&mut f.vcx, |ws, window, cx| ws.activate_tab(0, window, cx));
    f.draw();
    let editor = f.edit();
    // Alt+Enter follows the link under the caret.
    let exact = HUB.find("[[A]]").expect("link") + 2;
    editor.update(&mut f.vcx, |e, cx| e.set_cursor_for_tests(exact, cx));
    f.draw();
    f.vcx.dispatch_action(commands::Follow);
    f.draw();
    assert_eq!(f.active_path(), Some(f.path("A.md")));
    crate::files::overlay::clean(&f.path("Hub.md"));
}
