// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Changing `keymap.toml` from the keymap editor, keeping the rest of the
//! file: its comments, spacing and order stay as written, entries are
//! appended with `toml_edit`, and removed entries take only their own
//! table (with the comments directly above it) along.
//!
//! The editor asks for a [`KeymapChange`]; [`apply_change`] turns it into
//! the new text from the parsed file and the combined keymap:
//!
//! - **Set** binds keys to a command: a `bind` entry, top-level with the
//!   portable `secondary` modifier (Cmd on macOS, Ctrl elsewhere) or in
//!   the current platform's section. Replacing one of the command's
//!   bindings removes it: the user's entry is deleted, a built-in binding
//!   gets an `unbind` entry in the platform's section. With `take_over`,
//!   the bindings of other commands on the same keys ([`conflicts`]) are
//!   removed the same way.
//! - **Remove** drops one binding of a command the same way.
//! - **Reset** deletes the user's `bind` and `unbind` entries naming the
//!   command, top-level and in the platform's section, which brings back
//!   its built-in keys.

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use super::bindings::{Binding, Keymap};
use super::command::{CommandId, CommandRegistry};
use super::context;
use super::file::{EntryAction, KEYMAP_VERSION, UserKeymap};
use super::keystroke::{KeySequence, Modifiers, Platform};
use crate::edit::EditError;

/// Where a new `bind` entry goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryScope {
    /// Top-level, for every platform: the platform's main modifier is
    /// written as `secondary`.
    AllPlatforms,
    /// The section of one platform, with the keys as pressed.
    Platform(Platform),
}

/// A change asked for in the keymap editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeymapChange {
    /// Gives `command` the keys `keys`. With `replace`, that binding of the
    /// command goes. With `take_over`, other commands' bindings on the same
    /// keys (or on a chord one starts) where both apply go too; without it
    /// they stay, and the new binding wins where it is more specific or
    /// equally specific (user bindings rank first).
    Set {
        command: CommandId,
        keys: KeySequence,
        scope: EntryScope,
        replace: Option<KeySequence>,
        take_over: bool,
    },
    /// Removes the binding of `command` on `keys`.
    Remove {
        command: CommandId,
        keys: KeySequence,
    },
    /// Brings back the built-in keys of `command`.
    Reset { command: CommandId },
}

/// The keys in keymap syntax with the platform's main modifier written as
/// `secondary`, so that one top-level entry means Cmd on macOS and Ctrl
/// elsewhere.
pub fn portable_keys(keys: &KeySequence, platform: Platform) -> String {
    let secondary = Modifiers::secondary(platform);
    let mut out = String::new();
    for (i, k) in keys.keystrokes().iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let m = k.modifiers;
        let has_secondary = (secondary.control && m.control) || (secondary.platform && m.platform);
        if has_secondary {
            out.push_str("secondary-");
        }
        for (on, name) in [
            (m.function, "fn-"),
            (m.control && !secondary.control, "ctrl-"),
            (m.alt, "alt-"),
            (m.platform && !secondary.platform, "cmd-"),
            (m.shift, "shift-"),
        ] {
            if on {
                out.push_str(name);
            }
        }
        out.push_str(&k.key);
    }
    out
}

/// The bindings of commands other than `command` that clash with binding
/// it to `keys`: the same keys, or one a strict start of the other (a
/// chord), in contexts that can be active together. Only reachable
/// bindings count, in the keymap's order.
pub fn conflicts<'a>(
    keymap: &'a Keymap,
    registry: &CommandRegistry,
    command: &str,
    keys: &KeySequence,
) -> Vec<&'a Binding> {
    let own = registry.get(command).and_then(|c| c.context());
    keymap
        .bindings()
        .iter()
        .filter(|b| b.is_reachable() && b.command.as_str() != command)
        .filter(|b| {
            b.keys == *keys
                || b.keys.has_strict_prefix(keys.keystrokes())
                || keys.has_strict_prefix(b.keys.keystrokes())
        })
        .filter(|b| context::overlaps(b.context.as_ref(), own))
        .collect()
}

