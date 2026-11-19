// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Link checker section in headless GPUI windows over temporary
//! vaults: scopes, grouping, opening a finding, fixes and their undo,
//! re-checks after edits.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Bounds, Entity, Modifiers, TestAppContext, VisualTestContext, WindowBounds,
    WindowOptions, point, px, size,
};
use gv_vault::content::{CheckScope, FindingKind};
use gv_vault::files::journal::JournalStore;

use super::link_checker::{LinkChecker, RECHECK_DELAY, ScopeKind};
use super::{OpenTarget, Workspace};
use crate::chrome::WindowChrome;
use crate::keymap::actions::links as commands;
use crate::keymap::actions::workspace as workspace_commands;
use crate::{AppSettings, ThemeChoice, TogglePanel};

struct Fixture {
    dir: tempfile::TempDir,
    _journal: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

/// An Obsidian vault with `files`, a window showing `open`, the right
/// panel shown.
fn fixture(cx: &mut TestAppContext, files: &[(&str, &str)], open: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join(".obsidian")).expect("marker");
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
        // Tests have no file watcher: an autosave would hide edits from the
        // index, which sees them as unsaved buffers instead.
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
    vcx.dispatch_action(TogglePanel);
    draw(&mut vcx);
    Fixture {
        dir,
        _journal: journal,
        workspace,
        vcx,
    }
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

    fn checker(&self) -> Entity<LinkChecker> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.link_checker())
            .expect("the Link checker exists")
    }

    /// Announces the content index's snapshots, lets the re-check delay
    /// pass, then draws.
    fn poll(&mut self) {
        self.vcx.executor().advance_clock(RECHECK_DELAY);
        let vault = self
            .workspace
            .read_with(&self.vcx, |ws, _| ws.vault.clone());
        if let Some(vault) = vault {
            vault.update(&mut self.vcx, |v, cx| v.poll_content(cx));
        }
        draw(&mut self.vcx);
    }

    /// Waits (in real time: the indexes have their own threads) until `f`
    /// holds for the checker.
    fn until(&mut self, what: &str, f: impl Fn(&LinkChecker) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            self.poll();
            if self.checker().read_with(&self.vcx, |c, _| f(c)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let rows = self.rows();
        panic!("{what} did not happen in time; rows: {rows:?}");
    }

    fn rows(&self) -> Vec<(String, String, u32)> {
        self.checker().read_with(&self.vcx, |c, _| c.finding_rows())
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
        draw(&mut self.vcx);
    }

    fn active_path(&self) -> Option<PathBuf> {
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.active_tab().path().map(Path::to_path_buf)
        })
    }

    fn editor_text(&self, name: &str) -> Option<String> {
        let path = self.path(name);
        let editor = self.workspace.read_with(&self.vcx, |ws, _| {
            ws.tabs()
                .iter()
                .find(|t| t.path() == Some(path.as_path()))
                .and_then(|t| t.editor().cloned())
        })?;
        editor.read_with(&self.vcx, |e, _| e.editor().map(|e| e.text().to_string()))
    }

    fn undo(&mut self, name: &str) {
        let path = self.path(name);
        let editor = self
            .workspace
            .read_with(&self.vcx, |ws, _| {
                ws.tabs()
                    .iter()
                    .find(|t| t.path() == Some(path.as_path()))
                    .and_then(|t| t.editor().cloned())
            })
            .expect("a Source view");
        editor.update(&mut self.vcx, |e, cx| e.undo_for_tests(cx));
        draw(&mut self.vcx);
    }
}

fn kinds_of(c: &LinkChecker) -> Vec<String> {
    c.finding_rows().into_iter().map(|(k, ..)| k).collect()
}

const HUB: &str = "# Hub\n\n[[Missing]] [[Dup]] [[a]] [[A#Nope]] [[A#Intro]]\n";

fn vault_files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Hub.md", HUB),
        ("A.md", "## Intro\n\nSee [[Hub]].\n"),
        ("x/Dup.md", "one [[Gone]]\n"),
        ("y/Dup.md", "two\n"),
        ("y/sub/Deep.md", "[[Deeper]]\n"),
    ]
}

