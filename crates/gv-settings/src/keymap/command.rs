// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Commands: ids, titles, categories, where they apply and their default
//! keys per platform.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;

use super::context::{ContextExpr, ContextParseError};
use super::keystroke::{KeyParseError, KeySequence, Platform};

/// A command's stable id, such as `view.toggle_mode`: dot-separated
/// segments of lower-case letters, digits and `_`, each starting with a
/// letter, at least two segments. The first segment is the area.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandId(Cow<'static, str>);

/// An id that does not follow the [`CommandId`] rules.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "`{0}` is not a command id; ids look like `area.name`, in lower-case letters, digits and `_`"
)]
pub struct InvalidCommandId(pub String);

impl CommandId {
    /// Checks and wraps an id.
    pub fn new(id: impl Into<Cow<'static, str>>) -> Result<Self, InvalidCommandId> {
        let id = id.into();
        if is_valid_id(&id) {
            Ok(Self(id))
        } else {
            Err(InvalidCommandId(id.into_owned()))
        }
    }

    /// The id text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first segment, such as `view`.
    pub fn area(&self) -> &str {
        self.0.split('.').next().unwrap_or_default()
    }
}

fn is_valid_id(id: &str) -> bool {
    let mut segments = 0;
    for segment in id.split('.') {
        segments += 1;
        let mut bytes = segment.bytes();
        let starts_with_letter = bytes.next().is_some_and(|b| b.is_ascii_lowercase());
        if !starts_with_letter
            || !bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return false;
        }
    }
    segments >= 2
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for CommandId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// The group a command is listed under in the palette and the cheatsheet.
/// The derived order is the cheatsheet order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    Application,
    File,
    Tabs,
    Workspace,
    View,
    Navigation,
    Search,
    Links,
    Editing,
    Formatting,
}

impl Category {
    /// Every category, in cheatsheet order.
    pub const ALL: [Self; 10] = [
        Self::Application,
        Self::File,
        Self::Tabs,
        Self::Workspace,
        Self::View,
        Self::Navigation,
        Self::Search,
        Self::Links,
        Self::Editing,
        Self::Formatting,
    ];

    /// The heading shown to the user.
    pub const fn title(self) -> &'static str {
        match self {
            Self::Application => "Application",
            Self::File => "File",
            Self::Tabs => "Tabs",
            Self::Workspace => "Workspace",
            Self::View => "View",
            Self::Navigation => "Navigation",
            Self::Search => "Search",
            Self::Links => "Links",
            Self::Editing => "Editing",
            Self::Formatting => "Formatting",
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.title())
    }
}

/// A set of platforms, for default keys that differ between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Platforms(u8);

impl Platforms {
    pub const MACOS: Self = Self(1);
    pub const LINUX: Self = Self(2);
    pub const WINDOWS: Self = Self(4);
    pub const ALL: Self = Self(7);
    /// Linux and Windows.
    pub const NOT_MACOS: Self = Self(6);

    /// Both sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether `platform` is in the set.
    pub const fn contains(self, platform: Platform) -> bool {
        let bit = match platform {
            Platform::MacOs => 1,
            Platform::Linux => 2,
            Platform::Windows => 4,
        };
        self.0 & bit != 0
    }
}

/// A default key binding of a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultKeys {
    /// The keys in keymap syntax; `secondary-` is resolved per platform.
    pub keys: Cow<'static, str>,
    /// Where this binding is a default.
    pub platforms: Platforms,
}

/// A command the keymap can bind and the palette can list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    id: CommandId,
    title: Cow<'static, str>,
    category: Category,
    context: Option<ContextExpr>,
    defaults: Vec<DefaultKeys>,
}

/// Why a command could not be defined or registered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error(transparent)]
    InvalidId(#[from] InvalidCommandId),
    #[error("{id}: invalid context: {source}")]
    InvalidContext {
        id: String,
        #[source]
        source: ContextParseError,
    },
    #[error("{id}: invalid default keys `{keys}` on {platform}: {source}")]
    InvalidKeys {
        id: String,
        keys: String,
        platform: Platform,
        #[source]
        source: KeyParseError,
    },
    #[error("the command `{0}` is already registered")]
    Duplicate(String),
}

impl Command {
    /// A command without a context or default keys.
    pub fn new(
        id: impl Into<Cow<'static, str>>,
        title: impl Into<Cow<'static, str>>,
        category: Category,
    ) -> Result<Self, CommandError> {
        Ok(Self {
            id: CommandId::new(id)?,
            title: title.into(),
            category,
            context: None,
            defaults: Vec::new(),
        })
    }