/// Whether the user changed `command`'s keys: a `bind` or `unbind` entry
/// of the file names it on `user`'s platform.
pub fn is_customized(user: &UserKeymap, command: &str) -> bool {
    user.active_entries().any(|e| match &e.action {
        EntryAction::Bind { command: c, .. } => c.as_str() == command,
        EntryAction::Unbind { command: Some(c) } => c.as_str() == command,
        EntryAction::Unbind { command: None } => false,
    })
}

/// The text of `user`'s file after `change`. Refuses a file that is
/// read-only (newer version, broken TOML) or not valid TOML.
pub fn apply_change(
    user: &UserKeymap,
    keymap: &Keymap,
    registry: &CommandRegistry,
    change: &KeymapChange,
) -> Result<String, EditError> {
    if user.is_read_only() {
        return Err(EditError::Syntax(
            "the keymap file cannot be changed until its problems are fixed".to_owned(),
        ));
    }
    let platform = user.platform();
    let mut plan = Plan::default();
    match change {
        KeymapChange::Set {
            command,
            keys,
            scope,
            replace,
            take_over,
        } => {
            let has = keymap
                .bindings_for(command.as_str())
                .iter()
                .any(|b| b.keys == *keys);
            if let Some(old) = replace.as_ref().filter(|old| *old != keys) {
                for b in keymap.bindings_for(command.as_str()) {
                    if b.keys == *old {
                        plan.drop_binding(b);
                    }
                }
            }
            if *take_over {
                for b in conflicts(keymap, registry, command.as_str(), keys) {
                    plan.drop_binding(b);
                }
            }
            if !has {
                let (section, text) = match scope {
                    EntryScope::AllPlatforms => (None, portable_keys(keys, platform)),
                    EntryScope::Platform(p) => (Some(*p), keys.to_string()),
                };
                plan.binds.push((section, text, command.to_string()));
            }
        }
        KeymapChange::Remove { command, keys } => {
            for b in keymap.bindings_for(command.as_str()) {
                if b.keys == *keys {
                    plan.drop_binding(b);
                }
            }
        }
        KeymapChange::Reset { command } => {
            for entry in user.active_entries() {
                let named = match &entry.action {
                    EntryAction::Bind { command: c, .. } => c == command,
                    EntryAction::Unbind { command: Some(c) } => c == command,
                    EntryAction::Unbind { command: None } => false,
                };
                if named && let Some(key) = &entry.origin.key {
                    plan.remove.push(key.clone());
                }
            }
        }
    }
    plan.apply(user.text(), platform)
}

/// The entries a change removes and adds.
#[derive(Default)]
struct Plan {
    /// Paths of user entries to delete, such as `bind[2]`.
    remove: Vec<String>,
    /// Built-in bindings to unbind in the platform's section: keys, command.
    unbinds: Vec<(String, String)>,
    /// New `bind` entries: section, keys, command.
    binds: Vec<(Option<Platform>, String, String)>,
}

impl Plan {
    fn drop_binding(&mut self, b: &Binding) {
        if b.origin.is_builtin() {
            let entry = (b.keys.to_string(), b.command.to_string());
            if !self.unbinds.contains(&entry) {
                self.unbinds.push(entry);
            }
        } else if let Some(key) = &b.origin.key
            && !self.remove.contains(key)
        {
            self.remove.push(key.clone());
        }
    }

    fn apply(self, text: &str, platform: Platform) -> Result<String, EditError> {
        let mut doc: DocumentMut = text
            .parse()
            .map_err(|e: toml_edit::TomlError| EditError::Syntax(e.message().to_owned()))?;
        if text.trim().is_empty() {
            doc.insert("version", toml_edit::value(i64::from(KEYMAP_VERSION)));
        }
        remove_entries(&mut doc, &self.remove)?;
        for (keys, command) in &self.unbinds {
            let mut table = Table::new();
            table.insert("keys", toml_edit::value(keys.as_str()));
            table.insert("command", toml_edit::value(command.as_str()));
            push_entry(&mut doc, Some(platform), "unbind", table)?;
        }
        for (section, keys, command) in &self.binds {
            let mut table = Table::new();
            table.insert("keys", toml_edit::value(keys.as_str()));
            table.insert("command", toml_edit::value(command.as_str()));
            push_entry(&mut doc, *section, "bind", table)?;
        }
        let mut out = doc.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        Ok(out)
    }
}

