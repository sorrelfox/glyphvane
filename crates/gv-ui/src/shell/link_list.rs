// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! *Links in this note…* (`links.list`, FR-NAV-14) and *Copy link to
//! heading or block* (`links.copy_link_to_heading`).
//!
//! - **Links in this note** lists every link of the active note with its
//!   status in the Go popover (not designed beyond the plan's "lists every
//!   link with its status; Enter follows"): the link as its text reads,
//!   and at the right where it goes (the target's file name) or why it
//!   goes nowhere. Typing filters; Enter follows the link as a click
//!   would, so an unresolved one offers *Create note* and an ambiguous one
//!   its infobar. The list comes from the vault's content index; without
//!   one yet, the popover says the vault is being indexed.
//! - **Copy link to heading or block** copies a link to the block id or
//!   the heading the Source view's caret is in, else to the heading of the
//!   section at the top of the Reading view, in the vault's link style
//!   (09 § Link generation; the shortest form that resolves from the
//!   vault's root).

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, ClipboardItem, Context, Div, Focusable as _, InteractiveElement as _, SharedString,
    Stateful, Window,
};
use gv_vault::resolve::Status;
use gv_vault::{LinkKind, ProfileParams, VaultPath};

use super::Workspace;
use super::links_data;
use crate::autocomplete::{LinkStyle, Linker, heading_fragment, is_note, wiki_heading};
use crate::goto::{self, Choice, GotoEvent, GotoItem, GotoKind, GotoRequest};
use crate::keymap::actions::links as commands;
use crate::reader::Disposition;
use crate::reader::link_info::LinkAt;
use crate::settings::AppSettings;

/// One row of *Links in this note*.
#[derive(Debug, Clone)]
struct Listed {
    item: GotoItem,
    link: LinkAt,
}

/// The view's link kind of an indexed link, and whether it is an image.
fn model_kind(kind: LinkKind) -> (gv_model::LinkKind, bool) {
    match kind {
        LinkKind::Wikilink => (gv_model::LinkKind::Wikilink, false),
        LinkKind::Embed => (gv_model::LinkKind::Embed, false),
        LinkKind::Image | LinkKind::HtmlSrc => (gv_model::LinkKind::Inline, true),
        LinkKind::Reference => (gv_model::LinkKind::Full, false),
        LinkKind::Autolink => (gv_model::LinkKind::UriAutolink, false),
        LinkKind::Inline | LinkKind::HtmlHref | LinkKind::HtmlCard => {
            (gv_model::LinkKind::Inline, false)
        }
    }
}

/// What the row says at the right: where the link goes, or why it goes
/// nowhere.
fn detail(status: Status, target: Option<&SharedString>) -> SharedString {
    match status {
        Status::Unresolved => "not found".into(),
        Status::Ambiguous(n) => format!("{n} notes match").into(),
        Status::OutsideRoot => "outside the vault".into(),
        Status::Blocked => "blocked".into(),
        Status::External => "web".into(),
        Status::Pending => "not indexed yet".into(),
        Status::CaseFold | Status::Loose => match target {
            Some(t) => format!("{t} (approximate)").into(),
            None => "approximate".into(),
        },
        Status::Exact => target.cloned().unwrap_or_default(),
    }
}

impl Workspace {
    /// The active note's file and its path in the vault.
    fn active_note(&self, cx: &App) -> Option<(std::path::PathBuf, VaultPath)> {
        let file = self.active_tab().path.clone()?;
        let root = self.link_vault_root(cx)?;
        let rel = file.strip_prefix(&root).ok()?;
        let path = VaultPath::from_relative_fs_path(rel).ok()?;
        Some((file, path))
    }

    /// The text of the active note at `range`: the Source view's, else the
    /// Reading view's.
    fn active_text(&self, range: std::ops::Range<usize>, cx: &App) -> Option<Arc<str>> {
        let tab = self.active_tab();
        if let Some(editor) = &tab.editor
            && let Some(e) = editor.read(cx).editor()
        {
            return e.text().text_for_range(range).ok().map(Arc::from);
        }
        tab.reader.as_ref()?.read(cx).source_text(range)
    }

