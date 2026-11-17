// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The items a trigger lists, ranked against its query, and the edit an
//! accepted item makes.
//!
//! Files, aliases, headings and blocks of the vault are the quick
//! switcher's entries ranked with its matcher ([`model::rank`]); everything
//! else (tags, emoji, callout types, languages, footnotes, properties) is
//! ranked with the same matcher by label ([`model::rank_labels`]).

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use gv_syntax::{Leaf, LeafKind, SyntaxTree, Visitor};
use gv_text::TextSnapshot;
use gv_vault::content::{ContentSnapshot, NoteData, PropertyValue};
use gv_vault::{LinkKind, LinkRef, PathIndex, ProfileId, ProfileParams, VaultPath, resolve};

use super::context::{Kind, Scope, Trigger};
use super::link::{Linker, heading_fragment, wiki_heading};
use crate::switcher::model::{self, Entry, EntryKind};

/// The most items listed.
pub(crate) const MAX_ITEMS: usize = 50;

/// The icon at the start of a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ItemIcon {
    Note,
    Alias,
    Image,
    Canvas,
    Base,
    OtherFile,
    Heading,
    Block,
    Tag,
    /// The emoji itself.
    Emoji(&'static str),
    Callout,
    Language,
    Footnote,
    Property,
    /// "Create …" rows.
    Create,
}

/// What accepting an item writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pick {
    /// This text, from the query's start to the trigger's end.
    Text(String),
    /// A link to a file of the vault (`alias` when an alias row was picked).
    File {
        path: VaultPath,
        alias: Option<String>,
        title: String,
    },
    /// A heading of `file` (`None`: the document being edited).
    Heading {
        file: Option<VaultPath>,
        text: String,
    },
    /// A block of `file`, with its id; without one, a new id is added at
    /// `create_at` (the current document only).
    Block {
        file: Option<VaultPath>,
        id: Option<String>,
        create_at: Option<usize>,
    },
    /// A new footnote with this label and its definition.
    NewFootnote(String),
}

/// One row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Item {
    pub icon: ItemIcon,
    pub label: String,
    /// After the label in `text.secondary` (" → Target" on alias rows).
    pub suffix: String,
    /// At the right in 11 px `text.secondary`.
    pub detail: String,
    /// The label's bytes the query matched, drawn in weight 700.
    pub matched: Vec<Range<usize>>,
    pub pick: Pick,
}

/// A paragraph of the document being edited, for block links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Paragraph {
    /// Its first line, without a block id.
    pub text: String,
    /// The start of its first line.
    pub start: usize,
    /// The end of its last line's content, where a new id goes.
    pub end: usize,
    /// Its block id.
    pub id: Option<String>,
}

/// A [`Visitor`] that calls a closure on every leaf.
pub(crate) struct Leaves<F>(pub F);

impl<F: FnMut(&Leaf<'_>)> Visitor for Leaves<F> {
    fn leaf(&mut self, leaf: &Leaf<'_>) {
        (self.0)(leaf);
    }
}

/// The paragraphs of `text` (list items and quotes included).
pub(crate) fn paragraphs(tree: &SyntaxTree, text: &TextSnapshot) -> Vec<Paragraph> {
    let mut out = Vec::new();
    tree.walk(&mut Leaves(|leaf: &Leaf<'_>| {
        if leaf.kind != LeafKind::Paragraph {
            return;
        }
        let lines: Vec<_> = leaf.lines.iter().filter(|l| l.has_content).collect();
        let (Some(first), Some(last)) = (lines.first(), lines.last()) else {
            return;
        };
        let first_text = text
            .text_for_range(first.content.clone())
            .unwrap_or_default();
        let last_text = text
            .text_for_range(last.content.clone())
            .unwrap_or_default();
        let id = block_id_of(&last_text);
        let mut label = first_text.trim().to_owned();
        if lines.len() == 1
            && let Some(id) = &id
        {
            label = label
                .trim_end()
                .strip_suffix(&format!("^{id}"))
                .unwrap_or(&label)
                .trim_end()
                .to_owned();
        }
        out.push(Paragraph {
            text: truncate(&label, 80),
            start: first.content.start,
            end: last.content.end,
            id,
        });
    }));
    out
}

