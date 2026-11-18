// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! GFM table commands: insert or format, Tab navigation, row and column
//! operations, column alignment and sorting.
//!
//! A table is the [`LeafKind::Table`] block at a caret; its rows are split
//! into cells by the parser's GFM rules ([`gv_syntax::table`]), so an
//! escaped `\|` stays inside its cell. Every command rewrites the whole
//! table formatted:
//!
//! - `| cell | cell |` rows, each column padded to its widest cell
//!   (at least 3 columns wide) by **display width**: East Asian wide and
//!   fullwidth characters and emoji take two columns
//!   ([`unicode_width`]);
//! - the delimiter row keeps each column's alignment colons; cells are
//!   padded on the side the alignment asks for (right-aligned cells on the
//!   left, centred cells on both sides);
//! - a body row with fewer cells than the header gets empty cells; cells
//!   beyond the header's count are kept after the others;
//! - each line keeps its container prefix (`> `, list indentation), and
//!   new lines take the prefix of the line next to them.
//!
//! The caret stays in its cell (at the same place in the cell's text), or
//! moves with the row or column it acted on.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ops::Range;

use gv_syntax::LeafKind;
use gv_syntax::table::{Alignment, delimiter_row, row_cells};
use unicode_width::UnicodeWidthStr;

use super::context::{LeafInfo, col_after, ctx_at};
use super::doc::Doc;
use crate::selection::SelectionSpec;

/// A table command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TableOp {
    /// Format the table at the caret; outside a table, insert one.
    Format,
    /// Insert an empty row above the caret's (body rows only).
    InsertRowAbove,
    /// Insert an empty row below the caret's (below the delimiter row on
    /// the header).
    InsertRowBelow,
    /// Insert an empty column left of the caret's.
    InsertColumnLeft,
    /// Insert an empty column right of the caret's.
    InsertColumnRight,
    /// Delete the caret's body row.
    DeleteRow,
    /// Delete the caret's column (not the last one).
    DeleteColumn,
    /// Swap the caret's body row with the one above.
    MoveRowUp,
    /// Swap the caret's body row with the one below.
    MoveRowDown,
    /// Swap the caret's column with the one on its left.
    MoveColumnLeft,
    /// Swap the caret's column with the one on its right.
    MoveColumnRight,
    /// Align the caret's column left (`:--`).
    AlignLeft,
    /// Centre the caret's column (`:-:`).
    AlignCenter,
    /// Align the caret's column right (`--:`).
    AlignRight,
    /// Remove the caret's column alignment (`---`).
    AlignNone,
    /// Sort the body rows by the caret's column: ascending, or descending
    /// when they already are ascending. Numbers compare as numbers when
    /// every cell of the column is one, text ignoring case.
    SortByColumn,
}

impl TableOp {
    /// Every table command.
    pub const ALL: [TableOp; 16] = [
        TableOp::Format,
        TableOp::InsertRowAbove,
        TableOp::InsertRowBelow,
        TableOp::InsertColumnLeft,
        TableOp::InsertColumnRight,
        TableOp::DeleteRow,
        TableOp::DeleteColumn,
        TableOp::MoveRowUp,
        TableOp::MoveRowDown,
        TableOp::MoveColumnLeft,
        TableOp::MoveColumnRight,
        TableOp::AlignLeft,
        TableOp::AlignCenter,
        TableOp::AlignRight,
        TableOp::AlignNone,
        TableOp::SortByColumn,
    ];

    /// The command id.
    pub fn id(self) -> &'static str {
        match self {
            TableOp::Format => "editor.table",
            TableOp::InsertRowAbove => "editor.table_insert_row_above",
            TableOp::InsertRowBelow => "editor.table_insert_row_below",
            TableOp::InsertColumnLeft => "editor.table_insert_column_left",
            TableOp::InsertColumnRight => "editor.table_insert_column_right",
            TableOp::DeleteRow => "editor.table_delete_row",
            TableOp::DeleteColumn => "editor.table_delete_column",
            TableOp::MoveRowUp => "editor.table_move_row_up",
            TableOp::MoveRowDown => "editor.table_move_row_down",
            TableOp::MoveColumnLeft => "editor.table_move_column_left",
            TableOp::MoveColumnRight => "editor.table_move_column_right",
            TableOp::AlignLeft => "editor.table_align_left",
            TableOp::AlignCenter => "editor.table_align_center",
            TableOp::AlignRight => "editor.table_align_right",
            TableOp::AlignNone => "editor.table_align_none",
            TableOp::SortByColumn => "editor.table_sort_by_column",
        }
    }
}

