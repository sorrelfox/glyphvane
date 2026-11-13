// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The combined keymap: built-in bindings with the user's on top, checks for
//! conflicts, key resolution and cheatsheet data.

use std::cmp::Ordering;

use super::command::{Category, Command, CommandId, CommandRegistry};
use super::context::{self, ContextExpr, ContextSet, context_info};
use super::diagnostic::{BindingOrigin, KeymapDiagnostic, KeymapIssue};
use super::file::{EntryAction, EntryContext, UserEntry, UserKeymap};
use super::keystroke::{KeySequence, Keystroke, Platform};
use crate::diagnostic::Severity;
use crate::layer::LayerKind;

/// One active binding.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub keys: KeySequence,
    pub command: CommandId,
    /// Arguments passed to the command.
    pub args: Option<toml::Value>,
    /// Where the binding applies; `None` means everywhere.
    pub context: Option<ContextExpr>,
    pub origin: BindingOrigin,
    /// The binding's position in precedence order among equals.
    order: usize,
    specificity: usize,
    reachable: bool,
}

impl Binding {
    /// Whether the binding applies for the active contexts.
    pub fn is_active(&self, active: &ContextSet) -> bool {
        self.context.as_ref().is_none_or(|c| c.matches(active))
    }

    /// How specific the context is ([`ContextExpr::specificity`]); 0 for
    /// bindings that apply everywhere.
    pub fn specificity(&self) -> usize {
        self.specificity
    }

    /// Whether some key press can reach the binding: it is not shadowed
    /// everywhere it applies by a binding that takes precedence on the same
    /// keys or on a prefix of them.
    pub fn is_reachable(&self) -> bool {
        self.reachable
    }

    /// The precedence: the user layer first, then the more specific context,
    /// then the later binding.
    fn rank(&self) -> (LayerKind, usize, usize) {
        (self.origin.layer, self.specificity, self.order)
    }

    fn outranks(&self, other: &Self) -> bool {
        self.rank().cmp(&other.rank()) == Ordering::Greater
    }
}

/// The outcome of a key press, given the keystrokes pressed so far.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Resolution<'a> {
    /// Run this binding's command; the pending keystrokes are cleared.
    Match(&'a Binding),
    /// The keystrokes start a longer chord: wait for the next one. When the
    /// chord is abandoned (a timeout, or a key that continues no chord), the
    /// UI runs `fallback` if there is one.
    PendingChord { fallback: Option<&'a Binding> },
    /// No binding: the key goes on to text input.
    NoMatch,
}

/// A command and its keys, for the cheatsheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheatsheetEntry {
    pub category: Category,
    pub command: CommandId,
    pub title: String,
    /// Key labels for the platform, primary first.
    pub keys: Vec<String>,
}

/// The effective bindings for one platform.
#[derive(Debug, Clone)]
pub struct Keymap {
    platform: Platform,
    bindings: Vec<Binding>,
    diagnostics: Vec<KeymapDiagnostic>,
}

impl Keymap {
    /// The built-in bindings only.
    pub fn defaults(registry: &CommandRegistry, platform: Platform) -> Self {
        Self::build(registry, &UserKeymap::empty(platform))
    }

    /// The built-in bindings with the user's file on top, for the file's
    /// platform. The diagnostics include the file's own.
    pub fn build(registry: &CommandRegistry, user: &UserKeymap) -> Self {
        let platform = user.platform();
        let mut keymap = Self {
            platform,
            bindings: Vec::new(),
            diagnostics: user.diagnostics().to_vec(),
        };
        for command in registry.iter() {
            for keys in command.default_keys(platform) {
                keymap.push_binding(
                    keys,
                    command.id().clone(),
                    None,
                    command.context().cloned(),
                    BindingOrigin::builtin(),
                );
            }
        }
        let text = user.text();
        let entries: Vec<&UserEntry> = user.active_entries().collect();
        for entry in entries
            .iter()
            .filter(|e| matches!(e.action, EntryAction::Unbind { .. }))
        {
            keymap.unbind(registry, entry, text);
        }
        for entry in entries
            .iter()
            .filter(|e| matches!(e.action, EntryAction::Bind { .. }))
        {
            keymap.bind(registry, entry, text);
        }
        keymap.check_pairs(registry);
        keymap.mark_reachable();
        keymap
    }

