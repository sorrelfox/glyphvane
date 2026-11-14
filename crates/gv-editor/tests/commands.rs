// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Table tests of the Markdown commands: input and expected output as
//! marked text (`|` a caret, `{`…`}` a selection), per flavor. Every case
//! also checks that one undo restores the input bytes and selections, and
//! that redo reproduces the output text.

mod support;

use gv_editor::commands::{
    Availability, CommandSettings, EmphasisStyle, Flavor, IndentUnit, LinkStyle, MarkdownCommand,
    MarkdownCommands, Numbering, Outcome, SyntaxRef,
};
use gv_editor::{EditKind, Editor, UndoOutcome};
use gv_syntax::SyntaxTree;

use MarkdownCommand::{
    Bold, Heading, Highlight, Indent, InlineCode, InsertLink, Italic, Newline, NewlinePlain,
    Outdent, Paragraph, Quote, RenumberList, Strikethrough, ToggleTask,
};

const CM: Flavor = Flavor::CommonMark;
const GF: Flavor = Flavor::GfmFile;
const OB: Flavor = Flavor::Obsidian;
const EX: Flavor = Flavor::Extended;

/// Runs `command` on marked text with a tree parsed for the flavor.
fn run_with(
    commands: MarkdownCommands,
    command: MarkdownCommand,
    marked: &str,
    url: Option<&str>,
) -> (String, Outcome, Editor) {
    let (mut editor, _) = support::editor(marked);
    let text = editor.snapshot();
    let tree = SyntaxTree::parse_with(&text, commands.flavor.parse_options());
    let syntax = Some(SyntaxRef::new(&tree, &text));
    let outcome = if command == InsertLink {
        commands.insert_link(&mut editor, syntax, url)
    } else {
        commands.run(command, &mut editor, syntax)
    };
    (support::render(&editor), outcome, editor)
}

/// Checks one case; returns a failure message.
fn check(
    commands: MarkdownCommands,
    command: MarkdownCommand,
    input: &str,
    expected: &str,
    url: Option<&str>,
) -> Option<String> {
    let (got, outcome, mut editor) = run_with(commands, command, input, url);
    let label = format!("{:?} {} {input:?}", commands.flavor, command.id());
    if got != expected {
        return Some(format!("{label}\n   got {got:?}\n  want {expected:?}"));
    }
    let (input_text, _) = support::parse(input);
    let (expected_text, _) = support::parse(expected);
    let changed = input_text != expected_text;
    if changed != (outcome == Outcome::Edited) {
        return Some(format!("{label}: outcome {outcome:?}"));
    }
    if changed {
        let output = editor.text().to_string();
        if editor.history().undo_len() != 1 {
            return Some(format!(
                "{label}: {} undo steps",
                editor.history().undo_len()
            ));
        }
        if editor.undo() != UndoOutcome::Undone {
            return Some(format!("{label}: nothing to undo"));
        }
        let restored = support::render(&editor);
        if restored != input {
            return Some(format!("{label}: undo gave {restored:?}"));
        }
        editor.redo();
        if editor.text().to_string() != output {
            return Some(format!(
                "{label}: redo gave {:?}",
                editor.text().to_string()
            ));
        }
    }
    None
}