    /// The rows of *Links in this note*, or why there are none.
    fn listed_links(
        &self,
        profile: gv_vault::ProfileId,
        cx: &App,
    ) -> Result<Vec<Listed>, &'static str> {
        let (file, path) = self.active_note(cx).ok_or("This note is not in a vault.")?;
        let content = self
            .vault
            .as_ref()
            .and_then(|v| v.read(cx).content())
            .ok_or("The vault is still being indexed.")?;
        let (rows, _) = links_data::outgoing(&content, &path, None);
        let indexed = content.outgoing(&path);
        let listed = rows
            .into_iter()
            .zip(indexed)
            .map(|(row, out)| {
                let construct = self.active_text(row.span.clone(), cx);
                let label: SharedString = construct
                    .as_deref()
                    .map_or_else(|| row.label.clone(), |c| links_data::link_label(c).into());
                let (kind, image) = model_kind(out.link.kind);
                Listed {
                    item: GotoItem {
                        label,
                        detail: Some(detail(row.status, row.target_name.as_ref())),
                        depth: 0,
                    },
                    link: LinkAt {
                        file: file.clone(),
                        kind,
                        image,
                        destination: out.link.raw.clone(),
                        range: Some(row.span),
                        construct,
                        profile,
                    },
                }
            })
            .collect();
        Ok(listed)
    }

    /// `links.list`: the active note's links in the Go popover; Enter
    /// follows one.
    fn list_links(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.ensure_vault(cx);
        let profile = self.tab_profile(self.active, cx).compiled.id;
        let (rows, empty) = match self.listed_links(profile, cx) {
            Ok(rows) => (rows, "No links"),
            Err(why) => (Vec::new(), why),
        };
        let items = rows.iter().map(|r| r.item.clone()).collect();
        let links: Vec<LinkAt> = rows.into_iter().map(|r| r.link).collect();
        let tab = self.active;
        let workspace = cx.weak_entity();
        let request = GotoRequest {
            title: "Links in this note".into(),
            kind: GotoKind::List {
                placeholder: "Links in this note".into(),
                action: "follow".into(),
                empty: empty.into(),
                items,
                initial: 0,
            },
            on_event: Rc::new(move |event, window, cx| {
                if let GotoEvent::Confirm(Choice::Item(i)) = event
                    && let Some(link) = links.get(i).cloned()
                {
                    let _ = workspace.update(cx, |ws, cx| {
                        ws.follow_link_at(tab, link, Disposition::InPlace, false, window, cx);
                    });
                }
            }),
        };
        window.defer(cx, move |window, cx| {
            goto::open(request, window, cx);
        });
    }

    /// The fragment `links.copy_link_to_heading` links to: the block id
    /// or heading the Source view's caret is in, else the heading of the
    /// section at the top of the Reading view. `Ok` is a heading's text,
    /// `Err` a block id.
    fn link_fragment(&self, window: &Window, cx: &App) -> Option<Result<String, String>> {
        if let Some(editor) = self.shown_editor()
            && editor
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
        {
            let caret = editor.read(cx).editor()?.selections().primary().head();
            let (_, path) = self.active_note(cx)?;
            let content = self.vault.as_ref()?.read(cx).content()?;
            let data = &content.note(&path)?.data;
            if let Some(block) = data.block_ids.iter().find(|b| b.span.contains(caret)) {
                return Some(Err(block.id.to_string()));
            }
            let heading = data
                .headings
                .iter()
                .rev()
                .find(|h| (h.span.start as usize) <= caret)?;
            return Some(Ok(heading.source.to_string()));
        }
        Some(Ok(self.active_doc()?.heading_text()?.to_string()))
    }

    /// `links.copy_link_to_heading`.
    fn copy_link_to_heading(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(fragment) = self.link_fragment(window, cx) else {
            return;
        };
        let Some((_, target)) = self.active_note(cx) else {
            return;
        };
        let Some(root) = self.link_vault_root(cx) else {
            return;
        };
        let index = self.vault.as_ref().map(|v| v.read(cx).snapshot());
        let profile = self.tab_profile(self.active, cx).compiled.id;
        let settings = cx
            .try_global::<AppSettings>()
            .map(|s| s.settings().links.clone());
        let style = LinkStyle::for_vault(Some(&root), settings.as_ref());
        let params = ProfileParams::for_profile(profile);
        let text = link_to(&target, &fragment, index.as_ref(), &params, profile, style);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// The handlers of `links.list` and `links.copy_link_to_heading`, where
    /// the active tab shows a note.
    pub(super) fn link_list_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        let Some(path) = self.active_tab().path.as_deref() else {
            return root;
        };
        if !crate::platform::is_markdown(path) {
            return root;
        }
        root.on_action(cx.listener(|this, _: &commands::List, window, cx| {
            this.list_links(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &commands::CopyLinkToHeading, window, cx| {
                this.copy_link_to_heading(window, cx);
            }),
        )
    }
}

/// A link to `fragment` (`Ok` a heading's text, `Err` a block id) of the
/// note `target`, as written from the vault's root in `style`.
fn link_to(
    target: &VaultPath,
    fragment: &Result<String, String>,
    index: Option<&gv_vault::PathIndex>,
    params: &ProfileParams,
    profile: gv_vault::ProfileId,
    style: LinkStyle,
) -> String {
    // From a note at the root, so the link reads the same wherever it is
    // pasted in Shortest form.
    let source = VaultPath::new("Untitled.md").unwrap_or_else(|_| target.clone());
    let linker = Linker {
        index,
        params,
        source: &source,
        profile,
        style,
    };
    let name = target.file_name().unwrap_or_default();
    let stem = if is_note(target) {
        gv_vault::path::stem_of(name)
    } else {
        name
    };
    if style.markdown {
        let dest = linker.markdown_dest(target);
        let (label, anchor) = match fragment {
            Ok(heading) => (heading.clone(), heading_fragment(profile, heading)),
            Err(id) => (format!("{stem} ^{id}"), format!("^{id}")),
        };
        let dest = match dest.strip_suffix('>') {
            Some(open) => format!("{open}#{anchor}>"),
            None => format!("{dest}#{anchor}"),
        };
        format!("[{label}]({dest})")
    } else {
        let path = linker.wiki_path(target);
        match fragment {
            Ok(heading) => format!("[[{path}#{}]]", wiki_heading(heading)),
            Err(id) => format!("[[{path}#^{id}]]"),
        }
    }
}

/// [`link_to`] without an index, in the Obsidian profile (tests).
#[cfg(test)]
pub(super) fn link_to_for_tests(
    target: &str,
    fragment: Result<&str, &str>,
    markdown: bool,
) -> String {
    let target = VaultPath::new(target).expect("path");
    let style = LinkStyle {
        markdown,
        ..LinkStyle::default()
    };
    let profile = gv_vault::ProfileId::Obsidian;
    let params = ProfileParams::for_profile(profile);
    let fragment = fragment.map(str::to_owned).map_err(str::to_owned);
    link_to(&target, &fragment, None, &params, profile, style)
}
