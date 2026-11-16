# Provenance of `third_party/zed/`

Code in this directory comes from the [Zed](https://github.com/zed-industries/zed)
editor. It is licensed under the GNU General Public License, version 3 or
later (`GPL-3.0-or-later`), the same licence as Glyphvane. The licence text
Zed ships is in [`LICENSE-GPL`](LICENSE-GPL).

Copyright for this code remains with Zed Industries, Inc. and the Zed
contributors.

| | |
|---|---|
| Upstream repository | https://github.com/zed-industries/zed |
| Upstream commit | `d2a4fb6ded7c470be25d1541281da3882495b65b` (2026-11-08) |
| Why this commit | It is the commit the `gpui-pre 0.3.12` crates snapshot (their manifests record it as `zed-rev`). Glyphvane's GPUI pin resolves to `gpui-pre =0.3.12`, so the vendored code and the `gpui-pre-sum-tree` crate it builds on come from the same Zed tree |
| Imported | 2026-11-13 |

## `rope/`: Zed's `crates/rope`

| Path here | Upstream path | Changes |
|---|---|---|
| `rope/src/rope.rs` | `crates/rope/src/rope.rs` | none |
| `rope/src/chunk.rs` | `crates/rope/src/chunk.rs` | none |
| `rope/src/offset_utf16.rs` | `crates/rope/src/offset_utf16.rs` | none |
| `rope/src/point.rs` | `crates/rope/src/point.rs` | none |
| `rope/src/point_utf16.rs` | `crates/rope/src/point_utf16.rs` | none |
| `rope/src/unclipped.rs` | `crates/rope/src/unclipped.rs` | none |
| `rope/benches/rope_benchmark.rs` | `crates/rope/benches/rope_benchmark.rs` | none |
| `rope/Cargo.toml` | `crates/rope/Cargo.toml` | adapted, see below |
| `LICENSE-GPL` | `LICENSE-GPL` (repository root; `crates/rope/LICENSE-GPL` is a symlink to it) | none |

SHA-256 of the unmodified files:

```
0993c2d71044c047c124da61ea277af5f3d542c308fbb8a1d8ae2a04a0c20dc5  rope/src/rope.rs
7256682714925f16c61026010cb59e267d8aa30943b9cc8b0a52efeb6b766084  rope/src/chunk.rs
c4c6a8ea367539878dcecc31276d4b6c53fc0398cb6b06fc73317817e2f880c7  rope/src/offset_utf16.rs
720fd4d3988c4323aa95407b2597657793103324b1aa387e0c0505de97b44783  rope/src/point.rs
a2cfefe1a99f44072730fb8961ba8b82d3e4c3a88bf04d9faafacca4430c69fe  rope/src/point_utf16.rs
b59c90acae03b9b2569e53e56ad2f6c72923186e8eb3ddb7a2bc28119be0ef59  rope/src/unclipped.rs
cf16b3a5a714d6e53bd2324f1d43963946fc9e34958684a41e4d64de4bdb5ceb  rope/benches/rope_benchmark.rs
0aa0322ea494da441088e4d9144c6720bf99d9e2e1eefcd29d1f58b19b5c4246  LICENSE-GPL
```

### Changes to `rope/Cargo.toml`

The upstream manifest takes everything from Zed's workspace. Here it builds
inside the Glyphvane workspace against published crates:

1. `edition` and `publish` (upstream `edition.workspace`, `publish.workspace`)
   are written out with Zed's workspace values: `edition = "2024"`,
   `publish = false`.
2. `[lints] workspace = true` is replaced by a copy of Zed's
   `[workspace.lints]` table at the same commit, so the crate keeps its
   upstream lint levels instead of Glyphvane's stricter ones.
3. Dependencies keep `workspace = true`; the Glyphvane root `Cargo.toml`
   maps the Zed workspace names to published crates:
   - `sum_tree` → `gpui-pre-sum-tree =0.3.12`
   - `gpui_util` → `gpui-pre-util =0.3.12`
   - `ztracing` → `gpui-pre-ztracing =0.3.12`
   - `heapless`, `log`, `rayon`, `tracing`, `unicode-segmentation`, `ctor`,
     `rand`, `criterion`: the same crates and version requirements as Zed's
     workspace (`criterion` without its default features).

   All of these are byte-identical to the Zed sources at the commit above
   (checked with `diff -r` for the `gpui-pre-*` crates).
4. Dev-dependencies used only by the upstream tests and benchmark:
   - `gpui` (for the `#[gpui::test]` macro) is `gpui-pre =0.3.12` with the
     `test-support` feature, renamed to `gpui`. It is a dev-dependency only;
     the rope does not depend on GPUI.
   - `util` and `zlog` are Zed workspace crates that are not published. They
     are replaced by the test shims below.
5. A header comment pointing here.

## `test-shims/`: stand-ins for unpublished Zed test helpers

Used only as dev-dependencies of `rope`. Never shipped.

| Path here | Package | Library name | Contents |
|---|---|---|---|
| `test-shims/util/` | `zed-test-util` | `util` | `RandomCharIter`, copied verbatim from Zed `crates/util/src/util.rs` lines 602–655 at the commit above (behind the same `test-support` feature) |
| `test-shims/zlog/` | `zed-test-zlog` | `zlog` | A no-op `init_test()`, written for Glyphvane. Upstream it installs a logger when `RUST_LOG` is set |

## Tests

The 25 upstream tests run as part of `cargo test --workspace`:

```sh
cargo test -p rope
```

## Updating

When the GPUI pin moves to a new `gpui-pre` version:

1. Read the new `zed-rev` from the `gpui-pre-sum-tree` manifest
   (`[package.metadata.gpui-pre]`).
2. Copy `crates/rope/src` and `crates/rope/benches` from that commit over
   the files here, and compare `crates/rope/Cargo.toml` and Zed's
   `[workspace.lints]` with the adapted manifest.
3. Update the commit, the hashes and the version pins in this file and in
   the root `Cargo.toml`, and record the size of the upstream diff.
4. Run `cargo test -p rope -p gv-text`.

## Display map: ported into `crates/gv-editor/src/display_map/`

Parts of Zed's `crates/editor` display map are ported into `gv-editor`
rather than vendored here: they are rewritten against Glyphvane's buffer
(`gv-text`) and layer types, so they live in that crate. Each ported file
starts with Glyphvane's SPDX header, followed by a notice naming the upstream
file, the commit above, Zed's copyright, the licence (GPL-3.0-or-later) and a
summary of the changes. This section is the full record.

| File in `crates/gv-editor/` | Upstream (commit above) | Kind | Ported |
|---|---|---|---|
| `src/display_map/tab_map.rs` | `crates/editor/src/display_map/tab_map.rs` | ported, with all 20 upstream tests | 2026-11-13 |
| `src/display_map/primitives.rs` | `Point` arithmetic: `crates/rope/src/point.rs`; `Edit<D>`: `crates/text/src/text.rs`; `TextSummary`: `crates/editor/src/display_map/tab_map.rs` with the scan of `From<&str>` in `crates/rope/src/rope.rs`; `Chunk`: `crates/editor/src/display_map/fold_map.rs` | derived: small types reduced to the fields and operations the layers use | 2026-11-13 |

Written for Glyphvane, not taken from Zed: `src/display_map.rs`,
`src/display_map/replace_map.rs` (the replace layer, an identity layer until
folds and inline replacements are ported), `src/display_map/text_measure.rs`,
`src/display_map/test_support.rs` and `tests/display_map.rs`.

Not ported yet: `invisibles.rs` and `dimensions.rs` (the tab map does not use
them), the merged `inlay_map.rs` + `fold_map.rs` replace layer, and the
`wrap_map.rs` core.

SHA-256 of the upstream files at the commit above, to diff against on the
next re-sync:

```
b07a6b6146a79d8cf6ab3717365189266ff087a407330e9f5a5a7dcfcb60077d  crates/editor/src/display_map/tab_map.rs
64f304edcb794326df50a48bc155a0f5ebdd03e3f2c8ce84ed806e8139aa4631  crates/editor/src/display_map/fold_map.rs
569ffbf9f529e7628a0961dbd2f6ff4e73c4994558c3a76649160d1c6648f14d  crates/text/src/text.rs
720fd4d3988c4323aa95407b2597657793103324b1aa387e0c0505de97b44783  crates/rope/src/point.rs
0993c2d71044c047c124da61ea277af5f3d542c308fbb8a1d8ae2a04a0c20dc5  crates/rope/src/rope.rs
```

### Modifications to `tab_map.rs`

1. **Input layer.** Zed's fold map (`FoldSnapshot`, `FoldPoint`,
   `FoldOffset`, `FoldChunks`, `FoldEdit`) is replaced by gv-editor's replace
   layer (`ReplaceSnapshot`, `ReplacePoint`, `ReplaceChunks`, `ReplaceEdit`).
   The replace layer's text turns every line terminator (LF, CRLF, lone CR)
   into one `\n` and has no offset coordinate, so:
   - `sync` takes the input edits as `Edit<ReplacePoint>`. Extending an edit
     to the next tab adds to the end column instead of the end offset (the
     tab is on the same row, so the result is the same);
   - `chunks`, `TabChunks::seek` and `TabStopCursor::seek_to` take point
     ranges instead of offset ranges.
