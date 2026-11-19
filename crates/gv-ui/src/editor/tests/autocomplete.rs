// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Autocomplete in the Source view, in headless windows: every trigger
//! with a vault's content index, accepting with the keys and the mouse as
//! one undo step, the vault's link style, the profile's triggers, input
//! methods, Esc and the popover's place.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{ScrollDelta, ScrollEvent};
use gv_profile::ProfileId;
use gv_vault::content::{ContentIndexer, ContentOptions};
use gv_vault::{IndexConfig, PathIndex};

use super::*;

/// A vault on disk with its content index, registered as the windows'
/// indexes are.
pub(super) struct Vault {
    _dir: tempfile::TempDir,
    pub(super) root: PathBuf,
    index: PathIndex,
    _indexer: ContentIndexer,
}

pub(super) fn vault(files: &[(&str, &str)]) -> Vault {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::path::absolute(dir.path().join("vault")).expect("absolute");
    for (path, text) in files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, text).expect("write");
    }
    let index = PathIndex::build(&root, IndexConfig::default())
        .expect("index")
        .0;
    let mut options = ContentOptions::for_profile(ProfileId::Obsidian);
    options.overlay_debounce = Duration::ZERO;
    let indexer = ContentIndexer::start(index.clone(), options, |_| {}).expect("starts");
    assert!(indexer.flush(Duration::from_secs(10)));
    crate::files::overlay::register(&root, indexer.handle());
    Vault {
        _dir: dir,
        root,
        index,
        _indexer: indexer,
    }
}

