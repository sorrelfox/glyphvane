// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Property tests of the Markdown commands on random documents built from
//! list, quote, heading, code and paragraph lines (LF or CRLF), with random
//! carets and selections, in every flavor:
//!
//! - every command is one undo step: undo restores the bytes and the
//!   selections, redo the result;
//! - the edit is a patch: the top-level blocks the edit touches may grow
//!   (CommonMark joins across it), every other top-level block parses
//!   exactly as before (shifted), and no code or HTML block is opened; for
//!   commands that change what a line is (heading, paragraph, task, quote,
//!   link), CommonMark reads the following lines anew (a list even across
//!   blank lines), so only the blocks before the last blank line before the
//!   edit are checked; so too for Enter that leaves an empty item right
//!   above a blank line while the item went on after it (an item may start
//!   with at most one blank line, so the rest of the old item is read anew);
//! - a CRLF document stays CRLF;
//! - the line commands (move, duplicate, join, sort, toggle comment) are
//!   checked for the properties above and below but not for structure:
//!   rearranging lines changes what they are; change case keeps the
//!   structure exactly; insert callout is checked like Quote;
//! - the editor's text is the command's own result (checked inside the
//!   crate on every commit in debug builds);
//! - on documents of pipe tables (escaped pipes, East Asian and emoji
//!   cells, tables in quotes and list items), the table commands, Tab,
//!   Shift+Tab and insert footnote keep the properties above, and
//!   formatting is idempotent and keeps every cell's text.

use std::cell::RefCell;
use std::rc::Rc;

use gv_editor::commands::{CommandSettings, Flavor, MarkdownCommand, MarkdownCommands, SyntaxRef};
use gv_editor::{Editor, EditorConfig, EditorEvent, SelectionSpec, UndoOutcome};
use gv_syntax::{BlockKind, BlockSpan, SyntaxTree};
use gv_text::Buffer;
use proptest::prelude::*;
use proptest::sample::Index;

const LINES: &[&str] = &[
    "- a",
    "- bb cc",
    "  - d",
    "    - e",
    "* f",
    "+ g",
    "1. h",
    "2. i",
    "9. j",
    "10. k",
    "   l",
    "    m",
    "> n",
    "> - o",
    ">",
    "> > p",
    "",
    "",
    "```",
    "# q",
    "r s t",
    "- [ ] u",
    "- [x] v",
    "-\tw",
    "\tx",
    "1) y",
    "-",
    "1.",
    "==z== x",
    "~~w~~ y",
    "**b** *i* `c`",
    "Title",
    "===",
    "---",
    "  1. nested",
    "     deep",
    "   - three",
    "8. eight",
    "> 1. quoted",
];

/// Lines of the table documents.
const TABLE_LINES: &[&str] = &[
    "| a | b |",
    "|---|:-:|",
    "a | b | c",
    "--|--:|:--",
    "| c\\| | 日本語 |",
    "| 😀 | 👩‍💻 x |",
    "|x|",
    "| `p\\|q` |  |",
    "> | q | r |",
    "> |-|-|",
    "> | s |",
    "- item",
    "  | i | j |",
    "  | - | - |",
    "text [^1]",
    "[^1]: note",
    "",
    "",
    "```",
];

/// A transaction's replacements: old range and new length.
type Replacements = Vec<(std::ops::Range<usize>, usize)>;

/// A command under test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Under {
    /// A command of [`MarkdownCommand::ALL`].
    Command(MarkdownCommand),
    /// Insert link with a URL.
    LinkWithUrl,
    /// Insert callout of a type.
    Callout,
}

/// The commands under test: every command, plus insert link with a URL
/// and insert callout.
fn command(index: usize) -> Under {
    let all = MarkdownCommand::ALL;
    match all.get(index) {
        Some(&command) => Under::Command(command),
        None if index == all.len() => Under::LinkWithUrl,
        None => Under::Callout,
    }
}

fn document() -> impl Strategy<Value = String> {
    document_of(LINES)
}