/// The `^id` at the end of a paragraph's last line.
fn block_id_of(line: &str) -> Option<String> {
    let line = line.trim_end();
    let caret = line.rfind('^')?;
    let id = &line[caret + 1..];
    let before = line[..caret].chars().next_back();
    (!id.is_empty()
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && before.is_none_or(char::is_whitespace))
    .then(|| id.to_owned())
}

fn truncate(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

/// What the items are gathered from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sources<'a> {
    /// The vault's content index.
    pub content: Option<&'a ContentSnapshot>,
    pub root: Option<&'a Path>,
    pub index: Option<&'a PathIndex>,
    /// The vault's files and aliases, as the quick switcher lists them.
    pub files: &'a [Entry],
    /// The document, vault-relative.
    pub source: Option<&'a VaultPath>,
    pub params: &'a ProfileParams,
    pub profile: ProfileId,
    /// The document's headings, block ids, tags, footnotes, properties.
    pub current: &'a NoteData,
    /// The document's paragraphs (for `[[^` only).
    pub paragraphs: &'a [Paragraph],
    /// The callout or alert types of the profile.
    pub callouts: &'static [&'static str],
}

impl Sources<'_> {
    /// `file` vault-relative.
    fn vault_path(&self, file: &Path) -> Option<VaultPath> {
        let root = self.root?;
        VaultPath::from_relative_fs_path(file.strip_prefix(root).ok()?).ok()
    }

    /// The note a link of `kind` written `text` points to.
    fn resolve(&self, kind: LinkKind, text: &str) -> Option<VaultPath> {
        let index = self.index?;
        let source = self.source?;
        let link = LinkRef::new(kind, text, source.clone(), self.profile);
        resolve(index, &link, self.params).target_path
    }

    /// Whether `path` is the document being edited.
    fn is_current(&self, path: &VaultPath) -> bool {
        self.source == Some(path)
    }
}

/// The items of `trigger` for `query`, best first.
pub(crate) fn items(trigger: &Trigger, query: &str, s: &Sources<'_>) -> Vec<Item> {
    let mut items = match &trigger.kind {
        Kind::WikiFile | Kind::LinkPath => files(query, s),
        Kind::WikiHeading(scope) => match scope {
            Scope::Current => current_headings(query, s),
            Scope::Vault => vault_sections(query, s, false),
            Scope::Note(name) => match s.resolve(LinkKind::Wikilink, name) {
                Some(path) => note_headings(&path, query, s),
                None => Vec::new(),
            },
        },
        Kind::WikiBlock(scope) => match scope {
            Scope::Current => current_blocks(query, s),
            Scope::Vault => vault_sections(query, s, true),
            Scope::Note(name) => match s.resolve(LinkKind::Wikilink, name) {
                Some(path) if s.is_current(&path) => current_blocks(query, s),
                Some(path) => note_blocks(&path, query, s),
                None => Vec::new(),
            },
        },
        Kind::LinkAnchor { path } if path.is_empty() => current_headings(query, s),
        Kind::LinkAnchor { path } => match s.resolve(LinkKind::Inline, path) {
            Some(target) => note_headings(&target, query, s),
            None => Vec::new(),
        },
        Kind::Tag => tags(query, s),
        Kind::Emoji => emoji(query),
        Kind::Callout => texts(
            query,
            s.callouts
                .iter()
                .map(|t| ((*t).to_owned(), format!("{t}] "))),
            ItemIcon::Callout,
        ),
        Kind::Fence => languages(query),
        Kind::Footnote => footnotes(query, s),
        Kind::PropertyName => property_names(query, s),
        Kind::PropertyValue { key } => property_values(key, query, s),
    };
    items.truncate(MAX_ITEMS);
    items
}

