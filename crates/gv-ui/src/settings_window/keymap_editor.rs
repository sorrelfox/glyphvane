// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The keymap editor in the settings window's Keymap section (FR-SET-02,
//! Should). Not designed: built from the settings rows, the search field's
//! frame, the segmented control and the text buttons.
//!
//! - Every registered command is listed by category with its keys on this
//!   platform; a search field filters by title, command id or key label
//!   (`ctrl+s`, `⌘S`).
//! - **Add** (or a click on a key) records a shortcut: the next key presses
//!   (up to a two-keystroke chord) are caught before any binding runs.
//!   *Esc* cancels. The recorded keys show their **conflicts**: other
//!   commands on the same keys, or on a chord they start, where both can
//!   apply. *Save* (no conflict), *Replace* (the other commands lose those
//!   keys) or *Keep both* (the new binding wins where both apply).
//! - **All platforms / This platform**: a top-level entry with the
//!   platform's main modifier written as `secondary` (Cmd on macOS, Ctrl
//!   elsewhere), or an entry in this platform's section with the keys as
//!   pressed.
//! - **×** on a key removes it; **Reset** brings back a changed command's
//!   built-in keys.
//!
//! Changes go through [`gv_settings::keymap::apply_change`], which keeps
//! the file's comments and layout, and are written atomically to
//! `keymap.toml`, then applied at once (the file watcher applies them
//! again, to the same result). In safe mode, and without a config folder,
//! they apply until the app quits and are not saved. A file with problems
//! that make it read-only (a newer version, invalid TOML) is not changed.

use std::path::PathBuf;

use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gv_settings::keymap::{
    Category, CommandId, CommandRegistry, EntryScope, KeySequence, Keymap, KeymapChange, Keystroke,
    Modifiers, Platform, UserKeymap, apply_change, conflicts, is_customized,
};
use gv_settings::{Severity, atomic_write};

use super::controls::{self, OnClick, Segment};
use super::metrics::{field, row as row_metrics};
use crate::appearance::Appearance;
use crate::keymap::Keybindings;
use crate::metrics::{HAIRLINE, font, radius, text};
use crate::settings::AppSettings;
use crate::theme::Theme;

/// The most keystrokes a recorded shortcut has.
const MAX_CHORD: usize = 2;

/// A shortcut being recorded.
struct Recording {
    command: CommandId,
    /// The binding the new keys replace.
    replace: Option<KeySequence>,
    keys: Vec<Keystroke>,
    scope: EntryScope,
    /// Catches key presses before the bindings run.
    _intercept: Subscription,
}

/// A command as listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRow {
    pub id: CommandId,
    pub title: String,
    pub category: Category,
    /// The command's keys, primary first.
    pub keys: Vec<KeySequence>,
    /// Whether the user's file changes its keys.
    pub customized: bool,
}

