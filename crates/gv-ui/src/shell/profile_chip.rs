// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The status bar's profile chip (SPEC § Status bar), its menu, and the
//! dialect hint infobar (SPEC § Infobars).
//!
//! - **Chip:** the active tab's profile, always visible; its tooltip says
//!   why the profile was chosen (FR-PROF-04).
//! - **Menu** (not designed: drawn like the application menu, opening
//!   upwards from the chip, right-aligned with it): a scope switch (this
//!   file, this folder, this vault), Automatic and the built-in profiles
//!   with a check at the override of that scope, then *Explain detection*,
//!   *Show syntax differences* (disabled until the documentation exists)
//!   and *Open profile documentation* (the profile's upstream reference).
//!   A profile that has not shipped is listed, disabled, with its release.
//! - **Hint:** when a document shown in Preview has two or more distinct
//!   signs of another dialect, the tab shows the information infobar
//!   "This looks like X. Switch profile?" (FR-PROF-05). Its button opens
//!   the menu; dismissing it is remembered for the folder.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Bounds, Context, EntityId, FontWeight, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Pixels, Point, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, anchored, canvas, deferred, div, px,
};
use gv_profile::ProfileId;
use gv_profile::hints::{self, Signals};

use super::infobar::Tooltip;
use super::{Workspace, status_bar};
use crate::files::notice::{Notice, Topic};
use crate::icons::Icon;
use crate::menu::{panel, row, separator};
use crate::metrics::spacing::{menu as menu_spacing, profile_chip};
use crate::metrics::{DISABLED_OPACITY, icon, radius, size, text};
use crate::profile::{self, ProfileOverrides, Scope, TabProfile};
use crate::theme::Theme;

/// The chip's menu.
#[derive(Debug, Default)]
pub(super) struct ProfileMenu {
    open: bool,
    scope: Scope,
    /// The chip, in window coordinates.
    anchor: Rc<Cell<Bounds<Pixels>>>,
}

/// A row of the menu below the profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Explain,
    Differences,
    Documentation,
}

impl Workspace {
    /// The profile of tab `index`, detected again when the tab shows
    /// another file.
    pub(super) fn tab_profile(&mut self, index: usize, cx: &mut Context<'_, Self>) -> TabProfile {
        let tab = &self.tabs[index];
        if let Some(profile) = &tab.profile
            && profile.path == tab.path
        {
            return profile.clone();
        }
        let detected = profile::detect(tab.path.as_deref(), cx);
        self.tabs[index].profile = Some(detected.clone());
        detected
    }

