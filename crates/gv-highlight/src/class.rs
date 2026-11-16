// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Token classes: what a highlighted span *is*, independent of any theme.
//!
//! The engine never produces colours. It runs the grammars with a generated
//! classifier theme ([`classifier_theme_json`]) whose rules give every class
//! its own sentinel colour, then maps the resolved colour back to the class.
//! Rules cascade like a VS Code theme: a deeper scope overrides its parents,
//! and a longer selector beats a shorter one at the same depth.

/// The class of a highlighted span. The UI colours each class with a design
/// token ([`TokenClass::token`]), so one highlighting result serves every
/// theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TokenClass {
    /// Unclassified text (identifiers without a role, whitespace). Never
    /// emitted as a span: gaps between spans are plain.
    Plain,
    /// Comments.
    Comment,
    /// Keywords, storage keywords and modifiers, `self`/`this`.
    Keyword,
    /// Symbolic operators.
    Operator,
    /// String literals, including their quotes.
    String,
    /// Escapes and format placeholders inside strings.
    Escape,
    /// Regular expression literals.
    Regex,
    /// Numeric literals and units.
    Number,
    /// Language constants (`true`, `null`), symbols, enum members.
    Constant,
    /// Function and method names, macros, decorators.
    Function,
    /// Type, class, trait and namespace names.
    Type,
    /// Variables and parameters.
    Variable,
    /// Keys of maps and objects (JSON, YAML, TOML, INI), properties.
    Property,
    /// Markup tag names (HTML, XML, JSX).
    Tag,
    /// Markup attribute names, Rust attributes.
    Attribute,
    /// Punctuation and delimiters.
    Punctuation,
    /// Headings in markup languages; diff hunk headers.
    Heading,
    /// Emphasis in markup languages.
    Emphasis,
    /// Strong emphasis in markup languages.
    Strong,
    /// Link targets and URLs in markup languages.
    Link,
    /// Inline and block code in markup languages.
    Code,
    /// Inserted lines (diff).
    Inserted,
    /// Deleted lines (diff).
    Deleted,
    /// Changed lines (diff).
    Changed,
}

/// The style a class asks for besides its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FontStyle {
    /// Draw in the bold weight.
    pub bold: bool,
    /// Draw in italics.
    pub italic: bool,
}

impl TokenClass {
    /// Every class, in a fixed order (the sentinel colour index).
    pub const ALL: [Self; 24] = [
        Self::Plain,
        Self::Comment,
        Self::Keyword,
        Self::Operator,
        Self::String,
        Self::Escape,
        Self::Regex,
        Self::Number,
        Self::Constant,
        Self::Function,
        Self::Type,
        Self::Variable,
        Self::Property,
        Self::Tag,
        Self::Attribute,
        Self::Punctuation,
        Self::Heading,
        Self::Emphasis,
        Self::Strong,
        Self::Link,
        Self::Code,
        Self::Inserted,
        Self::Deleted,
        Self::Changed,
    ];

