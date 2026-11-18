// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The table and footnote commands. Pipes are table syntax here, so the
//! marked text uses `‸` for a caret and `«`…`»` for a selection (tail at
//! `«`, head at `»`); the last one written is primary. Every case checks
//! that the command is one undo step that restores the input bytes and
//! selections, and that redo reproduces the output.

use gv_editor::commands::{
    Availability, CommandSettings, FootnotePlacement, MarkdownCommand, MarkdownCommands, Outcome,
    SyntaxRef, TableOp,
};
use gv_editor::{Editor, EditorConfig, ManualClock, SelectionSpec, UndoOutcome};
use gv_syntax::SyntaxTree;
use gv_text::Buffer;

use MarkdownCommand::{Indent, InsertFootnote, Outdent, Table};

fn parse(marked: &str) -> (String, Vec<SelectionSpec>) {
    let mut text = String::new();
    let mut specs = Vec::new();
    let mut tail = None;
    for c in marked.chars() {
        match c {
            '‸' => specs.push(SelectionSpec::caret(text.len())),
            '«' => tail = Some(text.len()),
            '»' => specs.push(SelectionSpec::new(
                tail.take().expect("« before »"),
                text.len(),
            )),
            _ => text.push(c),
        }
    }
    (text, specs)
}

fn render(editor: &Editor) -> String {
    let text = editor.text().to_string();
    let mut marks: Vec<(usize, &str)> = Vec::new();
    for selection in editor.selections().iter() {
        if selection.is_empty() {
            marks.push((selection.head(), "‸"));
        } else {
            marks.push((selection.tail(), "«"));
            marks.push((selection.head(), "»"));
        }
    }
    marks.sort_by_key(|&(offset, mark)| (offset, mark != "»"));
    let mut out = String::new();
    let mut pos = 0;
    for (offset, mark) in marks {
        out.push_str(&text[pos..offset]);
        out.push_str(mark);
        pos = offset;
    }
    out.push_str(&text[pos..]);
    out
}

fn editor(marked: &str) -> Editor {
    let (text, specs) = parse(marked);
    let mut editor = Editor::with_config(
        Buffer::new(&text),
        EditorConfig::default(),
        Box::new(ManualClock::new()),
    );
    if !specs.is_empty() {
        editor.set_selections(&specs, specs.len() - 1);
    }
    editor
}

fn run(commands: MarkdownCommands, command: MarkdownCommand, input: &str) -> (String, Outcome) {
    let mut editor = editor(input);
    let text = editor.snapshot();
    let tree = SyntaxTree::parse_with(&text, commands.flavor.parse_options());
    let outcome = commands.run(command, &mut editor, Some(SyntaxRef::new(&tree, &text)));
    let got = render(&editor);
    let (input_text, _) = parse(input);
    let after = editor.text().to_string();
    if after != input_text {
        assert_eq!(outcome, Outcome::Edited, "{input:?}");
        assert_eq!(editor.history().undo_len(), 1, "one undo step: {input:?}");
        assert_eq!(editor.undo(), UndoOutcome::Undone);
        assert_eq!(render(&editor), input, "undo restores {input:?}");
        assert!(editor.redo());
        assert_eq!(editor.text().to_string(), after, "redo");
    } else {
        assert_eq!(editor.history().undo_len(), 0, "{input:?}");
    }
    (got, outcome)
}

fn gfm() -> MarkdownCommands {
    MarkdownCommands::for_profile("gfm-file")
}

#[track_caller]
fn check(command: MarkdownCommand, input: &str, expected: &str) {
    let (got, _) = run(gfm(), command, input);
    assert_eq!(got, expected, "\n{} on {input:?}", command.id());
}

const FORMAT: MarkdownCommand = Table(TableOp::Format);

#[test]
fn format_aligns_pipes() {
    check(
        FORMAT,
        "|a|b‸|\n|-|-:|\n|long cell|1|\n",
        "| a         |   b‸ |\n| --------- | --: |\n| long cell |   1 |\n",
    );
    // Without outer pipes, short rows filled, alignment colons kept.
    check(
        FORMAT,
        "x ‸| y | z\n:-|:-:|-\n1\n",
        "| x‸   |  y  | z   |\n| :-- | :-: | --- |\n| 1   |     |     |\n",
    );
    // Formatted already: nothing changes, no undo step.
    let done = "| a‸   | b   |\n| --- | --- |\n| 1   | 2   |";
    let (got, outcome) = run(gfm(), FORMAT, done);
    assert_eq!(got, done);
    assert_eq!(outcome, Outcome::Unchanged);
}

#[test]
fn format_keeps_escaped_pipes() {
    check(
        FORMAT,
        "| a | b |\n|---|---|\n| x\\|y‸ | `c\\|d` |\n",
        "| a    | b      |\n| ---- | ------ |\n| x\\|y‸ | `c\\|d` |\n",
    );
}