/// The keymap editor's view.
pub struct KeymapEditor {
    search: Entity<InputState>,
    query: String,
    recording: Option<Recording>,
    recorder: FocusHandle,
    /// Why the last change was not made.
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for KeymapEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeymapEditor")
            .field("query", &self.query)
            .field("recording", &self.recording.as_ref().map(|r| &r.command))
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl KeymapEditor {
    pub fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search commands or keys"));
        let search_events = cx.subscribe_in(&search, window, |this, input, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.query = input.read(cx).value().to_string();
                cx.notify();
            }
        });
        let keymap_changed = cx.observe_global::<Keybindings>(|_, cx| cx.notify());
        Self {
            search,
            query: String::new(),
            recording: None,
            recorder: cx.focus_handle(),
            error: None,
            _subscriptions: vec![search_events, keymap_changed],
        }
    }

    /// Replaces the search query, as typing it does.
    pub fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.search.update(cx, |input, cx| {
            input.set_value(query.to_owned(), window, cx)
        });
        self.query = query.to_owned();
        cx.notify();
    }

    /// The commands listed for the query, by category, with their keys.
    pub fn rows(&self, cx: &App) -> Vec<CommandRow> {
        let Some(bindings) = cx.try_global::<Keybindings>() else {
            return Vec::new();
        };
        command_rows(
            bindings.registry(),
            bindings.keymap(),
            bindings.user(),
            &self.query,
        )
    }

    /// The command whose shortcut is being recorded.
    pub fn recording(&self) -> Option<&CommandId> {
        self.recording.as_ref().map(|r| &r.command)
    }

    /// The keys recorded so far.
    pub fn recorded(&self) -> Option<KeySequence> {
        self.recording
            .as_ref()
            .and_then(|r| KeySequence::new(r.keys.clone()))
    }

    /// The last error.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Starts recording a shortcut for `command`, replacing its binding
    /// `replace` if given.
    pub fn start_recording(
        &mut self,
        command: CommandId,
        replace: Option<KeySequence>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let recorder = self.recorder.clone();
        let view = cx.weak_entity();
        let intercept = cx.intercept_keystrokes(move |event, window, cx| {
            if !recorder.is_focused(window) {
                return;
            }
            cx.stop_propagation();
            let keystroke = event.keystroke.clone();
            // Fails only when the view is gone.
            let _ = view.update(cx, |this, cx| this.press(&keystroke, cx));
        });
        self.recording = Some(Recording {
            command,
            replace,
            keys: Vec::new(),
            scope: EntryScope::AllPlatforms,
            _intercept: intercept,
        });
        self.error = None;
        window.focus(&self.recorder, cx);
        cx.notify();
    }

    /// Stops recording without a change.
    pub fn cancel_recording(&mut self, cx: &mut Context<'_, Self>) {
        self.recording = None;
        cx.notify();
    }

    /// Writes the recorded keys into a platform's section or for all.
    /// The recorder keeps the keyboard.
    pub fn set_scope(
        &mut self,
        scope: EntryScope,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(r) = self.recording.as_mut() {
            r.scope = scope;
            window.focus(&self.recorder, cx);
            cx.notify();
        }
    }

    /// A key press while recording: Esc cancels; other keys are added, a
    /// new chord starting after [`MAX_CHORD`] keystrokes.
    fn press(&mut self, keystroke: &gpui::Keystroke, cx: &mut Context<'_, Self>) {
        let Some(recording) = self.recording.as_mut() else {
            return;
        };
        let m = keystroke.modifiers;
        if keystroke.key == "escape" && !m.modified() {
            self.cancel_recording(cx);
            return;
        }
        let Some(key) = to_model(keystroke) else {
            return;
        };
        if recording.keys.len() >= MAX_CHORD {
            recording.keys.clear();
        }
        recording.keys.push(key);
        cx.notify();
    }

    /// The bindings of other commands the recorded keys clash with: their
    /// titles and key labels.
    pub fn conflicts(&self, cx: &App) -> Vec<(String, String)> {
        let (Some(recording), Some(keys)) = (self.recording.as_ref(), self.recorded()) else {
            return Vec::new();
        };
        let Some(bindings) = cx.try_global::<Keybindings>() else {
            return Vec::new();
        };
        let platform = bindings.keymap().platform();
        conflicts(
            bindings.keymap(),
            bindings.registry(),
            recording.command.as_str(),
            &keys,
        )
        .into_iter()
        .map(|b| {
            let title = bindings
                .registry()
                .get(b.command.as_str())
                .map_or_else(|| b.command.to_string(), |c| c.title().to_owned());
            (title, b.keys.label(platform))
        })
        .collect()
    }

    /// Saves the recorded keys. With `take_over`, the conflicting
    /// bindings of other commands are removed.
    pub fn save_recording(&mut self, take_over: bool, cx: &mut Context<'_, Self>) {
        let Some(keys) = self.recorded() else {
            return;
        };
        let Some(recording) = self.recording.take() else {
            return;
        };
        self.change(
            &KeymapChange::Set {
                command: recording.command,
                keys,
                scope: recording.scope,
                replace: recording.replace,
                take_over,
            },
            cx,
        );
    }

    /// Removes `command`'s binding on `keys`.
    pub fn remove(&mut self, command: CommandId, keys: KeySequence, cx: &mut Context<'_, Self>) {
        self.change(&KeymapChange::Remove { command, keys }, cx);
    }

    /// Brings back `command`'s built-in keys.
    pub fn reset(&mut self, command: CommandId, cx: &mut Context<'_, Self>) {
        self.change(&KeymapChange::Reset { command }, cx);
    }

    fn change(&mut self, change: &KeymapChange, cx: &mut Context<'_, Self>) {
        self.error = write_change(change, cx).err();
        cx.notify();
    }
}