fn document_of(lines: &'static [&'static str]) -> impl Strategy<Value = String> {
    (
        prop::collection::vec(prop::sample::select(lines), 1..12),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(lines, crlf, trailing)| {
            let ending = if crlf { "\r\n" } else { "\n" };
            let mut text = lines.join(ending);
            if trailing {
                text.push_str(ending);
            }
            text
        })
}

/// Selections from (position, length) pairs, clipped to the text (all
/// lines are ASCII, but a caret must not split a CRLF).
fn selections(text: &str, picks: &[(usize, u8)]) -> Vec<SelectionSpec> {
    let clip = |mut offset: usize| {
        offset = offset.min(text.len());
        if offset > 0
            && text.as_bytes()[offset - 1] == b'\r'
            && text.as_bytes().get(offset) == Some(&b'\n')
        {
            offset -= 1;
        }
        offset
    };
    picks
        .iter()
        .map(|&(start, len)| {
            let start = clip(start);
            let end = clip(start + usize::from(len % 4).saturating_sub(1));
            SelectionSpec::new(start, end.max(start))
        })
        .collect()
}

/// The blocks inside `range`, with ranges relative to `range.start`.
fn blocks_within(
    blocks: &[BlockSpan],
    range: std::ops::Range<usize>,
) -> Vec<(String, u32, usize, usize)> {
    blocks
        .iter()
        .filter(|block| block.range.start >= range.start && block.range.end <= range.end)
        .map(|block| {
            (
                format!("{:?}", block.kind),
                block.depth,
                block.range.start - range.start,
                block.range.end - range.start,
            )
        })
        .collect()
}

/// Checks the blocks outside the edit. `edited` is the old range spanning
/// every replacement of the command's transaction.
///
/// The top-level blocks of the new parse that touch the edited range may
/// have grown (CommonMark joins across the edit: a paragraph continues, a
/// list or quote gains lines, adjacent lists or quotes merge); every
/// top-level block wholly outside their span must parse exactly as before
/// (shifted), and the edit must not have opened a code or HTML block.
fn check_structure(
    flavor: Flavor,
    before: &str,
    after: &str,
    edits: &[(std::ops::Range<usize>, usize)],
    separated: bool,
) -> Result<(), String> {
    let (Some(first), Some(last)) = (edits.first(), edits.last()) else {
        return Ok(());
    };
    let edited = first.0.start..last.0.end;
    // An old offset outside the edits, in the new text.
    let map = |offset: usize| -> usize {
        let delta: isize = edits
            .iter()
            .filter(|(old, _)| old.end <= offset)
            .map(|(old, new_len)| *new_len as isize - old.len() as isize)
            .sum();
        offset.saturating_add_signed(delta)
    };
    let new_end = map(edited.end);
    let old_tree = SyntaxTree::parse_with(before, flavor.parse_options());
    let new_tree = SyntaxTree::parse_with(after, flavor.parse_options());
    let old = old_tree.blocks();
    let new = new_tree.blocks();
    let touched: Vec<&BlockSpan> = new
        .iter()
        .filter(|block| {
            block.depth == 0 && block.range.start <= new_end && block.range.end >= edited.start
        })
        .collect();
    let mut span_start = touched
        .iter()
        .map(|block| block.range.start)
        .min()
        .unwrap_or(edited.start);
    let mut span_end = touched
        .iter()
        .map(|block| block.range.end)
        .max()
        .unwrap_or(new_end);
    if separated {
        // The lines after the edit are read anew: only the blocks before
        // the old text's last blank line before the edit are checked.
        span_start = span_start.min(
            line_ranges(before)
                .filter(|range| before[range.clone()].trim().is_empty())
                .filter(|range| range.end <= edited.start)
                .last()
                .map_or(0, |range| range.end),
        );
        span_end = usize::MAX;
    }
    for block in &touched {
        let verbatim = matches!(block.kind, BlockKind::FencedCode | BlockKind::Html { .. });
        let existed = old
            .iter()
            .any(|o| o.kind == block.kind && map(o.range.start) == block.range.start);
        if verbatim && !existed && block.range.end > new_end {
            return Err(format!(
                "the edit opened {:?} at {:?}",
                block.kind, block.range
            ));
        }
    }
    for top in old.iter().filter(|block| block.depth == 0) {
        let shifted = if top.range.end <= span_start && top.range.end <= edited.start {
            top.range.clone()
        } else if top.range.start >= edited.end && map(top.range.start) >= span_end {
            map(top.range.start)..map(top.range.end)
        } else {
            continue;
        };
        let was = blocks_within(&old, top.range.clone());
        let is = blocks_within(&new, shifted.clone());
        if was != is {
            return Err(format!(
                "block {:?} at {:?} changed (edit {edited:?}):\n  was {was:?}\n  is  {is:?}",
                top.kind, top.range
            ));
        }
    }
    Ok(())
}