#[gpui::test]
fn scopes_group_findings_by_severity(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md");
    f.vcx.dispatch_action(commands::CheckNote);
    draw(&mut f.vcx);
    assert!(f.has("link-checker-head"));
    assert!(f.has("checker-scope-Note"));
    f.until("the note's findings", |c| c.report().is_some());
    // Errors first, then warnings, then information; in text order.
    assert_eq!(
        f.rows(),
        vec![
            ("Unresolved".into(), "Missing".into(), 3),
            ("MissingAnchor".into(), "A#Nope".into(), 3),
            ("Ambiguous".into(), "Dup".into(), 3),
            ("CaseFold".into(), "a".into(), 3),
        ]
    );
    let selectors = f.checker().read_with(&f.vcx, |c, _| c.row_selectors());
    assert_eq!(
        selectors,
        [
            "checker-severity-error",
            "checker-source-0",
            "checker-finding-0",
            "checker-finding-1",
            "checker-severity-warning",
            "checker-source-1",
            "checker-finding-2",
            "checker-severity-info",
            "checker-source-2",
            "checker-finding-3",
        ]
    );
    assert!(f.has("checker-summary"));
    assert!(f.has("checker-finding-0"));

    // The vault: every note.
    f.vcx.dispatch_action(commands::CheckVault);
    f.until("the vault's findings", |c| {
        c.checked_scope() == Some(CheckScope::Vault)
            && c.report().is_some_and(|r| r.findings.len() == 6)
    });
    let kinds = f.checker().read_with(&f.vcx, |c, _| kinds_of(c));
    assert_eq!(
        kinds,
        [
            "Unresolved",
            "MissingAnchor",
            "Unresolved",
            "Unresolved",
            "Ambiguous",
            "CaseFold"
        ]
    );

    // The folder of the active note (and its subfolders), from the
    // scope control.
    let deep = f.path("y/sub/Deep.md");
    f.workspace.update_in(&mut f.vcx, |ws, window, cx| {
        ws.open_tab_for_edit(OpenTarget::new(deep), None, window, cx);
    });
    draw(&mut f.vcx);
    f.click("checker-scope-Folder");
    f.until("the folder's findings", |c| {
        c.scope() == ScopeKind::Folder
            && c.checked_scope() == Some(CheckScope::Folder(vp("y/sub")))
            && c.report().is_some()
    });
    assert_eq!(f.rows(), vec![("Unresolved".into(), "Deeper".into(), 1)]);
    // Note scope follows the active note.
    f.click("checker-scope-Note");
    f.until("Deep's findings", |c| {
        c.checked_scope() == Some(CheckScope::Note(vp("y/sub/Deep.md"))) && c.report().is_some()
    });
    assert_eq!(f.rows().len(), 1);
}

fn vp(s: &str) -> gv_vault::VaultPath {
    gv_vault::VaultPath::new(s).expect("path")
}

#[gpui::test]
fn the_view_command_shows_the_section(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md");
    assert!(!f.has("link-checker-head"));
    f.vcx.dispatch_action(workspace_commands::ShowLinkChecker);
    draw(&mut f.vcx);
    assert!(f.has("link-checker-head"));
    f.until("findings", |c| {
        c.report().is_some_and(|r| r.findings.len() == 4)
    });
    // The head folds the section.
    f.click("link-checker-head");
    assert!(!f.has("checker-summary"));
    f.vcx.dispatch_action(workspace_commands::ShowLinkChecker);
    draw(&mut f.vcx);
    assert!(f.has("checker-summary"));
}

#[gpui::test]
fn a_finding_opens_its_note_at_the_link(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md");
    f.vcx.dispatch_action(commands::CheckVault);
    f.until("the vault's findings", |c| {
        c.report().is_some_and(|r| r.findings.len() == 6)
    });
    // Third error: x/Dup.md's [[Gone]].
    f.click("checker-finding-2");
    assert_eq!(f.active_path(), Some(f.path("x/Dup.md")));
    let can_go_back = f.workspace.read_with(&f.vcx, |ws, _| ws.can_go_back());
    assert!(can_go_back, "opening from the Link checker is history");
    // The vault scope stays.
    let scope = f.checker().read_with(&f.vcx, |c, _| c.checked_scope());
    assert_eq!(scope, Some(CheckScope::Vault));
}

