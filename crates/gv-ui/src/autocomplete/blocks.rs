// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The blocks a block link can name (`[[^`, `[[Note#^`, `[[^^`), where a
//! new block id goes, and adding one to another note.
//!
//! # Blocks and new ids
//!
//! [`blocks`] lists a note's blocks as Obsidian does, each with its id when
//! it has one:
//!
//! | Block | A new id goes |
//! |---|---|
//! | Paragraph, list item (its paragraphs), footnote | at the end of its last line: ` ^id` |
//! | Table, code block, math block, block quote, callout | on its own line after it, after a blank line |
//! | Heading | nowhere: it is linked by its text (`[[Note#Heading]]`) |
//!
//! An id on its own line names the block before it, as in the content
//! index ([`gv_vault::content::extract`]). Tables, code and quotes inside
//! list items or quotes are not listed (an id after them would end the
//! list); the paragraphs inside list items are. Ids are six lowercase
//! letters and digits ([`super::new_block_id`]), new in the note.
//!
//! # Other notes
//!
//! The blocks of another note are read off the UI thread ([`Job`]): from
//! its unsaved buffer when it is open with changes, else from the file.
//! They are kept per note while its index record (or its buffer's
//! revision) stays the same; `[[^^` reads every note once and then only
//! the notes that changed.
//!
//! Accepting a block of another note that has no id writes the link and
//! asks the window ([`BlockIdRequest`]) to add the id to that note:
//!
//! - open in a tab of the window: its buffer gets the id as one undo
//!   step, saved when it had no unsaved changes before;
//! - open with unsaved changes in another window: refused;
//! - else the file, through the refactor journal and its atomic save
//!   ([`gv_vault::refactor::apply`]), refused when the file changed after
//!   it was read.
//!
//! The block is found again in the text being edited (by its place among
//! the note's blocks and the hash of its text); when it is no longer
//! there, or the id got taken meanwhile, nothing is added. A refusal
//! takes the link back out of the document (when the text is still as
//! the completion left it) and says why in its infobar. Undoing the link
//! leaves the id in the other note, as in Obsidian.

use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gv_syntax::{Container, Leaf, LeafKind, ParseOptions, SyntaxTree, Visitor};
use gv_text::{Revision, TextSource};
use gv_vault::VaultPath;
use gv_vault::content::{ContentSnapshot, FileStamp, RecordKind};
use gv_vault::profile::ProfileId;

/// The largest note whose blocks are read (the embeds' limit).
const MAX_NOTE_BYTES: u64 = crate::reader::embed_texts::MAX_NOTE_BYTES;

/// What a block is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockKind {
    /// A paragraph, also in a list item or a footnote.
    Paragraph,
    /// The `n`th heading of the note (counting every heading).
    Heading(usize),
    Table,
    Code,
    Math,
    /// A block quote, callout or alert.
    Quote,
}

/// Where a new id goes in a block, and how it is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NewId {
    /// The byte the id is inserted at: the end of the block's last line.
    pub at: usize,
    /// On a line of its own after a blank line, else after a space.
    pub own_line: bool,
    /// The note's lines end with CR LF.
    pub crlf: bool,
    /// A line with text follows the block: a blank line goes after the
    /// id too.
    pub gap_after: bool,
}

impl NewId {
    /// The text inserted for `id`.
    pub fn text(&self, id: &str) -> String {
        if !self.own_line {
            return format!(" ^{id}");
        }
        let nl = if self.crlf { "\r\n" } else { "\n" };
        let after = if self.gap_after { nl } else { "" };
        format!("{nl}{nl}^{id}{after}")
    }
}

/// One block of a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NoteBlock {
    /// Its first line (without a block id), for the list.
    pub text: String,
    pub kind: BlockKind,
    /// The start of its first line.
    pub start: usize,
    /// The end of its last line's content.
    pub end: usize,
    /// Its block id.
    pub id: Option<String>,
    /// Where a new id goes (`None`: it has one, or it is a heading).
    pub new_id: Option<NewId>,
    /// The hash of its text (`start..end`), to find it again.
    pub hash: u64,
}

