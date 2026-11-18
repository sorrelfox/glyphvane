// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Plain-text mode (a file that is not Markdown), in headless windows: no
//! Markdown highlighting, folds, completions, links or Markdown commands;
//! Enter keeps the indentation, Tab and Shift+Tab indent; code files are
//! highlighted as their language and not spell checked.

use super::*;
use crate::editor::EditorLayout;
use crate::editor::highlight::Style;
use crate::editor::plain::Plain;
use crate::keymap::actions::view as view_commands;

/// A window holding a plain-text Source view of `text`, focused and drawn.
fn plain(cx: &mut TestAppContext, text: &str) -> (Entity<EditorView>, VisualTestContext) {
    let text = text.to_owned();
    let (window, view) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        gpui::open_window(options(), cx, |_, cx| {
            cx.new(|cx| {
                let mut v = EditorView::new(ParseOptions::default(), cx);
                v.set_plain(true, cx);
                v.set_text(&text, cx);
                v
            })
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    vcx.update(|window, cx| {
        let focus = view.read(cx).focus.clone();
        focus.focus(window, cx);
    });
    draw(&mut vcx);
    (view, vcx)
}

/// The style at the first `needle` on line `line`, as last highlighted.
fn style_at(
    view: &Entity<EditorView>,
    vcx: &mut VisualTestContext,
    line: usize,
    needle: &str,
) -> Style {
    view.read_with(vcx, |v, _| {
        let doc = v.doc.as_ref().expect("loaded");
        let text = doc.editor.text();
        let range = text.line_range(line).expect("line");
        let content = text.text_for_range(range).expect("text");
        let col = content
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} on line {line}"));
        doc.highlights
            .get(line)
            .unwrap_or_else(|| panic!("line {line} highlighted"))
            .style_at(col)
    })
}

fn caret_at(view: &Entity<EditorView>, vcx: &mut VisualTestContext, offset: usize) {
    view.update(vcx, |v, cx| v.set_cursor(offset, cx));
    draw(vcx);
}

const TEXT: &str =
    "# Not a heading\n\n- not a list\n    indented [link](a.md) *word*\n```\nx\n```\n";

#[gpui::test]
fn plain_text_has_no_markdown_roles_folds_links_or_completions(cx: &mut TestAppContext) {
    let (view, mut vcx) = plain(cx, TEXT);
    view.update(&mut vcx, |v, cx| v.set_layout(EditorLayout::Pane, cx));
    draw(&mut vcx);
    assert!(view.read_with(&vcx, |v, _| v.is_plain()));
    // Plain runs: no markers, no bold heading, no link, no code fill.
    assert_eq!(style_at(&view, &mut vcx, 0, "#"), Style::PLAIN);
    assert_eq!(style_at(&view, &mut vcx, 0, "heading"), Style::PLAIN);
    assert_eq!(style_at(&view, &mut vcx, 2, "-"), Style::PLAIN);
    assert_eq!(style_at(&view, &mut vcx, 3, "a.md"), Style::PLAIN);
    assert_eq!(style_at(&view, &mut vcx, 3, "*word*"), Style::PLAIN);
    assert_eq!(style_at(&view, &mut vcx, 4, "```"), Style::PLAIN);
    let probe = probe_of(&view, &mut vcx);
    assert!(probe.code.is_empty(), "{:?}", probe.code);
    // No folds: no foldable regions, no chevrons in the pane's gutter.
    let foldable = view.read_with(&vcx, |v, _| v.editor().expect("loaded").foldable());
    assert!(foldable.is_empty());
    assert!(probe.chevrons.is_empty());
    vcx.dispatch_action(view_commands::FoldAll);
    draw(&mut vcx);
    let folded = view.read_with(&vcx, |v, _| v.editor().expect("loaded").folded_ranges());
    assert!(folded.is_empty());
    // No links.
    let link = TEXT.find("a.md").unwrap();
    assert!(view.read_with(&vcx, |v, _| v.link_at_offset(link).is_none()));

    // Typing: no pairs, no completions.
    caret_at(&view, &mut vcx, TEXT.len());
    vcx.simulate_input("([[");
    draw(&mut vcx);
    assert!(text_of(&view, &mut vcx).ends_with("```\n([["));
    assert!(view.read_with(&vcx, |v, _| !v.autocomplete.is_open()));
}

#[gpui::test]
fn markdown_commands_do_nothing_in_plain_text(cx: &mut TestAppContext) {
    let (view, mut vcx) = plain(cx, TEXT);
    let word = TEXT.find("word").unwrap();
    view.update(&mut vcx, |v, cx| {
        let doc = v.doc.as_mut().expect("loaded");
        doc.editor.select_ranges(std::iter::once(word..word + 4));
        cx.notify();
    });
    draw(&mut vcx);
    vcx.dispatch_action(commands::Bold);
    vcx.dispatch_action(commands::Heading1);
    vcx.dispatch_action(commands::Quote);
    vcx.dispatch_action(commands::InsertLink);
    vcx.dispatch_action(commands::ToggleComment);
    vcx.dispatch_action(commands::ToggleTask);
    draw(&mut vcx);
    assert_eq!(text_of(&view, &mut vcx), TEXT);
    // Smart paste is off: a URL over a selection is pasted as text.
    vcx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("https://x.y".into())));
    vcx.dispatch_action(commands::Paste);
    draw(&mut vcx);
    let text = text_of(&view, &mut vcx);
    assert!(text.contains("*https://x.y*"), "{text}");
    // Line commands stay.
    vcx.dispatch_action(commands::DuplicateLine);
    draw(&mut vcx);
    assert_eq!(text_of(&view, &mut vcx).matches("indented").count(), 2);
}

