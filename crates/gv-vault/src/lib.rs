// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Vault roots, path and content index, watcher, link resolver, journaled refactors.
//!
//! This crate currently holds the **path index** and the **link resolver**:
//!
//! - [`PathIndex`]: an immutable, cheaply cloned snapshot of every file and
//!   folder under a vault root, with exact, NFC, case-fold and basename
//!   maps. Built by a parallel walk ([`PathIndex::build`]), updated
//!   incrementally ([`PathIndex::apply`]), published lock-free
//!   ([`SharedPathIndex`]). While the walk runs, the index is
//!   [`IndexState::Building`] and links that need it resolve `Pending`.
//! - [`resolve()`]: a pure function from a snapshot, a [`LinkRef`] and the
//!   profile's [`ProfileParams`] to a [`Resolution`] with a [`Status`], a
//!   target, a classified fragment, candidates and a trace.
//! - [`url`]: the URL classifier and policy shared with the HTML pipeline
//!   (network paths, script schemes, dangerous OS handlers, LAN hosts).
//! - [`platform`]: naming rules of Linux, macOS and Windows as data.
//! - [`expect`]: results in the `glyphvane-expect/1` oracle format.
//! - [`model`]: the bridge from the document model (`gv-model`): its links
//!   as [`LinkRef`]s, and [`ModelFragments`], the [`FragmentResolver`] that
//!   matches headings and block ids in target documents.
//!
//! The resolver itself only classifies fragments; matching headings and
//! block ids is the document model's job, through [`FragmentResolver`].

pub mod expect;
pub mod fragment;
pub mod index;
pub mod keys;
pub mod link;
pub mod model;
pub mod path;
pub mod platform;
pub mod probe;
pub mod profile;
pub mod repo;
pub mod resolve;
pub mod url;

pub use fragment::{Fragment, FragmentKind, FragmentResolver, FragmentTarget, StructuralFragments};
pub use index::{
    Change, Entry, EntryKind, FileId, FileMeta, IndexConfig, IndexError, IndexState, PathIndex,
    SharedPathIndex, SymlinkTarget, WalkStats,
};
pub use link::{LinkKind, LinkRef};
pub use model::{ModelFragments, fragment_in_model, links_from_model};
pub use path::VaultPath;
pub use platform::PlatformRules;
pub use probe::{FsProbe, NoProbe, PathProbe, ProbeResult};
pub use profile::{Anchoring, ProfileId, ProfileParams, ResolveSettings};
pub use resolve::{
    Candidate, Note, Resolution, Stage, Status, TargetKind, TraceStep, resolve, resolve_with_probe,
};
