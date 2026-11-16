// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The lossless block structure: a flat stream of events, stored per line.
//!
//! Every byte of the document is covered by exactly one [`Token`]. Blocks are
//! delimited by [`Event::Open`] / [`Event::Close`] pairs that nest strictly.
//!
//! **Events are append-only per line**: once emitted for a line, an event
//! never changes because of later lines. Whatever CommonMark decides
//! retroactively is emitted as a *marker* at the point where it is decided:
//!
//! - a setext underline emits [`Event::Setext`] on the underline line (the
//!   text block's `Open` stays a neutral [`BlockKind::Text`]);
//! - link reference definitions at the start of a text block are emitted as
//!   [`Event::RefDef`] markers once their extent is final;
//! - list looseness is an [`Event::Loosen`] marker, emitted where cmark's
//!   tightness check would find the blank line (when the next sibling
//!   starts). A list is tight iff it *directly* contains no such marker.
//!
//! Internally an event is packed into 8 bytes ([`Ev`]); BlockIds are kept
//! in a parallel array, one per `Open`.

/// Stable identity of a block. Survives reparses in which the block still
/// exists (see the crate documentation for the matching rule).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct BlockId(pub u64);

/// Token kinds. Tokens never nest; together they cover every byte.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum TokenKind {
    /// Byte-order mark at the start of the document.
    Bom = 0,
    /// Spaces and tabs that belong to no marker or content.
    Whitespace = 1,
    /// The continuation prefix of the containers open on a line (block
    /// quote markers and list item indentation), as one run.
    ContainerPrefix = 2,
    /// A block quote marker (`>` and the optional following space) on the
    /// line that opens the block quote, with its indentation.
    QuoteMarker = 3,
    /// A list item marker with its indentation and padding, on the item's
    /// first line.
    ListMarker = 4,
    /// Indentation consumed by a code block (4 columns of an indented code
    /// line, or the fence's indentation removed from a fenced code line).
    CodeIndent = 5,
    /// Structural punctuation: `#` runs, setext underlines, thematic breaks,
    /// code fences, front matter delimiters.
    Marker = 6,
    /// The info string of a fenced code block.
    InfoString = 7,
    /// Leaf content: paragraph or heading text, a code or HTML line.
    Content = 8,
    /// `\n`, `\r\n` or `\r`.
    LineEnding = 9,
    /// The marker of an extension on a container's first line, right
    /// after the container opens: a task item's checkbox (`[ ]`, `[x]`), a
    /// footnote definition's label (`[^label]:`), an alert's type
    /// (`[!NOTE]`). See [`Container::marker`](crate::Container::marker).
    ExtMarker = 10,
}

impl TokenKind {
    const ALL: [TokenKind; 11] = [
        TokenKind::Bom,
        TokenKind::Whitespace,
        TokenKind::ContainerPrefix,
        TokenKind::QuoteMarker,
        TokenKind::ListMarker,
        TokenKind::CodeIndent,
        TokenKind::Marker,
        TokenKind::InfoString,
        TokenKind::Content,
        TokenKind::LineEnding,
        TokenKind::ExtMarker,
    ];

    /// The kind's name in the CST dump.
    pub fn name(self) -> &'static str {
        match self {
            TokenKind::Bom => "bom",
            TokenKind::Whitespace => "ws",
            TokenKind::ContainerPrefix => "prefix",
            TokenKind::QuoteMarker => "quote-marker",
            TokenKind::ListMarker => "list-marker",
            TokenKind::CodeIndent => "code-indent",
            TokenKind::Marker => "marker",
            TokenKind::InfoString => "info",
            TokenKind::Content => "content",
            TokenKind::LineEnding => "line-ending",
            TokenKind::ExtMarker => "ext-marker",
        }
    }
}