2. **Base coordinates.** Multi-buffer `Point` and `MultiBufferRow` become
   `BufferPoint` (`gv_text::Point`: `usize` line and byte column).
   `point_to_tab_point` / `tab_point_to_point` are renamed
   `buffer_point_to_tab_point` / `tab_point_to_buffer_point` and clip their
   input; `buffer_row_to_tab_row(MultiBufferRow)` becomes
   `buffer_line_to_tab_row(usize)`; `fold_point_to_tab_point` /
   `tab_point_to_fold_point` are renamed `replace_point_to_tab_point` /
   `tab_point_to_replace_point`. `Bias` is `gv_text::Bias` (was
   `sum_tree::Bias`). `Point`, `Edit`, `TextSummary` and `Chunk` come from
   `primitives.rs` (was `language`, `text` and `fold_map`).
3. **Removed:** the `LanguageAwareStyling` and `Highlights` parameters of
   `chunks` (no highlighting yet); `rows()` (multi-buffer row infos);
   `Deref<Target = FoldSnapshot>` on `TabSnapshot`; every
   `#[ztracing::instrument]` attribute.
4. **No `unsafe`.** `str::from_utf8_unchecked(&SPACES[..n])` becomes
   `&SPACES[..n]` on a `&str` of 128 spaces built in a `const`.
   `rope::Chunk::MASK_BITS` becomes `MAX_CHUNK_LEN`; `MAX_TABS` is renamed
   `MAX_TAB_SIZE`; it and `MAX_EXPANSION_COLUMN` are public.