/// The display width of `text` in a monospace font: East Asian wide and
/// fullwidth characters and emoji (also ZWJ and modifier sequences) are two
/// columns.
pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// The table block at `offset`, if any.
pub(crate) fn table_leaf_at(doc: &Doc, offset: usize) -> Option<LeafInfo> {
    ctx_at(doc, offset)
        .leaf
        .filter(|leaf| leaf.kind == LeafKind::Table && leaf.lines.len() >= 2)
}

/// A caret's place in a table: line (0 the header, 1 the delimiter row,
/// then the body rows), column, and byte offset into the cell's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pos {
    line: usize,
    col: usize,
    intra: usize,
}

/// Where a caret goes after a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Target {
    pos: Pos,
    /// Select the cell's text instead of placing a caret.
    select: bool,
}

/// A table read from the text.
#[derive(Clone, Debug)]
struct Table {
    /// From the first line's start to the last line's content end.
    range: Range<usize>,
    /// Each line's container prefix.
    prefixes: Vec<String>,
    /// Each line's start of row text (after the prefix).
    row_starts: Vec<usize>,
    /// Each line's row text.
    row_texts: Vec<String>,
    /// The column alignments (the header's cell count).
    aligns: Vec<Alignment>,
    /// The header (row 0) and body rows; cells as written (escapes kept).
    rows: Vec<Vec<String>>,
    /// The line ending between rows.
    eol: &'static str,
}

