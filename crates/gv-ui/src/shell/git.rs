// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Git awareness in the window (FR-VAULT-12; SPEC § Sidebar, § Status
//! bar): the branch at the left of the status bar and the status badges at
//! the right of the Files panel's rows. The status comes from the vault's
//! Git watcher ([`super::vault::WindowVault::git`]), which reads it off the
//! UI thread; the window and the Files panel redraw on each
//! [`GitUpdated`].
//!
//! - **Branch:** the `git-branch` icon (12) and the branch name, or the
//!   short commit id when `HEAD` is detached, before the breadcrumb.
//! - **Badges:** JetBrains Mono 10/700, `M` in `status.warning`, `U` in
//!   `status.success` (designed). Not designed: `A` in `status.success`,
//!   `R` in `status.info`, `D` and `C` in `status.error`; ignored files and
//!   folders have no badge and their names in `text.secondary`. A folder
//!   shows the most important status inside it. The tooltip names the
//!   status ("Modified", "Untracked", …).

use gpui::{
    AppContext as _, Context, Div, Entity, FontWeight, InteractiveElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gv_vault::git::{FileStatus, GitStatus};

use super::Workspace;
use super::infobar::Tooltip;
use super::vault::{GitUpdated, WindowVault};
use crate::icons::Icon;
use crate::metrics::{font, icon, text};
use crate::theme::{Color, Theme};

/// The badge letters' size (SPEC § Sidebar: JetBrains Mono 10/700).
const BADGE_SIZE: f32 = 10.0;

impl Workspace {
    /// The window and its Files panel redraw when the vault's Git status
    /// changes.
    pub(super) fn follow_git(&mut self, vault: &Entity<WindowVault>, cx: &mut Context<'_, Self>) {
        self._git_updates = Some(cx.subscribe(vault, |this, _, _: &GitUpdated, cx| {
            if let Some(files) = &this.files {
                files.update(cx, |_, cx| cx.notify());
            }
            cx.notify();
        }));
    }

    /// The branch the status bar shows, when the vault is in a Git work
    /// tree.
    pub(super) fn git_branch(&self, cx: &gpui::App) -> Option<SharedString> {
        let vault = self.vault.as_ref()?.read(cx);
        let status = vault.git()?;
        Some(SharedString::from(status.branch.label().to_owned()))
    }
}

/// The status bar's branch: icon and name.
pub(super) fn branch_label(branch: SharedString) -> Div {
    div()
        .debug_selector(|| "status-git-branch".into())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(crate::metrics::spacing::status_bar::ICON_GAP))
        .child(Icon::GitBranch.sized(icon::SMALL))
        .child(branch)
}

/// The status of a Files panel row: a folder sums up what is inside it.
pub(super) fn row_status(
    status: &GitStatus,
    path: &gv_vault::VaultPath,
    is_dir: bool,
) -> Option<FileStatus> {
    if is_dir {
        status.folder(path)
    } else {
        status.file(path)
    }
}

/// The colour of a badge letter.
pub(super) fn badge_color(status: FileStatus, theme: &Theme) -> Color {
    match status {
        FileStatus::Modified => theme.status.warning,
        FileStatus::Untracked | FileStatus::Added => theme.status.success,
        FileStatus::Renamed => theme.status.info,
        FileStatus::Deleted | FileStatus::Conflicted => theme.status.error,
        FileStatus::Ignored => theme.text.secondary,
    }
}

/// The badge at the right of a Files panel row, with its tooltip; `None`
/// for ignored entries, which are dimmed instead.
pub(super) fn badge(
    ix: usize,
    status: FileStatus,
    theme: &Theme,
) -> Option<impl gpui::IntoElement> {
    let letter = status.badge()?;
    let label: SharedString = status.label().into();
    Some(
        div()
            .id(("git-badge", ix))
            .debug_selector(move || format!("git-badge-{ix}"))
            .flex_none()
            .font_family(font::EDITOR)
            .text_size(px(BADGE_SIZE))
            .font_weight(FontWeight(text::weight::BOLD))
            .text_color(badge_color(status, theme))
            .tooltip(move |_, cx| {
                let label = label.clone();
                cx.new(|_| Tooltip(label)).into()
            })
            .child(SharedString::from(letter.to_string())),
    )
}

#[cfg(test)]
mod tests {
    use gv_vault::git::FileStatus;

    use super::badge_color;
    use crate::theme::ThemeId;

    #[test]
    fn designed_badges_use_their_status_colours() {
        let theme = ThemeId::Light.theme();
        assert_eq!(
            badge_color(FileStatus::Modified, theme),
            theme.status.warning
        );
        assert_eq!(
            badge_color(FileStatus::Untracked, theme),
            theme.status.success
        );
        assert_eq!(
            badge_color(FileStatus::Ignored, theme),
            theme.text.secondary
        );
    }
}
