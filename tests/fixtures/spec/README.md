# Spec fixtures

Conformance examples run by `cargo xtask spec` and the gv-syntax tests.

| File | Source | Version |
|---|---|---|
| `commonmark-0.31.2.json` | <https://spec.commonmark.org/0.31.2/spec.json>, unmodified | CommonMark 0.31.2 (2024-01-28), 652 examples |
| `gfm-0.29.0.gfm.13.txt` | <https://raw.githubusercontent.com/github/cmark-gfm/0.29.0.gfm.13/test/spec.txt>, unmodified | GFM spec 0.29 (2019-04-06) as shipped with cmark-gfm 0.29.0.gfm.13, 672 examples |
| `gfm-0.29.0.gfm.13.json` | Generated from the file above by `cargo xtask fetch-spec` | 670 examples (the 2 `disabled` ones are skipped but keep their numbers) |
| `passing.txt`, `passing-gfm.txt` | Ours: the ratchet of examples that must keep passing | — |
| `SHA256SUMS` | Ours: SHA-256 of the three fixture files | — |

The GFM conversion follows cmark-gfm's `test/spec_tests.py`: examples are
fenced by 32 backticks plus ` example`, optionally followed by the extensions
they need (`table`, `strikethrough`, `autolink`, `tagfilter`); `→` stands for a
tab; the section is the most recent heading. Each example carries `example`,
`section`, `markdown`, `html`, `start_line`, `end_line` and `extensions`.

## Checksums

```
d431b29d97b6f73e69d547109cf5081578fac931e72afe95639ebe766c1b2a20  commonmark-0.31.2.json
7d8e5814befec287ac116786d81ff14e0adc9b13295b4494649e995408fd871c  gfm-0.29.0.gfm.13.txt
ae6d888df6c59ed74f4eb67135b555cdebe42ee3482e019204df2e1cc0aec599  gfm-0.29.0.gfm.13.json
```

```sh
cargo xtask fetch-spec --verify --offline   # local files only (runs in CI)
cargo xtask fetch-spec --verify             # also re-downloads and compares
cargo xtask fetch-spec                      # re-download; fails if a sum changes
cargo xtask fetch-spec --update-sums        # deliberate upgrade: rewrite SHA256SUMS
```

## Licences

- **CommonMark spec examples** (`commonmark-0.31.2.json`): extracted from the
  CommonMark Spec, Copyright (C) 2014-16 John MacFarlane, released under the
  [Creative Commons Attribution-ShareAlike 4.0 International licence](https://creativecommons.org/licenses/by-sa/4.0/)
  (CC BY-SA 4.0).
- **GFM spec** (`gfm-0.29.0.gfm.13.txt`) and the JSON generated from it: the
  GitHub Flavored Markdown Spec, based on the CommonMark Spec (Copyright (C)
  2014-15 John MacFarlane) with GitHub's additions, released under
  [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/).

CC BY-SA 4.0 allows sharing and adapting the material, including
commercially, provided that the source is credited, the licence is named or
linked, changes are indicated, and adaptations are shared under the same
licence. Accordingly:

- these files are redistributed under CC BY-SA 4.0, **not** under the
  project's GPL-3.0-or-later licence, and are used only as test data (no part
  of them is compiled into Glyphvane);
- `commonmark-0.31.2.json` and `gfm-0.29.0.gfm.13.txt` are unmodified copies;
- `gfm-0.29.0.gfm.13.json` is an adaptation (a format conversion of the GFM
  spec's examples, described above) and is likewise licensed under
  CC BY-SA 4.0.

The HTML normalizer in `crates/gv-syntax/src/harness/normalize.rs` is a port
of `test/normalize.py` from the commonmark-spec repository (BSD-2-Clause,
Copyright (c) 2014 John MacFarlane; derived from MIT-licensed code by Karl
Dubost); both notices are kept in that file.
