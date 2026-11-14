// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The edit path end to end, without a GUI: an editor command produces one
//! transaction; its `EditSummary` reaches the observers, the first of which
//! reparses incrementally with `gv-syntax`, and the second (a stand-in for
//! the render model) reads the parser's `Changes` and the display edits.
//! After every step the incremental tree equals a full parse of the text.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gv_editor::{
    DeleteUnit, EditCause, Editor, EditorConfig, EditorEvent, ManualClock, Motion, UndoOutcome,
};
use gv_syntax::{BlockKind, Budget, Changes, Reparse, SyntaxTree};
use gv_text::Buffer;
use proptest::prelude::*;
use proptest::sample::Index;

/// What the parser observer keeps.
#[derive(Debug)]
struct Parsed {
    tree: SyntaxTree,
    /// The reports of every reparse, in order.
    changes: Vec<Changes>,
}

/// What the render-model observer saw: per edit, the cause, the reparsed
/// line window and the number of display edits.
#[derive(Debug, Default)]
struct Rendered {
    updates: Vec<(EditCause, std::ops::Range<usize>, usize)>,
    selection_updates: usize,
}

/// An editor with the parser and render observers attached.
fn wired(
    text: &str,
) -> (
    Editor,
    ManualClock,
    Rc<RefCell<Parsed>>,
    Rc<RefCell<Rendered>>,
) {
    let clock = ManualClock::new();
    let mut editor = Editor::with_config(
        Buffer::new(text),
        EditorConfig::default(),
        Box::new(clock.clone()),
    );
    let parsed = Rc::new(RefCell::new(Parsed {
        tree: SyntaxTree::parse(&editor.snapshot()),
        changes: Vec::new(),
    }));
    let rendered = Rc::new(RefCell::new(Rendered::default()));

    // 1. The parser: reparse with the edit's summary (main-thread budget;
    //    a partial reparse would continue in the background in the app, here
    //    it finishes at once).
    let sink = parsed.clone();
    editor.observe(move |event| {
        let EditorEvent::Edited(edit) = event else {
            return;
        };
        let mut parsed = sink.borrow_mut();
        let (tree, changes) = match parsed.tree.reparse(
            edit.text,
            edit.summary,
            Budget::from_now(Duration::from_millis(1)),
        ) {
            Reparse::Done { tree, changes } => (tree, changes),
            Reparse::Partial { mut task, .. } => {
                task.run(edit.text, Budget::Unlimited);
                task.finish()
            }
        };
        parsed.tree = tree;
        parsed.changes.push(changes);
    });

    // 2. The render model: runs after the parser and reads its report.
    let parser = parsed.clone();
    let sink = rendered.clone();
    editor.observe(move |event| match event {
        EditorEvent::Edited(edit) => {
            let parsed = parser.borrow();
            let changes = parsed.changes.last().cloned().unwrap_or_default();
            sink.borrow_mut().updates.push((
                edit.cause,
                changes.restart_line..changes.new_end_line,
                edit.display_edits.len(),
            ));
        }
        EditorEvent::SelectionsChanged(_) => sink.borrow_mut().selection_updates += 1,
        _ => {}
    });
    (editor, clock, parsed, rendered)
}

/// The incremental tree must equal a full parse of the current text.
fn check(editor: &Editor, parsed: &Rc<RefCell<Parsed>>) {
    let snapshot = editor.snapshot();
    let parsed = parsed.borrow();
    parsed.tree.validate(&snapshot).expect("valid tree");
    let full = SyntaxTree::parse(&snapshot);
    parsed.tree.same_structure(&full).unwrap_or_else(|error| {
        panic!("incremental != full on {:?}: {error}", snapshot.to_string())
    });
}

fn kinds(parsed: &Rc<RefCell<Parsed>>) -> Vec<BlockKind> {
    parsed
        .borrow()
        .tree
        .blocks()
        .iter()
        .filter(|block| block.depth == 0)
        .map(|block| block.kind)
        .collect()
}

#[test]
fn typing_a_document_keeps_the_tree_current() {
    let (mut editor, clock, parsed, rendered) = wired("# Notes\r\n\r\nSome *text*\r\n");
    editor.move_selections(Motion::DocumentEnd, false);
    for c in "\n- one\n- two\n\n```\ncode\n".chars() {
        clock.advance(Duration::from_millis(80));
        let mut buf = [0u8; 4];
        editor.insert_at_selections(c.encode_utf8(&mut buf));
        check(&editor, &parsed);
    }
    assert_eq!(
        kinds(&parsed),
        [
            BlockKind::AtxHeading { level: 1 },
            BlockKind::Text,
            BlockKind::BulletList { marker: b'-' },
            BlockKind::FencedCode,
        ]
    );
    // One parser update and one render update per keystroke.
    let typed = "\n- one\n- two\n\n```\ncode\n".len();
    assert_eq!(parsed.borrow().changes.len(), typed);
    assert_eq!(rendered.borrow().updates.len(), typed);

    // Undo the steps one by one: each is one transaction, one reparse.
    let mut undos = 0;
    while editor.undo() == UndoOutcome::Undone {
        undos += 1;
        check(&editor, &parsed);
    }
    assert_eq!(editor.text().to_string(), "# Notes\r\n\r\nSome *text*\r\n");
    assert_eq!(parsed.borrow().changes.len(), typed + undos);
    while editor.redo() {
        check(&editor, &parsed);
    }
    assert_eq!(kinds(&parsed).len(), 4);
    let rendered = rendered.borrow();
    assert!(rendered.selection_updates >= 1);
    assert!(
        rendered
            .updates
            .iter()
            .any(|(cause, _, _)| *cause == EditCause::Undo)
    );
}

