// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The conflict-marker warning in headless windows: shown in Preview and
//! Edit for a note with Git conflict markers, not for markers in a code
//! fence; *Resolve…* opens the compare and merge view (making the Source
//! view of a Preview-only tab) and *Apply* writes the merged text back as
//! one undoable edit; Dismiss; viewer mode.

use std::path::Path;
use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Bounds, Entity, TestAppContext, VisualTestContext, WindowBounds,
    WindowOptions, point, px, size,
};

use super::infobar::NoticeSource;
use super::{OpenTarget, ViewMode, Workspace};
use crate::chrome::WindowChrome;
use crate::compare::Pick;
use crate::editor::{EditorStatus, EditorView};
use crate::files::notice::{Notice, NoticeAction, Topic};
use crate::settings::Target;
use crate::{AppSettings, ThemeChoice};

const CONFLICTED: &str = "# Plan\n\
    <<<<<<< HEAD\n\
    ours\n\
    =======\n\
    theirs\n\
    >>>>>>> feature\n\
    end\n";

const FENCED: &str = "# Example\n\n```\n<<<<<<< HEAD\na\n=======\nb\n>>>>>>> x\n```\n";

struct Fixture {
    _dir: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

fn fixture(cx: &mut TestAppContext, text: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("Note.md");
    std::fs::write(&path, text).expect("write");
    let target = Some(OpenTarget::new(path));
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
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
        _dir: dir,
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
    /// Waits (in real time: files load on other threads; the Source view
    /// checks a moment after a change) for `done`.
    fn until(&mut self, what: &str, done: impl Fn(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.vcx
                .executor()
                .advance_clock(Duration::from_millis(400));
            draw(&mut self.vcx);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn notice(&mut self) -> Option<(Notice, NoticeSource)> {
        self.workspace
            .read_with(&self.vcx, |ws, cx| ws.active_notice(cx))
    }

    fn markers_shown(&mut self) -> bool {
        self.notice()
            .is_some_and(|(n, _)| n.topic == Topic::ConflictMarkers)
    }

    fn reader_loaded(&mut self) -> bool {
        self.workspace.read_with(&self.vcx, |ws, cx| {
            ws.active_tab()
                .reader()
                .is_some_and(|r| r.read(cx).path().is_some() && r.read(cx).model().is_some())
        })
    }

    fn editor(&self) -> Option<Entity<EditorView>> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.active_tab().editor().cloned())
    }

    fn editor_text(&self) -> Option<String> {
        let editor = self.editor()?;
        editor.read_with(&self.vcx, |e, _| {
            (*e.status() == EditorStatus::Ready)
                .then(|| e.editor().map(|e| e.text().to_string()))
                .flatten()
        })
    }

    fn press(&mut self, action: NoticeAction) {
        let (_, source) = self.notice().expect("a notice");
        self.workspace.update_in(&mut self.vcx, |ws, window, cx| {
            ws.run_notice_action(&action, source, window, cx);
        });
        draw(&mut self.vcx);
    }

    fn compare_shown(&self) -> bool {
        self.editor()
            .is_some_and(|e| e.read_with(&self.vcx, |e, _| e.compare_view().is_some()))
    }
}

#[gpui::test]
fn preview_warns_and_resolve_writes_the_merge_back_undoably(cx: &mut TestAppContext) {
    let mut f = fixture(cx, CONFLICTED);
    f.until("the Reading view", Fixture::reader_loaded);
    assert_eq!(
        f.workspace.read_with(&f.vcx, |ws, _| ws.mode()),
        ViewMode::Preview
    );
    assert!(f.markers_shown(), "{:?}", f.notice());
    let (notice, source) = f.notice().expect("notice");
    assert_eq!(source, NoticeSource::Markers);
    assert!(notice.offers(&NoticeAction::ResolveConflictMarkers));
    assert!(notice.buttons.iter().all(|b| b.disabled.is_none()));

    // Resolve…: a Source view is made for the Preview-only tab, and the
    // compare view shows once it has loaded.
    f.press(NoticeAction::ResolveConflictMarkers);
    f.until("the compare view", |f| f.compare_shown());
    let editor = f.editor().expect("a Source view");
    let view = editor
        .read_with(&f.vcx, |e, _| e.compare_view())
        .expect("compare view");
    view.read_with(&f.vcx, |v, _| {
        let c = v.comparison();
        assert_eq!(c.sides().left.as_ref(), "HEAD");
        assert_eq!(c.sides().right.as_ref(), "feature");
        assert_eq!(c.unresolved(), 1);
    });
    view.update(&mut f.vcx, |v, cx| {
        v.pick(0, Pick::Right, cx);
        v.apply(cx);
    });
    draw(&mut f.vcx);
    assert!(!f.compare_shown());
    assert_eq!(f.editor_text().as_deref(), Some("# Plan\ntheirs\nend\n"));
    // The warning goes once the text was checked again.
    f.until("the warning to go", |f| !f.markers_shown());

    // One undo step brings the markers back.
    editor.update(&mut f.vcx, |e, cx| e.undo_for_tests(cx));
    draw(&mut f.vcx);
    assert_eq!(f.editor_text().as_deref(), Some(CONFLICTED));
}

#[gpui::test]
fn edit_mode_warns_and_dismiss_hides_it(cx: &mut TestAppContext) {
    let mut f = fixture(cx, CONFLICTED);
    f.workspace
        .update(&mut f.vcx, |ws, cx| ws.set_mode(ViewMode::Edit, cx));
    draw(&mut f.vcx);
    f.until("the Source view", |f| f.editor_text().is_some());
    f.until("the warning", Fixture::markers_shown);
    let (_, source) = f.notice().expect("notice");
    f.workspace.update(&mut f.vcx, |ws, cx| {
        ws.dismiss_notice(source, cx);
    });
    draw(&mut f.vcx);
    assert!(!f.markers_shown());
    // Resolve from Edit: the compare view, and Cancel changes nothing.
    f.workspace
        .update(&mut f.vcx, |ws, cx| ws.resolve_conflict_markers(cx));
    f.until("the compare view", |f| f.compare_shown());
    let view = f
        .editor()
        .and_then(|e| e.read_with(&f.vcx, |e, _| e.compare_view()))
        .expect("compare view");
    view.update(&mut f.vcx, |v, cx| v.cancel(cx));
    draw(&mut f.vcx);
    assert_eq!(f.editor_text().as_deref(), Some(CONFLICTED));
}

#[gpui::test]
fn markers_in_a_code_fence_do_not_warn(cx: &mut TestAppContext) {
    let mut f = fixture(cx, FENCED);
    f.until("the Reading view", Fixture::reader_loaded);
    assert!(!f.markers_shown());
    f.workspace
        .update(&mut f.vcx, |ws, cx| ws.set_mode(ViewMode::Edit, cx));
    f.until("the Source view", |f| f.editor_text().is_some());
    f.vcx.executor().advance_clock(Duration::from_secs(1));
    draw(&mut f.vcx);
    assert!(!f.markers_shown());
}

#[gpui::test]
fn viewer_mode_warns_but_cannot_resolve(cx: &mut TestAppContext) {
    let mut f = fixture(cx, CONFLICTED);
    f.vcx.update(|_, cx| {
        AppSettings::set_value(
            cx,
            Target::AllVaults,
            "general.viewer_mode",
            &toml::Value::Boolean(true),
        )
        .expect("a valid setting");
    });
    f.until("the Reading view", Fixture::reader_loaded);
    let (notice, _) = f.notice().expect("notice");
    assert_eq!(notice.topic, Topic::ConflictMarkers);
    assert!(notice.buttons.iter().all(|b| b.disabled.is_some()));
    f.press(NoticeAction::ResolveConflictMarkers);
    assert!(!f.compare_shown());
    let file = f.workspace.read_with(&f.vcx, |ws, _| {
        ws.active_tab().path().map(Path::to_path_buf)
    });
    assert_eq!(
        std::fs::read_to_string(file.expect("path")).expect("read"),
        CONFLICTED
    );
}
