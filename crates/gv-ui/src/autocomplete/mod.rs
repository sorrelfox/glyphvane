// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Autocomplete in the Source view (FR-EDIT-10; 07 § Autocomplete).
//!
//! # Triggers
//!
//! Typing opens the popover when the text before the caret on its line
//! asks for a completion ([`context::detect`]); the profile decides which
//! triggers exist:
//!
//! | Typed | Lists | Profiles |
//! |---|---|---|
//! | `[[`, `![[` | Notes and other files by fuzzy name and path; aliases as "Alias → Note" | wikilinks, embeds |
//! | `[[#`, `[[##` | Headings of this note, of every note | wikilinks |
//! | `[[^`, `[[#^`, `[[^^` | Blocks of this note (paragraphs, a new id when one has none), block ids of every note | wikilinks |
//! | `[[Note#`, `[[Note#^` | Headings or block ids of that note | wikilinks |
//! | `](`, `![…](` | Paths of the vault's files, then after `#` the headings of the file | all |
//! | `#` and a letter | Tags of the vault and of this note | tags |
//! | `:` and two letters | gemoji shortcodes | emoji |
//! | `> [!` | Callout or alert types | callouts, alerts |
//! | an opening fence | Languages of the highlighter | all |
//! | `[^` | Footnotes of this note, and a new one | footnotes |
//! | front matter | Property names, then values (tags for `tags`) | front matter |
//!
//! Nothing opens in code, HTML, math or comment blocks or inside a code
//! span, while an input method composes (its text is not typing), with
//! several carets or a selection, or in viewer mode. Moving the caret or
//! deleting keeps an open popover in step with the query and closes it
//! once the caret leaves the trigger; only typing opens it.
//!
//! # Keys and mouse
//!
//! Up and Down move the highlight (wrapping), Enter and Tab accept, Esc
//! closes it until the caret leaves that trigger. The editor keeps the
//! keyboard: the keys are taken in the capture phase while the popover is
//! open, and the editor's key context then also has `Autocomplete`, so
//! `editor && !autocomplete` bindings step aside. A click on a row accepts
//! it.
//!
//! # Accepting
//!
//! The accepted item replaces the query and the closer smart pairs typed
//! (`]]`, `]`, `:`) in one transaction, one undo step, with links written
//! in the vault's link style ([`link`]): with Markdown links, `[[` becomes
//! `[Title](path.md)`. An alias writes `[[Target|Alias]]`. A block without
//! an id gets a new 6-hex-digit id at its end in the same step. A new
//! footnote gets its definition at the end of the document, with the caret
//! there. A Markdown path keeps the caret before `)`, so `#` goes on to
//! the file's headings; an empty `[]` gets the note's title.
//!
//! # Placement and look
//!
//! SPEC § Popovers: 300 wide, rows 24 high with the icon, the label with
//! its matched letters in weight 700 and the folder at the right; the
//! selected row `accent.tint`; key hints in a footer. The labels line up
//! with the typed text, below the caret's row, or above it when the window
//! has no room below; it stays inside the window sideways.
//!
//! Not designed (built from the parts above): the number of rows shown
//! (8, the list scrolling with the highlight), the gap to the caret's row,
//! rows other than files and aliases (headings, blocks, tags, emoji,
//! callouts, languages, footnotes, properties) with their icons and right
//! column, and the footer of triggers other than `[[`.

mod context;
mod items;
mod link;
mod view;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gv_vault::PathIndex;
use gv_vault::content::ContentSnapshot;

pub(crate) use context::{Block, Features, Kind, Scope, Trigger, detect};
pub(crate) use items::{
    AcceptEnv, Item, Leaves, Sources, complete, items, new_block_id, paragraphs,
};
pub(crate) use link::{
    LinkStyle, Linker, encode as encode_segment, heading_fragment, is_note, json_string,
    wiki_heading,
};
#[cfg(test)]
pub(crate) use view::{LABEL_INSET, VISIBLE_ROWS};
pub(crate) use view::{RowClick, RowWheel, panel, panel_size, place};

use crate::switcher::model::{self, Entry, Gather, VaultSource};

/// An open popover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Open {
    pub trigger: Trigger,
    pub items: Vec<Item>,
    /// The highlighted row.
    pub selected: usize,
    /// The first row shown.
    pub first: usize,
}

/// The autocomplete of one Source view.
#[derive(Debug, Default)]
pub(crate) struct State {
    open: Option<Open>,
    /// Esc closed the trigger that starts here.
    dismissed: Option<usize>,
    /// The change being handled is typed text (not a composition).
    pub typed: bool,
    pub features: Features,
    /// The profile's callout or alert types.
    pub callouts: &'static [&'static str],
    /// The window's path index, when the content index has none.
    pub index: Option<PathIndex>,
    /// The quick switcher's file entries for the vault, and what they were
    /// built from (root and index generation).
    files: Option<(PathBuf, u64, Arc<Vec<Entry>>)>,
    /// The vault's headings or block ids (`[[##`, `[[^^`), and what they
    /// were built from (root, content generation, block ids or headings).
    sections: Option<(PathBuf, u64, bool, Arc<Vec<Entry>>)>,
    /// Wheel rows not scrolled yet (a fraction of a row).
    wheel: f32,
}

