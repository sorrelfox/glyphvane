// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Text storage for Glyphvane: a rope with line, column and UTF-16 maps,
//! one-pass transactions, anchors, and the encoding model.
//!
//! # Types
//!
//! - [`Buffer`]: the editable text of one document plus its on-disk
//!   [`TextFormat`]. Applies [`Transaction`]s and returns an
//!   [`EditSummary`].
//! - [`TextSnapshot`]: the text at one [`Revision`]. Cloning is O(1) and it is
//!   `Send + Sync`, so the parser and other threads read snapshots while the
//!   buffer moves on. All queries and conversions live here.
//! - [`Anchor`]: a byte offset plus a [`Bias`], moved through edits with
//!   [`Anchor::transform`].
//! - [`TextSource`]: the line-oriented read interface of the parser,
//!   implemented for snapshots, `str` and `String`.
//! - [`encoding`]: [`decode`] file bytes into text and a [`TextFormat`], and
//!   [`encode`] it back byte for byte.
//!
//! # Coordinates
//!
//! The primary coordinate is the **byte offset** into the UTF-8 text
//! (`usize`). A *valid* offset is at most [`TextSnapshot::len`] and lies on a
//! character boundary. Derived coordinates:
//!
//! | Type | Meaning | Used by |
//! |---|---|---|
//! | [`Point`] | line and byte column | editor, display map |
//! | [`OffsetUtf16`] | UTF-16 code units from the start | IME, accessibility |
//! | [`PointUtf16`] | line and UTF-16 column | IME, LSP-style positions |
//!
//! Every conversion is O(log n).
//!
//! # Validation policy
//!
//! Positions from outside (IME and accessibility ranges, user input, other
//! revisions) are never used unchecked:
//!
//! - Conversions (`offset_to_*`, `*_to_offset`, `point_to_*`) return
//!   `Result<_, PositionError>` and reject offsets past the end, offsets
//!   inside a multi-byte character, UTF-16 offsets between the two halves of a
//!   surrogate pair, lines past the last line and columns past the end of the
//!   line.
//! - `clip_*` methods turn any input into a valid position: clamp to the text
//!   (or line), then move out of a character or surrogate pair in the
//!   direction of the [`Bias`]. Clipping a valid position returns it
//!   unchanged.
//! - Transactions are validated as a whole before anything changes: ranges
//!   in bounds, on character boundaries, sorted and not overlapping.
//!
//! The underlying rope (Zed's, vendored in `third_party/zed/rope`) would
//! otherwise clamp silently and drop characters split by a bad offset; this
//! crate is the only code that talks to it.
//!
//! # Line model
//!
//! A line ends after **LF**, **CRLF** or a **lone CR**, as in CommonMark. The
//! model is kept in its own index, independent of the rope (which only knows
//! LF):
//!
//! - The text `"a\r\nb\rc\n"` has four lines: `a`, `b`, `c` and an empty last
//!   line. A text always has at least one line; a text that ends with a
//!   terminator has an empty last line.
//! - Each line has a content range (without the terminator) and a terminator
//!   kind ([`LineEnding`], `None` for the last line).
//! - A [`Point`] column counts bytes from the line start. Valid columns run
//!   from 0 to the content length; for a CRLF line, the position between CR
//!   and LF (content length + 1) is also valid, because it is a character
//!   boundary. [`TextSnapshot::is_inside_crlf`] reports it, so the editor can
//!   keep the caret off it.
//! - Line endings are never converted. The text holds exactly what was
//!   decoded; [`TextSnapshot::line_ending_stats`] counts each kind.
//!
//! # Edits
//!
//! A [`Transaction`] is a list of byte-range replacements in the coordinates
//! of the text before it. [`Buffer::apply`] validates it and rebuilds the rope
//! and the line index in a single pass, so a transaction of k edits costs
//! about one edit plus O(k log n), not k separate edits. The returned
//! [`EditSummary`] lists every change as an old range and a new range; the
//! incremental parser and anchors consume it.

mod buffer;
mod coords;
mod edit;
pub mod encoding;
mod lines;
mod snapshot;
mod source;

pub use buffer::Buffer;
pub use coords::{Bias, OffsetUtf16, Point, PointUtf16, PositionError};
pub use edit::{Anchor, Edit, EditError, EditSummary, Replacement, Transaction};
pub use encoding::{
    Bom, DecodeError, DecodeStatus, Decoded, EncodeError, Encoding, LossReason, TextFormat, decode,
    encode,
};
pub use lines::{LineEnding, LineEndingStats, LineEndingStyle};
pub use snapshot::{Chunks, LineInfo, Lines, Revision, TextSnapshot};
pub use source::TextSource;

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn snapshot_is_send_and_sync() {
        assert_send_sync::<TextSnapshot>();
        assert_send_sync::<Buffer>();
    }

    #[test]
    fn snapshot_survives_edits_on_another_thread() {
        let mut buffer = Buffer::new("hello\nworld\n");
        let snapshot = buffer.snapshot();
        let reader = std::thread::spawn(move || snapshot.to_string());
        buffer
            .apply(&Transaction::new().insert(0, "> "))
            .expect("valid");
        assert_eq!(reader.join().expect("thread"), "hello\nworld\n");
        assert_eq!(buffer.to_string(), "> hello\nworld\n");
    }
}
