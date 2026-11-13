// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Layering, provenance, the tighten-only rule and live reload.

mod common;

use common::value_for;
use gv_settings::{
    DiagnosticKind, Layer, LayerKind, RemoteContent, SettingsStack, Spec, ThemeChoice,
    WorkspaceTrust, resolve, specs,
};
use proptest::prelude::*;
use toml::Value;

fn s(text: &str) -> Value {
    Value::String(text.to_owned())
}

fn list(items: &[&str]) -> Value {
    Value::Array(items.iter().map(|i| s(i)).collect())
}

fn parse(kind: LayerKind, text: &str) -> Layer {
    Layer::parse(kind, None, text)
}

#[test]
fn defaults_come_from_the_default_layer() {
    let effective = resolve([]);
    assert_eq!(effective.settings(), &gv_settings::Settings::default());
    for spec in specs() {
        assert_eq!(
            effective.source(spec.path),
            Some(LayerKind::Default),
            "{}",
            spec.path
        );
        assert_eq!(
            effective.value(spec.path),
            Some(&spec.default),
            "{}",
            spec.path
        );
    }
}

#[test]
fn higher_layers_win_and_record_their_source() {
    let global = parse(
        LayerKind::Global,
        "[appearance]\ntheme = \"dark\"\nzoom_percent = 110\n",
    );
    let vault = parse(LayerKind::Vault, "[appearance]\nzoom_percent = 130\n");
    // Order of the arguments does not matter.
    let effective = resolve([&vault, &global]);
    assert_eq!(effective.settings().appearance.theme, ThemeChoice::Dark);
    assert_eq!(effective.settings().appearance.zoom_percent, 130);
    assert_eq!(
        effective.source("appearance.theme"),
        Some(LayerKind::Global)
    );
    assert_eq!(
        effective.source("appearance.zoom_percent"),
        Some(LayerKind::Vault)
    );
    assert_eq!(
        effective.source("appearance.code_theme"),
        Some(LayerKind::Default)
    );
}

#[test]
fn invalid_value_falls_back_to_the_layer_below() {
    let global = parse(LayerKind::Global, "[appearance]\nzoom_percent = 110\n");
    let vault = parse(
        LayerKind::Vault,
        "[appearance]\nzoom_percent = \"big\"\ntheme = \"dark\"\n",
    );
    let effective = resolve([&global, &vault]);
    assert_eq!(effective.settings().appearance.zoom_percent, 110);
    assert_eq!(
        effective.source("appearance.zoom_percent"),
        Some(LayerKind::Global)
    );
    assert_eq!(effective.settings().appearance.theme, ThemeChoice::Dark);
    assert!(
        effective
            .diagnostics()
            .iter()
            .any(|d| d.kind == DiagnosticKind::InvalidValue && d.layer == LayerKind::Vault)
    );
}

#[test]
fn in_vault_files_cannot_relax_security_settings() {
    let global = parse(LayerKind::Global, "");
    let in_vault = parse(
        LayerKind::InVault,
        "[privacy_and_security]\nremote_content = \"allow\"\nworkspace_trust = \"trusted\"\n\
         allow_outside_root_embeds = true\nremote_allow_sites = [\"tracker.example\"]\n\
         remote_block_sites = [\"ads.example\"]\nconfirm_every_handoff = true\n\
         [appearance]\ntheme = \"dark\"\n",
    );
    let effective = resolve([&global, &in_vault]);
    let p = &effective.settings().privacy_and_security;
    // Looser values are ignored...
    assert_eq!(p.remote_content, RemoteContent::Ask);
    assert_eq!(p.workspace_trust, WorkspaceTrust::Ask);
    assert!(!p.allow_outside_root_embeds);
    assert!(p.remote_allow_sites.is_empty());
    // ...stricter ones and ordinary settings apply.
    assert_eq!(p.remote_block_sites, vec!["ads.example".to_owned()]);
    assert!(p.confirm_every_handoff);
    assert_eq!(effective.settings().appearance.theme, ThemeChoice::Dark);
    assert_eq!(
        effective.source("appearance.theme"),
        Some(LayerKind::InVault)
    );

    let ignored: Vec<_> = effective
        .diagnostics()
        .iter()
        .filter(|d| d.kind == DiagnosticKind::NotTightening)
        .filter_map(|d| d.key.clone())
        .collect();
    assert_eq!(
        ignored,
        vec![
            "privacy_and_security.remote_content",
            "privacy_and_security.remote_allow_sites",
            "privacy_and_security.workspace_trust",
            "privacy_and_security.allow_outside_root_embeds",
        ]
    );
    let line = effective
        .diagnostics()
        .iter()
        .find(|d| d.key.as_deref() == Some("privacy_and_security.remote_content"))
        .and_then(|d| d.position)
        .map(|p| p.line);
    assert_eq!(line, Some(2));
}

