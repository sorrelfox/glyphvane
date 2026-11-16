// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The switch matrix: every syntax and rendering choice on which profiles
//! differ, as typed values ([`Switches`]).
//!
//! Each switch has a key, the name `.glyphvane.toml` uses under
//! `[switches]` ([`Switches::KEYS`]), and a value that is a boolean, a
//! small integer or one of a few words. [`Switches::set`] is the only way
//! a value read from a file reaches a profile, so it is also the validator.
//!
//! Link resolution (link root, lookup order, slug algorithm for links) is
//! per profile in the vault crate's resolver parameters, not a switch here.

use std::fmt;

/// A switch value as written in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchValue {
    Bool(bool),
    Int(i64),
    Word(String),
}

impl fmt::Display for SwitchValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int(n) => write!(f, "{n}"),
            Self::Word(w) => write!(f, "\"{w}\""),
        }
    }
}

/// Why a switch value was not taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchError {
    /// No switch has this key.
    UnknownKey,
    /// The value has the wrong type or is out of range; the text says what
    /// the switch takes.
    Invalid { expected: String },
}

macro_rules! switch_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $word:literal,)+ }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant,)+
        }

        impl $name {
            /// Every value, as files spell them.
            pub const WORDS: &'static [&'static str] = &[$($word),+];

            /// The word a file uses for the value.
            #[must_use]
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $word,)+
                }
            }

            /// The value a file's word names.
            #[must_use]
            pub fn from_word(word: &str) -> Option<Self> {
                match word {
                    $($word => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

switch_enum! {
    /// How a single newline inside a paragraph renders.
    SoftBreak {
        /// As a space (CommonMark).
        Space = "space",
        /// As a line break (GitHub comments, Obsidian).
        Br = "br",
    }
}

switch_enum! {
    /// Which raw HTML survives the sanitizer.
    RawHtml {
        /// Glyphvane's sanitized subset.
        Sanitized = "sanitized",
        /// GitHub's allow-list (no `style` attributes).
        GitHub = "github",
    }
}

switch_enum! {
    /// Whether Markdown inside raw HTML blocks is parsed.
    HtmlMarkdown {
        /// The CommonMark rule: after a blank line.
        CommonMark = "commonmark",
        /// Never (Obsidian).
        Never = "never",
        /// Only inside elements with `markdown="1"`.
        Attribute = "attribute",
    }
}

switch_enum! {
    /// Task list items.
    TaskLists {
        /// No task items.
        Off = "off",
        /// `[ ]` and `[x]`.
        Standard = "standard",
        /// Any single character between the brackets (Obsidian).
        AnyCharacter = "any",
    }
}

switch_enum! {
    /// What `~x~` means.
    TildeSingle {
        /// Literal text.
        Literal = "literal",
        /// Strikethrough (GFM).
        Strike = "strike",
        /// Subscript (Pandoc, Markdig).
        Subscript = "subscript",
    }
}

switch_enum! {
    /// How `> [!type]` block quotes are read.
    Admonitions {
        /// Plain block quotes.
        None = "none",
        /// GitHub alerts: five types, no title, no folding, no nesting.
        Alerts = "alerts",
        /// Obsidian callouts: titles, folding, nesting, custom types.
        Callouts = "callouts",
    }
}

switch_enum! {
    /// Math delimiters.
    Math {
        /// No math.
        Off = "off",
        /// GitHub: `$…$`, `` $`…`$ ``, `$$…$$` and ` ```math `.
        GitHub = "github",
        /// Obsidian: `$…$` and `$$…$$`.
        Obsidian = "obsidian",
    }
}

switch_enum! {
    /// The primary heading-anchor algorithm.
    Slugs {
        /// No heading anchors (GitHub comments).
        None = "none",
        /// github-slugger.
        GitHub = "github",
        /// The heading text itself (Obsidian).
        HeadingText = "heading-text",
    }
}

switch_enum! {
    /// What follows the language on a code fence's info line.
    FenceMeta {
        /// Nothing is read.
        None = "none",
        /// Ignored after the language (GitHub).
        Ignored = "ignored",
        /// Title, line highlights and line numbers (Extended).
        Rich = "rich",
    }
}

switch_enum! {
    /// Wikilink pipe order.
    WikiPipe {
        /// `[[target|title]]` (Obsidian).
        TargetTitle = "target-title",
        /// `[[title|target]]` (GitLab, Gollum).
        TitleTarget = "title-target",
    }
}

/// Front matter syntaxes recognized at the start of a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FrontMatter {
    /// `---` … `---`.
    pub yaml: bool,
    /// `+++` … `+++`.
    pub toml: bool,
}

/// Every switch of the matrix. [`Default`] is CommonMark 0.31.2 exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(clippy::struct_excessive_bools)] // One flag per construct, by design.
pub struct Switches {
    // Blocks and rendering.
    pub soft_break: SoftBreak,
    pub raw_html: RawHtml,
    pub html_markdown: HtmlMarkdown,
    /// GFM pipe tables.
    pub tables: bool,
    /// Dashes each delimiter-row cell needs.
    pub table_min_dashes: u8,
    pub task_lists: TaskLists,
    /// Link reference definitions may be indented by up to this many
    /// spaces (0–3).
    pub refdef_max_indent: u8,
    pub front_matter: FrontMatter,
    pub admonitions: Admonitions,
    /// `[^1]` footnotes.
    pub footnotes: bool,
    /// Continuation indent of a footnote definition (4, or 2 in Obsidian).
    pub footnote_indent: u8,
    /// `^[…]` inline footnotes.
    pub inline_footnotes: bool,
    pub definition_lists: bool,
    /// `*[X]: …` abbreviations.
    pub abbreviations: bool,
    /// `:::` Pandoc fenced divs.
    pub fenced_divs: bool,
    /// `{#id .class key=val}` attributes.
    pub attributes: bool,
    /// A paragraph of only `[TOC]`.
    pub toc_marker: bool,
    pub fence_meta: FenceMeta,
    /// ` ```mermaid ` diagrams.
    pub mermaid: bool,
    pub math: Math,
    /// `\(…\)` and `\[…\]` math.
    pub math_brackets: bool,
    pub slugs: Slugs,
    // Inlines.
    pub tilde_single: TildeSingle,
    /// `~~x~~` strikethrough.
    pub tilde_double: bool,
    /// `^x^` superscript.
    pub superscript: bool,
    /// `==x==` highlight.
    pub highlight: bool,
    /// `++x++` insert.
    pub insert: bool,
    /// `[[…]]` wikilinks.
    pub wikilinks: bool,
    /// `![[…]]` embeds.
    pub embeds: bool,
    pub wiki_pipe: WikiPipe,
    /// `%%…%%` comments.
    pub comments: bool,
    /// `#tag`.
    pub tags: bool,
    /// `^block-id`.
    pub block_ids: bool,
    /// `:shortcode:` emoji (gemoji).
    pub emoji: bool,
    /// Bare `www.` and `https://` links.
    pub extended_autolinks: bool,
    /// `@mentions`, `#123` references and commit SHAs styled.
    pub github_references: bool,
    /// Colour chips after `` `#rrggbb` `` code spans.
    pub colour_chips: bool,
    /// Curly quotes, dashes and ellipses.
    pub smart_punctuation: bool,
}

impl Default for Switches {
    fn default() -> Self {
        Self::COMMONMARK
    }
}

/// The kind of value a switch takes, for messages.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Bool,
    Int(i64, i64),
    Words(&'static [&'static str]),
}

