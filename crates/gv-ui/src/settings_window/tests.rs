// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The settings window in headless windows: opening with Mod+, section
//! navigation, search, editing each kind of setting with validation, the
//! scope switch, and saving to the files of a temporary config dir.

use std::fs;
use std::path::{Path, PathBuf};

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Modifiers, Pixels, TestAppContext,
    VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use gv_settings::{LayerKind, Locations, SettingsFile};

use super::model::SectionId;
use super::{SettingsWindow, settings_window};
use crate::appearance::Appearance;
use crate::chrome::WindowChrome;
use crate::settings::{AppSettings, Target, vault_id};
use crate::shell::{OpenTarget, Workspace};
use crate::theme::ThemeId;

const GLOBAL_TEXT: &str =
    "# My settings\nversion = 1\n\n[general]\n# Keep reading.\nopen_files_in = \"reading\"\n";

struct Fixture {
    dir: tempfile::TempDir,
    locations: Locations,
    vault: PathBuf,
    main: AnyWindowHandle,
    window: AnyWindowHandle,
    view: Entity<SettingsWindow>,
    vcx: VisualTestContext,
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

/// A config dir with `global` as `settings.toml`, a vault with a note, a
/// main window on the note, and the settings window opened with Mod+,.
fn fixture(cx: &mut TestAppContext, global: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("temporary folder");
    let locations = Locations::with_root(dir.path().join("app"));
    fs::create_dir_all(locations.config_dir()).expect("config dir");
    fs::write(locations.settings_file(), global).expect("settings written");
    let vault = dir.path().join("Notes");
    fs::create_dir_all(vault.join(".git")).expect("vault marker");
    let note = vault.join("a.md");
    fs::write(&note, "# A\n\nText.\n").expect("note written");
    let file = SettingsFile::load(locations.settings_file(), LayerKind::Global).expect("loads");
    let settings = AppSettings::new(Some(file), None).with_locations(locations.clone());
    let main = cx.update(|cx| {
        crate::init(cx, settings);
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1280.0), px(800.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |window, cx| {
            cx.new(|cx| {
                Workspace::with_chrome(
                    Some(OpenTarget::new(&note)),
                    WindowChrome::Linux,
                    window,
                    cx,
                )
            })
        })
        .expect("main window opens")
        .0
    });
    let mut main_cx = VisualTestContext::from_window(main, cx);
    draw(&mut main_cx);
    main_cx.simulate_keystrokes("secondary-,");
    let (window, view) = cx
        .update(|cx| settings_window(cx))
        .expect("Mod+, opens the settings window");
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    Fixture {
        dir,
        locations,
        vault,
        main,
        window,
        view,
        vcx,
    }
}

impl Fixture {
    fn bounds(&mut self, selector: &str) -> Option<Bounds<Pixels>> {
        // `debug_bounds` takes a `'static` selector.
        let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
        self.vcx.debug_bounds(selector)
    }

    fn drawn(&mut self, selector: &str) -> bool {
        self.bounds(selector).is_some()
    }

    fn click(&mut self, selector: &str) {
        let at = self
            .bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center();
        self.vcx.simulate_click(at, Modifiers::none());
        draw(&mut self.vcx);
    }

    fn keys(&mut self, keys: &str) {
        self.vcx.simulate_keystrokes(keys);
        draw(&mut self.vcx);
    }

    fn section(&mut self, section: SectionId) {
        self.click(&format!("settings-section-{}", section.slug()));
        assert_eq!(self.view.read_with(&self.vcx, |v, _| v.section()), section);
    }

    fn settings(&mut self) -> gv_settings::Settings {
        self.vcx
            .update(|_, cx| cx.global::<AppSettings>().settings().clone())
    }

    fn error(&mut self, path: &str) -> Option<String> {
        let view = self.view.clone();
        self.vcx.update(|_, cx| view.read(cx).error(path, cx))
    }

    fn commit(&mut self, path: &'static str, text: &str) {
        let view = self.view.clone();
        self.vcx.update(|window, cx| {
            view.update(cx, |v, cx| v.commit_text(path, text, window, cx));
        });
        draw(&mut self.vcx);
    }