/// The icon of a file entry.
fn file_icon(entry: &Entry) -> ItemIcon {
    match entry.kind {
        EntryKind::Alias => ItemIcon::Alias,
        EntryKind::Note => ItemIcon::Note,
        _ => {
            let ext = entry
                .file
                .as_deref()
                .and_then(Path::extension)
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            match ext.as_str() {
                "canvas" => ItemIcon::Canvas,
                "base" => ItemIcon::Base,
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "avif" | "ico" => {
                    ItemIcon::Image
                }
                _ => ItemIcon::OtherFile,
            }
        }
    }
}

fn files(query: &str, s: &Sources<'_>) -> Vec<Item> {
    model::rank(s.files, query)
        .into_iter()
        .filter_map(|i| {
            let entry = &s.files[i];
            let path = s.vault_path(entry.file.as_deref()?)?;
            let (suffix, detail, alias, title) = match (&entry.kind, &entry.target) {
                (EntryKind::Alias, Some(target)) => (
                    format!(" → {target}"),
                    "alias".to_owned(),
                    Some(entry.label.to_string()),
                    target.to_string(),
                ),
                _ => (
                    String::new(),
                    entry.folder.to_string(),
                    None,
                    entry.label.to_string(),
                ),
            };
            Some(Item {
                icon: file_icon(entry),
                label: entry.label.to_string(),
                suffix,
                detail,
                matched: model::highlight(&entry.label, query),
                pick: Pick::File { path, alias, title },
            })
        })
        .take(MAX_ITEMS)
        .collect()
}

/// Items for `labels` ranked by `query`; `make` builds each one.
fn ranked<T>(
    labels: &[String],
    query: &str,
    data: &[T],
    make: impl Fn(&String, &T) -> (ItemIcon, String, Pick),
) -> Vec<Item> {
    model::rank_labels(labels, query)
        .into_iter()
        .take(MAX_ITEMS)
        .map(|i| {
            let (icon, detail, pick) = make(&labels[i], &data[i]);
            Item {
                icon,
                label: labels[i].clone(),
                suffix: String::new(),
                detail,
                matched: model::highlight(&labels[i], query),
                pick,
            }
        })
        .collect()
}

/// `(label, inserted text)` pairs as plain items.
fn texts(query: &str, pairs: impl Iterator<Item = (String, String)>, icon: ItemIcon) -> Vec<Item> {
    let (labels, inserts): (Vec<String>, Vec<String>) = pairs.unzip();
    ranked(&labels, query, &inserts, |_, insert| {
        (icon, String::new(), Pick::Text(insert.clone()))
    })
}

fn headings_of(data: &NoteData, file: Option<&VaultPath>, query: &str) -> Vec<Item> {
    let labels: Vec<String> = data.headings.iter().map(|h| h.text.to_string()).collect();
    let levels: Vec<u8> = data.headings.iter().map(|h| h.level).collect();
    ranked(&labels, query, &levels, |label, level| {
        (
            ItemIcon::Heading,
            format!("H{level}"),
            Pick::Heading {
                file: file.cloned(),
                text: label.clone(),
            },
        )
    })
}

fn current_headings(query: &str, s: &Sources<'_>) -> Vec<Item> {
    headings_of(s.current, None, query)
}

fn note_headings(path: &VaultPath, query: &str, s: &Sources<'_>) -> Vec<Item> {
    if s.is_current(path) {
        return headings_of(s.current, Some(path), query);
    }
    match s.content.and_then(|c| c.note(path)) {
        Some(record) => headings_of(&record.data, Some(path), query),
        None => Vec::new(),
    }
}