    /// Detects every tab's profile again (an override or a setting
    /// changed) and hands changed profiles to the Source views.
    pub(super) fn refresh_profiles(&mut self, cx: &mut Context<'_, Self>) {
        let mut changed = false;
        for index in 0..self.tabs.len() {
            let detected = profile::detect(self.tabs[index].path.as_deref(), cx);
            let tab = &mut self.tabs[index];
            let same = tab.profile.as_ref().is_some_and(|old| {
                old.path == detected.path
                    && old.detection == detected.detection
                    && old.compiled == detected.compiled
            });
            if same {
                continue;
            }
            changed = true;
            let compiled = detected.compiled.clone();
            tab.profile = Some(detected);
            if let Some(editor) = tab.editor.clone() {
                editor.update(cx, |e, cx| e.set_compiled_profile(&compiled, cx));
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// The profile chip of the active tab.
    pub(super) fn render_profile_chip(
        &mut self,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let profile = self.tab_profile(self.active, cx);
        let anchor = self.profile_menu.anchor.clone();
        let tooltip: SharedString = profile.tooltip().into();
        let hover = theme.state.hover;
        div()
            .id("profile-chip")
            .debug_selector(|| "profile-chip".into())
            .h(px(size::PROFILE_CHIP))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(profile_chip::GAP))
            .pl(px(profile_chip::PAD_LEFT))
            .pr(px(profile_chip::PAD_RIGHT))
            .rounded(px(radius::CHIP))
            .bg(theme.accent.tint)
            .text_color(theme.text.primary)
            .font_weight(FontWeight(text::weight::ACTIVE))
            .hover(move |style| style.bg(hover))
            .tooltip(move |_, cx| {
                let tooltip = tooltip.clone();
                cx.new(|_| Tooltip(tooltip)).into()
            })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_profile_menu(cx)))
            .child(
                canvas(move |bounds, _, _| anchor.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
            .child(Icon::Profile.sized(icon::SMALL))
            .child(status_bar::profile_name(profile.id()))
            .child(Icon::Select.sized(icon::TINY))
            .into_any_element()
    }

    pub(super) fn toggle_profile_menu(&mut self, cx: &mut Context<'_, Self>) {
        self.profile_menu.open = !self.profile_menu.open;
        cx.notify();
    }

    fn close_profile_menu(&mut self, cx: &mut Context<'_, Self>) {
        if self.profile_menu.open {
            self.profile_menu.open = false;
            cx.notify();
        }
    }

    /// Whether the chip's menu is open.
    #[cfg(test)]
    pub(crate) fn profile_menu_open(&self) -> bool {
        self.profile_menu.open
    }

    /// The active tab's file, for overrides.
    fn active_file(&self) -> Option<PathBuf> {
        self.active_tab().path.clone()
    }

    /// Sets the override of the active tab's file at `scope`
    /// (`None`: Automatic).
    pub(crate) fn pick_profile(
        &mut self,
        scope: Scope,
        choice: Option<ProfileId>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(file) = self.active_file() else {
            return;
        };
        self.profile_menu.open = false;
        ProfileOverrides::set(cx, &file, scope, choice);
        // The observer refreshes every window; this one at once.
        self.refresh_profiles(cx);
        cx.notify();
    }

    fn run_profile_command(&mut self, command: Command, cx: &mut Context<'_, Self>) {
        self.profile_menu.open = false;
        let profile = self.tab_profile(self.active, cx);
        match command {
            Command::Explain => {
                let home = gv_vault::root::home_dir();
                let lines = profile.detection.explain(home.as_deref());
                self.tabs[self.active].notice = Some(Notice::profile_explained(&lines));
            }
            Command::Documentation => {
                cx.open_url(gv_profile::definition(profile.id()).reference_url);
            }
            Command::Differences => {}
        }
        cx.notify();
    }

    /// The chip's menu, while it is open.
    pub(super) fn render_profile_menu(
        &mut self,
        window: &mut Window,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        if !self.profile_menu.open {
            return None;
        }
        let anchor = self.profile_menu.anchor.get();
        let viewport = window.viewport_size();
        let scope = self.profile_menu.scope;
        let file = self.active_file();
        let current = file
            .as_deref()
            .and_then(|f| ProfileOverrides::get(cx, f, scope));

        let scopes = div()
            .flex()
            .gap(px(menu_spacing::SEPARATOR_MARGIN_Y))
            .px(px(menu_spacing::PAD))
            .pb(px(menu_spacing::PAD))
            .children(Scope::ALL.into_iter().map(|s| {
                let selected = s == scope;
                let id = format!("profile-menu-scope:{}", scope_id(s));
                div()
                    .id(SharedString::from(id.clone()))
                    .debug_selector(move || id.clone())
                    .flex_1()
                    .h(px(size::PROFILE_CHIP))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(radius::CHIP))
                    .text_size(px(text::UI_SMALL.size))
                    .when(selected, |d| {
                        d.bg(theme.accent.tint)
                            .text_color(theme.text.primary)
                            .font_weight(FontWeight(text::weight::ACTIVE))
                    })
                    .when(!selected, |d| d.text_color(theme.text.secondary))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.profile_menu.scope = s;
                        cx.notify();
                    }))
                    .child(s.label())
                    .into_any_element()
            }));

        let choices = std::iter::once(None).chain(ProfileId::ALL.into_iter().map(Some));
        let rows: Vec<AnyElement> = choices
            .map(|choice| {
                let checked = choice == current;
                let shipped = choice.is_none_or(ProfileId::is_shipped);
                let enabled = shipped && file.is_some();
                let name = choice.map_or("Automatic", status_bar::profile_name);
                let id = format!(
                    "profile-menu-item:{}",
                    choice.map_or("auto", ProfileId::as_str)
                );
                let selector = id.clone();
                row(theme, false)
                    .id(SharedString::from(id))
                    .debug_selector(move || selector.clone())
                    .map(|r| {
                        if enabled {
                            let hover = theme.state.hover;
                            r.hover(move |s| s.bg(hover)).on_click(cx.listener(
                                move |this, _, _, cx| this.pick_profile(scope, choice, cx),
                            ))
                        } else {
                            r.opacity(DISABLED_OPACITY)
                        }
                    })
                    .child(
                        div()
                            .flex_none()
                            .w(px(icon::ROW))
                            .when(checked, |slot| slot.child(Icon::MenuCheck.sized(icon::ROW))),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .children(choice.and_then(ProfileId::ships_in).map(|release| {
                        div()
                            .flex_none()
                            .text_size(px(text::UI_SMALL.size))
                            .text_color(theme.text.secondary)
                            .child(format!("in {release}"))
                    }))
                    .into_any_element()
            })
            .collect();

        let commands =
            [
                (Command::Explain, "Explain detection", true),
                (Command::Differences, "Show syntax differences", false),
                (Command::Documentation, "Open profile documentation", true),
            ]
            .into_iter()
            .map(|(command, title, enabled)| {
                let id = format!("profile-menu-command:{command:?}");
                let selector = id.clone();
                row(theme, false)
                    .id(SharedString::from(id))
                    .debug_selector(move || selector.clone())
                    .map(|r| {
                        if enabled {
                            let hover = theme.state.hover;
                            r.hover(move |s| s.bg(hover)).on_click(cx.listener(
                                move |this, _, _, cx| this.run_profile_command(command, cx),
                            ))
                        } else {
                            r.opacity(DISABLED_OPACITY)
                        }
                    })
                    .child(div().flex_none().w(px(icon::ROW)))
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .into_any_element()
            });

        let menu = panel(theme)
            .debug_selector(|| "profile-menu".into())
            .child(scopes)
            .children(rows)
            .child(separator(theme))
            .children(commands);

        let overlay = div()
            .id("profile-menu-overlay")
            .w(viewport.width)
            .h(viewport.height)
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.prevent_default();
                    this.close_profile_menu(cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, window, cx| {
                    window.prevent_default();
                    this.close_profile_menu(cx);
                }),
            )
            .child(
                div()
                    .absolute()
                    .right(viewport.width - anchor.right())
                    .bottom(viewport.height - anchor.top() + px(menu_spacing::OFFSET_Y))
                    .child(menu),
            );
        Some(
            deferred(anchored().position(Point::default()).child(overlay))
                .with_priority(1)
                .into_any_element(),
        )
    }

    /// The signals of another dialect counted in reader `id`'s document:
    /// shows the hint infobar on its tab when they make one.
    pub(super) fn dialect_signals(
        &mut self,
        id: EntityId,
        signals: &Signals,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.reader.as_ref().is_some_and(|r| r.entity_id() == id))
        else {
            return;
        };
        let profile = self.tab_profile(index, cx);
        let Some(hint) = hints::suggest(signals, &profile.compiled) else {
            return;
        };
        // Detection already names that dialect (and says it is coming).
        if hint.dialect == profile.detection.requested {
            return;
        }
        let tab = &mut self.tabs[index];
        let folder = tab.path.as_deref().and_then(std::path::Path::parent);
        if tab.notice.is_some() || folder.is_some_and(|f| ProfileOverrides::hint_dismissed(cx, f)) {
            return;
        }
        tab.notice = Some(Notice::dialect_hint(&hint.message()));
        cx.notify();
    }

    /// A tab notice was dismissed: a dialect hint stays away in that
    /// folder.
    pub(super) fn tab_notice_dismissed(
        &mut self,
        notice: Option<&Notice>,
        cx: &mut Context<'_, Self>,
    ) {
        if notice.is_some_and(|n| n.topic == Topic::DialectHint)
            && let Some(folder) = self
                .active_tab()
                .path
                .as_deref()
                .and_then(std::path::Path::parent)
        {
            ProfileOverrides::dismiss_hint(cx, folder);
        }
    }
}

fn scope_id(scope: Scope) -> &'static str {
    match scope {
        Scope::File => "file",
        Scope::Folder => "folder",
        Scope::Vault => "vault",
    }
}