/// Applies `change` to the user's keymap: saved to `keymap.toml` unless in
/// safe mode or without a config folder, and applied at once.
fn write_change(change: &KeymapChange, cx: &mut App) -> Result<(), String> {
    let Some(bindings) = cx.try_global::<Keybindings>() else {
        return Err("The keymap is not loaded.".to_owned());
    };
    let user = bindings.user();
    let text = apply_change(user, bindings.keymap(), bindings.registry(), change)
        .map_err(|e| capitalised(&e.to_string()))?;
    let platform = user.platform();
    let settings = cx.try_global::<AppSettings>();
    let path: Option<PathBuf> = user.path().map(PathBuf::from).or_else(|| {
        settings
            .and_then(AppSettings::locations)
            .map(gv_settings::Locations::keymap_file)
    });
    let save = !settings.is_some_and(AppSettings::safe_mode);
    if save && let Some(path) = &path {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        }
        atomic_write(path, text.as_bytes())
            .map_err(|e| format!("Cannot save {}: {e}", path.display()))?;
    }
    let updated = UserKeymap::parse(&text, path.as_deref(), platform);
    crate::keymap::apply(cx, &updated);
    Ok(())
}

/// The commands of `registry` matching `query` (title, id or key label,
/// ignoring case), by category and then registry order.
pub fn command_rows(
    registry: &CommandRegistry,
    keymap: &Keymap,
    user: &UserKeymap,
    query: &str,
) -> Vec<CommandRow> {
    let platform = keymap.platform();
    let needle = query.trim().to_lowercase();
    let mut rows: Vec<(usize, CommandRow)> = registry
        .iter()
        .enumerate()
        .map(|(i, command)| {
            let mut keys: Vec<KeySequence> = Vec::new();
            for b in keymap.bindings_for(command.id().as_str()) {
                if !keys.contains(&b.keys) {
                    keys.push(b.keys.clone());
                }
            }
            (
                i,
                CommandRow {
                    id: command.id().clone(),
                    title: command.title().to_owned(),
                    category: command.category(),
                    keys,
                    customized: is_customized(user, command.id().as_str()),
                },
            )
        })
        .filter(|(_, row)| {
            needle.is_empty()
                || row.title.to_lowercase().contains(&needle)
                || row.id.as_str().contains(&needle)
                || row.keys.iter().any(|k| {
                    k.label(platform).to_lowercase().contains(&needle)
                        || k.to_string().contains(&needle)
                })
        })
        .collect();
    rows.sort_by_key(|(i, row)| (row.category, *i));
    rows.into_iter().map(|(_, row)| row).collect()
}

/// A GPUI key press as a keymap keystroke; `None` for a modifier alone or
/// a key the keymap cannot name.
fn to_model(keystroke: &gpui::Keystroke) -> Option<Keystroke> {
    const MODIFIER_KEYS: &[&str] = &[
        "shift", "control", "ctrl", "alt", "option", "platform", "cmd", "command", "super", "win",
        "fn", "function", "capslock",
    ];
    if keystroke.key.is_empty() || MODIFIER_KEYS.contains(&keystroke.key.as_str()) {
        return None;
    }
    let m = keystroke.modifiers;
    let raw = Keystroke {
        modifiers: Modifiers {
            control: m.control,
            alt: m.alt,
            shift: m.shift,
            platform: m.platform,
            function: m.function,
        },
        key: keystroke.key.clone(),
    };
    // Through the parser, so the key name is the keymap's canonical one.
    Keystroke::parse(&raw.to_string(), Platform::current()).ok()
}

