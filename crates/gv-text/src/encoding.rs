// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Encodings: decoding file bytes to text and encoding text back.
//!
//! # Guarantee
//!
//! When [`decode`] reports [`DecodeStatus::Exact`], encoding the decoded text
//! with the reported [`TextFormat`] reproduces the input bytes exactly. Text
//! is never normalized: BOM, line endings and every other byte survive.
//!
//! When it reports [`DecodeStatus::Lossy`], the text contains replacement
//! characters or would not encode back to the same bytes. Such a file must be
//! opened read-only (or reopened with another encoding); saving it would
//! change bytes the user did not touch.
//!
//! # Pipeline
//!
//! 1. A byte-order mark selects UTF-8, UTF-16LE or UTF-16BE and is remembered.
//! 2. Without a BOM, valid UTF-8 (checked with `simdutf8`) is UTF-8.
//! 3. Otherwise, input that looks binary (a NUL byte, or many control
//!    characters) is rejected with [`DecodeError::Binary`].
//! 4. Otherwise the legacy encoding is guessed with `chardetng`, decoded with
//!    `encoding_rs`, and encoded back to check that the round trip is exact.
//!
//! Valid UTF-8 is accepted even if it contains NUL bytes: U+0000 stays in the
//! text. Callers that want to refuse such files can use [`looks_binary`].

use std::fmt;

use encoding_rs::EncoderResult;

use crate::lines::{LineEnding, LineEndingStats};

/// A text encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// UTF-8.
    Utf8,
    /// UTF-16, little endian.
    Utf16Le,
    /// UTF-16, big endian.
    Utf16Be,
    /// Any other encoding `encoding_rs` supports (windows-1252, Shift_JIS,
    /// GBK, …).
    Legacy(LegacyEncoding),
}

/// A non-Unicode encoding. Never UTF-8 or UTF-16.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct LegacyEncoding(&'static encoding_rs::Encoding);

impl fmt::Debug for LegacyEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.name())
    }
}

impl LegacyEncoding {
    /// The WHATWG name, for example `windows-1252`.
    pub fn name(self) -> &'static str {
        self.0.name()
    }
}

impl Encoding {
    /// Looks up an encoding by WHATWG label (`"latin1"`, `"utf-16le"`,
    /// `"sjis"`, …). Case and surrounding whitespace are ignored.
    pub fn for_label(label: &str) -> Option<Self> {
        encoding_rs::Encoding::for_label(label.as_bytes()).map(Self::from_encoding_rs)
    }

    fn from_encoding_rs(encoding: &'static encoding_rs::Encoding) -> Self {
        if encoding == encoding_rs::UTF_8 {
            Self::Utf8
        } else if encoding == encoding_rs::UTF_16LE {
            Self::Utf16Le
        } else if encoding == encoding_rs::UTF_16BE {
            Self::Utf16Be
        } else {
            Self::Legacy(LegacyEncoding(encoding))
        }
    }

    /// The encoding's name (`UTF-8`, `UTF-16LE`, `UTF-16BE`, or the WHATWG
    /// name of a legacy encoding).
    pub fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16LE",
            Self::Utf16Be => "UTF-16BE",
            Self::Legacy(encoding) => encoding.name(),
        }
    }

    /// The byte-order mark of this encoding, if it has one.
    pub fn bom(self) -> Option<&'static [u8]> {
        match self {
            Self::Utf8 => Some(b"\xEF\xBB\xBF"),
            Self::Utf16Le => Some(b"\xFF\xFE"),
            Self::Utf16Be => Some(b"\xFE\xFF"),
            Self::Legacy(_) => None,
        }
    }
}

impl fmt::Display for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Whether the file starts with a byte-order mark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Bom {
    /// No byte-order mark.
    #[default]
    Absent,
    /// The encoding's byte-order mark precedes the text.
    Present,
}

/// How a text is stored on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextFormat {
    /// The encoding.
    pub encoding: Encoding,
    /// Whether a BOM precedes the text.
    pub bom: Bom,
    /// The terminator for new lines the user types: the file's dominant one,
    /// or LF if it has none. Existing line endings are never converted.
    pub line_ending: LineEnding,
}

impl Default for TextFormat {
    /// UTF-8, no BOM, LF.
    fn default() -> Self {
        Self {
            encoding: Encoding::Utf8,
            bom: Bom::Absent,
            line_ending: LineEnding::Lf,
        }
    }
}

