// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Backspace at list markers and container prefixes, and smart pairs, on
//! marked text (`|` a caret, `{`…`}` a selection).

mod support;

use std::time::Duration;

use gv_editor::commands::{CommandSettings, Flavor, MarkdownCommands, Outcome, SyntaxRef};
use gv_editor::{DeleteUnit, Editor, UndoOutcome};
use gv_syntax::SyntaxTree;

const GF: Flavor = Flavor::GfmFile;
const CM: Flavor = Flavor::CommonMark;
const OB: Flavor = Flavor::Obsidian;

fn commands(flavor: Flavor) -> MarkdownCommands {
    MarkdownCommands::new(flavor, CommandSettings::default())
}

/// Backspace as the Source view does it: the Markdown rule, else a plain
/// delete.
fn backspace(commands: MarkdownCommands, editor: &mut Editor) -> Option<Outcome> {
    let text = editor.snapshot();
    let tree = SyntaxTree::parse_with(&text, commands.flavor.parse_options());
    let outcome = commands.backspace(editor, Some(SyntaxRef::new(&tree, &text)));
    if outcome.is_none() {
        editor.delete_backward(DeleteUnit::Grapheme);
    }
    outcome
}

/// Types `text` a character at a time as the Source view does.
fn type_str(commands: MarkdownCommands, editor: &mut Editor, text: &str) {
    let mut buf = [0u8; 4];
    for c in text.chars() {
        let c = c.encode_utf8(&mut buf);
        let snapshot = editor.snapshot();
        let tree = SyntaxTree::parse_with(&snapshot, commands.flavor.parse_options());
        let syntax = Some(SyntaxRef::new(&tree, &snapshot));
        if !commands.type_text(editor, syntax, c) {
            editor.insert_at_selections(c);
        }
    }
}