/// The byte ranges of the lines of `text`, with their terminators.
fn line_ranges(text: &str) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let mut start = 0;
    text.split_inclusive('\n').map(move |line| {
        let range = start..start + line.len();
        start = range.end;
        range
    })
}

/// Whether `offset` is at or before the text of a list item's marker line
/// (in its quote markers, indentation, marker or padding).
fn before_item_text(text: &str, offset: usize) -> bool {
    let start = text[..offset].rfind(['\n', '\r']).map_or(0, |i| i + 1);
    let rest = &text[start..];
    let line = &rest[..rest.find(['\n', '\r']).unwrap_or(rest.len())];
    let b = line.as_bytes();
    let ws = |c: u8| c == b' ' || c == b'\t';
    let mut i = 0;
    while i < b.len() && (b[i] == b'>' || ws(b[i])) {
        i += 1;
    }
    if b.get(i).is_some_and(|c| b"-+*".contains(c)) {
        i += 1;
    } else {
        let digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 || !matches!(b.get(i + digits), Some(b'.' | b')')) {
            return false;
        }
        i += digits + 1;
    }
    if b.get(i).is_some_and(|&c| !ws(c)) {
        return false;
    }
    while i < b.len() && ws(b[i]) {
        i += 1;
    }
    offset - start <= i
}

/// Whether Enter at `offset` leaves an empty item right above a blank line
/// while the innermost old item at `offset` goes on after that blank line:
/// the caret is at the end of its line's text, the next line is blank (but
/// for quote markers), and the item ends after it. An item may begin with at
/// most one blank line, so the empty item ends there and CommonMark reads
/// the rest of the old item anew (an indented paragraph may become code, a
/// paragraph may take in the lines after the item lazily).
fn empty_item_before_blank_line(text: &str, blocks: &[BlockSpan], offset: usize) -> bool {
    let blank = |line: &str| line.chars().all(|c| c == '>' || c.is_whitespace());
    let line_end = text[offset..]
        .find('\n')
        .map_or(text.len(), |i| offset + i + 1);
    if !text[offset..line_end].trim().is_empty() {
        return false;
    }
    let next_end = text[line_end..]
        .find('\n')
        .map_or(text.len(), |i| line_end + i + 1);
    if next_end == line_end || !blank(&text[line_end..next_end]) {
        return false;
    }
    blocks
        .iter()
        .filter(|block| {
            block.kind == BlockKind::Item && block.range.start <= offset && offset < block.range.end
        })
        .max_by_key(|block| block.depth)
        .is_some_and(|item| item.range.end > next_end)
}

/// Whether the line at `offset` is blank (but for quote markers) and the
/// next line is not.
fn fills_blank_line(text: &str, offset: usize) -> bool {
    let blank = |line: &str| line.chars().all(|c| c == '>' || c.is_whitespace());
    let start = text[..offset].rfind(['\n', '\r']).map_or(0, |i| i + 1);
    let mut rest = text[start..].split_inclusive('\n');
    let (Some(line), next) = (rest.next(), rest.next()) else {
        return false;
    };
    blank(line) && next.is_some_and(|next| !blank(next))
}