/// The blocks of `text` parsed as `tree`, in document order.
pub(crate) fn blocks<S: TextSource + ?Sized>(tree: &SyntaxTree, text: &S) -> Vec<NoteBlock> {
    let mut scan = Scan {
        text,
        out: Vec::new(),
        depth: 0,
        quote: None,
        headings: 0,
    };
    tree.walk(&mut scan);
    let mut out = scan.out;
    for block in &mut out {
        block.hash = hash_of(text, block.start..block.end);
        if block.id.is_some() {
            block.new_id = None;
        }
    }
    out
}

/// A top-level block quote being read.
struct Quote {
    start: usize,
    label: Option<String>,
    /// The id at the end of its last paragraph.
    id: Option<String>,
}

struct Scan<'a, S: ?Sized> {
    text: &'a S,
    out: Vec<NoteBlock>,
    /// Containers open.
    depth: usize,
    quote: Option<Quote>,
    headings: usize,
}

fn copy<S: TextSource + ?Sized>(text: &S, range: std::ops::Range<usize>) -> String {
    let mut bytes = Vec::new();
    text.copy_range(range, &mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

fn hash_of<S: TextSource + ?Sized>(text: &S, range: std::ops::Range<usize>) -> u64 {
    let mut bytes = Vec::new();
    text.copy_range(range, &mut bytes);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

impl<S: TextSource + ?Sized> Scan<'_, S> {
    /// The end of `range` without the line breaks and blanks after its
    /// last text.
    fn trimmed_end(&self, range: std::ops::Range<usize>) -> usize {
        let body = copy(self.text, range.clone());
        range.start + body.trim_end().len()
    }

    /// Where an id on its own line goes after a block ending at `end`.
    fn own_line(&self, end: usize) -> NewId {
        let mut scratch = Vec::new();
        let rest = self.text.line(end, &mut scratch).to_vec();
        let crlf = rest.ends_with(b"\r\n");
        let next = end + rest.len();
        let gap_after = next < self.text.len() && {
            let mut scratch = Vec::new();
            let line = self.text.line(next, &mut scratch);
            line.iter().any(|b| !b.is_ascii_whitespace())
        };
        NewId {
            at: end,
            own_line: true,
            crlf,
            gap_after,
        }
    }

    /// The first line of `leaf` with text, trimmed.
    fn first_line(&self, leaf: &Leaf<'_>) -> Option<String> {
        leaf.lines
            .iter()
            .map(|l| copy(self.text, l.content.clone()))
            .map(|l| l.trim().to_owned())
            .find(|l| !l.is_empty())
    }

    fn paragraph(&mut self, leaf: &Leaf<'_>, top: bool) {
        let lines: Vec<_> = leaf.lines.iter().filter(|l| l.has_content).collect();
        let (Some(first), Some(last)) = (lines.first(), lines.last()) else {
            return;
        };
        let first_text = copy(self.text, first.content.clone());
        let last_text = copy(self.text, last.content.clone());
        let id = block_id_of(&last_text);
        if let Some(quote) = &mut self.quote {
            if quote.label.is_none() {
                quote.label = Some(first_text.trim().to_owned());
            }
            quote.id = id;
            return;
        }
        // An id alone on its line names the block before it.
        if top
            && lines.len() == 1
            && let Some(id) = &id
            && first_text.trim() == format!("^{id}")
        {
            if let Some(prev) = self.out.last_mut()
                && prev.id.is_none()
                && !matches!(prev.kind, BlockKind::Heading(_))
            {
                prev.id = Some(id.clone());
            }
            return;
        }
        let mut label = first_text.trim().to_owned();
        if lines.len() == 1
            && let Some(id) = &id
        {
            label = label
                .strip_suffix(&format!("^{id}"))
                .unwrap_or(&label)
                .trim_end()
                .to_owned();
        }
        self.out.push(NoteBlock {
            text: truncate(&label, 80),
            kind: BlockKind::Paragraph,
            start: first.content.start,
            end: last.content.end,
            id,
            new_id: Some(NewId {
                at: last.content.end,
                own_line: false,
                crlf: false,
                gap_after: false,
            }),
            hash: 0,
        });
    }

    fn own_line_block(&mut self, leaf: &Leaf<'_>, kind: BlockKind, fallback: &str) {
        let end = self.trimmed_end(leaf.range.clone());
        let label = self.first_line(leaf).unwrap_or_else(|| fallback.to_owned());
        self.out.push(NoteBlock {
            text: truncate(&label, 80),
            kind,
            start: leaf.range.start,
            end,
            id: None,
            new_id: Some(self.own_line(end)),
            hash: 0,
        });
    }
}

impl<S: TextSource + ?Sized> Visitor for Scan<'_, S> {
    fn enter(&mut self, container: &Container) {
        if self.depth == 0 && container.kind.is_quote() {
            self.quote = Some(Quote {
                start: container.range.start,
                label: None,
                id: None,
            });
        }
        self.depth += 1;
    }

    fn exit(&mut self, container: &Container) {
        self.depth = self.depth.saturating_sub(1);
        if self.depth == 0
            && let Some(quote) = self.quote.take()
        {
            let end = self.trimmed_end(quote.start..container.range.end);
            let label = quote.label.unwrap_or_else(|| "Quote".to_owned());
            self.out.push(NoteBlock {
                text: truncate(&label, 80),
                kind: BlockKind::Quote,
                start: quote.start,
                end,
                new_id: Some(self.own_line(end)),
                id: quote.id,
                hash: 0,
            });
        }
    }

    fn leaf(&mut self, leaf: &Leaf<'_>) {
        let top = self.depth == 0;
        match leaf.kind {
            LeafKind::Paragraph => self.paragraph(leaf, top),
            LeafKind::CalloutTitle => {
                if let Some(quote) = &mut self.quote
                    && quote.label.is_none()
                {
                    let lines: Vec<String> = leaf
                        .lines
                        .iter()
                        .map(|l| copy(self.text, l.content.clone()))
                        .collect();
                    let title = lines.concat().trim().to_owned();
                    if !title.is_empty() {
                        quote.label = Some(title);
                    }
                }
            }
            LeafKind::Heading { .. } => {
                let n = self.headings;
                self.headings += 1;
                if !top {
                    return;
                }
                let label = self
                    .first_line(leaf)
                    .map(|l| l.trim_end_matches('#').trim_end().to_owned())
                    .unwrap_or_default();
                let end = self.trimmed_end(leaf.range.clone());
                self.out.push(NoteBlock {
                    text: truncate(&label, 80),
                    kind: BlockKind::Heading(n),
                    start: leaf.range.start,
                    end,
                    id: None,
                    new_id: None,
                    hash: 0,
                });
            }
            LeafKind::Table if top => self.own_line_block(leaf, BlockKind::Table, "Table"),
            LeafKind::FencedCode | LeafKind::IndentedCode if top => {
                let fallback = leaf
                    .info
                    .clone()
                    .map(|r| copy(self.text, r))
                    .filter(|i| !i.trim().is_empty())
                    .unwrap_or_else(|| "Code".to_owned());
                self.own_line_block(leaf, BlockKind::Code, &fallback);
            }
            LeafKind::MathBlock if top => self.own_line_block(leaf, BlockKind::Math, "Math"),
            _ => {}
        }
    }
}

/// The `^id` at the end of a block's last line.
pub(crate) fn block_id_of(line: &str) -> Option<String> {
    let line = line.trim_end();
    let caret = line.rfind('^')?;
    let id = &line[caret + 1..];
    let before = line[..caret].chars().next_back();
    (!id.is_empty()
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && before.is_none_or(char::is_whitespace))
    .then(|| id.to_owned())
}

pub(crate) fn truncate(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

// ----- other notes -----------------------------------------------------------------

/// The blocks of a note other than the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NoteBlocks {
    pub path: VaultPath,
    pub title: String,
    pub blocks: Vec<NoteBlock>,
    /// The text of each heading, by [`BlockKind::Heading`]'s number.
    pub headings: Vec<String>,
    /// Its block ids (a new one must differ).
    pub taken: Arc<[String]>,
}

/// A block of another note that gets a new id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockRef {
    /// Its place among the note's blocks.
    pub ordinal: usize,
    /// The hash of its text.
    pub hash: u64,
    /// The note's block ids when it was read.
    pub taken: Arc<[String]>,
}

