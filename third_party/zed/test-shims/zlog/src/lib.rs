// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Test-only stand-in for Zed's `zlog` crate. The vendored rope tests call
//! `zlog::init_test()` to set up logging; that is not needed here.

/// Does nothing. Upstream, this installs a logger when `RUST_LOG` is set.
pub fn init_test() {}