/// A Source view of `file` in `vault`, in `profile`, with the caret at the
/// end of the text.
pub(super) fn editor_in(
    cx: &mut TestAppContext,
    vault: &Vault,
    file: &str,
    profile: ProfileId,
) -> (Entity<EditorView>, VisualTestContext) {
    let path = vault.root.join(file);
    let index = vault.index.clone();
    let (window, view) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        gpui::open_window(options(), cx, |_, cx| {
            cx.new(|cx| {
                let mut v = EditorView::new(ParseOptions::default(), cx);
                v.set_compiled_profile(&gv_profile::compile_builtin(profile), cx);
                v.open(path, Some(index), cx);
                v
            })
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    vcx.update(|window, cx| {
        let focus = view.read(cx).focus.clone();
        focus.focus(window, cx);
    });
    for _ in 0..100 {
        draw(&mut vcx);
        if view.read_with(&vcx, |v, _| *v.status() == EditorStatus::Ready) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        view.read_with(&vcx, |v, _| v.status().clone()),
        EditorStatus::Ready
    );
    vcx.simulate_keystrokes("secondary-end");
    draw(&mut vcx);
    (view, vcx)
}

/// A Source view of `text` (no vault) in `profile`, caret at the end.
fn editor_for(
    cx: &mut TestAppContext,
    text: &str,
    profile: ProfileId,
) -> (Entity<EditorView>, VisualTestContext) {
    let (view, mut vcx) = editor_with(cx, text, ParseOptions::builtin(profile));
    view.update(&mut vcx, |v, cx| {
        v.set_compiled_profile(&gv_profile::compile_builtin(profile), cx);
    });
    vcx.simulate_keystrokes("secondary-end");
    draw(&mut vcx);
    (view, vcx)
}

/// Types `text` one character at a time, as keys arrive.
fn type_text(vcx: &mut VisualTestContext, text: &str) {
    for c in text.chars() {
        vcx.simulate_input(&c.to_string());
    }
    draw(vcx);
}

fn rows(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> Option<(Vec<String>, usize)> {
    view.read_with(vcx, |v, _| v.autocomplete_rows())
}

fn labels(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> Vec<String> {
    rows(view, vcx).map(|(l, _)| l).unwrap_or_default()
}

fn caret(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> usize {
    selections(view, vcx)[0].1
}

const PLAN: &str = "---\naliases: [Roadmap]\nstatus: done\n---\n# Intro\n\nPara one ^abc123\n\n## Details\n\n#project/alpha\n";

fn notes() -> Vault {
    vault(&[
        ("Home.md", "Start "),
        ("Projects/Plan.md", PLAN),
        ("img/pic.png", "png"),
    ])
}

#[gpui::test]
fn wikilinks_list_notes_and_accept_as_one_undo_step(cx: &mut TestAppContext) {
    let v = notes();
    let (view, mut vcx) = editor_in(cx, &v, "Home.md", ProfileId::Obsidian);
    type_text(&mut vcx, "[[");
    // Smart pairs closed the link; every file is listed.
    assert_eq!(text_of(&view, &mut vcx), "Start [[]]");
    let all = labels(&view, &mut vcx);
    assert!(all.contains(&"Plan".to_owned()), "{all:?}");
    assert!(all.contains(&"pic.png".to_owned()), "{all:?}");
    type_text(&mut vcx, "Pla");
    assert_eq!(labels(&view, &mut vcx)[0], "Plan");
    vcx.simulate_keystrokes("enter");
    assert_eq!(text_of(&view, &mut vcx), "Start [[Plan]]");
    assert_eq!(caret(&view, &mut vcx), 14);
    assert!(rows(&view, &mut vcx).is_none(), "accepting closes it");
    // One undo step takes the completion back, leaving what was typed.
    vcx.simulate_keystrokes("secondary-z");
    assert_eq!(text_of(&view, &mut vcx), "Start [[Pla]]");
}

#[gpui::test]
fn aliases_link_to_their_note_and_embeds_list_attachments(cx: &mut TestAppContext) {
    let v = notes();
    let (view, mut vcx) = editor_in(cx, &v, "Home.md", ProfileId::Obsidian);
    type_text(&mut vcx, "[[Roadm");
    assert_eq!(labels(&view, &mut vcx)[0], "Roadmap");
    vcx.simulate_keystrokes("tab");
    assert_eq!(text_of(&view, &mut vcx), "Start [[Plan|Roadmap]]");
    type_text(&mut vcx, " ![[pi");
    assert_eq!(labels(&view, &mut vcx)[0], "pic.png");
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        text_of(&view, &mut vcx),
        "Start [[Plan|Roadmap]] ![[pic.png]]"
    );
}

#[gpui::test]
fn headings_of_a_note_this_note_and_the_vault(cx: &mut TestAppContext) {
    let v = vault(&[("Home.md", "# Top\n\n"), ("Projects/Plan.md", PLAN)]);
    let (view, mut vcx) = editor_in(cx, &v, "Home.md", ProfileId::Obsidian);
    type_text(&mut vcx, "[[Plan#De");
    assert_eq!(labels(&view, &mut vcx), ["Details"]);
    vcx.simulate_keystrokes("enter");
    assert_eq!(text_of(&view, &mut vcx), "# Top\n\n[[Plan#Details]]");
    type_text(&mut vcx, " [[#");
    assert_eq!(labels(&view, &mut vcx), ["Top"]);
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        text_of(&view, &mut vcx),
        "# Top\n\n[[Plan#Details]] [[#Top]]"
    );
    type_text(&mut vcx, " [[##Intr");
    assert_eq!(labels(&view, &mut vcx)[0], "Intro");
    vcx.simulate_keystrokes("enter");
    assert!(
        text_of(&view, &mut vcx).ends_with(" [[Plan#Intro]]"),
        "{}",
        text_of(&view, &mut vcx)
    );
}

#[gpui::test]
fn blocks_get_an_id_in_the_same_undo_step(cx: &mut TestAppContext) {
    let v = vault(&[
        ("Home.md", "Some paragraph here.\n\n"),
        ("Projects/Plan.md", PLAN),
    ]);
    let (view, mut vcx) = editor_in(cx, &v, "Home.md", ProfileId::Obsidian);
    type_text(&mut vcx, "[[Plan#^");
    assert_eq!(labels(&view, &mut vcx), ["^abc123"]);
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        text_of(&view, &mut vcx),
        "Some paragraph here.\n\n[[Plan#^abc123]]"
    );
    type_text(&mut vcx, " [[^som");
    assert_eq!(labels(&view, &mut vcx)[0], "Some paragraph here.");
    vcx.simulate_keystrokes("enter");
    let text = text_of(&view, &mut vcx);
    let id = text
        .split(" ^")
        .nth(1)
        .and_then(|rest| rest.get(..6))
        .expect("an id was added")
        .to_owned();
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "{text}");
    assert_eq!(
        text,
        format!("Some paragraph here. ^{id}\n\n[[Plan#^abc123]] [[#^{id}]]")
    );
    // The link and the new id go together.
    vcx.simulate_keystrokes("secondary-z");
    assert_eq!(
        text_of(&view, &mut vcx),
        "Some paragraph here.\n\n[[Plan#^abc123]] [[^som]]"
    );
}

#[gpui::test]
fn markdown_paths_then_anchors(cx: &mut TestAppContext) {
    let v = notes();
    let (view, mut vcx) = editor_in(cx, &v, "Home.md", ProfileId::Obsidian);
    type_text(&mut vcx, "[](Pla");
    assert_eq!(text_of(&view, &mut vcx), "Start [](Pla)");
    assert_eq!(labels(&view, &mut vcx)[0], "Plan");
    vcx.simulate_keystrokes("enter");
    // The empty text gets the title; the caret stays before `)`.
    assert_eq!(text_of(&view, &mut vcx), "Start [Plan](Plan.md)");
    assert_eq!(caret(&view, &mut vcx), 20);
    type_text(&mut vcx, "#Int");
    assert_eq!(labels(&view, &mut vcx), ["Intro"]);
    vcx.simulate_keystrokes("enter");
    assert_eq!(text_of(&view, &mut vcx), "Start [Plan](Plan.md#Intro)");
}

#[gpui::test]
fn the_vaults_link_style_writes_markdown_links(cx: &mut TestAppContext) {
    let v = vault(&[
        (
            ".obsidian/app.json",
            r#"{"useMarkdownLinks": true, "newLinkFormat": "relative"}"#,
        ),
        ("Home.md", "Start "),
        ("Projects/My Plan.md", "# A\n"),
    ]);
    let (view, mut vcx) = editor_in(cx, &v, "Home.md", ProfileId::Obsidian);
    type_text(&mut vcx, "[[My");
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        text_of(&view, &mut vcx),
        "Start [My Plan](Projects/My%20Plan.md)"
    );
}