    fn global_text(&self) -> String {
        fs::read_to_string(self.locations.settings_file()).expect("settings file")
    }

    fn vault_file(&self) -> PathBuf {
        self.locations
            .vault_settings_file(&vault_id(&self.vault))
            .expect("valid vault id")
    }
}

#[gpui::test]
fn mod_comma_opens_one_settings_window_on_appearance(cx: &mut TestAppContext) {
    let mut f = fixture(cx, GLOBAL_TEXT);
    assert_eq!(
        f.view.read_with(&f.vcx, |v, _| v.section()),
        SectionId::Appearance
    );
    assert!(f.drawn("settings-title"));
    assert!(f.drawn("settings-theme-system"));
    assert!(f.drawn("settings-row-appearance.code_theme"));
    for section in SectionId::ALL {
        assert!(f.drawn(&format!("settings-section-{}", section.slug())));
    }
    // The scope switch names the vault of the window it was opened from.
    let vault_name = f.vcx.update(|_, cx| {
        cx.global::<AppSettings>()
            .vault()
            .map(|v| v.name().to_owned())
    });
    assert_eq!(vault_name.as_deref(), Some("Notes"));
    assert!(f.drawn("settings-scope-all") && f.drawn("settings-scope-vault"));

    // Opening it again brings the same window forward.
    f.vcx.cx.simulate_keystrokes(f.main, "secondary-,");
    f.vcx.run_until_parked();
    let windows = f.vcx.update(|_, cx| cx.windows().len());
    assert_eq!(windows, 2);
    let again = f.vcx.update(|_, cx| settings_window(cx).map(|(w, _)| w));
    assert_eq!(again, Some(f.window));

    // Esc closes it (with the search field empty and no list open).
    let mut app = f.vcx.cx.clone();
    app.update_window(f.window, |_, window, cx| {
        window.dispatch_action(Box::new(super::Dismiss), cx);
    })
    .expect("the window is open");
    app.run_until_parked();
    assert!(app.update(|cx| settings_window(cx)).is_none());
    assert_eq!(app.update(|cx| cx.windows().len()), 1);
}

#[gpui::test]
fn the_sidebar_switches_sections(cx: &mut TestAppContext) {
    let mut f = fixture(cx, GLOBAL_TEXT);
    f.section(SectionId::Editor);
    assert!(f.drawn("settings-row-editor.prose_font"));
    assert!(!f.drawn("settings-theme-system"));
    f.section(SectionId::Profiles);
    assert!(f.drawn("settings-row-profiles.default_profile"));
    assert!(f.drawn("settings-row-profiles.vault_profile"));
    assert!(!f.drawn("settings-row-editor.prose_font"));
    f.section(SectionId::Keymap);
    assert!(f.drawn("settings-open-keymap"));
    f.section(SectionId::Appearance);
    assert!(f.drawn("settings-theme-dark"));
}

#[gpui::test]
fn search_filters_sections_and_rows(cx: &mut TestAppContext) {
    let mut f = fixture(cx, GLOBAL_TEXT);
    // The search field has the keyboard when the window opens.
    f.vcx.simulate_input("hover delay");
    draw(&mut f.vcx);
    assert_eq!(
        f.view.read_with(&f.vcx, |v, _| v.query().to_owned()),
        "hover delay"
    );
    assert_eq!(
        f.view.read_with(&f.vcx, |v, _| v.listed_sections()),
        vec![SectionId::Preview]
    );
    assert_eq!(
        f.view.read_with(&f.vcx, |v, _| v.listed_settings()),
        vec!["preview.hover_preview_delay_ms"]
    );
    assert!(f.drawn("settings-row-preview.hover_preview_delay_ms"));
    assert!(f.drawn("settings-section-preview"));
    assert!(!f.drawn("settings-section-general"));
    assert!(!f.drawn("settings-theme-system"));

    // Designed labels are found too, and shown as rows.
    let view = f.view.clone();
    f.vcx
        .update(|window, cx| view.update(cx, |v, cx| v.set_query("light appearance", window, cx)));
    draw(&mut f.vcx);
    assert!(f.drawn("settings-row-appearance.preferred_light_theme"));

    f.vcx
        .update(|window, cx| view.update(cx, |v, cx| v.set_query("zzzz", window, cx)));
    draw(&mut f.vcx);
    assert!(f.drawn("settings-no-results"));

    // Esc clears the search first; the window stays.
    f.keys("escape");
    assert_eq!(f.view.read_with(&f.vcx, |v, _| v.query().to_owned()), "");
    assert!(f.drawn("settings-section-general"));
    assert!(f.vcx.update(|_, cx| settings_window(cx)).is_some());
}

