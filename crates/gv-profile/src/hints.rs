// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Dialect hints: strong signs that a document was written for another
//! dialect than its profile.
//!
//! [`scan`] counts the signals in a document's text, outside fenced code.
//! [`suggest`] drops the ones the active profile understands itself and,
//! when two or more distinct signals remain, names the dialect most of
//! them point to. The interface shows that as a dismissible infobar.

use crate::compile::CompiledProfile;
use crate::id::{PlannedProfile, ProfileId, Requested};
use crate::switches::Admonitions;

/// A strong sign of another dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Signal {
    /// `!!! note` (or `???`) followed by an indented body: MkDocs.
    MkDocsAdmonition,
    /// `:::note[`: Docusaurus.
    DocusaurusAdmonition,
    /// `{.class}` or `{#id}` after a heading: Pandoc.
    HeadingAttributes,
    /// `[[_TOC_]]`: GitLab.
    GitLabToc,
    /// `%%`: Obsidian comments.
    PercentComment,
    /// `^[`: inline footnotes (Obsidian, Pandoc).
    InlineFootnote,
    /// `> [!type]-` or `+`: a folding callout (Obsidian).
    FoldingCallout,
}

impl Signal {
    const ALL: [Signal; 7] = [
        Self::MkDocsAdmonition,
        Self::DocusaurusAdmonition,
        Self::HeadingAttributes,
        Self::GitLabToc,
        Self::PercentComment,
        Self::InlineFootnote,
        Self::FoldingCallout,
    ];

    /// The dialects the signal points to.
    fn dialects(self) -> &'static [Requested] {
        const OB: Requested = Requested::BuiltIn(ProfileId::Obsidian);
        match self {
            Self::MkDocsAdmonition => &[Requested::Planned(PlannedProfile::MkDocs)],
            Self::DocusaurusAdmonition => &[Requested::Planned(PlannedProfile::Docusaurus)],
            Self::HeadingAttributes => &[Requested::Planned(PlannedProfile::Pandoc)],
            Self::GitLabToc => &[Requested::Planned(PlannedProfile::GitLab)],
            Self::PercentComment | Self::FoldingCallout => &[OB],
            Self::InlineFootnote => &[OB, Requested::Planned(PlannedProfile::Pandoc)],
        }
    }

    /// Whether `profile` reads this syntax itself.
    fn native_to(self, profile: &CompiledProfile) -> bool {
        let s = &profile.switches;
        match self {
            Self::HeadingAttributes => s.attributes,
            Self::InlineFootnote => s.inline_footnotes,
            Self::PercentComment => s.comments,
            Self::FoldingCallout => s.admonitions == Admonitions::Callouts,
            Self::MkDocsAdmonition | Self::DocusaurusAdmonition | Self::GitLabToc => false,
        }
    }
}

/// The distinct signals found in a document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signals(Vec<Signal>);

impl Signals {
    fn add(&mut self, signal: Signal) {
        if !self.0.contains(&signal) {
            self.0.push(signal);
            self.0.sort();
        }
    }

    #[must_use]
    pub fn contains(&self, signal: Signal) -> bool {
        self.0.contains(&signal)
    }

    #[must_use]
    pub fn as_slice(&self) -> &[Signal] {
        &self.0
    }
}

/// Finds the signals in `text`, skipping fenced code blocks.
#[must_use]
pub fn scan(text: &str) -> Signals {
    let mut found = Signals::default();
    let mut fence: Option<(char, usize)> = None;
    let mut pending_admonition = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if let Some((ch, len)) = fence {
            let run = trimmed.chars().take_while(|&c| c == ch).count();
            if run >= len && trimmed[run * ch.len_utf8()..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        if indent < 4 {
            let run_of = |ch: char| trimmed.chars().take_while(|&c| c == ch).count();
            let (backticks, tildes) = (run_of('`'), run_of('~'));
            if backticks >= 3 {
                fence = Some(('`', backticks));
                pending_admonition = false;
                continue;
            }
            if tildes >= 3 {
                fence = Some(('~', tildes));
                pending_admonition = false;
                continue;
            }
        }
        if pending_admonition && !line.trim().is_empty() {
            if line.starts_with("    ") || line.starts_with('\t') {
                found.add(Signal::MkDocsAdmonition);
            }
            pending_admonition = false;
        }
        if indent < 4 {
            if (trimmed.starts_with("!!! ") || trimmed.starts_with("??? "))
                && trimmed[4..].starts_with(|c: char| c.is_alphanumeric())
            {
                pending_admonition = true;
            }
            if let Some(rest) = trimmed.strip_prefix(":::") {
                let word = rest.chars().take_while(|c| c.is_alphanumeric()).count();
                if word > 0 && rest[word..].starts_with('[') {
                    found.add(Signal::DocusaurusAdmonition);
                }
            }
            if is_attributed_heading(trimmed) {
                found.add(Signal::HeadingAttributes);
            }
            if trimmed.trim_end() == "[[_TOC_]]" {
                found.add(Signal::GitLabToc);
            }
            if is_folding_callout(trimmed) {
                found.add(Signal::FoldingCallout);
            }
        }
        if line.contains("%%") {
            found.add(Signal::PercentComment);
        }
        if line.contains("^[") {
            found.add(Signal::InlineFootnote);
        }
    }
    found
}

/// `## Title {.class}` or `## Title {#id}`.
fn is_attributed_heading(line: &str) -> bool {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&hashes) || !line[hashes..].starts_with(' ') {
        return false;
    }
    let line = line.trim_end();
    line.ends_with('}')
        && line
            .rfind(" {")
            .is_some_and(|i| matches!(line.as_bytes().get(i + 2), Some(b'.' | b'#')))
}

