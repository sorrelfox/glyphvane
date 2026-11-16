// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The embedded grammar bundle and the fence language alias map.
//!
//! `bundle/grammars.bin` is built by `cargo xtask highlight-bundle` from
//! `bundle/grammars.toml` (see there for the licence rules). Format, all
//! integers little-endian: the 8-byte magic `GVHLBND1`, a `u32` index
//! length, the JSON index, then each grammar's minified JSON as a
//! raw-deflate stream at the offset the index gives.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::sync::OnceLock;

use serde::Deserialize;

static BUNDLE_BYTES: &[u8] = include_bytes!("../bundle/grammars.bin");
const MAGIC: &[u8; 8] = b"GVHLBND1";

#[derive(Debug, Deserialize)]
struct Index {
    format: u32,
    source: String,
    linguist: String,
    grammars: Vec<GrammarEntry>,
    aliases: HashMap<String, u16>,
}

#[derive(Debug, Deserialize)]
struct GrammarEntry {
    name: String,
    scope: String,
    display: String,
    licence: String,
    deps: Vec<u16>,
    offset: u32,
    len: u32,
    raw_len: u32,
}

#[derive(Debug)]
pub(crate) struct Bundle {
    index: Index,
    data: &'static [u8],
}

/// The parsed bundle. A bundle that fails to parse (a build error, caught
/// by the tests) is logged and behaves as an empty bundle.
pub(crate) fn bundle() -> &'static Bundle {
    static BUNDLE: OnceLock<Bundle> = OnceLock::new();
    BUNDLE.get_or_init(|| {
        Bundle::parse(BUNDLE_BYTES).unwrap_or_else(|err| {
            log::error!("gv-highlight: grammar bundle unusable: {err}");
            Bundle {
                index: Index {
                    format: 1,
                    source: String::new(),
                    linguist: String::new(),
                    grammars: Vec::new(),
                    aliases: HashMap::new(),
                },
                data: &[],
            }
        })
    })
}

impl Bundle {
    fn parse(bytes: &'static [u8]) -> Result<Self, String> {
        let rest = bytes.strip_prefix(MAGIC.as_slice()).ok_or("bad magic")?;
        let (len, rest) = rest.split_first_chunk::<4>().ok_or("truncated header")?;
        let len = usize::try_from(u32::from_le_bytes(*len)).map_err(|e| e.to_string())?;
        if rest.len() < len {
            return Err("truncated index".to_owned());
        }
        let (index, data) = rest.split_at(len);
        let index: Index = serde_json::from_slice(index).map_err(|e| e.to_string())?;
        if index.format != 1 {
            return Err(format!("unknown bundle format {}", index.format));
        }
        let count = index.grammars.len();
        for g in &index.grammars {
            let end = usize::try_from(u64::from(g.offset) + u64::from(g.len)).unwrap_or(usize::MAX);
            if end > data.len() || g.deps.iter().any(|&d| usize::from(d) >= count) {
                return Err(format!("grammar {} out of range", g.name));
            }
        }
        if index.aliases.values().any(|&i| usize::from(i) >= count) {
            return Err("alias out of range".to_owned());
        }
        Ok(Self { index, data })
    }

    pub(crate) fn len(&self) -> usize {
        self.index.grammars.len()
    }

    fn entry(&self, id: u16) -> Option<&GrammarEntry> {
        self.index.grammars.get(usize::from(id))
    }

    /// The decompressed JSON of grammar `id`.
    pub(crate) fn grammar_json(&self, id: u16) -> Result<Vec<u8>, String> {
        let g = self.entry(id).ok_or("no such grammar")?;
        let start = usize::try_from(g.offset).map_err(|e| e.to_string())?;
        let end = start + usize::try_from(g.len).map_err(|e| e.to_string())?;
        let packed = self.data.get(start..end).ok_or("grammar out of range")?;
        let raw_len = usize::try_from(g.raw_len).map_err(|e| e.to_string())?;
        let json = miniz_oxide::inflate::decompress_to_vec_with_limit(packed, raw_len)
            .map_err(|e| format!("grammar {}: {e}", g.name))?;
        if json.len() != raw_len {
            return Err(format!("grammar {}: size mismatch", g.name));
        }
        Ok(json)
    }