#[test]
fn format_counts_east_asian_and_emoji_widths() {
    check(
        FORMAT,
        "|名前|x|\n|-|-|\n|ab‸|😀|\n|ｶﾀｶﾅ|👩‍💻|\n",
        "| 名前 | x   |\n| ---- | --- |\n| ab‸   | 😀  |\n| ｶﾀｶﾅ | 👩‍💻  |\n",
    );
}

#[test]
fn format_keeps_container_prefixes() {
    check(
        FORMAT,
        "> |a|b|\n> |-|-|\n> |c‸|d|\n",
        "> | a   | b   |\n> | --- | --- |\n> | c‸   | d   |\n",
    );
    check(
        FORMAT,
        "- item\n\n  |a|b|\n  |-|-|\n  |c‸|d|\n",
        "- item\n\n  | a   | b   |\n  | --- | --- |\n  | c‸   | d   |\n",
    );
    // CRLF stays CRLF.
    check(
        FORMAT,
        "|a|b|\r\n|-|-|\r\n|c‸|d|\r\n",
        "| a   | b   |\r\n| --- | --- |\r\n| c‸   | d   |\r\n",
    );
}

#[test]
fn insert_table() {
    check(FORMAT, "‸", "| ‸    |     |\n| --- | --- |\n|     |     |");
    check(
        FORMAT,
        "Text‸ here.\nmore\n\nNext",
        "Text here.\nmore\n\n| ‸    |     |\n| --- | --- |\n|     |     |\n\nNext",
    );
    check(
        FORMAT,
        "para\n‸\nnext\n",
        "para\n\n| ‸    |     |\n| --- | --- |\n|     |     |\n\nnext\n",
    );
    check(
        FORMAT,
        "> quote‸\n",
        "> quote\n>\n> | ‸    |     |\n> | --- | --- |\n> |     |     |\n",
    );
    // After a lazy continuation line, the table stays in the item (and the
    // quote): it takes the prefix of the paragraph's first line.
    check(
        FORMAT,
        "* f‸\nlazy\n   - nested\n- a",
        "* f\nlazy\n\n  | ‸    |     |\n  | --- | --- |\n  |     |     |\n\n   - nested\n- a",
    );
    // On an empty item, at the item's content column (a bare `-` is one
    // column short of it).
    check(
        FORMAT,
        "- a\n-‸\nnext",
        "- a\n-\n  | ‸    |     |\n  | --- | --- |\n  |     |     |\n\nnext",
    );
    check(
        FORMAT,
        "\n-‸\n   l",
        "\n- | ‸    |     |\n  | --- | --- |\n  |     |     |\n\n   l",
    );
    // On a blank line inside an item, in the item.
    check(
        FORMAT,
        "- a\n\n‸\n  b\n",
        "- a\n\n  | ‸    |     |\n  | --- | --- |\n  |     |     |\n\n  b\n",
    );
    check(
        FORMAT,
        "> 1.‸\n",
        "> 1. | ‸    |     |\n>    | --- | --- |\n>    |     |     |\n",
    );
    check(
        FORMAT,
        "> q‸\nlazy\n",
        "> q\nlazy\n>\n> | ‸    |     |\n> | --- | --- |\n> |     |     |\n",
    );
}

#[test]
fn tab_moves_between_cells() {
    let table = |marked: &str| marked.to_owned();
    check(
        Indent,
        &table("| a‸ | b |\n|---|---|\n| c | d |\n"),
        "| a   | «b»   |\n| --- | --- |\n| c   | d   |\n",
    );
    // At the end of a row: the first cell of the next body row.
    check(
        Indent,
        "| a | b‸ |\n|---|---|\n| c | d |\n",
        "| a   | b   |\n| --- | --- |\n| «c»   | d   |\n",
    );
    // At the last cell: a new row.
    check(
        Indent,
        "| a | b |\n|---|---|\n| c | d‸ |\n",
        "| a   | b   |\n| --- | --- |\n| c   | d   |\n| ‸    |     |\n",
    );
    // Shift+Tab goes back, from the first body row to the header.
    check(
        Outdent,
        "| a | b |\n|---|---|\n| c‸ | d |\n",
        "| a   | «b»   |\n| --- | --- |\n| c   | d   |\n",
    );
    check(
        Outdent,
        "| a | b |\n|---|---|\n| c | d‸ |\n",
        "| a   | b   |\n| --- | --- |\n| «c»   | d   |\n",
    );
    // Several carets, one per table row.
    check(
        Indent,
        "| a‸ | b |\n|---|---|\n| c‸ | d |\n",
        "| a   | «b»   |\n| --- | --- |\n| c   | «d»   |\n",
    );
    // Outside a table Tab still indents.
    check(Indent, "a‸", "a\t‸");
}