impl Table {
    fn read(doc: &Doc, leaf: &LeafInfo) -> Option<Table> {
        let first = doc.line(leaf.lines.first()?.line);
        let last = doc.line(leaf.lines.last()?.line);
        let mut table = Table {
            range: first.start..last.end(),
            prefixes: Vec::new(),
            row_starts: Vec::new(),
            row_texts: Vec::new(),
            aligns: Vec::new(),
            rows: Vec::new(),
            eol: doc.line_ending_for(&first),
        };
        for (index, leaf_line) in leaf.lines.iter().enumerate() {
            let line = doc.line(leaf_line.line);
            let start = leaf_line.content.start.clamp(line.start, line.end());
            let mut split = start - line.start;
            while !line.text.is_char_boundary(split) {
                split -= 1;
            }
            let (prefix, row) = line.text.split_at(split);
            table.prefixes.push(prefix.to_owned());
            table.row_starts.push(line.start + split);
            table.row_texts.push(row.to_owned());
            let cells = || -> Vec<String> {
                row_cells(row.as_bytes())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|r| row[r].to_owned())
                    .collect()
            };
            if index == 1 {
                table.aligns = delimiter_row(row.trim().as_bytes())?;
            } else {
                table.rows.push(cells());
            }
        }
        let columns = table.aligns.len();
        if columns == 0 {
            return None;
        }
        for row in &mut table.rows {
            if row.len() < columns {
                row.resize(columns, String::new());
            }
        }
        Some(table)
    }

    fn columns(&self) -> usize {
        self.aligns.len()
    }

    /// The number of lines (header, delimiter and body rows).
    fn lines(&self) -> usize {
        self.rows.len() + 1
    }

    /// The caret at `offset` (on line `line` of the table).
    fn pos(&self, line: usize, offset: usize) -> Pos {
        let line = line.min(self.lines() - 1);
        let text = &self.row_texts[line];
        let rel = offset.saturating_sub(self.row_starts[line]).min(text.len());
        let cells = row_cells(text.as_bytes()).unwrap_or_default();
        let col = column_at(text, rel).min(cells.len().max(1) - 1);
        let intra = cells
            .get(col)
            .map_or(0, |cell| rel.clamp(cell.start, cell.end) - cell.start);
        Pos { line, col, intra }
    }

    /// Inserts an empty body row at table line `line` (≥ 2); its prefix is
    /// the one of the line before it.
    fn insert_row(&mut self, line: usize) {
        let prefix = self.prefixes[line - 1].clone();
        self.prefixes.insert(line, prefix);
        let width = self.rows[0].len().max(self.columns());
        self.rows.insert(line - 1, vec![String::new(); width]);
    }

    /// Applies `op` at `pos`; the caret's new place, `None` if the command
    /// does not apply there.
    fn apply(&mut self, op: TableOp, pos: Pos) -> Option<Target> {
        let columns = self.columns();
        let col = pos.col.min(columns - 1);
        let lines = self.lines();
        let at = |line: usize, col: usize| Target {
            pos: Pos {
                line,
                col,
                intra: 0,
            },
            select: false,
        };
        let target = match op {
            TableOp::Format => Target { pos, select: false },
            TableOp::InsertRowAbove => {
                if pos.line < 2 {
                    return None;
                }
                self.insert_row(pos.line);
                at(pos.line, col)
            }
            TableOp::InsertRowBelow => {
                let line = pos.line.max(1) + 1;
                self.insert_row(line);
                at(line, col)
            }
            TableOp::InsertColumnLeft | TableOp::InsertColumnRight => {
                let index = if op == TableOp::InsertColumnLeft {
                    col
                } else {
                    col + 1
                };
                self.aligns.insert(index, Alignment::None);
                for row in &mut self.rows {
                    if row.len() >= index {
                        row.insert(index, String::new());
                    }
                }
                at(pos.line, index)
            }
            TableOp::DeleteRow => {
                if pos.line < 2 {
                    return None;
                }
                self.rows.remove(pos.line - 1);
                self.prefixes.remove(pos.line);
                let last = self.lines() - 1;
                let line = if pos.line <= last { pos.line } else { last };
                at(if line == 1 { 0 } else { line }, col)
            }
            TableOp::DeleteColumn => {
                if columns < 2 {
                    return None;
                }
                self.aligns.remove(col);
                for row in &mut self.rows {
                    if col < row.len() {
                        row.remove(col);
                    }
                }
                at(pos.line, col.min(columns - 2))
            }
            TableOp::MoveRowUp => {
                if pos.line < 3 {
                    return None;
                }
                self.rows.swap(pos.line - 1, pos.line - 2);
                Target {
                    pos: Pos {
                        line: pos.line - 1,
                        ..pos
                    },
                    select: false,
                }
            }
            TableOp::MoveRowDown => {
                if pos.line < 2 || pos.line + 1 >= lines {
                    return None;
                }
                self.rows.swap(pos.line - 1, pos.line);
                Target {
                    pos: Pos {
                        line: pos.line + 1,
                        ..pos
                    },
                    select: false,
                }
            }
            TableOp::MoveColumnLeft | TableOp::MoveColumnRight => {
                let other = if op == TableOp::MoveColumnLeft {
                    col.checked_sub(1)?
                } else {
                    Some(col + 1).filter(|&c| c < columns)?
                };
                self.aligns.swap(col, other);
                for row in &mut self.rows {
                    row.swap(col, other);
                }
                Target {
                    pos: Pos { col: other, ..pos },
                    select: false,
                }
            }
            TableOp::AlignLeft
            | TableOp::AlignCenter
            | TableOp::AlignRight
            | TableOp::AlignNone => {
                self.aligns[col] = match op {
                    TableOp::AlignLeft => Alignment::Left,
                    TableOp::AlignCenter => Alignment::Center,
                    TableOp::AlignRight => Alignment::Right,
                    _ => Alignment::None,
                };
                Target { pos, select: false }
            }
            TableOp::SortByColumn => {
                sort_rows(&mut self.rows[1..], col);
                Target { pos, select: false }
            }
        };
        Some(target)
    }

    /// The cell after (`forward`) or before `pos`; Tab at the last cell
    /// appends a row.
    fn step(&mut self, pos: Pos, forward: bool) -> Target {
        let columns = self.columns();
        let col = pos.col.min(columns - 1);
        let to = |line: usize, col: usize| Target {
            pos: Pos {
                line,
                col,
                intra: 0,
            },
            select: true,
        };
        if forward {
            if pos.line != 1 && col + 1 < columns {
                return to(pos.line, col + 1);
            }
            let next = if pos.line == 0 { 2 } else { pos.line + 1 };
            if next >= self.lines() {
                self.insert_row(next);
            }
            to(next, 0)
        } else {
            if pos.line != 1 && col > 0 {
                return to(pos.line, col - 1);
            }
            match pos.line {
                0 => to(0, 0),
                1 | 2 => to(0, columns - 1),
                line => to(line - 1, columns - 1),
            }
        }
    }

    /// The table's text and the offsets (relative to its start) of `targets`.
    fn render(&self, targets: &[Target]) -> (String, Vec<SelectionSpec>) {
        let lines = render_lines(&self.aligns, &self.rows);
        let mut text = String::new();
        let mut line_starts = Vec::with_capacity(lines.len());
        for (index, (line, _)) in lines.iter().enumerate() {
            if index > 0 {
                text.push_str(self.eol);
            }
            text.push_str(&self.prefixes[index.min(self.prefixes.len() - 1)]);
            line_starts.push(text.len());
            text.push_str(line);
        }
        let specs = targets
            .iter()
            .map(|target| {
                let line = target.pos.line.min(lines.len() - 1);
                let (_, cells) = &lines[line];
                let cell = cells
                    .get(target.pos.col)
                    .or(cells.last())
                    .cloned()
                    .unwrap_or(0..0);
                let base = line_starts[line];
                if target.select {
                    SelectionSpec::new(base + cell.start, base + cell.end)
                } else {
                    let at = base + cell.start + target.pos.intra.min(cell.len());
                    SelectionSpec::caret(at)
                }
            })
            .collect();
        (text, specs)
    }
}