5. **Visibility.** `TabSnapshot`'s fields are private, with accessors
   (`replace_snapshot`, `buffer`, `tab_size`, `max_expansion_column`,
   `version`). `chunks` and `text` are public (were `pub(crate)` and
   test-only).
6. **`set_max_expansion_column`** is public (was `#[cfg(test)]`), bumps the
   version and returns `(TabSnapshot, Vec<TabEdit>)` with one edit over the
   whole text (none if the column does not change).
7. **Merging extended edits.** The in-place `scan` + `filter_map` (with its
   `debug_assert` that the allocation is reused) is a plain loop with the
   same result; the `unwrap()` of the first edit is gone.
8. **Inverted ranges.** `chunks` and `TabChunks::seek` treat a range that
   ends before it starts as empty (upstream would underflow).
9. **Lints and docs.** `Debug` on every type (workspace lint
   `missing_debug_implementations`), doc comments on the public API, and
   `#[expect(clippy::needless_update)]` on `TabChunks::next`, whose
   `..self.chunk.clone()` carries chunk fields that Glyphvane's `Chunk` does
   not have yet.

### Modifications to the upstream tests

All 20 tests are kept, in the `tests` module of `tab_map.rs`.

1. `#[gpui::test]` becomes `#[test]`. `#[gpui::test(iterations = 100)]`
   becomes `for_each_seed(100, …)`: seeds 0 to 99 of `StdRng`, or the one in
   `SEED`; the failing seed is printed.
2. The layers are built as `TextSnapshot → ReplaceMap → TabMap` instead of
   `MultiBuffer → InlayMap → FoldMap → TabMap`. The expected-text model is a
   string (`TestText`) instead of `text::Rope`, and buffer edits are
   `gv_text::Buffer` transactions (`random_edit` follows Zed's
   `Buffer::randomly_edit`).
