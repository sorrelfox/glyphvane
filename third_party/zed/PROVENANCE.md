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