#[test]
fn rows() {
    let t = "| h | i |\n|---|---|\n| a | 1 |\n| b‸ | 2 |\n";
    check(
        Table(TableOp::InsertRowAbove),
        t,
        "| h   | i   |\n| --- | --- |\n| a   | 1   |\n| ‸    |     |\n| b   | 2   |\n",
    );
    check(
        Table(TableOp::InsertRowBelow),
        t,
        "| h   | i   |\n| --- | --- |\n| a   | 1   |\n| b   | 2   |\n| ‸    |     |\n",
    );
    check(
        Table(TableOp::DeleteRow),
        t,
        "| h   | i   |\n| --- | --- |\n| ‸a   | 1   |\n",
    );
    check(
        Table(TableOp::MoveRowUp),
        t,
        "| h   | i   |\n| --- | --- |\n| b‸   | 2   |\n| a   | 1   |\n",
    );
    // Nothing to move: nothing changes.
    let (got, outcome) = run(gfm(), Table(TableOp::MoveRowDown), t);
    assert_eq!((got.as_str(), outcome), (t, Outcome::Unchanged));
    // The header has no row above it and cannot be deleted.
    let head = "| h‸ | i |\n|---|---|\n| a | 1 |";
    let (got, outcome) = run(gfm(), Table(TableOp::DeleteRow), head);
    assert_eq!(got, head);
    assert_eq!(outcome, Outcome::Unchanged);
    check(
        Table(TableOp::InsertRowBelow),
        head,
        "| h   | i   |\n| --- | --- |\n| ‸    |     |\n| a   | 1   |",
    );
}

#[test]
fn columns() {
    let t = "| h | i‸ | j |\n|:--|--:|---|\n| a | 1 | x |\n";
    check(
        Table(TableOp::InsertColumnLeft),
        t,
        "| h   | ‸    |   i |  j  |\n| :-- | --- | --: | --- |\n| a   |     |   1 | x   |\n"
            .replace("|  j  |", "| j   |")
            .as_str(),
    );
    check(
        Table(TableOp::InsertColumnRight),
        t,
        "| h   |   i | ‸    | j   |\n| :-- | --: | --- | --- |\n| a   |   1 |     | x   |\n",
    );
    check(
        Table(TableOp::DeleteColumn),
        t,
        "| h   | ‸j   |\n| :-- | --- |\n| a   | x   |\n",
    );
    check(
        Table(TableOp::MoveColumnLeft),
        t,
        "|   i‸ | h   | j   |\n| --: | :-- | --- |\n|   1 | a   | x   |\n",
    );
    check(
        Table(TableOp::MoveColumnRight),
        t,
        "| h   | j   |   i‸ |\n| :-- | --- | --: |\n| a   | x   |   1 |\n",
    );
}

#[test]
fn alignment() {
    let t = "| h‸ | i |\n|---|---|\n| a | 1 |\n";
    check(
        Table(TableOp::AlignRight),
        t,
        "|   h‸ | i   |\n| --: | --- |\n|   a | 1   |\n",
    );
    check(
        Table(TableOp::AlignCenter),
        t,
        "|  h‸  | i   |\n| :-: | --- |\n|  a  | 1   |\n",
    );
    check(
        Table(TableOp::AlignLeft),
        t,
        "| h‸   | i   |\n| :-- | --- |\n| a   | 1   |\n",
    );
    check(
        Table(TableOp::AlignNone),
        "| h‸ | i |\n|:-:|---|\n| a | 1 |\n",
        "| h‸   | i   |\n| --- | --- |\n| a   | 1   |\n",
    );
}

#[test]
fn sort_by_column() {
    let t = "| n | v‸ |\n|---|---|\n| a | 10 |\n| b | 9 |\n| c | 100 |\n";
    check(
        Table(TableOp::SortByColumn),
        t,
        "| n   | v‸   |\n| --- | --- |\n| b   | 9   |\n| a   | 10  |\n| c   | 100 |\n",
    );
    // Sorted already: descending.
    check(
        Table(TableOp::SortByColumn),
        "| n | v‸ |\n|---|---|\n| b | 9 |\n| a | 10 |\n",
        "| n   | v‸   |\n| --- | --- |\n| a   | 10  |\n| b   | 9   |\n",
    );
}