fn current_blocks(query: &str, s: &Sources<'_>) -> Vec<Item> {
    let labels: Vec<String> = s.paragraphs.iter().map(|p| p.text.clone()).collect();
    ranked(&labels, query, s.paragraphs, |_, p| {
        (
            ItemIcon::Block,
            p.id.as_ref().map(|id| format!("^{id}")).unwrap_or_default(),
            Pick::Block {
                file: None,
                id: p.id.clone(),
                create_at: p.id.is_none().then_some(p.end),
            },
        )
    })
}

fn note_blocks(path: &VaultPath, query: &str, s: &Sources<'_>) -> Vec<Item> {
    let Some(record) = s.content.and_then(|c| c.note(path)) else {
        return Vec::new();
    };
    let labels: Vec<String> = record
        .data
        .block_ids
        .iter()
        .map(|b| format!("^{}", b.id))
        .collect();
    let ids: Vec<String> = record
        .data
        .block_ids
        .iter()
        .map(|b| b.id.to_string())
        .collect();
    ranked(&labels, query, &ids, |_, id| {
        (
            ItemIcon::Block,
            record.title().to_owned(),
            Pick::Block {
                file: Some(path.clone()),
                id: Some(id.clone()),
                create_at: None,
            },
        )
    })
}

/// Headings or block ids across the vault.
fn vault_sections(query: &str, s: &Sources<'_>, blocks: bool) -> Vec<Item> {
    let (Some(content), Some(root)) = (s.content, s.root) else {
        return Vec::new();
    };
    let entries = model::sections(content, root, blocks);
    model::rank(&entries, query)
        .into_iter()
        .filter_map(|i| {
            let entry = &entries[i];
            let path = s.vault_path(entry.file.as_deref()?)?;
            let pick = if blocks {
                Pick::Block {
                    file: Some(path),
                    id: Some(entry.label.trim_start_matches('^').to_owned()),
                    create_at: None,
                }
            } else {
                Pick::Heading {
                    file: Some(path),
                    text: entry.label.to_string(),
                }
            };
            Some(Item {
                icon: if blocks {
                    ItemIcon::Block
                } else {
                    ItemIcon::Heading
                },
                label: entry.label.to_string(),
                suffix: String::new(),
                detail: entry.target.as_deref().unwrap_or_default().to_owned(),
                matched: model::highlight(&entry.label, query),
                pick,
            })
        })
        .take(MAX_ITEMS)
        .collect()
}

fn tags(query: &str, s: &Sources<'_>) -> Vec<Item> {
    let mut counts: Vec<(String, usize)> = s
        .content
        .map(|c| {
            c.tags()
                .into_iter()
                .map(|t| (t.name.to_string(), t.count))
                .collect()
        })
        .unwrap_or_default();
    let typed = gv_vault::keys::fold_key(query);
    for tag in &s.current.tags {
        let key = gv_vault::keys::fold_key(&tag.name);
        // The tag being typed.
        if key == typed {
            continue;
        }
        if !counts
            .iter()
            .any(|(n, _)| gv_vault::keys::fold_key(n) == key)
        {
            counts.push((tag.name.to_string(), 1));
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let (labels, numbers): (Vec<String>, Vec<usize>) = counts.into_iter().unzip();
    ranked(&labels, query, &numbers, |label, count| {
        (ItemIcon::Tag, count.to_string(), Pick::Text(label.clone()))
    })
}

/// Every gemoji shortcode with its emoji.
fn shortcodes() -> &'static [(String, &'static str)] {
    static CODES: OnceLock<Vec<(String, &'static str)>> = OnceLock::new();
    CODES.get_or_init(|| {
        emojis::iter()
            .flat_map(|e| e.shortcodes().map(move |c| (c.to_owned(), e.as_str())))
            .collect()
    })
}

fn emoji(query: &str) -> Vec<Item> {
    let codes = shortcodes();
    let labels: Vec<String> = codes.iter().map(|(c, _)| c.clone()).collect();
    let glyphs: Vec<&'static str> = codes.iter().map(|(_, e)| *e).collect();
    ranked(&labels, query, &glyphs, |label, glyph| {
        (
            ItemIcon::Emoji(glyph),
            String::new(),
            Pick::Text(format!("{label}:")),
        )
    })
}

/// The fence languages: grammar names a fence resolves back to, with
/// their display names.
fn language_names() -> &'static [(String, String)] {
    static NAMES: OnceLock<Vec<(String, String)>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names: Vec<(String, String)> = gv_highlight::Language::all()
            .filter(|l| gv_highlight::Language::from_info(l.name()) == Some(*l))
            .map(|l| (l.name().to_owned(), l.display_name().to_owned()))
            .collect();
        names.sort();
        names.dedup_by(|a, b| a.0 == b.0);
        names
    })
}

