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