    fn push_binding(
        &mut self,
        keys: KeySequence,
        command: CommandId,
        args: Option<toml::Value>,
        context: Option<ContextExpr>,
        origin: BindingOrigin,
    ) {
        let specificity = context.as_ref().map_or(0, ContextExpr::specificity);
        let order = self.bindings.len();
        self.bindings.push(Binding {
            keys,
            command,
            args,
            context,
            origin,
            order,
            specificity,
            reachable: true,
        });
    }

    fn unbind(&mut self, registry: &CommandRegistry, entry: &UserEntry, text: &str) {
        let EntryAction::Unbind { command } = &entry.action else {
            return;
        };
        if let Some(id) = command
            && registry.get(id.as_str()).is_none()
        {
            self.unknown_command(entry, id, text);
            return;
        }
        let context = match &entry.context {
            EntryContext::Command => None,
            EntryContext::Anywhere => Some(None),
            EntryContext::Expr(e) => Some(Some(e)),
        };
        let before = self.bindings.len();
        self.bindings.retain(|b| {
            let matches = b.origin.is_builtin()
                && entry.keys.as_ref().is_none_or(|k| *k == b.keys)
                && command.as_ref().is_none_or(|c| *c == b.command)
                && context.is_none_or(|c| c == b.context.as_ref());
            !matches
        });
        if self.bindings.len() == before {
            self.diagnostics.push(
                KeymapDiagnostic::new(
                    Severity::Warning,
                    KeymapIssue::NothingToUnbind,
                    LayerKind::Global,
                    "no built-in binding matches this entry, so it has no effect",
                )
                .origin(&entry.origin),
            );
        }
    }

    fn bind(&mut self, registry: &CommandRegistry, entry: &UserEntry, text: &str) {
        let EntryAction::Bind { command: id, args } = &entry.action else {
            return;
        };
        let Some(keys) = entry.keys.clone() else {
            return;
        };
        let Some(command) = registry.get(id.as_str()) else {
            self.unknown_command(entry, id, text);
            return;
        };
        let context = match &entry.context {
            EntryContext::Command => command.context().cloned(),
            EntryContext::Anywhere => None,
            EntryContext::Expr(e) => Some(e.clone()),
        };
        self.check_context(entry, command, context.as_ref(), text);
        if let Some(first) = keys.keystrokes().first()
            && first.is_printable(self.platform)
            && context::overlaps(context.as_ref(), Some(&ContextExpr::Name("editor".into())))
        {
            self.diagnostics.push(
                KeymapDiagnostic::new(
                    Severity::Warning,
                    KeymapIssue::PrintableKeyInEditor,
                    LayerKind::Global,
                    format!(
                        "`{}` types a character, so binding it where the editor has focus \
                         breaks typing (and input methods); add a modifier, or a context such \
                         as `!editor`",
                        first.label(self.platform)
                    ),
                )
                .origin(&entry.origin),
            );
        }
        self.push_binding(
            keys,
            id.clone(),
            args.clone(),
            context,
            entry.origin.clone(),
        );
    }

    fn unknown_command(&mut self, entry: &UserEntry, id: &CommandId, text: &str) {
        self.diagnostics.push(
            KeymapDiagnostic::new(
                Severity::Warning,
                KeymapIssue::UnknownCommand,
                LayerKind::Global,
                format!("there is no command `{id}`; the entry is ignored"),
            )
            .origin(&entry.origin)
            .at(text, entry.command_span.clone()),
        );
    }