/// The column of byte `rel` in the row `text`: the number of unescaped
/// pipes before it, less the leading pipe.
fn column_at(text: &str, rel: usize) -> usize {
    let bytes = text.as_bytes();
    let lead = bytes.first() == Some(&b'|');
    let pipes = bytes[..rel.min(bytes.len())]
        .iter()
        .enumerate()
        .filter(|&(i, &b)| b == b'|' && (i == 0 || bytes[i - 1] != b'\\'))
        .count();
    if lead { pipes.saturating_sub(1) } else { pipes }
}

/// Sorts `rows` by column `col`: ascending, or descending if already
/// ascending.
fn sort_rows(rows: &mut [Vec<String>], col: usize) {
    let key = |row: &Vec<String>| row.get(col).map_or("", |s| s.as_str()).to_owned();
    let numeric = rows
        .iter()
        .all(|row| key(row).trim().parse::<f64>().is_ok());
    let compare = |a: &Vec<String>, b: &Vec<String>| -> Ordering {
        let (a, b) = (key(a), key(b));
        if numeric {
            let (x, y) = (
                a.trim().parse::<f64>().unwrap_or(0.0),
                b.trim().parse::<f64>().unwrap_or(0.0),
            );
            x.total_cmp(&y)
        } else {
            a.to_lowercase().cmp(&b.to_lowercase())
        }
    };
    let ascending = rows
        .windows(2)
        .all(|w| compare(&w[0], &w[1]) != Ordering::Greater);
    let varied = rows
        .windows(2)
        .any(|w| compare(&w[0], &w[1]) != Ordering::Equal);
    if ascending && varied {
        rows.sort_by(|a, b| compare(b, a));
    } else {
        rows.sort_by(compare);
    }
}