/// A token: `len` bytes of one kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Token {
    /// What the bytes are.
    pub kind: TokenKind,
    /// Virtual spaces that precede the content because a tab was only
    /// partially consumed by a container prefix (the tab byte itself
    /// belongs to the prefix token). Only content tokens of code blocks use
    /// it.
    pub virtual_spaces: u8,
    /// Length in bytes (may be 0 only when `virtual_spaces > 0`).
    pub len: u32,
}

/// The syntax of a front matter block (see
/// [`ParseOptions`](crate::ParseOptions)).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum FrontMatterKind {
    /// YAML: `---` ... `---` or `...`.
    Yaml,
    /// TOML: `+++` ... `+++`.
    Toml,
}

impl FrontMatterKind {
    /// The kind's name in the CST dump.
    pub fn name(self) -> &'static str {
        match self {
            FrontMatterKind::Yaml => "yaml",
            FrontMatterKind::Toml => "toml",
        }
    }
}

/// The five GitHub alert types (`> [!NOTE]` …).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum AlertKind {
    /// `[!NOTE]`.
    Note,
    /// `[!TIP]`.
    Tip,
    /// `[!IMPORTANT]`.
    Important,
    /// `[!WARNING]`.
    Warning,
    /// `[!CAUTION]`.
    Caution,
}

impl AlertKind {
    /// Every type, in GitHub's documentation order.
    pub const ALL: [AlertKind; 5] = [
        AlertKind::Note,
        AlertKind::Tip,
        AlertKind::Important,
        AlertKind::Warning,
        AlertKind::Caution,
    ];

    /// The type's name in lower case (`note`), as in GitHub's CSS classes
    /// and the CST dump.
    pub fn name(self) -> &'static str {
        match self {
            AlertKind::Note => "note",
            AlertKind::Tip => "tip",
            AlertKind::Important => "important",
            AlertKind::Warning => "warning",
            AlertKind::Caution => "caution",
        }
    }

    /// The title GitHub shows (`Note`).
    pub fn title(self) -> &'static str {
        match self {
            AlertKind::Note => "Note",
            AlertKind::Tip => "Tip",
            AlertKind::Important => "Important",
            AlertKind::Warning => "Warning",
            AlertKind::Caution => "Caution",
        }
    }

    /// The type named `name`, ignoring ASCII case.
    pub fn from_name(name: &[u8]) -> Option<AlertKind> {
        AlertKind::ALL
            .into_iter()
            .find(|k| k.name().as_bytes().eq_ignore_ascii_case(name))
    }
}

/// The fold state an Obsidian callout's header asks for (`[!type]-` or
/// `[!type]+`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub enum CalloutFold {
    /// No sign: not foldable.
    #[default]
    None,
    /// `+`: foldable, open at first.
    Open,
    /// `-`: foldable, closed at first.
    Closed,
}

impl CalloutFold {
    /// The fold sign's name in the CST dump.
    pub fn name(self) -> &'static str {
        match self {
            CalloutFold::None => "",
            CalloutFold::Open => "+",
            CalloutFold::Closed => "-",
        }
    }

    /// Whether the callout can be folded.
    pub fn is_foldable(self) -> bool {
        self != CalloutFold::None
    }
}