#[gpui::test]
fn tags_and_front_matter_properties(cx: &mut TestAppContext) {
    let v = vault(&[
        ("Home.md", "---\nstatus: draft\n---\nText "),
        ("Projects/Plan.md", PLAN),
    ]);
    let (view, mut vcx) = editor_in(cx, &v, "Home.md", ProfileId::Obsidian);
    type_text(&mut vcx, "#proj");
    assert_eq!(labels(&view, &mut vcx), ["project/alpha"]);
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        text_of(&view, &mut vcx),
        "---\nstatus: draft\n---\nText #project/alpha"
    );
    // A new key after `status: draft`.
    vcx.simulate_keystrokes("up up end enter");
    type_text(&mut vcx, "ali");
    assert_eq!(labels(&view, &mut vcx)[0], "aliases");
    vcx.simulate_keystrokes("escape");
    vcx.simulate_keystrokes("backspace backspace backspace");
    type_text(&mut vcx, "st");
    assert_eq!(labels(&view, &mut vcx)[0], "status");
    vcx.simulate_keystrokes("enter");
    type_text(&mut vcx, "d");
    let values = labels(&view, &mut vcx);
    assert!(
        values.contains(&"done".to_owned()) && values.contains(&"draft".to_owned()),
        "{values:?}"
    );
    type_text(&mut vcx, "o");
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        text_of(&view, &mut vcx),
        "---\nstatus: draft\nstatus: done\n---\nText #project/alpha"
    );
}

#[gpui::test]
fn callout_types(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_for(cx, "", ProfileId::Obsidian);
    type_text(&mut vcx, "> [!ti");
    assert_eq!(text_of(&view, &mut vcx), "> [!ti]");
    assert_eq!(labels(&view, &mut vcx)[0], "tip");
    vcx.simulate_keystrokes("enter");
    assert_eq!(text_of(&view, &mut vcx), "> [!tip] ");
}