    /// Restricts the command to a context (see [`super::context`]). Its
    /// default bindings, and user bindings that give no context, apply only
    /// there; the palette lists it only there.
    pub fn with_context(mut self, context: &str) -> Result<Self, CommandError> {
        let expr = ContextExpr::parse(context).map_err(|source| CommandError::InvalidContext {
            id: self.id.to_string(),
            source,
        })?;
        self.context = Some(expr);
        Ok(self)
    }

    /// Adds default keys on the given platforms.
    pub fn with_keys(
        mut self,
        keys: impl Into<Cow<'static, str>>,
        platforms: Platforms,
    ) -> Result<Self, CommandError> {
        let keys = keys.into();
        for platform in Platform::ALL {
            if platforms.contains(platform) {
                KeySequence::parse(&keys, platform).map_err(|source| {
                    CommandError::InvalidKeys {
                        id: self.id.to_string(),
                        keys: keys.to_string(),
                        platform,
                        source,
                    }
                })?;
            }
        }
        self.defaults.push(DefaultKeys { keys, platforms });
        Ok(self)
    }

    pub fn id(&self) -> &CommandId {
        &self.id
    }

    /// The title shown in the palette, menus and the cheatsheet.
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn category(&self) -> Category {
        self.category
    }

    /// Where the command applies; `None` means everywhere.
    pub fn context(&self) -> Option<&ContextExpr> {
        self.context.as_ref()
    }

    /// Whether the palette should offer the command for the active contexts.
    pub fn is_available(&self, active: &super::context::ContextSet) -> bool {
        self.context.as_ref().is_none_or(|c| c.matches(active))
    }

    /// The default keys as written, for every platform.
    pub fn defaults(&self) -> &[DefaultKeys] {
        &self.defaults
    }

    /// The default key sequences on `platform`, in order.
    pub fn default_keys(&self, platform: Platform) -> Vec<KeySequence> {
        self.defaults
            .iter()
            .filter(|d| d.platforms.contains(platform))
            // Checked in `with_keys`, so nothing is dropped.
            .filter_map(|d| KeySequence::parse(&d.keys, platform).ok())
            .collect()
    }
}

/// Every command the app knows, in registration order.
#[derive(Debug, Clone, Default)]
pub struct CommandRegistry {
    commands: Vec<Command>,
    index: HashMap<CommandId, usize>,
}

impl CommandRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// The built-in commands with their default keys
    /// ([`super::defaults::builtin_commands`]).
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        for command in super::defaults::builtin_commands().into_iter().flatten() {
            // Ids are unique in the built-in table (checked by tests).
            let _ = registry.register(command);
        }
        registry
    }

    /// Adds a command. Ids must be unique.
    pub fn register(&mut self, command: Command) -> Result<(), CommandError> {
        if self.index.contains_key(&command.id) {
            return Err(CommandError::Duplicate(command.id.to_string()));
        }
        self.index.insert(command.id.clone(), self.commands.len());
        self.commands.push(command);
        Ok(())
    }

    /// The command with this id.
    pub fn get(&self, id: &str) -> Option<&Command> {
        let id = CommandId::new(id.to_owned()).ok()?;
        self.index.get(&id).map(|&i| &self.commands[i])
    }

    /// The command's position in registration order.
    pub(crate) fn position(&self, id: &CommandId) -> Option<usize> {
        self.index.get(id).copied()
    }

    /// Every command, in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &Command> {
        self.commands.iter()
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        for ok in ["view.toggle_mode", "editor.heading_1", "a.b.c"] {
            assert!(CommandId::new(ok).is_ok(), "{ok}");
        }
        for bad in [
            "view",
            "View.toggle",
            "view.",
            ".x",
            "view.1x",
            "view.toggle-mode",
            "",
        ] {
            assert!(CommandId::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn invalid_defaults_are_refused() {
        let result = Command::new("view.zoom_in", "Zoom in", Category::View)
            .and_then(|c| c.with_keys("secondary-nope", Platforms::ALL));
        assert!(matches!(result, Err(CommandError::InvalidKeys { .. })));
        let result = Command::new("view.zoom_in", "Zoom in", Category::View)
            .and_then(|c| c.with_context("editor &&"));
        assert!(matches!(result, Err(CommandError::InvalidContext { .. })));
    }
}