#[gpui::test]
fn picking_a_candidate_rewrites_the_link_and_undo_restores_it(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md");
    f.vcx.dispatch_action(commands::CheckNote);
    f.until("findings", |c| {
        c.report().is_some_and(|r| r.findings.len() == 4)
    });
    // Selecting the ambiguous link lists a fix per candidate.
    f.click("checker-finding-2");
    assert!(f.has("checker-fix-2-0"));
    assert!(f.has("checker-fix-2-1"));
    f.click("checker-fix-2-1");
    assert_eq!(
        f.editor_text("Hub.md").as_deref(),
        Some("# Hub\n\n[[Missing]] [[y/Dup]] [[a]] [[A#Nope]] [[A#Intro]]\n")
    );
    // The index sees the buffer: the link is no longer ambiguous.
    f.until("the re-check", |c| {
        c.report()
            .is_some_and(|r| r.findings.iter().all(|x| x.kind != FindingKind::Ambiguous))
    });
    // One undo step takes it back, and the finding returns.
    f.undo("Hub.md");
    assert_eq!(f.editor_text("Hub.md").as_deref(), Some(HUB));
    f.until("the finding again", |c| {
        c.report()
            .is_some_and(|r| r.findings.iter().any(|x| x.kind == FindingKind::Ambiguous))
    });
    crate::files::overlay::clean(&f.path("Hub.md"));
}

#[gpui::test]
fn links_fix_fixes_the_case_of_the_selected_finding(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md");
    f.vcx.dispatch_action(commands::CheckNote);
    f.until("findings", |c| {
        c.report().is_some_and(|r| r.findings.len() == 4)
    });
    f.click("checker-finding-3");
    // The click opened Hub at the link; the command fixes the selection.
    f.vcx.dispatch_action(commands::Fix);
    draw(&mut f.vcx);
    assert_eq!(
        f.editor_text("Hub.md").as_deref(),
        Some("# Hub\n\n[[Missing]] [[Dup]] [[A]] [[A#Nope]] [[A#Intro]]\n")
    );
    f.until("the re-check", |c| {
        c.report().is_some_and(|r| r.findings.len() == 3)
    });
    f.undo("Hub.md");
    assert_eq!(f.editor_text("Hub.md").as_deref(), Some(HUB));
    crate::files::overlay::clean(&f.path("Hub.md"));
}

#[gpui::test]
fn create_note_creates_the_missing_note(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md");
    f.vcx.dispatch_action(commands::CheckNote);
    f.until("findings", |c| {
        c.report().is_some_and(|r| r.findings.len() == 4)
    });
    f.click("checker-finding-0");
    assert!(f.has("checker-fix-0-0"));
    f.vcx.dispatch_action(commands::CreateNote);
    draw(&mut f.vcx);
    let created = f.path("Missing.md");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !created.exists() && Instant::now() < deadline {
        f.poll();
    }
    assert!(created.exists());
    draw(&mut f.vcx);
    assert_eq!(f.active_path(), Some(created));
    // Hub's link resolves now; Hub is still the note being checked? No:
    // the new note is active, so the Note scope follows it.
    f.vcx.dispatch_action(commands::CheckVault);
    f.until("the vault re-checked", |c| {
        c.checked_scope() == Some(CheckScope::Vault)
            && c.report().is_some_and(|r| {
                !r.findings.iter().any(|x| &*x.raw == "Missing") && r.findings.len() == 5
            })
    });
}