/// Whether decoding was lossless.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DecodeStatus {
    /// Encoding the text with the reported format gives back the input bytes.
    Exact,
    /// The round trip is not possible; open read-only.
    Lossy(LossReason),
}

impl DecodeStatus {
    /// Whether the round trip is exact, so the text may be edited and saved.
    pub fn is_exact(self) -> bool {
        self == Self::Exact
    }
}

/// Why a decode is lossy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LossReason {
    /// The bytes are not valid UTF-8 (after a UTF-8 BOM, or with UTF-8
    /// chosen explicitly). `valid_up_to` is the length of the valid prefix.
    InvalidUtf8 {
        /// Length of the longest valid prefix, in bytes.
        valid_up_to: usize,
    },
    /// Odd length or an unpaired surrogate in UTF-16.
    MalformedUtf16,
    /// Byte sequences the legacy encoding does not define.
    MalformedLegacy,
    /// The text decodes, but encoding it gives different bytes (the
    /// encoding maps several byte sequences to the same text).
    NotReversible,
}

/// Decoded file contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    /// The text, exactly as decoded (U+FFFD where decoding failed).
    pub text: String,
    /// The detected format.
    pub format: TextFormat,
    /// Terminator counts, including whether endings are mixed.
    pub line_endings: LineEndingStats,
    /// Whether the round trip is exact.
    pub status: DecodeStatus,
}

/// Why bytes were not decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DecodeError {
    /// The bytes are not text (see [`looks_binary`]).
    Binary,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Binary => f.write_str("the file is binary, not text"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Why text could not be encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EncodeError {
    /// The encoding has no byte-order mark.
    BomNotSupported {
        /// The encoding.
        encoding: Encoding,
    },
    /// A character has no representation in the encoding.
    Unmappable {
        /// Byte offset of the character in the text.
        offset: usize,
        /// The character.
        ch: char,
        /// The encoding.
        encoding: Encoding,
    },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BomNotSupported { encoding } => {
                write!(f, "{encoding} has no byte-order mark")
            }
            Self::Unmappable {
                offset,
                ch,
                encoding,
            } => write!(
                f,
                "character {ch:?} (U+{:04X}) at byte {offset} cannot be written in {encoding}",
                u32::from(*ch)
            ),
        }
    }
}

impl std::error::Error for EncodeError {}

/// How many leading bytes [`looks_binary`] examines.
const BINARY_SNIFF_LEN: usize = 64 * 1024;

/// Whether `bytes` look like a binary file: a NUL byte, or more than 10 % of
/// control characters other than tab, LF, CR, form feed and escape, in the
/// first 64 KiB.
pub fn looks_binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(BINARY_SNIFF_LEN)];
    if memchr::memchr(0, sample).is_some() {
        return true;
    }
    let controls = sample
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C | 0x1B))
        .count();
    controls * 10 > sample.len()
}

/// The encoding and length of a byte-order mark at the start of `bytes`.
fn sniff_bom(bytes: &[u8]) -> Option<(Encoding, usize)> {
    [Encoding::Utf8, Encoding::Utf16Le, Encoding::Utf16Be]
        .into_iter()
        .find_map(|encoding| {
            let bom = encoding.bom()?;
            bytes.starts_with(bom).then_some((encoding, bom.len()))
        })
}

/// Decodes file contents, detecting the encoding (see the module docs).
pub fn decode(bytes: &[u8]) -> Result<Decoded, DecodeError> {
    if let Some((encoding, bom_len)) = sniff_bom(bytes) {
        return Ok(decode_body(&bytes[bom_len..], encoding, Bom::Present));
    }
    if let Ok(text) = simdutf8::basic::from_utf8(bytes) {
        return Ok(finish(
            text.to_owned(),
            Encoding::Utf8,
            Bom::Absent,
            DecodeStatus::Exact,
        ));
    }
    if looks_binary(bytes) {
        return Err(DecodeError::Binary);
    }
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    detector.feed(bytes, true);
    let guess = detector.guess(None, chardetng::Utf8Detection::Deny);
    Ok(decode_body(
        bytes,
        Encoding::from_encoding_rs(guess),
        Bom::Absent,
    ))
}

