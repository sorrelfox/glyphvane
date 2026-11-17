// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Workspace trust: when the prompt is due, what the choices store, and
//! the prompt in a headless window.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AppContext as _, Bounds, Entity, Modifiers, Pixels, Point, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};
use gv_settings::{LayerKind, RemoteContent, WorkspaceTrust};

use super::{
    TrustChoice, TrustPrompt, TrustPromptEvent, apply, confirm_every_handoff, needs_prompt,
    prompt_detail, snippets_allowed, vault_trust,
};
use crate::{AppSettings, ThemeChoice};

fn vault_with_snippets() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let snippets = dir.path().join(".obsidian").join("snippets");
    std::fs::create_dir_all(&snippets).expect("mkdir");
    std::fs::write(snippets.join("wide.css"), "").expect("write");
    std::fs::write(dir.path().join("Note.md"), "# Note\n").expect("write");
    dir
}

fn init(cx: &mut TestAppContext, settings: AppSettings) {
    cx.update(|cx| crate::init(cx, settings));
}

fn light() -> AppSettings {
    AppSettings::new(None, Some(ThemeChoice::Light))
}

#[test]
fn the_detail_names_what_the_folder_holds() {
    let plain = prompt_detail(None);
    assert!(plain.starts_with("Notes can load remote images and media"));
    assert!(!plain.contains("This folder has"));
    let summary = gv_vault::trust::TrustSummary {
        css_snippets: 2,
        plugins: 1,
        themes: 0,
        shared_settings: true,
    };
    assert!(prompt_detail(Some(&summary)).ends_with(
        "This folder has 2 CSS snippets, 1 Obsidian plugin (never run) and shared settings."
    ));
}

#[gpui::test]
fn the_prompt_is_due_until_a_choice_is_stored(cx: &mut TestAppContext) {
    init(cx, light());
    let dir = vault_with_snippets();
    cx.update(|cx| {
        // No vault: nothing to ask.
        assert!(!needs_prompt(cx));
        AppSettings::open_vault(cx, dir.path());
        assert_eq!(vault_trust(cx), Some(WorkspaceTrust::Ask));
        assert!(needs_prompt(cx));
        assert!(snippets_allowed(cx));

        apply(cx, TrustChoice::Trust(RemoteContent::Block)).expect("stored");
        assert!(!needs_prompt(cx));
        let s = cx
            .global::<AppSettings>()
            .settings()
            .privacy_and_security
            .clone();
        assert_eq!(s.workspace_trust, WorkspaceTrust::Trusted);
        assert_eq!(s.remote_content, RemoteContent::Block);
        assert!(!confirm_every_handoff(cx));
        // In the vault's own layer, not the global one.
        let global = cx
            .global::<AppSettings>()
            .effective_for(crate::settings::Target::AllVaults);
        assert_eq!(
            global.settings().privacy_and_security.workspace_trust,
            WorkspaceTrust::Ask
        );

        apply(cx, TrustChoice::Restricted).expect("stored");
        assert_eq!(vault_trust(cx), Some(WorkspaceTrust::Restricted));
        assert!(confirm_every_handoff(cx));
        assert!(!snippets_allowed(cx));
        assert!(!needs_prompt(cx));
    });
}

#[gpui::test]
fn safe_mode_skips_the_prompt(cx: &mut TestAppContext) {
    init(cx, light().with_safe_mode());
    let dir = vault_with_snippets();
    cx.update(|cx| {
        AppSettings::open_vault(cx, dir.path());
        assert!(!needs_prompt(cx), "safe mode");
    });
}

#[gpui::test]
fn a_trusted_global_default_skips_the_prompt(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("settings.toml");
    std::fs::write(
        &path,
        "version = 1\n[privacy_and_security]\nworkspace_trust = \"trusted\"\n",
    )
    .expect("write");
    let file = gv_settings::SettingsFile::load(path, LayerKind::Global).expect("settings");
    init(cx, AppSettings::new(Some(file), Some(ThemeChoice::Light)));
    let vault = vault_with_snippets();
    cx.update(|cx| {
        AppSettings::open_vault(cx, vault.path());
        assert!(!needs_prompt(cx));
    });
}

struct Prompt {
    _dir: tempfile::TempDir,
    prompt: Entity<TrustPrompt>,
    vcx: VisualTestContext,
    events: Rc<RefCell<Vec<TrustPromptEvent>>>,
}

fn prompt(cx: &mut TestAppContext) -> Prompt {
    init(cx, light());
    let dir = vault_with_snippets();
    let root = dir.path().to_path_buf();
    let (window, prompt) = cx.update(|cx| {
        AppSettings::open_vault(cx, &root);
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1400.0), px(200.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |_, cx| {
            cx.new(|cx| TrustPrompt::new(root.clone(), cx))
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = events.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&prompt, move |_, e: &TrustPromptEvent, _| {
            seen.borrow_mut().push(*e);
        })
        .detach();
    });
    let mut p = Prompt {
        _dir: dir,
        prompt,
        vcx,
        events,
    };
    p.draw();
    p
}

impl Prompt {
    fn draw(&mut self) {
        for _ in 0..2 {
            self.vcx.run_until_parked();
            self.vcx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    fn center(&mut self, selector: &'static str) -> Point<Pixels> {
        self.vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center()
    }

    fn click(&mut self, selector: &'static str) {
        let at = self.center(selector);
        self.vcx.simulate_click(at, Modifiers::none());
        self.draw();
    }
}

#[gpui::test]
fn the_prompt_scans_the_folder_and_stores_the_choice(cx: &mut TestAppContext) {
    let mut p = prompt(cx);
    let detail = p.prompt.read_with(&p.vcx, |t, _| t.detail());
    assert!(
        detail.ends_with("This folder has 1 CSS snippet."),
        "{detail}"
    );
    assert!(p.vcx.debug_bounds("trust-detail").is_some());
    // The mode in effect is preselected; Block is picked, then Trust.
    assert_eq!(
        p.prompt.read_with(&p.vcx, |t, _| t.mode()),
        RemoteContent::Ask
    );
    p.click("trust-mode-block");
    assert_eq!(
        p.prompt.read_with(&p.vcx, |t, _| t.mode()),
        RemoteContent::Block
    );
    p.click("trust-accept");
    assert_eq!(
        *p.events.borrow(),
        [TrustPromptEvent::Decided(TrustChoice::Trust(
            RemoteContent::Block
        ))]
    );
    p.vcx.update(|_, cx| {
        assert!(!needs_prompt(cx));
        assert_eq!(
            cx.global::<AppSettings>()
                .settings()
                .privacy_and_security
                .remote_content,
            RemoteContent::Block
        );
    });
}

#[gpui::test]
fn restricted_mode_and_decide_later(cx: &mut TestAppContext) {
    let mut p = prompt(cx);
    p.click("trust-dismiss");
    assert_eq!(*p.events.borrow(), [TrustPromptEvent::Dismissed]);
    p.vcx
        .update(|_, cx| assert!(needs_prompt(cx), "nothing stored"));
    p.click("trust-restricted");
    assert_eq!(
        p.events.borrow().last(),
        Some(&TrustPromptEvent::Decided(TrustChoice::Restricted))
    );
    p.vcx.update(|_, cx| {
        assert_eq!(vault_trust(cx), Some(WorkspaceTrust::Restricted));
    });
}
