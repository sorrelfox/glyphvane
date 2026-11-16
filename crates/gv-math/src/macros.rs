// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The document macro pass.
//!
//! Macros are document-wide in the MathJax way: a `\newcommand`,
//! `\renewcommand`, `\providecommand`, `\def` or `\gdef` in one formula is
//! available to every formula after it, in document order. RaTeX itself
//! scopes definitions to the formula that makes them, so [`MacroPass`]
//! scans each formula for definitions and hands the formulas after it a
//! [`MacroSet`] to inject before parsing.
//!
//! Supported forms: `\newcommand{\name}[n]{body}` (also without braces
//! around the name, and starred), and `\def\name#1#2{body}`. A definition
//! with an optional-argument default (`[n][default]`) or delimited
//! parameters (`\def\x#1.{…}`) is left to the formula that makes it and is
//! not carried forward.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

/// Caps on what the pass carries forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacroLimits {
    /// Definitions kept per document (later ones are ignored).
    pub max_macros: usize,
    /// Bytes of one macro body.
    pub max_body_bytes: usize,
    /// Bytes of one macro name, backslash included.
    pub max_name_bytes: usize,
}

impl Default for MacroLimits {
    fn default() -> Self {
        Self {
            max_macros: 512,
            max_body_bytes: 2048,
            max_name_bytes: 64,
        }
    }
}

/// How a definition was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    /// `\newcommand` (errors in RaTeX if the name exists).
    New,
    /// `\renewcommand`.
    Renew,
    /// `\providecommand` (defines only a new name).
    Provide,
    /// `\def`, `\gdef`.
    Def,
}

/// One macro definition found in a formula.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    pub kind: DefKind,
    /// The control sequence, with its backslash (`\RR`).
    pub name: String,
    /// The replacement text, ready for RaTeX: unused declared parameters
    /// are referenced by a no-op so the argument count is right.
    pub body: String,
}

/// The macros in force for one formula. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct MacroSet {
    defs: Arc<[(Arc<str>, Arc<str>)]>,
    hash: u64,
}

impl PartialEq for MacroSet {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && self.defs == other.defs
    }
}

impl Eq for MacroSet {}

impl MacroSet {
    /// No macros.
    pub fn empty() -> Self {
        Self::default()
    }

    /// A set from `(name, body)` pairs (names with their backslash);
    /// a later pair replaces an earlier one with the same name.
    pub fn from_pairs<N: AsRef<str>, B: AsRef<str>>(
        pairs: impl IntoIterator<Item = (N, B)>,
    ) -> Self {
        let map: BTreeMap<Arc<str>, Arc<str>> = pairs
            .into_iter()
            .map(|(n, b)| (Arc::from(n.as_ref()), Arc::from(b.as_ref())))
            .collect();
        Self::from_map(&map)
    }

    fn from_map(map: &BTreeMap<Arc<str>, Arc<str>>) -> Self {
        let defs: Arc<[(Arc<str>, Arc<str>)]> =
            map.iter().map(|(n, b)| (n.clone(), b.clone())).collect();
        let mut h = DefaultHasher::new();
        defs.hash(&mut h);
        Self {
            defs,
            hash: if map.is_empty() { 0 } else { h.finish() },
        }
    }

    /// A hash of the definitions (0 when empty), for cache keys.
    pub fn hash_value(&self) -> u64 {
        self.hash
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// The `(name, body)` pairs, sorted by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.defs.iter().map(|(n, b)| (&**n, &**b))
    }

    /// The body of macro `name` (with its backslash).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.defs
            .binary_search_by(|(n, _)| (**n).cmp(name))
            .ok()
            .map(|i| &*self.defs[i].1)
    }

    /// Every character of every body, for fallback glyph metrics.
    pub(crate) fn chars(&self) -> impl Iterator<Item = char> + '_ {
        self.defs.iter().flat_map(|(_, b)| b.chars())
    }
}

/// Runs the macro pass over a document's formulas, in document order.
#[derive(Debug, Default)]
pub struct MacroPass {
    limits: MacroLimits,
    defs: BTreeMap<Arc<str>, Arc<str>>,
    /// The set handed out last, reused while nothing changes.
    current: Option<MacroSet>,
}

impl MacroPass {
    pub fn new(limits: MacroLimits) -> Self {
        Self {
            limits,
            ..Self::default()
        }
    }

    /// The macros to render `tex` with (those of the formulas before it),
    /// then records the definitions `tex` makes for the formulas after it.
    pub fn next(&mut self, tex: &str) -> MacroSet {
        let found = definitions(tex, &self.limits);
        let set = if found
            .iter()
            .any(|d| d.kind == DefKind::New && self.defs.contains_key(d.name.as_str()))
        {
            // RaTeX rejects `\newcommand` of a defined name; let the
            // formula's own definition win, as MathJax redefines.
            let mut map = self.defs.clone();
            for d in found.iter().filter(|d| d.kind == DefKind::New) {
                map.remove(d.name.as_str());
            }
            MacroSet::from_map(&map)
        } else {
            self.current
                .get_or_insert_with(|| MacroSet::from_map(&self.defs))
                .clone()
        };
        for d in found {
            let exists = self.defs.contains_key(d.name.as_str());
            if d.kind == DefKind::Provide && exists {
                continue;
            }
            if !exists && self.defs.len() >= self.limits.max_macros {
                continue;
            }
            self.defs.insert(Arc::from(d.name), Arc::from(d.body));
            self.current = None;
        }
        set
    }
}