fn run_case(
    text: &str,
    picks: &[(usize, u8)],
    command_index: usize,
    flavor: Flavor,
) -> Result<(), TestCaseError> {
    let under = command(command_index);
    let command = match under {
        Under::Command(command) => command,
        Under::LinkWithUrl => MarkdownCommand::InsertLink,
        // Quote is what callout is checked like.
        Under::Callout => MarkdownCommand::Quote,
    };
    let mut editor = Editor::with_config(
        Buffer::new(text),
        EditorConfig::default(),
        Box::new(gv_editor::ManualClock::new()),
    );
    let specs = selections(text, picks);
    editor.set_selections(&specs, specs.len() - 1);
    let edited: Rc<RefCell<Replacements>> = Rc::default();
    let sink = edited.clone();
    let observer = editor.observe(move |event| {
        if let EditorEvent::Edited(edit) = event {
            *sink.borrow_mut() = edit
                .summary
                .edits()
                .iter()
                .map(|e| (e.old.clone(), e.new.len()))
                .collect();
        }
    });
    let before_selections = editor.selections().specs();
    let snapshot = editor.snapshot();
    let tree = SyntaxTree::parse_with(&snapshot, flavor.parse_options());
    let commands = MarkdownCommands::new(flavor, CommandSettings::default());
    let syntax = Some(SyntaxRef::new(&tree, &snapshot));
    match under {
        Under::LinkWithUrl => {
            commands.insert_link(&mut editor, syntax, Some("https://example.com/a"));
        }
        Under::Callout => {
            let kind = gv_editor::commands::callout_types(flavor)
                .first()
                .copied()
                .unwrap_or("note");
            commands.insert_callout(&mut editor, syntax, kind);
        }
        Under::Command(command) => {
            commands.run(command, &mut editor, syntax);
        }
    }
    let after = editor.text().to_string();
    editor.unobserve(observer);
    let edited = edited.borrow().clone();
    let old_tree = SyntaxTree::parse_with(text, flavor.parse_options());
    let old_blocks = old_tree.blocks();
    // Enter, Tab and Shift+Tab fall back to plain text edits outside list
    // items and quotes and in code: their reinterpretation is the text's.
    let in_containers = specs.iter().all(|spec| {
        let at = spec.head;
        let inside = |block: &BlockSpan| {
            block.range.start <= at && at < block.range.end.max(block.range.start + 1)
        };
        old_blocks.iter().any(|block| {
            matches!(block.kind, BlockKind::Item | BlockKind::BlockQuote) && inside(block)
        }) && !old_blocks.iter().any(|block| {
            matches!(
                block.kind,
                BlockKind::FencedCode | BlockKind::IndentedCode | BlockKind::Html { .. }
            ) && inside(block)
        })
    });
    let marked: Vec<(usize, usize)> = specs.iter().map(|spec| (spec.tail, spec.head)).collect();
    let name = match under {
        Under::Callout => "insert_callout",
        _ => command.id(),
    };
    let label = format!("{flavor} {name} on {text:?} with {marked:?} gave {after:?}");
    editor
        .selections()
        .check(editor.text())
        .map_err(|e| TestCaseError::fail(format!("{label}: selections: {e}")))?;
    // Edits whose bytes happen to cancel out (Enter replacing a selected
    // line break while another caret adds one) are still one step.
    if after != text || !edited.is_empty() {
        prop_assert_eq!(editor.history().undo_len(), 1, "{}", label);
        prop_assert_eq!(editor.undo(), UndoOutcome::Undone, "{}", label);
        prop_assert_eq!(editor.text().to_string(), text, "{}: undo", label);
        prop_assert_eq!(
            editor.selections().specs(),
            before_selections,
            "{}: undo selections",
            label
        );
        prop_assert!(editor.redo(), "{}", label);
        prop_assert_eq!(editor.text().to_string(), after.clone(), "{}: redo", label);
    } else {
        prop_assert_eq!(editor.history().undo_len(), 0, "{}", label);
    }
    let crlf_only = text.contains("\r\n") && !text.replace("\r\n", "").contains('\n');
    if crlf_only {
        prop_assert!(
            !after.replace("\r\n", "").contains('\n'),
            "{}: a lone LF in a CRLF document",
            label
        );
    }
    // Enter replaces the selection: deleting a selected line break joins
    // lines, which may join blocks.
    let joins_lines = matches!(
        command,
        MarkdownCommand::Newline | MarkdownCommand::NewlinePlain
    ) && specs.iter().any(|spec| {
        text[spec.tail.min(spec.head)..spec.tail.max(spec.head)].contains(['\n', '\r'])
    });
    // A task box or link put on a blank line followed by text makes the
    // following lines continue its paragraph: the user's join.
    let fills_separator = matches!(
        command,
        MarkdownCommand::ToggleTask | MarkdownCommand::InsertLink
    ) && specs.iter().any(|spec| fills_blank_line(text, spec.head));
    // Enter before an item's text leaves an empty item above it, which
    // cannot interrupt a paragraph before the list (CommonMark).
    let empties_item = matches!(
        command,
        MarkdownCommand::Newline | MarkdownCommand::NewlinePlain
    ) && specs
        .iter()
        .any(|spec| before_item_text(text, spec.tail.min(spec.head)));
    // Enter at the end of an item's text above a blank line inside the item
    // leaves an empty item that ends there: the rest is read anew.
    let ends_item = command == MarkdownCommand::Newline
        && specs
            .iter()
            .any(|spec| empty_item_before_blank_line(text, &old_blocks, spec.tail.max(spec.head)));
    let plain = matches!(
        command,
        MarkdownCommand::Newline
            | MarkdownCommand::NewlinePlain
            | MarkdownCommand::Indent
            | MarkdownCommand::Outdent
    ) && !in_containers;
    // Commands that keep the container structure are checked strictly;
    // commands that change what a line is (a heading, a paragraph, a task
    // item, quoted) let CommonMark read the lines after it anew, so only
    // the blocks before it are checked. Inline toggles on a blank line
    // insert a bare delimiter pair (`~~~~`, `====`, `****`) that the next
    // keystroke completes: not checked.
    // Tab and Shift+Tab in a quote change what the paragraph is (quoted
    // one level deeper or less).
    // Both ends: a selection ending at a line start leaves that line out.
    let in_items = specs
        .iter()
        .flat_map(|spec| [spec.tail, spec.head])
        .all(|at| {
            old_blocks
                .iter()
                .filter(|block| {
                    matches!(block.kind, BlockKind::Item | BlockKind::BlockQuote)
                        && block.range.start <= at
                        && at < block.range.end.max(block.range.start + 1)
                })
                .max_by_key(|block| block.depth)
                .is_some_and(|block| block.kind == BlockKind::Item)
        });
    let mode = match command {
        MarkdownCommand::Bold
        | MarkdownCommand::Italic
        | MarkdownCommand::Strikethrough
        | MarkdownCommand::Highlight
        | MarkdownCommand::InlineCode => None,
        MarkdownCommand::Heading(_)
        | MarkdownCommand::Paragraph
        | MarkdownCommand::Quote
        | MarkdownCommand::ToggleTask
        | MarkdownCommand::InsertLink => Some(true),
        MarkdownCommand::Indent | MarkdownCommand::Outdent => Some(!in_items),
        MarkdownCommand::Newline => Some(ends_item),
        // Line commands rearrange, join or comment out lines: what the
        // lines become is the text's (a fence moved up opens a code block),
        // so only the undo, selection and line-ending properties apply.
        MarkdownCommand::MoveLineUp
        | MarkdownCommand::MoveLineDown
        | MarkdownCommand::DuplicateLine
        | MarkdownCommand::JoinLines
        | MarkdownCommand::SortLines
        | MarkdownCommand::ToggleComment => None,
        // Changing the case of letters never changes the block structure.
        MarkdownCommand::ChangeCase => Some(false),
        _ => Some(false),
    };
    if let Some(separated) =
        mode.filter(|_| !joins_lines && !plain && !fills_separator && !empties_item)
    {
        check_structure(flavor, text, &after, &edited, separated)
            .map_err(|e| TestCaseError::fail(format!("{label}: {e}")))?;
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 2000,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn commands_are_patches_and_undo_exactly(
        text in document(),
        picks in prop::collection::vec((any::<Index>(), any::<u8>()), 1..4),
        command_index in 0..=MarkdownCommand::ALL.len() + 1,
        flavor in prop::sample::select(Flavor::ALL.to_vec()),
    ) {
        let picks: Vec<(usize, u8)> = picks
            .iter()
            .map(|(index, len)| (index.index(text.len() + 1), *len))
            .collect();
        run_case(&text, &picks, command_index, flavor)?;
    }
}

/// Cases the properties once failed on.
#[test]
fn regressions() {
    let index = |id: &str| {
        MarkdownCommand::ALL
            .iter()
            .position(|command| command.id() == id)
            .expect("command")
    };
    // A table inserted after a lazy continuation line of a list item kept
    // no indentation: it closed the item, so the nested list after it
    // became a top-level list and merged with the next one.
    run_case(
        "* f\n==z== x\n   - three\n- a",
        &[(0, 0)],
        index("editor.table"),
        Flavor::GfmFile,
    )
    .unwrap();
    // A table on an empty item's line (`-` with nothing after it) went one
    // column short of the item's content: outside the item.
    for (text, at) in [
        (
            "- a\r\n# q\r\n10. k\r\n- bb cc\r\n-\r\n  1. nested\r\n1.\r\n- bb cc",
            24,
        ),
        ("\n-\n   l\n2. i\n**b** *i* `c`", 1),
    ] {
        run_case(text, &[(at, 0)], index("editor.table"), Flavor::GfmFile).unwrap();
    }
    // A table on a blank line inside an item went to column 0: it closed
    // the item, and the lines after it were read anew.
    for (text, at) in [
        ("- [ ] u\n  - d\n\n\n\tx\n==z== x\n---\n- a", 14),
        (
            "  1. nested\n1. h\n* f\n  - d\nr s t\n-\tw\n\n\tx\n==z== x\n\tx\n1.",
            37,
        ),
    ] {
        run_case(text, &[(at, 0)], index("editor.table"), Flavor::GfmFile).unwrap();
    }
    // Outdent of the line before a selection's end at an item's line start
    // acts outside the items.
    run_case(
        "     deep\n-\n   l\n==z== x\n2. i\n- a\n- a\n==z== x",
        &[(9, 2)],
        index("editor.outdent"),
        Flavor::CommonMark,
    )
    .unwrap();
    run_case(
        "\n\n\n\n",
        &[(4, 0), (0, 3)],
        index("editor.newline"),
        Flavor::CommonMark,
    )
    .unwrap();
}

/// The indices of the commands under test on table documents.
fn table_command_indices() -> Vec<usize> {
    MarkdownCommand::ALL
        .iter()
        .enumerate()
        .filter(|(_, command)| {
            matches!(
                command,
                MarkdownCommand::Table(_)
                    | MarkdownCommand::Indent
                    | MarkdownCommand::Outdent
                    | MarkdownCommand::InsertFootnote
            )
        })
        .map(|(index, _)| index)
        .collect()
}

/// The cells of every table row of `text` (the delimiter row's alignments
/// aside), trimmed, by the parser's rules.
fn table_cells(text: &str, flavor: Flavor) -> Vec<Vec<String>> {
    let tree = SyntaxTree::parse_with(text, flavor.parse_options());
    struct Rows<'a>(&'a str, Vec<Vec<String>>);
    impl gv_syntax::Visitor for Rows<'_> {
        fn leaf(&mut self, leaf: &gv_syntax::Leaf<'_>) {
            if leaf.kind != gv_syntax::LeafKind::Table {
                return;
            }
            for (index, line) in leaf.lines.iter().enumerate() {
                let end = self.0[line.content.start..]
                    .find(['\r', '\n'])
                    .map_or(self.0.len(), |i| line.content.start + i);
                let row = &self.0[line.content.start..end];
                if index == 1 {
                    continue;
                }
                let cells = gv_syntax::table::row_cells(row.as_bytes()).unwrap_or_default();
                self.1.push(
                    cells
                        .into_iter()
                        .map(|r| row[r].to_owned())
                        .filter(|cell| !cell.is_empty())
                        .collect(),
                );
            }
        }
    }
    let mut rows = Rows(text, Vec::new());
    tree.walk(&mut rows);
    rows.1
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 1000,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn table_commands_are_patches_and_undo_exactly(
        text in document_of(TABLE_LINES),
        picks in prop::collection::vec((any::<Index>(), any::<u8>()), 1..4),
        which in any::<Index>(),
        flavor in prop::sample::select(vec![Flavor::GfmFile, Flavor::Obsidian, Flavor::CommonMark]),
    ) {
        let picks: Vec<(usize, u8)> = picks
            .iter()
            .map(|(index, len)| (index.index(text.len() + 1), *len))
            .collect();
        // Carets must not split a character.
        let picks: Vec<(usize, u8)> = picks
            .into_iter()
            .map(|(mut at, len)| {
                while !text.is_char_boundary(at) {
                    at -= 1;
                }
                (at, len % 2)
            })
            .collect();
        let indices = table_command_indices();
        run_case(&text, &picks, indices[which.index(indices.len())], flavor)?;
    }

    #[test]
    fn formatting_is_idempotent_and_keeps_cells(text in document_of(TABLE_LINES)) {
        let flavor = Flavor::GfmFile;
        let format = MarkdownCommand::Table(gv_editor::commands::TableOp::Format);
        let commands = MarkdownCommands::new(flavor, CommandSettings::default());
        let mut editor = Editor::new(Buffer::new(&text));
        // A caret at every line start: every table is formatted at once.
        let carets: Vec<SelectionSpec> = line_ranges(&text)
            .map(|range| SelectionSpec::caret(range.start))
            .collect();
        editor.set_selections(&carets, 0);
        let in_table = commands.availability(
            MarkdownCommand::Table(gv_editor::commands::TableOp::DeleteRow),
            &editor,
            None,
        ) == gv_editor::Availability::Available;
        if in_table {
            commands.run(format, &mut editor, None);
            let once = editor.text().to_string();
            prop_assert_eq!(table_cells(&once, flavor), table_cells(&text, flavor));
            let carets: Vec<SelectionSpec> = line_ranges(&once)
                .map(|range| SelectionSpec::caret(range.start))
                .collect();
            editor.set_selections(&carets, 0);
            let outcome = commands.run(format, &mut editor, None);
            prop_assert_eq!(outcome, gv_editor::Outcome::Unchanged, "{:?} gave {:?}", text, once);
            prop_assert_eq!(editor.text().to_string(), once);
        }
    }
}

