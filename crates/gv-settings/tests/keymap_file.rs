// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `keymap.toml`: entries, platform sections, diagnostics with line and
//! column, versions and migrations.

use std::fs;

use gv_settings::keymap::{
    CommandRegistry, ContextSet, EntryAction, EntryContext, KEYMAP_VERSION, KeySequence, Keymap,
    KeymapDiagnostic, KeymapIssue, Platform, Resolution, UserKeymap,
};
use gv_settings::toml_edit::{DocumentMut, Item, Value};
use gv_settings::{LayerKind, Migration, Severity};

fn parse(text: &str) -> UserKeymap {
    UserKeymap::parse(text, None, Platform::Linux)
}

/// `(issue, line, column, key)` of every diagnostic.
fn places(diagnostics: &[KeymapDiagnostic]) -> Vec<(KeymapIssue, usize, usize, String)> {
    diagnostics
        .iter()
        .map(|d| {
            let p = d.diagnostic.position;
            (
                d.issue,
                p.map_or(0, |p| p.line),
                p.map_or(0, |p| p.column),
                d.diagnostic.key.clone().unwrap_or_default(),
            )
        })
        .collect()
}

fn place(
    issue: KeymapIssue,
    line: usize,
    column: usize,
    key: &str,
) -> (KeymapIssue, usize, usize, String) {
    (issue, line, column, key.to_owned())
}

#[test]
fn entries_and_platform_sections() {
    let text = r#"version = 1

[[bind]]
keys = "secondary-j"
command = "editor.bold"

[[macos.bind]]
keys = "secondary-j"
command = "editor.italic"

[[linux.bind]]
keys = "secondary-j"
command = "editor.highlight"
context = ""

[[linux.unbind]]
command = "app.quit"

[windows]
bind = [{ keys = "secondary-j", command = "editor.strikethrough", context = "editor && !autocomplete" }]
"#;
    let linux = UserKeymap::parse(text, None, Platform::Linux);
    assert!(linux.diagnostics().is_empty(), "{:#?}", linux.diagnostics());
    assert_eq!(linux.entries().len(), 5);
    let active: Vec<_> = linux.active_entries().collect();
    assert_eq!(active.len(), 3);
    assert_eq!(active[1].context, EntryContext::Anywhere);
    assert!(matches!(active[2].action, EntryAction::Unbind { .. }));
    // `secondary` is read per section: Cmd in the macOS one.
    let mac_entry = linux
        .entries()
        .iter()
        .find(|e| e.platform == Some(Platform::MacOs));
    assert_eq!(
        mac_entry
            .and_then(|e| e.keys.as_ref())
            .map(ToString::to_string)
            .as_deref(),
        Some("cmd-j")
    );

    let registry = CommandRegistry::builtin();
    let p = Platform::Linux;
    let keymap = Keymap::build(&registry, &linux);
    let pressed = KeySequence::parse("ctrl-j", p)
        .map(|k| k.keystrokes().to_vec())
        .unwrap_or_default();
    let resolved = |active: &[&str]| match keymap
        .resolve(&pressed, &active.iter().copied().collect::<ContextSet>())
    {
        Resolution::Match(b) => b.command.to_string(),
        other => format!("{other:?}"),
    };
    // The Linux section's binding applies everywhere, but in the editor the
    // top-level binding's context is more specific.
    assert_eq!(resolved(&["editor"]), "editor.bold");
    assert_eq!(resolved(&["tree"]), "editor.highlight");
    assert!(keymap.key_labels("app.quit").is_empty());

    let windows = UserKeymap::parse(text, None, Platform::Windows);
    let keymap = Keymap::build(&registry, &windows);
    let mut active = ContextSet::new();
    active.insert("editor");
    let pressed = KeySequence::parse("ctrl-j", Platform::Windows)
        .map(|k| k.keystrokes().to_vec())
        .unwrap_or_default();
    let command = match keymap.resolve(&pressed, &active) {
        Resolution::Match(b) => Some(b.command.to_string()),
        _ => None,
    };
    assert_eq!(command.as_deref(), Some("editor.strikethrough"));
    assert_eq!(keymap.key_labels("app.quit"), ["Ctrl+Q"]);
}