    /// Grammar `id` and every grammar it needs (embedded grammars and
    /// injections), transitively, in load order.
    pub(crate) fn closure(&self, id: u16) -> Vec<u16> {
        let mut seen = BTreeSet::new();
        let mut order = Vec::new();
        let mut stack = vec![id];
        while let Some(g) = stack.pop() {
            if !seen.insert(g) {
                continue;
            }
            order.push(g);
            if let Some(entry) = self.entry(g) {
                stack.extend(entry.deps.iter().rev().copied());
            }
        }
        order
    }
}

/// A language (grammar) in the bundle.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Language(u16);

impl Language {
    /// Resolves a fence info string (or a bare language name) through the
    /// Linguist alias map: `rust`, `rs`, `Rust`, `{.rust}`, `rust,ignore`,
    /// `language-rust` and `py3` all resolve. Only the first word counts.
    pub fn from_info(info: &str) -> Option<Self> {
        let word = info
            .split(|c: char| c.is_whitespace() || c == ',' || c == '{' || c == '}')
            .find(|w| !w.is_empty())?;
        let word = word.trim_start_matches('.').to_lowercase();
        let aliases = &bundle().index.aliases;
        if let Some(&id) = aliases.get(&word) {
            return Some(Self(id));
        }
        let stripped = word
            .strip_prefix("language-")
            .or_else(|| word.strip_prefix("lang-"))?;
        aliases.get(stripped).map(|&id| Self(id))
    }

    /// Every bundled language, embedded grammars included.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..bundle().len()).filter_map(|i| u16::try_from(i).ok().map(Self))
    }

    /// The grammar name (`rust`, `shellscript`).
    pub fn name(self) -> &'static str {
        bundle().entry(self.0).map_or("", |g| g.name.as_str())
    }

    /// The display name (`Rust`, `Shell`), for the code block's language
    /// label.
    pub fn display_name(self) -> &'static str {
        bundle().entry(self.0).map_or("", |g| g.display.as_str())
    }

    /// The grammar's root scope (`source.rust`).
    pub fn scope(self) -> &'static str {
        bundle().entry(self.0).map_or("", |g| g.scope.as_str())
    }

    /// The grammar's licence (SPDX, or `LicenseRef-...`).
    pub fn licence(self) -> &'static str {
        bundle().entry(self.0).map_or("", |g| g.licence.as_str())
    }

    pub(crate) fn id(self) -> u16 {
        self.0
    }

    pub(crate) fn from_id(id: u16) -> Self {
        Self(id)
    }
}

impl fmt::Debug for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Language({})", self.name())
    }
}

/// Where the bundle's grammars and fence names come from
/// (`repository@commit`), for diagnostics.
pub fn bundle_sources() -> (&'static str, &'static str) {
    let index = &bundle().index;
    (index.source.as_str(), index.linguist.as_str())
}

/// The number of fence aliases in the map.
pub fn alias_count() -> usize {
    bundle().index.aliases.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_parses() {
        let b = Bundle::parse(BUNDLE_BYTES).unwrap();
        assert!(b.len() >= 59, "{}", b.len());
        for id in 0..b.len() {
            let id = u16::try_from(id).unwrap();
            let json = b.grammar_json(id).unwrap();
            assert_eq!(json.first(), Some(&b'{'));
        }
    }

    #[test]
    fn corrupt_bundles_are_rejected() {
        assert!(Bundle::parse(b"nope").is_err());
        assert!(Bundle::parse(b"GVHLBND1\xff\xff\x00\x00{}").is_err());
        assert!(Bundle::parse(b"GVHLBND1\x02\x00\x00\x00{}").is_err());
    }
}
