// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Daily notes and templates: the formats and template variables, and in
//! headless GPUI windows over a temporary vault, today's note made from
//! its template, previous and next daily notes, and a template inserted
//! as one undo step.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Bounds, Entity, TestAppContext, VisualTestContext, WindowBounds,
    WindowOptions, point, px, size,
};
use gv_bases::Date;

use super::daily_notes::{
    DailyConfig, TemplatesConfig, daily_config, daily_path, existing_daily_notes, expand,
    neighbour, templates_config, today_path,
};
use super::{OpenTarget, ViewMode, Workspace};
use crate::chrome::WindowChrome;
use crate::editor::EditorStatus;
use crate::keymap::actions::{editor as editor_commands, notes as commands};
use crate::{AppSettings, ThemeChoice};

fn day(y: i64, m: u32, d: u32) -> Date {
    Date::from_civil(y, m, d, 0, 0, 0, 0)
}

// --- Formats and variables -------------------------------------------------------

#[test]
fn variables_are_filled_in_and_the_rest_is_kept() {
    let at = Date::from_civil(2026, 3, 7, 9, 5, 0, 0);
    let config = TemplatesConfig::default();
    let text = "# {{title}}\n{{date}} {{time}} {{ date:dddd, MMMM Do }} {{time:h:mm A}}\n\
                {{unknown}} <% tp.date.now() %> {{date:}} {{tit";
    assert_eq!(
        expand(text, "Note", at, at, &config),
        "# Note\n2026-03-07 09:05 Saturday, March 7th 9:05 AM\n\
         {{unknown}} <% tp.date.now() %> {{date:}} {{tit"
    );
    let custom = TemplatesConfig {
        folder: None,
        date_format: "DD.MM.YYYY".into(),
        time_format: "HH[h]mm".into(),
    };
    assert_eq!(
        expand("{{DATE}} {{Time}}", "", at, at, &custom),
        "07.03.2026 09h05"
    );
}

#[test]
fn daily_paths_follow_folder_and_format() {
    let config = DailyConfig {
        folder: "Journal".into(),
        format: "YYYY/MM/YYYY-MM-DD ddd".into(),
        template: String::new(),
    };
    assert_eq!(
        daily_path(&config, day(2026, 1, 2)),
        "Journal/2026/01/2026-01-02 Fri.md"
    );
    assert_eq!(
        daily_path(&DailyConfig::default(), day(2026, 1, 2)),
        "2026-01-02.md"
    );
}

#[test]
fn obsidian_settings_are_read_with_defaults() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    assert_eq!(daily_config(root), DailyConfig::default());
    assert_eq!(templates_config(root).folder, None);
    std::fs::create_dir_all(root.join("Templates")).expect("folder");
    assert_eq!(templates_config(root).folder.as_deref(), Some("Templates"));
    std::fs::create_dir_all(root.join(".obsidian")).expect("marker");
    std::fs::write(
        root.join(".obsidian/daily-notes.json"),
        r#"{"folder": "/Daily/", "format": "", "template": "T/Day"}"#,
    )
    .expect("write");
    std::fs::write(
        root.join(".obsidian/templates.json"),
        r#"{"folder": "T", "dateFormat": "D/M/YY"}"#,
    )
    .expect("write");
    let daily = daily_config(root);
    assert_eq!(daily.folder, "Daily");
    assert_eq!(daily.format, "YYYY-MM-DD", "an empty format is the default");
    assert_eq!(daily.template, "T/Day");
    let templates = templates_config(root);
    assert_eq!(templates.folder.as_deref(), Some("T"));
    assert_eq!(templates.date_format, "D/M/YY");
    assert_eq!(templates.time_format, "HH:mm");
}

#[test]
fn previous_and_next_skip_missing_days() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("Daily/sub")).expect("folders");
    for name in [
        "2026-01-01.md",
        "2026-01-05.md",
        "2026-02-01.md",
        "notes.md",
    ] {
        std::fs::write(root.join("Daily").join(name), "").expect("write");
    }
    let config = DailyConfig {
        folder: "Daily".into(),
        ..DailyConfig::default()
    };
    let today = day(2026, 1, 20);
    let notes = existing_daily_notes(root, &config, today);
    let paths: Vec<&str> = notes.iter().map(|(_, p)| p.as_str()).collect();
    assert_eq!(
        paths,
        [
            "Daily/2026-01-01.md",
            "Daily/2026-01-05.md",
            "Daily/2026-02-01.md"
        ]
    );
    let at = Some("Daily/2026-01-05.md");
    assert_eq!(
        neighbour(&notes, at, today, false).as_deref(),
        Some("Daily/2026-01-01.md")
    );
    assert_eq!(
        neighbour(&notes, at, today, true).as_deref(),
        Some("Daily/2026-02-01.md")
    );
    // Not a daily note: from today.
    assert_eq!(
        neighbour(&notes, Some("Other.md"), today, false).as_deref(),
        Some("Daily/2026-01-05.md")
    );
    assert_eq!(
        neighbour(&notes, Some("Daily/2026-01-01.md"), today, false),
        None
    );
}

