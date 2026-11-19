// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Workspace trust (FR-SET-07, Should; plan 12 § Workspace trust).
//!
//! The first time a folder is opened (its trust is still `ask`), the window
//! shows a non-modal prompt above the document ([`TrustPrompt`]) saying
//! what the folder's content may trigger: remote images and media, links
//! that leave the app, and what the folder holds besides notes (CSS
//! snippets, Obsidian themes and plugins, shared settings;
//! [`gv_vault::trust::scan_trust`], read off the UI thread). It offers:
//!
//! - **Trust**, with the remote-content mode the vault uses (*Allow*,
//!   *Ask* or *Block*; the mode in effect is preselected);
//! - **Restricted mode**: remote content blocked, every external hand-off
//!   confirmed, plugin highlighters and CSS snippets ignored;
//! - **Decide later** (the close button): nothing is stored, and the
//!   prompt comes back the next time the folder opens.
//!
//! The choice is stored in the vault's overlay in the app data dir
//! ([`AppSettings::set_value`] with [`Target::Vault`]):
//! `privacy_and_security.workspace_trust` (`trusted` or `restricted`) and,
//! for Trust, `privacy_and_security.remote_content`. Files inside the vault
//! can never loosen these (the settings layers only let them tighten).
//!
//! Safe mode never shows the prompt (remote content is blocked for the
//! run anyway). [`confirm_every_handoff`] and [`snippets_allowed`] are what
//! the rest of the window asks.
//!
//! Apart from trust, `links.confirm_web_links` asks before the first web
//! link to each site opens in the browser ([`web_site_to_confirm`]); the
//! sites confirmed are remembered until the app quits ([`site_opened`]).
//!
//! Not designed: built as an information infobar with the infobar's
//! metrics, a three-way choice made of infobar buttons, and flagged.

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, App, AppContext as _, Context, EventEmitter, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};
use gv_settings::{RemoteContent, WorkspaceTrust, toml};
use gv_vault::trust::{TrustSummary, scan_trust};

use crate::a11y::{A11yExt as _, Role, Toggled};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::metrics::size::infobar as size;
use crate::metrics::spacing::infobar as spacing;
use crate::metrics::{icon, radius, size as sizes, text};
use crate::settings::{AppSettings, EditRefused, Target};
use crate::theme::Theme;

/// The setting that stores the decision.
pub const TRUST_KEY: &str = "privacy_and_security.workspace_trust";
/// The remote-content mode a trusted vault uses.
pub const REMOTE_KEY: &str = "privacy_and_security.remote_content";

/// What the user decided for a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustChoice {
    /// Trusted, loading remote content as the mode says.
    Trust(RemoteContent),
    /// Restricted mode.
    Restricted,
}

/// The vault's trust in effect, if a vault is open.
pub fn vault_trust(cx: &App) -> Option<WorkspaceTrust> {
    let settings = cx.try_global::<AppSettings>()?;
    settings.vault()?;
    Some(settings.settings().privacy_and_security.workspace_trust)
}

/// Whether the open vault still needs the prompt: its trust is `ask` and
/// the run is not in safe mode.
pub fn needs_prompt(cx: &App) -> bool {
    let safe = cx
        .try_global::<AppSettings>()
        .is_some_and(AppSettings::safe_mode);
    !safe && vault_trust(cx) == Some(WorkspaceTrust::Ask)
}

/// Whether every external hand-off needs a confirmation: the setting, or
/// a restricted vault.
pub fn confirm_every_handoff(cx: &App) -> bool {
    cx.try_global::<AppSettings>().is_some_and(|s| {
        let p = &s.settings().privacy_and_security;
        p.confirm_every_handoff || p.workspace_trust == WorkspaceTrust::Restricted
    })
}

/// The sites (hosts) whose web links were opened after a confirmation,
/// in this run.
#[derive(Default)]
struct ConfirmedSites(std::collections::HashSet<String>);

impl gpui::Global for ConfirmedSites {}

/// The site of the web link `url` when it must be confirmed before it
/// opens: `links.confirm_web_links` is on and no link to the site was
/// opened since the app started. `None` for other URLs (`mailto:`).
pub fn web_site_to_confirm(url: &str, cx: &App) -> Option<String> {
    let on = cx
        .try_global::<AppSettings>()
        .is_some_and(|s| s.settings().links.confirm_web_links);
    if !on {
        return None;
    }
    let site = web_site(url)?;
    let confirmed = cx
        .try_global::<ConfirmedSites>()
        .is_some_and(|sites| sites.0.contains(&site));
    (!confirmed).then_some(site)
}