3. The replace layer has no folds or inlays yet, so the random fold and inlay
   mutations in `test_random_tabs`, `test_to_tab_point_random`,
   `test_random_tab_point_cursor` and `test_random_tab_map_sync` are dropped
   or replaced by random buffer edits. Restore them when the replace layer is
   ported. `MultiBuffer::build_random` becomes a random text of up to 200
   characters.
4. `test_empty_chunk_after_leading_tab_trim` gets its one-byte tab chunk
   from a lone CR (where the replace layer splits chunks) instead of a fold,
   and also checks the resulting text.
5. `test_to_fold_point_panic_reproduction` is renamed
   `test_to_replace_point_panic_reproduction` and its unused `_text` is
   removed.
6. `test_tab_stop_cursor_random_utf16` takes the expected tab stops from the
   replace layer's text (line endings normalized), as upstream takes them
   from Zed's buffer, which normalizes line endings to LF.
7. `pretty_assertions::assert_eq` becomes `assert_eq`, and the expected tab
   stops of the fixed-text tests come from one helper.
8. `RandomCharIter` comes from the `zed-test-util` shim above (a
   dev-dependency of `gv-editor`).

### Updating the display-map ports

1. Diff the upstream files above between the recorded commit and the new
   `zed-rev`, and check the hashes.
2. Port the changes by hand; mark new modifications in the file header and
   in this section.
3. Update the commit and hashes here, and record the size of the upstream
   diff.
4. Run `cargo test -p gv-editor`.

## Display map: the wrap layer (ported 2026-11-14)

The `wrap_map.rs` core is now ported, which supersedes "not ported yet" for
it in the section above. With it comes the part of GPUI's line wrapper that
finds the wrap boundaries. Same commit as above.

| File in `crates/gv-editor/` | Upstream (commit above) | Licence upstream | Kind |
|---|---|---|---|
| `src/display_map/wrap_map.rs` | `crates/editor/src/display_map/wrap_map.rs` | GPL-3.0-or-later | ported, with 5 of its 6 upstream tests |
| `src/display_map/line_wrapper.rs` | `crates/gpui/src/text_system/line_wrapper.rs` (`LineWrapper::wrap_line`, `LineFragment`, `Boundary`, `IndentAdjustment`, the width cache) | **Apache-2.0** | ported, with its 6 wrapping tests |

Written for Glyphvane, not taken from Zed: `src/display_map/wrap_indent.rs`
(the per-row indent callback and the Markdown hanging indent), the soft-wrap
parts of `src/editor.rs` (the wrap layer in `DisplayLayers`, `DisplayRows`
and the wrap-row motions), the `ColumnMeasure` test double in
`src/display_map/test_support.rs`, `tests/wrap_map.rs`,
`tests/editor_wrap.rs` and `benches/wrap.rs`.

### Licence of `line_wrapper.rs`