/// `> [!type]-` or `> [!type]+`.
fn is_folding_callout(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('>') else {
        return false;
    };
    let Some(rest) = rest.trim_start().strip_prefix("[!") else {
        return false;
    };
    let Some(close) = rest.find(']') else {
        return false;
    };
    close > 0 && matches!(rest.as_bytes().get(close + 1), Some(b'-' | b'+'))
}

/// What the hint infobar offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialectHint {
    /// The dialect the signals point to.
    pub dialect: Requested,
    /// The signals the active profile does not read.
    pub signals: Vec<Signal>,
}

impl DialectHint {
    /// "This looks like MkDocs. Switch profile? (full support in 1.2)".
    #[must_use]
    pub fn message(&self) -> String {
        let release = match self.dialect {
            Requested::BuiltIn(id) => id.ships_in(),
            Requested::Planned(p) => Some(p.ships_in()),
        };
        let base = format!("This looks like {}. Switch profile?", self.dialect.name());
        match release {
            Some(r) => format!("{base} (full support in {r})"),
            None => base,
        }
    }
}

/// The hint for a document with `signals` shown in `profile`: two or more
/// distinct signals the profile does not read itself.
#[must_use]
pub fn suggest(signals: &Signals, profile: &CompiledProfile) -> Option<DialectHint> {
    let foreign: Vec<Signal> = signals
        .as_slice()
        .iter()
        .copied()
        .filter(|s| !s.native_to(profile))
        .collect();
    if foreign.len() < 2 {
        return None;
    }
    // The dialect most signals point to; ties go to the first in signal
    // order.
    let mut best: Option<(Requested, usize)> = None;
    for signal in Signal::ALL.into_iter().filter(|s| foreign.contains(s)) {
        for &dialect in signal.dialects() {
            let votes = foreign
                .iter()
                .filter(|s| s.dialects().contains(&dialect))
                .count();
            if best.is_none_or(|(_, b)| votes > b) {
                best = Some((dialect, votes));
            }
        }
    }
    let (dialect, _) = best?;
    if dialect == Requested::BuiltIn(profile.id) {
        return None;
    }
    Some(DialectHint {
        dialect,
        signals: foreign,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::compile_builtin;

    #[test]
    fn each_signal_is_found() {
        let cases = [
            (
                "!!! note \"Title\"\n    Body text\n",
                Signal::MkDocsAdmonition,
            ),
            (
                ":::note[Careful]\nText\n:::\n",
                Signal::DocusaurusAdmonition,
            ),
            ("## Setup {.unnumbered}\n", Signal::HeadingAttributes),
            ("# Intro {#intro}\n", Signal::HeadingAttributes),
            ("[[_TOC_]]\n", Signal::GitLabToc),
            ("Text %%hidden%%\n", Signal::PercentComment),
            ("Claim.^[A note.]\n", Signal::InlineFootnote),
            ("> [!tip]- Folded\n> body\n", Signal::FoldingCallout),
        ];
        for (text, signal) in cases {
            assert_eq!(scan(text).as_slice(), [signal], "{text:?}");
        }
    }

    #[test]
    fn near_misses_are_not_signals() {
        for text in [
            "!!! note\nnot indented\n",
            "!!! note\n\n    indented after a blank line counts\n",
            ":::note\n",
            "## Plain {heading}\n",
            "Text {.class} not a heading\n",
            "> [!tip] Not folded\n",
            "```\n%% in code ^[x]\n[[_TOC_]]\n```\n",
            "    [[_TOC_]] indented code\n",
        ] {
            let found = scan(text);
            let expected: &[Signal] = if text.contains("blank line counts") {
                &[Signal::MkDocsAdmonition]
            } else {
                &[]
            };
            assert_eq!(found.as_slice(), expected, "{text:?}");
        }
    }

    #[test]
    fn two_distinct_signals_make_a_hint() {
        let one = scan("!!! note\n    Body\n\n!!! warning\n    Again\n");
        assert_eq!(suggest(&one, &compile_builtin(ProfileId::GfmFile)), None);

        let mkdocs = scan("!!! note\n    Body\n\n[[_TOC_]]\n:::tip[x]\n:::\n");
        let hint = suggest(&mkdocs, &compile_builtin(ProfileId::GfmFile)).expect("hint");
        assert_eq!(hint.dialect, Requested::Planned(PlannedProfile::MkDocs));
        assert_eq!(
            hint.message(),
            "This looks like MkDocs. Switch profile? (full support in 1.2)"
        );
    }

    #[test]
    fn the_profiles_own_syntax_does_not_count() {
        let obsidian = scan("Text %%comment%%\n\nA claim.^[note]\n\n> [!info]- x\n");
        let hint = suggest(&obsidian, &compile_builtin(ProfileId::CommonMark)).expect("hint");
        assert_eq!(hint.dialect, Requested::BuiltIn(ProfileId::Obsidian));
        assert_eq!(hint.message(), "This looks like Obsidian. Switch profile?");
        // Extended reads `^[`; `%%` and folding callouts still point to
        // Obsidian.
        let hint = suggest(&obsidian, &compile_builtin(ProfileId::Extended)).expect("hint");
        assert_eq!(
            hint.signals,
            [Signal::PercentComment, Signal::FoldingCallout]
        );
        // Obsidian reads all three.
        assert_eq!(
            suggest(&obsidian, &compile_builtin(ProfileId::Obsidian)),
            None
        );

        let pandoc = scan("# A {#a}\n\nText.^[note]\n");
        assert_eq!(
            suggest(&pandoc, &compile_builtin(ProfileId::Extended)),
            None
        );
        let hint = suggest(&pandoc, &compile_builtin(ProfileId::GfmFile)).expect("hint");
        assert_eq!(hint.dialect, Requested::Planned(PlannedProfile::Pandoc));
    }
}
