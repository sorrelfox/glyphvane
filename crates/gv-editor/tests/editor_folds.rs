// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Folding through the editor: fold commands on the foldable regions,
//! motions over folds, carets and edits that unfold, undo bringing folds
//! back, and the display layers (tab and wrap) and events following.

// The fold and selection commands take lists of ranges.
#![allow(clippy::single_range_in_vec_init)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use gv_editor::commands::SyntaxRef;
use gv_editor::display_map::{TabPoint, WrapPoint, WrapRow};
use gv_editor::{DeleteUnit, EditKind, Editor, EditorEvent, FoldKind, Motion, SelectionSpec};
use gv_text::Buffer;
use support::{editor, render};

const DOC: &str = "\
# One
intro

## Sub
sub text
more

# Two
two text
";

/// Gives the editor the foldable regions of its text.
fn refresh(editor: &mut Editor) {
    let text = editor.snapshot();
    let tree = gv_syntax::parse(&text);
    editor.update_foldable(SyntaxRef::new(&tree, &text));
}

fn folded(text: &str) -> Editor {
    let mut editor = Editor::new(Buffer::new(text));
    refresh(&mut editor);
    editor
}

fn shown(editor: &Editor) -> String {
    editor.display().text()
}

fn caret(editor: &Editor) -> usize {
    editor.selections().primary().head()
}

/// Display events by kind.
fn display_events(editor: &mut Editor) -> Rc<RefCell<Vec<&'static str>>> {
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    editor.observe(move |event| {
        let name = match event {
            EditorEvent::Edited(_) => "edited",
            EditorEvent::SelectionsChanged(_) => "selections",
            EditorEvent::DisplayChanged { .. } => "display",
            EditorEvent::WrapChanged { .. } => "wrap",
            _ => "other",
        };
        sink.borrow_mut().push(name);
    });
    events
}

// ----- commands --------------------------------------------------------------

#[test]
fn fold_and_unfold_a_heading() {
    let mut editor = folded(DOC);
    assert!(editor.is_foldable(0));
    assert!(editor.is_foldable(3));
    assert!(!editor.is_foldable(1));
    assert!(!editor.is_row_folded(0));

    assert!(editor.fold_at(3));
    assert_eq!(
        shown(&editor),
        "# One\nintro\n\n## Sub⋯\n\n# Two\ntwo text\n"
    );
    assert!(editor.is_row_folded(3));
    assert!(editor.is_line_folded(4));
    assert!(!editor.is_line_folded(6));
    assert_eq!(editor.folded_ranges(), [19..33]);

    assert!(editor.toggle_fold(3));
    assert_eq!(shown(&editor), DOC);
    assert!(editor.toggle_fold(0));
    assert_eq!(shown(&editor), "# One⋯\n\n# Two\ntwo text\n");
}

#[test]
fn fold_at_a_line_inside_a_region_folds_the_smallest_one() {
    let mut editor = folded(DOC);
    assert!(editor.fold_at(4));
    assert_eq!(
        shown(&editor),
        "# One\nintro\n\n## Sub⋯\n\n# Two\ntwo text\n"
    );
    // The next one out.
    assert!(editor.fold_at(1));
    assert_eq!(shown(&editor), "# One⋯\n\n# Two\ntwo text\n");
    assert!(!editor.fold_at(2));

    // At a header that is folded already, the region around it folds.
    let mut editor = folded(DOC);
    assert!(editor.fold_at(3));
    assert!(editor.fold_at(3));
    assert_eq!(shown(&editor), "# One⋯\n\n# Two\ntwo text\n");
}