#[gpui::test]
fn fence_languages_and_nothing_in_code(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_for(cx, "", ProfileId::Obsidian);
    type_text(&mut vcx, "```rus");
    assert_eq!(text_of(&view, &mut vcx), "```rus");
    assert!(labels(&view, &mut vcx).contains(&"rust".to_owned()));
    vcx.simulate_keystrokes("escape");
    type_text(&mut vcx, "t");
    assert!(
        rows(&view, &mut vcx).is_none(),
        "Esc closed it for this fence"
    );
    vcx.simulate_keystrokes("end enter");
    // Inside the code block nothing completes.
    type_text(&mut vcx, "[[x #tag");
    assert!(rows(&view, &mut vcx).is_none());
}

#[gpui::test]
fn tilde_fences_take_the_highlighted_language(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_for(cx, "", ProfileId::Obsidian);
    type_text(&mut vcx, "~~~p");
    vcx.simulate_keystrokes("down");
    let (all, selected) = rows(&view, &mut vcx).expect("open");
    assert_eq!(selected, 1);
    vcx.simulate_keystrokes("enter");
    assert_eq!(text_of(&view, &mut vcx), format!("~~~{}", all[selected]));
}

#[gpui::test]
fn emoji_and_footnotes_in_github_profiles(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_for(cx, "Note[^1].\n\n[^1]: One.\n\nHi ", ProfileId::GfmFile);
    type_text(&mut vcx, ":smile");
    assert_eq!(labels(&view, &mut vcx)[0], "smile");
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        text_of(&view, &mut vcx),
        "Note[^1].\n\n[^1]: One.\n\nHi :smile:"
    );
    type_text(&mut vcx, " x[^");
    assert_eq!(labels(&view, &mut vcx), ["1", "New footnote [^2]"]);
    vcx.simulate_keystrokes("down enter");
    assert_eq!(
        text_of(&view, &mut vcx),
        "Note[^1].\n\n[^1]: One.\n\nHi :smile: x[^2]\n\n[^2]: "
    );
    let len = text_of(&view, &mut vcx).len();
    assert_eq!(
        caret(&view, &mut vcx),
        len,
        "the caret is in the definition"
    );
    // GitHub has no wikilinks.
    type_text(&mut vcx, "[[No");
    assert!(rows(&view, &mut vcx).is_none());
}

#[gpui::test]
fn commonmark_has_only_link_paths(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_for(cx, "", ProfileId::CommonMark);
    type_text(&mut vcx, "[[Fo #ta :sm [^");
    assert!(rows(&view, &mut vcx).is_none());
}

#[gpui::test]
fn input_method_text_opens_nothing(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_for(cx, "", ProfileId::Obsidian);
    view.update_in(&mut vcx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "[[", None, window, cx);
    });
    draw(&mut vcx);
    assert!(rows(&view, &mut vcx).is_none(), "not while composing");
    view.update_in(&mut vcx, |v, window, cx| {
        v.replace_text_in_range(None, "[[", window, cx);
    });
    draw(&mut vcx);
    assert_eq!(text_of(&view, &mut vcx), "[[");
    assert!(rows(&view, &mut vcx).is_none(), "nor when it commits");
}

#[gpui::test]
fn keys_move_wrap_and_dismiss(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_for(cx, "", ProfileId::Obsidian);
    type_text(&mut vcx, "> [!");
    let (all, selected) = rows(&view, &mut vcx).expect("open");
    assert_eq!(selected, 0);
    vcx.simulate_keystrokes("up");
    assert_eq!(rows(&view, &mut vcx).expect("open").1, all.len() - 1);
    vcx.simulate_keystrokes("down down");
    assert_eq!(rows(&view, &mut vcx).expect("open").1, 1);
    // The text did not move: the keys went to the popover.
    assert_eq!(text_of(&view, &mut vcx), "> [!]");
    vcx.simulate_keystrokes("escape");
    assert!(rows(&view, &mut vcx).is_none());
    type_text(&mut vcx, "n");
    assert!(
        rows(&view, &mut vcx).is_none(),
        "dismissed for this trigger"
    );
    // Enter is a new line again.
    vcx.simulate_keystrokes("end enter");
    assert_eq!(text_of(&view, &mut vcx), "> [!n]\n> ");
}

