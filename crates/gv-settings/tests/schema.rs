// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The registry, the typed structs and the JSON Schema agree.

use std::collections::BTreeSet;

use gv_settings::{Kind, Layer, LayerKind, Settings, json_schema, resolve, sections, specs};

fn leaves(value: &toml::Value, prefix: &str, out: &mut Vec<(String, toml::Value)>) {
    match value {
        toml::Value::Table(table) => {
            for (k, v) in table {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                leaves(v, &path, out);
            }
        }
        other => out.push((prefix.to_owned(), other.clone())),
    }
}

fn struct_defaults() -> Vec<(String, toml::Value)> {
    let value = toml::Value::try_from(Settings::default()).unwrap_or_else(|e| panic!("{e}"));
    let mut out = Vec::new();
    leaves(&value, "", &mut out);
    out
}

#[test]
fn registry_matches_the_structs() {
    let from_structs = struct_defaults();
    let paths: BTreeSet<_> = from_structs.iter().map(|(p, _)| p.clone()).collect();
    let registry: BTreeSet<_> = specs().iter().map(|s| s.path.to_owned()).collect();
    assert_eq!(paths, registry);
    for (path, value) in &from_structs {
        let spec = gv_settings::spec(path).unwrap_or_else(|| panic!("{path}"));
        assert_eq!(&spec.default, value, "{path}");
        // Each default passes its own validation.
        assert_eq!(spec.check_owned(value).as_ref(), Ok(value), "{path}");
    }
}

#[test]
fn every_setting_and_section_is_documented() {
    for spec in specs() {
        assert!(!spec.doc.is_empty(), "{}", spec.path);
    }
    for section in sections() {
        assert!(!section.doc.is_empty(), "{}", section.name);
        assert!(
            specs().iter().any(|s| s.section == section.name),
            "{}",
            section.name
        );
    }
}

#[test]
fn every_enum_value_and_range_end_reaches_the_typed_struct() {
    for spec in specs() {
        let candidates: Vec<toml::Value> = match spec.kind {
            Kind::Enum(variants) => variants
                .iter()
                .map(|v| toml::Value::String((*v).to_owned()))
                .collect(),
            Kind::Integer { min, max } => {
                vec![toml::Value::Integer(min), toml::Value::Integer(max)]
            }
            Kind::Float { min, max } => vec![toml::Value::Float(min), toml::Value::Float(max)],
            Kind::Bool => vec![toml::Value::Boolean(true), toml::Value::Boolean(false)],
            Kind::String => vec![toml::Value::String("x y".to_owned())],
            Kind::StringList => vec![toml::Value::Array(vec![toml::Value::String(
                "a".to_owned(),
            )])],
        };
        for value in candidates {
            let layer = Layer::from_values(LayerKind::Global, [(spec.path, value.clone())]);
            assert!(
                layer.diagnostics().is_empty(),
                "{}: {:?}",
                spec.path,
                layer.diagnostics()
            );
            let effective = resolve([&layer]);
            assert!(effective.diagnostics().is_empty(), "{}", spec.path);
            let typed =
                toml::Value::try_from(effective.settings()).unwrap_or_else(|e| panic!("{e}"));
            let mut out = Vec::new();
            leaves(&typed, "", &mut out);
            let got = out.iter().find(|(p, _)| p == spec.path).map(|(_, v)| v);
            match (&value, got) {
                (toml::Value::Float(want), Some(toml::Value::Float(got))) => {
                    // Settings store f32.
                    assert!((want - got).abs() <= want.abs() * 1e-6, "{}", spec.path);
                }
                _ => assert_eq!(got, Some(&value), "{}", spec.path),
            }
        }
    }
}

#[test]
fn json_schema_matches_the_registry() {
    let schema = json_schema();
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    let props = schema["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("no properties"));
    assert!(props.contains_key("version"));
    let mut seen = BTreeSet::new();
    for (section, body) in props {
        if section == "version" {
            continue;
        }
        let fields = body["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{section}"));
        for (key, field) in fields {
            let path = format!("{section}.{key}");
            let spec = gv_settings::spec(&path).unwrap_or_else(|| panic!("{path}"));
            let ty = field["type"].as_str().unwrap_or_default();
            let expected = match spec.kind {
                Kind::Bool => "boolean",
                Kind::Integer { .. } => "integer",
                Kind::Float { .. } => "number",
                Kind::String | Kind::Enum(_) => "string",
                Kind::StringList => "array",
            };
            assert_eq!(ty, expected, "{path}");
            if let Kind::Enum(variants) = spec.kind {
                let listed: Vec<&str> = field["enum"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str())
                    .collect();
                assert_eq!(listed, variants, "{path}");
            }
            assert!(field["default"] != serde_json::Value::Null, "{path}");
            seen.insert(path);
        }
    }
    let registry: BTreeSet<_> = specs().iter().map(|s| s.path.to_owned()).collect();
    assert_eq!(seen, registry);
    // The schema text is valid JSON.
    let text = gv_settings::json_schema_string();
    assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
}

#[test]
fn security_relevant_settings_are_the_planned_ones() {
    let security: Vec<&str> = specs()
        .iter()
        .filter(|s| s.is_security_relevant())
        .map(|s| s.path)
        .collect();
    assert_eq!(
        security,
        vec![
            "links.confirm_web_links",
            "privacy_and_security.remote_content",
            "privacy_and_security.remote_allow_sites",
            "privacy_and_security.remote_block_sites",
            "privacy_and_security.workspace_trust",
            "privacy_and_security.confirm_every_handoff",
            "privacy_and_security.allow_outside_root_embeds",
        ]
    );
}