/// Decodes file contents with a given encoding, for example one the user
/// picked. A leading BOM of that encoding is stripped and remembered.
pub fn decode_with(bytes: &[u8], encoding: Encoding) -> Decoded {
    match encoding.bom() {
        Some(bom) if bytes.starts_with(bom) => {
            decode_body(&bytes[bom.len()..], encoding, Bom::Present)
        }
        _ => decode_body(bytes, encoding, Bom::Absent),
    }
}

fn decode_body(body: &[u8], encoding: Encoding, bom: Bom) -> Decoded {
    let (text, status) = match encoding {
        Encoding::Utf8 => match simdutf8::basic::from_utf8(body) {
            Ok(text) => (text.to_owned(), DecodeStatus::Exact),
            Err(_) => {
                let valid_up_to = std::str::from_utf8(body)
                    .err()
                    .map_or(0, |e| e.valid_up_to());
                (
                    String::from_utf8_lossy(body).into_owned(),
                    DecodeStatus::Lossy(LossReason::InvalidUtf8 { valid_up_to }),
                )
            }
        },
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let rs = if encoding == Encoding::Utf16Le {
                encoding_rs::UTF_16LE
            } else {
                encoding_rs::UTF_16BE
            };
            match rs.decode_without_bom_handling_and_without_replacement(body) {
                Some(text) => (text.into_owned(), DecodeStatus::Exact),
                None => (
                    rs.decode_without_bom_handling(body).0.into_owned(),
                    DecodeStatus::Lossy(LossReason::MalformedUtf16),
                ),
            }
        }
        Encoding::Legacy(legacy) => {
            match legacy
                .0
                .decode_without_bom_handling_and_without_replacement(body)
            {
                Some(text) => {
                    let text = text.into_owned();
                    let format = TextFormat {
                        encoding,
                        bom,
                        line_ending: LineEnding::Lf,
                    };
                    let status = match encode(&text, &format) {
                        Ok(bytes) if bytes == body => DecodeStatus::Exact,
                        _ => DecodeStatus::Lossy(LossReason::NotReversible),
                    };
                    (text, status)
                }
                None => (
                    legacy.0.decode_without_bom_handling(body).0.into_owned(),
                    DecodeStatus::Lossy(LossReason::MalformedLegacy),
                ),
            }
        }
    };
    finish(text, encoding, bom, status)
}

fn finish(text: String, encoding: Encoding, bom: Bom, status: DecodeStatus) -> Decoded {
    let line_endings = LineEndingStats::from_text(&text);
    Decoded {
        format: TextFormat {
            encoding,
            bom,
            line_ending: line_endings.dominant().unwrap_or(LineEnding::Lf),
        },
        text,
        line_endings,
        status,
    }
}

/// Encodes `text` in `format`. Line endings are written as they are in the
/// text; `format.line_ending` is not applied.
pub fn encode(text: &str, format: &TextFormat) -> Result<Vec<u8>, EncodeError> {
    encode_chunks(std::iter::once(text), format)
}

