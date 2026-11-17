// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Links and embeds the Source view inserts for a drop or a pasted image
//! (07 § Paste and drop, FR-EDIT-09).
//!
//! - **Where.** A drop goes where it lands ([`EditorView::drop_offset`]);
//!   a pasted image replaces the selection, like any paste.
//! - **How.** Links are written in the vault's link style
//!   ([`LinkStyle::for_vault`]: Obsidian's `useMarkdownLinks` and
//!   `newLinkFormat`, else the `links.*` settings), as wikilinks only where
//!   the profile has them, and checked to resolve back to their target
//!   ([`Linker`]). Images, audio, video and PDFs are embedded; a heading
//!   is linked as `[[Note#Heading]]` (`[[#Heading]]` in its own note).
//!   Several files go one per line.
//! - **One step.** Each insertion is one transaction (one undo step); the
//!   caret goes after the inserted text.
//! - **Pasted images** are saved in the attachment folder
//!   ([`crate::attachments`]) as `Pasted image YYYYMMDDHHMMSS.png` off the
//!   UI thread, then embedded. An untitled document has nowhere to keep
//!   them: the paste does nothing.

use std::ops::Range;
use std::path::{Path, PathBuf};

use gpui::{App, AppContext as _, ClipboardEntry, ClipboardItem, Context, Pixels, Point, Window};
use gv_editor::EditKind;
use gv_vault::{Change, EntryKind, FileMeta, VaultPath};

use super::{EditorStatus, EditorView};
use crate::attachments;
use crate::autocomplete::{LinkStyle, Linker, heading_fragment, is_note, wiki_heading};
use crate::settings::AppSettings;

/// A file (or a heading of a note) to link to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkTarget {
    /// The file, absolute.
    pub path: PathBuf,
    /// Written as an embed (`![[…]]`, `![](…)`).
    pub embed: bool,
    /// A heading of the note to link to.
    pub heading: Option<String>,
}

impl LinkTarget {
    /// A link to `path`, embedded when it is an image, media or a PDF.
    pub(crate) fn file(path: PathBuf) -> Self {
        Self {
            embed: attachments::is_embeddable(&path),
            path,
            heading: None,
        }
    }
}

/// The image on the clipboard, when it holds one and no text.
pub(super) fn clipboard_image(item: &ClipboardItem) -> Option<gpui::Image> {
    let has_text = item
        .entries()
        .iter()
        .any(|e| matches!(e, ClipboardEntry::String(s) if !s.text().is_empty()));
    if has_text {
        return None;
    }
    item.entries().iter().find_map(|e| match e {
        ClipboardEntry::Image(image) => Some(image.clone()),
        _ => None,
    })
}

impl EditorView {
    /// The text offset under window point `position`, when it is over the
    /// text of a document that may be edited.
    pub(crate) fn drop_offset(
        &mut self,
        position: Point<Pixels>,
        window: &Window,
        cx: &App,
    ) -> Option<usize> {
        if self.status != EditorStatus::Ready || self.read_only {
            return None;
        }
        if !self.bounds?.contains(&position) {
            return None;
        }
        self.offset_at(position, window, cx)
    }

    /// The view's bounds as last drawn, for tests.
    #[cfg(test)]
    pub(crate) fn drawn_bounds(&self) -> Option<gpui::Bounds<Pixels>> {
        self.bounds
    }

    /// Whether the document may take an insertion.
    pub(crate) fn accepts_insertions(&self) -> bool {
        self.status == EditorStatus::Ready && !self.read_only
    }

    /// The vault root links are written against: the vault's, else the
    /// document's folder.
    fn link_root(&self) -> Option<PathBuf> {
        self.vault()
            .root
            .or_else(|| self.path.as_deref()?.parent().map(Path::to_path_buf))
    }

    /// The text of a link to each target, in the vault's link style; a
    /// target outside the vault gives none.
    pub(crate) fn link_texts(&self, targets: &[LinkTarget], cx: &App) -> Vec<String> {
        let vault = self.vault();
        let Some(root) = vault.root.clone().or_else(|| self.link_root()) else {
            return Vec::new();
        };
        let profile = self.links.profile();
        let settings = cx
            .try_global::<AppSettings>()
            .map(|s| s.settings().links.clone());
        let mut style = LinkStyle::for_vault(Some(&root), settings.as_ref());
        if !self.autocomplete.features.wikilinks {
            style.markdown = true;
        }
        let vault_path = |path: &Path| {
            let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
            VaultPath::from_relative_fs_path(path.strip_prefix(&root).ok()?).ok()
        };
        let source = vault
            .source
            .clone()
            .or_else(|| self.path.as_deref().and_then(vault_path))
            .or_else(|| VaultPath::new("Untitled.md").ok());
        let Some(source) = source else {
            return Vec::new();
        };
        let resolved: Vec<Option<VaultPath>> =
            targets.iter().map(|t| vault_path(&t.path)).collect();
        // Files made a moment ago (a copied attachment) are not indexed
        // yet: links to them are checked against the index with them.
        let index = vault.index.as_ref().map(|index| {
            let missing: Vec<Change> = resolved
                .iter()
                .flatten()
                .filter(|p| index.entry_by_path(p.as_str()).is_none())
                .map(|p| Change::Upsert {
                    path: p.clone(),
                    kind: EntryKind::File,
                    meta: FileMeta {
                        size: 0,
                        mtime: None,
                        identity: None,
                    },
                })
                .collect();
            if missing.is_empty() {
                index.clone()
            } else {
                index.apply(&missing).unwrap_or_else(|_| index.clone())
            }
        });
        let linker = Linker {
            index: index.as_ref(),
            params: &vault.params,
            source: &source,
            profile,
            style,
        };
        targets
            .iter()
            .zip(resolved)
            .filter_map(|(target, path)| Some(link_text(&linker, target, &path?)))
            .collect()
    }