#[gpui::test]
fn a_switch_saves_a_boolean_and_applies_at_once(cx: &mut TestAppContext) {
    let mut f = fixture(cx, GLOBAL_TEXT);
    f.section(SectionId::General);
    assert!(!f.settings().general.viewer_mode);
    f.click("settings-switch-general.viewer_mode");
    assert!(f.settings().general.viewer_mode);
    assert!(f.vcx.update(|_, cx| AppSettings::viewer_mode(cx)));
    // Only the value is added; the comments stay.
    let text = f.global_text();
    assert!(text.starts_with("# My settings\nversion = 1\n\n[general]\n# Keep reading.\n"));
    assert!(text.contains("viewer_mode = true"));
    // Reset removes it again.
    f.click("settings-reset-general.viewer_mode");
    assert!(!f.settings().general.viewer_mode);
    assert!(!f.global_text().contains("viewer_mode"));
}

#[gpui::test]
fn a_select_saves_an_enum(cx: &mut TestAppContext) {
    let mut f = fixture(cx, GLOBAL_TEXT);
    f.section(SectionId::Files);
    f.click("settings-select-files.external_files");
    assert!(f.drawn("settings-list-files.external_files"));
    f.click("settings-option-files.external_files-copy");
    assert!(!f.drawn("settings-list-files.external_files"));
    assert_eq!(
        f.settings().files.external_files,
        gv_settings::ExternalFiles::Copy
    );
    assert!(f.global_text().contains("external_files = \"copy\""));

    // Esc closes an open list before anything else.
    f.click("settings-select-files.external_files");
    assert!(f.drawn("settings-list-files.external_files"));
    f.keys("escape");
    assert!(!f.drawn("settings-list-files.external_files"));
    assert!(f.vcx.update(|_, cx| settings_window(cx)).is_some());
}

#[gpui::test]
fn number_fields_are_validated_before_saving(cx: &mut TestAppContext) {
    let mut f = fixture(cx, GLOBAL_TEXT);
    f.section(SectionId::Preview);
    let key = "preview.hover_preview_delay_ms";

    // Out of range: refused, shown in the row, nothing saved.
    f.commit(key, "99999");
    let error = f.error(key).expect("an inline error");
    assert!(error.contains("from 0 to 5000"), "{error}");
    assert_eq!(f.settings().preview.hover_preview_delay_ms, 300);
    assert!(!f.global_text().contains("hover_preview_delay_ms"));

    // Not a number at all.
    f.commit(key, "soon");
    assert!(f.error(key).expect("an inline error").contains("soon"));

    // Typed into the field and confirmed with Enter.
    f.click(&format!("settings-field-{key}"));
    f.keys("secondary-a");
    f.vcx.simulate_input("450");
    f.keys("enter");
    assert_eq!(f.error(key), None);
    assert_eq!(f.settings().preview.hover_preview_delay_ms, 450);
    assert!(f.global_text().contains("hover_preview_delay_ms = 450"));

    // A float setting takes a fraction.
    f.section(SectionId::Appearance);
    f.commit("appearance.editor_font_size", "13.5");
    assert_eq!(f.settings().appearance.editor_font_size, 13.5);
}

