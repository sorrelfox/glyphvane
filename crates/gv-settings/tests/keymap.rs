// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The keymap model: keystroke syntax and labels, contexts, the built-in
//! commands, precedence and resolution.

use gv_settings::keymap::{
    Binding, Category, CommandRegistry, ContextExpr, ContextSet, KeySequence, Keymap, KeymapIssue,
    Keystroke, Modifiers, NAMED_KEYS, Platform, Resolution, UserKeymap, builtin_commands,
};
use proptest::prelude::*;

fn registry() -> CommandRegistry {
    CommandRegistry::builtin()
}

fn keys(text: &str, platform: Platform) -> Vec<Keystroke> {
    KeySequence::parse(text, platform)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
        .keystrokes()
        .to_vec()
}

fn ctx(names: &[&str]) -> ContextSet {
    names.iter().copied().collect()
}

fn user(text: &str, platform: Platform) -> Keymap {
    Keymap::build(&registry(), &UserKeymap::parse(text, None, platform))
}

fn command_of(resolution: Resolution<'_>) -> Option<&str> {
    match resolution {
        Resolution::Match(b) => Some(b.command.as_str()),
        _ => None,
    }
}

// --- Keystrokes -----------------------------------------------------------

fn key_strategy() -> impl Strategy<Value = String> {
    let named: Vec<String> = NAMED_KEYS.iter().map(|k| (*k).to_owned()).collect();
    prop_oneof![
        prop::sample::select(named),
        (1u32..=35).prop_map(|n| format!("f{n}")),
        prop::sample::select(
            "abcdefghijklmnopqrstuvwxyz0123456789,.;/[]=-`'\\+!?é"
                .chars()
                .collect::<Vec<_>>()
        )
        .prop_map(|c| c.to_string()),
    ]
}

fn keystroke_strategy() -> impl Strategy<Value = Keystroke> {
    (any::<[bool; 5]>(), key_strategy()).prop_map(|(m, key)| Keystroke {
        modifiers: Modifiers {
            control: m[0],
            alt: m[1],
            shift: m[2],
            platform: m[3],
            function: m[4],
        },
        key,
    })
}

fn platform_strategy() -> impl Strategy<Value = Platform> {
    prop::sample::select(Platform::ALL.to_vec())
}

/// A keystroke written in any accepted spelling: modifier aliases, any case,
/// any order, upper-case letters for Shift.
fn spelled(k: &Keystroke, platform: Platform, choice: &[u8]) -> String {
    let even = |i: usize| choice[i % choice.len()].is_multiple_of(2);
    let pick = |i: usize, options: &[&str]| -> String {
        let o = options[choice[i % choice.len()] as usize % options.len()];
        if even(i + 3) {
            o.to_ascii_uppercase()
        } else {
            o.to_owned()
        }
    };
    let m = k.modifiers;
    let secondary = Modifiers::secondary(platform);
    let mut parts = Vec::new();
    let use_secondary =
        even(0) && ((secondary.platform && m.platform) || (secondary.control && m.control));
    if use_secondary {
        parts.push(pick(1, &["secondary", "mod"]));
    }
    if m.control && !(use_secondary && secondary.control) {
        parts.push(pick(2, &["ctrl", "control"]));
    }
    if m.alt {
        parts.push(pick(3, &["alt", "option"]));
    }
    if m.platform && !(use_secondary && secondary.platform) {
        parts.push(pick(4, &["cmd", "command", "super", "win"]));
    }
    let letter_shift = m.shift && k.key.len() == 1 && k.key.as_bytes()[0].is_ascii_lowercase();
    let shift_as_case = letter_shift && even(5);
    if m.shift && !shift_as_case {
        parts.push(pick(6, &["shift"]));
    }
    if m.function {
        parts.push(pick(7, &["fn"]));
    }
    // Any modifier order is accepted.
    if !parts.is_empty() {
        let rotate = choice[8 % choice.len()] as usize % parts.len();
        parts.rotate_left(rotate);
    }
    let named_upper = k.key.chars().count() > 1 && even(9);
    let key = if shift_as_case || named_upper {
        k.key.to_ascii_uppercase()
    } else {
        k.key.clone()
    };
    parts.push(key);
    parts.join("-")
}

