// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Table tests of the line commands (move, duplicate, join, sort, change
//! case), toggle comment, insert callout, expand and shrink selection and
//! the Go helpers, as marked text (`|` a caret, `{`…`}` a selection). Every
//! edit is checked to be one undo step that restores the input bytes and
//! selections.

mod support;

use gv_editor::commands::{
    Availability, CommandSettings, Flavor, MarkdownCommand, MarkdownCommands, Outcome,
    SelectionHistory, SyntaxRef, line_target_offset, parse_line_target,
};
use gv_editor::{Editor, UndoOutcome};
use gv_syntax::SyntaxTree;

use MarkdownCommand::{
    ChangeCase, DuplicateLine, JoinLines, MoveLineDown, MoveLineUp, SortLines, ToggleComment,
};

const CM: Flavor = Flavor::CommonMark;
const GF: Flavor = Flavor::GfmFile;
const OB: Flavor = Flavor::Obsidian;

fn commands(flavor: Flavor) -> MarkdownCommands {
    MarkdownCommands::new(flavor, CommandSettings::default())
}

/// Checks the result and undo/redo of an edit made by `f`.
fn check_edit(
    label: &str,
    input: &str,
    expected: &str,
    f: impl FnOnce(&mut Editor, Option<SyntaxRef<'_>>) -> Outcome,
    flavor: Flavor,
) -> Option<String> {
    let (mut editor, _) = support::editor(input);
    let text = editor.snapshot();
    let tree = SyntaxTree::parse_with(&text, flavor.parse_options());
    let outcome = f(&mut editor, Some(SyntaxRef::new(&tree, &text)));
    let got = support::render(&editor);
    if got != expected {
        return Some(format!(
            "{label} {input:?}\n   got {got:?}\n  want {expected:?}"
        ));
    }
    let changed = support::parse(input).0 != support::parse(expected).0;
    if changed != (outcome == Outcome::Edited) {
        return Some(format!("{label} {input:?}: outcome {outcome:?}"));
    }
    if changed {
        let output = editor.text().to_string();
        if editor.history().undo_len() != 1 {
            return Some(format!(
                "{label} {input:?}: {} undo steps",
                editor.history().undo_len()
            ));
        }
        if editor.undo() != UndoOutcome::Undone {
            return Some(format!("{label} {input:?}: nothing to undo"));
        }
        let restored = support::render(&editor);
        if restored != input {
            return Some(format!("{label} {input:?}: undo gave {restored:?}"));
        }
        editor.redo();
        if editor.text().to_string() != output {
            return Some(format!("{label} {input:?}: redo differs"));
        }
    }
    None
}

fn check_all(cases: &[(Flavor, MarkdownCommand, &str, &str)]) {
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|&(flavor, command, input, expected)| {
            let c = commands(flavor);
            check_edit(
                &format!("{flavor} {}", command.id()),
                input,
                expected,
                |editor, syntax| c.run(command, editor, syntax),
                flavor,
            )
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn move_lines() {
    check_all(&[
        (CM, MoveLineUp, "a\nb|\nc", "b|\na\nc"),
        (CM, MoveLineDown, "a|\nb\nc", "b\na|\nc"),
        // The last line has no line ending, before and after.
        (CM, MoveLineDown, "a\nb|\nc", "a\nc\nb|"),
        (CM, MoveLineUp, "a\nb\nc|", "a\nc|\nb"),
        // At the edges nothing moves.
        (CM, MoveLineUp, "a|\nb", "a|\nb"),
        (CM, MoveLineDown, "a\nb|", "a\nb|"),
        // Several lines, and the selection moves with them.
        (CM, MoveLineUp, "a\n{b\nc}\nd", "{b\nc}\na\nd"),
        (CM, MoveLineDown, "{a\nb\n}c\nd", "c\n{a\nb\n}d"),
        // Several carets: each line moves; touching ones move together.
        (CM, MoveLineDown, "a|\nb\nc|\nd", "b\na|\nd\nc|"),
        (CM, MoveLineUp, "a\nb|\nc|\nd", "b|\nc|\na\nd"),
        // One caret at the edge stops them all.
        (CM, MoveLineUp, "a|\nb\nc|", "a|\nb\nc|"),
        // A list item moves whole, past the whole sibling.
        (
            GF,
            MoveLineUp,
            "- a\n  - a1\n- b|\n  - b1\n",
            "- b|\n  - b1\n- a\n  - a1\n",
        ),
        (
            GF,
            MoveLineDown,
            "- a|\n  - a1\n- b\n  - b1\n",
            "- b\n  - b1\n- a|\n  - a1\n",
        ),
        // A loose list keeps its blank lines in place.
        (GF, MoveLineUp, "- a\n\n- b|\n", "- b|\n\n- a\n"),
        // The first item moves past the line above the list.
        (GF, MoveLineUp, "x\n- a|\n  more\n", "- a|\n  more\nx\n"),
        // A child line (not a marker line) moves alone.
        (GF, MoveLineUp, "- a\n  b|\n", "  b|\n- a\n"),
        // A fenced block moves whole from its opening fence.
        (
            GF,
            MoveLineUp,
            "x\n```|\ncode\n```\n",
            "```|\ncode\n```\nx\n",
        ),
        // CRLF stays CRLF.
        (CM, MoveLineDown, "a|\r\nb\r\n", "b\r\na|\r\n"),
        (CM, MoveLineDown, "a\r\nb|\r\nc", "a\r\nc\r\nb|"),
    ]);
}

#[test]
fn duplicate_lines() {
    check_all(&[
        (CM, DuplicateLine, "a|b\nc", "ab\na|b\nc"),
        (CM, DuplicateLine, "ab\nc|", "ab\nc\nc|"),
        (CM, DuplicateLine, "{a\nb}\nc", "a\nb\n{a\nb}\nc"),
        (CM, DuplicateLine, "a|\nb|\n", "a\na|\nb\nb|\n"),
        (CM, DuplicateLine, "a|\r\nb", "a\r\na|\r\nb"),
        (CM, DuplicateLine, "|", "\n|"),
    ]);
}

#[test]
fn join_lines() {
    check_all(&[
        (CM, JoinLines, "a|\nb\nc", "a| b\nc"),
        (CM, JoinLines, "a  |\n   b", "a| b"),
        (CM, JoinLines, "{a\nb\nc}\nd", "{a b c}\nd"),
        (CM, JoinLines, "a\nb|", "a\nb|"),
        (CM, JoinLines, "> a|\n> b\n", "> a| b\n"),
        (GF, JoinLines, "- a|\n  b\n", "- a| b\n"),
        (CM, JoinLines, "a|\n\nb", "a|\nb"),
        (CM, JoinLines, "a|\nb\nc|\nd", "a| b\nc| d"),
        (CM, JoinLines, "a|\r\nb\r\n", "a| b\r\n"),
    ]);
}

#[test]
fn sort_lines() {
    check_all(&[
        (CM, SortLines, "{c\nA\nb}\n", "{A\nb\nc}\n"),
        // A caret sorts its list; ordered numbers stay in place.
        (
            GF,
            SortLines,
            "1. pear\n2. apple|\n3. fig\n",
            "1. apple\n2. fig|\n3. pear\n",
        ),
        (GF, SortLines, "- b|\n- a\n\nz\n", "- a|\n- b\n\nz\n"),
        // Task boxes are not part of the key.
        (GF, SortLines, "- [x] b|\n- [ ] a\n", "- [ ] a|\n- [x] b\n"),
        // A caret in a paragraph sorts it.
        (CM, SortLines, "b|\na\n\nc\n", "a|\nb\n\nc\n"),
        // Sorted already, or one line: nothing changes.
        (CM, SortLines, "{a\nb}", "{a\nb}"),
        (CM, SortLines, "a|", "a|"),
        // The last line has no line ending.
        (CM, SortLines, "{b\na}", "{a\nb}"),
        (CM, SortLines, "{b\r\na}", "{a\r\nb}"),
    ]);
}

#[test]
fn change_case() {
    check_all(&[
        (CM, ChangeCase, "{abc} d", "{ABC} d"),
        (CM, ChangeCase, "{ABC DEF} d", "{Abc Def} d"),
        (CM, ChangeCase, "{Abc Def} d", "{abc def} d"),
        // A caret changes its word and stays where it was.
        (CM, ChangeCase, "ab|c d", "AB|C d"),
        // The primary selection decides; every selection follows.
        (CM, ChangeCase, "{ABC} {def}", "{ABC} {DEF}"),
        (CM, ChangeCase, "{def} {ABC}", "{Def} {Abc}"),
        (CM, ChangeCase, "{abc} x {Def}", "{abc} x {def}"),
        // A longer result stays selected.
        (CM, ChangeCase, "{straße}", "{STRASSE}"),
        (CM, ChangeCase, "}abc{", "}ABC{"),
        (CM, ChangeCase, "| x", "| x"),
    ]);
}

#[test]
fn toggle_comment() {
    check_all(&[
        (GF, ToggleComment, "a|b\n", "<!-- a|b -->\n"),
        (GF, ToggleComment, "<!-- a|b -->\n", "a|b\n"),
        (GF, ToggleComment, "x {ab} y\n", "x <!-- {ab} --> y\n"),
        (GF, ToggleComment, "x <!-- {ab} --> y\n", "x {ab} y\n"),
        (GF, ToggleComment, "{a\nb}\n", "<!-- {a\nb} -->\n"),
        (GF, ToggleComment, "> - a|\n", "> - <!-- a| -->\n"),
        (GF, ToggleComment, "|\n", "<!-- | -->\n"),
        (OB, ToggleComment, "a|b\n", "%%a|b%%\n"),
        (OB, ToggleComment, "%%a|b%%\n", "a|b\n"),
        (OB, ToggleComment, "x {ab} y", "x %%{ab}%% y"),
        (OB, ToggleComment, "%%\na|\n%%\n", "\na|\n\n"),
        (CM, ToggleComment, "a|\nb|\n", "<!-- a| -->\n<!-- b| -->\n"),
    ]);
}

#[test]
fn toggle_comment_is_not_offered_in_code() {
    let (editor, _) = support::editor("```\nco|de\n```\n");
    let c = commands(GF);
    assert_eq!(
        c.availability(ToggleComment, &editor, None),
        Availability::NotHere
    );
    assert_eq!(
        c.availability(MoveLineUp, &editor, None),
        Availability::Available
    );
}

#[test]
fn callouts() {
    let cases: &[(Flavor, &str, &str, &str)] = &[
        (OB, "note", "|\n", "> [!note]\n> |\n"),
        (OB, "tip", "a|\nb\n\nc\n", "> [!tip]\n> a|\n> b\n\nc\n"),
        (
            GF,
            "WARNING",
            "x\n\n{a\n\nb}\n",
            "x\n\n> [!WARNING]\n> {a\n>\n> b}\n",
        ),
        (GF, "NOTE", "- a|\n", "- > [!NOTE]\n  > a|\n"),
        // In a callout, the type changes.
        (
            OB,
            "bug",
            "> [!note] Title\n> bo|dy\n",
            "> [!bug] Title\n> bo|dy\n",
        ),
        (GF, "TIP", "a|\r\n", "> [!TIP]\r\n> a|\r\n"),
    ];
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|&(flavor, kind, input, expected)| {
            let c = commands(flavor);
            check_edit(
                &format!("{flavor} callout {kind}"),
                input,
                expected,
                |editor, syntax| c.insert_callout(editor, syntax, kind),
                flavor,
            )
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let (mut editor, _) = support::editor("a|");
    assert_eq!(
        commands(CM).insert_callout(&mut editor, None, "note"),
        Outcome::NotInFlavor
    );
}

/// Expands (`+`) or shrinks (`-`) step by step from `input`, checking the
/// selections after each step.
fn select_steps(flavor: Flavor, input: &str, steps: &[(char, &str)]) {
    let c = commands(flavor);
    let (mut editor, _) = support::editor(input);
    let mut history = SelectionHistory::default();
    for (k, &(op, expected)) in steps.iter().enumerate() {
        let text = editor.snapshot();
        let tree = SyntaxTree::parse_with(&text, flavor.parse_options());
        let syntax = Some(SyntaxRef::new(&tree, &text));
        if op == '+' {
            c.expand_selection(&mut editor, syntax, &mut history);
        } else {
            c.shrink_selection(&mut editor, syntax, &mut history);
        }
        assert_eq!(
            support::render(&editor),
            expected,
            "step {k} ({op}) from {input:?}"
        );
    }
    assert_eq!(editor.history().undo_len(), 0);
}

#[test]
fn expand_and_shrink() {
    let doc = "# H\n\npara **bo|ld** x\n\n## S\n\nmore\n";
    select_steps(
        GF,
        doc,
        &[
            ('+', "# H\n\npara **{bold}** x\n\n## S\n\nmore\n"),
            ('+', "# H\n\npara {**bold**} x\n\n## S\n\nmore\n"),
            ('+', "# H\n\n{para **bold** x}\n\n## S\n\nmore\n"),
            ('+', "{# H\n\npara **bold** x\n\n## S\n\nmore}\n"),
            ('+', "{# H\n\npara **bold** x\n\n## S\n\nmore\n}"),
            ('+', "{# H\n\npara **bold** x\n\n## S\n\nmore\n}"),
            ('-', "{# H\n\npara **bold** x\n\n## S\n\nmore}\n"),
            ('-', "# H\n\n{para **bold** x}\n\n## S\n\nmore\n"),
            ('-', "# H\n\npara {**bold**} x\n\n## S\n\nmore\n"),
            ('-', "# H\n\npara **{bold}** x\n\n## S\n\nmore\n"),
            ('-', "# H\n\npara **bo|ld** x\n\n## S\n\nmore\n"),
        ],
    );
    // Links: the text, then the link; list items, lists, quotes.
    select_steps(
        GF,
        "> - see [the do|cs](u) now\n> - b\n",
        &[
            ('+', "> - see [the {docs}](u) now\n> - b\n"),
            ('+', "> - see [{the docs}](u) now\n> - b\n"),
            ('+', "> - see {[the docs](u)} now\n> - b\n"),
            ('+', "> - {see [the docs](u) now}\n> - b\n"),
            ('+', "> {- see [the docs](u) now}\n> - b\n"),
            ('+', "> {- see [the docs](u) now\n> - b}\n"),
            ('+', "{> - see [the docs](u) now\n> - b}\n"),
        ],
    );
    // Without history (the selection was made by hand), shrink picks the
    // largest step inside.
    select_steps(GF, "{# H\n\npara x}\n", &[('-', "# H\n\n{para x}\n")]);
    // A section within a section.
    select_steps(
        GF,
        "# A\n\n## B\n\nte|xt\n\n# C\n",
        &[
            ('+', "# A\n\n## B\n\n{text}\n\n# C\n"),
            ('+', "# A\n\n{## B\n\ntext}\n\n# C\n"),
            ('+', "{# A\n\n## B\n\ntext}\n\n# C\n"),
        ],
    );
}

#[test]
fn shrink_history_ends_when_the_selection_changes() {
    let c = commands(GF);
    let (mut editor, _) = support::editor("one tw|o three\n");
    let mut history = SelectionHistory::default();
    c.expand_selection(&mut editor, None, &mut history);
    c.expand_selection(&mut editor, None, &mut history);
    assert_eq!(support::render(&editor), "{one two three}\n");
    editor.set_selections(&[gv_editor::SelectionSpec::new(0, 3)], 0);
    c.shrink_selection(&mut editor, None, &mut history);
    // Not back to the word "two": nothing is inside "one", so a caret.
    assert_eq!(support::render(&editor), "one| two three\n");
}

#[test]
fn commands_do_nothing_while_composing() {
    let c = commands(GF);
    let (mut editor, _) = support::editor("a|\nb");
    editor.replace_and_mark_text_in_range_utf16(None, "x", None);
    let before = editor.text().to_string();
    for command in [
        MoveLineDown,
        DuplicateLine,
        JoinLines,
        SortLines,
        ChangeCase,
        ToggleComment,
    ] {
        assert_eq!(
            c.run(command, &mut editor, None),
            Outcome::Unchanged,
            "{}",
            command.id()
        );
    }
    assert_eq!(
        c.insert_callout(&mut editor, None, "NOTE"),
        Outcome::Unchanged
    );
    let mut history = SelectionHistory::default();
    assert!(!c.expand_selection(&mut editor, None, &mut history));
    assert_eq!(editor.text().to_string(), before);
    assert!(editor.is_composing());
}

#[test]
fn headings_and_footnotes() {
    let c = commands(GF);
    let (editor, _) = support::editor("# One\n\ntext\n\nTwo *b*\n---\n\n   ### Three ###\n");
    let headings = c.headings(&editor, None);
    let summary: Vec<(u8, &str, usize)> = headings
        .iter()
        .map(|h| (h.level, h.text.as_str(), h.line))
        .collect();
    assert_eq!(
        summary,
        vec![(1, "One", 0), (2, "Two *b*", 4), (3, "Three", 7)]
    );
    assert_eq!(
        headings[2].offset,
        "# One\n\ntext\n\nTwo *b*\n---\n\n".len()
    );

    let text = "See[^n|ote] and [^x].\n\n[^note]: The note.\n";
    let (editor, _) = support::editor(text);
    let def = "See[^note] and [^x].\n\n".len();
    assert_eq!(c.footnote_target(&editor, None), Some(def..def + 7));
    let (editor, _) = support::editor("See[^note] x.\n\n[^no|te]: The note.\n");
    assert_eq!(c.footnote_target(&editor, None), Some(3..10));
    let (editor, _) = support::editor("See [^x|] x.\n");
    assert_eq!(c.footnote_target(&editor, None), None);
    let (editor, _) = support::editor("pla|in\n");
    assert_eq!(c.footnote_target(&editor, None), None);
}

#[test]
fn line_targets() {
    assert_eq!(parse_line_target("4:2"), Some((4, Some(2))));
    let text = gv_text::Buffer::new("a\r\nbc\r\n").snapshot();
    assert_eq!(line_target_offset(&text, 2, Some(2)), 4);
    assert_eq!(line_target_offset(&text, 3, None), 7);
}