/// Splits an entry path (`bind[2]`, `linux.unbind[0]`) into section, list
/// and index.
fn entry_path(path: &str) -> Option<(Option<&str>, &str, usize)> {
    let (head, index) = path.strip_suffix(']')?.rsplit_once('[')?;
    let index = index.parse().ok()?;
    match head.split_once('.') {
        Some((section, list)) => Some((Some(section), list, index)),
        None => Some((None, head, index)),
    }
}

/// Deletes the entries at `paths`, the highest index of each list first so
/// the others keep their positions. An emptied list is removed.
fn remove_entries(doc: &mut DocumentMut, paths: &[String]) -> Result<(), EditError> {
    let mut parsed: Vec<_> = paths
        .iter()
        .map(|p| entry_path(p).ok_or_else(|| EditError::BadPath(p.clone())))
        .collect::<Result<_, _>>()?;
    parsed.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)).then(b.2.cmp(&a.2)));
    parsed.dedup();
    for (section, list, index) in parsed {
        let parent: &mut dyn toml_edit::TableLike = match section {
            None => doc.as_table_mut(),
            Some(name) => match doc.get_mut(name).and_then(Item::as_table_like_mut) {
                Some(t) => t,
                None => continue,
            },
        };
        let emptied = match parent.get_mut(list) {
            Some(Item::ArrayOfTables(tables)) => {
                if index < tables.len() {
                    tables.remove(index);
                }
                tables.is_empty()
            }
            Some(Item::Value(Value::Array(array))) => {
                if index < array.len() {
                    array.remove(index);
                }
                array.is_empty()
            }
            _ => false,
        };
        if emptied {
            parent.remove(list);
        }
    }
    // A platform table left with nothing in it goes too.
    for platform in Platform::ALL {
        let empty = doc
            .get(platform.name())
            .and_then(Item::as_table_like)
            .is_some_and(toml_edit::TableLike::is_empty);
        if empty {
            doc.remove(platform.name());
        }
    }
    Ok(())
}