#[test]
fn in_vault_files_can_tighten() {
    let global = parse(
        LayerKind::Global,
        "[privacy_and_security]\nremote_content = \"allow\"\n",
    );
    let in_vault = parse(
        LayerKind::InVault,
        "[privacy_and_security]\nremote_content = \"ask\"\n",
    );
    let effective = resolve([&global, &in_vault]);
    assert_eq!(
        effective.settings().privacy_and_security.remote_content,
        RemoteContent::Ask
    );
    assert_eq!(
        effective.source("privacy_and_security.remote_content"),
        Some(LayerKind::InVault)
    );
}

#[test]
fn the_app_owned_vault_overlay_may_relax_but_not_a_global_block() {
    // "Allow for this vault" is stored in the vault overlay and applies.
    let global = parse(LayerKind::Global, "");
    let vault = parse(
        LayerKind::Vault,
        "[privacy_and_security]\nremote_content = \"allow\"\n",
    );
    let effective = resolve([&global, &vault]);
    assert_eq!(
        effective.settings().privacy_and_security.remote_content,
        RemoteContent::Allow
    );

    // A global Block is absolute.
    let global = parse(
        LayerKind::Global,
        "[privacy_and_security]\nremote_content = \"block\"\n",
    );
    let effective = resolve([&global, &vault]);
    assert_eq!(
        effective.settings().privacy_and_security.remote_content,
        RemoteContent::Block
    );
    assert!(
        effective
            .diagnostics()
            .iter()
            .any(|d| d.kind == DiagnosticKind::LockedByGlobal)
    );
}

#[test]
fn site_lists_combine_across_layers() {
    let global = parse(
        LayerKind::Global,
        "[privacy_and_security]\nremote_block_sites = [\"a.example\"]\nremote_allow_sites = [\"img.example\"]\n",
    );
    let vault = parse(
        LayerKind::Vault,
        "[privacy_and_security]\nremote_block_sites = [\"b.example\", \"a.example\"]\nremote_allow_sites = []\n",
    );
    let effective = resolve([&global, &vault]);
    assert_eq!(
        effective.value("privacy_and_security.remote_block_sites"),
        Some(&list(&["a.example", "b.example"]))
    );
    // An empty vault list cannot drop global entries.
    assert_eq!(
        effective.value("privacy_and_security.remote_allow_sites"),
        Some(&list(&["img.example"]))
    );
}

#[test]
fn global_only_settings_are_ignored_in_vault_layers() {
    let vault = parse(LayerKind::Vault, "[general]\ncheck_for_updates = true\n");
    let effective = resolve([&vault]);
    assert!(!effective.settings().general.check_for_updates);
    assert!(
        effective
            .diagnostics()
            .iter()
            .any(|d| d.kind == DiagnosticKind::GlobalOnly)
    );
}

#[test]
fn per_file_state_layers_on_top() {
    let vault = parse(
        LayerKind::Vault,
        "[profiles]\nvault_profile = \"obsidian\"\n",
    );
    let file = Layer::from_values(
        LayerKind::File,
        [("profiles.vault_profile", s("commonmark"))],
    );
    let effective = resolve([&vault, &file]);
    assert_eq!(
        effective.settings().profiles.vault_profile,
        gv_settings::ProfileOverride::Commonmark
    );
    assert_eq!(
        effective.source("profiles.vault_profile"),
        Some(LayerKind::File)
    );

    let bad = Layer::from_values(
        LayerKind::File,
        [("profiles.vault_profile", s("nope")), ("x.y", s("z"))],
    );
    assert_eq!(bad.diagnostics().len(), 2);
}

#[test]
fn live_reload_reports_changed_keys() {
    let mut stack = SettingsStack::new([
        parse(LayerKind::Global, "[appearance]\ntheme = \"dark\"\n"),
        parse(LayerKind::Vault, ""),
    ]);
    let changes = stack.reload(
        LayerKind::Vault,
        None,
        "[appearance]\ntheme = \"dark\"\nzoom_percent = 120\n",
    );
    let keys: Vec<_> = changes.iter().map(|c| (c.key, c.value_changed())).collect();
    // The theme keeps its value but now comes from the vault.
    assert_eq!(
        keys,
        vec![
            ("appearance.theme", false),
            ("appearance.zoom_percent", true)
        ]
    );
    assert_eq!(stack.settings().appearance.zoom_percent, 120);

    let changes = stack.reload(
        LayerKind::Vault,
        None,
        "[appearance]\ntheme = \"dark\"\nzoom_percent = 120\n",
    );
    assert!(changes.is_empty());

    let changes = stack.remove_layer(LayerKind::Vault);
    assert_eq!(changes.len(), 2);
    assert_eq!(
        stack.effective().source("appearance.theme"),
        Some(LayerKind::Global)
    );
}