impl Kind {
    fn expected(self) -> String {
        match self {
            Self::Bool => "true or false".to_owned(),
            Self::Int(lo, hi) => format!("an integer from {lo} to {hi}"),
            Self::Words(words) => {
                let quoted: Vec<String> = words.iter().map(|w| format!("\"{w}\"")).collect();
                format!("one of {}", quoted.join(", "))
            }
        }
    }
}

impl Switches {
    /// CommonMark 0.31.2 exactly: every extension off.
    pub const COMMONMARK: Switches = Switches {
        soft_break: SoftBreak::Space,
        raw_html: RawHtml::Sanitized,
        html_markdown: HtmlMarkdown::CommonMark,
        tables: false,
        table_min_dashes: 1,
        task_lists: TaskLists::Off,
        refdef_max_indent: 3,
        front_matter: FrontMatter {
            yaml: false,
            toml: false,
        },
        admonitions: Admonitions::None,
        footnotes: false,
        footnote_indent: 4,
        inline_footnotes: false,
        definition_lists: false,
        abbreviations: false,
        fenced_divs: false,
        attributes: false,
        toc_marker: false,
        fence_meta: FenceMeta::None,
        mermaid: false,
        math: Math::Off,
        math_brackets: false,
        slugs: Slugs::GitHub,
        tilde_single: TildeSingle::Literal,
        tilde_double: false,
        superscript: false,
        highlight: false,
        insert: false,
        wikilinks: false,
        embeds: false,
        wiki_pipe: WikiPipe::TargetTitle,
        comments: false,
        tags: false,
        block_ids: false,
        emoji: false,
        extended_autolinks: false,
        github_references: false,
        colour_chips: false,
        smart_punctuation: false,
    };