/// The macro set for each formula of a document, in order.
pub fn document_macros<'a>(formulas: impl IntoIterator<Item = &'a str>) -> Vec<MacroSet> {
    let mut pass = MacroPass::new(MacroLimits::default());
    formulas.into_iter().map(|f| pass.next(f)).collect()
}

/// The definitions formula `tex` makes, in order.
pub fn definitions(tex: &str, limits: &MacroLimits) -> Vec<Definition> {
    let mut s = Scanner {
        b: tex.as_bytes(),
        i: 0,
    };
    let mut out = Vec::new();
    while s.i < s.b.len() {
        match s.b[s.i] {
            b'%' => s.skip_comment(),
            b'\\' => {
                let cs = s.control_sequence();
                let kind = match tex.get(cs).unwrap_or_default() {
                    "\\newcommand" => Some(DefKind::New),
                    "\\renewcommand" => Some(DefKind::Renew),
                    "\\providecommand" => Some(DefKind::Provide),
                    "\\def" | "\\gdef" => Some(DefKind::Def),
                    _ => None,
                };
                let Some(kind) = kind else { continue };
                let def = if kind == DefKind::Def {
                    s.def(tex)
                } else {
                    s.newcommand(tex)
                };
                if let Some((name, args, body)) = def
                    && name.len() <= limits.max_name_bytes
                    && body.len() <= limits.max_body_bytes
                {
                    out.push(Definition {
                        kind,
                        name: name.to_owned(),
                        body: with_all_args(body, args),
                    });
                }
            }
            _ => s.i += 1,
        }
    }
    out
}

/// RaTeX infers a text macro's arity from the `#1`, `#2`, … it contains,
/// so a declared but unused parameter is referenced through a no-op.
fn with_all_args(body: &str, args: u8) -> String {
    let mut out = body.to_owned();
    for k in 1..=args {
        let p = format!("#{k}");
        if !body.contains(&p) {
            out.push_str(&format!("\\@secondoftwo{{{p}}}{{}}"));
        }
    }
    out
}

struct Scanner<'a> {
    b: &'a [u8],
    i: usize,
}

