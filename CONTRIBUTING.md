# Contributing to Glyphvane

Thanks for your interest in Glyphvane. The project is in early development
(milestone M0: foundations), so the code base changes quickly. Questions and
ideas are welcome in [GitHub Discussions](https://github.com/sorrelfox/glyphvane/discussions);
issues are for confirmed bugs and accepted features.

By participating you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
Security problems must be reported privately, as described in
[SECURITY.md](SECURITY.md).

## Building and testing

Install [rustup](https://rustup.rs/). The toolchain (version, `rustfmt` and
`clippy`) is pinned in `rust-toolchain.toml`, and rustup installs it on first
use.

```sh
cargo build                      # build the app and libraries
cargo test --workspace           # run all tests
cargo xtask ci                   # run every check that CI runs, locally
```

`cargo xtask` is the project's task runner (the `xtask/` crate). Useful
commands:

| Command | What it does |
|---|---|
| `cargo xtask ci` | Formatting, SPDX headers, dependency rules, spec fixtures and ratchet, clippy and tests in one go |
| `cargo xtask check-headers` | Checks that every source file starts with the SPDX header |
| `cargo xtask check-deps` | Checks that core crates do not depend on GPUI or the UI crates |
| `cargo xtask spec` | Runs the CommonMark (or `--suite gfm`) spec examples against the parser; see [Tests](#tests) |
| `cargo xtask fetch-spec --verify` | Checks the vendored spec fixtures against their SHA-256 sums |
| `cargo xtask pathological` | Linear-time checks on pathological inputs (release build) |
| `cargo xtask fuzz-seed` | Writes the fuzz seed corpora from the spec examples |

Run `cargo xtask ci` before opening a pull request.

### Linux system packages

The GUI crates (`gv-ui`, `gv-app`) use GPUI (from the [Zed](https://github.com/zed-industries/zed) project), which
needs some system development packages on Linux. On Ubuntu or Debian:

```sh
sudo apt-get install libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libvulkan-dev libx11-xcb-dev libxcb1-dev libfontconfig-dev libfreetype-dev \
  libasound2-dev libssl-dev pkg-config
```

This list follows Zed's requirements and will be confirmed as GPUI
integration progresses. On Fedora, the equivalents are the `-devel` packages
(`libxkbcommon-x11-devel`, `wayland-devel`, `vulkan-loader-devel`,
`libxcb-devel`, `fontconfig-devel`, `freetype-devel`, `alsa-lib-devel`,
`openssl-devel`).

macOS and Windows need no extra packages beyond the platform's standard
build tools (Xcode Command Line Tools; Visual Studio Build Tools with the C++
workload).

## Workspace layout

```
crates/   the gv-* library crates and the gv-app binary
xtask/    project task runner (cargo xtask …)
tests/    shared test fixtures (spec examples, known divergences)
fuzz/     cargo-fuzz targets (a separate workspace)
```

| Crate | Role |
|---|---|
| `gv-text` | Rope wrapper, line/column/UTF-16 maps, edits and anchors, encodings, BOM and EOL |
| `gv-syntax` | Lossless incremental Markdown parser: CST, line checkpoints, flavor profiles, CommonMark HTML emitter |
| `gv-model` | Document semantics from the CST: outline, slugs, links, embeds, tags, block IDs, front matter |
| `gv-vault` | Vault roots, path and content index, file watcher, link resolver, journaled refactors |
| `gv-search` | Query language, parallel scan, quick-switcher matching |
| `gv-render-tree` | Render tree from CST and model, flattened virtual items, plain-text projection |
| `gv-html` | Raw HTML regions, sanitizer, HTML to render nodes, HTML to Markdown for paste |
| `gv-highlight` | Code highlighting with caches and the Linguist alias map |
| `gv-math` | LaTeX to display list or SVG, with caches and the macro pass |
| `gv-diagram` | Mermaid to SVG with caches; diagram registry and fallback cards |
| `gv-media` | Image, SVG, audio and PDF decoding and rasterizing |
| `gv-bases` | Bases model, expression language and evaluation over the content index |
| `gv-canvas` | JSON Canvas 1.0 model that preserves unknown keys |
| `gv-export` | HTML exporter, Typst document builder, print preparation |
| `gv-editor` | Buffer model, selections, transactions, undo, Markdown commands, display map |
| `gv-spell` | Spell-check backend trait, spellbook backend, prose tokenizer |
| `gv-helper` | Helper-process entry points and IPC protocol for risky or non-cancellable engines |
| `gv-settings` | Settings schema, layers with provenance, format-preserving TOML files, migrations and file locations |
| `gv-ui` | GPUI views: workspace, panes, editor and reader elements, panels, theming |
| `gv-app` | The `glyphvane` binary: CLI, single instance, platform glue, logging, settings bootstrap |

**GUI-free rule:** only `gv-ui` and `gv-app` may depend on GPUI. Every other
crate is a core crate: it must build and test without a GPU, a display or any
GUI dependency, so it can be used by headless commands (`glyphvane render`,
`check-links`, …) and tested on any CI runner. `cargo xtask check-deps`
enforces this.

## Code requirements

### SPDX header

Every Rust source file starts with:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors
```

Other source-like files (TOML, YAML, shell scripts) carry the same two lines
with their own comment syntax where the format allows it.
`cargo xtask check-headers` checks this in CI.

### Formatting and lints

- Code is formatted with `rustfmt` (`cargo fmt --all`). CI rejects unformatted code.
- `clippy` runs with warnings denied:
  `cargo clippy --workspace --all-targets -- -D warnings`. Workspace lint
  levels live in the root `Cargo.toml`. If a lint must be silenced, use a
  narrowly scoped `#[allow(...)]` or `#[expect(...)]` with a comment explaining why.
- No `unsafe` in core crates. `unsafe_code` is denied workspace-wide; an
  exception needs a reviewed justification and a `// SAFETY:` comment on
  every block.
- Avoid `unwrap()` outside tests; return errors or document the invariant
  with `expect("…")`.

### UI: themes and colours

- **Never hardcode colours** in UI code. Use theme tokens, so every element
  follows the active theme.
- Three themes ship together: **Light**, **Semi-light** and **Dark**. A UI
  change must work and look right in all three, and pull requests that change
  the UI include a screenshot of each.

### Tests

- Add tests for new behaviour and for every bug fix (a regression test that
  fails before the fix).
- Parser and rendering changes should come with fixtures that state the
  expected output and, where relevant, the reference implementation it was
  compared against (GitHub, Obsidian, pandoc, the CommonMark spec).

Parser test layers:

- **Spec conformance.** `cargo xtask spec` runs the CommonMark 0.31.2 examples
  (`--suite gfm` for GFM 0.29) and prints pass counts per section and a diff
  for each failure. Filter with `--section NAME` or `--example N`;
  `--format json` gives a machine-readable report. HTML is compared after the
  spec's normalization (whitespace, attribute order, character references).
- **The ratchet.** `tests/fixtures/spec/passing.txt` (and `passing-gfm.txt`)
  list the examples that pass. CI runs `cargo xtask spec --ratchet`, which
  fails if any listed example stops passing; examples not listed yet may
  fail. When your change makes more examples pass, record them with
  `cargo xtask spec --update-ratchet` and commit the file.
- **Differential.** `crates/gv-syntax/tests/differential.rs` compares the
  parser with [comrak](https://crates.io/crates/comrak) on random documents
  (ignored by default; run with `-- --ignored`). Deliberate or known
  differences go in `tests/fixtures/differential/known-divergences.toml`
  with a reason. Never change the parser towards a comrak or cmark bug.
- **Pathological inputs.** `cargo xtask pathological` checks that time grows
  linearly (n vs 2n) on cmark's pathological inputs and on deep-nesting
  shapes; `--engine comrak` runs the same suite against comrak.
- **Fuzzing.** See [fuzz/README.md](fuzz/README.md).

## Commits

### Developer Certificate of Origin

Glyphvane uses the [Developer Certificate of Origin](https://developercertificate.org/)
(DCO). There is no CLA. Every commit must carry a `Signed-off-by:` line,
which you add with:

```sh
git commit -s
```

By signing off you certify that you wrote the change or otherwise have the
right to submit it under the project's license, `GPL-3.0-or-later`. To fix
the last commit, use `git commit --amend -s`; for several commits,
`git rebase --signoff main`.

### Conventional Commits

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/).
The changelog is generated from them with git-cliff (`cliff.toml`).

```
<type>(<scope>): <summary in the imperative, lower case, no full stop>

<optional body explaining what and why>

Signed-off-by: Your Name <you@example.com>
```

Types: `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `build`, `ci`,
`chore`. Mark breaking changes with `!` (`feat(vault)!: …`) or a
`BREAKING CHANGE:` footer.

Common scopes:

| Scope | Area |
|---|---|
| `parser` | Markdown parser and CST (`gv-syntax`) |
| `profiles` | Flavor profiles and detection |
| `model` | Document semantics (`gv-model`) |
| `links` | Link parsing and resolution |
| `vault` | Vault index, watcher, refactors (`gv-vault`) |
| `search` | Search and quick switcher |
| `render` | Render tree, HTML, highlighting, math, diagrams, media |
| `editor` | Editing model (`gv-editor`) |
| `ui` | GPUI views and theming (`gv-ui`) |
| `app` | CLI and platform integration (`gv-app`) |
| `export` | HTML/PDF export and print |
| `xtask` | Task runner |
| `ci` | Workflows and repository tooling |
| `deps` | Dependency updates |

Examples: `feat(parser): parse GFM task list items`,
`fix(links): resolve wikilinks case-insensitively on macOS`.

## Pull requests

- Keep pull requests focused; split unrelated changes.
- Fill in the pull request template: tests, docs, performance impact and,
  for UI changes, screenshots in all three themes.
- CI must pass on Linux (x64 and arm64), macOS and Windows, together with
  `cargo-deny` and the Markdown link check.
- Update user-facing documentation when behaviour changes.

## License

By contributing, you agree that your contributions are licensed under the
GNU General Public License, version 3 or (at your option) any later version.
See [LICENSE](LICENSE).