#[gpui::test]
fn the_wide_blocks_row_fills_or_sets_a_width(cx: &mut TestAppContext) {
    let mut f = fixture(cx, GLOBAL_TEXT);
    const FILL: &str = "appearance.fill_wide_blocks";
    const CHARS: &str = "appearance.wide_block_width_chars";
    // Right below the readable line width, on by default.
    let readable = f
        .bounds("settings-row-appearance.readable_line_width")
        .expect("the readable line width row");
    let wide = f
        .bounds(&format!("settings-row-{FILL}"))
        .expect("the wide blocks row");
    assert!(wide.top() >= readable.bottom() - px(1.0), "{wide:?}");
    assert!(wide.top() <= readable.bottom() + px(1.0), "{wide:?}");
    assert!(f.drawn(&format!("settings-field-{CHARS}")));
    assert!(f.settings().appearance.fill_wide_blocks);

    // While it fills the space, the width field does not take the
    // keyboard: typing goes on into the search field.
    f.click(&format!("settings-field-{CHARS}"));
    f.vcx.simulate_input("200");
    draw(&mut f.vcx);
    assert_eq!(f.view.read_with(&f.vcx, |v, _| v.query().to_owned()), "200");
    f.keys("escape");
    assert_eq!(f.view.read_with(&f.vcx, |v, _| v.query().to_owned()), "");
    assert_eq!(f.settings().appearance.wide_block_width_chars, 120);

    // Off: the field sets the width, applied at once.
    f.click(&format!("settings-switch-{FILL}"));
    assert!(!f.settings().appearance.fill_wide_blocks);
    assert!(f.global_text().contains("fill_wide_blocks = false"));
    f.click(&format!("settings-field-{CHARS}"));
    f.keys("secondary-a");
    f.vcx.simulate_input("160");
    f.keys("enter");
    assert_eq!(f.error(CHARS), None);
    assert_eq!(f.settings().appearance.wide_block_width_chars, 160);
    assert!(f.global_text().contains("wide_block_width_chars = 160"));
    f.commit(CHARS, "1000");
    let error = f.error(CHARS).expect("an inline error");
    assert!(error.contains("from 40 to 400"), "{error}");
    assert_eq!(f.settings().appearance.wide_block_width_chars, 160);

    // Without a readable line width the row is disabled.
    f.click("settings-switch-appearance.readable_line_width");
    assert!(!f.settings().appearance.readable_line_width);
    f.click(&format!("settings-switch-{FILL}"));
    assert!(!f.settings().appearance.fill_wide_blocks);
    f.click("settings-switch-appearance.readable_line_width");
    f.click(&format!("settings-switch-{FILL}"));
    assert!(f.settings().appearance.fill_wide_blocks);
}

#[gpui::test]
fn text_and_list_fields_save_strings(cx: &mut TestAppContext) {
    let mut f = fixture(cx, GLOBAL_TEXT);
    f.section(SectionId::Files);
    f.commit("files.attachment_folder", "./attachments");
    assert_eq!(f.settings().files.attachment_folder, "./attachments");
    f.section(SectionId::Spelling);
    f.commit("spelling.languages", "en_GB, de_DE");
    assert_eq!(f.settings().spelling.languages, vec!["en_GB", "de_DE"]);
    let text = f.global_text();
    assert!(text.contains("attachment_folder = \"./attachments\""));
    assert!(text.contains("languages = [\"en_GB\", \"de_DE\"]"));
}

#[gpui::test]
fn the_theme_picker_applies_the_theme_at_once(cx: &mut TestAppContext) {
    let mut f = fixture(cx, GLOBAL_TEXT);
    // System in a light test window: the preferred light theme shows.
    assert_eq!(
        f.vcx.update(|_, cx| Appearance::current_id(cx)),
        ThemeId::Light
    );
    f.click("settings-preferred-semi-light");
    assert_eq!(
        f.vcx.update(|_, cx| Appearance::current_id(cx)),
        ThemeId::SemiLight
    );
    f.click("settings-theme-dark");
    assert_eq!(
        f.vcx.update(|_, cx| Appearance::current_id(cx)),
        ThemeId::Dark
    );
    let text = f.global_text();
    assert!(text.contains("theme = \"dark\""));
    assert!(text.contains("preferred_light_theme = \"semi-light\""));
    // Away from System the light-theme choice is disabled.
    f.click("settings-preferred-light");
    assert_eq!(
        f.settings().appearance.preferred_light_theme,
        gv_settings::PreferredLightTheme::SemiLight
    );
    // The zoom stepper steps by 10.
    f.click("settings-zoom-plus");
    assert_eq!(f.settings().appearance.zoom_percent, 110);
}