#[gpui::test]
fn enter_keeps_the_indentation_and_tab_indents(cx: &mut TestAppContext) {
    let text = "fn main() {\n    let a = 1;\n}";
    let (view, mut vcx) = plain(cx, text);
    let end = text.find(";").unwrap() + 1;
    caret_at(&view, &mut vcx, end);
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    // A line ending and the line's indentation.
    assert_eq!(
        text_of(&view, &mut vcx),
        "fn main() {\n    let a = 1;\n    \n}"
    );
    vcx.simulate_input("b;");
    vcx.dispatch_action(commands::Indent);
    draw(&mut vcx);
    assert_eq!(
        text_of(&view, &mut vcx),
        "fn main() {\n    let a = 1;\n    b;\t\n}"
    );
    // Shift+Tab over two lines takes one level (four spaces) off each.
    view.update(&mut vcx, |v, cx| {
        let doc = v.doc.as_mut().expect("loaded");
        doc.editor.select_ranges(std::iter::once(12..28));
        cx.notify();
    });
    vcx.dispatch_action(commands::Outdent);
    draw(&mut vcx);
    assert_eq!(text_of(&view, &mut vcx), "fn main() {\nlet a = 1;\nb;\t\n}");
    // Tab over the two lines puts one tab in front of each; one undo.
    view.update(&mut vcx, |v, cx| {
        let doc = v.doc.as_mut().expect("loaded");
        doc.editor.select_ranges(std::iter::once(12..24));
        cx.notify();
    });
    vcx.dispatch_action(commands::Indent);
    draw(&mut vcx);
    assert_eq!(
        text_of(&view, &mut vcx),
        "fn main() {\n\tlet a = 1;\n\tb;\t\n}"
    );
    vcx.dispatch_action(commands::Undo);
    draw(&mut vcx);
    assert_eq!(text_of(&view, &mut vcx), "fn main() {\nlet a = 1;\nb;\t\n}");
}

#[gpui::test]
fn a_code_file_is_highlighted_as_its_language(cx: &mut TestAppContext) {
    let dir = temp(&[(
        "plain_mode.rs",
        "fn plain_mode_source() -> &'static str { \"s\" }\n# not markdown\n",
    )]);
    let path = dir.path().join("plain_mode.rs");
    let (window, view) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        gpui::open_window(options(), cx, |_, cx| {
            cx.new(|cx| {
                let mut v = EditorView::new(ParseOptions::default(), cx);
                v.set_plain(true, cx);
                v.open(path.clone(), None, cx);
                v
            })
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while view.read_with(&vcx, |v, _| v.status().clone()) != EditorStatus::Ready {
        assert!(std::time::Instant::now() < deadline, "the file loads");
        draw(&mut vcx);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    draw(&mut vcx);
    draw(&mut vcx);
    assert_eq!(style_at(&view, &mut vcx, 0, "fn").color, Token::SyntaxTag);
    assert_eq!(
        style_at(&view, &mut vcx, 0, "\"s\"").color,
        Token::SyntaxString
    );
    assert_ne!(style_at(&view, &mut vcx, 1, "#"), Style::MARKER);
    // Code is not spell checked.
    let mode = view.read_with(&vcx, |v, _| v.plain.clone()).expect("plain");
    assert!(mode.language.is_some());
    assert!(!mode.spell);
}

#[test]
fn prose_files_keep_spell_check() {
    use std::path::Path;
    let txt = Plain::for_path(Some(Path::new("/n/notes.txt")));
    assert_eq!((txt.language.as_deref(), txt.spell), (None, true));
    let license = Plain::for_path(Some(Path::new("/n/LICENSE")));
    assert!(license.spell);
    let json = Plain::for_path(Some(Path::new("/n/data.json")));
    assert_eq!(
        (json.language.as_deref(), json.spell),
        (Some("json"), false)
    );
    assert!(json.mono, "code is monospace");
    for prose in [txt, license, Plain::for_path(Some(Path::new("/n/a.rst")))] {
        assert!(!prose.mono, "prose keeps the editor font");
    }
    assert!(!Plain::for_path(None).mono);
}

/// Code keeps a monospace family when the editor font set is
/// proportional; prose uses the font set.
#[gpui::test]
fn code_is_shown_in_a_monospace_family(cx: &mut TestAppContext) {
    let (view, mut vcx) = plain(cx, "fn main() {}\n");
    let installed = vcx.update(|_, cx| cx.text_system().all_font_names());
    let Some(proportional) = [
        "Inter",
        "DejaVu Sans",
        "Noto Sans",
        "Liberation Sans",
        "Arial",
    ]
    .into_iter()
    .find(|name| installed.iter().any(|n| n == name)) else {
        return;
    };
    let family = |vcx: &mut VisualTestContext| {
        view.read_with(vcx, |v, _| v.fonts.regular().family.to_string())
    };
    for code in [false, true] {
        view.update(&mut vcx, |v, cx| {
            let mut metrics = v.metrics().clone();
            metrics.family = proportional.into();
            metrics.code = code;
            v.set_metrics(metrics, cx);
        });
        draw(&mut vcx);
        if code {
            assert_ne!(family(&mut vcx), proportional, "code: a monospace family");
        } else {
            assert_eq!(family(&mut vcx), proportional, "prose: the font set");
        }
    }
}