/// The formatted lines of a table (header, delimiter row, body rows) with
/// the byte range of each cell's text in its line.
fn render_lines(aligns: &[Alignment], rows: &[Vec<String>]) -> Vec<(String, Vec<Range<usize>>)> {
    let total = rows
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .max(aligns.len());
    let mut widths = vec![3usize; total];
    for row in rows {
        for (col, cell) in row.iter().enumerate() {
            widths[col] = widths[col].max(display_width(cell));
        }
    }
    let align = |col: usize| aligns.get(col).copied().unwrap_or(Alignment::None);
    let row_line = |row: &[String]| {
        let mut line = String::from("|");
        let mut cells = Vec::with_capacity(row.len());
        for (col, cell) in row.iter().enumerate() {
            let gap = widths[col].saturating_sub(display_width(cell));
            let (before, after) = match align(col) {
                Alignment::Right => (gap, 0),
                Alignment::Center => (gap / 2, gap - gap / 2),
                Alignment::Left | Alignment::None => (0, gap),
            };
            line.push(' ');
            line.extend(std::iter::repeat_n(' ', before));
            cells.push(line.len()..line.len() + cell.len());
            line.push_str(cell);
            line.extend(std::iter::repeat_n(' ', after));
            line.push_str(" |");
        }
        (line, cells)
    };
    let mut out = Vec::with_capacity(rows.len() + 1);
    out.push(row_line(&rows[0][..rows[0].len().min(aligns.len())]));
    let mut delimiter = String::from("|");
    let mut cells = Vec::with_capacity(aligns.len());
    for (col, &alignment) in aligns.iter().enumerate() {
        let width = widths[col];
        delimiter.push(' ');
        let start = delimiter.len();
        let (left, right) = match alignment {
            Alignment::None => (false, false),
            Alignment::Left => (true, false),
            Alignment::Right => (false, true),
            Alignment::Center => (true, true),
        };
        let dashes = width - usize::from(left) - usize::from(right);
        if left {
            delimiter.push(':');
        }
        delimiter.extend(std::iter::repeat_n('-', dashes));
        if right {
            delimiter.push(':');
        }
        cells.push(start..delimiter.len());
        delimiter.push_str(" |");
    }
    out.push((delimiter, cells));
    for row in &rows[1..] {
        out.push(row_line(row));
    }
    out
}

/// One table's carets: the table and the indices of the selections in it.
struct Group {
    leaf: LeafInfo,
    sels: Vec<usize>,
}

/// The tables at the selections, in document order.
fn groups(doc: &Doc) -> BTreeMap<usize, Group> {
    let mut groups: BTreeMap<usize, Group> = BTreeMap::new();
    for (index, spec) in doc.sels.iter().enumerate() {
        if let Some(leaf) = table_leaf_at(doc, spec.head) {
            groups
                .entry(leaf.range.start)
                .or_insert_with(|| Group {
                    leaf,
                    sels: Vec::new(),
                })
                .sels
                .push(index);
        }
    }
    groups
}

/// Whether a caret is in a table.
pub(crate) fn any_in_table(doc: &Doc) -> bool {
    doc.sels
        .iter()
        .any(|spec| table_leaf_at(doc, spec.head).is_some())
}

/// What to do with the carets of one table.
enum Action {
    /// A command at the lead caret.
    Op(TableOp),
    /// Tab (`true`) or Shift+Tab at every caret.
    Step(bool),
}

/// One table rewritten: its old range, its new length, and the new
/// selections (by index) relative to its start.
struct Placed {
    old: Range<usize>,
    new_len: usize,
    specs: Vec<(usize, SelectionSpec)>,
}

/// Runs `action` on every table at the selections: one replacement per
/// table that changes, and the new selections.
fn run(doc: &mut Doc, action: &Action) {
    let groups = groups(doc);
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut placed: Vec<Placed> = Vec::new();
    for group in groups.values() {
        let Some(mut table) = Table::read(doc, &group.leaf) else {
            continue;
        };
        let first_line = doc.line_of(table.range.start);
        let pos_of = |table: &Table, sel: usize| {
            let head = doc.sels[sel].head;
            table.pos(doc.line_of(head).saturating_sub(first_line), head)
        };
        let (sels, targets): (Vec<usize>, Vec<Target>) = match action {
            Action::Op(op) => {
                let lead = group
                    .sels
                    .iter()
                    .copied()
                    .find(|&sel| sel == doc.primary)
                    .unwrap_or(group.sels[0]);
                let pos = pos_of(&table, lead);
                let Some(target) = table.apply(*op, pos) else {
                    continue;
                };
                (group.sels.clone(), vec![target; group.sels.len()])
            }
            Action::Step(forward) => {
                // Positions first: a row appended for one caret must not
                // move the others.
                let positions: Vec<Pos> = group.sels.iter().map(|&s| pos_of(&table, s)).collect();
                let targets = positions
                    .into_iter()
                    .map(|pos| table.step(pos, *forward))
                    .collect();
                (group.sels.clone(), targets)
            }
        };
        let (text, specs) = table.render(&targets);
        let old = doc.slice(table.range.clone());
        let new_len = text.len();
        if old != text {
            edits.push((table.range.clone(), text));
        }
        placed.push(Placed {
            old: table.range.clone(),
            new_len,
            specs: sels.into_iter().zip(specs).collect(),
        });
    }
    doc.apply(edits);
    let mut delta: isize = 0;
    for Placed {
        old,
        new_len,
        specs,
    } in placed
    {
        let base = old.start.saturating_add_signed(delta);
        for (sel, spec) in specs {
            doc.sels[sel] = SelectionSpec::new(base + spec.tail, base + spec.head);
        }
        delta += new_len as isize - old.len() as isize;
    }
}