/// Block kinds as known when the block opens.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum BlockKind {
    /// A block quote.
    BlockQuote,
    /// A bullet list; `marker` is `-`, `+` or `*`.
    BulletList {
        /// The bullet character.
        marker: u8,
    },
    /// An ordered list.
    OrderedList {
        /// The start number.
        start: u32,
        /// `.` or `)`.
        delimiter: u8,
    },
    /// A list item.
    Item,
    /// A paragraph, a setext heading and/or link reference definitions;
    /// resolved by the [`Event::RefDef`] and [`Event::Setext`] markers
    /// inside it.
    Text,
    /// An ATX heading.
    AtxHeading {
        /// 1 to 6.
        level: u8,
    },
    /// A thematic break.
    ThematicBreak,
    /// A fenced code block.
    FencedCode,
    /// An indented code block.
    IndentedCode,
    /// An HTML block of the given type (1 to 7, CommonMark §4.6).
    Html {
        /// The start condition that matched.
        kind: u8,
    },
    /// Front matter at the start of the document (only with
    /// [`ParseOptions::front_matter`](crate::ParseOptions::front_matter)).
    FrontMatter {
        /// Its syntax.
        kind: FrontMatterKind,
    },
    /// A `$$` display math block (with
    /// [`ParseOptions::math`](crate::ParseOptions::math)): opened by a line
    /// starting with `$$`, closed by a line ending with `$$` (possibly the
    /// same line). Like a fenced code block, it may hold blank lines and
    /// runs to the end of its container when unclosed.
    MathBlock,
    /// A GitHub alert (with [`ParseOptions::alerts`](crate::ParseOptions::alerts)):
    /// a top-level block quote whose first line is only `[!TYPE]`. It
    /// continues like a block quote; the `[!TYPE]` is an
    /// [`TokenKind::ExtMarker`] token.
    Alert {
        /// Its type.
        kind: AlertKind,
    },
    /// A footnote definition (with
    /// [`ParseOptions::footnotes`](crate::ParseOptions::footnotes)):
    /// `[^label]:` and the blocks indented under it. The `[^label]:` and
    /// the spaces after it are an [`TokenKind::ExtMarker`] token.
    FootnoteDefinition,
    /// An Obsidian callout (with
    /// [`ParseOptions::callouts`](crate::ParseOptions::callouts)): a block
    /// quote, at any depth, whose first line starts with `[!type]`, an
    /// optional `+` or `-`, and an optional title. The `[!type]` and the
    /// fold sign are an [`TokenKind::ExtMarker`] token; a title is a
    /// [`BlockKind::CalloutTitle`] leaf.
    Callout {
        /// The fold sign.
        fold: CalloutFold,
    },
    /// The title of an Obsidian callout: the rest of its first line, a
    /// one-line leaf with inline content.
    CalloutTitle,
    /// An Obsidian `%%` block comment (with
    /// [`ParseOptions::comments`](crate::ParseOptions::comments)): opened
    /// by a line starting with exactly `%%` that holds no other `%%`, closed
    /// by the next line holding `%%` (the closing marker token runs to the
    /// end of that line). May hold blank lines; unclosed, it runs to the end
    /// of its container.
    CommentBlock,
}

impl BlockKind {
    /// Coarse tag used for BlockId matching (properties such as a heading
    /// level may change in place without changing the identity).
    pub(crate) fn tag(self) -> u8 {
        match self {
            BlockKind::BlockQuote => 0,
            BlockKind::BulletList { .. } | BlockKind::OrderedList { .. } => 1,
            BlockKind::Item => 2,
            BlockKind::Text => 3,
            BlockKind::AtxHeading { .. } => 4,
            BlockKind::ThematicBreak => 5,
            BlockKind::FencedCode => 6,
            BlockKind::IndentedCode => 7,
            BlockKind::Html { .. } => 8,
            BlockKind::FrontMatter { .. } => 9,
            BlockKind::MathBlock => 10,
            BlockKind::Alert { .. } => 11,
            BlockKind::FootnoteDefinition => 12,
            BlockKind::Callout { .. } => 13,
            BlockKind::CalloutTitle => 14,
            BlockKind::CommentBlock => 15,
        }
    }

    /// Whether the block holds other blocks.
    pub fn is_container(self) -> bool {
        matches!(
            self,
            BlockKind::BlockQuote
                | BlockKind::BulletList { .. }
                | BlockKind::OrderedList { .. }
                | BlockKind::Item
                | BlockKind::Alert { .. }
                | BlockKind::Callout { .. }
                | BlockKind::FootnoteDefinition
        )
    }

    /// Whether the block is a list.
    pub fn is_list(self) -> bool {
        matches!(
            self,
            BlockKind::BulletList { .. } | BlockKind::OrderedList { .. }
        )
    }
}