/// Encodes text given as consecutive chunks (for example a snapshot's
/// chunks) in `format`.
pub fn encode_chunks<'a>(
    chunks: impl IntoIterator<Item = &'a str>,
    format: &TextFormat,
) -> Result<Vec<u8>, EncodeError> {
    let encoding = format.encoding;
    let mut out = Vec::new();
    if format.bom == Bom::Present {
        out.extend_from_slice(
            encoding
                .bom()
                .ok_or(EncodeError::BomNotSupported { encoding })?,
        );
    }
    match encoding {
        Encoding::Utf8 => {
            for chunk in chunks {
                out.extend_from_slice(chunk.as_bytes());
            }
        }
        Encoding::Utf16Le => {
            for chunk in chunks {
                out.extend(chunk.encode_utf16().flat_map(u16::to_le_bytes));
            }
        }
        Encoding::Utf16Be => {
            for chunk in chunks {
                out.extend(chunk.encode_utf16().flat_map(u16::to_be_bytes));
            }
        }
        Encoding::Legacy(legacy) => {
            let mut encoder = legacy.0.new_encoder();
            let mut base = 0;
            for chunk in chunks.into_iter().chain(std::iter::once("")) {
                // The empty chunk at the end flushes stateful encoders.
                let last = chunk.is_empty();
                let mut consumed = 0;
                loop {
                    let src = &chunk[consumed..];
                    let needed = encoder
                        .max_buffer_length_from_utf8_without_replacement(src.len())
                        .unwrap_or(src.len().saturating_mul(4));
                    out.reserve(needed.max(16));
                    let (result, read) =
                        encoder.encode_from_utf8_to_vec_without_replacement(src, &mut out, last);
                    consumed += read;
                    match result {
                        EncoderResult::InputEmpty => break,
                        EncoderResult::OutputFull => {}
                        EncoderResult::Unmappable(ch) => {
                            return Err(EncodeError::Unmappable {
                                offset: base + consumed - ch.len_utf8(),
                                ch,
                                encoding,
                            });
                        }
                    }
                }
                base += chunk.len();
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy(label: &str) -> Encoding {
        Encoding::for_label(label).expect("known label")
    }

    #[test]
    fn labels_map_unicode_to_dedicated_variants() {
        assert_eq!(Encoding::for_label("utf8"), Some(Encoding::Utf8));
        assert_eq!(Encoding::for_label("UTF-16"), Some(Encoding::Utf16Le));
        assert_eq!(Encoding::for_label("utf-16be"), Some(Encoding::Utf16Be));
        assert_eq!(legacy("latin1").name(), "windows-1252");
        assert_eq!(Encoding::for_label("no such thing"), None);
    }

    #[test]
    fn utf8_without_bom() {
        let bytes = "# Titel\r\nÄpfel 🍎\n".as_bytes();
        let decoded = decode(bytes).expect("text");
        assert_eq!(decoded.status, DecodeStatus::Exact);
        assert_eq!(decoded.format.encoding, Encoding::Utf8);
        assert_eq!(decoded.format.bom, Bom::Absent);
        assert!(decoded.line_endings.is_mixed());
        assert_eq!(
            encode(&decoded.text, &decoded.format).expect("encodes"),
            bytes
        );
    }

    #[test]
    fn boms_are_detected_and_kept() {
        let cases: [(&[u8], Encoding, &str); 3] = [
            (b"\xEF\xBB\xBFa\r\n", Encoding::Utf8, "a\r\n"),
            (b"\xFF\xFEa\x00\n\x00", Encoding::Utf16Le, "a\n"),
            (b"\xFE\xFF\x00a\x00\r", Encoding::Utf16Be, "a\r"),
        ];
        for (bytes, encoding, text) in cases {
            let decoded = decode(bytes).expect("text");
            assert_eq!(decoded.text, text);
            assert_eq!(decoded.format.encoding, encoding);
            assert_eq!(decoded.format.bom, Bom::Present);
            assert_eq!(decoded.status, DecodeStatus::Exact);
            assert_eq!(
                encode(&decoded.text, &decoded.format).expect("encodes"),
                bytes
            );
        }
        assert_eq!(
            decode(b"\xFF\xFE").expect("text").text,
            "",
            "a lone BOM is an empty UTF-16 text"
        );
    }

    #[test]
    fn utf16_surrogates_round_trip() {
        let text = "x😀y";
        let format = TextFormat {
            encoding: Encoding::Utf16Be,
            bom: Bom::Present,
            line_ending: LineEnding::Lf,
        };
        let bytes = encode(text, &format).expect("encodes");
        assert_eq!(bytes, b"\xFE\xFF\x00x\xD8\x3D\xDE\x00\x00y");
        let decoded = decode(&bytes).expect("text");
        assert_eq!(decoded.text, text);
        assert!(decoded.status.is_exact());
    }

    #[test]
    fn malformed_unicode_is_lossy() {
        // Unpaired high surrogate.
        let decoded = decode(b"\xFF\xFE\x3D\xD8a\x00").expect("text");
        assert_eq!(
            decoded.status,
            DecodeStatus::Lossy(LossReason::MalformedUtf16)
        );
        assert_eq!(decoded.text, "\u{FFFD}a");
        // Odd length.
        let decoded = decode(b"\xFF\xFEa\x00b").expect("text");
        assert_eq!(
            decoded.status,
            DecodeStatus::Lossy(LossReason::MalformedUtf16)
        );
        // Invalid UTF-8 after a UTF-8 BOM.
        let decoded = decode(b"\xEF\xBB\xBFab\xFFc").expect("text");
        assert_eq!(
            decoded.status,
            DecodeStatus::Lossy(LossReason::InvalidUtf8 { valid_up_to: 2 })
        );
        assert_eq!(decoded.text, "ab\u{FFFD}c");
    }

    #[test]
    fn legacy_encoding_is_detected_and_round_trips() {
        let text =
            "Ein schöner Tag in der Straße, über den Fluß.\r\nÄrger, Öl und Übermut.\r\n".repeat(4);
        let format = TextFormat {
            encoding: legacy("windows-1252"),
            bom: Bom::Absent,
            line_ending: LineEnding::CrLf,
        };
        let bytes = encode(&text, &format).expect("encodes");
        let decoded = decode(&bytes).expect("text");
        assert_eq!(decoded.text, text);
        assert_eq!(decoded.format, format);
        assert_eq!(decoded.status, DecodeStatus::Exact);
    }

    #[test]
    fn shift_jis_round_trips() {
        let text = "日本語のテキストです。漢字とひらがなとカタカナ。\n".repeat(8);
        let format = TextFormat {
            encoding: legacy("shift_jis"),
            bom: Bom::Absent,
            line_ending: LineEnding::Lf,
        };
        let bytes = encode(&text, &format).expect("encodes");
        let decoded = decode(&bytes).expect("text");
        assert_eq!(decoded.format.encoding, format.encoding);
        assert_eq!(decoded.text, text);
        assert!(decoded.status.is_exact());
    }

    #[test]
    fn non_reversible_legacy_bytes_are_lossy() {
        // Shift_JIS 0xED40 (NEC-selected IBM extension) decodes to U+7E8A,
        // which the WHATWG encoder writes as the IBM extension 0xFA5C.
        let decoded = decode_with(b"\xED\x40", legacy("shift_jis"));
        assert_eq!(decoded.text, "\u{7E8A}");
        assert_eq!(
            decoded.status,
            DecodeStatus::Lossy(LossReason::NotReversible)
        );
    }

    #[test]
    fn malformed_legacy_is_lossy() {
        // A Shift_JIS lead byte followed by an invalid trail byte.
        let decoded = decode_with(b"a\x81\x20b", legacy("shift_jis"));
        assert!(!decoded.status.is_exact());
        assert!(decoded.text.contains('\u{FFFD}'));
    }

    #[test]
    fn binary_is_rejected() {
        assert_eq!(
            decode(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR"),
            Err(DecodeError::Binary)
        );
        assert!(looks_binary(&[1, 2, 3, 4, 5, b'a']));
        assert!(!looks_binary(b"plain text\twith tab\x1b[0m"));
        // Valid UTF-8 with a NUL stays text.
        let decoded = decode(b"a\x00b").expect("text");
        assert_eq!(decoded.text, "a\u{0}b");
        assert!(decoded.status.is_exact());
    }

    #[test]
    fn unmappable_characters_are_reported() {
        let format = TextFormat {
            encoding: legacy("windows-1252"),
            bom: Bom::Absent,
            line_ending: LineEnding::Lf,
        };
        let err = encode_chunks(["abc", "dé€", "x😀"], &format).expect_err("emoji");
        assert_eq!(
            err,
            EncodeError::Unmappable {
                offset: 10,
                ch: '😀',
                encoding: format.encoding
            }
        );
        let format = TextFormat {
            bom: Bom::Present,
            ..format
        };
        assert!(matches!(
            encode("a", &format),
            Err(EncodeError::BomNotSupported { .. })
        ));
    }

    #[test]
    fn stateful_encoder_is_flushed() {
        let format = TextFormat {
            encoding: legacy("iso-2022-jp"),
            bom: Bom::Absent,
            line_ending: LineEnding::Lf,
        };
        let bytes = encode("日本", &format).expect("encodes");
        // Ends by switching back to ASCII.
        assert!(bytes.ends_with(b"\x1b(B"));
        let decoded = decode_with(&bytes, format.encoding);
        assert_eq!(decoded.text, "日本");
        assert!(decoded.status.is_exact());
    }

    #[test]
    fn dominant_line_ending_sets_format() {
        let decoded = decode(b"a\r\nb\r\nc\n").expect("text");
        assert_eq!(decoded.format.line_ending, LineEnding::CrLf);
        let decoded = decode(b"no newline").expect("text");
        assert_eq!(decoded.format.line_ending, LineEnding::Lf);
    }
}
