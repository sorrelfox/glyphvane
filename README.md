# Glyphvane

[![CI](https://github.com/sorrelfox/glyphvane/actions/workflows/ci.yml/badge.svg)](https://github.com/sorrelfox/glyphvane/actions/workflows/ci.yml)

Glyphvane is a native, GPU-rendered Markdown editor and viewer written in Rust,
for Linux, macOS and Windows. It aims at precise link navigation between
documents (relative links, wikilinks, headings and block references across a
folder of notes) and renders each document according to a selectable flavor
profile, such as CommonMark, GitHub Flavored Markdown or Obsidian.

**Status:** Pre-release (0.0.x). Most of the features planned for 1.0 are in
place and being tested; there are no published builds yet.

## Building from source

You need [rustup](https://rustup.rs/). The toolchain version is pinned in
`rust-toolchain.toml` and is installed automatically on the first build.

```sh
git clone https://github.com/sorrelfox/glyphvane.git
cd glyphvane
cargo build
```

On Linux, the GUI crates need some system development packages; see
[CONTRIBUTING.md](CONTRIBUTING.md#linux-system-packages). To run the same
checks as CI:

```sh
cargo xtask ci
```

`glyphvane render notes.md -o notes.html` renders a file to HTML without
opening a window (CommonMark only, for now).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Commits must be signed off under the
Developer Certificate of Origin. Please read the
[Code of Conduct](CODE_OF_CONDUCT.md), and report security issues as described
in [SECURITY.md](SECURITY.md).

## License

Glyphvane is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version (`GPL-3.0-or-later`). See [LICENSE](LICENSE).