#[test]
fn fold_all_and_by_heading_level() {
    let text = "# A\n## A1\nx\n## A2\ny\n# B\n- item\n  - child\n";
    let mut editor = folded(text);
    assert!(editor.fold_headings(2));
    assert_eq!(
        shown(&editor),
        "# A\n## A1⋯\n## A2⋯\n# B\n- item\n  - child\n"
    );
    assert!(editor.unfold_headings(2));
    assert_eq!(shown(&editor), text);
    assert!(editor.fold_headings(1));
    assert_eq!(shown(&editor), "# A⋯\n# B⋯\n");
    // Unfolding level 2 and deeper leaves the level-1 folds.
    assert!(editor.unfold_headings(2));
    assert_eq!(shown(&editor), "# A⋯\n# B⋯\n");
    assert_eq!(editor.folds().len(), 2);
    assert!(editor.unfold_all());
    assert_eq!(shown(&editor), text);

    assert!(editor.fold_all());
    assert_eq!(shown(&editor), "# A⋯\n# B⋯\n");
    // Every region is folded, nested ones too.
    assert_eq!(editor.folds().len(), 5);
    assert!(editor.unfold_at(5));
    assert_eq!(shown(&editor), "# A⋯\n# B\n- item⋯\n");
    let kinds: Vec<FoldKind> = editor.foldable().iter().map(|c| c.kind).collect();
    assert_eq!(
        kinds,
        [
            FoldKind::Heading { level: 1 },
            FoldKind::Heading { level: 2 },
            FoldKind::Heading { level: 2 },
            FoldKind::Heading { level: 1 },
            FoldKind::ListItem,
        ]
    );
}

#[test]
fn setext_heading_folds_after_its_underline() {
    let mut editor = folded("Title\n=====\nbody\n");
    assert!(editor.is_foldable(0));
    assert!(editor.toggle_fold(0));
    assert_eq!(shown(&editor), "Title\n=====⋯\n");
    assert!(editor.is_row_folded(0));
    assert!(editor.toggle_fold(0));
    assert_eq!(shown(&editor), "Title\n=====\nbody\n");
}

#[test]
fn folding_moves_carets_inside_to_the_fold_start() {
    let (mut editor, _) = editor("# A\nbo|dy\n{x}y\n");
    refresh(&mut editor);
    assert!(editor.fold_at(0));
    assert_eq!(render(&editor), "# A|\nbody\nxy\n");
    assert_eq!(shown(&editor), "# A⋯\n");
}

#[test]
fn folds_survive_edits_elsewhere_and_at_their_edges() {
    let mut editor = folded(DOC);
    editor.fold_at(3);
    let fold = editor.folded_ranges()[0].clone();
    // Typing at the fold's start (the heading's end) and after its end.
    editor.set_cursor(fold.start);
    editor.insert_at_selections("!");
    assert_eq!(
        shown(&editor),
        "# One\nintro\n\n## Sub!⋯\n\n# Two\ntwo text\n"
    );
    editor.set_cursor(fold.end + 1);
    editor.insert_at_selections("?");
    assert_eq!(
        shown(&editor),
        "# One\nintro\n\n## Sub!⋯?\n\n# Two\ntwo text\n"
    );
    editor.set_cursor(0);
    editor.insert_at_selections("x");
    assert_eq!(
        shown(&editor),
        "x# One\nintro\n\n## Sub!⋯?\n\n# Two\ntwo text\n"
    );
    assert_eq!(editor.folds().len(), 1);
    assert_eq!(
        &editor.text().to_string()[editor.folded_ranges()[0].clone()],
        "\nsub text\nmore"
    );
}

// ----- motions ------------------------------------------------------------------

#[test]
fn horizontal_motions_skip_folds() {
    let mut editor = folded(DOC);
    editor.fold_at(3);
    let fold = editor.folded_ranges()[0].clone();
    editor.set_cursor(fold.start);
    editor.move_selections(Motion::Right, false);
    assert_eq!(caret(&editor), fold.end);
    editor.move_selections(Motion::Left, false);
    assert_eq!(caret(&editor), fold.start);
    editor.move_selections(Motion::WordRight, false);
    assert!(caret(&editor) >= fold.end, "{}", caret(&editor));
    editor.set_cursor(fold.end);
    editor.move_selections(Motion::WordLeft, false);
    assert!(caret(&editor) <= fold.start);
    // Selecting across the fold keeps it.
    editor.set_cursor(fold.start);
    editor.move_selections(Motion::Right, true);
    assert_eq!(editor.selections().primary().range(), fold.clone());
    assert_eq!(editor.folds().len(), 1);
}

#[test]
fn vertical_motions_skip_folded_rows() {
    let mut editor = folded(DOC);
    editor.fold_at(3);
    editor.set_cursor(DOC.find("intro").unwrap() + 2);
    editor.move_selections(Motion::Down, false);
    assert_eq!(caret(&editor), DOC.find("\n\n## Sub").unwrap() + 1);
    editor.move_selections(Motion::Down, false);
    // Row "## Sub⋯": column 2 of it.
    assert_eq!(caret(&editor), DOC.find("## Sub").unwrap() + 2);
    editor.move_selections(Motion::Down, false);
    assert_eq!(caret(&editor), DOC.find("\n\n# Two").unwrap() + 1);
    editor.move_selections(Motion::Down, false);
    assert_eq!(caret(&editor), DOC.find("# Two").unwrap() + 2);
    editor.move_selections(Motion::Up, false);
    editor.move_selections(Motion::Up, false);
    assert_eq!(caret(&editor), DOC.find("## Sub").unwrap() + 2);
    assert_eq!(editor.folds().len(), 1);
}