proptest! {
    #[test]
    fn keystroke_text_round_trips(k in keystroke_strategy(), platform in platform_strategy()) {
        let text = k.to_string();
        prop_assert_eq!(Keystroke::parse(&text, platform), Ok(k));
    }

    #[test]
    fn sequences_round_trip(
        ks in prop::collection::vec(keystroke_strategy(), 1..=KeySequence::MAX_LEN),
        platform in platform_strategy(),
    ) {
        let Some(seq) = KeySequence::new(ks) else { return Err(TestCaseError::fail("empty")) };
        let text = seq.to_string();
        prop_assert_eq!(KeySequence::parse(&text, platform), Ok(seq.clone()));
        // Extra spaces between keystrokes are accepted.
        let spaced = text.replace(' ', "   ");
        prop_assert_eq!(KeySequence::parse(&format!(" {spaced} "), platform), Ok(seq));
    }

    #[test]
    fn every_spelling_parses_to_the_same_keystroke(
        k in keystroke_strategy(),
        platform in platform_strategy(),
        choice in prop::collection::vec(any::<u8>(), 10),
    ) {
        let text = spelled(&k, platform, &choice);
        prop_assert_eq!(Keystroke::parse(&text, platform), Ok(k), "{}", text);
    }

    #[test]
    fn garbage_never_panics(text in "\\PC{0,24}", platform in platform_strategy()) {
        if let Ok(seq) = KeySequence::parse(&text, platform) {
            prop_assert_eq!(KeySequence::parse(&seq.to_string(), platform), Ok(seq));
        }
        let _ = ContextExpr::parse(&text);
    }

    #[test]
    fn contexts_print_canonically(expr in context_strategy()) {
        let text = expr.to_string();
        let parsed = ContextExpr::parse(&text);
        prop_assert!(parsed.is_ok(), "{}", text);
        let parsed = parsed.unwrap_or_else(|_| expr.clone());
        // Printing the parsed form is a fixed point.
        let canonical = parsed.to_string();
        prop_assert_eq!(ContextExpr::parse(&canonical), Ok(parsed.clone()));
        // Parsing keeps the meaning, for every combination of the names.
        let names = expr.names();
        for bits in 0u32..(1 << names.len()) {
            let active: ContextSet = names
                .iter()
                .enumerate()
                .filter(|(i, _)| bits & (1 << i) != 0)
                .map(|(_, n)| *n)
                .collect();
            prop_assert_eq!(expr.matches(&active), parsed.matches(&active), "{}", text);
        }
    }
}

fn context_strategy() -> impl Strategy<Value = ContextExpr> {
    let leaf = prop::sample::select(vec![
        "editor",
        "reader",
        "tree",
        "autocomplete",
        "split",
        "x_1",
    ])
    .prop_map(|n| ContextExpr::Name(n.to_owned()));
    leaf.prop_recursive(4, 24, 3, |inner| {
        prop_oneof![
            inner.clone().prop_map(|e| ContextExpr::Not(Box::new(e))),
            prop::collection::vec(inner.clone(), 2..4).prop_map(ContextExpr::And),
            prop::collection::vec(inner, 2..4).prop_map(ContextExpr::Or),
        ]
    })
}

#[test]
fn gpui_syntax_examples() {
    let p = Platform::MacOs;
    assert_eq!(keys("cmd-k cmd-t", p), keys("secondary-k secondary-t", p));
    assert_eq!(
        keys("ctrl-k ctrl-t", Platform::Linux),
        keys("secondary-k secondary-t", Platform::Linux)
    );
    assert_eq!(
        keys("super-x", Platform::Linux),
        keys("cmd-x", Platform::Linux)
    );
    assert_eq!(
        keys("win-x", Platform::Windows),
        keys("cmd-x", Platform::Windows)
    );
    assert_eq!(keys("fn-f1", p)[0].to_string(), "fn-f1");
    assert_eq!(
        keys("shift-cmd-ctrl-alt-fn-up", p)[0].to_string(),
        "fn-ctrl-alt-cmd-shift-up"
    );
    assert_eq!(keys("ctrl-shift-.", p)[0].to_string(), "ctrl-shift-.");
}

// --- Labels -----------------------------------------------------------------