/// A confirmed link to `url` was handed off: links to the same site no
/// longer ask (`links.confirm_web_links`).
pub fn site_opened(url: &str, cx: &mut App) {
    if let Some(site) = web_site(url) {
        cx.default_global::<ConfirmedSites>().0.insert(site);
    }
}

fn web_site(url: &str) -> Option<String> {
    match gv_vault::url::classify(url) {
        gv_vault::url::UrlClass::Web { host, .. } => Some(host.to_ascii_lowercase()),
        _ => None,
    }
}

/// Whether CSS snippets and plugin highlighters of the vault may be used
/// (not in restricted mode).
pub fn snippets_allowed(cx: &App) -> bool {
    vault_trust(cx) != Some(WorkspaceTrust::Restricted)
}

/// Stores `choice` for the open vault and applies it at once.
pub fn apply(cx: &mut App, choice: TrustChoice) -> Result<(), EditRefused> {
    let string = |s: &str| toml::Value::String(s.to_owned());
    match choice {
        TrustChoice::Trust(mode) => {
            AppSettings::set_value(cx, Target::Vault, REMOTE_KEY, &string(mode_name(mode)))?;
            AppSettings::set_value(cx, Target::Vault, TRUST_KEY, &string("trusted"))
        }
        TrustChoice::Restricted => {
            AppSettings::set_value(cx, Target::Vault, TRUST_KEY, &string("restricted"))
        }
    }
}

fn mode_name(mode: RemoteContent) -> &'static str {
    match mode {
        RemoteContent::Allow => "allow",
        RemoteContent::Ask => "ask",
        RemoteContent::Block => "block",
    }
}

/// The prompt's main sentence, with what the folder holds.
pub fn prompt_detail(summary: Option<&TrustSummary>) -> String {
    let mut text = "Notes can load remote images and media, and their links can open other \
                    apps. Restricted mode blocks remote content, confirms every link that \
                    leaves Glyphvane and ignores CSS snippets."
        .to_owned();
    let found = summary.map(TrustSummary::lines).unwrap_or_default();
    if !found.is_empty() {
        text.push_str(" This folder has ");
        text.push_str(&join_list(&found));
        text.push('.');
    }
    text
}

/// "a", "a and b", "a, b and c".
fn join_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

/// What the prompt tells its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustPromptEvent {
    /// The choice was made and stored; the window hides the prompt.
    Decided(TrustChoice),
    /// Decide later: the window hides the prompt for now.
    Dismissed,
    /// The choice could not be stored (it applies for this run only, or
    /// not at all); the window may say so.
    Failed,
}

/// The trust prompt of one vault.
pub struct TrustPrompt {
    root: PathBuf,
    summary: Option<TrustSummary>,
    mode: RemoteContent,
    _scan: Option<gpui::Task<()>>,
}

impl EventEmitter<TrustPromptEvent> for TrustPrompt {}