fn languages(query: &str) -> Vec<Item> {
    let names = language_names();
    let labels: Vec<String> = names.iter().map(|(n, _)| n.clone()).collect();
    let displays: Vec<String> = names.iter().map(|(_, d)| d.clone()).collect();
    ranked(&labels, query, &displays, |label, display| {
        (
            ItemIcon::Language,
            display.clone(),
            Pick::Text(label.clone()),
        )
    })
}

fn footnotes(query: &str, s: &Sources<'_>) -> Vec<Item> {
    let mut labels: Vec<String> = Vec::new();
    for label in &s.current.footnotes {
        if !labels.iter().any(|l| l == &**label) {
            labels.push(label.to_string());
        }
    }
    let inserts: Vec<String> = labels.iter().map(|l| format!("{l}]")).collect();
    let mut items = ranked(&labels, query, &inserts, |_, insert| {
        (
            ItemIcon::Footnote,
            String::new(),
            Pick::Text(insert.clone()),
        )
    });
    let typed = query.trim();
    let new = if !typed.is_empty() && !labels.iter().any(|l| l == typed) {
        typed.to_owned()
    } else {
        (1..)
            .map(|n: u32| n.to_string())
            .find(|n| !labels.contains(n))
            .unwrap_or_default()
    };
    items.push(Item {
        icon: ItemIcon::Create,
        label: format!("New footnote [^{new}]"),
        suffix: String::new(),
        detail: String::new(),
        matched: Vec::new(),
        pick: Pick::NewFootnote(new),
    });
    items
}

/// Every property of the vault's notes and the document, with how many
/// notes have it, most used first.
fn property_counts(s: &Sources<'_>) -> Vec<(String, usize, Vec<String>)> {
    let mut by_key: HashMap<String, (usize, Vec<String>)> = HashMap::new();
    let mut add = |data: &NoteData| {
        for p in &data.properties {
            let entry = by_key.entry(p.key.to_string()).or_default();
            entry.0 += 1;
            match &p.value {
                PropertyValue::Text(t) | PropertyValue::Number(t) => entry.1.push(t.to_string()),
                PropertyValue::Bool(b) => entry.1.push(b.to_string()),
                PropertyValue::List(list) => entry.1.extend(list.iter().map(|v| v.to_string())),
                PropertyValue::Null | PropertyValue::Other(_) => {}
            }
        }
    };
    if let Some(content) = s.content {
        for record in content.notes() {
            if s.source != Some(&record.path) {
                add(&record.data);
            }
        }
    }
    add(s.current);
    let mut out: Vec<(String, usize, Vec<String>)> =
        by_key.into_iter().map(|(k, (n, v))| (k, n, v)).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

fn property_names(query: &str, s: &Sources<'_>) -> Vec<Item> {
    let counts = property_counts(s);
    let labels: Vec<String> = counts.iter().map(|(k, _, _)| k.clone()).collect();
    let numbers: Vec<usize> = counts.iter().map(|(_, n, _)| *n).collect();
    ranked(&labels, query, &numbers, |label, n| {
        (
            ItemIcon::Property,
            if *n == 1 {
                "1 note".to_owned()
            } else {
                format!("{n} notes")
            },
            Pick::Text(format!("{label}: ")),
        )
    })
}

fn property_values(key: &str, query: &str, s: &Sources<'_>) -> Vec<Item> {
    if key.eq_ignore_ascii_case("tags") || key.eq_ignore_ascii_case("tag") {
        return tags(query, s);
    }
    let counts = property_counts(s);
    let Some((_, _, values)) = counts.into_iter().find(|(k, _, _)| k == key) else {
        return Vec::new();
    };
    let mut tally: Vec<(String, usize)> = Vec::new();
    for v in values {
        match tally.iter_mut().find(|(t, _)| *t == v) {
            Some(entry) => entry.1 += 1,
            None => tally.push((v, 1)),
        }
    }
    // The value being typed is no completion of itself.
    let typed = gv_vault::keys::fold_key(query.trim());
    tally.retain(|(v, _)| gv_vault::keys::fold_key(v) != typed);
    tally.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let (labels, numbers): (Vec<String>, Vec<usize>) = tally.into_iter().unzip();
    ranked(&labels, query, &numbers, |label, _| {
        (ItemIcon::Property, String::new(), Pick::Text(label.clone()))
    })
}

// ----- accepting ------------------------------------------------------------------

/// The edit an accepted item makes: replacements in the text before it
/// (sorted, not overlapping) and where the caret goes after them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Completion {
    pub edits: Vec<(Range<usize>, String)>,
    pub caret: usize,
}