fn check_all(cases: &[(Flavor, MarkdownCommand, &str, &str)]) {
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|&(flavor, command, input, expected)| {
            let commands = MarkdownCommands::new(flavor, CommandSettings::default());
            check(commands, command, input, expected, None)
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} cases failed:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

#[test]
fn enter_continues_lists() {
    check_all(&[
        (GF, Newline, "- a|", "- a\n- |"),
        (GF, Newline, "- a|\n- b", "- a\n- |\n- b"),
        (GF, Newline, "* a|", "* a\n* |"),
        (GF, Newline, "+   a|", "+   a\n+   |"),
        (GF, Newline, "  - a|", "  - a\n  - |"),
        (GF, Newline, "1. a|\n2. b\n3. c", "1. a\n2. |\n3. b\n4. c"),
        (GF, Newline, "1) a|", "1) a\n2) |"),
        (GF, Newline, "7. a|", "7. a\n8. |"),
        (GF, Newline, "9. a|\n10. b", "9. a\n10. |\n11. b"),
        (GF, Newline, "8. a|\n9. b\n   c", "8. a\n9. |\n10. b\n    c"),
        (GF, Newline, "1. a\n1. b|", "1. a\n1. b\n1. |"),
        (GF, Newline, "- a\n  - b|", "- a\n  - b\n  - |"),
        (GF, Newline, "10. a\n    - b|", "10. a\n    - b\n    - |"),
        (
            GF,
            Newline,
            "- a\n  1. b|\n  2. c",
            "- a\n  1. b\n  2. |\n  3. c",
        ),
        (GF, Newline, "-\ta|", "-\ta\n-   |"),
        (GF, Newline, "- foo| bar", "- foo\n- |bar"),
        (GF, Newline, "|- foo", "- \n- |foo"),
        (GF, Newline, "- {foo} bar", "- \n- |bar"),
        (GF, Newline, "- a\n  b|", "- a\n  b\n- |"),
        (GF, Newline, "- a\nb|", "- a\nb\n- |"),
        (GF, Newline, "- a\n\n  b|", "- a\n\n  b\n- |"),
        (GF, Newline, "- a\n|\n  b", "- a\n\n|\n  b"),
        (GF, Newline, "- # a|", "- # a\n- |"),
        (
            GF,
            Newline,
            "1. a|\n2. b|\n3. c",
            "1. a\n2. |\n3. b\n4. |\n5. c",
        ),
        (GF, Newline, "- a|\n- b|", "- a\n- |\n- b\n- |"),
    ]);
}

#[test]
fn enter_on_tasks() {
    check_all(&[
        (GF, Newline, "- [ ] a|", "- [ ] a\n- [ ] |"),
        (GF, Newline, "- [x] a|", "- [x] a\n- [ ] |"),
        (GF, Newline, "1. [X] a|", "1. [X] a\n2. [ ] |"),
        (GF, Newline, "- [ ] |a", "- [ ] \n- [ ] |a"),
        (GF, Newline, "- [ ]| a", "- [ ] \n- [ ] |a"),
        (CM, Newline, "- [ ] a|", "- [ ] a\n- |"),
        (OB, Newline, "- [?] a|", "- [?] a\n- [ ] |"),
        (GF, Newline, "- [?] a|", "- [?] a\n- |"),
        (GF, Newline, "- a\n- [ ] |", "- a\n|"),
    ]);
}

#[test]
fn enter_on_empty_items() {
    check_all(&[
        (GF, Newline, "- a\n- |", "- a\n|"),
        (GF, Newline, "- |", "|"),
        (GF, Newline, "- a\n-|", "- a\n|"),
        (GF, Newline, "1. a\n2. |", "1. a\n|"),
        (GF, Newline, "- a\n- |\n- c", "- a\n|\n- c"),
        (GF, Newline, "- a\n  - b\n  - |", "- a\n  - b\n- |"),
        (
            GF,
            Newline,
            "1. a\n   - b\n   - |\n2. c",
            "1. a\n   - b\n2. |\n3. c",
        ),
        (GF, Newline, "> - a\n> - |", "> - a\n> |"),
        // CommonMark reads an empty item after a paragraph as a setext
        // underline or as paragraph text: still the next sibling.
        (GF, Newline, "- a\n  - |", "- a\n- |"),
        (GF, Newline, "1. a\n   1. |", "1. a\n2. |"),
        (GF, Newline, "- a\n  * |", "- a\n- |"),
    ]);
}

#[test]
fn enter_on_empty_item_mid_list_renumbers() {
    // The blank line left by the removed marker keeps one (loose) list, so
    // the items after it are renumbered to stay sequential.
    check_all(&[
        (
            CM,
            Newline,
            "1. one\n2. new\n3. |\n4. two\n",
            "1. one\n2. new\n|\n3. two\n",
        ),
        (
            CM,
            Newline,
            "1) one\n2) new\n3) |\n4) two\n",
            "1) one\n2) new\n|\n3) two\n",
        ),
        (
            CM,
            Newline,
            "1. one\r\n2. new\r\n3. |\r\n4. two\r\n",
            "1. one\r\n2. new\r\n|\r\n3. two\r\n",
        ),
        (
            CM,
            Newline,
            "1) one\r\n2) new\r\n3) |\r\n4) two\r\n",
            "1) one\r\n2) new\r\n|\r\n3) two\r\n",
        ),
        (GF, Newline, "3. a\n4. |\n5. b\n6. c", "3. a\n|\n4. b\n5. c"),
        (GF, Newline, "1. |\n2. b\n3. c", "|\n1. b\n2. c"),
        (GF, Newline, "> 1. a\n> 2. |\n> 3. b", "> 1. a\n> |\n> 2. b"),
        // Already sequential after the removal, or numbered all alike.
        (GF, Newline, "1. a\n1. |\n1. b", "1. a\n|\n1. b"),
        (GF, Newline, "1. a\n2. |", "1. a\n|"),
    ]);
}

#[test]
fn enter_in_quotes_and_code() {
    check_all(&[
        (GF, Newline, "> a|", "> a\n> |"),
        (GF, Newline, "> a\n> |", "> a\n|"),
        (GF, Newline, "> a\n>|", "> a\n|"),
        (GF, Newline, "> > a|", "> > a\n> > |"),
        (GF, Newline, "> > a\n> > |", "> > a\n> |"),
        (GF, Newline, "> a\nlazy|", "> a\nlazy\n> |"),
        (GF, Newline, "- > a|", "- > a\n  > |"),
        (GF, Newline, "> - a|", "> - a\n> - |"),
        (GF, Newline, "  > a|", "  > a\n  > |"),
        (
            GF,
            Newline,
            "- ```\n  co|de\n  ```",
            "- ```\n  co\n  |de\n  ```",
        ),
        (GF, Newline, "```\na|\n```", "```\na\n|\n```"),
        (GF, Newline, "a|", "a\n|"),
        (GF, Newline, "# a|", "# a\n|"),
        (GF, Newline, "|", "\n|"),
    ]);
}

#[test]
fn enter_keeps_crlf() {
    check_all(&[
        (GF, Newline, "- a|\r\n- b\r\n", "- a\r\n- |\r\n- b\r\n"),
        (GF, Newline, "1. a\r\n2. b|", "1. a\r\n2. b\r\n3. |"),
        (GF, Newline, "1. a|\r\n2. b\r\n", "1. a\r\n2. |\r\n3. b\r\n"),
        (GF, Newline, "> a|\r\n", "> a\r\n> |\r\n"),
        (GF, Newline, "a|\r\nb", "a\r\n|\r\nb"),
        (GF, NewlinePlain, "- a|\r\n", "- a\r\n  |\r\n"),
    ]);
}

#[test]
fn shift_enter_stays_in_the_container() {
    check_all(&[
        (GF, NewlinePlain, "- a|", "- a\n  |"),
        (GF, NewlinePlain, "10. a|", "10. a\n    |"),
        (GF, NewlinePlain, "> a|", "> a\n> |"),
        (GF, NewlinePlain, "> - a|", "> - a\n>   |"),
        (GF, NewlinePlain, "a|", "a\n|"),
        (OB, NewlinePlain, "- [ ] a|", "- [ ] a\n  |"),
    ]);
}

#[test]
fn tab_indents_items() {
    check_all(&[
        (GF, Indent, "- a\n- b|", "- a\n  - b|"),
        (GF, Indent, "- a\n- b|\n- c", "- a\n  - b|\n- c"),
        (GF, Indent, "- a\n- |b", "- a\n  - |b"),
        (GF, Indent, "1. a\n2. b|\n3. c", "1. a\n   1. b|\n2. c"),
        (GF, Indent, "- a\n  - x\n- b|", "- a\n  - x\n  - b|"),
        (GF, Indent, "- a\n  * x\n- b|", "- a\n  * x\n  * b|"),
        (
            GF,
            Indent,
            "1. a\n   1. x\n2. b|",
            "1. a\n   1. x\n   2. b|",
        ),
        (GF, Indent, "10. a\n11. b|", "10. a\n    1. b|"),
        (GF, Indent, "- a|\n- b", "- a|\n- b"),
        (
            GF,
            Indent,
            "- a\n- b|\n  c\n  - d",
            "- a\n  - b|\n    c\n    - d",
        ),
        (
            GF,
            Indent,
            "-\ta\n-\tb|\n\tc",
            "-\ta\n    -   b|\n        c",
        ),
        (GF, Indent, "- a\n- {b\n- c}", "- a\n  - {b\n  - c}"),
        (GF, Indent, "- a\n- {b\n  - c}", "- a\n  - {b\n    - c}"),
        (GF, Indent, "> - a\n> - b|", "> - a\n>   - b|"),
        (GF, Indent, "- a\n- b|\n- c|", "- a\n  - b|\n  - c|"),
        (GF, Indent, "- [ ] a\n- [ ] b|", "- [ ] a\n  - [ ] b|"),
    ]);
}

#[test]
fn shift_tab_outdents_items() {
    check_all(&[
        (GF, Outdent, "- a\n  - b|", "- a\n- b|"),
        (GF, Outdent, "- a\n  - b|\n  - c", "- a\n- b|\n  - c"),
        (GF, Outdent, "1. a\n   - b|\n2. c", "1. a\n2. b|\n3. c"),
        (
            GF,
            Outdent,
            "1. a\n   1. b|\n   2. c\n2. d",
            "1. a\n2. b|\n   1. c\n3. d",
        ),
        (
            GF,
            Outdent,
            "9. a\n   - b|\n   - c",
            "9. a\n10. b|\n    - c",
        ),
        (GF, Outdent, "- a\n  - b|\n\n  more", "- a\n- b|\n\n  more"),
        (GF, Outdent, "- a\n  - b|\n    c", "- a\n- b|\n  c"),
        (GF, Outdent, "- a|", "- a|"),
        (GF, Outdent, "- a\n  - {b\n  - c}", "- a\n- {b\n- c}"),
        (GF, Outdent, "> - a\n>   - b|", "> - a\n> - b|"),
    ]);
}

#[test]
fn tab_in_quotes_and_elsewhere() {
    check_all(&[
        (GF, Indent, "> a|", "> > a|"),
        (GF, Indent, "> a\n> b|", "> > a\n> > b|"),
        (GF, Indent, "> a\nb|", "> > a\nb|"),
        (GF, Outdent, "> > a|", "> a|"),
        (GF, Outdent, "> a|", "a|"),
        (GF, Indent, "a|b", "a\t|b"),
        (GF, Indent, "a{b}c", "a\t|c"),
        (GF, Indent, "{a\nb}", "\t{a\n\tb}"),
        (GF, Indent, "- ```\n  x|\n  ```", "- ```\n  x\t|\n  ```"),
        (GF, Outdent, "\ta|", "a|"),
        (GF, Outdent, "      a|", "  a|"),
        (GF, Outdent, "  a|", "a|"),
    ]);
}

#[test]
fn renumbering() {
    check_all(&[
        (GF, RenumberList, "3. a\n1. b|\n7. c", "3. a\n4. b|\n5. c"),
        (GF, RenumberList, "1. a\n1. b\n5. c|", "1. a\n2. b\n3. c|"),
        (
            GF,
            RenumberList,
            "8. a\n8. b\n8. c|\n   d",
            "8. a\n9. b\n10. c|\n    d",
        ),
        (
            GF,
            RenumberList,
            "10. a\n10. b\n10. c\n10. d|",
            "10. a\n11. b\n12. c\n13. d|",
        ),
        (GF, RenumberList, "1) a\n5) b|", "1) a\n2) b|"),
        (GF, RenumberList, "- a|", "- a|"),
        (
            GF,
            RenumberList,
            "1. a\n\n   3. x|\n   9. y\n5. b",
            "1. a\n\n   3. x|\n   4. y\n5. b",
        ),
        (
            GF,
            RenumberList,
            "9.\ta\n9.\tb|\n\tc",
            "9.\ta\n10.  b|\n     c",
        ),
    ]);
}

#[test]
fn task_toggle() {
    check_all(&[
        (GF, ToggleTask, "- [ ] a|", "- [x] a|"),
        (GF, ToggleTask, "- [x] a|", "- [ ] a|"),
        (GF, ToggleTask, "- [X] a|", "- [ ] a|"),
        (GF, ToggleTask, "1. [ ] a|", "1. [x] a|"),
        (GF, ToggleTask, "- a|", "- [ ] a|"),
        (GF, ToggleTask, "- |", "- [ ] |"),
        (GF, ToggleTask, "-|", "- [ ] |"),
        (GF, ToggleTask, "a|", "- [ ] a|"),
        (GF, ToggleTask, "|", "- [ ] |"),
        (GF, ToggleTask, "> a|", "> - [ ] a|"),
        (GF, ToggleTask, "- [ ] a\n  b|", "- [x] a\n  b|"),
        (GF, ToggleTask, "- {[ ] a\n- [x] b}", "- {[x] a\n- [ ] b}"),
        (OB, ToggleTask, "- [?] a|", "- [ ] a|"),
        (GF, ToggleTask, "- [?] a|", "- [ ] [?] a|"),
        (GF, ToggleTask, "# a|", "# a|"),
        (GF, ToggleTask, "```\na|\n```", "```\na|\n```"),
        (GF, ToggleTask, "- [ ] a|\r\n", "- [x] a|\r\n"),
    ]);
}

#[test]
fn emphasis_toggles() {
    check_all(&[
        (GF, Bold, "a {b} c", "a **{b}** c"),
        (GF, Bold, "a b|c d", "a **b|c** d"),
        (GF, Bold, "a |bc d", "a **|bc** d"),
        (GF, Bold, "a bc| d", "a **bc|** d"),
        (GF, Bold, "a **b|c** d", "a b|c d"),
        (GF, Bold, "a {**bc**} d", "a {bc} d"),
        (GF, Bold, "a **{bc}** d", "a {bc} d"),
        (GF, Bold, "|", "**|**"),
        (GF, Bold, "a | b", "a **|** b"),
        (GF, Bold, "{ a }", "{ **a** }"),
        (GF, Bold, "a| b|", "**a|** **b|**"),
        (GF, Bold, "a b|b| c", "a **b|b|** c"),
        (GF, Bold, "```\na|\n```", "```\na|\n```"),
        (GF, Bold, "`a|b`", "`a|b`"),
        (GF, Bold, "- a|", "- **a|**"),
        (GF, Bold, "> {a\n> b}", "> **{a\n> b}**"),
        (GF, Italic, "a b|c d", "a *b|c* d"),
        (GF, Italic, "*a|b*", "a|b"),
        (GF, Italic, "***a|b***", "**a|b**"),
        (GF, Bold, "***a|b***", "*a|b*"),
        (GF, Italic, "**a|b**", "***a|b***"),
        (GF, Italic, "_a|b_", "a|b"),
        (GF, Bold, "__a|b__", "a|b"),
        (GF, Bold, "# t|itle", "# **t|itle**"),
    ]);
}

#[test]
fn strike_highlight_and_code_toggles() {
    check_all(&[
        (GF, Strikethrough, "a {b} c", "a ~~{b}~~ c"),
        (GF, Strikethrough, "a ~~b|c~~ d", "a b|c d"),
        (GF, Strikethrough, "a ~~{bc}~~ d", "a {bc} d"),
        (EX, Strikethrough, "a b|c", "a ~~b|c~~"),
        (OB, Highlight, "a b|c", "a ==b|c=="),
        (OB, Highlight, "x ==b|c== y", "x b|c y"),
        (OB, Highlight, "x ==a== ==b|c== y", "x ==a== b|c y"),
        (EX, Highlight, "a {b}", "a =={b}=="),
        (GF, InlineCode, "a {b} c", "a `{b}` c"),
        (GF, InlineCode, "a `b|c` d", "a b|c d"),
        (GF, InlineCode, "a {x`y} z", "a ``{x`y}`` z"),
        (GF, InlineCode, "a {`y} z", "a `` {`y} `` z"),
        (GF, InlineCode, "`` `a|` ``", "`a|`"),
        (GF, InlineCode, "|", "`|`"),
    ]);
}

#[test]
fn flavors_gate_syntax() {
    for (flavor, command) in [
        (CM, Strikethrough),
        (CM, Highlight),
        (GF, Highlight),
        (Flavor::GfmComment, Highlight),
        (CM, ToggleTask),
    ] {
        let commands = MarkdownCommands::new(flavor, CommandSettings::default());
        let (got, outcome, editor) = run_with(commands, command, "a b|c", None);
        assert_eq!(outcome, Outcome::NotInFlavor, "{flavor} {}", command.id());
        assert_eq!(got, "a b|c");
        assert_eq!(editor.history().undo_len(), 0);
        assert_eq!(
            commands.availability(command, &editor, None),
            Availability::NotInFlavor
        );
    }
}

#[test]
fn headings() {
    check_all(&[
        (GF, Heading(2), "a|", "## a|"),
        (GF, Heading(1), "## a|", "# a|"),
        (GF, Heading(2), "## a|", "## a|"),
        (GF, Heading(6), "# a|", "###### a|"),
        (GF, Paragraph, "## a|", "a|"),
        (GF, Paragraph, "# a #|", "a|"),
        (GF, Paragraph, "  ## a|", "a|"),
        (GF, Paragraph, "# - a|", "\\- a|"),
        (GF, Paragraph, "# 1. a|", "1\\. a|"),
        (GF, Paragraph, "# ***|", "\\***|"),
        (GF, Paragraph, "a|", "a|"),
        (GF, Heading(2), "a|\n===", "## a|"),
        (GF, Heading(1), "a|\n===", "a|\n==="),
        (GF, Heading(1), "a\nb|\n---", "# a b|"),
        (GF, Paragraph, "a\n===|", "a|"),
        (GF, Paragraph, "a\n===|\nb", "a\n|\nb"),
        (GF, Paragraph, "a\n===|\n\nb", "a|\n\nb"),
        (GF, Heading(1), "- a|", "- # a|"),
        (GF, Heading(1), "> a|", "> # a|"),
        (GF, Heading(3), "|", "### |"),
        (GF, Heading(1), "a\nb|\nc", "a\n# b|\nc"),
        (GF, Heading(1), "> a\nb|", "> a\n> # b|"),
        (GF, Heading(1), "a\n    b|", "a\n# b|"),
        (GF, Heading(1), "```\na|\n```", "```\na|\n```"),
        (GF, Heading(1), "{a\n\nb}", "# {a\n\n# b}"),
        (GF, Heading(2), "a|\r\n", "## a|\r\n"),
        (GF, Paragraph, "a\r\n===|\r\n", "a|\r\n"),
    ]);
}

#[test]
fn quotes() {
    check_all(&[
        (GF, Quote, "a|", "> a|"),
        (GF, Quote, "> a|", "a|"),
        (GF, Quote, "a\nb|", "> a\n> b|"),
        (GF, Quote, "{a\n\nb}", "> {a\n>\n> b}"),
        (GF, Quote, "- a|", "- > a|"),
        (GF, Quote, "> - a|", "> - > a|"),
        (GF, Quote, "- > a|", "- a|"),
        (GF, Quote, "|", "> |"),
        (GF, Quote, "a|\n\nb|", "> a|\n\n> b|"),
        (GF, Quote, "> a\n>\n> b|", "> a\n>\nb|"),
        (GF, Quote, "> a\nb|", "a\nb|"),
        (GF, Quote, "```\nx|\n```", "> ```\n> x|\n> ```"),
        (GF, Quote, "a|\r\nb\r\n", "> a|\r\n> b\r\n"),
    ]);
}

#[test]
fn links() {
    let commands = MarkdownCommands::new(GF, CommandSettings::default());
    let wiki = MarkdownCommands::new(
        OB,
        CommandSettings {
            link_style: LinkStyle::Wikilink,
            ..CommandSettings::default()
        },
    );
    let wiki_gfm = MarkdownCommands { flavor: GF, ..wiki };
    let url = Some("https://example.com/x");
    let cases: &[(MarkdownCommands, &str, Option<&str>, &str)] = &[
        (commands, "a {text} b", None, "a [text](|) b"),
        (commands, "a b|c d", None, "a [bc](|) d"),
        (commands, "|", None, "[|]()"),
        (
            commands,
            "a {text} b",
            url,
            "a [text](https://example.com/x)| b",
        ),
        (commands, "|", url, "[|](https://example.com/x)"),
        (commands, "{https://x.y/z}", None, "[|](https://x.y/z)"),
        (commands, "a {b} c", Some("not a url"), "a [b](|) c"),
        (
            commands,
            "{a} {b}",
            Some("https://u.v/(1)"),
            "[a](https://u.v/(1))| [b](https://u.v/(1))|",
        ),
        (
            commands,
            "x {a} y",
            Some("https://u.v/a)"),
            "x [a](<https://u.v/a)>)| y",
        ),
        (commands, "`a|b`", None, "`a|b`"),
        (wiki, "a {Note} b", None, "a [[Note|]] b"),
        (wiki, "|", None, "[[|]]"),
        (
            wiki,
            "a {Note} b",
            url,
            "a [Note](https://example.com/x)| b",
        ),
        (wiki_gfm, "a {Note} b", None, "a [Note](|) b"),
    ];
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|&(commands, input, url, expected)| {
            check(commands, InsertLink, input, expected, url)
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Cases the property tests found: each kept the block structure wrong
/// once.
#[test]
fn structure_regressions() {
    check_all(&[
        // The rest of a split item follows into the new item, re-indented
        // for a wider marker; lazy lines get the item's prefix.
        (
            GF,
            Newline,
            "9. j|\n   - three\n- x",
            "9. j\n10. |\n    - three\n- x",
        ),
        (
            GF,
            Newline,
            "* f\n  dee|p\nzz\n---",
            "* f\n  dee\n* |p\n  zz\n---",
        ),
        (GF, Newline, "> - o|\n   l\n---", "> - o\n> - |\n>   l\n---"),
        // Shift+Enter keeps lazy lines in the container and opens a line
        // above when the caret is in the marker.
        (GF, NewlinePlain, "> - o|\nlazy", "> - o\n>   |\n>   lazy"),
        (GF, NewlinePlain, "1|. h", "\n1|. h"),
        (GF, NewlinePlain, "> 1|. h", "\n> 1|. h"),
        (GF, NewlinePlain, "> a\n> 1|. h", "> a\n> \n> 1|. h"),
        // A caret in a marker: inline commands work on the item's text.
        (GF, InsertLink, "|- bb cc", "- [bb](|) cc"),
        (GF, Bold, "|- bb cc", "- **|bb** cc"),
        // A blank line inside an item gets the item's indentation; a bare
        // marker gets a space.
        (GF, Heading(1), "- a\n|\n  b", "- a\n  # |\n  b"),
        (GF, ToggleTask, "- a\n|\n  b", "- [ ] a\n|\n  b"),
        (GF, ToggleTask, "> a\n>|\n> b", "> a\n> - [ ] |\n> b"),
        (GF, Heading(2), "1.|", "1. ## |"),
        (GF, InsertLink, "1.|", "1. [|]()"),
        (GF, Bold, "1.|", "1. **|**"),
        // The rest of a paragraph after a line made a heading stays text.
        (GF, Heading(1), "a|\n2. i", "# a|\n2\\. i"),
        (GF, Heading(1), "a|\n\tx", "# a|\nx"),
        (GF, Heading(1), "- a|\nlazy", "- # a|\n  lazy"),
        // Tab over a selected line break indents the lines.
        (GF, Indent, "a{b\n}c", "\ta{b\n}c"),
    ]);
}

#[test]
fn settings_change_what_is_written() {
    let spaces = MarkdownCommands::new(
        GF,
        CommandSettings {
            indent: IndentUnit::Spaces(2),
            ..CommandSettings::default()
        },
    );
    let underscore = MarkdownCommands::new(
        GF,
        CommandSettings {
            emphasis: EmphasisStyle::Underscore,
            ..CommandSettings::default()
        },
    );
    let ones = MarkdownCommands::new(
        GF,
        CommandSettings {
            numbering: Numbering::Ones,
            ..CommandSettings::default()
        },
    );
    let cases: &[(MarkdownCommands, MarkdownCommand, &str, &str)] = &[
        (spaces, Indent, "a|b", "a  |b"),
        (spaces, Outdent, "    a|", "  a|"),
        (underscore, Italic, "a b|c", "a _b|c_"),
        (underscore, Bold, "a b|c", "a __b|c__"),
        (underscore, Italic, "x{b}y", "x*{b}*y"),
        (ones, Newline, "1. a|", "1. a\n1. |"),
        (ones, Newline, "1. a\n2. b|", "1. a\n2. b\n3. |"),
        (ones, RenumberList, "1. a\n2. b|", "1. a\n1. b|"),
        (ones, Indent, "1. a\n1. b|\n1. c", "1. a\n   1. b|\n1. c"),
    ];
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|&(commands, command, input, expected)| {
            check(commands, command, input, expected, None)
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn availability_follows_the_caret() {
    let commands = MarkdownCommands::new(GF, CommandSettings::default());
    let at = |marked: &str, command: MarkdownCommand| {
        let (editor, _) = support::editor(marked);
        commands.availability(command, &editor, None)
    };
    assert_eq!(at("a|", Bold), Availability::Available);
    assert_eq!(at("```\na|\n```", Bold), Availability::NotHere);
    assert_eq!(at("```\na|\n```", InlineCode), Availability::NotHere);
    assert_eq!(at("`a|b`", Bold), Availability::NotHere);
    assert_eq!(at("`a|b`", InlineCode), Availability::Available);
    assert_eq!(at("- a|", RenumberList), Availability::NotHere);
    assert_eq!(at("1. a|", RenumberList), Availability::Available);
    assert_eq!(at("```\na|\n```", Heading(1)), Availability::NotHere);
    assert_eq!(at("---\n|", Heading(1)), Availability::Available);
    assert_eq!(at("# a|", ToggleTask), Availability::NotHere);
    assert_eq!(at("- a|", ToggleTask), Availability::Available);
    assert_eq!(at("```\na|\n```", Newline), Availability::Available);
    // Any caret will do.
    assert_eq!(at("```\na|\n```\nb|", Bold), Availability::Available);
    let (editor, _) = support::editor("a|");
    let all = commands.availabilities(&editor, None);
    assert_eq!(all.len(), MarkdownCommand::ALL.len());
    assert!(all.contains(&(Highlight, Availability::NotInFlavor)));
    assert!(all.contains(&(Bold, Availability::Available)));
}

#[test]
fn command_ids_match_the_registry() {
    for command in MarkdownCommand::ALL {
        assert_eq!(MarkdownCommand::from_id(command.id()), Some(command));
        assert!(command.id().starts_with("editor."));
    }
    assert_eq!(
        MarkdownCommand::from_id("editor.heading_3"),
        Some(Heading(3))
    );
    assert_eq!(MarkdownCommand::from_id("editor.nope"), None);
}

#[test]
fn a_stale_tree_is_replaced() {
    let commands = MarkdownCommands::new(GF, CommandSettings::default());
    let (mut editor, _) = support::editor("- a|");
    let old_text = editor.snapshot();
    let old_tree = SyntaxTree::parse(&old_text);
    editor.insert_at_selections("b");
    // The tree is of the text before "b": the command parses again.
    commands.run(
        Newline,
        &mut editor,
        Some(SyntaxRef::new(&old_tree, &old_text)),
    );
    assert_eq!(support::render(&editor), "- ab\n- |");
}

#[test]
fn a_command_is_one_undo_step_after_typing() {
    let commands = MarkdownCommands::new(GF, CommandSettings::default());
    let (mut editor, _) = support::editor("|");
    editor.insert_at_selections("- a");
    commands.run(Newline, &mut editor, None);
    editor.insert_at_selections("b");
    assert_eq!(support::render(&editor), "- a\n- b|");
    editor.undo();
    assert_eq!(support::render(&editor), "- a\n- |");
    editor.undo();
    assert_eq!(support::render(&editor), "- a|");
    let _ = EditKind::Command;
}