/// A table command at the selections. [`TableOp::Format`] outside every
/// table inserts a new table at each caret.
pub(crate) fn table_command(doc: &mut Doc, op: TableOp) {
    if op == TableOp::Format && !any_in_table(doc) {
        insert_table(doc);
        return;
    }
    run(doc, &Action::Op(op));
}

/// Tab (`forward`) or Shift+Tab in the tables at the selections.
pub(crate) fn step(doc: &mut Doc, forward: bool) {
    run(doc, &Action::Step(forward));
}

/// The continuation prefix of the line at `offset`: its container prefix
/// with list markers turned into spaces, the content start and whether the
/// line is blank after its prefix, plus the spaces the prefix needs past
/// the line's own to reach the innermost item's content column (a blank
/// line inside an item has no prefix of its own, and an empty item's
/// marker with nothing after it, `-`, is one column short).
fn continuation(doc: &Doc, offset: usize) -> (String, usize, bool, usize) {
    let ctx = ctx_at(doc, offset);
    let line = &ctx.line;
    let mut prefix: String = if ctx.lazy {
        String::new()
    } else {
        line.text[..ctx
            .content_start
            .saturating_sub(line.start)
            .min(line.text.len())]
            .chars()
            .map(|c| if c == '>' || c == '\t' { c } else { ' ' })
            .collect()
    };
    let mut pad = 0;
    if let Some(item) = ctx.chain.last().and_then(|level| level.item())
        && !ctx.lazy
        && ctx.blank
    {
        let target = item.base_col + item.content_offset;
        while col_after(prefix.as_bytes(), 0) < target {
            prefix.push(' ');
            pad += 1;
        }
    }
    (prefix, ctx.content_start, ctx.blank, pad)
}

/// Whether line `index` is blank (or past the end).
fn blank_line(doc: &Doc, index: usize) -> bool {
    index >= doc.line_count()
        || doc
            .line(index)
            .text
            .chars()
            .all(|c| c == '>' || c == ' ' || c == '\t')
}