#[gpui::test]
fn the_scope_switch_edits_the_vault_overlay(cx: &mut TestAppContext) {
    let mut f = fixture(cx, GLOBAL_TEXT);
    f.click("settings-scope-vault");
    assert_eq!(f.view.read_with(&f.vcx, |v, _| v.target()), Target::Vault);
    f.section(SectionId::General);
    f.click("settings-switch-general.viewer_mode");
    assert!(f.settings().general.viewer_mode);
    let overlay = fs::read_to_string(f.vault_file()).expect("vault overlay written");
    assert!(overlay.contains("viewer_mode = true"));
    assert!(!f.global_text().contains("viewer_mode"));

    // A global-only setting cannot be set for one vault.
    let view = f.view.clone();
    f.vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.apply("general.check_for_updates", toml::Value::Boolean(true), cx)
        })
    });
    assert!(
        f.error("general.check_for_updates")
            .expect("refused")
            .contains("all vaults")
    );
    assert!(!f.settings().general.check_for_updates);

    // Profiles: the vault profile the profile chip reads.
    f.section(SectionId::Profiles);
    f.click("settings-select-profiles.vault_profile");
    f.click("settings-option-profiles.vault_profile-gfm-file");
    assert_eq!(
        f.settings().profiles.vault_profile,
        gv_settings::ProfileOverride::GfmFile
    );

    // All vaults shows the global value; the vault's stays in effect.
    f.click("settings-scope-all");
    f.section(SectionId::General);
    let all = f.vcx.update(|_, cx| {
        cx.global::<AppSettings>()
            .effective_for(Target::AllVaults)
            .settings()
            .general
            .viewer_mode
    });
    assert!(!all);
    assert!(f.settings().general.viewer_mode);

    // "Use all vaults" removes the vault's value.
    f.click("settings-scope-vault");
    f.click("settings-reset-general.viewer_mode");
    assert!(!f.settings().general.viewer_mode);
    let overlay = fs::read_to_string(f.vault_file()).expect("vault overlay");
    assert!(!overlay.contains("viewer_mode"));
}

#[gpui::test]
fn invalid_values_in_the_file_show_in_their_row(cx: &mut TestAppContext) {
    let text = "version = 1\n[preview]\nembed_max_depth = 500\n";
    let mut f = fixture(cx, text);
    f.section(SectionId::Preview);
    let error = f
        .error("preview.embed_max_depth")
        .expect("the file's error");
    assert!(error.contains("from 1 to 20"), "{error}");
    // Fixing it from the window clears the error.
    f.commit("preview.embed_max_depth", "6");
    assert_eq!(f.error("preview.embed_max_depth"), None);
    assert!(f.global_text().contains("embed_max_depth = 6"));
}

#[gpui::test]
fn edits_survive_a_restart(cx: &mut TestAppContext) {
    let mut f = fixture(cx, GLOBAL_TEXT);
    f.click("settings-theme-semi-light");
    f.section(SectionId::Editor);
    f.click("settings-switch-editor.larger_headings");
    f.commit("editor.caret_blink_idle_secs", "30");
    f.click("settings-scope-vault");
    f.section(SectionId::Links);
    f.click("settings-switch-links.use_markdown_links");
    let before = f.settings();

    // What the next start reads.
    let global = SettingsFile::load(f.locations.settings_file(), LayerKind::Global)
        .expect("global file loads");
    let vault = SettingsFile::load(f.vault_file(), LayerKind::Vault).expect("overlay loads");
    let stack = gv_settings::SettingsStack::new([global.layer().clone(), vault.layer().clone()]);
    let after = stack.settings();
    assert_eq!(after.appearance.theme, gv_settings::ThemeChoice::SemiLight);
    assert!(after.editor.larger_headings);
    assert_eq!(after.editor.caret_blink_idle_secs, 30);
    assert!(after.links.use_markdown_links);
    assert_eq!(*after, before);
    assert!(global.text().starts_with("# My settings\n"));
    assert!(stack.effective().diagnostics().is_empty());
    let _ = &f.dir;
    let _: &Path = &f.vault;
}