    fn check_context(
        &mut self,
        entry: &UserEntry,
        command: &Command,
        context: Option<&ContextExpr>,
        text: &str,
    ) {
        let span = entry.context_span.clone();
        let Some(expr) = context else {
            return;
        };
        if matches!(entry.context, EntryContext::Expr(_)) {
            for name in expr.names() {
                if context_info(name).is_none() {
                    self.diagnostics.push(
                        KeymapDiagnostic::new(
                            Severity::Warning,
                            KeymapIssue::UnknownContext,
                            LayerKind::Global,
                            format!(
                                "the app never sets the context `{name}`, so this part of the \
                                 context is never active"
                            ),
                        )
                        .origin(&entry.origin)
                        .at(text, span.clone()),
                    );
                }
            }
        }
        let message = if !expr.is_satisfiable() {
            Some(format!("the context `{expr}` can never be active"))
        } else if !context::overlaps(Some(expr), command.context()) {
            command.context().map(|own| {
                format!(
                    "`{}` works only in `{own}`, which never overlaps `{expr}`",
                    command.title()
                )
            })
        } else {
            None
        };
        if let Some(message) = message {
            self.diagnostics.push(
                KeymapDiagnostic::new(
                    Severity::Warning,
                    KeymapIssue::NeverActive,
                    LayerKind::Global,
                    format!("{message}, so this binding has no effect"),
                )
                .origin(&entry.origin)
                .at(text, span),
            );
        }
    }

    /// Reports same-key conflicts, overrides, duplicates and chord prefixes.
    fn check_pairs(&mut self, registry: &CommandRegistry) {
        let title = |id: &CommandId| {
            registry
                .get(id.as_str())
                .map_or_else(|| id.to_string(), |c| c.title().to_owned())
        };
        let platform = self.platform;
        let mut found = Vec::new();
        for (j, later) in self.bindings.iter().enumerate() {
            for earlier in &self.bindings[..j] {
                let same_keys = earlier.keys == later.keys;
                let prefix = earlier.keys.has_strict_prefix(later.keys.keystrokes())
                    || later.keys.has_strict_prefix(earlier.keys.keystrokes());
                if !(same_keys || prefix)
                    || !context::overlaps(earlier.context.as_ref(), later.context.as_ref())
                {
                    continue;
                }
                // Report at the user binding when there is one (the later).
                let (at, other) = (later, earlier);
                let keys = at.keys.label(platform);
                let same_layer = at.origin.layer == other.origin.layer;
                let same_action = at.command == other.command && at.args == other.args;
                let found_one = if same_keys {
                    if same_action {
                        Some((
                            Severity::Info,
                            KeymapIssue::Duplicate,
                            format!(
                                "{keys} is already bound to `{}` ({})",
                                title(&other.command),
                                other.origin
                            ),
                        ))
                    } else if !same_layer {
                        Some((
                            Severity::Info,
                            KeymapIssue::Overrides,
                            format!(
                                "{keys} runs `{}` instead of `{}` ({}) where both apply",
                                title(&at.command),
                                title(&other.command),
                                other.origin
                            ),
                        ))
                    } else if at.specificity == other.specificity {
                        Some((
                            Severity::Warning,
                            KeymapIssue::Conflict,
                            format!(
                                "{keys} is also bound to `{}` ({}) and both can apply at once; \
                                 `{}`, the later one, wins",
                                title(&other.command),
                                other.origin,
                                title(&at.command)
                            ),
                        ))
                    } else {
                        // A deliberate refinement: the more specific context
                        // wins.
                        None
                    }
                } else {
                    let (short, long) = if at.keys.len() < other.keys.len() {
                        (at, other)
                    } else {
                        (other, at)
                    };
                    let (short_keys, long_keys) =
                        (short.keys.label(platform), long.keys.label(platform));
                    if short.outranks(long) {
                        let severity = if same_layer {
                            Severity::Warning
                        } else {
                            Severity::Info
                        };
                        Some((
                            severity,
                            KeymapIssue::ChordPrefix,
                            format!(
                                "{short_keys} runs `{}` at once, so the chord {long_keys} \
                                 (`{}`, {}) cannot be typed where both apply",
                                title(&short.command),
                                title(&long.command),
                                long.origin
                            ),
                        ))
                    } else {
                        Some((
                            Severity::Info,
                            KeymapIssue::ChordPrefix,
                            format!(
                                "{short_keys} starts the chord {long_keys} (`{}`), so `{}` waits \
                                 for the next key and runs only when the chord is abandoned",
                                title(&long.command),
                                title(&short.command)
                            ),
                        ))
                    }
                };
                if let Some((severity, issue, message)) = found_one {
                    found.push(
                        KeymapDiagnostic::new(severity, issue, at.origin.layer, message)
                            .origin(&at.origin)
                            .related(&other.origin),
                    );
                }
            }
        }
        self.diagnostics.extend(found);
    }

