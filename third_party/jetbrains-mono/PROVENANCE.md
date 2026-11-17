# Provenance of `third_party/jetbrains-mono/`

JetBrains Mono, the variable build (axis `wght` 100–800), from
[JetBrains/JetBrainsMono](https://github.com/JetBrains/JetBrainsMono). It is
licensed under the SIL Open Font License, version 1.1 (`OFL-1.1`); the
licence text is in [`LICENSE`](LICENSE). Copyright 2020 The JetBrains Mono
Project Authors.

| | |
|---|---|
| Upstream release | [`v2.304`](https://github.com/JetBrains/JetBrainsMono/releases/tag/v2.304) (2023-01-14) |
| Family | `JetBrains Mono` (upright and italic) |
| PostScript names | `JetBrainsMono-Regular`, `JetBrainsMono-Italic` |
| Imported | 2026-11-17 |

| File here | From `JetBrainsMono-2.304.zip` of the release | Changes |
|---|---|---|
| `JetBrainsMono-Variable.ttf` | `fonts/variable/JetBrainsMono[wght].ttf` | renamed |
| `JetBrainsMono-Variable-Italic.ttf` | `fonts/variable/JetBrainsMono-Italic[wght].ttf` | renamed |
| `LICENSE` | `OFL.txt` | renamed |

```
6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf  JetBrainsMono-2.304.zip  (5 622 857 bytes, https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip)
662a196d58f1183bf2d77428b6d5283fe3f45161ab021bea4036bc98e5cac016  JetBrainsMono-Variable.ttf         (303 144 bytes)
f115aaa12113718c02ce72864fe6823b87241bc23d3e44cf1220155f861063f2  JetBrainsMono-Variable-Italic.ttf  (308 888 bytes)
30f0c136e3c88e422d0791acd97238870f9054a9729bc34cf2ff0d4ed8cac4ad  LICENSE                            (4 399 bytes)
```

The licence matches `OFL.txt` at the tag in the repository.

## Why it is here

Code in PDF exports is set in this face, so the same note gives the same
PDF on every machine. Packaged and found like Inter (see
[`../inter/PROVENANCE.md`](../inter/PROVENANCE.md)).

## Updating

As for Inter: download the release zip, check it, take the two variable
fonts and `OFL.txt`, run `cargo test -p gv-helper fonts`, update the tables.