#[gpui::test]
fn a_click_accepts_and_the_popover_sits_under_the_text(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_for(cx, "Text\n", ProfileId::Obsidian);
    type_text(&mut vcx, "> [!");
    let start = text_of(&view, &mut vcx).find("[!").expect("typed") + 2;
    let anchor = view.update_in(&mut vcx, |v, window, cx| {
        v.range_bounds(start..start, None, window, cx)
            .expect("on screen")
    });
    let placed = view
        .read_with(&vcx, |v, _| v.autocomplete_placed)
        .expect("placed");
    assert_eq!(
        placed.origin.x,
        anchor.left() - px(crate::autocomplete::LABEL_INSET)
    );
    assert!(placed.origin.y > anchor.bottom() - px(0.5));
    let all = labels(&view, &mut vcx);
    let target = all[2].clone();
    let row = vcx
        .debug_bounds(Box::leak(
            format!("autocomplete-row:{target}").into_boxed_str(),
        ))
        .expect("the row is drawn");
    press(&mut vcx, row.center(), 1, Modifiers::none());
    assert_eq!(text_of(&view, &mut vcx), format!("Text\n> [!{target}] "));
}

/// The wheel over the popover scrolls its rows, not the text; the
/// highlight stays on its row.
#[gpui::test]
fn the_wheel_scrolls_the_popover(cx: &mut TestAppContext) {
    let text = format!("\n{}", "line\n".repeat(200));
    let (view, mut vcx) = editor_for(cx, &text, ProfileId::Obsidian);
    vcx.simulate_keystrokes("secondary-home");
    draw(&mut vcx);
    type_text(&mut vcx, "> [!");
    let all = labels(&view, &mut vcx);
    assert!(all.len() > crate::autocomplete::VISIBLE_ROWS + 3, "{all:?}");
    let row = |vcx: &mut VisualTestContext, label: &str| {
        vcx.debug_bounds(Box::leak(
            format!("autocomplete-row:{label}").into_boxed_str(),
        ))
    };
    let first = row(&mut vcx, &all[0]).expect("the first row is drawn");
    assert!(row(&mut vcx, &all[crate::autocomplete::VISIBLE_ROWS]).is_none());
    let top = view.read_with(&vcx, |v, _| v.scroll_y);
    vcx.simulate_event(ScrollEvent {
        position: first.center(),
        delta: ScrollDelta::Lines(point(0.0, -3.0)),
        ..ScrollEvent::default()
    });
    draw(&mut vcx);
    assert!(
        row(&mut vcx, &all[0]).is_none(),
        "scrolled past the first row"
    );
    assert!(row(&mut vcx, &all[crate::autocomplete::VISIBLE_ROWS + 2]).is_some());
    assert_eq!(
        view.read_with(&vcx, |v, _| v.scroll_y),
        top,
        "the text stayed"
    );
    assert_eq!(rows(&view, &mut vcx).expect("open").1, 0);
    // Far past the end: the last rows fill the popover.
    vcx.simulate_event(ScrollEvent {
        position: first.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-10_000.0))),
        ..ScrollEvent::default()
    });
    draw(&mut vcx);
    assert!(row(&mut vcx, all.last().expect("rows")).is_some());
    assert!(
        row(
            &mut vcx,
            &all[all.len() - crate::autocomplete::VISIBLE_ROWS]
        )
        .is_some()
    );
    // The keys bring the highlight back into view.
    vcx.simulate_keystrokes("down");
    assert!(row(&mut vcx, &all[1]).is_some());
}

#[gpui::test]
fn the_popover_flips_above_near_the_bottom(cx: &mut TestAppContext) {
    let text = "line\n".repeat(200);
    let (view, mut vcx) = editor_for(cx, &text, ProfileId::Obsidian);
    // The caret's row is the last one on screen.
    type_text(&mut vcx, "> [!");
    let start = text_of(&view, &mut vcx).rfind("[!").expect("typed") + 2;
    let anchor = view.update_in(&mut vcx, |v, window, cx| {
        v.range_bounds(start..start, None, window, cx)
            .expect("on screen")
    });
    let placed = view
        .read_with(&vcx, |v, _| v.autocomplete_placed)
        .expect("placed");
    assert!(anchor.bottom() > px(HEIGHT - 200.0), "{anchor:?}");
    assert!(placed.bottom() < anchor.top(), "{placed:?} {anchor:?}");
}