#[gpui::test]
fn a_name_not_valid_everywhere_is_created_sanitised_and_the_link_rewritten(
    cx: &mut TestAppContext,
) {
    // 09 § Unresolved: a name some platform does not allow (`:`) gets the
    // create-note popover's offer: the sanitised note, the link rewritten.
    let hub = "# Hub\n\n[[Plan: A]]\n";
    let mut f = fixture(cx, &[("Hub.md", hub)], "Hub.md");
    f.vcx.dispatch_action(commands::CheckNote);
    f.until("the offer", |c| {
        c.report().is_some_and(|r| {
            r.findings.iter().any(|x| {
                x.kind == FindingKind::Unresolved
                    && x.fixes.iter().any(|fix| {
                        fix.label == "Create Plan A.md and rewrite link"
                            && fix.action.creates_note()
                    })
            })
        })
    });
    let i = f
        .vcx
        .read(|cx| {
            let c = f.workspace.read(cx).link_checker().expect("checker");
            c.read(cx)
                .report()?
                .findings
                .iter()
                .position(|x| x.kind == FindingKind::Unresolved)
        })
        .expect("the unresolved finding");
    assert_eq!(i, 0, "the unresolved finding comes first");
    f.click("checker-finding-0");
    f.vcx.dispatch_action(commands::CreateNote);
    draw(&mut f.vcx);
    let created = f.path("Plan A.md");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !created.exists() && Instant::now() < deadline {
        f.poll();
    }
    assert!(created.exists());
    draw(&mut f.vcx);
    assert_eq!(
        f.editor_text("Hub.md").as_deref(),
        Some("# Hub\n\n[[Plan A]]\n")
    );
    assert_eq!(f.active_path(), Some(created));
    crate::files::overlay::clean(&f.path("Hub.md"));
}

#[gpui::test]
fn edits_are_checked_again(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), "Hub.md");
    f.vcx.dispatch_action(commands::CheckVault);
    f.until("findings", |c| {
        c.report().is_some_and(|r| r.findings.len() == 6)
    });
    let a = f.path("A.md");
    crate::files::overlay::dirty(&a, &gv_text::TextSnapshot::new("## Intro\n\n[[Nowhere]]\n"));
    f.until("the edit", |c| {
        c.report()
            .is_some_and(|r| r.findings.iter().any(|x| &*x.raw == "Nowhere"))
    });
    crate::files::overlay::clean(&a);
    f.until("the edit undone", |c| {
        c.report().is_some_and(|r| r.findings.len() == 6)
    });
}

/// The vault scope on a generated 10,000-note vault: progress while it
/// runs, then the findings, drawn lazily.
#[gpui::test]
fn ten_thousand_notes(cx: &mut TestAppContext) {
    const NOTES: usize = 10_000;
    let mut files: Vec<(String, String)> = vec![("Hub.md".into(), "# Hub\n".into())];
    for i in 0..NOTES {
        files.push((
            format!("n/{:02}/Note {i}.md", i % 50),
            format!(
                "# Note {i}\n\n## Part\n\n[[Note {}]] [[Note {}#Part]] [[Missing {i}]] [[Hub#Nope]]\n",
                (i + 1) % NOTES,
                (i + 7) % NOTES
            ),
        ));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let mut f = fixture(cx, &refs, "Hub.md");
    let started = Instant::now();
    f.vcx.dispatch_action(commands::CheckVault);
    f.until("the vault's findings", |c| {
        c.checked_scope() == Some(CheckScope::Vault)
            && c.report().is_some_and(|r| r.notes == NOTES + 1)
    });
    let ready = started.elapsed();
    let (check, findings) = f.checker().read_with(&f.vcx, |c, _| {
        (c.elapsed(), c.report().map(|r| r.findings.len()))
    });
    assert_eq!(findings, Some(2 * NOTES));
    let mut frames = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        f.vcx.update(|window, cx| window.draw(cx).clear(cx));
        frames.push(t.elapsed());
    }
    frames.sort();
    let frame = frames[frames.len() / 2];
    assert!(f.has("checker-finding-0"));
    assert!(!f.has("checker-finding-19999"), "rows are drawn lazily");
    eprintln!(
        "link checker, 10k notes: check {check:?}; index + check ready {ready:?}; \
         frame (median of 5) {frame:?}"
    );
    assert!(frame < Duration::from_millis(500), "{frame:?}");
}