#[test]
fn invalid_entries_point_at_the_problem() {
    let text = r#"version = 1
[[bind]]
keys = "ctrl-k ctrl-hyper-t"
command = "view.cycle_theme"

[[bind]]
keys = "ctrl-j"
command = "editor.bold"
context = "editor && (reader"

[[bind]]
keys = "ctrl-j"
command = "editor.does_not_exist"

[[bind]]
keys = "ctrl-j"
command = "Editor Bold"

[[bind]]
keys = ["ctrl-j"]
command = "editor.bold"

[[bind]]
command = "editor.bold"
when = "editor"

[[unbind]]
context = "editor"

[sidebar]
x = 1

[macos]
bind = 3
shortcuts = []

[[windows.bind]]
keys = "secondary-bogus"
command = "editor.bold"
"#;
    let user = parse(text);
    assert_eq!(
        places(user.diagnostics()),
        [
            // The bad keystroke inside the string.
            place(KeymapIssue::InvalidKeys, 3, 21, "bind[0].keys"),
            // The missing `)` from the `(` to the end of the context.
            place(KeymapIssue::InvalidContext, 9, 22, "bind[1].context"),
            place(KeymapIssue::InvalidEntry, 17, 11, "bind[3].command"),
            place(KeymapIssue::InvalidEntry, 20, 8, "bind[4].keys"),
            place(KeymapIssue::UnknownKey, 25, 1, "bind[5].when"),
            place(KeymapIssue::InvalidEntry, 23, 1, "bind[5]"),
            place(KeymapIssue::InvalidEntry, 27, 1, "unbind[0]"),
            place(KeymapIssue::UnknownKey, 30, 2, "sidebar"),
            place(KeymapIssue::NotATable, 34, 8, "macos.bind"),
            place(KeymapIssue::UnknownKey, 35, 1, "macos.shortcuts"),
            place(KeymapIssue::InvalidKeys, 38, 19, "windows.bind[0].keys"),
        ]
    );
    // Only the entry with an unknown command is left, and building reports it
    // at the command's value.
    assert_eq!(user.entries().len(), 1);
    let keymap = Keymap::build(&CommandRegistry::builtin(), &user);
    let unknown: Vec<_> = keymap
        .diagnostics()
        .iter()
        .filter(|d| d.issue == KeymapIssue::UnknownCommand)
        .collect();
    assert_eq!(
        places(&unknown.iter().map(|d| (*d).clone()).collect::<Vec<_>>()),
        [place(KeymapIssue::UnknownCommand, 13, 11, "bind[2]")]
    );
    assert_eq!(unknown[0].severity(), Severity::Warning);
    assert!(keymap.bindings().iter().all(|b| b.origin.is_builtin()));
    // Messages read like the settings diagnostics.
    let first = user.diagnostics()[0].to_string();
    assert!(
        first.starts_with("<keymap>:3:21: error: bind[0].keys: `hyper` is not a modifier"),
        "{first}"
    );
}

#[test]
fn escaped_strings_report_the_whole_value() {
    let user = parse("[[bind]]\nkeys = \"ctrl-\\u0068yper-t\"\ncommand = \"editor.bold\"\n");
    assert_eq!(
        places(user.diagnostics()),
        [place(KeymapIssue::InvalidKeys, 2, 8, "bind[0].keys")]
    );
}

#[test]
fn syntax_errors_drop_the_file() {
    let user = parse("[[bind]]\nkeys = \"ctrl-j\ncommand = \"editor.bold\"\n");
    assert!(user.entries().is_empty());
    assert!(user.is_read_only());
    let d = &user.diagnostics()[0];
    assert_eq!(d.issue, KeymapIssue::Syntax);
    assert_eq!(d.diagnostic.position.map(|p| p.line), Some(2));
    assert_eq!(d.diagnostic.layer, LayerKind::Global);
}

#[test]
fn args_are_kept() {
    let user = parse(
        "[[bind]]\nkeys = \"ctrl-j\"\ncommand = \"editor.bold\"\nargs = { level = 2, names = [\"a\", \"b\"] }\n",
    );
    assert!(user.diagnostics().is_empty(), "{:#?}", user.diagnostics());
    let EntryAction::Bind {
        args: Some(args), ..
    } = &user.entries()[0].action
    else {
        panic!("expected arguments");
    };
    assert_eq!(args.get("level").and_then(toml::Value::as_integer), Some(2));
    assert_eq!(
        args.get("names")
            .and_then(toml::Value::as_array)
            .map(Vec::len),
        Some(2)
    );
}