/// Every command on every caret position of a few nested documents.
#[test]
fn every_caret_of_nested_documents() {
    let docs = [
        "- a\n  - b\n    - c\n  - d\n- e\n",
        "1. a\n   1. b\n   2. c\n2. d\n10. e\n    f\n",
        "> - a\n>   > b\n> - c\n\npara\n",
        "-\ta\n\t-\tb\n-\tc\r\n",
        "- [ ] a\n  - [x] b\n- c\nlazy\n",
    ];
    for text in docs {
        for offset in 0..=text.len() {
            for command_index in 0..=MarkdownCommand::ALL.len() + 1 {
                for flavor in [Flavor::CommonMark, Flavor::Obsidian] {
                    if let Err(e) = run_case(text, &[(offset, 0)], command_index, flavor) {
                        panic!("{e}");
                    }
                }
            }
        }
    }
}

/// Enter at the end of an item whose text a blank line and more of the item
/// follow: the new empty item ends at the blank line, so the rest of the
/// old item is read anew (here an indented code block, then a paragraph that
/// takes in the following `8.` line).
#[test]
fn enter_before_a_blank_line_inside_an_item() {
    let text = ">\n-\tw\n\n    m\n==z== x\n8. eight\n- a\n10. k";
    let newline = MarkdownCommand::ALL
        .iter()
        .position(|&command| command == MarkdownCommand::Newline)
        .unwrap();
    if let Err(e) = run_case(text, &[(5, 104)], newline, Flavor::CommonMark) {
        panic!("{e}");
    }
}
