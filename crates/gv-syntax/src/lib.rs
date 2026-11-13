// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Lossless incremental Markdown parser: CST, line checkpoints, profiles, CommonMark HTML emitter.
//!
//! The parser itself is not written yet. [`harness`] holds the string-level
//! entry points that the conformance runner (`cargo xtask spec`), the
//! differential and pathological tests and the fuzz targets are built
//! against; until the parser lands they are placeholders.

pub mod harness;