GPUI is licensed under the Apache License, Version 2.0
(`crates/gpui/Cargo.toml`: `license = "Apache-2.0"`; `crates/gpui/LICENSE-APACHE`
is a symlink to Zed's root `LICENSE-APACHE`). Apache-2.0 code may be included
in a GPL-3.0 work. The ported file:

- is distributed under GPL-3.0-or-later as part of Glyphvane (its SPDX
  line), and its header names the upstream file, the commit, Zed's
  copyright and the Apache-2.0 licence of the original;
- states that it was modified, and this section lists the changes
  (Apache-2.0 § 4(b));
- keeps the upstream comments; Zed ships no `NOTICE` file, so there is no
  notice text to carry (§ 4(d)).

Apache-2.0 § 4(a) asks for a copy of the licence with the code: Zed's
`LICENSE-APACHE` (hash below) belongs beside `LICENSE-GPL` in this
directory.

SHA-256 of the upstream files at the commit above:

```
f5d8ee0f98662d65c52dc3d6ce0e7d0d294760ab1edce62e6bac31b0b582baec  crates/editor/src/display_map/wrap_map.rs
8912c5d16dc00bbcc1f19f56c2f2f4e3b826aa244b4f7c04c62a3934b22f7c4f  crates/gpui/src/text_system/line_wrapper.rs
752daf2fb234ca4a1fa372c073fe127f44b7b90fd2529ae44273a64f9d53da7a  LICENSE-APACHE
```

### Modifications to `wrap_map.rs`

1. **Input layer and coordinates.** The layer sits on gv-editor's
   `TabSnapshot` (over the replace layer and gv-text) instead of Zed's tab
   map over the fold map and multi-buffer. `make_wrap_point(Point, Bias)`
   and `to_point` are renamed `buffer_point_to_wrap_point(BufferPoint,
   Bias)` and `wrap_point_to_buffer_point`. The public API takes
   `gv_text::Bias`; `sum_tree::Bias` is used only for tree seeks.
   `Deref<Target = TabSnapshot>` is replaced by `tab_snapshot()` and
   `buffer()`.
2. **Measurement.** Wrapping goes through the ported `LineWrapper` over an
   injected `TextMeasure`; `Font`, `FontId`, `Pixels` and `TextSystem` are
   gone and widths are `f32`. `set_font_with_size` becomes
   `set_measure(Box<dyn TextMeasure>)`; `WrapMap::new` takes the measure.
3. **Scheduling.** `WrapMap` is a plain struct, not a GPUI entity: no
   `background_spawn`, `block_with_timeout`, `cx.notify()` or `yield_now`,
   and `update` is synchronous. A full re-wrap is a `RewrapJob` (new):
   `begin_rewrap` / `defer_wrap_width` hand out a `Send` job that wraps rows
   in steps with its own `LineWrapper`, so a view can run it on another
   thread; `finish_rewrap` completes it, re-applies the tab edits synced
   meanwhile with `update` (upstream's `pending_edits`), and reports **one
   edit over the whole text** instead of composing
   `interpolated_edits.invert()` with the job's edits, so `text::Patch` is
   not needed. A job from before a newer re-wrap is rejected (generation
   count). The setters (`set_wrap_width`, `set_measure`, `set_indent`) run a
   job to completion at once.
4. **`sync`.** While a job is out it interpolates, as upstream does while
   its background task runs; otherwise it re-wraps the touched rows at
   once, also when there is no width (upstream interpolates then), so the
   unwrapped layer equals a rebuild exactly, longest row included.
5. **Soft-wrap indent.** The `SoftWrapIndent` language setting and
   `indent_adjustment_for` become a per-row `WrapIndent` callback that
   returns an `IndentAdjustment` for the row's tab-expanded text
   (`with_indent`, `set_indent`; `set_soft_wrap_indent` is gone).
   `refresh_rows` (new) re-wraps rows whose answer changed.
6. **Capped transforms.** Isomorphic transforms are merged only up to 32
   rows (`MAX_ISOMORPHIC_ROWS`), so they split at row starts. Summarizing
   a range of the tab layer scans its text here (the replace layer has no
   summary tree yet), and upstream re-summarizes the rest of the
   transform an edit falls in, which in unwrapped text can be the rest of
   the document. For the same reason the fast path of `interpolate` for a
   text without wraps (which summarizes the whole new text) is removed.
   `check_invariants` allows adjacent isomorphic transforms (wraps still
   never follow each other) and checks the row infos from six start rows
   instead of every row (quadratic).
7. **Lines that fit** skip the line wrapper (`fits`): same result, since
   the wrapper breaks only once the running width exceeds the wrap width,
   and widths are summed in the same order.
8. **Chunks.** A wrap's display text (up to 257 bytes, upstream yielded
   whole with empty bitmaps) comes in chunks of at most 128 bytes with
   their bitmaps. The `\n` + 256 spaces text is a `const` (was a
   `LazyLock`). `Transform::wrap` clamps the indent to `MAX_INDENT`.
9. **Removed:** `LineFragmentBuilder` (invisible characters as elements of
   their replacement glyph's width: the `TextMeasure` answers for the glyph
   the view draws instead), chunk renderer widths, the
   `LanguageAwareStyling` and `Highlights` parameters of `chunks`, every
   `#[ztracing::instrument]`. `WrapRows` yields `WrapRowInfo { tab_row,
   buffer_line, soft_wrapped }` instead of multi-buffer `RowInfo`.
10. **Row arithmetic.** `RowDelta` and `impl_for_row_types!` are replaced
    by `u32` arithmetic on `WrapRow.0` and `Add for WrapRow`. `WrapEdit` is
    `primitives::Edit<WrapRow>` and patches are `Vec<WrapEdit>`.
    `consolidate_wrap_edits` is a plain loop (upstream scans in place and
    asserts the allocation is reused).
11. **Visibility and small changes.** `text` and `text_chunks` are public
    (were test-only); `is_interpolated`, `wrap_width`, `into_measure` are
    new accessors; `clip_point` saturates the column it steps back;
    `Transform::isomorphic` uses `debug_assert!` (was a test-only
    `assert!`); `unwrap()`s on the first edit become matches; `Debug` on
    every type and doc comments on the public API.

### Modifications to `line_wrapper.rs`

1. **Widths** come from `TextMeasure::char_width` through the same cache
   (ASCII array and hash map) instead of
   `TextSystem::layout_width(font_id, font_size, c)`. `LineWrapper::new`
   takes the measure; `into_measure` gives it back; `width_for_char` is
   public; `Pixels` is `f32`.
2. **Break opportunities** come from `unicode-linebreak` 0.1.5 (UAX #14,
   Unicode 15.0, already a workspace dependency) instead of ICU4X's
   `LineSegmenter::new_for_non_complex_scripts` (`icu_segmenter` 2.x). It
   does not report the opportunity at index 0, which could never become a
   candidate, and opportunities that no candidate starts at are skipped
   instead of blocking later ones. All six upstream wrapping tests pass
   unchanged with it, including the UAX #14 cases (`required!`,
   `foo/bar`, `(hello)`, `a\u{a0}b`, `你好！`) and the grapheme tests.
   Differences between the two segmenters in Unicode 15.1+ rules are not
   checked.
