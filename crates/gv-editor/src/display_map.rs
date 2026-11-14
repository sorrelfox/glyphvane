// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The display map: layered coordinate transforms from buffer text to what
//! the editor shows.
//!
//! # Layers
//!
//! Each layer reads the snapshot of the layer below and turns its edits into
//! edits of its own text:
//!
//! | Layer | Module | Point | What it does |
//! |---|---|---|---|
//! | Buffer | `gv_text` | [`BufferPoint`] (line, byte column, `usize`) | The exact text, with LF, CRLF and lone CR line endings |
//! | Replace | [`replace_map`] | [`ReplacePoint`] | Folds and inline replacements (not yet: an identity layer); every line ending becomes one `\n` |
//! | Tab | [`tab_map`] | [`TabPoint`] | Expands hard tabs to tab stops |
//! | Wrap | [`wrap_map`] | [`WrapPoint`] | Soft wrap, measured through [`TextMeasure`], with continuation indents from a [`WrapIndent`] |
//! | Block | not yet | | Block widgets with pixel heights |
//!
//! Display-layer points are a `u32` row and a byte column in that layer's
//! text ([`Point`]). Every layer converts both ways with its neighbours and
//! clips invalid positions with a [`Bias`](gv_text::Bias).
//!
//! # Edits
//!
//! A buffer transaction returns an [`EditSummary`](gv_text::EditSummary) of
//! byte ranges. [`ReplaceMap::sync`] turns it into [`ReplaceEdit`]s and
//! [`TabMap::sync`] those into [`TabEdit`]s, and [`WrapMap::sync`] those into
//! [`WrapEdit`]s (whole wrap rows): in each, the old range is in the previous
//! snapshot of the layer and the new range in the next one.
//!
//! ```
//! use std::num::NonZeroU32;
//! use gv_editor::display_map::{ReplaceMap, TabMap, TabPoint};
//! use gv_text::Buffer;
//!
//! let mut buffer = Buffer::new("a\tb\r\n");
//! let tab_size = NonZeroU32::new(4).unwrap();
//! let (mut replace_map, replace_snapshot) = ReplaceMap::new(buffer.snapshot());
//! let (mut tab_map, tabs) = TabMap::new(replace_snapshot, tab_size);
//! assert_eq!(tabs.text(), "a   b\n");
//!
//! let summary = buffer.edit([(0..0, "xy")]).unwrap();
//! let (replace_snapshot, replace_edits) = replace_map.sync(buffer.snapshot(), &summary);
//! let (tabs, tab_edits) = tab_map.sync(replace_snapshot, replace_edits, tab_size);
//! assert_eq!(tabs.text(), "xya b\n");
//! assert_eq!(tab_edits[0].new, TabPoint::new(0, 0)..TabPoint::new(0, 4));
//! ```

pub mod line_wrapper;
pub mod primitives;
pub mod replace_map;
pub mod tab_map;
pub mod text_measure;
pub mod wrap_indent;
pub mod wrap_map;

#[cfg(test)]
mod test_support;

pub use line_wrapper::{Boundary, IndentAdjustment, LineFragment, LineWrapper};
pub use primitives::{Chunk, Edit, MAX_CHUNK_LEN, Point, TextSummary};
pub use replace_map::{ReplaceChunks, ReplaceEdit, ReplaceMap, ReplacePoint, ReplaceSnapshot};
pub use tab_map::{
    MAX_EXPANSION_COLUMN, MAX_TAB_SIZE, TabChunks, TabEdit, TabMap, TabPoint, TabPointCursor,
    TabSnapshot,
};
pub use text_measure::{MonospaceMeasure, TextMeasure};
pub use wrap_indent::{MarkdownHangingIndent, WrapIndent};
pub use wrap_map::{
    RewrapJob, WrapChunks, WrapEdit, WrapMap, WrapPoint, WrapPointCursor, WrapRow, WrapRowInfo,
    WrapRows, WrapSnapshot,
};

/// A buffer position: a line and a byte column (see [`gv_text::Point`]).
pub type BufferPoint = gv_text::Point;
