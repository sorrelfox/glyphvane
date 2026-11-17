// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Workspace trust in a window, in headless GPUI windows over a temporary
//! vault: the prompt shows when the window gets its vault, any answer
//! hides it, and restricted mode confirms links that leave the app.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use gpui::{
    App, AppContext as _, Bounds, Entity, TestAppContext, VisualTestContext, WindowBounds,
    WindowOptions, point, px, size,
};
use gv_model::LinkKind;
use gv_settings::{RemoteContent, WorkspaceTrust};

use super::{OpenTarget, Workspace};
use crate::chrome::WindowChrome;
use crate::files::notice::Topic;
use crate::keymap::actions::workspace as commands;
use crate::platform::Launcher;
use crate::reader::Disposition;
use crate::reader::link_info::LinkAt;
use crate::trust::{TrustChoice, vault_trust};
use crate::{AppSettings, ThemeChoice};

/// Records the URLs handed to the system.
#[derive(Clone, Default)]
struct Urls(Rc<RefCell<Vec<String>>>);

impl Launcher for Urls {
    fn reveal(&self, _: &Path, _: &mut App) {}

    fn open_with_default_app(&self, _: &Path, _: &mut App) {}

    fn open_url(&self, url: &str, _: &mut App) {
        self.0.borrow_mut().push(url.to_owned());
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    urls: Urls,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

fn fixture(cx: &mut TestAppContext) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join(".obsidian")).expect("marker");
    std::fs::write(
        dir.path().join("A.md"),
        "# A\n\n[web](https://example.com/)\n",
    )
    .expect("write");
    let urls = Urls::default();
    let target = Some(OpenTarget::new(dir.path().join("A.md")));
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        crate::platform::set_launcher(cx, urls.clone());
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
    Fixture {
        dir,
        urls,
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
    /// The window gets its vault (the sidebar asks for it).
    fn open_vault(&mut self) {
        self.vcx.dispatch_action(commands::ShowFiles);
        draw(&mut self.vcx);
    }

    fn prompt_shown(&self) -> bool {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.trust_prompt().is_some())
    }

    fn follow_web_link(&mut self) {
        let file = self.dir.path().join("A.md");
        self.vcx.update(|window, cx| {
            self.workspace.update(cx, |ws, cx| {
                let link = LinkAt {
                    file,
                    kind: LinkKind::Inline,
                    image: false,
                    destination: "https://example.com/".into(),
                    range: None,
                    construct: None,
                    profile: gv_vault::ProfileId::Obsidian,
                };
                ws.follow_link_at(ws.active, link, Disposition::InPlace, false, window, cx);
            });
        });
        draw(&mut self.vcx);
    }
}

#[gpui::test]
fn the_prompt_shows_with_the_vault_and_decide_later_hides_it(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    assert!(!f.prompt_shown(), "no vault yet, nothing to ask");
    f.open_vault();
    assert!(f.prompt_shown());
    assert!(f.vcx.debug_bounds("trust-prompt").is_some());
    f.vcx.update(|_, cx| {
        assert_eq!(vault_trust(cx), Some(WorkspaceTrust::Ask));
    });
    let prompt = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.trust_prompt())
        .expect("the prompt");
    prompt.update(&mut f.vcx, |p, cx| p.dismiss(cx));
    draw(&mut f.vcx);
    assert!(!f.prompt_shown());
    assert!(f.vcx.debug_bounds("trust-prompt").is_none());
    // Nothing is stored: the folder is still undecided.
    f.vcx.update(|_, cx| {
        assert_eq!(vault_trust(cx), Some(WorkspaceTrust::Ask));
    });
    // A web link goes straight to the browser.
    f.follow_web_link();
    assert_eq!(*f.urls.0.borrow(), ["https://example.com/"]);
}

#[gpui::test]
fn trust_hides_the_prompt_and_stores_the_choice(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.open_vault();
    let prompt = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.trust_prompt())
        .expect("the prompt");
    prompt.update(&mut f.vcx, |p, cx| {
        p.choose(TrustChoice::Trust(RemoteContent::Block), cx);
    });
    draw(&mut f.vcx);
    assert!(!f.prompt_shown());
    f.vcx.update(|_, cx| {
        assert_eq!(vault_trust(cx), Some(WorkspaceTrust::Trusted));
    });
}

#[gpui::test]
fn restricted_mode_confirms_links_that_leave_the_app(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.open_vault();
    let prompt = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.trust_prompt())
        .expect("the prompt");
    prompt.update(&mut f.vcx, |p, cx| p.choose(TrustChoice::Restricted, cx));
    draw(&mut f.vcx);
    assert!(!f.prompt_shown());
    f.follow_web_link();
    assert!(
        f.urls.0.borrow().is_empty(),
        "not handed off without asking"
    );
    let topic = f
        .workspace
        .read_with(&f.vcx, |ws, cx| ws.active_notice(cx).map(|(n, _)| n.topic));
    assert_eq!(topic, Some(Topic::Confirm));
}