/// A completion being put together: its edits, and the edit the caret
/// ends at the end of.
#[derive(Debug)]
struct Draft {
    edits: Vec<(Range<usize>, String)>,
    caret_edit: usize,
}

impl Draft {
    /// One replacement, with the caret after it.
    fn one(range: Range<usize>, text: String) -> Self {
        Self {
            edits: vec![(range, text)],
            caret_edit: 0,
        }
    }

    /// Adds another replacement (not overlapping the others); with
    /// `caret_there` the caret goes to its end.
    fn with(mut self, extra: (Range<usize>, String), caret_there: bool) -> Self {
        self.edits.push(extra);
        if caret_there {
            self.caret_edit = self.edits.len() - 1;
        }
        self
    }

    fn done(mut self) -> Completion {
        let (range, text) = &self.edits[self.caret_edit];
        let shift: isize = self
            .edits
            .iter()
            .enumerate()
            .filter(|(i, (r, _))| *i != self.caret_edit && r.end <= range.start)
            .map(|(_, (r, t))| t.len() as isize - r.len() as isize)
            .sum();
        let caret = (range.start as isize + shift) as usize + text.len();
        self.edits.sort_by_key(|(r, _)| (r.start, r.end));
        Completion {
            edits: self.edits,
            caret,
        }
    }
}

/// What accepting needs beyond the trigger and the item.
#[derive(Debug, Clone)]
pub(crate) struct AcceptEnv<'a> {
    /// Writes links (none without a vault: then only the document's own
    /// headings and blocks are listed).
    pub linker: Option<Linker<'a>>,
    pub profile: ProfileId,
    /// The text's length, and whether it ends with a line break.
    pub len: usize,
    pub ends_with_newline: bool,
    /// The id a new block id gets.
    pub new_block_id: String,
}

impl AcceptEnv<'_> {
    fn markdown(&self) -> bool {
        self.linker.as_ref().is_some_and(|l| l.style.markdown)
    }

    fn wiki_path(&self, path: &VaultPath) -> String {
        match &self.linker {
            Some(l) => l.wiki_path(path),
            None => path.as_str().to_owned(),
        }
    }

    fn markdown_dest(&self, path: &VaultPath) -> String {
        match &self.linker {
            Some(l) => l.markdown_dest(path),
            None => super::link::encode(path.as_str()),
        }
    }

    /// The destination of a Markdown link to `file` (`None`: this
    /// document) with `fragment`.
    fn dest_with(&self, file: Option<&VaultPath>, fragment: &str) -> String {
        let own = file.is_none_or(|f| self.linker.as_ref().is_some_and(|l| l.source == f));
        if own {
            format!("#{fragment}")
        } else {
            let dest = self.markdown_dest(file.expect("checked"));
            match dest.strip_suffix('>') {
                // `<a b.md>` takes the fragment inside.
                Some(open) => format!("{open}#{fragment}>"),
                None => format!("{dest}#{fragment}"),
            }
        }
    }
}