fn capitalised(message: &str) -> String {
    let mut chars = message.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Key labels in the mono font of menus and the palette: one keycap per
/// keystroke of a chord.
pub(crate) fn key_caps(label: impl Into<SharedString>, theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .h(px(row_metrics::KEY_CAP))
        .px(px(row_metrics::KEY_CAP_PAD_X))
        .flex()
        .items_center()
        .rounded(px(radius::CONTROL))
        .border_1()
        .border_color(theme.border.default)
        .bg(theme.surface.raised)
        .font_family(font::EDITOR)
        .text_size(px(text::MENU_SHORTCUT_SIZE))
        .text_color(theme.text.secondary)
        .child(label.into())
}

impl KeymapEditor {
    fn on_click(
        &self,
        cx: &mut Context<'_, Self>,
        f: impl Fn(&mut Self, &mut Window, &mut Context<'_, Self>) + 'static,
    ) -> OnClick {
        let view = cx.entity().downgrade();
        Box::new(move |_, window, cx| {
            // Fails only when the window has closed.
            let _ = view.update(cx, |this, cx| f(this, window, cx));
        })
    }

    fn head(&self, theme: &Theme, window: &Window, cx: &mut Context<'_, Self>) -> AnyElement {
        let focused = self.search.read(cx).focus_handle(cx).is_focused(window);
        let (problems, read_only) = cx.try_global::<Keybindings>().map_or((0, false), |b| {
            let problems = b
                .keymap()
                .diagnostics()
                .iter()
                .filter(|d| d.severity() != Severity::Info)
                .count();
            (problems, b.user().is_read_only())
        });
        let note: Option<SharedString> = if read_only {
            Some("The keymap file has problems that must be fixed in the file before keys can be changed here.".into())
        } else if problems > 0 {
            Some(format!("The keymap file has {problems} warning(s); open it to see them.").into())
        } else if cx
            .try_global::<AppSettings>()
            .is_some_and(AppSettings::safe_mode)
        {
            Some("Safe mode: changes apply until the app quits and are not saved.".into())
        } else {
            None
        };
        div()
            .pt(px(row_metrics::HEADING_GAP))
            .flex()
            .flex_col()
            .gap(px(row_metrics::GAP_Y))
            .child(
                controls::field_frame(focused, false, Some(row_metrics::KEY_SEARCH_WIDTH), theme)
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.search)
                                .aria_label("Search commands or keys")
                                .appearance(false)
                                .h(px(text::UI.line))
                                .px_0()
                                .py_0()
                                .text_size(px(text::UI.size))
                                .text_color(theme.text.primary),
                        ),
                    ),
            )
            .when_some(note, |d, note| {
                d.child(
                    div()
                        .text_size(px(text::UI_SMALL.size))
                        .line_height(px(text::UI_SMALL.line))
                        .text_color(theme.status.warning)
                        .child(note),
                )
            })
            .when_some(self.error.clone(), |d, error| {
                d.child(
                    div()
                        .debug_selector(|| "keymap-error".into())
                        .text_size(px(text::UI_SMALL.size))
                        .line_height(px(text::UI_SMALL.line))
                        .text_color(theme.status.error)
                        .child(error),
                )
            })
            .into_any_element()
    }

    fn command_row(
        &self,
        row: CommandRow,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let platform = cx
            .try_global::<Keybindings>()
            .map_or(Platform::current(), |b| b.keymap().platform());
        let recording = self
            .recording
            .as_ref()
            .filter(|r| r.command == row.id)
            .is_some();
        let id = row.id.clone();
        let slug = row.id.to_string();
        let mut controls: Vec<AnyElement> = Vec::new();
        if recording {
            controls.push(self.recorder_controls(theme, cx));
        } else {
            for keys in &row.keys {
                let label = keys.label(platform);
                let (change_id, remove_id) = (id.clone(), id.clone());
                let (change_keys, remove_keys) = (keys.clone(), keys.clone());
                let hover = theme.state.hover;
                controls.push(
                    div()
                        .flex()
                        .items_center()
                        .child(
                            key_caps(label.clone(), theme)
                                .id(SharedString::from(format!("keymap-key:{slug}:{keys}")))
                                .debug_selector({
                                    let s = format!("keymap-key:{slug}:{label}");
                                    move || s.clone()
                                })
                                .cursor_pointer()
                                .hover(move |s| s.bg(hover))
                                .on_click(self.on_click(cx, move |this, window, cx| {
                                    this.start_recording(
                                        change_id.clone(),
                                        Some(change_keys.clone()),
                                        window,
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            controls::quiet_button(
                                SharedString::from(format!("keymap-remove:{slug}:{keys}")),
                                "×",
                                self.on_click(cx, move |this, _, cx| {
                                    this.remove(remove_id.clone(), remove_keys.clone(), cx);
                                }),
                                theme,
                            )
                            .debug_selector({
                                let s = format!("keymap-remove:{slug}:{label}");
                                move || s.clone()
                            }),
                        )
                        .into_any_element(),
                );
            }
            let add_id = id.clone();
            controls.push(
                controls::quiet_button(
                    SharedString::from(format!("keymap-add:{slug}")),
                    "Add",
                    self.on_click(cx, move |this, window, cx| {
                        this.start_recording(add_id.clone(), None, window, cx);
                    }),
                    theme,
                )
                .debug_selector({
                    let s = format!("keymap-add:{slug}");
                    move || s.clone()
                })
                .into_any_element(),
            );
            if row.customized {
                let reset_id = id.clone();
                controls.push(
                    controls::quiet_button(
                        SharedString::from(format!("keymap-reset:{slug}")),
                        "Reset",
                        self.on_click(cx, move |this, _, cx| this.reset(reset_id.clone(), cx)),
                        theme,
                    )
                    .debug_selector({
                        let s = format!("keymap-reset:{slug}");
                        move || s.clone()
                    })
                    .into_any_element(),
                );
            }
        }
        controls::row(
            SharedString::from(format!("keymap-row:{slug}")),
            row.title,
            Some(SharedString::from(slug)),
            None,
            controls,
            theme,
        )
        .into_any_element()
    }

    fn scope_segment(
        &self,
        id: &'static str,
        label: String,
        value: EntryScope,
        current: EntryScope,
        cx: &mut Context<'_, Self>,
    ) -> Segment {
        Segment {
            id: id.into(),
            label: label.into(),
            active: current == value,
            on_click: Some(self.on_click(cx, move |this, window, cx| {
                this.set_scope(value, window, cx);
            })),
        }
    }

    /// The controls of the row being recorded: the keys pressed so far,
    /// the scope, conflicts, and the buttons.
    fn recorder_controls(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> AnyElement {
        let platform = Platform::current();
        let recorded = self.recorded();
        let scope = self
            .recording
            .as_ref()
            .map_or(EntryScope::AllPlatforms, |r| r.scope);
        let conflicts = self.conflicts(cx);
        let prompt: SharedString = match &recorded {
            Some(keys) => keys.label(platform).into(),
            None => "Press the keys…".into(),
        };
        let segments = vec![
            self.scope_segment(
                "keymap-scope-all",
                "All platforms".to_owned(),
                EntryScope::AllPlatforms,
                scope,
                cx,
            ),
            self.scope_segment(
                "keymap-scope-platform",
                format!("{platform} only"),
                EntryScope::Platform(platform),
                scope,
                cx,
            ),
        ];
        let mut buttons: Vec<AnyElement> = Vec::new();
        if recorded.is_some() {
            if conflicts.is_empty() {
                buttons.push(button(
                    "keymap-save",
                    "Save",
                    self.on_click(cx, |this, _, cx| this.save_recording(false, cx)),
                    theme,
                ));
            } else {
                buttons.push(button(
                    "keymap-replace",
                    "Replace",
                    self.on_click(cx, |this, _, cx| this.save_recording(true, cx)),
                    theme,
                ));
                buttons.push(button(
                    "keymap-keep-both",
                    "Keep both",
                    self.on_click(cx, |this, _, cx| this.save_recording(false, cx)),
                    theme,
                ));
            }
        }
        buttons.push(button(
            "keymap-cancel",
            "Cancel",
            self.on_click(cx, |this, _, cx| this.cancel_recording(cx)),
            theme,
        ));
        div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(row_metrics::GAP_Y))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(row_metrics::GAP_X))
                    .child(
                        div()
                            .id("keymap-recorder")
                            .debug_selector(|| "keymap-recorder".into())
                            .track_focus(&self.recorder)
                            .min_w(px(row_metrics::KEY_SEARCH_WIDTH / 2.0))
                            .h(px(crate::metrics::size::CONTROL))
                            .px(px(field::PAD_X))
                            .flex()
                            .items_center()
                            .rounded(px(radius::CONTROL))
                            .border_1()
                            .border_color(theme.accent.default)
                            .bg(theme.surface.raised)
                            .font_family(font::EDITOR)
                            .text_size(px(text::MENU_SHORTCUT_SIZE))
                            .when(recorded.is_none(), |d| d.text_color(theme.text.secondary))
                            .child(prompt),
                    )
                    .child(controls::segmented(segments, true, theme)),
            )
            .when(!conflicts.is_empty(), |d| {
                let text_: String = conflicts
                    .iter()
                    .map(|(title, keys)| format!("{title} ({keys})"))
                    .collect::<Vec<_>>()
                    .join(", ");
                d.child(
                    div()
                        .debug_selector(|| "keymap-conflict".into())
                        .text_size(px(text::UI_SMALL.size))
                        .line_height(px(text::UI_SMALL.line))
                        .text_color(theme.status.warning)
                        .child(format!("Also used by: {text_}")),
                )
            })
            .child(div().flex().gap(px(row_metrics::GAP_X)).children(buttons))
            .into_any_element()
    }
}

/// A secondary text button (as the settings window's).
fn button(id: &'static str, label: &'static str, on_click: OnClick, theme: &Theme) -> AnyElement {
    let (hover, press) = (theme.state.hover, theme.state.press);
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .h(px(crate::metrics::size::CONTROL))
        .px(px(row_metrics::SEGMENT_PAD_X))
        .flex()
        .items_center()
        .rounded(px(radius::CONTROL))
        .border_1()
        .border_color(theme.border.strong)
        .bg(theme.surface.raised)
        .font_weight(FontWeight(text::weight::ACTIVE))
        .hover(move |s| s.bg(hover))
        .active(move |s| s.bg(press))
        .on_click(on_click)
        .child(label)
        .into_any_element()
}

impl Render for KeymapEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let rows = self.rows(cx);
        let mut children: Vec<AnyElement> = vec![self.head(theme, window, cx)];
        let mut category = None;
        let count = rows.len();
        for row in rows {
            if category != Some(row.category) {
                children
                    .push(controls::heading(row.category.title(), false, theme).into_any_element());
                category = Some(row.category);
            }
            children.push(self.command_row(row, theme, cx));
        }
        if count == 0 {
            children.push(
                div()
                    .pt(px(row_metrics::HEADING_GAP))
                    .text_color(theme.text.secondary)
                    .child("No command matches.")
                    .into_any_element(),
            );
        }
        div()
            .id("keymap-editor")
            .border_t(px(HAIRLINE))
            .border_color(theme.border.default)
            .flex()
            .flex_col()
            .children(children)
    }
}