    /// The class's stable name (`keyword`, `string`, ...), for dumps and
    /// theme files.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Comment => "comment",
            Self::Keyword => "keyword",
            Self::Operator => "operator",
            Self::String => "string",
            Self::Escape => "escape",
            Self::Regex => "regex",
            Self::Number => "number",
            Self::Constant => "constant",
            Self::Function => "function",
            Self::Type => "type",
            Self::Variable => "variable",
            Self::Property => "property",
            Self::Tag => "tag",
            Self::Attribute => "attribute",
            Self::Punctuation => "punctuation",
            Self::Heading => "heading",
            Self::Emphasis => "emphasis",
            Self::Strong => "strong",
            Self::Link => "link",
            Self::Code => "code",
            Self::Inserted => "inserted",
            Self::Deleted => "deleted",
            Self::Changed => "changed",
        }
    }

    /// The design token that colours this class (`syntax.*` where one
    /// fits, else a text or status token).
    pub const fn token(self) -> &'static str {
        match self {
            Self::Plain | Self::Variable | Self::Emphasis | Self::Strong => "text.primary",
            Self::Comment => "text.tertiary",
            Self::Operator | Self::Punctuation => "text.secondary",
            Self::Keyword => "syntax.tag",
            Self::String | Self::Regex => "syntax.string",
            Self::Escape | Self::Number | Self::Constant => "syntax.number",
            Self::Function | Self::Property | Self::Attribute | Self::Heading => "syntax.key",
            Self::Type | Self::Tag | Self::Code => "syntax.code",
            Self::Link => "syntax.url",
            Self::Inserted => "status.success",
            Self::Deleted => "status.error",
            Self::Changed => "status.warning",
        }
    }

    /// Bold or italic, for the classes that carry it.
    pub const fn font_style(self) -> FontStyle {
        match self {
            Self::Comment | Self::Emphasis => FontStyle {
                bold: false,
                italic: true,
            },
            Self::Strong | Self::Heading => FontStyle {
                bold: true,
                italic: false,
            },
            _ => FontStyle {
                bold: false,
                italic: false,
            },
        }
    }

    /// The class with this [`name`](Self::name).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }

    /// The sentinel colour of the class in the classifier theme.
    pub(crate) fn sentinel(self) -> String {
        format!("#0001{:02X}", self as u8)
    }
}

/// The classifier theme's name in the giallo registry.
pub(crate) const CLASSIFIER_THEME: &str = "glyphvane-classes";