impl State {
    /// Follows the profile's syntax.
    pub fn set_switches(&mut self, switches: &gv_profile::Switches) {
        use gv_editor::commands::{ALERTS, OBSIDIAN_CALLOUTS};
        use gv_profile::switches::Admonitions;
        self.callouts = match switches.admonitions {
            Admonitions::Callouts => &OBSIDIAN_CALLOUTS,
            Admonitions::Alerts => &ALERTS,
            Admonitions::None => &[],
        };
        self.features = Features {
            wikilinks: switches.wikilinks,
            embeds: switches.embeds,
            tags: switches.tags,
            footnotes: switches.footnotes,
            emoji: switches.emoji,
            callouts: !self.callouts.is_empty(),
            front_matter: switches.front_matter.yaml || switches.front_matter.toml,
        };
        self.close();
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn open(&self) -> Option<&Open> {
        self.open.as_ref()
    }

    pub fn close(&mut self) {
        self.open = None;
    }

    /// Takes the open popover, closing it.
    pub fn take(&mut self) -> Option<Open> {
        self.open.take()
    }

    /// Esc: closes the popover until the caret leaves its trigger.
    pub fn dismiss(&mut self) {
        if let Some(open) = self.open.take() {
            self.dismissed = Some(open.trigger.start);
        }
    }

    /// Whether Esc closed the trigger that starts at `start`.
    pub fn is_dismissed(&self, start: usize) -> bool {
        self.dismissed == Some(start)
    }

    /// Whether Esc closed a trigger the caret may still be in.
    pub fn has_dismissed(&self) -> bool {
        self.dismissed.is_some()
    }

    /// No trigger at the caret: closed, and Esc forgotten.
    pub fn clear(&mut self) {
        self.open = None;
        self.dismissed = None;
    }

    /// Shows `items` for `trigger`; closes when there are none.
    pub fn show(&mut self, trigger: Trigger, items: Vec<Item>) {
        if items.is_empty() {
            self.open = None;
            return;
        }
        // The same query (a new index): the highlight stays on its item;
        // a new query starts from the best match.
        let kept = self
            .open
            .as_ref()
            .filter(|o| o.trigger == trigger)
            .and_then(|o| o.items.get(o.selected))
            .and_then(|old| {
                items
                    .iter()
                    .position(|i| i.label == old.label && i.pick == old.pick)
            });
        let selected = kept.unwrap_or(0);
        let mut open = Open {
            trigger,
            items,
            selected,
            first: 0,
        };
        open.first = selected.saturating_sub(view::VISIBLE_ROWS - 1);
        self.open = Some(open);
        self.wheel = 0.0;
    }

    /// Moves the highlight down (or up), wrapping.
    pub fn step(&mut self, down: bool) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        let n = open.items.len();
        open.selected = if down {
            (open.selected + 1) % n
        } else {
            (open.selected + n - 1) % n
        };
        if open.selected < open.first {
            open.first = open.selected;
        } else if open.selected >= open.first + view::VISIBLE_ROWS {
            open.first = open.selected + 1 - view::VISIBLE_ROWS;
        }
    }

    /// Scrolls the rows by `rows` (down when positive) without moving the
    /// highlight; fractions add up across calls. Whether the rows shown
    /// changed.
    pub fn scroll(&mut self, rows: f32) -> bool {
        let Some(open) = self.open.as_mut() else {
            return false;
        };
        self.wheel += rows;
        let whole = self.wheel.trunc();
        self.wheel -= whole;
        let last = open.items.len().saturating_sub(view::VISIBLE_ROWS);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let first = if whole < 0.0 {
            open.first.saturating_sub((-whole) as usize)
        } else {
            open.first.saturating_add(whole as usize).min(last)
        };
        let changed = first != open.first;
        open.first = first;
        changed
    }

    /// The quick switcher's entries for the vault at `root`: from the
    /// content index when it is built, else from the path index (names
    /// only). Built again when the index changes.
    pub fn files(
        &mut self,
        root: &Path,
        content: Option<&ContentSnapshot>,
        index: Option<&PathIndex>,
    ) -> Arc<Vec<Entry>> {
        let generation = content
            .map(|c| c.generation() << 1)
            .or_else(|| index.map(|i| (i.generation() << 1) | 1))
            .unwrap_or(u64::MAX);
        if let Some((r, g, files)) = &self.files
            && r == root
            && *g == generation
        {
            return files.clone();
        }
        let vault = match (content, index) {
            (Some(content), _) => Some(VaultSource::from_content(root, content)),
            (None, Some(index)) => Some(VaultSource::from_paths(index)),
            (None, None) => None,
        };
        let gather = Gather {
            vault,
            ..Gather::default()
        };
        // Links name files, not titles: notes are listed by file name.
        let files: Arc<Vec<Entry>> = Arc::new(
            model::collect(&gather)
                .into_iter()
                .filter(|e| !matches!(e.kind, model::EntryKind::Unresolved { .. }))
                .map(|mut e| {
                    let name = e
                        .file
                        .as_deref()
                        .and_then(|f| {
                            let stem = if e.kind == model::EntryKind::File {
                                f.file_name()
                            } else {
                                f.file_stem()
                            };
                            stem.map(|s| s.to_string_lossy().into_owned())
                        })
                        .map(Arc::<str>::from);
                    if let Some(name) = name {
                        match e.kind {
                            model::EntryKind::Alias => e.target = Some(name),
                            _ => e.label = name,
                        }
                    }
                    e
                })
                .collect(),
        );
        self.files = Some((root.to_path_buf(), generation, files.clone()));
        files
    }

    /// The headings (or block ids, `blocks`) of every note of the vault
    /// at `root`, as the quick switcher lists them: built again only when
    /// the content index changes, not on every key typed.
    pub fn sections(
        &mut self,
        root: &Path,
        content: &ContentSnapshot,
        blocks: bool,
    ) -> Arc<Vec<Entry>> {
        let generation = content.generation();
        if let Some((r, g, b, entries)) = &self.sections
            && r == root
            && *g == generation
            && *b == blocks
        {
            return entries.clone();
        }
        let entries = Arc::new(model::sections(content, root, blocks));
        self.sections = Some((root.to_path_buf(), generation, blocks, entries.clone()));
        entries
    }
}
