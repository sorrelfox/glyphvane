# Fuzz targets

A [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) project for the
parser. It has its own workspace (and `Cargo.lock`), so the nightly toolchain
and sanitizer flags cargo-fuzz needs never touch the main build.

| Target | Checks |
|---|---|
| `parse_full` | Arbitrary bytes (decoded lossily as UTF-8) parse and render without panicking |
| `incremental_equals_full` | A document plus an edit script: reparsing incrementally gives the same CST as a full parse of the edited text, with the CommonMark options and with the GFM extensions |
| `roundtrip_lossless` | The concatenated CST token texts equal the input |

The targets call the string-level entry points in `gv_syntax::harness`. Until
the parser exists those are placeholders, so the invariants hold trivially;
the targets do not need to change when the parser lands.

`incremental_equals_full` inputs are `document 0xFF edit*`, where each edit is
`pos_lo pos_hi delete insert_len insert…` (see `decode_edits` in
`src/lib.rs`). Every byte string decodes to valid edits.

## Seed corpus

```sh
cargo xtask fuzz-seed    # writes fuzz/corpus/<target>/ from the spec examples
```

`corpus/` and `artifacts/` are not committed.

## Fuzzing with cargo-fuzz (nightly)

```sh
rustup toolchain install nightly
cargo install cargo-fuzz
cd fuzz
cargo +nightly fuzz run parse_full -- -max_total_time=60 -timeout=2
cargo +nightly fuzz run incremental_equals_full -- -max_total_time=60 -timeout=2
cargo +nightly fuzz run roundtrip_lossless -- -max_total_time=60 -timeout=2
```

`-timeout` (seconds per input) turns super-linear behaviour into a failure.
Crashes and timeouts land in `fuzz/artifacts/<target>/`; turn each into a
regression test in `crates/gv-syntax`.

## Without nightly

Built normally, each target is a standalone runner that replays a corpus and
can run a simple (not coverage-guided) mutation loop:

```sh
cd fuzz
cargo run --release --bin parse_full -- [--iterations N] [--seed S] [--timeout-ms T] [PATH...]
```

PATH defaults to `corpus/<target>`. A panicking or slow input is saved to
`artifacts/<target>/` and the runner exits with status 1. Replaying a
cargo-fuzz crash file works the same way:
`cargo run --bin parse_full -- artifacts/parse_full/crash-…`.