#[test]
fn label_snapshots() {
    let cases: &[(&str, [&str; 3])] = &[
        // keys,                  macOS,      Linux,               Windows
        ("secondary-shift-p", ["⇧⌘P", "Ctrl+Shift+P", "Ctrl+Shift+P"]),
        (
            "secondary-k secondary-t",
            ["⌘K ⌘T", "Ctrl+K Ctrl+T", "Ctrl+K Ctrl+T"],
        ),
        (
            "ctrl-alt-shift-cmd-a",
            ["⌃⌥⇧⌘A", "Super+Ctrl+Alt+Shift+A", "Win+Ctrl+Alt+Shift+A"],
        ),
        ("alt-left", ["⌥←", "Alt+←", "Alt+←"]),
        ("secondary-,", ["⌘,", "Ctrl+,", "Ctrl+,"]),
        ("secondary--", ["⌘-", "Ctrl+-", "Ctrl+-"]),
        ("secondary-=", ["⌘=", "Ctrl+=", "Ctrl+="]),
        ("ctrl-+", ["⌃+", "Ctrl+Plus", "Ctrl+Plus"]),
        ("shift-f10", ["⇧F10", "Shift+F10", "Shift+F10"]),
        ("alt-enter", ["⌥↩", "Alt+Enter", "Alt+Enter"]),
        ("escape", ["⎋", "Esc", "Esc"]),
        ("shift-tab", ["⇧⇥", "Shift+Tab", "Shift+Tab"]),
        ("backspace", ["⌫", "Backspace", "Backspace"]),
        ("delete", ["⌦", "Delete", "Delete"]),
        ("space", ["Space", "Space", "Space"]),
        ("pageup", ["⇞", "PgUp", "PgUp"]),
        ("end", ["↘", "End", "End"]),
        ("menu", ["Menu", "Menu", "Menu"]),
        ("fn-f1", ["fn F1", "Fn+F1", "Fn+F1"]),
        ("secondary-shift-.", ["⇧⌘.", "Ctrl+Shift+.", "Ctrl+Shift+."]),
        ("cmd-[", ["⌘[", "Super+[", "Win+["]),
    ];
    for (text, expected) in cases {
        for (platform, want) in Platform::ALL.into_iter().zip(expected) {
            let seq = KeySequence::parse(text, platform).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(seq.label(platform), *want, "{text} on {platform}");
        }
    }
    let seq = KeySequence::parse("cmd-k cmd-t", Platform::MacOs).ok();
    assert_eq!(
        seq.map(|s| s.keycaps(Platform::MacOs)),
        Some(vec!["⌘K".to_owned(), "⌘T".to_owned()])
    );
}

/// The cheatsheet rows of a few commands, per platform.
#[test]
fn cheatsheet_snapshot() {
    let registry = registry();
    let wanted = [
        "workspace.command_palette",
        "view.cycle_theme",
        "navigation.back",
        "search.replace",
        "editor.expand_selection",
        "editor.table",
        "app.quit",
    ];
    let rows = |platform: Platform| -> Vec<String> {
        Keymap::defaults(&registry, platform)
            .cheatsheet(&registry)
            .into_iter()
            .filter(|e| wanted.contains(&e.command.as_str()))
            .map(|e| format!("{} | {} | {}", e.category, e.title, e.keys.join(", ")))
            .collect()
    };
    assert_eq!(
        rows(Platform::MacOs),
        [
            "Application | Quit | ⌘Q",
            "Workspace | Command palette | ⌘P, ⇧⌘P",
            "View | Cycle theme | ⌘K ⌘T",
            "Navigation | Go back | ⌘[, ⌥⌘←",
            "Search | Find and replace | ⌥⌘F",
            "Editing | Expand selection | ⌃⇧⌘→",
            "Formatting | Insert or format table | ⌥⌘T",
        ]
    );
    assert_eq!(
        rows(Platform::Linux),
        [
            "Application | Quit | Ctrl+Q",
            "Workspace | Command palette | Ctrl+P, Ctrl+Shift+P",
            "View | Cycle theme | Ctrl+K Ctrl+T",
            "Navigation | Go back | Alt+←",
            "Search | Find and replace | Ctrl+H",
            "Editing | Expand selection | Alt+Shift+→",
        ]
    );
    assert_eq!(
        rows(Platform::Windows),
        [
            "Application | Quit | Ctrl+Q",
            "Workspace | Command palette | Ctrl+P, Ctrl+Shift+P",
            "View | Cycle theme | Ctrl+K Ctrl+T",
            "Navigation | Go back | Alt+←",
            "Search | Find and replace | Ctrl+H",
            "Editing | Expand selection | Alt+Shift+→",
            "Formatting | Insert or format table | Ctrl+Alt+T",
        ]
    );
}