/// TextMate scope selectors for each class. Plain resets embedded code
/// (template expressions, fenced code in Markdown) to unclassified so the
/// embedded language's own classes show.
const RULES: &[(TokenClass, &[&str])] = &[
    (
        TokenClass::Plain,
        &[
            "meta.embedded",
            "meta.template.expression",
            "source.groovy.embedded",
            "string meta.embedded",
        ],
    ),
    (
        TokenClass::Comment,
        &[
            "comment",
            "punctuation.definition.comment",
            "string.comment",
        ],
    ),
    (
        TokenClass::Keyword,
        &[
            "keyword",
            "keyword.control",
            "keyword.other",
            "storage",
            "storage.type",
            "storage.modifier",
            "variable.language",
            "keyword.operator.new",
            "keyword.operator.expression",
            "keyword.operator.word",
            "keyword.operator.wordlike",
            "keyword.operator.logical.python",
            "keyword.operator.sizeof",
            "keyword.operator.instanceof",
            "keyword.operator.delete",
            "keyword.operator.cast",
            "punctuation.definition.keyword",
        ],
    ),
    (
        TokenClass::Operator,
        &[
            "keyword.operator",
            "punctuation.separator.key-value.mapping",
        ],
    ),
    (
        TokenClass::String,
        &[
            "string",
            "punctuation.definition.string",
            "string.quoted",
            "string.unquoted.plain",
            "storage.type.string",
        ],
    ),
    (
        TokenClass::Escape,
        &[
            "constant.character.escape",
            "constant.character.format.placeholder",
            "constant.other.placeholder",
            "constant.character.string.escape",
        ],
    ),
    (
        TokenClass::Regex,
        &[
            "string.regexp",
            "source.regexp",
            "string.regexp keyword",
            "string.regexp punctuation",
            "string.regexp constant",
        ],
    ),
    (
        TokenClass::Number,
        &[
            "constant.numeric",
            "keyword.other.unit",
            "constant.other.color",
        ],
    ),
    (
        TokenClass::Constant,
        &[
            "constant",
            "constant.language",
            "constant.other",
            "constant.character",
            "constant.other.symbol",
            "support.constant",
            "variable.other.constant",
            "variable.other.enummember",
            "entity.name.constant",
        ],
    ),
    (
        TokenClass::Function,
        &[
            "entity.name.function",
            "support.function",
            "variable.function",
            "meta.function-call.generic",
            "entity.name.function.macro",
            "entity.name.function.decorator",
            "meta.decorator",
            "support.function.builtin",
            "entity.name.command",
        ],
    ),
    (
        TokenClass::Type,
        &[
            "entity.name.type",
            "entity.name.class",
            "entity.name.struct",
            "entity.name.enum",
            "entity.name.interface",
            "entity.name.trait",
            "entity.name.namespace",
            "entity.name.module",
            "entity.other.inherited-class",
            "support.type",
            "support.class",
            "storage.type.primitive",
            "storage.type.built-in",
            "entity.name.type.primitive",
            "entity.name.scope-resolution",
        ],
    ),
    (
        TokenClass::Variable,
        &[
            "variable",
            "variable.parameter",
            "variable.other",
            "support.variable",
            "punctuation.definition.variable",
        ],
    ),
    (
        TokenClass::Property,
        &[
            "support.type.property-name",
            "meta.object-literal.key",
            "variable.other.property",
            "variable.other.object.property",
            "entity.name.tag.yaml",
            "keyword.other.definition.ini",
            "entity.name.tag.toml",
            "support.type.property-name.toml",
            "variable.other.key.toml",
            "meta.mapping.key",
            "entity.other.attribute-name.table.toml",
        ],
    ),
    (
        TokenClass::Tag,
        &["entity.name.tag", "support.class.component"],
    ),
    (
        TokenClass::Attribute,
        &[
            "entity.other.attribute-name",
            "meta.attribute.rust",
            "storage.type.annotation",
            "punctuation.definition.annotation",
        ],
    ),
    (
        TokenClass::Punctuation,
        &[
            "punctuation",
            "meta.brace",
            "punctuation.definition.tag",
            "punctuation.separator",
            "punctuation.terminator",
            "punctuation.accessor",
        ],
    ),
    (
        TokenClass::Heading,
        &[
            "markup.heading",
            "entity.name.section",
            "punctuation.definition.heading",
            "meta.diff.header",
            "meta.diff.range",
            "meta.separator.diff",
        ],
    ),
    (TokenClass::Emphasis, &["markup.italic"]),
    (TokenClass::Strong, &["markup.bold"]),
    (
        TokenClass::Link,
        &[
            "markup.underline.link",
            "string.other.link",
            "constant.other.reference.link",
            "markup.underline.link.image",
        ],
    ),
    (
        TokenClass::Code,
        &[
            "markup.inline.raw",
            "markup.inline.raw.string",
            "markup.raw",
            "markup.fenced_code",
            "punctuation.definition.raw",
        ],
    ),
    (TokenClass::Inserted, &["markup.inserted"]),
    (TokenClass::Deleted, &["markup.deleted"]),
    (TokenClass::Changed, &["markup.changed"]),
];

/// The classifier theme as VS Code theme JSON.
pub(crate) fn classifier_theme_json() -> String {
    let mut rules = Vec::new();
    for (class, scopes) in RULES {
        rules.push(serde_json::json!({
            "scope": scopes,
            "settings": { "foreground": class.sentinel(), "fontStyle": "" },
        }));
    }
    serde_json::json!({
        "name": CLASSIFIER_THEME,
        "type": "light",
        "colors": {
            "editor.foreground": TokenClass::Plain.sentinel(),
            "editor.background": "#FFFFFF",
        },
        "tokenColors": rules,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for class in TokenClass::ALL {
            assert_eq!(TokenClass::from_name(class.name()), Some(class));
        }
        assert_eq!(TokenClass::ALL.len(), TokenClass::Changed as usize + 1);
    }

    #[test]
    fn sentinels_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for class in TokenClass::ALL {
            assert!(seen.insert(class.sentinel()));
        }
    }

    #[test]
    fn tokens_are_design_tokens() {
        for class in TokenClass::ALL {
            let token = class.token();
            assert!(
                ["syntax.", "text.", "status."]
                    .iter()
                    .any(|p| token.starts_with(p)),
                "{token}"
            );
        }
    }
}
