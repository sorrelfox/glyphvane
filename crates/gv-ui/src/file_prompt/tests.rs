// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The file prompt in a headless window: the name field's selection,
//! validation as typed, Enter and Esc, and a prompt without a field.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement, Render,
    Styled as _, TestAppContext, VisualTestContext, Window, WindowOptions, div,
};

use super::{FilePrompt, PromptField, PromptRequest, PromptRow, RowStyle, prompt_of};
use crate::{AppSettings, ThemeChoice};

/// A view that only holds the keyboard.
struct Host(FocusHandle);

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
        div().track_focus(&self.0).size_full()
    }
}

fn window(cx: &mut TestAppContext) -> (Entity<Host>, VisualTestContext) {
    let (window, host) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        gpui::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| {
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                Host(focus)
            })
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    (host, vcx)
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..2 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn prompt(vcx: &mut VisualTestContext) -> Entity<FilePrompt> {
    vcx.update(|window, cx| prompt_of(window, cx))
        .expect("prompt")
}

type Seen = Rc<RefCell<Vec<String>>>;

fn request(field: Option<PromptField>, seen: &Seen) -> PromptRequest {
    let seen = seen.clone();
    PromptRequest {
        title: "Rename “Note.md”".into(),
        message: None,
        field,
        rows: vec![PromptRow::new("a.md (1)", RowStyle::Heading)],
        action: "rename".into(),
        validate: Some(Rc::new(|text: &str| {
            text.contains('|').then(|| "No bars".into())
        })),
        on_confirm: Rc::new(move |text, _, _| seen.borrow_mut().push(text)),
    }
}

#[gpui::test]
fn the_name_field_selects_the_stem_and_checks_as_typed(cx: &mut TestAppContext) {
    let (host, mut vcx) = window(cx);
    let seen = Seen::default();
    let prompt = prompt(&mut vcx);
    vcx.update(|window, cx| {
        let field = PromptField::for_name("Note.md", false);
        prompt.update(cx, |p, cx| p.open(request(Some(field), &seen), window, cx));
    });
    draw(&mut vcx);
    assert!(vcx.debug_bounds("file-prompt").is_some());
    assert!(vcx.debug_bounds("prompt-row:0").is_some());
    let selection = vcx.update(|_, cx| prompt.read(cx).selection(cx));
    assert_eq!(selection, 0..4);

    vcx.update(|window, cx| prompt.update(cx, |p, cx| p.set_text("a|b.md", window, cx)));
    draw(&mut vcx);
    assert_eq!(
        prompt.read_with(&vcx, |p, _| p.problem().map(str::to_owned)),
        Some("No bars".to_owned())
    );
    assert!(vcx.debug_bounds("prompt-problem").is_some());
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert!(seen.borrow().is_empty());
    assert!(prompt.read_with(&vcx, |p, _| p.is_open()));

    vcx.update(|window, cx| prompt.update(cx, |p, cx| p.set_text("Other.md", window, cx)));
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert_eq!(*seen.borrow(), ["Other.md"]);
    assert!(!prompt.read_with(&vcx, |p, _| p.is_open()));
    // The keyboard goes back.
    let focused = vcx.update(|window, cx| host.read(cx).0.is_focused(window));
    assert!(focused);
}

#[gpui::test]
fn escape_cancels_and_a_prompt_without_field_confirms_with_enter(cx: &mut TestAppContext) {
    let (_, mut vcx) = window(cx);
    let seen = Seen::default();
    let prompt = prompt(&mut vcx);
    vcx.update(|window, cx| {
        let field = PromptField::for_name("Folder.v2", true);
        prompt.update(cx, |p, cx| p.open(request(Some(field), &seen), window, cx));
    });
    draw(&mut vcx);
    assert_eq!(vcx.update(|_, cx| prompt.read(cx).selection(cx)), 0..9);
    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert!(!prompt.read_with(&vcx, |p, _| p.is_open()));
    assert!(seen.borrow().is_empty());

    vcx.update(|window, cx| {
        prompt.update(cx, |p, cx| p.open(request(None, &seen), window, cx));
    });
    draw(&mut vcx);
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert_eq!(*seen.borrow(), [""]);
}