/// `![` for embeds, `[` otherwise.
fn open_bracket(t: &Trigger) -> &'static str {
    if t.embed { "![" } else { "[" }
}

/// The edit accepting `item` for `trigger` makes.
pub(crate) fn complete(t: &Trigger, item: &Item, env: &AcceptEnv<'_>) -> Completion {
    draft(t, item, env).done()
}

fn draft(t: &Trigger, item: &Item, env: &AcceptEnv<'_>) -> Draft {
    match &item.pick {
        Pick::Text(text) => Draft::one(t.query.start..t.end, text.clone()),
        Pick::File { path, alias, title } => match t.kind {
            Kind::LinkPath => {
                let done = Draft::one(t.query.start..t.end, env.markdown_dest(path));
                match &t.text {
                    Some(r) if r.is_empty() => {
                        let text = alias.clone().unwrap_or_else(|| title.clone());
                        done.with((r.clone(), escape_text(&text)), false)
                    }
                    _ => done,
                }
            }
            _ if env.markdown() => {
                let text = alias.clone().unwrap_or_else(|| title.clone());
                let link = format!(
                    "{}{}]({})",
                    open_bracket(t),
                    escape_text(&text),
                    env.markdown_dest(path)
                );
                Draft::one(t.start..t.end, link)
            }
            _ => {
                let mut link = env.wiki_path(path);
                if let Some(alias) = alias {
                    link.push('|');
                    link.push_str(alias);
                }
                link.push_str("]]");
                Draft::one(t.inner..t.end, link)
            }
        },
        Pick::Heading { file, text } => {
            if let Kind::LinkAnchor { .. } = t.kind {
                return Draft::one(t.query.clone(), heading_fragment(env.profile, text));
            }
            if env.markdown() {
                let dest = env.dest_with(file.as_ref(), &heading_fragment(env.profile, text));
                let link = format!("{}{}]({dest})", open_bracket(t), escape_text(text));
                return Draft::one(t.start..t.end, link);
            }
            let heading = wiki_heading(text);
            match (&t.kind, file) {
                (Kind::WikiHeading(Scope::Vault), Some(file)) => Draft::one(
                    t.inner..t.end,
                    format!("{}#{heading}]]", env.wiki_path(file)),
                ),
                (Kind::WikiHeading(Scope::Vault), None) => {
                    Draft::one(t.inner..t.end, format!("#{heading}]]"))
                }
                _ => Draft::one(t.query.start..t.end, format!("{heading}]]")),
            }
        }
        Pick::Block {
            file,
            id,
            create_at,
        } => {
            let id = id.clone().unwrap_or_else(|| env.new_block_id.clone());
            let done = if env.markdown() {
                let dest = env.dest_with(file.as_ref(), &format!("^{id}"));
                let text = truncate(item.label.trim_start_matches('^'), 40);
                let link = format!("{}{}]({dest})", open_bracket(t), escape_text(&text));
                Draft::one(t.start..t.end, link)
            } else {
                match (&t.kind, file) {
                    (Kind::WikiBlock(Scope::Vault), Some(file)) => {
                        Draft::one(t.inner..t.end, format!("{}#^{id}]]", env.wiki_path(file)))
                    }
                    (Kind::WikiBlock(Scope::Note(_)), _) => {
                        Draft::one(t.query.start..t.end, format!("{id}]]"))
                    }
                    _ => Draft::one(t.inner..t.end, format!("#^{id}]]")),
                }
            };
            match create_at {
                Some(at) => done.with((*at..*at, format!(" ^{id}")), false),
                None => done,
            }
        }
        Pick::NewFootnote(label) => {
            let done = Draft::one(t.query.start..t.end, format!("{label}]"));
            let lead = if env.ends_with_newline || env.len == 0 {
                "\n"
            } else {
                "\n\n"
            };
            done.with((env.len..env.len, format!("{lead}[^{label}]: ")), true)
        }
    }
}