/// What another note's blocks were read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Disk(FileStamp),
    Buffer(Revision, usize),
}

/// The key of `path` now.
fn key_of(root: &Path, content: &ContentSnapshot, path: &VaultPath) -> Option<Key> {
    let record = content.note(path)?;
    if record.kind == RecordKind::Overlay
        && let Some(buffer) = crate::files::overlay::unsaved(&path.to_fs_path(root))
    {
        return Some(Key::Buffer(buffer.revision(), buffer.len()));
    }
    Some(Key::Disk(record.stamp))
}

/// The text of the note at `file`: its unsaved buffer, else the file.
fn note_text(file: &Path) -> Option<String> {
    if let Some(buffer) = crate::files::overlay::unsaved(file) {
        return buffer.text_for_range(0..buffer.len()).ok();
    }
    let meta = std::fs::metadata(file).ok()?;
    if !meta.is_file() || meta.len() > MAX_NOTE_BYTES {
        return None;
    }
    String::from_utf8(std::fs::read(file).ok()?).ok()
}

/// Reads the blocks of `path`.
fn read_note(
    root: &Path,
    content: &ContentSnapshot,
    path: &VaultPath,
    profile: ProfileId,
) -> Option<NoteBlocks> {
    let record = content.note(path)?;
    let text = note_text(&path.to_fs_path(root))?;
    let tree = gv_syntax::parse_with(text.as_str(), ParseOptions::builtin(profile));
    let blocks = blocks(&tree, text.as_str());
    let count = blocks
        .iter()
        .filter_map(|b| match b.kind {
            BlockKind::Heading(n) => Some(n + 1),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let indexed = &record.data.headings;
    let headings: Vec<String> = if indexed.len() >= count {
        indexed.iter().map(|h| h.text.to_string()).collect()
    } else {
        let mut raw = vec![String::new(); count];
        for b in &blocks {
            if let BlockKind::Heading(n) = b.kind {
                raw[n].clone_from(&b.text);
            }
        }
        raw
    };
    let mut taken: Vec<String> = blocks.iter().filter_map(|b| b.id.clone()).collect();
    taken.extend(record.data.block_ids.iter().map(|b| b.id.to_string()));
    taken.sort();
    taken.dedup();
    Some(NoteBlocks {
        path: path.clone(),
        title: record.title().to_owned(),
        blocks,
        headings,
        taken: taken.into(),
    })
}

/// The blocks of every note, for `[[^^`.
#[derive(Debug, Default)]
pub(crate) struct VaultBlocks {
    pub notes: Vec<Arc<NoteBlocks>>,
    /// Every block's text, and its note and place.
    pub labels: Vec<String>,
    pub at: Vec<(usize, usize)>,
}

/// What a [`Job`] reads.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Want {
    Note(VaultPath),
    /// Every note, for this content generation.
    Vault(u64),
}

/// The blocks of other notes read so far.
#[derive(Debug, Default)]
pub(crate) struct Cache {
    root: Option<PathBuf>,
    /// Each note read, with what it was read from (`None`: it could not
    /// be read, not read again until it changes).
    notes: HashMap<VaultPath, (Key, Option<Arc<NoteBlocks>>)>,
    vault: Option<(u64, Arc<VaultBlocks>)>,
    /// The read running.
    loading: Option<Want>,
}

/// Reading blocks off the UI thread.
pub(crate) struct Job {
    root: PathBuf,
    content: ContentSnapshot,
    profile: ProfileId,
    want: Want,
    known: HashMap<VaultPath, (Key, Option<Arc<NoteBlocks>>)>,
}

/// What a [`Job`] read.
pub(crate) struct Done {
    root: PathBuf,
    want: Want,
    notes: Vec<(VaultPath, Key, Option<Arc<NoteBlocks>>)>,
    /// For `[[^^`: every note's blocks.
    vault: Option<(u64, Arc<VaultBlocks>)>,
}

impl Cache {
    fn at_root(&mut self, root: &Path) {
        if self.root.as_deref() != Some(root) {
            *self = Self {
                root: Some(root.to_path_buf()),
                ..Self::default()
            };
        }
    }

    /// The blocks of `path`, when read and current; else the read to run
    /// (none when it runs already).
    pub fn note(
        &mut self,
        root: &Path,
        content: &ContentSnapshot,
        path: &VaultPath,
        profile: ProfileId,
    ) -> (Option<Arc<NoteBlocks>>, Option<Job>) {
        self.at_root(root);
        let key = key_of(root, content, path);
        let have = self.notes.get(path);
        let blocks = have.and_then(|(_, b)| b.clone());
        if have.is_some_and(|(k, _)| Some(*k) == key) {
            return (blocks, None);
        }
        let want = Want::Note(path.clone());
        // An older read is listed meanwhile: accepting checks the block
        // again in the note as it is then.
        if key.is_none() || self.loading.as_ref() == Some(&want) {
            return (blocks, None);
        }
        self.loading = Some(want.clone());
        let job = Job {
            root: root.to_path_buf(),
            content: content.clone(),
            profile,
            want,
            known: HashMap::new(),
        };
        (blocks, Some(job))
    }

    /// The blocks of every note, when read (possibly for an older index);
    /// and the read to run when the index changed since.
    pub fn vault(
        &mut self,
        root: &Path,
        content: &ContentSnapshot,
        profile: ProfileId,
    ) -> (Option<Arc<VaultBlocks>>, Option<Job>) {
        self.at_root(root);
        let generation = content.generation();
        let have = self.vault.as_ref().map(|(_, v)| v.clone());
        if self.vault.as_ref().is_some_and(|(g, _)| *g == generation)
            || matches!(self.loading, Some(Want::Vault(_)))
        {
            return (have, None);
        }
        self.loading = Some(Want::Vault(generation));
        let job = Job {
            root: root.to_path_buf(),
            content: content.clone(),
            profile,
            want: Want::Vault(generation),
            known: self.notes.clone(),
        };
        (have, Some(job))
    }

    /// Keeps what `done` read.
    pub fn finish(&mut self, done: Done) {
        if self.loading.as_ref() == Some(&done.want) {
            self.loading = None;
        }
        if self.root.as_deref() != Some(done.root.as_path()) {
            return;
        }
        if let Some(vault) = done.vault {
            // Notes gone from the vault are forgotten.
            let live: std::collections::HashSet<&VaultPath> =
                vault.1.notes.iter().map(|n| &n.path).collect();
            self.notes
                .retain(|p, (_, b)| b.is_none() || live.contains(p));
            self.vault = Some(vault);
        }
        for (path, key, blocks) in done.notes {
            self.notes.insert(path, (key, blocks));
        }
    }
}

impl Job {
    /// Reads the blocks (off the UI thread).
    pub fn run(self) -> Done {
        let Job {
            root,
            content,
            profile,
            want,
            known,
        } = self;
        let mut notes = Vec::new();
        let mut vault = None;
        match &want {
            Want::Note(path) => {
                if let Some(key) = key_of(&root, &content, path) {
                    let blocks = read_note(&root, &content, path, profile).map(Arc::new);
                    notes.push((path.clone(), key, blocks));
                }
            }
            Want::Vault(generation) => {
                let mut all = VaultBlocks::default();
                for record in content.notes() {
                    let path = &record.path;
                    let Some(key) = key_of(&root, &content, path) else {
                        continue;
                    };
                    let blocks = match known.get(path) {
                        Some((k, blocks)) if *k == key => blocks.clone(),
                        _ => {
                            let blocks = read_note(&root, &content, path, profile).map(Arc::new);
                            notes.push((path.clone(), key, blocks.clone()));
                            blocks
                        }
                    };
                    let Some(blocks) = blocks else {
                        continue;
                    };
                    let n = all.notes.len();
                    for (i, b) in blocks.blocks.iter().enumerate() {
                        all.labels.push(b.text.clone());
                        all.at.push((n, i));
                    }
                    all.notes.push(blocks);
                }
                vault = Some((*generation, Arc::new(all)));
            }
        }
        Done {
            root,
            want,
            notes,
            vault,
        }
    }
}

// ----- adding an id ---------------------------------------------------------------

/// A new block id for another note, asked of the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockIdRequest {
    /// The note, absolute.
    pub file: PathBuf,
    pub root: PathBuf,
    pub note: VaultPath,
    pub block: BlockRef,
    pub id: String,
    pub profile: ProfileId,
}