impl std::fmt::Debug for TrustPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrustPrompt")
            .field("root", &self.root)
            .field("summary", &self.summary)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl TrustPrompt {
    /// The prompt for the vault at `root`; the folder is scanned off the
    /// UI thread. The remote-content mode in effect is preselected.
    pub fn new(root: PathBuf, cx: &mut Context<'_, Self>) -> Self {
        let mode = cx
            .try_global::<AppSettings>()
            .map_or(RemoteContent::Ask, |s| {
                s.settings().privacy_and_security.remote_content
            });
        let at = root.clone();
        let scan = cx.spawn(async move |this, cx| {
            let summary = cx.background_spawn(async move { scan_trust(&at) }).await;
            this.update(cx, |p, cx| {
                p.summary = Some(summary);
                cx.notify();
            })
            .ok();
        });
        Self {
            root,
            summary: None,
            mode,
            _scan: Some(scan),
        }
    }

    /// The vault's folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// What the scan found, once done.
    pub fn summary(&self) -> Option<&TrustSummary> {
        self.summary.as_ref()
    }

    /// The remote-content mode Trust stores.
    pub fn mode(&self) -> RemoteContent {
        self.mode
    }

    pub fn set_mode(&mut self, mode: RemoteContent, cx: &mut Context<'_, Self>) {
        self.mode = mode;
        cx.notify();
    }

    /// The sentence under the title.
    pub fn detail(&self) -> String {
        prompt_detail(self.summary.as_ref())
    }

    /// Stores `choice` and tells the window.
    pub fn choose(&mut self, choice: TrustChoice, cx: &mut Context<'_, Self>) {
        match apply(cx, choice) {
            Ok(()) => cx.emit(TrustPromptEvent::Decided(choice)),
            Err(err) => {
                tracing::warn!("trust: the choice is not stored: {err}");
                cx.emit(TrustPromptEvent::Failed);
            }
        }
    }

    /// Decide later.
    pub fn dismiss(&mut self, cx: &mut Context<'_, Self>) {
        cx.emit(TrustPromptEvent::Dismissed);
    }

    fn button(
        &self,
        id: &'static str,
        label: &'static str,
        pressed: bool,
        theme: &Theme,
        on_click: impl Fn(&mut Self, &mut Context<'_, Self>) + 'static,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let (hover, press) = (theme.state.hover, theme.state.press);
        div()
            .id(id)
            .a11y_button(label)
            .aria_toggled(if pressed {
                Toggled::True
            } else {
                Toggled::False
            })
            .keyboard_control(theme)
            .debug_selector(move || id.to_owned())
            .flex_none()
            .h(px(size::BUTTON_HEIGHT))
            .px(px(spacing::BUTTON_PAD_X))
            .flex()
            .items_center()
            .rounded(px(radius::CONTROL))
            .border_1()
            .border_color(theme.border.strong)
            .bg(if pressed {
                theme.accent.tint
            } else {
                theme.surface.raised
            })
            .text_color(theme.text.primary)
            .font_weight(FontWeight(text::weight::ACTIVE))
            .hover(move |style| style.bg(hover))
            .active(move |style| style.bg(press))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
            .into_any_element()
    }
}

impl Render for TrustPrompt {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let detail: SharedString = self.detail().into();
        let modes = [
            ("trust-mode-allow", "Allow", RemoteContent::Allow),
            ("trust-mode-ask", "Ask", RemoteContent::Ask),
            ("trust-mode-block", "Block", RemoteContent::Block),
        ];
        let mode_buttons: Vec<AnyElement> = modes
            .into_iter()
            .map(|(id, label, mode)| {
                self.button(
                    id,
                    label,
                    self.mode == mode,
                    theme,
                    move |this, cx| this.set_mode(mode, cx),
                    cx,
                )
            })
            .collect();
        let trust = self.button(
            "trust-accept",
            "Trust",
            false,
            theme,
            |this, cx| {
                let mode = this.mode;
                this.choose(TrustChoice::Trust(mode), cx);
            },
            cx,
        );
        let restricted = self.button(
            "trust-restricted",
            "Restricted mode",
            false,
            theme,
            |this, cx| this.choose(TrustChoice::Restricted, cx),
            cx,
        );
        let (hover, press) = (theme.state.hover, theme.state.press);
        div()
            .id("trust-prompt")
            .role(Role::AlertDialog)
            .aria_label("Trust this vault?")
            .debug_selector(|| "trust-prompt".into())
            .flex_none()
            .min_h(px(sizes::INFOBAR))
            .flex()
            .items_start()
            .gap(px(spacing::GAP))
            .pt(px(spacing::PAD_TOP))
            .pr(px(spacing::PAD_RIGHT))
            .pb(px(spacing::PAD_BOTTOM))
            .pl(px(spacing::PAD_LEFT))
            .bg(theme.status.info_tint)
            .border_b_1()
            .border_color(theme.border.default)
            .text_size(px(text::UI.size))
            .line_height(px(text::UI.line))
            .child(crate::shell::wrapping_row(
                div()
                    .flex_none()
                    .text_color(theme.status.info)
                    .child(Icon::Trust.sized(icon::ROW)),
                div()
                    .flex_auto()
                    .min_w_0()
                    .text_color(theme.text.primary)
                    .flex()
                    .flex_wrap()
                    .gap_x(px(spacing::GAP / 2.0))
                    .child(
                        div()
                            .font_weight(FontWeight(text::weight::STRONG))
                            .child("Trust the files in this folder?"),
                    )
                    .child(
                        div()
                            .debug_selector(|| "trust-detail".into())
                            .min_w_0()
                            .child(detail),
                    ),
                [
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(spacing::GAP))
                        .child(
                            div()
                                .text_color(theme.text.secondary)
                                .child("Remote content:"),
                        )
                        .child(
                            div()
                                .flex()
                                .gap(px(spacing::GAP / 2.0))
                                .children(mode_buttons),
                        )
                        .into_any_element(),
                    trust,
                    restricted,
                ],
            ))
            .child(
                div()
                    .id("trust-dismiss")
                    .a11y_button("Dismiss")
                    .keyboard_control(theme)
                    .debug_selector(|| "trust-dismiss".into())
                    .flex_none()
                    .size(px(size::DISMISS))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(radius::CONTROL))
                    .text_color(theme.text.secondary)
                    .hover(move |style| style.bg(hover))
                    .active(move |style| style.bg(press))
                    .child(Icon::Dismiss.sized(icon::ROW))
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss(cx))),
            )
    }
}