#[test]
fn multi_cursor_edit_is_one_reparse() {
    let (mut editor, _, parsed, rendered) = wired("one\ntwo\nthree\n");
    editor.add_cursor(4);
    editor.add_cursor(8);
    editor.insert_at_selections("> ");
    check(&editor, &parsed);
    assert_eq!(parsed.borrow().changes.len(), 1);
    assert_eq!(kinds(&parsed), [BlockKind::BlockQuote]);
    // One reparse window covers the three edited lines.
    let (_, window, _) = rendered.borrow().updates[0].clone();
    assert!(window.start == 0 && window.end >= 3, "{window:?}");
    editor.delete_backward(DeleteUnit::Line);
    check(&editor, &parsed);
    assert_eq!(editor.text().to_string(), "one\ntwo\nthree\n");
}

#[test]
fn opening_a_fence_at_the_top() {
    let body: String = (0..2000).map(|i| format!("Paragraph {i}\n\n")).collect();
    let (mut editor, _, parsed, _) = wired(&body);
    editor.insert_at_selections("```\n");
    check(&editor, &parsed);
    let changes = parsed.borrow().changes.last().cloned().unwrap_or_default();
    // The fence swallows the rest: the reparse ran to the end.
    assert!(!changes.converged);
    editor.undo();
    check(&editor, &parsed);
}

#[test]
fn input_method_composition_reparses_each_step() {
    let (mut editor, _, parsed, _) = wired("# 見出し\n\n");
    editor.move_selections(Motion::DocumentEnd, false);
    editor.replace_and_mark_text_in_range_utf16(None, "k", None);
    check(&editor, &parsed);
    editor.replace_and_mark_text_in_range_utf16(None, "か", None);
    check(&editor, &parsed);
    editor.replace_text_in_range_utf16(None, "仮名");
    check(&editor, &parsed);
    assert_eq!(editor.text().to_string(), "# 見出し\n\n仮名");
    editor.undo();
    check(&editor, &parsed);
    assert_eq!(editor.text().to_string(), "# 見出し\n\n");
}

const FRAGMENTS: &[&str] = &[
    "a", " ", "\n", "\r\n", "# ", "- ", "> ", "```", "*", "[x]", "1. ", "    ", "\t", "|", "---",
];

#[derive(Clone, Debug)]
enum Step {
    Type(&'static str),
    Backspace,
    DeleteWord,
    Undo,
    Redo,
    Cursor(Index),
    AddCursor(Index),
    Move(bool),
}

fn arb_step() -> impl Strategy<Value = Step> {
    prop_oneof![
        5 => prop::sample::select(FRAGMENTS).prop_map(Step::Type),
        2 => Just(Step::Backspace),
        1 => Just(Step::DeleteWord),
        2 => Just(Step::Undo),
        1 => Just(Step::Redo),
        1 => any::<Index>().prop_map(Step::Cursor),
        1 => any::<Index>().prop_map(Step::AddCursor),
        1 => any::<bool>().prop_map(Step::Move),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|c| c.parse().ok()).unwrap_or(128),
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// Random editing through the editor keeps the incremental tree equal
    /// to a full parse after every transaction, undo and redo.
    #[test]
    fn random_editing_keeps_the_tree_exact(
        start in prop::collection::vec(prop::sample::select(FRAGMENTS), 0..12),
        steps in prop::collection::vec(arb_step(), 1..40),
    ) {
        let (mut editor, clock, parsed, _) = wired(&start.concat());
        for step in steps {
            clock.advance(Duration::from_millis(300));
            let len = editor.text().len();
            match step {
                Step::Type(text) => {
                    editor.insert_at_selections(text);
                }
                Step::Backspace => {
                    editor.delete_backward(DeleteUnit::Grapheme);
                }
                Step::DeleteWord => {
                    editor.delete_forward(DeleteUnit::Word);
                }
                Step::Undo => {
                    let _ = editor.undo();
                }
                Step::Redo => {
                    editor.redo();
                }
                Step::Cursor(index) => editor.set_cursor(index.index(len + 1)),
                Step::AddCursor(index) => {
                    editor.add_cursor(index.index(len + 1));
                }
                Step::Move(down) => {
                    let motion = if down { Motion::Down } else { Motion::Up };
                    editor.move_selections(motion, false);
                }
            }
            check(&editor, &parsed);
        }
    }
}