3. **`IndentAdjustment::Columns(u32)`** (new): an absolute continuation
   indent, with the same overflow guard as `ExtraColumns`.
4. **`SameIndent` overflow guard** (new): no indent when the text carried
   to the continuation row would not fit after it. Upstream keeps the
   indent, and the row then overflows with more than one grapheme (found by
   the property test; `same_indent_overflow_guard` has the example). No
   upstream test changes.
5. **Not ported:** truncation (`TruncateFrom`, `should_truncate_line*`,
   `truncate_line*`, `update_runs_after_*`), `into_char_widths`, and the
   crate-private constructor. `break_indices` is a free function.
6. `if base_indent.is_none() { if let … }` becomes a let chain; `Debug`.

### Modifications to the upstream tests

`wrap_map.rs` (5 of 6 kept, in its `tests` module):

1. `#[gpui::test]` becomes `#[test]`; `#[gpui::test(iterations = 100)]`
   becomes `for_each_seed(100, …)`.
2. Upstream measures its test font at 14 px (8.4 px per character). Here
   `MonospaceMeasure::new(1.0)` with widths in columns: `px(14.0) * 15.0`
   is 25 columns, `px(14.0) * 8.0` is 13, `font_size * n * 0.6` is `n`.
3. The `SoftWrapIndent` setting becomes a test-only `WrapIndent` that reads
   the tab size from the snapshot (`indent_adjustment_for` is kept).
4. `test_invisibles_wrap_at_replacement_glyph_width` gives control
   characters a replacement-glyph width through the measure.
   `test_invisibles_become_width_measured_elements` is not ported: it tests
   `LineFragmentBuilder`.
5. `test_random_wraps`: the text is random (up to 300 characters) instead
   of `MultiBuffer::build_random`; fold and inlay mutations become
   `refresh_rows` and tab-size changes; the background re-wrap becomes a
   job run in random steps on its own wrapper and finished at random, or
   left stale by a newer one; the measure is `ColumnMeasure` (wide
   characters two columns); widths are 0 to 120 columns. `verify_chunks`
   also checks chunk sizes and bitmaps. `Rope` becomes `String` and
   `TestText`.

`line_wrapper.rs` (the 6 wrapping tests of 21 kept; the 14 truncation tests
and the macOS-only `test_wrap_shaped_line` are not ported): `.ZedMono` at
16 px through GPUI becomes `MonospaceMeasure::new(9.6)` (its ASCII
advance); the 1 px width-cache fill becomes `MonospaceMeasure::new(1.0)`;
the two-column crab of `test_extra_columns_overflow_guard` comes from
`ColumnMeasure`; the grapheme and break-opportunity tests also run with
`ColumnMeasure`.

### Updating the wrap layer

As for the other display-map ports, and also: diff `line_wrapper.rs` and
`LICENSE-APACHE`, and if upstream changes its segmenter or its break rules,
check them against `unicode-linebreak` with the break-opportunity tests.