#[test]
fn line_motions_work_on_the_display_line() {
    let text = "  ## Sub\nhidden\nlast line\nafter\n";
    let mut editor = folded(text);
    // The fold hides `\nhidden\nlast` (from the heading's end into the
    // middle of a line), so the row reads `  ## Sub⋯ line`.
    editor.fold_ranges([8..20]);
    assert_eq!(shown(&editor), "  ## Sub⋯ line\nafter\n");
    editor.set_cursor(3);
    editor.move_selections(Motion::LineEnd, false);
    assert_eq!(caret(&editor), 25);
    editor.move_selections(Motion::LineStart, false);
    assert_eq!(caret(&editor), 0);
    editor.set_cursor(22);
    editor.move_selections(Motion::SmartLineStart, false);
    assert_eq!(caret(&editor), 2);
    editor.move_selections(Motion::SmartLineStart, false);
    assert_eq!(caret(&editor), 0);

    // The same with soft wrap on (wide enough not to wrap).
    editor.set_wrap_width(Some(80.0));
    editor.set_cursor(3);
    editor.move_selections(Motion::LineEnd, false);
    assert_eq!(caret(&editor), 25);
    editor.move_selections(Motion::LineStart, false);
    assert_eq!(caret(&editor), 0);
    assert_eq!(editor.folds().len(), 1);
}

// ----- unfolding ------------------------------------------------------------------

#[test]
fn a_caret_placed_inside_a_fold_unfolds_it() {
    let mut editor = folded(DOC);
    editor.fold_at(3);
    editor.fold_at(7);
    let events = display_events(&mut editor);
    editor.set_cursor(DOC.find("sub text").unwrap());
    assert_eq!(editor.folds().len(), 1);
    assert_eq!(
        shown(&editor),
        "# One\nintro\n\n## Sub\nsub text\nmore\n\n# Two⋯\n"
    );
    assert_eq!(*events.borrow(), ["display", "selections"]);

    // A selection that covers a fold keeps it; one that ends inside it
    // unfolds it.
    editor.select_ranges([0..DOC.len()]);
    assert_eq!(editor.folds().len(), 1);
    editor.select_ranges([0..DOC.len() - 3]);
    assert_eq!(editor.folds().len(), 0);
}

#[test]
fn an_edit_inside_a_fold_unfolds_it_and_undo_brings_it_back() {
    let mut editor = folded(DOC);
    editor.fold_at(3);
    let fold = editor.folded_ranges()[0].clone();
    let inside = DOC.find("sub text").unwrap();
    editor
        .edit([(inside..inside + 3, "SUB")], EditKind::Command)
        .unwrap();
    assert!(editor.folds().is_empty());
    assert!(editor.text().to_string().contains("SUB text"));

    editor.undo();
    assert_eq!(editor.text().to_string(), DOC);
    assert_eq!(editor.folded_ranges(), std::slice::from_ref(&fold));
    editor.redo();
    assert!(editor.folds().is_empty());
    editor.undo();
    assert_eq!(editor.folded_ranges(), [fold]);
}

#[test]
fn deleting_a_selection_over_a_fold_and_undoing_it() {
    let mut editor = folded(DOC);
    editor.fold_at(3);
    let fold = editor.folded_ranges()[0].clone();
    let start = DOC.find("## Sub").unwrap();
    let end = DOC.find("# Two").unwrap();
    editor.select_ranges([start..end]);
    assert_eq!(editor.folds().len(), 1);
    editor.delete_backward(DeleteUnit::Grapheme);
    assert_eq!(shown(&editor), "# One\nintro\n\n# Two\ntwo text\n");
    assert!(editor.folds().is_empty());
    editor.undo();
    assert_eq!(editor.text().to_string(), DOC);
    assert_eq!(editor.folded_ranges(), [fold]);
    assert_eq!(editor.selections().primary().range(), start..end);
}