/// Inserts a two-column table (an empty header, the delimiter row and one
/// empty row) at each caret: on a blank line in place, else after the
/// caret's block; blank lines separate it from the text around it. The
/// caret goes to the first header cell.
fn insert_table(doc: &mut Doc) {
    let mut inserts: BTreeMap<usize, (Range<usize>, String, usize)> = BTreeMap::new();
    for spec in &doc.sels {
        let ctx = ctx_at(doc, spec.head);
        let line = ctx.line.clone();
        let (prefix, content_start, blank, pad) = continuation(doc, spec.head);
        let eol = doc.line_ending_for(&line);
        let blank_prefix = prefix.trim_end().to_owned();
        let rows = ["|     |     |", "| --- | --- |", "|     |     |"];
        let body = |prefix: &str| {
            rows.iter()
                .enumerate()
                .map(|(i, row)| {
                    if i == 0 {
                        (*row).to_owned()
                    } else {
                        format!("{eol}{prefix}{row}")
                    }
                })
                .collect::<String>()
        };
        let (range, text, caret) = if blank {
            let mut text = String::new();
            if line.index > 0 && !blank_line(doc, line.index - 1) {
                text.push_str(eol);
                text.push_str(&prefix);
            } else {
                // The first row goes on the line itself, at the content
                // column.
                text.push_str(&" ".repeat(pad));
            }
            let caret = text.len() + 2;
            text.push_str(&body(&prefix));
            if !blank_line(doc, line.index + 1) {
                text.push_str(eol);
                text.push_str(&blank_prefix);
            }
            (content_start.max(line.start)..line.end(), text, caret)
        } else {
            let last = ctx
                .leaf
                .as_ref()
                .filter(|leaf| leaf.kind != LeafKind::Table)
                .map_or(line.index, |leaf| leaf.last_line.max(line.index));
            let last_line = doc.line(last);
            // A lazy last line has no container prefix of its own: the
            // table takes the prefix of the block's first line, so it stays
            // in the caret's list items and quotes (at column 0 it would
            // close them and the lines after it would be read anew).
            let prefix_line = if ctx_at(doc, last_line.start).lazy {
                ctx.leaf.as_ref().map_or(line.index, |leaf| leaf.first_line)
            } else {
                last
            };
            let (prefix, _, _, _) = continuation(doc, doc.line(prefix_line).start);
            let blank_prefix = prefix.trim_end().to_owned();
            let mut text = format!("{eol}{blank_prefix}{eol}{prefix}");
            let caret = text.len() + 2;
            text.push_str(&body(&prefix));
            if !blank_line(doc, last + 1) {
                text.push_str(eol);
                text.push_str(&blank_prefix);
            }
            (last_line.end()..last_line.end(), text, caret)
        };
        inserts.entry(range.start).or_insert((range, text, caret));
    }
    // Carets: each at its table's first header cell.
    let mut delta: isize = 0;
    let mut carets = Vec::with_capacity(inserts.len());
    for (range, text, caret) in inserts.values() {
        carets.push(range.start.saturating_add_signed(delta) + caret);
        delta += text.len() as isize - range.len() as isize;
    }
    let edits: Vec<(Range<usize>, String)> = inserts
        .into_values()
        .map(|(range, text, _)| (range, text))
        .collect();
    if doc.apply(edits).is_some() {
        doc.sels = carets.into_iter().map(SelectionSpec::caret).collect();
        doc.primary = doc.sels.len() - 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(aligns: &[Alignment], rows: &[&[&str]]) -> Vec<String> {
        let rows: Vec<Vec<String>> = rows
            .iter()
            .map(|row| row.iter().map(|s| (*s).to_owned()).collect())
            .collect();
        render_lines(aligns, &rows)
            .into_iter()
            .map(|(line, _)| line)
            .collect()
    }

    #[test]
    fn widths() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("日本"), 4);
        assert_eq!(display_width("ＡＢ"), 4);
        assert_eq!(display_width("😀"), 2);
        assert_eq!(display_width("👍🏽"), 2);
        assert_eq!(display_width("👩‍💻"), 2);
        assert_eq!(display_width("é"), 1);
        assert_eq!(display_width(r"a\|b"), 4);
    }

    #[test]
    fn columns_of_carets() {
        assert_eq!(column_at("| a | b |", 0), 0);
        assert_eq!(column_at("| a | b |", 2), 0);
        assert_eq!(column_at("| a | b |", 5), 1);
        assert_eq!(column_at("a | b", 0), 0);
        assert_eq!(column_at("a | b", 4), 1);
        assert_eq!(column_at(r"| a\|b | c |", 5), 0);
        assert_eq!(column_at(r"| a\|b | c |", 9), 1);
    }

    #[test]
    fn rendering() {
        use Alignment as A;
        assert_eq!(
            lines(
                &[A::None, A::Right, A::Center],
                &[&["a", "b", "c"], &["long", "1", "x"]]
            ),
            [
                "| a    |   b |  c  |",
                "| ---- | --: | :-: |",
                "| long |   1 |  x  |",
            ]
        );
        assert_eq!(
            lines(&[A::Left, A::None], &[&["日本", "😀"], &["x", r"a\|b"]]),
            ["| 日本 | 😀   |", "| :--- | ---- |", "| x    | a\\|b |"]
        );
        // Extra cells are kept.
        assert_eq!(
            lines(&[A::None], &[&["a"], &["b", "extra"]]),
            ["| a   |", "| --- |", "| b   | extra |"]
        );
    }

    #[test]
    fn sorting() {
        let mut rows: Vec<Vec<String>> = ["10", "9", "x"]
            .iter()
            .map(|s| vec![(*s).to_owned()])
            .collect();
        sort_rows(&mut rows[..2], 0);
        assert_eq!(rows[0][0], "9");
        sort_rows(&mut rows[..2], 0);
        assert_eq!(rows[0][0], "10");
        let mut rows: Vec<Vec<String>> = ["b", "A", "c"]
            .iter()
            .map(|s| vec![(*s).to_owned()])
            .collect();
        sort_rows(&mut rows, 0);
        let keys: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(keys, ["A", "b", "c"]);
    }
}