## Display map: folds in the replace layer (ported 2026-11-14)

The transform core of Zed's `fold_map.rs` is now ported into the replace
layer, which supersedes "an identity layer until folds and inline
replacements are ported" and "not ported yet" for `fold_map.rs` in the
first display-map section. Same commit as above; the SHA-256 of
`fold_map.rs` is in that section's list.

| File in `crates/gv-editor/` | Upstream (commit above) | Kind | Ported |
|---|---|---|---|
| `src/display_map/replace_map.rs` | `crates/editor/src/display_map/fold_map.rs` (`FoldMap::sync`, the transform tree and summaries, `push_isomorphic`, `consolidate_*_edits`, `FoldSnapshot::{to_fold_point, clip_point, folds_in_range, is_line_folded, placeholder_range_at}`, `FoldChunks`, `FoldMapWriter::{fold, unfold_intersecting, remove_folds}`) | ported | 2026-11-14 |
| `src/display_map/replace_map/tests.rs` | the `tests` module of the same file | ported, with all 8 upstream tests | 2026-11-14 |

Written for Glyphvane, not taken from Zed: `src/folds.rs` (the foldable
regions of a Markdown document, from the gv-syntax tree), the folding parts
of `src/editor.rs` (fold commands, motions over folds, unfolding at carets
and on deletes, folds brought back by undo, `FoldPlaceholder`),
`tests/folds.rs`, `tests/editor_folds.rs` and `tests/fold_ranges.rs`. The
line-terminator handling of the replace layer (`BufferChunks`, the CRLF
widening of edits) is the earlier identity layer's, kept.

Not ported: `inlay_map.rs` (no inline replacements yet; Live Preview adds
them to this layer), `FoldPlaceholder`'s render closure, width constraint,
type tag and collapsed text, fold widths (`update_fold_widths`,
`ChunkRenderer`), `FoldRows` / `row_infos`, `FoldOffset`,
`FoldPointCursor` and `crease_map.rs`.

### Modifications to `fold_map.rs`

1. **Input layer.** The layer sits directly on a gv-text `TextSnapshot`
   instead of the inlay map over a multi-buffer, and its text turns every
   line terminator into one `\n` (as the identity layer did). Inlay offsets
   and points become buffer byte offsets and buffer points (the private
   `InputOffset` and `InputPoint` dimensions); `FoldPoint` is
   `ReplacePoint`. There is no output offset: `chunks` takes a
   `Range<ReplacePoint>` (clipped first; a range from inside a placeholder
   starts at it, where upstream yields the whole placeholder), and
   `line_len` is computed from the tree in points.
2. **Summaries.** `TransformSummary { input, output }` of rope text
   summaries becomes buffer bytes and lines on the input side and lines on
   the output side. Character counts are not kept in the tree, so
   `text_summary_for_range` scans the range (as the identity layer did) and
   building or syncing never reads the text.