/// One event of the block stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Event {
    /// A block starts here.
    Open {
        /// Its identity.
        id: BlockId,
        /// Its kind as known when it opened.
        kind: BlockKind,
    },
    /// The innermost open block ends here.
    Close,
    /// Bytes of the document.
    Token(Token),
    /// The next `lines` not yet attributed content lines of the enclosing
    /// text block form one link reference definition.
    RefDef {
        /// Number of lines of the definition.
        lines: u32,
    },
    /// The enclosing text block is a setext heading; this line is its
    /// underline.
    Setext {
        /// 1 (`=`) or 2 (`-`).
        level: u8,
    },
    /// The innermost enclosing list is loose.
    Loosen,
    /// The enclosing text block is a GFM table from here on: this line is
    /// its delimiter row, the content line before it its header row, and
    /// every later line a body row. Content lines before the header are a
    /// paragraph (after the link reference definitions).
    TableHeader,
}

const T_OPEN: u8 = 0;
const T_CLOSE: u8 = 1;
const T_TOK: u8 = 2;
const T_REFDEF: u8 = 3;
const T_SETEXT: u8 = 4;
const T_LOOSEN: u8 = 5;
const T_TABLE: u8 = 6;

const K_QUOTE: u8 = 0;
const K_BULLET: u8 = 1;
const K_ORDERED: u8 = 2;
const K_ITEM: u8 = 3;
const K_TEXT: u8 = 4;
const K_ATX: u8 = 5;
const K_BREAK: u8 = 6;
const K_FENCED: u8 = 7;
const K_INDENTED: u8 = 8;
const K_HTML: u8 = 9;
const K_FRONT: u8 = 10;
const K_MATH: u8 = 11;
const K_ALERT: u8 = 12;
const K_FOOTNOTE: u8 = 13;
const K_CALLOUT: u8 = 14;
const K_CALLOUT_TITLE: u8 = 15;
const K_COMMENT: u8 = 16;

/// A packed event: 8 bytes. The BlockId of an `Open` lives in a parallel
/// array.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Ev {
    tag: u8,
    a: u8,
    b: u16,
    n: u32,
}

impl Ev {
    pub(crate) const CLOSE: Ev = Ev {
        tag: T_CLOSE,
        a: 0,
        b: 0,
        n: 0,
    };

    /// A `Loosen` marker. `up` says which list it loosens: 0 when the list
    /// is the innermost open block, 1 when one of its items is, 2 when a
    /// text block in one of its items is. The public [`Event::Loosen`]
    /// leaves it out; range queries use it to find the list's depth
    /// without resolving the open blocks.
    #[inline]
    pub(crate) fn loosen(up: u8) -> Ev {
        Ev {
            tag: T_LOOSEN,
            a: up,
            b: 0,
            n: 0,
        }
    }

    #[inline]
    pub(crate) fn open(kind: BlockKind) -> Ev {
        let (a, b, n) = match kind {
            BlockKind::BlockQuote => (K_QUOTE, 0, 0),
            BlockKind::BulletList { marker } => (K_BULLET, u16::from(marker), 0),
            BlockKind::OrderedList { start, delimiter } => (K_ORDERED, u16::from(delimiter), start),
            BlockKind::Item => (K_ITEM, 0, 0),
            BlockKind::Text => (K_TEXT, 0, 0),
            BlockKind::AtxHeading { level } => (K_ATX, u16::from(level), 0),
            BlockKind::ThematicBreak => (K_BREAK, 0, 0),
            BlockKind::FencedCode => (K_FENCED, 0, 0),
            BlockKind::IndentedCode => (K_INDENTED, 0, 0),
            BlockKind::Html { kind } => (K_HTML, u16::from(kind), 0),
            BlockKind::FrontMatter { kind } => (K_FRONT, kind as u16, 0),
            BlockKind::MathBlock => (K_MATH, 0, 0),
            BlockKind::Alert { kind } => (K_ALERT, kind as u16, 0),
            BlockKind::FootnoteDefinition => (K_FOOTNOTE, 0, 0),
            BlockKind::Callout { fold } => (K_CALLOUT, fold as u16, 0),
            BlockKind::CalloutTitle => (K_CALLOUT_TITLE, 0, 0),
            BlockKind::CommentBlock => (K_COMMENT, 0, 0),
        };
        Ev {
            tag: T_OPEN,
            a,
            b,
            n,
        }
    }