#[test]
fn typing_over_a_selection_with_a_fold_is_its_own_undo_step() {
    let mut editor = folded(DOC);
    editor.fold_at(3);
    let fold = editor.folded_ranges()[0].clone();
    let start = DOC.find("## Sub").unwrap();
    let end = DOC.find("# Two").unwrap();
    editor.select_ranges([start..end]);
    editor.insert_at_selections("x");
    editor.insert_at_selections("y");
    editor.undo();
    assert_eq!(editor.folds().len(), 0);
    editor.undo();
    assert_eq!(editor.text().to_string(), DOC);
    assert_eq!(editor.folded_ranges(), [fold]);
}

#[test]
fn backspace_and_delete_next_to_a_placeholder_unfold_instead() {
    let mut editor = folded(DOC);
    editor.fold_at(3);
    let fold = editor.folded_ranges()[0].clone();
    editor.set_cursor(fold.end);
    assert!(!editor.delete_backward(DeleteUnit::Grapheme));
    assert_eq!(editor.text().to_string(), DOC);
    assert!(editor.folds().is_empty());
    assert_eq!(caret(&editor), fold.end);

    editor.fold_at(3);
    editor.set_cursor(fold.start);
    assert!(!editor.delete_forward(DeleteUnit::Word));
    assert_eq!(editor.text().to_string(), DOC);
    assert!(editor.folds().is_empty());

    // Another caret still deletes.
    editor.fold_at(3);
    editor.set_selections(
        &[SelectionSpec::caret(1), SelectionSpec::caret(fold.end)],
        1,
    );
    assert!(editor.delete_backward(DeleteUnit::Grapheme));
    assert_eq!(editor.text().to_string(), &DOC[1..]);
    assert!(editor.folds().is_empty());
}

#[test]
fn undo_reveals_folds_its_selections_are_in() {
    let mut editor = folded(DOC);
    let inside = DOC.find("sub text").unwrap();
    editor.set_cursor(inside);
    editor.insert_at_selections("x");
    editor.break_undo_group();
    editor.fold_at(3);
    assert_eq!(editor.folds().len(), 1);
    // Undoing the typing edits inside the fold: it unfolds.
    editor.undo();
    assert_eq!(editor.text().to_string(), DOC);
    assert!(editor.folds().is_empty());
    assert_eq!(caret(&editor), inside);
}

// ----- display layers -------------------------------------------------------------

#[test]
fn soft_wrap_follows_folds() {
    let mut editor = folded(DOC);
    editor.set_wrap_width(Some(5.0));
    let events = display_events(&mut editor);
    editor.fold_at(0);
    assert_eq!(*events.borrow(), ["display", "wrap"]);
    // `# One⋯` is six columns: the placeholder wraps with `One`.
    let wrap = editor.wrap().unwrap();
    assert_eq!(wrap.tab_snapshot().text(), "# One⋯\n\n# Two\ntwo text\n");
    let placeholders = editor.fold_placeholders(0..10);
    assert_eq!(placeholders.len(), 1);
    assert_eq!(placeholders[0].buffer_range, editor.folded_ranges()[0]);
    assert_eq!(
        placeholders[0].tab_range,
        TabPoint::new(0, 5)..TabPoint::new(0, 8)
    );
    let wrap_range = placeholders[0].wrap_range.clone().unwrap();
    assert_eq!(wrap_range.start, WrapPoint::new(WrapRow(1), 3));
    assert!(editor.fold_placeholders(2..10).is_empty());

    editor.set_wrap_width(None);
    let placeholders = editor.fold_placeholders(0..1);
    assert_eq!(placeholders.len(), 1);
    assert_eq!(placeholders[0].wrap_range, None);
    assert!(editor.fold_placeholders(1..10).is_empty());
}

#[test]
fn crlf_text() {
    let text = "# A\r\nbody\r\n\r\n# B\r\nmore\r\n";
    let mut editor = folded(text);
    assert!(editor.fold_at(0));
    assert_eq!(shown(&editor), "# A⋯\n\n# B\nmore\n");
    editor.set_cursor(3);
    editor.move_selections(Motion::Right, false);
    assert_eq!(caret(&editor), 9);
    editor.move_selections(Motion::Right, false);
    assert_eq!(caret(&editor), 11);
    assert!(editor.toggle_fold(0));
    assert_eq!(shown(&editor), "# A\nbody\n\n# B\nmore\n");
}