#[test]
fn cheatsheet_is_grouped_by_category() {
    let registry = registry();
    let sheet = Keymap::defaults(&registry, Platform::Linux).cheatsheet(&registry);
    assert!(!sheet.is_empty());
    assert!(sheet.windows(2).all(|w| w[0].category <= w[1].category));
    assert!(sheet.iter().all(|e| !e.keys.is_empty()));
    // Commands without keys stay out of the cheatsheet.
    assert!(
        !sheet
            .iter()
            .any(|e| e.command.as_str() == "workspace.toggle_sidebar")
    );
    assert_eq!(
        sheet.first().map(|e| e.category),
        Some(Category::Application)
    );
}

// --- Built-in commands -------------------------------------------------------

#[test]
fn builtin_commands_are_valid_and_unique() {
    let all = builtin_commands();
    for c in &all {
        assert!(c.is_ok(), "{c:?}");
    }
    assert_eq!(registry().len(), all.len());
    for id in [
        "app.quit",
        "view.toggle_mode",
        "view.cycle_theme",
        "workspace.toggle_sidebar",
        "workspace.toggle_panel",
        "workspace.command_palette",
        "editor.insert_link",
    ] {
        assert!(registry().get(id).is_some(), "{id}");
    }
}

/// The built-in keymap has no conflicts, prefix clashes, keys that type in
/// the editor, or unreachable bindings, on any platform.
#[test]
fn defaults_are_clean() {
    let registry = registry();
    for platform in Platform::ALL {
        let keymap = Keymap::defaults(&registry, platform);
        let problems: Vec<String> = keymap
            .diagnostics()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(problems.is_empty(), "{platform}: {problems:#?}");
        for b in keymap.bindings() {
            assert!(b.is_reachable(), "{platform}: {} {}", b.keys, b.command);
            let first = &b.keys.keystrokes()[0];
            assert!(
                !first.is_printable(platform),
                "{platform}: {} {}",
                b.keys,
                b.command
            );
        }
    }
}

/// The bindings the UI has today keep their keys.
#[test]
fn current_ui_bindings() {
    let registry = registry();
    for (platform, quit, mode, theme) in [
        (Platform::MacOs, "cmd-q", "cmd-e", "cmd-k cmd-t"),
        (Platform::Linux, "ctrl-q", "ctrl-e", "ctrl-k ctrl-t"),
        (Platform::Windows, "ctrl-q", "ctrl-e", "ctrl-k ctrl-t"),
    ] {
        let keymap = Keymap::defaults(&registry, platform);
        let none = ContextSet::new();
        assert_eq!(
            command_of(keymap.resolve(&keys(quit, platform), &none)),
            Some("app.quit")
        );
        assert_eq!(
            command_of(keymap.resolve(&keys(mode, platform), &none)),
            Some("view.toggle_mode")
        );
        let theme = keys(theme, platform);
        assert_eq!(
            keymap.resolve(&theme[..1], &none),
            Resolution::PendingChord { fallback: None }
        );
        assert_eq!(
            command_of(keymap.resolve(&theme, &none)),
            Some("view.cycle_theme")
        );
    }
}

// --- Resolution -------------------------------------------------------------

#[test]
fn mod_k_inserts_a_link_in_the_editor_and_starts_the_theme_chord_elsewhere() {
    let p = Platform::Linux;
    let keymap = Keymap::defaults(&registry(), p);
    let editor = ctx(&["editor"]);
    assert_eq!(
        command_of(keymap.resolve(&keys("ctrl-k", p), &editor)),
        Some("editor.insert_link")
    );
    assert_eq!(
        keymap.resolve(&keys("ctrl-k ctrl-t", p), &editor),
        Resolution::NoMatch
    );
    let reader = ctx(&["reader"]);
    assert_eq!(
        keymap.resolve(&keys("ctrl-k", p), &reader),
        Resolution::PendingChord { fallback: None }
    );
    assert_eq!(
        keymap.resolve(&keys("ctrl-k ctrl-x", p), &reader),
        Resolution::NoMatch
    );
    assert_eq!(keymap.resolve(&[], &reader), Resolution::NoMatch);
}

