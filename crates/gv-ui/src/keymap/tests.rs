// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The keymap in GPUI: commands and actions, contexts, the keys the window
//! had before the keymap drove them, user overrides and reloads.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use gpui::{
    Action, AnyWindowHandle, AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, KeyBindingContextPredicate, KeyContext, Render,
    Styled as _, Subscription, TestAppContext, VisualTestContext, Window, WindowBounds,
    WindowOptions, div, point, px, size,
};
use gv_settings::keymap::{
    CONTEXTS, CommandRegistry, ContextExpr, KeySequence, Keymap, Platform, UserKeymap,
};

use super::actions::{self, action_name, command_ids};
use super::context::{KEY_CONTEXTS, gpui_context, predicate};
use super::{BindingSpec, action_for, binding_specs, shortcut};
use crate::chrome::WindowChrome;
use crate::shell::{OpenTarget, ViewMode, Workspace};
use crate::{AppSettings, Appearance, ThemeChoice};

// --- Commands and actions -----------------------------------------------------

#[test]
fn every_command_has_an_action_named_after_it() {
    let registry = CommandRegistry::builtin();
    for command in registry.iter() {
        let id = command.id().as_str();
        let action = action_for(id).unwrap_or_else(|| panic!("{id} has no action"));
        assert_eq!(Some(action.name().to_owned()), action_name(id), "{id}");
        assert_eq!(actions::command_of(action.as_ref()), Some(id));
    }
    for id in command_ids() {
        assert!(registry.get(id).is_some(), "{id} is not a command");
    }
    assert_eq!(command_ids().count(), registry.len());
}

#[test]
fn the_window_actions_are_the_commands_actions() {
    let pairs: [(Box<dyn Action>, &str); 12] = [
        (Box::new(crate::Quit), "app.quit"),
        (Box::new(crate::CycleTheme), "view.cycle_theme"),
        (Box::new(crate::ToggleEditMode), "view.toggle_mode"),
        (Box::new(crate::ToggleSidebar), "workspace.toggle_sidebar"),
        (Box::new(crate::TogglePanel), "workspace.toggle_panel"),
        (Box::new(crate::GoBack), "navigation.back"),
        (Box::new(crate::GoForward), "navigation.forward"),
        (Box::new(crate::CloseTab), "tabs.close"),
        (Box::new(crate::ReopenClosedTab), "tabs.reopen_closed"),
        (Box::new(crate::reader::Copy), "editor.copy"),
        (Box::new(crate::reader::SelectAll), "editor.select_all"),
        (Box::new(crate::find::Find), "search.find"),
    ];
    for (action, id) in pairs {
        assert_eq!(actions::command_of(action.as_ref()), Some(id));
    }
}

// --- Contexts -------------------------------------------------------------------

#[test]
fn every_model_context_has_a_key_context() {
    for info in CONTEXTS {
        assert!(
            KEY_CONTEXTS.iter().any(|(name, _)| *name == info.name),
            "{} has no key context",
            info.name
        );
    }
    let mut keys: Vec<&str> = KEY_CONTEXTS.iter().map(|(_, key)| *key).collect();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(
        keys.len(),
        KEY_CONTEXTS.len(),
        "two contexts share a key context"
    );
    assert_eq!(gpui_context("reader"), crate::reader::KEY_CONTEXT);
    assert_eq!(gpui_context("tree"), "FileTree");
    assert_eq!(gpui_context("x_unknown"), "x_unknown");
}

fn stack(contexts: &[&str]) -> Vec<KeyContext> {
    contexts
        .iter()
        .map(|c| KeyContext::parse(c).unwrap_or_default())
        .collect()
}

/// Whether a binding with this context applies with this focus chain (on
/// some element of it, as GPUI matches bindings).
fn holds(expr: &str, contexts: &[&str]) -> bool {
    let expr = ContextExpr::parse(expr).unwrap_or_else(|e| panic!("{expr}: {e}"));
    predicate(&expr).depth_of(&stack(contexts)).is_some()
}