// Property tests.

fn layer_values() -> impl Strategy<Value = Vec<(&'static Spec, Value)>> {
    prop::collection::vec(
        common::any_spec().prop_flat_map(|spec| (Just(spec), value_for(spec).prop_map(|(_, v)| v))),
        0..12,
    )
}

fn layer_from(kind: LayerKind, values: &[(&'static Spec, Value)]) -> Layer {
    Layer::from_values(kind, values.iter().map(|(s, v)| (s.path, v.clone())))
}

/// The last value per path, as a layer would hold it.
fn last_per_path(values: &[(&'static Spec, Value)]) -> Vec<(&'static Spec, Value)> {
    let mut out: Vec<(&'static Spec, Value)> = Vec::new();
    for (spec, value) in values {
        out.retain(|(s, _)| s.path != spec.path);
        out.push((spec, value.clone()));
    }
    out
}

fn security_specs() -> Vec<&'static Spec> {
    specs()
        .iter()
        .filter(|s| s.is_security_relevant())
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Without in-vault layers or locks, each value comes from the highest
    /// layer that sets it, and the source says so.
    #[test]
    fn provenance_is_the_highest_setting_layer(
        global in layer_values(),
        vault in layer_values(),
        file in layer_values(),
    ) {
        let layers = [
            (LayerKind::Global, last_per_path(&global)),
            (LayerKind::Vault, last_per_path(&vault)),
            (LayerKind::File, last_per_path(&file)),
        ];
        let built: Vec<Layer> = layers.iter().map(|(k, v)| layer_from(*k, v)).collect();
        let effective = resolve(&built);
        for spec in specs() {
            if spec.absolute_in_global.is_some() || spec.merge == gv_settings::Merge::Union {
                continue;
            }
            let mut expected = (spec.default.clone(), LayerKind::Default);
            for (kind, values) in &layers {
                if spec.scope == gv_settings::Scope::GlobalOnly && *kind != LayerKind::Global {
                    continue;
                }
                if let Some((_, v)) = values.iter().find(|(s, _)| s.path == spec.path) {
                    expected = (v.clone(), *kind);
                }
            }
            prop_assert_eq!(effective.value(spec.path), Some(&expected.0), "{}", spec.path);
            prop_assert_eq!(effective.source(spec.path), Some(expected.1), "{}", spec.path);
        }
    }

    /// Layering is associative: merging two trusted layers into one first
    /// gives the same values as applying them one after the other.
    #[test]
    fn layering_is_associative(a in layer_values(), b in layer_values(), c in layer_values()) {
        let ab: Vec<_> = a.iter().chain(b.iter()).cloned().collect();
        let bc: Vec<_> = b.iter().chain(c.iter()).cloned().collect();
        let left = [layer_from(LayerKind::Global, &ab), layer_from(LayerKind::Vault, &c)];
        let right = [layer_from(LayerKind::Global, &a), layer_from(LayerKind::Vault, &bc)];
        let left = resolve(&left);
        let right = resolve(&right);
        for spec in specs() {
            if spec.absolute_in_global.is_some()
                || spec.merge == gv_settings::Merge::Union
                || spec.scope == gv_settings::Scope::GlobalOnly
            {
                continue;
            }
            prop_assert_eq!(left.value(spec.path), right.value(spec.path), "{}", spec.path);
        }
        // The typed view always builds from validated values.
        prop_assert!(left.diagnostics().iter().all(|d| !d.message.contains("internal error")));
    }

    /// An in-vault layer never makes a security-relevant setting less strict
    /// than the layers below it, whatever it contains.
    #[test]
    fn in_vault_layers_never_relax(
        global in layer_values(),
        in_vault in prop::collection::vec(
            prop::sample::select(security_specs())
                .prop_flat_map(|spec| (Just(spec), value_for(spec).prop_map(|(_, v)| v))),
            0..10,
        ),
    ) {
        let global = layer_from(LayerKind::Global, &last_per_path(&global));
        let in_vault = layer_from(LayerKind::InVault, &last_per_path(&in_vault));
        let base = resolve([&global]);
        let with = resolve([&global, &in_vault]);
        for spec in security_specs() {
            let (Some(before), Some(after), Some(tighten)) =
                (base.value(spec.path), with.value(spec.path), spec.tighten)
            else {
                return Err(TestCaseError::fail(spec.path));
            };
            prop_assert!(
                tighten.at_least_as_strict(&spec.kind, after, before),
                "{} relaxed from {} to {}", spec.path, before, after
            );
            // A value that is at least as strict is applied.
            if let Some(entry) = in_vault.get(spec.path)
                && spec.merge == gv_settings::Merge::Replace
                && tighten.at_least_as_strict(&spec.kind, &entry.value, before)
            {
                prop_assert_eq!(after, &entry.value);
            }
        }
    }
}