    fn mark_reachable(&mut self) {
        let reachable: Vec<bool> = self
            .bindings
            .iter()
            .map(|b| {
                let satisfiable = b.context.as_ref().is_none_or(ContextExpr::is_satisfiable);
                let shadows: Vec<Option<&ContextExpr>> = self
                    .bindings
                    .iter()
                    .filter(|c| {
                        c.outranks(b)
                            && (c.keys == b.keys || b.keys.has_strict_prefix(c.keys.keystrokes()))
                    })
                    .map(|c| c.context.as_ref())
                    .collect();
                satisfiable
                    && (shadows.is_empty() || !context::covered(b.context.as_ref(), &shadows))
            })
            .collect();
        for (b, r) in self.bindings.iter_mut().zip(reachable) {
            b.reachable = r;
        }
    }

    /// The platform the bindings are for.
    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// Every binding, built-in ones (in registry order) first, then the
    /// user's in the order applied.
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// The file's diagnostics and those found combining the layers, with
    /// line and column for user entries.
    pub fn diagnostics(&self) -> &[KeymapDiagnostic] {
        &self.diagnostics
    }

    /// What `pending` (the keystrokes pressed so far, the newest last) does
    /// with the active contexts. Among the active bindings, precedence is:
    /// the user's over built-in ones, then the more specific context, then
    /// the later binding. A binding on exactly `pending` runs at once unless
    /// a longer chord starting with `pending` takes precedence over it; then
    /// the chord is pending and the exact binding is the fallback.
    pub fn resolve(&self, pending: &[Keystroke], active: &ContextSet) -> Resolution<'_> {
        if pending.is_empty() {
            return Resolution::NoMatch;
        }
        let mut exact: Option<&Binding> = None;
        let mut longer: Option<&Binding> = None;
        for b in self.bindings.iter().filter(|b| b.is_active(active)) {
            let slot = if b.keys.keystrokes() == pending {
                &mut exact
            } else if b.keys.has_strict_prefix(pending) {
                &mut longer
            } else {
                continue;
            };
            if slot.is_none_or(|best| b.outranks(best)) {
                *slot = Some(b);
            }
        }
        match (exact, longer) {
            (Some(e), Some(l)) if l.outranks(e) => Resolution::PendingChord { fallback: Some(e) },
            (Some(e), _) => Resolution::Match(e),
            (None, Some(_)) => Resolution::PendingChord { fallback: None },
            (None, None) => Resolution::NoMatch,
        }
    }

    /// The reachable bindings of a command: the user's first, then the
    /// built-in ones, each in order. The first is the primary binding shown
    /// in menus and the palette.
    pub fn bindings_for(&self, command: &str) -> Vec<&Binding> {
        let mut out: Vec<&Binding> = self
            .bindings
            .iter()
            .filter(|b| b.reachable && b.command.as_str() == command)
            .collect();
        out.sort_by_key(|b| (std::cmp::Reverse(b.origin.layer), b.order));
        out
    }

    /// The key labels of a command for this platform, primary first, without
    /// repeats.
    pub fn key_labels(&self, command: &str) -> Vec<String> {
        let mut labels: Vec<String> = Vec::new();
        for b in self.bindings_for(command) {
            let label = b.keys.label(self.platform);
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        labels
    }

    /// Every command with at least one reachable binding, by category
    /// (in [`Category`] order) and then in registry order, with its key
    /// labels.
    pub fn cheatsheet(&self, registry: &CommandRegistry) -> Vec<CheatsheetEntry> {
        let mut entries: Vec<(usize, CheatsheetEntry)> = registry
            .iter()
            .filter_map(|command| {
                let keys = self.key_labels(command.id().as_str());
                if keys.is_empty() {
                    return None;
                }
                let position = registry.position(command.id()).unwrap_or(usize::MAX);
                Some((
                    position,
                    CheatsheetEntry {
                        category: command.category(),
                        command: command.id().clone(),
                        title: command.title().to_owned(),
                        keys,
                    },
                ))
            })
            .collect();
        entries.sort_by_key(|(position, e)| (e.category, *position));
        entries.into_iter().map(|(_, e)| e).collect()
    }
}