#[test]
fn predicates_keep_their_meaning() {
    let reader = ["Workspace", "Reader"];
    let editor = ["Workspace", "Editor"];
    let field = ["Workspace", "Reader", "FindBar", "Input"];
    assert!(holds("editor || reader", &reader));
    assert!(holds("editor || reader", &editor));
    assert!(holds("editor || reader", &field));
    assert!(!holds("editor || reader", &["Workspace"]));
    assert!(holds("!editor", &reader));
    assert!(!holds("!editor", &editor));
    assert!(holds("editor && !autocomplete", &editor));
    assert!(!holds(
        "editor && !autocomplete",
        &["Workspace", "Editor Autocomplete"]
    ));
    assert!(holds("editor && split", &["Workspace", "Editor Split"]));
    assert!(holds(
        "editor || reader || tree",
        &["Workspace", "FileTree"]
    ));
    // Bindings of a view rank by how deep it is in the focus chain.
    let depth = |expr: &str, contexts: &[&str]| {
        let expr = ContextExpr::parse(expr).ok()?;
        predicate(&expr).depth_of(&stack(contexts))
    };
    assert_eq!(depth("editor || reader", &field), Some(2));
    assert_eq!(depth("!editor", &field), Some(4));
}

#[test]
fn and_or_fold_into_binary_predicates() {
    let expr = ContextExpr::parse("editor || reader || tree").expect("parses");
    let KeyBindingContextPredicate::Or(_, right) = predicate(&expr) else {
        panic!("an or");
    };
    assert_eq!(
        *right,
        KeyBindingContextPredicate::Identifier("FileTree".into())
    );
}

// --- Bindings -------------------------------------------------------------------

fn gpui_os(platform: Platform) -> gpui::OperatingSystem {
    match platform {
        Platform::MacOs => gpui::OperatingSystem::Mac,
        Platform::Linux => gpui::OperatingSystem::Linux,
        Platform::Windows => gpui::OperatingSystem::Windows,
    }
}

/// GPUI reads every key of the model's syntax as the model does.
#[test]
fn gpui_reads_every_binding_as_the_model_does() {
    let registry = CommandRegistry::builtin();
    let extra = [
        "ctrl--",
        "-",
        "secondary-shift-.",
        "fn-f12",
        "cmd-[",
        "alt-cmd-left",
        "ctrl-shift-cmd-right",
        "pageup",
        "shift-escape",
        "ctrl-k ctrl-t",
        "é",
    ];
    for platform in Platform::ALL {
        let mut sequences: Vec<KeySequence> = Keymap::defaults(&registry, platform)
            .bindings()
            .iter()
            .map(|b| b.keys.clone())
            .collect();
        sequences.extend(
            extra
                .iter()
                .map(|k| KeySequence::parse(k, platform).expect("parses")),
        );
        for sequence in sequences {
            let text = sequence.to_string();
            let parsed: Vec<gpui::Keystroke> = text
                .split(' ')
                .map(|k| {
                    gpui::Keystroke::parse_with_os(k, gpui_os(platform))
                        .unwrap_or_else(|e| panic!("{platform}: {text}: {e}"))
                })
                .collect();
            assert_eq!(parsed.len(), sequence.len(), "{text}");
            for (ours, theirs) in sequence.keystrokes().iter().zip(&parsed) {
                let (m, g) = (ours.modifiers, theirs.modifiers);
                assert_eq!(
                    (m.control, m.alt, m.shift, m.platform, m.function),
                    (g.control, g.alt, g.shift, g.platform, g.function),
                    "{platform}: {text}"
                );
                assert_eq!(ours.key, theirs.key, "{platform}: {text}");
            }
        }
    }
}

