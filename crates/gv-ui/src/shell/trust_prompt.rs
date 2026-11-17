// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The window's side of workspace trust ([`crate::trust`]).
//!
//! - When the window gets its vault, the settings use that vault's layers
//!   ([`AppSettings::open_vault`]); while its trust is still `ask` (and
//!   the run is not in safe mode) the prompt shows above the document,
//!   before the application notices and the infobar.
//! - Any answer hides it: a choice is stored and applies at once; the
//!   close button (decide later) stores nothing, so the prompt comes back
//!   the next time the folder opens; a choice that cannot be stored hides
//!   it too (logged).
//! - Links that leave the app are confirmed first when the vault is in
//!   restricted mode or the setting asks for it
//!   ([`crate::trust::confirm_every_handoff`], in
//!   [`super::link_actions`]).

use gpui::{AnyElement, AppContext as _, Context, IntoElement as _};

use super::Workspace;
use crate::settings::AppSettings;
use crate::trust::{TrustPrompt, TrustPromptEvent, needs_prompt};

impl Workspace {
    /// The window has a vault: its settings apply, and the trust prompt
    /// shows when the folder has not been decided on yet.
    pub(super) fn vault_opened(&mut self, cx: &mut Context<'_, Self>) {
        let Some(root) = self.vault.as_ref().map(|v| v.read(cx).root().to_path_buf()) else {
            return;
        };
        if !cx.has_global::<AppSettings>() {
            return;
        }
        AppSettings::open_vault(cx, &root);
        if self
            .trust
            .as_ref()
            .is_some_and(|(p, _)| p.read(cx).root() == root.as_path())
        {
            return;
        }
        self.trust = None;
        if !needs_prompt(cx) {
            return;
        }
        let prompt = cx.new(|cx| TrustPrompt::new(root, cx));
        let events = cx.subscribe(&prompt, |this, _, event: &TrustPromptEvent, cx| {
            if *event == TrustPromptEvent::Failed {
                tracing::warn!("trust: the choice could not be stored");
            }
            this.trust = None;
            cx.notify();
        });
        self.trust = Some((prompt, events));
        cx.notify();
    }

    /// The prompt, while it asks.
    pub(super) fn render_trust_prompt(&self) -> Option<AnyElement> {
        self.trust
            .as_ref()
            .map(|(prompt, _)| prompt.clone().into_any_element())
    }

    /// The trust prompt, while it shows (tests).
    #[cfg(test)]
    pub(crate) fn trust_prompt(&self) -> Option<gpui::Entity<TrustPrompt>> {
        self.trust.as_ref().map(|(p, _)| p.clone())
    }
}
