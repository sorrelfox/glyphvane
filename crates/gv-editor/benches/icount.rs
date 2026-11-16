// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Instruction-count benchmarks (Callgrind through gungraun), the CI gate
//! for the editor keystroke. Run them with `cargo xtask bench-icount`
//! (Linux, needs Valgrind and the matching `gungraun-runner`).
//!
//! Each case runs once under Callgrind; only the benchmark function is
//! counted: one keystroke through the editor (rope edit, selections, undo
//! grouping and the display-map sync), not building the document and the
//! editor.
//!
//! - `keystroke::insert` (`unwrapped`, `wrapped`): one character typed at the
//!   end of the line in the middle of the document;
//! - `keystroke::delete` (`unwrapped`, `wrapped`): that character deleted
//!   again with Backspace.
//!
//! Document: the 1 MB Markdown-like document of the `wrap` bench (see
//! `common`), unwrapped or soft-wrapped at 80 columns.

#[cfg(target_os = "linux")]
mod common;

#[cfg(target_os = "linux")]
mod benches {
    use std::hint::black_box;

    use gungraun::{library_benchmark, library_benchmark_group};
    use gv_editor::{DeleteUnit, Editor};

    use crate::common::{document, make_editor};

    const DOC_LEN: usize = 1 << 20;

    /// The editor before the keystroke, wrapped at 80 columns or not.
    pub fn editor(wrapped: bool) -> Editor {
        make_editor(&document(DOC_LEN), wrapped)
    }

    /// The editor after [`insert`] typed its character.
    pub fn editor_with_char(wrapped: bool) -> Editor {
        let mut editor = editor(wrapped);
        editor.insert_at_selections("x");
        editor
    }

    // The editor is returned so that freeing it is not counted.

    #[library_benchmark]
    #[bench::unwrapped(args = (false), setup = editor)]
    #[bench::wrapped(args = (true), setup = editor)]
    fn insert(mut editor: Editor) -> Editor {
        black_box(editor.insert_at_selections(black_box("x")));
        editor
    }

    #[library_benchmark]
    #[bench::unwrapped(args = (false), setup = editor_with_char)]
    #[bench::wrapped(args = (true), setup = editor_with_char)]
    fn delete(mut editor: Editor) -> Editor {
        black_box(editor.delete_backward(black_box(DeleteUnit::Grapheme)));
        editor
    }

    library_benchmark_group!(name = keystroke, benchmarks = [insert, delete]);
}

#[cfg(target_os = "linux")]
use benches::keystroke;

#[cfg(target_os = "linux")]
gungraun::main!(library_benchmark_groups = keystroke);

/// Valgrind-based benchmarks run on Linux only.
#[cfg(not(target_os = "linux"))]
fn main() {}