#[test]
fn vault_ids_are_safe_and_distinct() {
    let a = vault_id(Path::new("/home/me/Notes"));
    let b = vault_id(Path::new("/srv/Notes"));
    assert_ne!(a, b);
    assert!(a.starts_with("Notes-"));
    let odd = vault_id(Path::new("/tmp/my notes (2)"));
    assert!(
        odd.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "{odd}"
    );
    let locations = Locations::with_root("/x");
    assert!(locations.vault_settings_file(&odd).is_ok());
}

#[gpui::test]
fn the_interface_font_and_size_apply_to_the_chrome_at_once(cx: &mut TestAppContext) {
    use crate::metrics::{font, text};

    let mut f = fixture(cx, GLOBAL_TEXT);
    assert_eq!(text::UI.size, 12.0);
    assert_eq!(text::UI.line, 16.0);

    // The size field scales every interface size, and GPUI Component's
    // default text size with it.
    f.commit("appearance.interface_font_size", "15");
    assert_eq!(f.settings().appearance.interface_font_size, 15.0);
    assert_eq!(
        f.vcx.update(|_, cx| Appearance::interface_font_size(cx)),
        15.0
    );
    assert_eq!(text::UI.size, 15.0);
    assert_eq!(text::UI.line, 20.0);
    assert_eq!(text::UI_SMALL.size, 13.75);
    assert_eq!(
        f.vcx
            .update(|_, cx| gpui::component::Theme::global(cx).font_size),
        px(15.0)
    );
    assert!(f.global_text().contains("interface_font_size = 15"));

    // Out of range: refused, the scale stays.
    f.commit("appearance.interface_font_size", "40");
    assert!(f.error("appearance.interface_font_size").is_some());
    assert_eq!(text::UI.size, 15.0);

    // The family menu offers the system font first; picking it uses GPUI's
    // name for the platform font.
    f.click("settings-select-appearance.interface_font");
    assert!(f.drawn("settings-list-appearance.interface_font"));
    f.click("settings-option-appearance.interface_font-system");
    assert_eq!(f.settings().appearance.interface_font, "system");
    assert_eq!(
        f.vcx
            .update(|_, cx| Appearance::interface_font(cx))
            .as_ref(),
        font::SYSTEM_FALLBACK
    );
    assert_eq!(
        f.vcx
            .update(|_, cx| gpui::component::Theme::global(cx).font_family.clone())
            .as_ref(),
        font::SYSTEM_FALLBACK
    );

    // An installed family is used as is; one that is not installed falls
    // back to the system font.
    let installed = f
        .vcx
        .update(|_, cx| cx.text_system().all_font_names())
        .into_iter()
        .find(|name| !name.starts_with('.'));
    if let Some(installed) = installed {
        f.commit_family(&installed);
        assert_eq!(
            f.vcx
                .update(|_, cx| Appearance::interface_font(cx))
                .as_ref(),
            installed
        );
    }
    f.commit_family("No Such Family");
    assert_eq!(
        f.vcx
            .update(|_, cx| Appearance::interface_font(cx))
            .as_ref(),
        font::SYSTEM_FALLBACK
    );

    // Removing the size brings back the design size.
    f.vcx.update(|_, cx| {
        AppSettings::reset_value(cx, Target::AllVaults, "appearance.interface_font_size")
            .expect("the size is removed");
    });
    draw(&mut f.vcx);
    assert_eq!(text::UI.size, 12.0);
    assert_eq!(text::UI.line, 16.0);
}

impl Fixture {
    fn commit_family(&mut self, family: &str) {
        let value = toml::Value::String(family.to_owned());
        self.vcx.update(|_, cx| {
            AppSettings::set_value(cx, Target::AllVaults, "appearance.interface_font", &value)
                .expect("a family is saved");
        });
        draw(&mut self.vcx);
    }
}
