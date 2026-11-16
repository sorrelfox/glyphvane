# Bundled dictionaries

The files below are shipped unmodified and compiled into the application
(`include_bytes!` in `src/dictionary.rs`).

## en_US (SCOWL 2026.02.25, size 60)

- Source: <https://github.com/en-wl/wordlist>, release `rel-2026.02.25`,
  asset `hunspell-en_US-2026.02.25.zip`
  (SHA-256 `ac8e73310e951d88c52c2cf2ba54ceaca34f8486a81630ac8a75dc5f931179f9`).
- Files taken from the archive, unmodified:

  | File | SHA-256 |
  |---|---|
  | `en_US/en_US.aff` | `526e1dc32d1c0e8806dd72809c017b5546f2104b08b9424a4c47311aca526cca` |
  | `en_US/en_US.dic` | `ab012ae86ce4b46b60db5b150869288de9c4a87bc747911764e582125b1b961e` |
  | `en_US/README_en_US.txt` | `d63c711523fc9504fb71e1278489c6d7bb8cafcba75158c107cd17844955470f` |

- Licence: the SCOWL licence (Kevin Atkinson and contributors, MIT-like)
  for the word list, and Geoff Kuenning's BSD-style licence for the affix
  file. Both texts are in `en_US/README_en_US.txt`, which is reproduced in
  the third-party notices.
- Encoding: UTF-8 (`SET UTF-8`).

To update: download the release asset, check its SHA-256 against the
release page, replace the three files, update this table and the notices,
and run `cargo test -p gv-spell`.