/// The keys the window bound itself before they came from the keymap:
/// keys, the action they ran, and the key context they applied in.
fn former_bindings(
    platform: Platform,
) -> Vec<(&'static str, Box<dyn Action>, Option<&'static str>)> {
    let mac = platform == Platform::MacOs;
    let pick = |mac_keys: &'static str, keys: &'static str| if mac { mac_keys } else { keys };
    let reader = Some(crate::reader::KEY_CONTEXT);
    let mut former: Vec<(&'static str, Box<dyn Action>, Option<&'static str>)> = vec![
        (
            pick("cmd-k cmd-t", "ctrl-k ctrl-t"),
            Box::new(crate::CycleTheme),
            None,
        ),
        (
            pick("cmd-e", "ctrl-e"),
            Box::new(crate::ToggleEditMode),
            None,
        ),
        (pick("cmd-q", "ctrl-q"), Box::new(crate::Quit), None),
        (pick("cmd-w", "ctrl-w"), Box::new(crate::CloseTab), None),
        (
            pick("cmd-shift-t", "ctrl-shift-t"),
            Box::new(crate::ReopenClosedTab),
            None,
        ),
        (
            pick("cmd-c", "ctrl-c"),
            Box::new(crate::reader::Copy),
            reader,
        ),
        (
            pick("cmd-a", "ctrl-a"),
            Box::new(crate::reader::SelectAll),
            reader,
        ),
        (pick("cmd-f", "ctrl-f"), Box::new(crate::find::Find), reader),
        (pick("cmd-g", "f3"), Box::new(crate::find::FindNext), reader),
        (
            pick("cmd-shift-g", "shift-f3"),
            Box::new(crate::find::FindPrevious),
            reader,
        ),
    ];
    if mac {
        former.extend([
            ("cmd-[", Box::new(crate::GoBack) as Box<dyn Action>, None),
            ("cmd-alt-left", Box::new(crate::GoBack), None),
            ("cmd-]", Box::new(crate::GoForward), None),
            ("cmd-alt-right", Box::new(crate::GoForward), None),
        ]);
    } else {
        former.extend([
            ("alt-left", Box::new(crate::GoBack) as Box<dyn Action>, None),
            ("alt-right", Box::new(crate::GoForward), None),
        ]);
    }
    former
}

/// On every platform, the keymap binds each former key to the same action,
/// where it applied before.
#[test]
fn the_keymap_keeps_every_former_binding() {
    let registry = CommandRegistry::builtin();
    for platform in Platform::ALL {
        let specs = binding_specs(&Keymap::defaults(&registry, platform));
        for (keys, action, context) in former_bindings(platform) {
            let keys = KeySequence::parse(keys, platform).expect("parses");
            let found: Vec<&BindingSpec> = specs
                .iter()
                .filter(|s| KeySequence::parse(&s.keys, platform).ok().as_ref() == Some(&keys))
                .collect();
            assert_eq!(found.len(), 1, "{platform}: {keys}: {found:?}");
            let spec = found[0];
            let bound = action_for(spec.command.as_str()).expect("an action");
            assert!(bound.partial_eq(action.as_ref()), "{platform}: {keys}");
            // It applies wherever it applied before, and in the view's
            // children (the find field).
            let mut contexts = vec!["Workspace"];
            contexts.extend(context);
            let applies = |contexts: &[&str]| {
                spec.context
                    .as_ref()
                    .is_none_or(|c| predicate(c).depth_of(&stack(contexts)).is_some())
            };
            assert!(applies(&contexts), "{platform}: {keys}");
            contexts.extend(["FindBar", "Input"]);
            assert!(applies(&contexts), "{platform}: {keys}");
        }
    }
}