#[test]
fn contexts_gate_bindings() {
    let p = Platform::Linux;
    let keymap = Keymap::defaults(&registry(), p);
    assert_eq!(
        command_of(keymap.resolve(&keys("ctrl-b", p), &ctx(&["editor"]))),
        Some("editor.bold")
    );
    assert_eq!(
        keymap.resolve(&keys("ctrl-b", p), &ctx(&["reader"])),
        Resolution::NoMatch
    );
    assert_eq!(
        command_of(keymap.resolve(&keys("alt-enter", p), &ctx(&["reader"]))),
        Some("links.follow")
    );
}

#[test]
fn user_bindings_win_over_defaults() {
    let p = Platform::Linux;
    let keymap = user(
        r#"
[[bind]]
keys = "ctrl-b"
command = "editor.italic"
"#,
        p,
    );
    assert_eq!(
        command_of(keymap.resolve(&keys("ctrl-b", p), &ctx(&["editor"]))),
        Some("editor.italic")
    );
    // Bold lost its only key, so it leaves the cheatsheet.
    assert!(keymap.key_labels("editor.bold").is_empty());
    assert_eq!(keymap.key_labels("editor.italic"), ["Ctrl+B", "Ctrl+I"]);
    let overrides: Vec<_> = keymap
        .diagnostics()
        .iter()
        .filter(|d| d.issue == KeymapIssue::Overrides)
        .collect();
    assert_eq!(overrides.len(), 1, "{overrides:#?}");
    assert!(
        overrides[0]
            .related
            .as_ref()
            .is_some_and(|o| o.is_builtin())
    );
}

/// A user binding beats a more specific built-in one: layer first.
#[test]
fn layer_comes_before_specificity() {
    let p = Platform::Linux;
    let keymap = user(
        r#"
[[bind]]
keys = "ctrl-k"
command = "workspace.quick_switcher"
context = ""
"#,
        p,
    );
    assert_eq!(
        command_of(keymap.resolve(&keys("ctrl-k", p), &ctx(&["editor"]))),
        Some("workspace.quick_switcher")
    );
    // The theme chord is shadowed everywhere now.
    assert!(keymap.key_labels("view.cycle_theme").is_empty());
    assert!(keymap.key_labels("editor.insert_link").is_empty());
    let prefix = keymap
        .diagnostics()
        .iter()
        .find(|d| d.issue == KeymapIssue::ChordPrefix);
    assert!(prefix.is_some(), "{:#?}", keymap.diagnostics());
}

#[test]
fn specific_context_wins_then_later_entry() {
    let p = Platform::Linux;
    let keymap = user(
        r#"
[[bind]]
keys = "ctrl-j"
command = "editor.bold"
context = "editor"

[[bind]]
keys = "ctrl-j"
command = "editor.italic"
context = "editor && !autocomplete"

[[bind]]
keys = "ctrl-j"
command = "editor.highlight"
context = "editor"
"#,
        p,
    );
    let pressed = keys("ctrl-j", p);
    assert_eq!(
        command_of(keymap.resolve(&pressed, &ctx(&["editor"]))),
        Some("editor.italic")
    );
    assert_eq!(
        command_of(keymap.resolve(&pressed, &ctx(&["editor", "autocomplete"]))),
        Some("editor.highlight")
    );
    // Same layer, same specificity, overlapping: a conflict naming both.
    let conflicts: Vec<_> = keymap
        .diagnostics()
        .iter()
        .filter(|d| d.issue == KeymapIssue::Conflict)
        .collect();
    assert_eq!(conflicts.len(), 1, "{conflicts:#?}");
    let c = conflicts[0];
    assert_eq!(c.diagnostic.key.as_deref(), Some("bind[2]"));
    assert_eq!(
        c.related.as_ref().and_then(|o| o.key.as_deref()),
        Some("bind[0]")
    );
    assert_eq!(c.diagnostic.position.map(|p| p.line), Some(12));
    assert_eq!(
        c.related.as_ref().and_then(|o| o.position).map(|p| p.line),
        Some(2)
    );
    assert!(
        c.diagnostic.message.contains("Bold"),
        "{}",
        c.diagnostic.message
    );
    // The first binding is shadowed everywhere, so it is unreachable.
    assert!(
        keymap
            .key_labels("editor.bold")
            .iter()
            .all(|l| l != "Ctrl+J")
    );
}