3. **Folds.** Multi-buffer anchor ranges in a `SumTree<Fold>` become byte
   ranges in a sorted `Vec` (behind an `Arc`), moved through each
   `EditSummary`: the start sticks to the text after it and the end to the
   text before it (upstream's `anchor_after` / `anchor_before`), and both
   stay out of CRLFs. Folds wholly before or after the edits only stay or
   shift. **An edit that changes text inside a fold removes it**
   (`sync_with_unfolded` returns the removed folds, a replacement by the
   same text does not count); upstream keeps the fold. Folds that an edit
   brings to one range become one; a range folded already is not folded
   again. `FoldMapWriter`'s methods are `ReplaceMap::fold`,
   `unfold_intersecting`, `remove_folds` (by `FoldId`, not by type tag) and
   `unfold_all` (new).
4. **Placeholders.** A fold shows the constant `PLACEHOLDER` (`⋯`, as
   upstream's default) and its transform carries the `FoldId` of the first
   fold of its run instead of a render closure; `placeholders`,
   `placeholder_at` and `folded_range_at` (new) give the view what it needs
   to paint and hit-test them. **Folds that only touch keep a placeholder
   each** (upstream merges them unless `merge_adjacent` is off), so the
   position between them stays a visible caret position.
5. **Sync.** The step that joins the isomorphic transform before an edit to
   the last new transform only joins one that ends before the edit, and the
   transforms after the last edit are joined the same way: with folds
   removed by edits, the region of one edit can end with the text of a
   removed fold and the next edit can start in the text after it, which the
   unconditional join of upstream would skip. The edits of the layer are
   widened in input offsets exactly as upstream and converted to points
   with a three-dimension cursor. `consolidate_inlay_edits` /
   `consolidate_fold_edits` are plain loops (upstream scans in place and
   asserts the allocation is reused).
6. **Queries.** `to_fold_point(InlayPoint, Bias)` is
   `to_replace_point(BufferPoint, Bias)` and `to_inlay_point` is
   `to_buffer_point(ReplacePoint, Bias)`; both clip first, and the ends of a
   placeholder map to the ends of its fold. `is_line_folded` takes a buffer
   line. `row_infos` becomes `buffer_line` / `buffer_lines` (the buffer
   line at a row's start). `intersects_fold` is replaced by
   `folded_range_at`, which only counts positions strictly inside.
7. **Removed:** highlighting and `LanguageAwareStyling`, the renderer and
   width fields of `Chunk`, fold metadata, `#[ztracing::instrument]`.
   `check_invariants` also checks the placeholders against the folds; it
   runs in the unit tests.
8. **Lints and docs.** `Debug` on every type, doc comments on the public
   API, no `unwrap()` outside tests.

### Modifications to the upstream tests

All 8 tests are kept, in `src/display_map/replace_map/tests.rs`, next to
the identity layer's own tests.

1. `#[gpui::test]` becomes `#[test]`; `#[gpui::test(iterations = 100)]`
   becomes `for_each_seed(100, …)`. `sample_text` is a local copy of
   `util::test::sample_text`. Ranges are byte ranges built from buffer
   points, and the expected edits are in points.
2. `test_adjacent_folds` and `test_merging_folds_via_edit` expect a
   placeholder per fold where folds only touch (`⋯b⋯⋯ijkl`, `aa⋯⋯eeeee`),
   and the edit inside a fold in `test_adjacent_folds` removes it
   (`12345b⋯fghijkl`).
3. `test_random_folds`: no inlay mutations (60 % fold changes, 40 % random
   buffer edits through `random_edit`); the text is random, up to 200
   characters; the conversions are checked with both biases; it also
   checks `folded_range_at` for every offset, `text_summary_for_range` on
   the random ranges, and that rebuilding the layer from its folds gives the
   same placeholders. `row_infos` becomes `buffer_lines`.
4. `test_buffer_rows` reads `buffer_lines`; `test_random_chunk_bitmaps`
   checks every chunk's bitmaps against `Chunk::new` (upstream: `chars` and
   `tabs`).

The random tests of `tab_map.rs` and `wrap_map.rs` still have no fold
mutations; the tab and wrap layers over folds
are property-tested in `tests/folds.rs` instead.

### Updating the replace layer

As for the other display-map ports. When `inlay_map.rs` is ported for Live
Preview, merge it into this layer rather than stacking it below.

### Local changes of 2026-11-16 (payloads and chunk styles)

These go beyond upstream and are our own:

1. `replace_map.rs`: the fold placeholder becomes a payload,
   `Fold(FoldId) | Hidden | Widget`. `Hidden` outputs nothing; a widget
   outputs its text (or U+FFFC for a box) and may cover an empty range.
   `ReplaceMap::set_replacements` sets these replacements; an edit that
   touches one drops it, and a fold that overlaps one wins. Point
   conversions honour the bias at inlays and hidden text.
2. `primitives.rs`: `Chunk` carries `style: Option<StyleId>` and
   `replaced: Option<Replaced>`.
3. `tab_map.rs`, `wrap_map.rs`: `highlighted_chunks(range, &[Highlight])`
   cuts chunks at highlight boundaries; `chunks()` passes no highlights.
   This plays the role of upstream's highlights argument to `chunks()`.
4. Tests: `replace_map/tests.rs` adds tests for hidden text, widgets and
   inlays, and a random test mixing folds, replacements and buffer edits
   that also checks the tab and wrap layers against fresh builds.

### Local change of 2026-11-16 (provisional unwrapped snapshot)

`wrap_map.rs`: with no wrap width and a text over 256 KB, a whole-text
rebuild produces a provisional snapshot of 32-row blocks taken from the
tab layer's row count, without scanning the text. It is marked
interpolated until the background wrap job for the real width lands.