#[test]
fn foldable_regions_follow_edits() {
    let mut editor = folded(DOC);
    editor.set_cursor(0);
    editor.newline();
    // The regions moved one line down without a new parse.
    assert!(editor.is_foldable(1));
    assert!(!editor.is_foldable(0));
    assert!(editor.fold_at(1));
    assert_eq!(shown(&editor), "\n# One⋯\n\n# Two\ntwo text\n");
}

// ----- properties -------------------------------------------------------------------

mod properties {
    use super::*;
    use gv_editor::display_map::{MonospaceMeasure, PLACEHOLDER, WrapMap};
    use proptest::prelude::*;
    use proptest::test_runner::{Config, TestCaseError};

    fn config() -> Config {
        let cases = std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|cases| cases.parse().ok())
            .unwrap_or(64);
        Config {
            cases,
            failure_persistence: None,
            ..Config::default()
        }
    }

    /// Markdown-ish lines, so there are regions to fold.
    fn document() -> impl Strategy<Value = String> {
        let line = prop::sample::select(vec![
            "# One",
            "## Two",
            "### Three",
            "text",
            "more text",
            "",
            "- item",
            "  - nested",
            "> quote",
            "```",
            "code",
            "    indented",
            "tab\there",
        ]);
        let ending = prop::sample::select(vec!["\n", "\n", "\n", "\r\n"]);
        prop::collection::vec((line, ending), 0..24).prop_map(|lines| {
            lines
                .into_iter()
                .map(|(line, ending)| format!("{line}{ending}"))
                .collect()
        })
    }

    #[derive(Clone, Debug)]
    enum Op {
        Refresh,
        FoldAt(usize),
        UnfoldAt(usize),
        Toggle(usize),
        FoldAll,
        FoldHeadings(u8),
        UnfoldAll,
        Cursor(usize),
        Select(usize, usize),
        Move(Motion, bool),
        Type(&'static str),
        Backspace,
        Delete,
        Command(usize, usize, &'static str),
        Undo,
        Redo,
        Wrap(Option<u8>),
    }

    fn op() -> impl Strategy<Value = Op> {
        let motion = prop::sample::select(vec![
            Motion::Left,
            Motion::Right,
            Motion::WordLeft,
            Motion::WordRight,
            Motion::LineStart,
            Motion::SmartLineStart,
            Motion::LineEnd,
            Motion::Up,
            Motion::Down,
            Motion::DownRows(3),
        ]);
        let typed = prop::sample::select(vec!["x", " ", "\n", "# ", "- "]);
        prop_oneof![
            2 => Just(Op::Refresh),
            3 => (0usize..30).prop_map(Op::FoldAt),
            1 => (0usize..30).prop_map(Op::UnfoldAt),
            2 => (0usize..30).prop_map(Op::Toggle),
            1 => Just(Op::FoldAll),
            1 => (1u8..4).prop_map(Op::FoldHeadings),
            1 => Just(Op::UnfoldAll),
            2 => any::<usize>().prop_map(Op::Cursor),
            1 => (any::<usize>(), any::<usize>()).prop_map(|(a, b)| Op::Select(a, b)),
            4 => (motion, any::<bool>()).prop_map(|(m, e)| Op::Move(m, e)),
            2 => typed.prop_map(Op::Type),
            1 => Just(Op::Backspace),
            1 => Just(Op::Delete),
            1 => (any::<usize>(), 0usize..8, prop::sample::select(vec!["", "y", "z\n"]))
                .prop_map(|(at, len, text)| Op::Command(at, len, text)),
            2 => Just(Op::Undo),
            1 => Just(Op::Redo),
            1 => prop::option::of(4u8..30).prop_map(Op::Wrap),
        ]
    }

    fn clip(editor: &Editor, offset: usize) -> usize {
        let text = editor.text();
        let mut offset = offset % (text.len() + 1);
        while !text.is_char_boundary(offset) || text.is_inside_crlf(offset) {
            offset -= 1;
        }
        offset
    }

    fn run(editor: &mut Editor, op: &Op) {
        match *op {
            Op::Refresh => refresh(editor),
            Op::FoldAt(line) => {
                editor.fold_at(line);
            }
            Op::UnfoldAt(line) => {
                editor.unfold_at(line);
            }
            Op::Toggle(line) => {
                editor.toggle_fold(line);
            }
            Op::FoldAll => {
                editor.fold_all();
            }
            Op::FoldHeadings(level) => {
                editor.fold_headings(level);
            }
            Op::UnfoldAll => {
                editor.unfold_all();
            }
            Op::Cursor(at) => {
                let at = clip(editor, at);
                editor.set_cursor(at);
            }
            Op::Select(a, b) => {
                let (a, b) = (clip(editor, a), clip(editor, b));
                editor.set_selections(&[SelectionSpec::new(a, b)], 0);
            }
            Op::Move(motion, extend) => editor.move_selections(motion, extend),
            Op::Type(text) => {
                editor.insert_at_selections(text);
            }
            Op::Backspace => {
                editor.delete_backward(DeleteUnit::Grapheme);
            }
            Op::Delete => {
                editor.delete_forward(DeleteUnit::Grapheme);
            }
            Op::Command(at, len, text) => {
                let start = clip(editor, at);
                let end = clip(editor, start + len).max(start);
                editor
                    .edit([(start..end, text)], EditKind::Command)
                    .expect("a valid edit");
            }
            Op::Undo => {
                editor.undo();
            }
            Op::Redo => {
                editor.redo();
            }
            Op::Wrap(width) => editor.set_wrap_width(width.map(f32::from)),
        }
    }

    fn check(editor: &Editor) -> Result<(), TestCaseError> {
        let text = editor.text();
        prop_assert!(editor.selections().check(text).is_ok());
        let replace = editor.display().replace_snapshot();
        prop_assert_eq!(replace.buffer().revision(), text.revision());
        // No selection end is hidden.
        for selection in editor.selections().iter() {
            for offset in [selection.tail(), selection.head()] {
                prop_assert_eq!(replace.folded_range_at(offset), None, "{} hidden", offset);
            }
        }
        // The display shows the text with the folds replaced.
        let mut runs: Vec<std::ops::Range<usize>> = Vec::new();
        for fold in editor.folds() {
            prop_assert!(fold.range.end <= text.len());
            match runs.last_mut() {
                Some(last) if fold.range.start < last.end => {
                    last.end = last.end.max(fold.range.end)
                }
                _ => runs.push(fold.range.clone()),
            }
        }
        let mut expected = text.to_string();
        for run in runs.iter().rev() {
            expected.replace_range(run.clone(), PLACEHOLDER);
        }
        let expected = expected.replace("\r\n", "\n").replace('\r', "\n");
        prop_assert_eq!(replace.text(), expected);
        if let Some(wrap) = editor.wrap() {
            let rebuilt = WrapMap::new(
                editor.display().clone(),
                Box::new(MonospaceMeasure::new(1.0)),
                editor.wrap_width(),
            )
            .1;
            prop_assert_eq!(wrap.text(), rebuilt.text());
        }
        for candidate in editor.foldable() {
            prop_assert!(candidate.range.start < candidate.range.end);
            prop_assert!(candidate.range.end <= text.len());
        }
        Ok(())
    }

    proptest! {
        #![proptest_config(config())]

        #[test]
        fn folding_keeps_the_editor_consistent(
            document in document(),
            ops in prop::collection::vec(op(), 1..40),
        ) {
            let mut editor = Editor::new(Buffer::new(&document));
            refresh(&mut editor);
            for op in &ops {
                run(&mut editor, op);
                check(&editor)?;
            }
            // Undo brings back the first text.
            while editor.history().can_undo() {
                editor.undo();
                check(&editor)?;
            }
            prop_assert_eq!(editor.text().to_string(), document);
        }

        #[test]
        fn undo_brings_back_the_folds_an_edit_removed(
            document in document(),
            level in 1u8..4,
            at in any::<usize>(),
            len in 0usize..40,
            inserted in prop::sample::select(vec!["", "x", "a\nb", "\r\n"]),
        ) {
            let mut editor = Editor::new(Buffer::new(&document));
            refresh(&mut editor);
            if level == 1 {
                editor.fold_all();
            } else {
                editor.fold_headings(level);
            }
            let folds = editor.folded_ranges();
            let start = clip(&editor, at);
            let end = clip(&editor, start + len).max(start);
            editor.edit([(start..end, inserted)], EditKind::Command).unwrap();
            check(&editor)?;
            editor.undo();
            check(&editor)?;
            prop_assert_eq!(editor.text().to_string(), document);
            prop_assert_eq!(editor.folded_ranges(), folds);
        }
    }
}
