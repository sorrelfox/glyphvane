# Provenance of `third_party/noto-emoji/`

Noto Color Emoji, the CBDT (colour bitmap) build, from Google's
[noto-emoji](https://github.com/googlefonts/noto-emoji) repository. It is
licensed under the SIL Open Font License, version 1.1 (`OFL-1.1`); the
licence text is in [`LICENSE`](LICENSE). Copyright 2022 Google Inc.

| | |
|---|---|
| Upstream release | [`v2.051`](https://github.com/googlefonts/noto-emoji/releases/tag/v2.051) (2025-09-15) |
| Font version (`name` ID 5) | `Version 2.051;GOOG;noto-emoji:20250818:e92753bfa55fd449e427d4d325f9c8c40408c74e` |
| PostScript name | `NotoColorEmoji` |
| Tables | `CBDT`, `CBLC`, `GSUB`, `OS/2`, `cmap`, `head`, `hhea`, `hmtx`, `maxp`, `name`, `post`, `vhea`, `vmtx` |
| Imported | 2026-11-16 |

| File here | Downloaded from | Changes |
|---|---|---|
| `NotoColorEmoji.ttf` | https://raw.githubusercontent.com/googlefonts/noto-emoji/v2.051/fonts/NotoColorEmoji.ttf | none |
| `LICENSE` | https://raw.githubusercontent.com/googlefonts/noto-emoji/v2.051/fonts/LICENSE | none |

```
72a635cb3d2f3524c51620cdde406b217204e8a6a06c6a096ff8ed4b5fd6e27b  NotoColorEmoji.ttf  (10 673 480 bytes; git blob 943741df1e37aa3ee3b5afc9129843fd8d81908c)
6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2  LICENSE             (4 301 bytes; git blob d952d62c065f3f35fb83a173496e90b21525aef3)
```

The git blob ids match the files listed by the GitHub API for `fonts/` at
the tag.

## Why it is here

GPUI's Linux rasterizer draws colour emoji only from CBDT or sbix bitmaps
or COLR version 0 layers, and only from the face named `NotoColorEmoji`.
Some distributions (Fedora among them) install that font only as COLRv1,
which draws nothing. The Linux packages install this file in
`share/glyphvane/fonts/`, and the app adds it only when no installed
`NotoColorEmoji` face can be drawn (`crates/gv-app/src/emoji_font.rs`). It
is not embedded in the executable and not shipped on macOS or Windows,
whose GPUI text systems draw the system emoji fonts.

Drop it once GPUI's Linux rasterizer draws COLRv1.

## Updating

1. Pick a release tag that still ships `fonts/NotoColorEmoji.ttf` (the CBDT
   build).
2. Download the font and `fonts/LICENSE` from that tag into this folder.
   Check that the font has `CBDT` and `CBLC` tables and the PostScript name
   `NotoColorEmoji`; `cargo test -p gv-app emoji` checks both.
3. Update the tables above.