/// Link text with `[` and `]` escaped.
fn escape_text(text: &str) -> String {
    text.replace('[', "\\[").replace(']', "\\]")
}

/// A new 6-hex-digit block id that `taken` does not have.
pub(crate) fn new_block_id(seed: u64, taken: &[Arc<str>]) -> String {
    let mut n = seed;
    loop {
        // splitmix64
        n = n.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = n;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let id = format!("{:06x}", z & 0xFF_FFFF);
        if !taken.iter().any(|t| **t == *id) {
            return id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(kind: Kind, start: usize, inner: usize, query: Range<usize>, end: usize) -> Trigger {
        Trigger {
            kind,
            start,
            inner,
            query,
            end,
            embed: false,
            text: None,
        }
    }

    fn env() -> AcceptEnv<'static> {
        AcceptEnv {
            linker: None,
            profile: ProfileId::Obsidian,
            len: 20,
            ends_with_newline: true,
            new_block_id: "abc123".into(),
        }
    }

    fn item(pick: Pick) -> Item {
        Item {
            icon: ItemIcon::Note,
            label: "Some text".into(),
            suffix: String::new(),
            detail: String::new(),
            matched: Vec::new(),
            pick,
        }
    }

    #[test]
    fn a_new_block_id_goes_with_the_link_in_one_edit() {
        // `[[^so]]` at 0..7 and a paragraph ending at 15.
        let t = trigger(Kind::WikiBlock(Scope::Current), 0, 2, 3..5, 7);
        let done = complete(
            &t,
            &item(Pick::Block {
                file: None,
                id: None,
                create_at: Some(15),
            }),
            &env(),
        );
        assert_eq!(
            done.edits,
            vec![
                (2..7, "#^abc123]]".to_owned()),
                (15..15, " ^abc123".to_owned())
            ]
        );
        assert_eq!(done.caret, 12);
    }

    #[test]
    fn a_new_footnote_gets_its_definition() {
        let t = trigger(Kind::Footnote, 4, 6, 6..6, 7);
        let done = complete(&t, &item(Pick::NewFootnote("1".into())), &env());
        assert_eq!(
            done.edits,
            vec![(6..7, "1]".to_owned()), (20..20, "\n[^1]: ".to_owned())]
        );
        // After the definition: 20 + 1 (the label's net growth) + 7.
        assert_eq!(done.caret, 28);
    }

    #[test]
    fn text_before_the_caret_moves_it() {
        // `[](no` with the link text at 1..1 and the query at 3..5.
        let mut t = trigger(Kind::LinkPath, 0, 3, 3..5, 5);
        t.text = Some(1..1);
        let done = complete(
            &t,
            &item(Pick::File {
                path: VaultPath::new("Note.md").expect("path"),
                alias: None,
                title: "Note".into(),
            }),
            &env(),
        );
        assert_eq!(
            done.edits,
            vec![(1..1, "Note".to_owned()), (3..5, "Note.md".to_owned())]
        );
        assert_eq!(done.caret, 14);
    }

    #[test]
    fn block_ids_are_six_hex_digits_and_new() {
        let id = new_block_id(1, &[]);
        assert_eq!(id.len(), 6);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        let taken: Vec<Arc<str>> = vec![Arc::from(id.as_str())];
        assert_ne!(new_block_id(1, &taken), id);
        assert_eq!(block_id_of("text ^ab-1"), Some("ab-1".into()));
        assert_eq!(block_id_of("x^ab"), None);
    }
}