    /// Every switch key, in matrix order.
    pub const KEYS: &'static [&'static str] = &[
        "soft_break",
        "raw_html",
        "html_markdown",
        "tables",
        "table_min_dashes",
        "task_lists",
        "refdef_max_indent",
        "front_matter_yaml",
        "front_matter_toml",
        "admonitions",
        "footnotes",
        "footnote_indent",
        "inline_footnotes",
        "definition_lists",
        "abbreviations",
        "fenced_divs",
        "attributes",
        "toc_marker",
        "fence_meta",
        "mermaid",
        "math",
        "math_brackets",
        "slugs",
        "tilde_single",
        "tilde_double",
        "superscript",
        "highlight",
        "insert",
        "wikilinks",
        "embeds",
        "wiki_pipe",
        "comments",
        "tags",
        "block_ids",
        "emoji",
        "extended_autolinks",
        "github_references",
        "colour_chips",
        "smart_punctuation",
    ];

    /// The kind of value switch `key` takes.
    fn kind(key: &str) -> Option<Kind> {
        Some(match key {
            "soft_break" => Kind::Words(SoftBreak::WORDS),
            "raw_html" => Kind::Words(RawHtml::WORDS),
            "html_markdown" => Kind::Words(HtmlMarkdown::WORDS),
            "task_lists" => Kind::Words(TaskLists::WORDS),
            "admonitions" => Kind::Words(Admonitions::WORDS),
            "fence_meta" => Kind::Words(FenceMeta::WORDS),
            "math" => Kind::Words(Math::WORDS),
            "slugs" => Kind::Words(Slugs::WORDS),
            "tilde_single" => Kind::Words(TildeSingle::WORDS),
            "wiki_pipe" => Kind::Words(WikiPipe::WORDS),
            "table_min_dashes" => Kind::Int(1, 3),
            "refdef_max_indent" => Kind::Int(0, 3),
            "footnote_indent" => Kind::Int(2, 4),
            key if Self::KEYS.contains(&key) => Kind::Bool,
            _ => return None,
        })
    }

    /// The value of switch `key`; `None` for an unknown key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<SwitchValue> {
        let word = |w: &'static str| Some(SwitchValue::Word(w.to_owned()));
        let int = |n: u8| Some(SwitchValue::Int(i64::from(n)));
        let flag = |b: bool| Some(SwitchValue::Bool(b));
        match key {
            "soft_break" => word(self.soft_break.as_str()),
            "raw_html" => word(self.raw_html.as_str()),
            "html_markdown" => word(self.html_markdown.as_str()),
            "tables" => flag(self.tables),
            "table_min_dashes" => int(self.table_min_dashes),
            "task_lists" => word(self.task_lists.as_str()),
            "refdef_max_indent" => int(self.refdef_max_indent),
            "front_matter_yaml" => flag(self.front_matter.yaml),
            "front_matter_toml" => flag(self.front_matter.toml),
            "admonitions" => word(self.admonitions.as_str()),
            "footnotes" => flag(self.footnotes),
            "footnote_indent" => int(self.footnote_indent),
            "inline_footnotes" => flag(self.inline_footnotes),
            "definition_lists" => flag(self.definition_lists),
            "abbreviations" => flag(self.abbreviations),
            "fenced_divs" => flag(self.fenced_divs),
            "attributes" => flag(self.attributes),
            "toc_marker" => flag(self.toc_marker),
            "fence_meta" => word(self.fence_meta.as_str()),
            "mermaid" => flag(self.mermaid),
            "math" => word(self.math.as_str()),
            "math_brackets" => flag(self.math_brackets),
            "slugs" => word(self.slugs.as_str()),
            "tilde_single" => word(self.tilde_single.as_str()),
            "tilde_double" => flag(self.tilde_double),
            "superscript" => flag(self.superscript),
            "highlight" => flag(self.highlight),
            "insert" => flag(self.insert),
            "wikilinks" => flag(self.wikilinks),
            "embeds" => flag(self.embeds),
            "wiki_pipe" => word(self.wiki_pipe.as_str()),
            "comments" => flag(self.comments),
            "tags" => flag(self.tags),
            "block_ids" => flag(self.block_ids),
            "emoji" => flag(self.emoji),
            "extended_autolinks" => flag(self.extended_autolinks),
            "github_references" => flag(self.github_references),
            "colour_chips" => flag(self.colour_chips),
            "smart_punctuation" => flag(self.smart_punctuation),
            _ => None,
        }
    }

    /// Sets switch `key` to `value`, or says why it cannot.
    ///
    /// # Errors
    ///
    /// [`SwitchError::UnknownKey`] for a key not in [`Switches::KEYS`];
    /// [`SwitchError::Invalid`] for a value of the wrong type or range.
    pub fn set(&mut self, key: &str, value: &SwitchValue) -> Result<(), SwitchError> {
        let kind = Self::kind(key).ok_or(SwitchError::UnknownKey)?;
        let invalid = || SwitchError::Invalid {
            expected: kind.expected(),
        };
        match (kind, value) {
            (Kind::Bool, SwitchValue::Bool(b)) => {
                *self.flag_mut(key).ok_or_else(invalid)? = *b;
            }
            (Kind::Int(lo, hi), SwitchValue::Int(n)) if (lo..=hi).contains(n) => {
                let n = u8::try_from(*n).map_err(|_| invalid())?;
                match key {
                    "table_min_dashes" => self.table_min_dashes = n,
                    "refdef_max_indent" => self.refdef_max_indent = n,
                    "footnote_indent" if n == 2 || n == 4 => self.footnote_indent = n,
                    _ => {
                        return Err(SwitchError::Invalid {
                            expected: "2 or 4".to_owned(),
                        });
                    }
                }
            }
            (Kind::Words(_), SwitchValue::Word(w)) => {
                let w = w.as_str();
                let set = match key {
                    "soft_break" => SoftBreak::from_word(w).map(|v| self.soft_break = v),
                    "raw_html" => RawHtml::from_word(w).map(|v| self.raw_html = v),
                    "html_markdown" => HtmlMarkdown::from_word(w).map(|v| self.html_markdown = v),
                    "task_lists" => TaskLists::from_word(w).map(|v| self.task_lists = v),
                    "admonitions" => Admonitions::from_word(w).map(|v| self.admonitions = v),
                    "fence_meta" => FenceMeta::from_word(w).map(|v| self.fence_meta = v),
                    "math" => Math::from_word(w).map(|v| self.math = v),
                    "slugs" => Slugs::from_word(w).map(|v| self.slugs = v),
                    "tilde_single" => TildeSingle::from_word(w).map(|v| self.tilde_single = v),
                    "wiki_pipe" => WikiPipe::from_word(w).map(|v| self.wiki_pipe = v),
                    _ => None,
                };
                set.ok_or_else(invalid)?;
            }
            _ => return Err(invalid()),
        }
        Ok(())
    }

    fn flag_mut(&mut self, key: &str) -> Option<&mut bool> {
        Some(match key {
            "tables" => &mut self.tables,
            "front_matter_yaml" => &mut self.front_matter.yaml,
            "front_matter_toml" => &mut self.front_matter.toml,
            "footnotes" => &mut self.footnotes,
            "inline_footnotes" => &mut self.inline_footnotes,
            "definition_lists" => &mut self.definition_lists,
            "abbreviations" => &mut self.abbreviations,
            "fenced_divs" => &mut self.fenced_divs,
            "attributes" => &mut self.attributes,
            "toc_marker" => &mut self.toc_marker,
            "mermaid" => &mut self.mermaid,
            "math_brackets" => &mut self.math_brackets,
            "tilde_double" => &mut self.tilde_double,
            "superscript" => &mut self.superscript,
            "highlight" => &mut self.highlight,
            "insert" => &mut self.insert,
            "wikilinks" => &mut self.wikilinks,
            "embeds" => &mut self.embeds,
            "comments" => &mut self.comments,
            "tags" => &mut self.tags,
            "block_ids" => &mut self.block_ids,
            "emoji" => &mut self.emoji,
            "extended_autolinks" => &mut self.extended_autolinks,
            "github_references" => &mut self.github_references,
            "colour_chips" => &mut self.colour_chips,
            "smart_punctuation" => &mut self.smart_punctuation,
            _ => return None,
        })
    }

    /// The keys whose values differ between `self` and `other`, in matrix
    /// order.
    #[must_use]
    pub fn differences(&self, other: &Switches) -> Vec<&'static str> {
        Self::KEYS
            .iter()
            .copied()
            .filter(|key| self.get(key) != other.get(key))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_reads_and_writes() {
        for key in Switches::KEYS {
            let mut switches = Switches::COMMONMARK;
            let value = switches.get(key).expect("readable");
            switches.set(key, &value).expect("writable");
            assert_eq!(switches, Switches::COMMONMARK, "{key}");
        }
    }

    #[test]
    fn values_are_validated() {
        let mut s = Switches::COMMONMARK;
        assert_eq!(
            s.set("nope", &SwitchValue::Bool(true)),
            Err(SwitchError::UnknownKey)
        );
        assert!(matches!(
            s.set("tilde_single", &SwitchValue::Word("sub".into())),
            Err(SwitchError::Invalid { expected }) if expected.contains("\"subscript\"")
        ));
        assert!(s.set("tables", &SwitchValue::Word("yes".into())).is_err());
        assert!(s.set("refdef_max_indent", &SwitchValue::Int(4)).is_err());
        assert!(s.set("footnote_indent", &SwitchValue::Int(3)).is_err());
        assert_eq!(s, Switches::COMMONMARK);
        s.set("tilde_single", &SwitchValue::Word("strike".into()))
            .expect("valid");
        s.set("smart_punctuation", &SwitchValue::Bool(true))
            .expect("valid");
        s.set("table_min_dashes", &SwitchValue::Int(2))
            .expect("valid");
        assert_eq!(s.tilde_single, TildeSingle::Strike);
        assert!(s.smart_punctuation);
        assert_eq!(s.table_min_dashes, 2);
        assert_eq!(
            s.differences(&Switches::COMMONMARK),
            ["table_min_dashes", "tilde_single", "smart_punctuation"]
        );
    }
}