#[test]
fn specs_follow_the_models_precedence() {
    let registry = CommandRegistry::builtin();
    let text = r#"
version = 1
[[bind]]
keys = "ctrl-shift-y"
command = "view.toggle_mode"
[[bind]]
keys = "ctrl-shift-y"
command = "editor.bold"
context = "editor"
[[bind]]
keys = "ctrl-q"
command = "tabs.close"
"#;
    let user = UserKeymap::parse(text, None, Platform::Linux);
    let specs = binding_specs(&Keymap::build(&registry, &user));
    let position = |keys: &str, command: &str| {
        specs
            .iter()
            .position(|s| s.keys == keys && s.command.as_str() == command)
    };
    // User bindings come after the built-in ones, the more specific later.
    let mode = position("ctrl-shift-y", "view.toggle_mode").expect("bound");
    let bold = position("ctrl-shift-y", "editor.bold").expect("bound");
    assert!(position("ctrl-e", "view.toggle_mode").expect("bound") < mode);
    assert!(mode < bold);
    // A built-in binding the user's replaces everywhere is left out.
    assert!(position("ctrl-q", "app.quit").is_none());
    assert!(position("ctrl-q", "tabs.close").is_some());
}

// --- In a window ----------------------------------------------------------------

const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 800.0;

fn temp(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).expect("write");
    }
    dir
}

fn window_options() -> WindowOptions {
    let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(WIDTH), px(HEIGHT)));
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..WindowOptions::default()
    }
}

fn open(cx: &mut TestAppContext, file: &Path) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let target = Some(OpenTarget::new(file));
        gpui::open_window(window_options(), cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    })
}

