# Provenance of `third_party/inter/`

Inter, the variable build (axes `wght` 100–900 and `opsz` 14–32), from
[rsms/inter](https://github.com/rsms/inter). It is licensed under the SIL
Open Font License, version 1.1 (`OFL-1.1`); the licence text is in
[`LICENSE`](LICENSE). Copyright (c) 2016 The Inter Project Authors.

| | |
|---|---|
| Upstream release | [`v4.1`](https://github.com/rsms/inter/releases/tag/v4.1) (2024-11-16) |
| Family | `Inter Variable` (upright and italic) |
| PostScript names | `InterVariable`, `InterVariableItalic` |
| Imported | 2026-11-17 |

| File here | From `Inter-4.1.zip` of the release | Changes |
|---|---|---|
| `InterVariable.ttf` | `InterVariable.ttf` | none |
| `InterVariable-Italic.ttf` | `InterVariable-Italic.ttf` | none |
| `LICENSE` | `LICENSE.txt` | renamed |

```
9883fdd4a49d4fb66bd8177ba6625ef9a64aa45899767dde3d36aa425756b11e  Inter-4.1.zip  (33 707 794 bytes, https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip)
4989b125924991b90d05b2d16e0e388c48f7d5bb8b30539bbf9c755278d0ccaf  InterVariable.ttf         (879 708 bytes)
d6f1f6a172d9e588438db9f986fd5cfad7b30f644374080a8a9d4d91e344586f  InterVariable-Italic.ttf  (910 252 bytes)
262481e844521b326f5ecd053e59b98c8b2da78c8ee1bdbb6e8174305e54935a  LICENSE                   (4 380 bytes)
```

The licence matches `LICENSE.txt` at the tag in the repository.

## Why it is here

PDF export lays text out with these faces, so the same note gives the same
PDF on every machine, whatever fonts are installed. The packages install
the files in `share/glyphvane/fonts/` (`Contents/Resources/fonts/` in the
macOS bundle), where `glyphvane-helper` looks for them
(`crates/gv-helper/src/fonts.rs`). They are not embedded in the
executables.

## Updating

1. Download the release zip, check its SHA-256 and take the two variable
   fonts and the licence from it.
2. Check that the family is still `Inter Variable`; `cargo test -p
   gv-helper fonts` checks that the bundled faces load.
3. Update the tables above.