    #[inline]
    pub(crate) fn tok(kind: TokenKind, virt: u8, len: u32) -> Ev {
        Ev {
            tag: T_TOK,
            a: kind as u8,
            b: u16::from(virt),
            n: len,
        }
    }

    #[inline]
    pub(crate) fn refdef(lines: u32) -> Ev {
        Ev {
            tag: T_REFDEF,
            a: 0,
            b: 0,
            n: lines,
        }
    }

    /// A `TableHeader` marker.
    #[inline]
    pub(crate) fn table_header() -> Ev {
        Ev {
            tag: T_TABLE,
            a: 0,
            b: 0,
            n: 0,
        }
    }

    #[inline]
    pub(crate) fn setext(level: u8) -> Ev {
        Ev {
            tag: T_SETEXT,
            a: level,
            b: 0,
            n: 0,
        }
    }

    #[inline]
    pub(crate) fn is_open(self) -> bool {
        self.tag == T_OPEN
    }

    #[inline]
    pub(crate) fn is_close(self) -> bool {
        self.tag == T_CLOSE
    }

    #[inline]
    pub(crate) fn is_refdef(self) -> bool {
        self.tag == T_REFDEF
    }

    /// Whether this opens a footnote definition (it defines a label, like
    /// a `RefDef` marker).
    #[inline]
    pub(crate) fn is_footnote_open(self) -> bool {
        self.tag == T_OPEN && self.a == K_FOOTNOTE
    }

    /// Whether this is a token (the most common event).
    #[inline]
    pub(crate) fn is_token(self) -> bool {
        self.tag == T_TOK
    }

    /// For a `Loosen` marker, how far below the innermost open block the
    /// loosened list is (see [`Ev::loosen`]).
    #[inline]
    pub(crate) fn loosen_up(self) -> Option<u32> {
        (self.tag == T_LOOSEN).then_some(u32::from(self.a))
    }

    /// Length in bytes if this is a token, else 0.
    #[inline]
    pub(crate) fn tok_len(self) -> usize {
        if self.tag == T_TOK {
            self.n as usize
        } else {
            0
        }
    }

    /// The token, if this is one.
    #[inline]
    pub(crate) fn token(self) -> Option<Token> {
        (self.tag == T_TOK).then(|| Token {
            kind: TokenKind::ALL[usize::from(self.a)],
            virtual_spaces: self.b as u8,
            len: self.n,
        })
    }

    /// The block kind, if this is an `Open`.
    #[inline]
    pub(crate) fn open_kind(self) -> Option<BlockKind> {
        if self.tag != T_OPEN {
            return None;
        }
        Some(match self.a {
            K_QUOTE => BlockKind::BlockQuote,
            K_BULLET => BlockKind::BulletList {
                marker: self.b as u8,
            },
            K_ORDERED => BlockKind::OrderedList {
                start: self.n,
                delimiter: self.b as u8,
            },
            K_ITEM => BlockKind::Item,
            K_TEXT => BlockKind::Text,
            K_ATX => BlockKind::AtxHeading {
                level: self.b as u8,
            },
            K_BREAK => BlockKind::ThematicBreak,
            K_FENCED => BlockKind::FencedCode,
            K_INDENTED => BlockKind::IndentedCode,
            K_MATH => BlockKind::MathBlock,
            K_ALERT => BlockKind::Alert {
                kind: AlertKind::ALL
                    .get(usize::from(self.b))
                    .copied()
                    .unwrap_or(AlertKind::Note),
            },
            K_FOOTNOTE => BlockKind::FootnoteDefinition,
            K_CALLOUT => BlockKind::Callout {
                fold: match self.b {
                    1 => CalloutFold::Open,
                    2 => CalloutFold::Closed,
                    _ => CalloutFold::None,
                },
            },
            K_CALLOUT_TITLE => BlockKind::CalloutTitle,
            K_COMMENT => BlockKind::CommentBlock,
            K_FRONT => BlockKind::FrontMatter {
                kind: if self.b == FrontMatterKind::Toml as u16 {
                    FrontMatterKind::Toml
                } else {
                    FrontMatterKind::Yaml
                },
            },
            _ => BlockKind::Html { kind: self.b as u8 },
        })
    }