    /// Replaces `range` (the primary selection when `None`) with `text`:
    /// one transaction, one undo step; the caret goes after it.
    pub(crate) fn insert_text_at(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        if !self.accepts_insertions() || text.is_empty() {
            return false;
        }
        let Some(editor) = self.editor() else {
            return false;
        };
        let current = editor.text();
        let primary = editor.selections().primary().range();
        // The text may have changed since the place was taken.
        let range = range
            .filter(|r| r.end <= current.len() && current.text_for_range(r.clone()).is_ok())
            .unwrap_or(primary);
        let caret = range.start + text.len();
        let mut inserted = false;
        let text = text.to_owned();
        self.with_editor(window, cx, |editor, _| {
            match editor.edit([(range, text)], EditKind::Command) {
                Ok(_) => {
                    editor.set_cursor(caret);
                    inserted = true;
                }
                Err(err) => tracing::warn!("cannot insert the link: {err:?}"),
            }
        });
        inserted
    }

    /// Inserts links to `targets` at `at` (the selection when `None`), one
    /// per line, as one undo step. Returns whether anything was inserted.
    pub(crate) fn insert_links(
        &mut self,
        at: Option<usize>,
        targets: &[LinkTarget],
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let texts = self.link_texts(targets, cx);
        if texts.is_empty() {
            return false;
        }
        self.insert_text_at(at.map(|a| a..a), &texts.join("\n"), window, cx)
    }

    /// Saves the pasted `image` in the attachment folder and embeds it in
    /// place of the selection.
    pub(super) fn paste_image(
        &mut self,
        image: gpui::Image,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.accepts_insertions() {
            return;
        }
        let (Some(note), Some(root)) = (self.path.clone(), self.link_root()) else {
            tracing::info!("a pasted image needs a saved document to sit next to");
            return;
        };
        let Some(range) = self.editor().map(|e| e.selections().primary().range()) else {
            return;
        };
        let default = cx.try_global::<AppSettings>().map_or_else(
            || "/".to_owned(),
            |s| s.settings().files.attachment_folder.clone(),
        );
        let name = attachments::pasted_image_name(chrono::Local::now(), image.format.extension());
        let task = cx.background_spawn(async move {
            let setting = attachments::folder_setting(&root, &default);
            let dir = attachments::folder(&root, Some(&note), &setting);
            attachments::write_new(&dir, &name, &image.bytes)
        });
        cx.spawn_in(window, async move |this, cx| {
            let written = task.await;
            this.update_in(cx, |this, window, cx| match written {
                Ok(path) => {
                    let texts = this.link_texts(&[LinkTarget::file(path)], cx);
                    if let Some(text) = texts.first() {
                        this.insert_text_at(Some(range), text, window, cx);
                    }
                }
                Err(err) => tracing::warn!("cannot save the pasted image: {err}"),
            })
            .ok();
        })
        .detach();
    }
}

/// The link to `target` (at vault path `path`) that `linker` writes.
fn link_text(linker: &Linker<'_>, target: &LinkTarget, path: &VaultPath) -> String {
    let same = path == linker.source;
    let heading = target.heading.as_deref();
    let name = path.file_name().unwrap_or_default();
    let label = if is_note(path) {
        gv_vault::path::stem_of(name)
    } else {
        name
    };
    if !linker.style.markdown {
        let file = if same && heading.is_some() {
            String::new()
        } else {
            linker.wiki_path(path)
        };
        let fragment = heading
            .map(|h| format!("#{}", wiki_heading(h)))
            .unwrap_or_default();
        let bang = if target.embed { "!" } else { "" };
        return format!("{bang}[[{file}{fragment}]]");
    }
    let dest = if same && heading.is_some() {
        String::new()
    } else {
        linker.markdown_dest(path)
    };
    let fragment = heading
        .map(|h| format!("#{}", heading_fragment(linker.profile, h)))
        .unwrap_or_default();
    // `<…>` destinations keep the fragment inside the brackets.
    let dest = match dest.strip_suffix('>') {
        Some(open) => format!("{open}{fragment}>"),
        None => format!("{dest}{fragment}"),
    };
    if target.embed {
        return format!("![]({dest})");
    }
    let text = heading.unwrap_or(label);
    let text = text.replace('[', "\\[").replace(']', "\\]");
    format!("[{text}]({dest})")
}