fn drawn(window: AnyWindowHandle, cx: &mut TestAppContext) -> VisualTestContext {
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    vcx
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

/// The actions keystrokes ran, newest last; `None` for a keystroke that
/// ran none.
#[derive(Clone, Default)]
struct Ran(Rc<RefCell<Vec<Option<&'static str>>>>);

impl Ran {
    fn record(&self, cx: &mut TestAppContext) -> Subscription {
        let log = self.0.clone();
        cx.update(|cx| {
            cx.observe_keystrokes(move |event, _, _| {
                log.borrow_mut()
                    .push(event.action.as_ref().map(|a| a.name()));
            })
        })
    }

    fn last(&self) -> Option<&'static str> {
        self.0.borrow().last().copied().flatten()
    }
}

fn mode(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> ViewMode {
    workspace.read_with(vcx, |ws, _| ws.active_tab().mode())
}

fn reader_focused(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> bool {
    vcx.update(|window, cx| {
        workspace
            .read(cx)
            .active_tab()
            .reader()
            .is_some_and(|r| r.read(cx).focus_handle(cx).is_focused(window))
    })
}

/// Each key the window bound before the keymap drove it still runs the
/// same action, with the same effect.
#[gpui::test]
fn former_shortcuts_run_the_same_actions(cx: &mut TestAppContext) {
    let dir = temp(&[("a.md", "# A\n\nSome text.\n")]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let ran = Ran::default();
    let _subscription = ran.record(cx);
    let mut vcx = drawn(window, cx);
    let mac = Platform::current() == Platform::MacOs;
    let pick = |mac_keys: &'static str, keys: &'static str| if mac { mac_keys } else { keys };
    assert!(reader_focused(&workspace, &mut vcx));

    // In the Reading view: its own keys, then copy and select all.
    let mut reader_keys = vec![
        ("down", "reader::LineDown"),
        ("up", "reader::LineUp"),
        ("pagedown", "reader::PageDown"),
        ("pageup", "reader::PageUp"),
        ("space", "reader::PageDown"),
        ("shift-space", "reader::PageUp"),
        ("end", "reader::ScrollToBottom"),
        ("home", "reader::ScrollToTop"),
    ];
    reader_keys.extend(if mac {
        [
            ("cmd-down", "reader::ScrollToBottom"),
            ("cmd-up", "reader::ScrollToTop"),
        ]
    } else {
        [
            ("ctrl-end", "reader::ScrollToBottom"),
            ("ctrl-home", "reader::ScrollToTop"),
        ]
    });
    for (keys, action) in reader_keys {
        vcx.simulate_keystrokes(keys);
        assert_eq!(ran.last(), Some(action), "{keys}");
    }
    vcx.simulate_keystrokes("secondary-a");
    assert_eq!(ran.last(), Some("editor::SelectAll"));
    vcx.simulate_keystrokes("secondary-c");
    assert_eq!(ran.last(), Some("editor::Copy"));
    let copied = vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert!(copied.is_some_and(|t| t.contains("Some text.")));

    // Find, next and previous (also from the find field), and Esc.
    let find_open = |vcx: &mut VisualTestContext| vcx.debug_bounds("find-bar").is_some();
    vcx.simulate_keystrokes("secondary-f");
    assert_eq!(ran.last(), Some("search::Find"));
    draw(&mut vcx);
    assert!(find_open(&mut vcx));
    vcx.simulate_keystrokes(pick("cmd-g", "f3"));
    assert_eq!(ran.last(), Some("search::FindNext"));
    vcx.simulate_keystrokes(pick("cmd-shift-g", "shift-f3"));
    assert_eq!(ran.last(), Some("search::FindPrevious"));
    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert!(!find_open(&mut vcx));
    assert!(reader_focused(&workspace, &mut vcx));
    vcx.simulate_keystrokes(pick("cmd-g", "f3"));
    assert_eq!(ran.last(), Some("search::FindNext"));
    vcx.simulate_keystrokes("escape");
    assert_eq!(ran.last(), Some("find::CloseFind"));
    draw(&mut vcx);

    // The window's keys.
    for keys in if mac {
        vec![
            ("cmd-[", "navigation::Back"),
            ("cmd-alt-left", "navigation::Back"),
        ]
    } else {
        vec![("alt-left", "navigation::Back")]
    } {
        vcx.simulate_keystrokes(keys.0);
        assert_eq!(ran.last(), Some(keys.1), "{}", keys.0);
    }
    for keys in if mac {
        vec![
            ("cmd-]", "navigation::Forward"),
            ("cmd-alt-right", "navigation::Forward"),
        ]
    } else {
        vec![("alt-right", "navigation::Forward")]
    } {
        vcx.simulate_keystrokes(keys.0);
        assert_eq!(ran.last(), Some(keys.1), "{}", keys.0);
    }

    let before = vcx.update(|_, cx| Appearance::choice(cx));
    vcx.simulate_keystrokes("secondary-k secondary-t");
    assert_eq!(ran.last(), Some("view::CycleTheme"));
    assert_ne!(vcx.update(|_, cx| Appearance::choice(cx)), before);

    vcx.simulate_keystrokes("secondary-e");
    assert_eq!(ran.last(), Some("view::ToggleMode"));
    assert_eq!(mode(&workspace, &mut vcx), ViewMode::Edit);
    draw(&mut vcx);
    vcx.simulate_keystrokes("secondary-e");
    assert_eq!(mode(&workspace, &mut vcx), ViewMode::Preview);
    draw(&mut vcx);

    let tabs = |vcx: &mut VisualTestContext| {
        workspace.read_with(vcx, |ws, _| ws.active_tab().path().is_some())
    };
    vcx.simulate_keystrokes("secondary-w");
    assert_eq!(ran.last(), Some("tabs::Close"));
    draw(&mut vcx);
    assert!(!tabs(&mut vcx), "the file's tab is closed");
    vcx.simulate_keystrokes("secondary-shift-t");
    assert_eq!(ran.last(), Some("tabs::ReopenClosed"));
    draw(&mut vcx);
    assert!(tabs(&mut vcx), "the file's tab is back");

    vcx.simulate_keystrokes("secondary-q");
    assert_eq!(ran.last(), Some("app::Quit"));
}

/// A view standing in for the editor (or any other view) in key context
/// tests: it takes the keyboard and counts inserted links.
struct Probe {
    focus: FocusHandle,
    context: &'static str,
    links: Rc<Cell<usize>>,
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
        let links = self.links.clone();
        div()
            .key_context(self.context)
            .track_focus(&self.focus)
            .size_full()
            .on_action(move |_: &actions::editor::InsertLink, _, _| links.set(links.get() + 1))
    }
}

fn open_probe(
    cx: &mut TestAppContext,
    context: &'static str,
    links: &Rc<Cell<usize>>,
) -> AnyWindowHandle {
    let links = links.clone();
    cx.update(|cx| {
        gpui::open_window(window_options(), cx, |window, cx| {
            cx.new(|cx| {
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                Probe {
                    focus,
                    context,
                    links,
                }
            })
        })
        .expect("test window opens")
        .0
    })
}

/// Mod+K inserts a link at once in the editor; elsewhere it starts the
/// theme chord.
#[gpui::test]
fn mod_k_depends_on_the_context(cx: &mut TestAppContext) {
    cx.update(|cx| crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light))));
    let links = Rc::new(Cell::new(0));
    let theme = |cx: &mut TestAppContext| cx.update(|cx| Appearance::choice(cx));

    let editor = open_probe(cx, "Editor", &links);
    let mut vcx = VisualTestContext::from_window(editor, cx);
    draw(&mut vcx);
    vcx.simulate_keystrokes("secondary-k");
    assert_eq!(links.get(), 1);
    assert!(!vcx.update(|window, _| window.has_pending_keystrokes()));
    vcx.simulate_keystrokes("secondary-k secondary-t");
    assert_eq!(links.get(), 2);
    assert_eq!(theme(cx), ThemeChoice::Light);

    let reader = open_probe(cx, crate::reader::KEY_CONTEXT, &links);
    let mut vcx = VisualTestContext::from_window(reader, cx);
    draw(&mut vcx);
    vcx.simulate_keystrokes("secondary-k");
    assert!(vcx.update(|window, _| window.has_pending_keystrokes()));
    vcx.simulate_keystrokes("secondary-t");
    assert_eq!(links.get(), 2);
    assert_eq!(theme(cx), ThemeChoice::SemiLight);
}