#[test]
fn versions() {
    let newer = parse("version = 9\n[[bind]]\nkeys = \"ctrl-j\"\ncommand = \"editor.bold\"\n");
    assert!(newer.is_read_only());
    assert_eq!(newer.version(), 9);
    assert_eq!(
        places(newer.diagnostics()),
        [place(KeymapIssue::NewerVersion, 1, 11, "version")]
    );
    // Read as far as possible.
    assert_eq!(newer.entries().len(), 1);

    let invalid = parse("version = \"one\"\n");
    assert_eq!(
        places(invalid.diagnostics()),
        [place(KeymapIssue::InvalidVersion, 1, 11, "version")]
    );
    assert_eq!(invalid.version(), KEYMAP_VERSION);
}

/// A test chain: version 1 named the mode toggle `view.toggle_edit_mode`.
fn rename_command(doc: &mut DocumentMut) -> Result<(), String> {
    let Some(Item::ArrayOfTables(binds)) = doc.get_mut("bind") else {
        return Ok(());
    };
    for table in binds.iter_mut() {
        if table.get("command").and_then(Item::as_str) == Some("view.toggle_edit_mode")
            && let Some(Item::Value(v)) = table.get_mut("command")
        {
            let decor = v.decor().clone();
            *v = Value::from("view.toggle_mode");
            *v.decor_mut() = decor;
        }
    }
    Ok(())
}

static CHAIN: &[Migration] = &[Migration {
    from: 1,
    description: "rename view.toggle_edit_mode",
    apply: rename_command,
}];

#[test]
fn migrations_upgrade_the_file_with_a_backup() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let path = dir.path().join("keymap.toml");
    let old =
        "# my keys\n[[bind]]\nkeys = \"ctrl-j\" # mode\ncommand = \"view.toggle_edit_mode\"\n";
    fs::write(&path, old).unwrap_or_else(|e| panic!("{e}"));

    let user =
        UserKeymap::load_with(&path, Platform::Linux, CHAIN, 2).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(user.migrated_from(), Some(1));
    assert_eq!(user.version(), 2);
    assert_eq!(user.diagnostics()[0].issue, KeymapIssue::Migrated);
    let written = fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        written,
        "version = 2\n# my keys\n[[bind]]\nkeys = \"ctrl-j\" # mode\ncommand = \"view.toggle_mode\"\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("keymap.toml.v1.bak")).unwrap_or_default(),
        old
    );
    let keymap = Keymap::build(&CommandRegistry::builtin(), &user);
    assert_eq!(keymap.key_labels("view.toggle_mode"), ["Ctrl+J", "Ctrl+E"]);

    // Loading again changes nothing.
    let again =
        UserKeymap::load_with(&path, Platform::Linux, CHAIN, 2).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(again.migrated_from(), None);
    assert!(again.diagnostics().is_empty(), "{:#?}", again.diagnostics());
}

#[test]
fn missing_and_unreadable_files() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let missing = UserKeymap::load(dir.path().join("keymap.toml"), Platform::MacOs)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(missing.entries().is_empty());
    assert!(missing.diagnostics().is_empty());
    assert_eq!(missing.platform(), Platform::MacOs);

    let path = dir.path().join("bad.toml");
    fs::write(&path, [0xff, 0xfe, b'x']).unwrap_or_else(|e| panic!("{e}"));
    let bad = UserKeymap::load(&path, Platform::Linux).unwrap_or_else(|e| panic!("{e}"));
    assert!(bad.is_read_only());
    assert_eq!(bad.diagnostics()[0].issue, KeymapIssue::Syntax);
    assert_eq!(
        bad.diagnostics()[0].diagnostic.file.as_deref(),
        Some(path.as_path())
    );
}

#[test]
fn diagnostics_carry_the_file() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let path = dir.path().join("keymap.toml");
    fs::write(
        &path,
        "[[bind]]\nkeys = \"ctrl-j\"\ncommand = \"no.such\"\n",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let user = UserKeymap::load(&path, Platform::Linux).unwrap_or_else(|e| panic!("{e}"));
    let keymap = Keymap::build(&CommandRegistry::builtin(), &user);
    let d = &keymap.diagnostics()[0];
    assert_eq!(d.diagnostic.file.as_deref(), Some(path.as_path()));
    let text = d.to_string();
    assert!(
        text.ends_with(
            ":3:11: warning: bind[0]: there is no command `no.such`; the entry is ignored"
        ),
        "{text}"
    );
}