impl BlockIdRequest {
    /// The note's file name, for messages.
    pub fn name(&self) -> String {
        self.note
            .file_name()
            .unwrap_or(self.note.as_str())
            .to_owned()
    }
}

/// Where `id` goes in `text` for `block`, and the text inserted: the
/// block found again (at its place, else the one block with its text)
/// with no id, and `id` not taken.
pub(crate) fn insertion(
    text: &str,
    profile: ProfileId,
    block: &BlockRef,
    id: &str,
) -> Result<(usize, String), String> {
    let tree = gv_syntax::parse_with(text, ParseOptions::builtin(profile));
    let found = blocks(&tree, text);
    let at_place = found.get(block.ordinal).filter(|b| b.hash == block.hash);
    let target = match at_place {
        Some(b) => Some(b),
        None => {
            let mut same = found.iter().filter(|b| b.hash == block.hash);
            match (same.next(), same.next()) {
                (Some(b), None) => Some(b),
                _ => None,
            }
        }
    };
    let Some(target) = target else {
        return Err("the block changed since it was listed".to_owned());
    };
    let Some(new) = target.new_id else {
        return Err("the block has an id already".to_owned());
    };
    let index = gv_vault::content::extract(text, profile);
    let taken = index
        .block_ids
        .iter()
        .any(|b| b.id.eq_ignore_ascii_case(id))
        || found
            .iter()
            .any(|b| b.id.as_deref().is_some_and(|t| t.eq_ignore_ascii_case(id)));
    if taken {
        return Err(format!("another block has the id ^{id}"));
    }
    Ok((new.at, new.text(id)))
}