impl Scanner<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn skip_comment(&mut self) {
        while let Some(c) = self.peek() {
            self.i += 1;
            if c == b'\n' {
                break;
            }
        }
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            match c {
                b' ' | b'\t' | b'\n' | b'\r' => self.i += 1,
                b'%' => self.skip_comment(),
                _ => break,
            }
        }
    }

    /// At a backslash: moves past the control sequence (letters, or one
    /// character) and returns its byte range.
    fn control_sequence(&mut self) -> std::ops::Range<usize> {
        let start = self.i;
        self.i += 1;
        let letters = self.b[self.i..]
            .iter()
            .take_while(|c| c.is_ascii_alphabetic() || **c == b'@')
            .count();
        if letters > 0 {
            self.i += letters;
        } else if self.i < self.b.len() {
            // One character, possibly multi-byte.
            self.i += 1;
            while self.i < self.b.len() && (self.b[self.i] & 0xC0) == 0x80 {
                self.i += 1;
            }
        }
        start..self.i
    }

    /// At `{`: the content of the balanced group, moving past it.
    fn group<'t>(&mut self, tex: &'t str) -> Option<&'t str> {
        if self.peek() != Some(b'{') {
            return None;
        }
        self.i += 1;
        let start = self.i;
        let mut depth = 1usize;
        while let Some(c) = self.peek() {
            match c {
                b'\\' => {
                    self.i += 2;
                    continue;
                }
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        let body = tex.get(start..self.i);
                        self.i += 1;
                        return body;
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
        self.i = self.i.min(self.b.len());
        None
    }

    /// A macro name: `\cs` or `{\cs}`.
    fn name<'t>(&mut self, tex: &'t str) -> Option<&'t str> {
        self.skip_ws();
        if self.peek() == Some(b'{') {
            self.i += 1;
            self.skip_ws();
            if self.peek() != Some(b'\\') {
                return None;
            }
            let r = self.control_sequence();
            self.skip_ws();
            if self.peek() != Some(b'}') {
                return None;
            }
            self.i += 1;
            tex.get(r)
        } else if self.peek() == Some(b'\\') {
            let r = self.control_sequence();
            tex.get(r)
        } else {
            None
        }
    }

    /// After `\newcommand`: `*`? name `[n]`? `{body}`.
    fn newcommand<'t>(&mut self, tex: &'t str) -> Option<(&'t str, u8, &'t str)> {
        if self.peek() == Some(b'*') {
            self.i += 1;
        }
        let name = self.name(tex)?;
        self.skip_ws();
        let mut args = 0u8;
        if self.peek() == Some(b'[') {
            let d = *self.b.get(self.i + 1)?;
            if !d.is_ascii_digit() || self.b.get(self.i + 2) != Some(&b']') {
                return None;
            }
            args = d - b'0';
            self.i += 3;
            self.skip_ws();
            if self.peek() == Some(b'[') {
                // An optional argument with a default: not carried forward.
                return None;
            }
        }
        let body = self.group(tex)?;
        Some((name, args, body))
    }

    /// After `\def`: name, `#1#2…`, `{body}`.
    fn def<'t>(&mut self, tex: &'t str) -> Option<(&'t str, u8, &'t str)> {
        let name = self.name(tex)?;
        let mut args = 0u8;
        while self.peek() == Some(b'#') {
            let d = *self.b.get(self.i + 1)?;
            if d != b'1' + args || args >= 9 {
                return None;
            }
            args += 1;
            self.i += 2;
        }
        if self.peek() != Some(b'{') {
            // Delimited parameters: not carried forward.
            return None;
        }
        let body = self.group(tex)?;
        Some((name, args, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs(tex: &str) -> Vec<(DefKind, String, String)> {
        definitions(tex, &MacroLimits::default())
            .into_iter()
            .map(|d| (d.kind, d.name, d.body))
            .collect()
    }

    #[test]
    fn finds_every_form() {
        assert_eq!(
            defs(r"\newcommand{\RR}{\mathbb{R}} x \renewcommand\norm[1]{\left\|#1\right\|}"),
            vec![
                (DefKind::New, r"\RR".into(), r"\mathbb{R}".into()),
                (DefKind::Renew, r"\norm".into(), r"\left\|#1\right\|".into()),
            ]
        );
        assert_eq!(
            defs(r"\def\sq#1{#1^2}\gdef\e{\varepsilon}\providecommand*{\pp}{p}"),
            vec![
                (DefKind::Def, r"\sq".into(), "#1^2".into()),
                (DefKind::Def, r"\e".into(), r"\varepsilon".into()),
                (DefKind::Provide, r"\pp".into(), "p".into()),
            ]
        );
    }

    #[test]
    fn skips_unsupported_and_broken_forms() {
        assert!(defs(r"\newcommand{\x}[1][0]{#1}").is_empty());
        assert!(defs(r"\def\x#1.{#1}").is_empty());
        assert!(defs(r"\newcommand{\x}{unclosed").is_empty());
        assert!(defs(r"% \newcommand{\x}{y}").is_empty());
        assert!(defs(r"\newcommand{x}{y}").is_empty());
        assert!(defs(r"\newcommand").is_empty());
        assert!(defs(r"\def").is_empty());
        assert!(defs("\\newcommand{\\é}{y}").len() == 1);
    }

    #[test]
    fn unused_parameters_keep_the_arity() {
        assert_eq!(
            defs(r"\newcommand{\second}[2]{#2}")[0].2,
            r"#2\@secondoftwo{#1}{}"
        );
    }

    #[test]
    fn braces_in_bodies_balance_and_escapes_do_not_count() {
        assert_eq!(
            defs(r"\newcommand{\set}[1]{\left\{#1\right\}} \newcommand{\b}{{a}}")[1].2,
            "{a}"
        );
    }

    #[test]
    fn macros_flow_forward_in_document_order() {
        let sets = document_macros([
            r"x",
            r"\newcommand{\RR}{\mathbb{R}} \RR",
            r"\RR^n",
            r"\renewcommand{\RR}{\mathbb{C}}",
            r"\RR",
        ]);
        assert!(sets[0].is_empty() && sets[1].is_empty());
        assert_eq!(sets[2].get(r"\RR"), Some(r"\mathbb{R}"));
        assert_eq!(sets[3].get(r"\RR"), Some(r"\mathbb{R}"));
        assert_eq!(sets[4].get(r"\RR"), Some(r"\mathbb{C}"));
        assert_ne!(sets[2].hash_value(), sets[4].hash_value());
        assert_eq!(sets[0].hash_value(), 0);
    }

    #[test]
    fn a_later_newcommand_of_the_same_name_is_not_injected_into_itself() {
        let sets = document_macros([r"\newcommand{\a}{1}", r"\newcommand{\a}{2}\a", r"\a"]);
        assert_eq!(sets[1].get(r"\a"), None);
        assert_eq!(sets[2].get(r"\a"), Some("2"));
    }

    #[test]
    fn provide_does_not_replace() {
        let sets = document_macros([r"\newcommand{\a}{1}", r"\providecommand{\a}{2}", ""]);
        assert_eq!(sets[2].get(r"\a"), Some("1"));
    }

    #[test]
    fn caps_hold() {
        let limits = MacroLimits {
            max_macros: 2,
            max_body_bytes: 4,
            max_name_bytes: 64,
        };
        assert!(definitions(r"\def\a{12345}", &limits).is_empty());
        let mut pass = MacroPass::new(limits);
        pass.next(r"\def\a{1}\def\b{2}\def\c{3}");
        assert_eq!(pass.next("").len(), 2);
    }
}