// --- In a window -----------------------------------------------------------------

struct Fixture {
    dir: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

fn fixture(cx: &mut TestAppContext, open: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    std::fs::create_dir_all(root.join(".obsidian")).expect("marker");
    std::fs::create_dir_all(root.join("Daily")).expect("folder");
    std::fs::create_dir_all(root.join("Templates")).expect("folder");
    std::fs::write(
        root.join(".obsidian/daily-notes.json"),
        r#"{"folder": "Daily", "format": "YYYY-MM-DD", "template": "Templates/Day"}"#,
    )
    .expect("write");
    std::fs::write(
        root.join(".obsidian/templates.json"),
        r#"{"folder": "Templates"}"#,
    )
    .expect("write");
    std::fs::write(
        root.join("Templates/Day.md"),
        "# {{title}}\n\nDay {{date:YYYY}}\n",
    )
    .expect("write");
    std::fs::write(root.join("Templates/Meeting.md"), "## Meeting {{title}}\n").expect("write");
    for name in ["2020-01-01.md", "2020-01-03.md"] {
        std::fs::write(root.join("Daily").join(name), "").expect("write");
    }
    std::fs::write(root.join("Note.md"), "first\n").expect("write");
    let target = Some(OpenTarget::new(root.join(open)));
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1280.0), px(800.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    Fixture {
        dir,
        workspace,
        vcx,
    }
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..2 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

impl Fixture {
    fn root(&self) -> PathBuf {
        std::path::absolute(self.dir.path()).expect("absolute")
    }

    fn active_file(&self) -> Option<PathBuf> {
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.active_tab().path().map(Path::to_path_buf)
        })
    }

    fn until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            draw(&mut self.vcx);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn editor_text(&self) -> Option<String> {
        self.workspace.read_with(&self.vcx, |ws, cx| {
            let editor = ws.active_tab().editor()?.read(cx);
            (*editor.status() == EditorStatus::Ready)
                .then(|| editor.editor().map(|e| e.text().to_string()))
                .flatten()
        })
    }
}

#[gpui::test]
fn todays_note_is_made_from_its_template_and_opened(cx: &mut TestAppContext) {
    let mut f = fixture(cx, "Note.md");
    let today = today_path(&f.root());
    f.vcx.dispatch_action(commands::DailyNote);
    f.until("today's note", |f| f.active_file().as_ref() == Some(&today));
    let name = today
        .file_stem()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    let year = &name[..4];
    assert_eq!(
        std::fs::read_to_string(&today).expect("read"),
        format!("# {name}\n\nDay {year}\n")
    );
    let mode = f.workspace.read_with(&f.vcx, |ws, _| ws.mode());
    assert_eq!(mode, ViewMode::Edit);

    // Again: the same note, not replaced.
    std::fs::write(&today, "kept\n").expect("write");
    let note = f.root().join("Note.md");
    f.workspace.update_in(&mut f.vcx, |ws, window, cx| {
        ws.open_tab(OpenTarget::new(note), window, cx);
    });
    f.vcx.dispatch_action(commands::DailyNote);
    f.until("today's note again", |f| {
        f.active_file().as_ref() == Some(&today)
    });
    assert_eq!(std::fs::read_to_string(&today).expect("read"), "kept\n");
}

#[gpui::test]
fn previous_and_next_daily_notes_open(cx: &mut TestAppContext) {
    let mut f = fixture(cx, "Daily/2020-01-03.md");
    let root = f.root();
    f.vcx.dispatch_action(commands::PreviousDailyNote);
    f.until("the previous note", |f| {
        f.active_file() == Some(root.join("Daily/2020-01-01.md"))
    });
    f.vcx.dispatch_action(commands::NextDailyNote);
    f.until("the next note", |f| {
        f.active_file() == Some(root.join("Daily/2020-01-03.md"))
    });
    // None before the first: an infobar.
    f.vcx.dispatch_action(commands::PreviousDailyNote);
    f.vcx.dispatch_action(commands::PreviousDailyNote);
    f.until("the infobar", |f| {
        f.workspace.read_with(&f.vcx, |ws, _| {
            ws.active_tab()
                .notice()
                .is_some_and(|n| n.title.contains("No daily note before"))
        })
    });
}

#[gpui::test]
fn a_template_is_inserted_as_one_undo_step(cx: &mut TestAppContext) {
    let mut f = fixture(cx, "Note.md");
    f.vcx.simulate_keystrokes("secondary-e");
    f.until("the Source view", |f| f.editor_text().is_some());
    let template = f.root().join("Templates/Meeting.md");
    let config = templates_config(&f.root());
    f.workspace.update_in(&mut f.vcx, |ws, window, cx| {
        ws.insert_template(&template, &config, window, cx);
    });
    draw(&mut f.vcx);
    assert_eq!(f.editor_text().as_deref(), Some("## Meeting Note\nfirst\n"));
    f.vcx.dispatch_action(editor_commands::Undo);
    draw(&mut f.vcx);
    assert_eq!(f.editor_text().as_deref(), Some("first\n"));
}