fn check_backspace(cases: &[(Flavor, &str, &str)]) {
    let mut failures = Vec::new();
    for &(flavor, input, expected) in cases {
        let (mut editor, _) = support::editor(input);
        let outcome = backspace(commands(flavor), &mut editor);
        let got = support::render(&editor);
        if got != expected {
            failures.push(format!("{input:?}\n   got {got:?}\n  want {expected:?}"));
            continue;
        }
        if outcome == Some(Outcome::Edited) {
            // One undo step restores the input.
            if editor.history().undo_len() != 1 || editor.undo() != UndoOutcome::Undone {
                failures.push(format!("{input:?}: not one undo step"));
                continue;
            }
            let restored = support::render(&editor);
            if restored != input {
                failures.push(format!("{input:?}: undo gave {restored:?}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn backspace_removes_list_markers() {
    check_backspace(&[
        (GF, "- |foo", "|foo"),
        (GF, "- |", "|"),
        (GF, "* |foo", "|foo"),
        (GF, "-   |foo", "|foo"),
        (GF, "1. |foo", "|foo"),
        (GF, "1) |foo", "|foo"),
        (GF, "- a\n- |b", "- a\n|b"),
        (GF, "- a\n  - |b", "- a\n  |b"),
        (GF, "> - |a", "> |a"),
        // Not right after the marker: a plain delete.
        (GF, "- f|oo", "- |oo"),
        (GF, "-| foo", "| foo"),
        (GF, "|- foo", "|- foo"),
    ]);
}

#[test]
fn backspace_renumbers_the_rest_of_an_ordered_list() {
    check_backspace(&[
        (GF, "1. a\n2. |b\n3. c\n4. d", "1. a\n|b\n2. c\n3. d"),
        (GF, "1. |a\n2. b\n3. c", "|a\n1. b\n2. c"),
        (GF, "9. a\n10. |b\n11. c", "9. a\n|b\n10. c"),
        // A list numbered all alike stays so.
        (GF, "1. a\n1. |b\n1. c", "1. a\n|b\n1. c"),
    ]);
}

#[test]
fn backspace_removes_task_boxes_first() {
    check_backspace(&[
        (GF, "- [ ] |foo", "- |foo"),
        (GF, "- [x] |foo", "- |foo"),
        (GF, "- [ ] |", "- |"),
        (GF, "1. [ ] |a", "1. |a"),
        // Then the marker.
        (GF, "- |[ ] foo", "|[ ] foo"),
    ]);
    // Twice: box, then marker.
    let (mut editor, _) = support::editor("- [ ] |foo");
    backspace(commands(GF), &mut editor);
    backspace(commands(GF), &mut editor);
    assert_eq!(support::render(&editor), "|foo");
    // CommonMark has no task boxes: `[ ]` is text.
    check_backspace(&[(CM, "- [ ] |foo", "- [ ]|foo")]);
}

#[test]
fn backspace_outdents_continuation_lines_and_removes_quotes() {
    check_backspace(&[
        (GF, "- a\n\n  |b", "- a\n\n|b"),
        (GF, "- a\n  - b\n\n    |c", "- a\n  - b\n\n  |c"),
        (GF, "> |a", "|a"),
        (GF, ">|a", "|a"),
        (GF, "  > |a", "  |a"),
        (GF, "> > |a", "> |a"),
        (GF, "> a\n> |b", "> a\n|b"),
        // Code keeps its prefixes: a plain delete.
        (GF, "- a\n\n      |code", "- a\n\n     |code"),
        (GF, "> ```\n> |x\n> ```", "> ```\n>|x\n> ```"),
    ]);
    // The marker, then the outdent, of a nested item.
    let (mut editor, _) = support::editor("- a\n\n  - |b");
    backspace(commands(GF), &mut editor);
    assert_eq!(support::render(&editor), "- a\n\n  |b");
    backspace(commands(GF), &mut editor);
    assert_eq!(support::render(&editor), "- a\n\n|b");
}

#[test]
fn backspace_with_several_carets_is_one_step() {
    let (mut editor, _) = support::editor("- |a\n- b|\n> |c");
    assert_eq!(backspace(commands(GF), &mut editor), Some(Outcome::Edited));
    assert_eq!(support::render(&editor), "|a\n- |\n|c");
    assert_eq!(editor.history().undo_len(), 1);
    editor.undo();
    assert_eq!(support::render(&editor), "- |a\n- b|\n> |c");
    // No caret at a marker: the plain delete runs.
    let (mut editor, _) = support::editor("- a|\n- b|");
    assert_eq!(backspace(commands(GF), &mut editor), None);
    assert_eq!(support::render(&editor), "- |\n- |");
}

#[test]
fn backspace_after_typing_is_its_own_step() {
    let (mut editor, clock) = support::editor("|");
    support::type_chars(&mut editor, &clock, "- a", Duration::from_millis(10));
    editor.move_selections(gv_editor::Motion::LineStart, false);
    editor.move_selections(gv_editor::Motion::Right, false);
    editor.move_selections(gv_editor::Motion::Right, false);
    backspace(commands(GF), &mut editor);
    assert_eq!(support::render(&editor), "|a");
    editor.undo();
    assert_eq!(support::render(&editor), "- |a");
}

fn check_typing(cases: &[(Flavor, &str, &str, &str)]) {
    let mut failures = Vec::new();
    for &(flavor, input, typed, expected) in cases {
        let (mut editor, _) = support::editor(input);
        type_str(commands(flavor), &mut editor, typed);
        let got = support::render(&editor);
        if got != expected {
            failures.push(format!(
                "{input:?} + {typed:?}\n   got {got:?}\n  want {expected:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn brackets_and_quotes_pair() {
    check_typing(&[
        (GF, "|", "(", "(|)"),
        (GF, "|", "[", "[|]"),
        (GF, "|", "{", "{|}"),
        (GF, "|", "\"", "\"|\""),
        (GF, "|", "`", "`|`"),
        (GF, "a |", "(", "a (|)"),
        (GF, "foo|", "(", "foo(|)"),
        (GF, "a | b", "[", "a [|] b"),
        (GF, "(|)", "[", "([|])"),
        (OB, "|", "[[", "[[|]]"),
        (GF, "[text]|", "(", "[text](|)"),
    ]);
}

#[test]
fn pairs_are_not_made_where_they_get_in_the_way() {
    check_typing(&[
        // Before a word.
        (GF, "|word", "(", "(|word"),
        (GF, "|word", "\"", "\"|word"),
        // After a word: quotes and backticks.
        (GF, "it|", "\"", "it\"|"),
        (GF, "x|", "`", "x`|"),
        // After a backslash.
        (GF, "\\|", "(", "\\(|"),
        // In code.
        (GF, "```\n|\n```", "(", "```\n(|\n```"),
        (GF, "    a|", "(", "    a(|"),
        (GF, "a `b|c` d", "(", "a `b(|c` d"),
        (GF, "a `b | c` d", "\"", "a `b \"| c` d"),
        (GF, "<div>\na |\n</div>", "\"", "<div>\na \"|\n</div>"),
        // Emphasis delimiters never pair at a caret.
        (GF, "|", "*", "*|"),
        (GF, "|", "_", "_|"),
        (GF, "|", "- ", "- |"),
    ]);
}

#[test]
fn closers_are_typed_over() {
    check_typing(&[
        (GF, "|", "(a)", "(a)|"),
        (GF, "|", "\"a\"", "\"a\"|"),
        (GF, "|", "[[a]]", "[[a]]|"),
        (GF, "|", "`a`", "`a`|"),
        // A fence: the third backtick does not pair.
        (GF, "|", "```", "```|"),
        // A closer that was not inserted by a pair is typed.
        (GF, "|)", ")", ")|)"),
    ]);
    // Moving the caret forgets the pair.
    let (mut editor, _) = support::editor("|");
    type_str(commands(GF), &mut editor, "(");
    editor.move_selections(gv_editor::Motion::Left, false);
    editor.move_selections(gv_editor::Motion::Right, false);
    type_str(commands(GF), &mut editor, ")");
    assert_eq!(support::render(&editor), "()|)");
}

#[test]
fn selections_are_wrapped() {
    check_typing(&[
        (GF, "a {b} c", "*", "a *{b}* c"),
        (GF, "a {b} c", "**", "a **{b}** c"),
        (GF, "a }b{ c", "_", "a _}b{_ c"),
        (GF, "a {b} c", "(", "a ({b}) c"),
        (GF, "a {b} c", "`", "a `{b}` c"),
        (GF, "a {b} c", "~", "a ~{b}~ c"),
        (OB, "a {b} c", "=", "a ={b}= c"),
        // No highlight in GFM: `=` replaces the selection.
        (GF, "a {b} c", "=", "a =| c"),
        // Several lines: replaced.
        (GF, "{a\nb}", "*", "*|"),
        // In code: replaced.
        (GF, "```\n{a}\n```", "*", "```\n*|\n```"),
    ]);
}

#[test]
fn backspace_deletes_an_empty_pair() {
    let (mut editor, _) = support::editor("a |");
    type_str(commands(GF), &mut editor, "(");
    editor.delete_backward(DeleteUnit::Grapheme);
    assert_eq!(support::render(&editor), "a |");
    // A pair typed by hand stays: only the opener goes.
    let (mut editor, _) = support::editor("(|)");
    editor.delete_backward(DeleteUnit::Grapheme);
    assert_eq!(support::render(&editor), "|)");
}

#[test]
fn pairs_work_with_several_carets() {
    check_typing(&[
        (GF, "a |\nb |", "(x)", "a (x)|\nb (x)|"),
        // One caret pairs, the other is before a word.
        (GF, "|\n|w", "(", "(|)\n(|w"),
        (GF, "{a} {b}", "*", "*{a}* *{b}*"),
    ]);
    let (mut editor, _) = support::editor("|\n|");
    type_str(commands(GF), &mut editor, "[");
    editor.delete_backward(DeleteUnit::Grapheme);
    assert_eq!(support::render(&editor), "|\n|");
}

#[test]
fn a_pair_is_part_of_the_typing_step() {
    let (mut editor, _) = support::editor("|");
    type_str(commands(GF), &mut editor, "x(b)");
    assert_eq!(support::render(&editor), "x(b)|");
    assert_eq!(editor.history().undo_len(), 1);
    editor.undo();
    assert_eq!(support::render(&editor), "|");
    // A wrap is undone in one step.
    let (mut editor, _) = support::editor("a {b} c");
    type_str(commands(GF), &mut editor, "*");
    editor.undo();
    assert_eq!(support::render(&editor), "a {b} c");
}

#[test]
fn no_pairs_while_composing_or_when_off() {
    let (mut editor, _) = support::editor("|");
    editor.replace_and_mark_text_in_range_utf16(None, "(", None);
    assert!(editor.is_composing());
    assert!(!commands(GF).type_text(&mut editor, None, "("));
    editor.replace_text_in_range_utf16(None, "(");
    assert_eq!(support::render(&editor), "(|");
    let off = MarkdownCommands::new(
        GF,
        CommandSettings {
            smart_pairs: false,
            ..CommandSettings::default()
        },
    );
    let (mut editor, _) = support::editor("|");
    assert!(!off.type_text(&mut editor, None, "("));
    assert_eq!(support::render(&editor), "|");
    // Backspace at a marker does nothing special while composing.
    let (mut editor, _) = support::editor("- |");
    editor.replace_and_mark_text_in_range_utf16(None, "x", None);
    assert_eq!(commands(GF).backspace(&mut editor, None), None);
}