    /// The public form; `id` is used for `Open` events.
    pub(crate) fn to_event(self, id: BlockId) -> Event {
        match self.tag {
            T_OPEN => Event::Open {
                id,
                kind: self.open_kind().unwrap_or(BlockKind::Text),
            },
            T_CLOSE => Event::Close,
            T_REFDEF => Event::RefDef { lines: self.n },
            T_SETEXT => Event::Setext { level: self.a },
            T_LOOSEN => Event::Loosen,
            T_TABLE => Event::TableHeader,
            _ => Event::Token(Token {
                kind: TokenKind::ALL[usize::from(self.a)],
                virtual_spaces: self.b as u8,
                len: self.n,
            }),
        }
    }
}

/// Where the block pass writes its output: events, the BlockIds of their
/// `Open`s, and the id counter.
#[derive(Debug, Default)]
pub(crate) struct Sink {
    pub events: Vec<Ev>,
    pub ids: Vec<BlockId>,
    pub next_id: u64,
}

impl Sink {
    #[inline]
    pub(crate) fn push(&mut self, ev: Ev) {
        self.events.push(ev);
    }

    #[inline]
    pub(crate) fn open(&mut self, kind: BlockKind) {
        self.events.push(Ev::open(kind));
        self.ids.push(BlockId(self.next_id));
        self.next_id += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packing_round_trips() {
        assert_eq!(std::mem::size_of::<Ev>(), 8);
        let kinds = [
            BlockKind::BlockQuote,
            BlockKind::BulletList { marker: b'*' },
            BlockKind::OrderedList {
                start: 123_456_789,
                delimiter: b')',
            },
            BlockKind::Item,
            BlockKind::Text,
            BlockKind::AtxHeading { level: 6 },
            BlockKind::ThematicBreak,
            BlockKind::FencedCode,
            BlockKind::IndentedCode,
            BlockKind::Html { kind: 7 },
            BlockKind::FrontMatter {
                kind: FrontMatterKind::Yaml,
            },
            BlockKind::FrontMatter {
                kind: FrontMatterKind::Toml,
            },
            BlockKind::MathBlock,
            BlockKind::Alert {
                kind: AlertKind::Caution,
            },
            BlockKind::Alert {
                kind: AlertKind::Note,
            },
            BlockKind::FootnoteDefinition,
            BlockKind::Callout {
                fold: CalloutFold::None,
            },
            BlockKind::Callout {
                fold: CalloutFold::Open,
            },
            BlockKind::Callout {
                fold: CalloutFold::Closed,
            },
            BlockKind::CalloutTitle,
            BlockKind::CommentBlock,
        ];
        for k in kinds {
            assert_eq!(Ev::open(k).open_kind(), Some(k));
        }
        for k in TokenKind::ALL {
            let t = Ev::tok(k, 3, 77).token().expect("token");
            assert_eq!((t.kind, t.virtual_spaces, t.len), (k, 3, 77));
        }
        assert_eq!(Ev::table_header().to_event(BlockId(0)), Event::TableHeader);
        for up in 0..3 {
            assert_eq!(Ev::loosen(up).loosen_up(), Some(u32::from(up)));
            assert_eq!(Ev::loosen(up).to_event(BlockId(0)), Event::Loosen);
        }
    }
}