#[test]
fn chords_wait_for_the_next_key_with_a_fallback() {
    let p = Platform::Linux;
    let keymap = user(
        r#"
[[bind]]
keys = "ctrl-m"
command = "editor.bold"

[[bind]]
keys = "ctrl-m ctrl-m"
command = "editor.italic"
"#,
        p,
    );
    let editor = ctx(&["editor"]);
    let first = keys("ctrl-m", p);
    let Resolution::PendingChord {
        fallback: Some(fallback),
    } = keymap.resolve(&first, &editor)
    else {
        panic!("expected a pending chord");
    };
    assert_eq!(fallback.command.as_str(), "editor.bold");
    assert_eq!(
        command_of(keymap.resolve(&keys("ctrl-m ctrl-m", p), &editor)),
        Some("editor.italic")
    );
    let info = keymap
        .diagnostics()
        .iter()
        .find(|d| d.issue == KeymapIssue::ChordPrefix);
    assert!(info.is_some_and(|d| d.related.is_some()));
}

#[test]
fn shorter_binding_that_outranks_a_chord_is_a_warning() {
    let p = Platform::Linux;
    let keymap = user(
        r#"
[[bind]]
keys = "ctrl-m ctrl-m"
command = "editor.italic"

[[bind]]
keys = "ctrl-m"
command = "editor.bold"
context = "editor && !autocomplete"
"#,
        p,
    );
    let editor = ctx(&["editor"]);
    assert_eq!(
        command_of(keymap.resolve(&keys("ctrl-m", p), &editor)),
        Some("editor.bold")
    );
    let d = keymap
        .diagnostics()
        .iter()
        .find(|d| d.issue == KeymapIssue::ChordPrefix);
    assert_eq!(
        d.map(|d| d.severity()),
        Some(gv_settings::Severity::Warning),
        "{:#?}",
        keymap.diagnostics()
    );
    // Still reachable while autocomplete is open.
    assert_eq!(
        keymap.key_labels("editor.italic"),
        ["Ctrl+M Ctrl+M", "Ctrl+I"]
    );
}

#[test]
fn unbind_removes_builtin_bindings() {
    let p = Platform::Linux;
    let keymap = user(
        r#"
[[unbind]]
keys = "ctrl-shift-p"

[[unbind]]
command = "app.quit"

[[unbind]]
keys = "f6"
command = "editor.bold"
"#,
        p,
    );
    assert_eq!(keymap.key_labels("workspace.command_palette"), ["Ctrl+P"]);
    assert!(keymap.key_labels("app.quit").is_empty());
    assert_eq!(
        keymap.resolve(&keys("ctrl-q", p), &ContextSet::new()),
        Resolution::NoMatch
    );
    // The third entry matches nothing.
    let nothing: Vec<_> = keymap
        .diagnostics()
        .iter()
        .filter(|d| d.issue == KeymapIssue::NothingToUnbind)
        .collect();
    assert_eq!(nothing.len(), 1);
    assert_eq!(nothing[0].diagnostic.key.as_deref(), Some("unbind[2]"));
}

#[test]
fn unbind_by_context() {
    let p = Platform::Linux;
    let keymap = user(
        r#"
[[unbind]]
keys = "alt-enter"
context = "reader"

[[unbind]]
keys = "alt-enter"
context = "editor || reader"
"#,
        p,
    );
    assert!(keymap.key_labels("links.follow").is_empty());
    let nothing = keymap
        .diagnostics()
        .iter()
        .filter(|d| d.issue == KeymapIssue::NothingToUnbind)
        .count();
    assert_eq!(nothing, 1);
}