/// Appends `table` to the `list` (`bind` or `unbind`) of `section` (the
/// top level for `None`), as `[[list]]` or `[[section.list]]`, or to an
/// inline array when the file writes the list that way.
fn push_entry(
    doc: &mut DocumentMut,
    section: Option<Platform>,
    list: &str,
    table: Table,
) -> Result<(), EditError> {
    let parent: &mut dyn toml_edit::TableLike = match section {
        None => doc.as_table_mut(),
        Some(platform) => {
            let name = platform.name();
            if !doc.contains_key(name) {
                let mut t = Table::new();
                t.set_implicit(true);
                doc.insert(name, Item::Table(t));
            }
            doc.get_mut(name)
                .and_then(Item::as_table_like_mut)
                .ok_or_else(|| EditError::NotATable(name.to_owned()))?
        }
    };
    let path = || match section {
        None => list.to_owned(),
        Some(p) => format!("{}.{list}", p.name()),
    };
    match parent.get_mut(list) {
        None => {
            let mut tables = ArrayOfTables::new();
            tables.push(table);
            parent.insert(list, Item::ArrayOfTables(tables));
        }
        Some(Item::ArrayOfTables(tables)) => tables.push(table),
        Some(Item::Value(Value::Array(array))) => {
            array.push(Value::InlineTable(table.into_inline_table()));
        }
        Some(_) => return Err(EditError::NotATable(path())),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::Keystroke;

    const LINUX: Platform = Platform::Linux;

    fn keys(text: &str, platform: Platform) -> KeySequence {
        KeySequence::parse(text, platform).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    fn id(text: &'static str) -> CommandId {
        CommandId::new(text).unwrap_or_else(|e| panic!("{e:?}"))
    }

    fn change(text: &str, change: &KeymapChange) -> (String, Keymap) {
        let registry = CommandRegistry::builtin();
        let user = UserKeymap::parse(text, None, LINUX);
        let keymap = Keymap::build(&registry, &user);
        let out = apply_change(&user, &keymap, &registry, change).unwrap_or_else(|e| panic!("{e}"));
        let after = Keymap::build(&registry, &UserKeymap::parse(&out, None, LINUX));
        (out, after)
    }

    #[test]
    fn portable_keys_use_secondary_for_the_main_modifier() {
        let ctrl = keys("ctrl-shift-k ctrl-t", LINUX);
        assert_eq!(portable_keys(&ctrl, LINUX), "secondary-shift-k secondary-t");
        let cmd = keys("cmd-alt-p", Platform::MacOs);
        assert_eq!(portable_keys(&cmd, Platform::MacOs), "secondary-alt-p");
        // Ctrl on macOS is not the main modifier.
        let mac_ctrl = keys("ctrl-tab", Platform::MacOs);
        assert_eq!(portable_keys(&mac_ctrl, Platform::MacOs), "ctrl-tab");
        let plain = keys("f6", LINUX);
        assert_eq!(portable_keys(&plain, LINUX), "f6");
        assert_eq!(
            KeySequence::parse(&portable_keys(&ctrl, LINUX), LINUX).ok(),
            Some(ctrl)
        );
    }

    #[test]
    fn set_appends_a_bind_entry_and_keeps_comments() {
        let text = "# My keys\nversion = 1\n\n# Links\n[[bind]]\nkeys = \"f7\" # mine\ncommand = \"links.copy_url\"\n";
        let (out, after) = change(
            text,
            &KeymapChange::Set {
                command: id("app.about"),
                keys: keys("ctrl-alt-a", LINUX),
                scope: EntryScope::AllPlatforms,
                replace: None,
                take_over: false,
            },
        );
        assert!(out.starts_with(text), "{out}");
        assert!(
            out.ends_with("[[bind]]\nkeys = \"secondary-alt-a\"\ncommand = \"app.about\"\n"),
            "{out}"
        );
        assert_eq!(after.key_labels("app.about"), vec!["Ctrl+Alt+A".to_owned()]);
    }

    #[test]
    fn an_empty_file_gets_a_version() {
        let (out, _) = change(
            "",
            &KeymapChange::Set {
                command: id("app.about"),
                keys: keys("f9", LINUX),
                scope: EntryScope::Platform(LINUX),
                replace: None,
                take_over: false,
            },
        );
        assert_eq!(
            out,
            "version = 1\n\n[[linux.bind]]\nkeys = \"f9\"\ncommand = \"app.about\"\n"
        );
    }

    #[test]
    fn replacing_a_builtin_binding_unbinds_it_on_this_platform() {
        let (out, after) = change(
            "version = 1\n",
            &KeymapChange::Set {
                command: id("file.save"),
                keys: keys("ctrl-alt-s", LINUX),
                scope: EntryScope::AllPlatforms,
                replace: Some(keys("ctrl-s", LINUX)),
                take_over: false,
            },
        );
        assert!(out.contains("[[linux.unbind]]\nkeys = \"ctrl-s\"\ncommand = \"file.save\"\n"));
        assert_eq!(after.key_labels("file.save"), vec!["Ctrl+Alt+S".to_owned()]);
        assert!(after.diagnostics().is_empty(), "{:?}", after.diagnostics());
    }

    #[test]
    fn replacing_a_user_binding_deletes_its_entry() {
        let text = "version = 1\n\n[[bind]]\nkeys = \"f7\"\ncommand = \"app.about\"\n\n# keep\n[[bind]]\nkeys = \"f8\"\ncommand = \"links.copy_url\"\n";
        let (out, after) = change(
            text,
            &KeymapChange::Set {
                command: id("app.about"),
                keys: keys("f9", LINUX),
                scope: EntryScope::AllPlatforms,
                replace: Some(keys("f7", LINUX)),
                take_over: false,
            },
        );
        assert!(!out.contains("\"f7\""), "{out}");
        assert!(out.contains("# keep\n[[bind]]\nkeys = \"f8\""), "{out}");
        assert_eq!(after.key_labels("app.about"), vec!["F9".to_owned()]);
    }

    #[test]
    fn conflicts_are_found_and_taken_over() {
        let registry = CommandRegistry::builtin();
        let keymap = Keymap::defaults(&registry, LINUX);
        let save = keys("ctrl-s", LINUX);
        let found = conflicts(&keymap, &registry, "app.about", &save);
        assert_eq!(
            found.iter().map(|b| b.command.as_str()).collect::<Vec<_>>(),
            vec!["file.save"]
        );
        assert!(conflicts(&keymap, &registry, "file.save", &save).is_empty());
        // The start of a chord clashes with the chord.
        let chord_start = keys("ctrl-k", LINUX);
        let found = conflicts(&keymap, &registry, "app.about", &chord_start);
        assert!(
            found
                .iter()
                .any(|b| b.command.as_str() == "view.cycle_theme")
        );

        let (out, after) = change(
            "",
            &KeymapChange::Set {
                command: id("app.about"),
                keys: save.clone(),
                scope: EntryScope::AllPlatforms,
                replace: None,
                take_over: true,
            },
        );
        assert!(out.contains("[[linux.unbind]]\nkeys = \"ctrl-s\"\ncommand = \"file.save\""));
        assert!(after.key_labels("file.save").is_empty());
        assert_eq!(after.key_labels("app.about"), vec!["Ctrl+S".to_owned()]);
        let resolved = after.resolve(
            &[Keystroke::parse("ctrl-s", LINUX).unwrap_or_else(|e| panic!("{e}"))],
            &crate::keymap::ContextSet::new(),
        );
        assert!(matches!(
            resolved,
            crate::keymap::Resolution::Match(b) if b.command.as_str() == "app.about"
        ));
    }

    #[test]
    fn remove_and_reset() {
        let (out, after) = change(
            "",
            &KeymapChange::Remove {
                command: id("file.save"),
                keys: keys("ctrl-s", LINUX),
            },
        );
        assert!(after.key_labels("file.save").is_empty());
        let user = UserKeymap::parse(&out, None, LINUX);
        assert!(is_customized(&user, "file.save"));
        assert!(!is_customized(&user, "file.save_as"));

        let text = format!(
            "{out}\n[[bind]]\nkeys = \"f7\"\ncommand = \"file.save\"\n\n[[bind]]\nkeys = \"f8\"\ncommand = \"app.about\"\n\n[[macos.bind]]\nkeys = \"f9\"\ncommand = \"file.save\"\n"
        );
        let (out, after) = change(
            &text,
            &KeymapChange::Reset {
                command: id("file.save"),
            },
        );
        assert_eq!(after.key_labels("file.save"), vec!["Ctrl+S".to_owned()]);
        assert_eq!(after.key_labels("app.about"), vec!["F8".to_owned()]);
        // Other platforms' entries stay.
        assert!(out.contains("[[macos.bind]]\nkeys = \"f9\""), "{out}");
        assert!(!out.contains("linux"), "{out}");
    }

    #[test]
    fn inline_lists_are_edited_in_place() {
        let text = "version = 1\nbind = [{ keys = \"f7\", command = \"app.about\" }]\n";
        let (out, after) = change(
            text,
            &KeymapChange::Set {
                command: id("app.licenses"),
                keys: keys("f8", LINUX),
                scope: EntryScope::AllPlatforms,
                replace: None,
                take_over: false,
            },
        );
        assert!(out.contains("command = \"app.licenses\""), "{out}");
        assert_eq!(after.key_labels("app.licenses"), vec!["F8".to_owned()]);
        assert_eq!(after.key_labels("app.about"), vec!["F7".to_owned()]);
    }

    #[test]
    fn read_only_files_are_refused() {
        let registry = CommandRegistry::builtin();
        let user = UserKeymap::parse("version = 99\n", None, LINUX);
        let keymap = Keymap::build(&registry, &user);
        let change = KeymapChange::Reset {
            command: id("file.save"),
        };
        assert!(apply_change(&user, &keymap, &registry, &change).is_err());
    }
}