/// Rebinding moves a command to other keys and its menu label with it;
/// unbinding frees the keys; an empty keymap brings the defaults back.
#[gpui::test]
fn a_user_keymap_rebinds_and_unbinds(cx: &mut TestAppContext) {
    let dir = temp(&[("a.md", "# A\n")]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let platform = Platform::current();
    let label = |keys: &str| {
        KeySequence::parse(keys, platform)
            .expect("parses")
            .label(platform)
    };
    let text = r#"
version = 1

[[unbind]]
command = "view.toggle_mode"

[[bind]]
keys = "secondary-shift-m"
command = "view.toggle_mode"

[[unbind]]
keys = "secondary-w"
"#;
    vcx.update(|_, cx| super::apply(cx, &UserKeymap::parse(text, None, platform)));
    draw(&mut vcx);

    vcx.simulate_keystrokes("secondary-e");
    assert_eq!(mode(&workspace, &mut vcx), ViewMode::Preview);
    vcx.simulate_keystrokes("secondary-shift-m");
    assert_eq!(mode(&workspace, &mut vcx), ViewMode::Edit);
    draw(&mut vcx);
    vcx.simulate_keystrokes("secondary-shift-m");
    assert_eq!(mode(&workspace, &mut vcx), ViewMode::Preview);
    draw(&mut vcx);

    vcx.simulate_keystrokes("secondary-w");
    let has_file = |vcx: &mut VisualTestContext| {
        workspace.read_with(vcx, |ws, _| ws.active_tab().path().is_some())
    };
    assert!(has_file(&mut vcx), "Mod+W is unbound");

    vcx.update(|_, cx| {
        assert_eq!(
            shortcut(cx, "view.toggle_mode"),
            Some(label("secondary-shift-m"))
        );
        assert_eq!(shortcut(cx, "tabs.close"), None);
    });

    // Back to the built-in keys.
    vcx.update(|_, cx| super::apply(cx, &UserKeymap::empty(platform)));
    draw(&mut vcx);
    vcx.simulate_keystrokes("secondary-e");
    assert_eq!(mode(&workspace, &mut vcx), ViewMode::Edit);
    vcx.update(|_, cx| {
        assert_eq!(shortcut(cx, "view.toggle_mode"), Some(label("secondary-e")));
        assert_eq!(shortcut(cx, "tabs.close"), Some(label("secondary-w")));
    });
}

/// Loading `keymap.toml` again picks up every change; a missing or broken
/// file leaves the built-in keys.
#[gpui::test]
fn keymap_file_changes_apply(cx: &mut TestAppContext) {
    let dir = temp(&[("a.md", "# A\n")]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let path = dir.path().join("keymap.toml");
    let toggle_mode_keys = |vcx: &mut VisualTestContext| {
        vcx.update(|_, cx| {
            let bindings = cx.global::<super::Keybindings>();
            bindings
                .keymap()
                .bindings_for("view.toggle_mode")
                .iter()
                .map(|b| b.keys.to_string())
                .collect::<Vec<_>>()
        })
    };
    let ctrl_e = KeySequence::parse("secondary-e", Platform::current())
        .expect("parses")
        .to_string();

    // No file: the defaults.
    vcx.update(|_, cx| super::load_file(cx, &path));
    assert_eq!(toggle_mode_keys(&mut vcx), std::slice::from_ref(&ctrl_e));

    std::fs::write(
        &path,
        "version = 1\n[[bind]]\nkeys = \"f9\"\ncommand = \"view.toggle_mode\"\n",
    )
    .expect("write");
    vcx.update(|_, cx| super::load_file(cx, &path));
    assert_eq!(
        toggle_mode_keys(&mut vcx),
        ["f9".to_owned(), ctrl_e.clone()]
    );
    vcx.simulate_keystrokes("f9");
    assert_eq!(mode(&workspace, &mut vcx), ViewMode::Edit);
    draw(&mut vcx);

    // A syntax error: reported, and the built-in keys stay.
    std::fs::write(&path, "version = 1\n[[bind]\n").expect("write");
    vcx.update(|_, cx| super::load_file(cx, &path));
    assert_eq!(toggle_mode_keys(&mut vcx), std::slice::from_ref(&ctrl_e));
    let errors = vcx.update(|_, cx| {
        cx.global::<super::Keybindings>()
            .keymap()
            .diagnostics()
            .len()
    });
    assert!(errors > 0);

    std::fs::remove_file(&path).expect("removed");
    vcx.update(|_, cx| super::load_file(cx, &path));
    assert_eq!(toggle_mode_keys(&mut vcx), [ctrl_e]);
}

#[test]
fn safe_mode_reads_without_writing() {
    let dir = temp(&[]);
    let path = dir.path().join("keymap.toml");
    let platform = Platform::Linux;
    let read = |read_only| super::read(&path, platform, read_only);
    assert!(read(true).is_ok_and(|k| k.entries().is_empty()));
    std::fs::write(
        &path,
        "version = 1\n[[bind]]\nkeys = \"f9\"\ncommand = \"app.quit\"\n",
    )
    .expect("write");
    assert!(read(true).is_ok_and(|k| k.entries().len() == 1));
    assert!(read(false).is_ok_and(|k| k.entries().len() == 1));
    std::fs::write(&path, [0xff, 0xfe]).expect("write");
    assert!(read(true).is_ok_and(|k| k.entries().is_empty()));
}

/// The app-level commands have handlers wherever the focus is.
#[gpui::test]
fn app_commands_are_always_available(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        for id in super::APP_COMMANDS {
            let action = action_for(id).expect("an action");
            assert!(cx.is_action_available(action.as_ref()), "{id}");
        }
        let unhandled = action_for("tabs.new").expect("an action");
        assert!(!cx.is_action_available(unhandled.as_ref()));
    });
}