#[test]
fn tables_need_a_profile_with_tables() {
    let cm = MarkdownCommands::for_profile("commonmark");
    let (got, outcome) = run(cm, FORMAT, "‸");
    assert_eq!(got, "‸");
    assert_eq!(outcome, Outcome::NotInFlavor);
    let at = |commands: MarkdownCommands, command, marked: &str| {
        let editor = editor(marked);
        commands.availability(command, &editor, None)
    };
    assert_eq!(at(cm, FORMAT, "‸"), Availability::NotInFlavor);
    assert_eq!(at(gfm(), FORMAT, "‸"), Availability::Available);
    assert_eq!(at(gfm(), FORMAT, "```\n‸\n```"), Availability::NotHere);
    let row = Table(TableOp::InsertRowBelow);
    assert_eq!(at(gfm(), row, "text‸"), Availability::NotHere);
    assert_eq!(at(gfm(), row, "|a|\n|-|\n|b‸|"), Availability::Available);
    // In CommonMark a pipe table is a paragraph: Tab inserts a tab.
    let (got, _) = run(cm, Indent, "|a‸|\n|-|");
    assert_eq!(got, "|a\t‸|\n|-|");
}

#[test]
fn no_table_command_while_composing() {
    let mut editor = editor("|a‸|\n|-|");
    editor.replace_and_mark_text_in_range_utf16(None, "x", None);
    let before = editor.text().to_string();
    assert_eq!(gfm().run(FORMAT, &mut editor, None), Outcome::Unchanged);
    assert_eq!(
        gfm().run(InsertFootnote, &mut editor, None),
        Outcome::Unchanged
    );
    assert_eq!(editor.text().to_string(), before);
}

fn footnotes(placement: FootnotePlacement) -> MarkdownCommands {
    MarkdownCommands::new(
        gv_editor::commands::Flavor::GfmFile,
        CommandSettings {
            footnotes: placement,
            ..CommandSettings::default()
        },
    )
}

#[test]
fn insert_footnote() {
    let end = footnotes(FootnotePlacement::EndOfDocument);
    let cases = [
        ("Text‸ here.\n", "Text[^1] here.\n\n[^1]: ‸\n"),
        ("Text‸", "Text[^1]\n\n[^1]: ‸"),
        // The next free number; after existing definitions.
        (
            "A[^1] b[^3]‸ c\n\n[^1]: one\n[^3]: three\n",
            "A[^1] b[^3][^4] c\n\n[^1]: one\n[^3]: three\n[^4]: ‸\n",
        ),
        // Code is not read.
        ("```\n[^7]\n```\nx‸\n", "```\n[^7]\n```\nx[^1]\n\n[^1]: ‸\n"),
        // Two carets: two footnotes in document order.
        ("a‸ b‸\n", "a[^1] b[^2]\n\n[^1]: ‸\n[^2]: ‸\n"),
        // CRLF.
        ("a‸\r\n", "a[^1]\r\n\r\n[^1]: ‸\r\n"),
        // After a selection.
        ("«word» x\n", "word[^1] x\n\n[^1]: ‸\n"),
    ];
    for (input, expected) in cases {
        let (got, _) = run(end, InsertFootnote, input);
        assert_eq!(got, expected, "{input:?}");
    }
    let after = footnotes(FootnotePlacement::AfterParagraph);
    let cases = [
        ("One‸\ntwo\n\nThree\n", "One[^1]\ntwo\n\n[^1]: ‸\n\nThree\n"),
        ("- a‸\n- b\n# H\n", "- a[^1]\n- b\n\n[^1]: ‸\n\n# H\n"),
        ("Last‸", "Last[^1]\n\n[^1]: ‸"),
    ];
    for (input, expected) in cases {
        let (got, _) = run(after, InsertFootnote, input);
        assert_eq!(got, expected, "{input:?}");
    }
    let cm = MarkdownCommands::for_profile("commonmark");
    assert_eq!(run(cm, InsertFootnote, "a‸").1, Outcome::NotInFlavor);
}

#[test]
fn go_to_footnote() {
    let commands = gfm();
    let target = |marked: &str| {
        let editor = editor(marked);
        let (text, _) = parse(marked);
        commands
            .footnote_target(&editor, None)
            .map(|r| (r.start, text[r].to_owned()))
    };
    assert_eq!(target("a[^n‸]\n\n[^n]: x\n"), Some((7, "[^n]".to_owned())));
    assert_eq!(target("a[^n]\n\n[^n‸]: x\n"), Some((1, "[^n]".to_owned())));
    // Case-insensitive labels. Code blocks are skipped; code spans are
    // not (the parser does not read footnotes yet, so the text is matched).
    assert_eq!(
        target("`[^N]` b[^N]\n\n[^n‸]: x\n"),
        Some((1, "[^N]".to_owned()))
    );
    assert_eq!(
        target("```\n[^a]\n```\nb[^a]\n\n[^a‸]: x\n"),
        Some((14, "[^a]".to_owned()))
    );
    assert_eq!(target("a[^n]\n\n[^n]: x‸\n"), Some((1, "[^n]".to_owned())));
    assert_eq!(target("a[^n‸]\n"), None);
    assert_eq!(target("plain‸\n"), None);
}
