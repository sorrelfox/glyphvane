// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Flavor profiles.
//!
//! Every document has exactly one profile, and each profile gives every
//! syntax exactly one meaning. This crate holds:
//!
//! - the profile names ([`ProfileId`], and [`PlannedProfile`] for dialects
//!   detected before their profiles ship);
//! - the switch matrix ([`Switches`]) and the built-in definitions
//!   ([`builtin::definition`]);
//! - the compiler ([`compile()`]): definition, tool tuning and switch
//!   overrides become a [`CompiledProfile`], the flag set, rule lists and
//!   inline trigger table the parser and renderer read;
//! - the `.glyphvane.toml` reader ([`config`]);
//! - detection with its precedence and reasons ([`detect`]);
//! - dialect hints ([`hints`]).
//!
//! It depends on no other Glyphvane crate, so the parser can take a
//! [`CompiledProfile`] directly.

pub mod builtin;
pub mod compile;
pub mod config;
pub mod detect;
pub mod hints;
mod id;
pub mod switches;

pub use builtin::{ProfileDef, definition};
pub use compile::{
    BlockRule, CompiledProfile, ContainerTransform, InlineRule, SwitchOverrides, Tuning, compile,
    compile_builtin,
};
pub use config::{ConfigDiagnostic, ConfigFile, FolderConfig, Severity};
pub use detect::{DetectOptions, Detection, Detector, Overrides, Reason, detect};
pub use id::{PlannedProfile, ProfileId, Requested};
pub use switches::{SwitchError, SwitchValue, Switches};