#[test]
fn bindings_carry_args_and_inherit_the_command_context() {
    let p = Platform::MacOs;
    let keymap = user(
        r#"
[[bind]]
keys = "secondary-shift-1"
command = "editor.heading_1"
args = { level = 1, toggle = true }
"#,
        p,
    );
    let pressed = keys("cmd-shift-1", p);
    let b: Option<&Binding> = match keymap.resolve(&pressed, &ctx(&["editor"])) {
        Resolution::Match(b) => Some(b),
        _ => None,
    };
    let args = b.and_then(|b| b.args.as_ref()).map(ToString::to_string);
    assert_eq!(args.as_deref(), Some("{ level = 1, toggle = true }"));
    // The command's context (editor) applies.
    assert_eq!(
        keymap.resolve(&pressed, &ctx(&["reader"])),
        Resolution::NoMatch
    );
    assert_eq!(keymap.key_labels("editor.heading_1"), ["⇧⌘1", "⌥⌘1"]);
}

#[test]
fn printable_keys_in_the_editor_are_warned() {
    let p = Platform::Linux;
    let keymap = user(
        r#"
[[bind]]
keys = "j"
command = "navigation.back"

[[bind]]
keys = "j"
command = "navigation.forward"
context = "reader"

[[bind]]
keys = "shift-space"
command = "view.toggle_mode"
context = "editor || tree"
"#,
        p,
    );
    let warned: Vec<_> = keymap
        .diagnostics()
        .iter()
        .filter(|d| d.issue == KeymapIssue::PrintableKeyInEditor)
        .filter_map(|d| d.diagnostic.key.clone())
        .collect();
    assert_eq!(warned, ["bind[0]", "bind[2]"]);
    // Alt types characters on macOS only.
    let mac = user(
        "[[bind]]\nkeys = \"alt-j\"\ncommand = \"navigation.back\"\n",
        Platform::MacOs,
    );
    assert!(
        mac.diagnostics()
            .iter()
            .any(|d| d.issue == KeymapIssue::PrintableKeyInEditor)
    );
    let linux = user(
        "[[bind]]\nkeys = \"alt-j\"\ncommand = \"navigation.back\"\n",
        p,
    );
    assert!(
        !linux
            .diagnostics()
            .iter()
            .any(|d| d.issue == KeymapIssue::PrintableKeyInEditor)
    );
}

#[test]
fn contexts_that_never_apply_are_warned() {
    let keymap = user(
        r#"
[[bind]]
keys = "ctrl-j"
command = "editor.bold"
context = "reader"

[[bind]]
keys = "ctrl-j"
command = "view.zoom_in"
context = "editor && tree"

[[bind]]
keys = "ctrl-shift-j"
command = "view.zoom_in"
context = "editor && sidebar"
"#,
        Platform::Linux,
    );
    let issues: Vec<_> = keymap
        .diagnostics()
        .iter()
        .map(|d| (d.issue, d.diagnostic.key.clone().unwrap_or_default()))
        .collect();
    assert!(
        issues.contains(&(KeymapIssue::NeverActive, "bind[0]".into())),
        "{issues:#?}"
    );
    assert!(
        issues.contains(&(KeymapIssue::NeverActive, "bind[1]".into())),
        "{issues:#?}"
    );
    assert!(
        issues.contains(&(KeymapIssue::UnknownContext, "bind[2]".into())),
        "{issues:#?}"
    );
}

#[test]
fn duplicates_are_info() {
    let keymap = user(
        "[[bind]]\nkeys = \"ctrl-b\"\ncommand = \"editor.bold\"\n",
        Platform::Linux,
    );
    let d: Vec<_> = keymap
        .diagnostics()
        .iter()
        .map(|d| (d.issue, d.severity()))
        .collect();
    assert_eq!(d, [(KeymapIssue::Duplicate, gv_settings::Severity::Info)]);
    assert_eq!(keymap.key_labels("editor.bold"), ["Ctrl+B"]);
}

#[test]
fn palette_availability() {
    let registry = registry();
    let bold = registry.get("editor.bold");
    assert_eq!(bold.map(|c| c.is_available(&ctx(&["editor"]))), Some(true));
    assert_eq!(bold.map(|c| c.is_available(&ctx(&["reader"]))), Some(false));
    let quit = registry.get("app.quit");
    assert_eq!(quit.map(|c| c.is_available(&ContextSet::new())), Some(true));
}