/// Adds the id of `request` to its note's file through the refactor
/// journal: refused when the file is not as it was read.
pub(crate) fn write_block_id(
    request: &BlockIdRequest,
    journal: Option<&Path>,
) -> Result<(), String> {
    let bytes = std::fs::read(&request.file).map_err(|e| e.to_string())?;
    let text = String::from_utf8(bytes).map_err(|_| "it is not UTF-8 text".to_owned())?;
    let (at, insert) = insertion(&text, request.profile, &request.block, &request.id)?;
    let mut after = String::with_capacity(text.len() + insert.len());
    after.push_str(&text[..at]);
    after.push_str(&insert);
    after.push_str(&text[at..]);
    let plan = gv_vault::refactor::RefactorPlan {
        root: request.root.clone(),
        moves: Vec::new(),
        moved: Vec::new(),
        edits: vec![gv_vault::refactor::FileEdit {
            path: request.note.clone(),
            moved_to: request.note.clone(),
            before_hash: gv_vault::files::ContentHash::of(text.as_bytes()),
            before: text,
            after,
            changes: Vec::new(),
        }],
        skipped: Vec::new(),
        warnings: Vec::new(),
    };
    gv_vault::refactor::apply(&plan, journal)
        .map(|_| ())
        .map_err(|e| e.to_string())
}
