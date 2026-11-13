// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! A JSON Schema (draft 2020-12) of `settings.toml`, generated from the
//! registry, so text editors with a TOML language server can complete keys
//! and check values. Unknown keys are allowed, as the app keeps them.

use serde_json::{Map, Value, json};

use crate::migrate::CURRENT_VERSION;
use crate::spec::{self, Kind, Spec};

/// The schema's `$id`.
pub const SCHEMA_ID: &str = "https://github.com/sorrelfox/glyphvane/settings.schema.json";

/// The JSON Schema of the settings file.
pub fn json_schema() -> Value {
    let mut properties = Map::new();
    properties.insert(
        "version".to_owned(),
        json!({
            "description": "The settings file version. Older files are upgraded automatically.",
            "type": "integer",
            "minimum": 1,
            "default": CURRENT_VERSION,
        }),
    );
    for section in spec::sections() {
        let mut fields = Map::new();
        for spec in spec::specs().iter().filter(|s| s.section == section.name) {
            fields.insert(spec.key.to_owned(), field(spec));
        }
        properties.insert(
            section.name.to_owned(),
            json!({
                "description": section.doc,
                "type": "object",
                "properties": fields,
            }),
        );
    }
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": SCHEMA_ID,
        "title": "Glyphvane settings",
        "type": "object",
        "properties": properties,
    })
}

/// [`json_schema`] as pretty-printed text.
pub fn json_schema_string() -> String {
    // A `serde_json::Value` always serializes.
    serde_json::to_string_pretty(&json_schema()).unwrap_or_default()
}

fn field(spec: &Spec) -> Value {
    let mut out = Map::new();
    let mut description = spec.doc.clone();
    if spec.is_security_relevant() {
        description.push_str(" Settings inside a vault can only make this stricter.");
    }
    out.insert("description".to_owned(), Value::String(description));
    match spec.kind {
        Kind::Bool => {
            out.insert("type".to_owned(), json!("boolean"));
        }
        Kind::Integer { min, max } => {
            out.insert("type".to_owned(), json!("integer"));
            out.insert("minimum".to_owned(), json!(min));
            out.insert("maximum".to_owned(), json!(max));
        }
        Kind::Float { min, max } => {
            out.insert("type".to_owned(), json!("number"));
            out.insert("minimum".to_owned(), json!(min));
            out.insert("maximum".to_owned(), json!(max));
        }
        Kind::String => {
            out.insert("type".to_owned(), json!("string"));
        }
        Kind::StringList => {
            out.insert("type".to_owned(), json!("array"));
            out.insert("items".to_owned(), json!({ "type": "string" }));
        }
        Kind::Enum(variants) => {
            out.insert("type".to_owned(), json!("string"));
            out.insert("enum".to_owned(), json!(variants));
        }
    }
    out.insert("default".to_owned(), to_json(&spec.default));
    Value::Object(out)
}

pub(crate) fn to_json(value: &toml::Value) -> Value {
    match value {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => json!(i),
        toml::Value::Float(f) => json!(f),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(items) => Value::Array(items.iter().map(to_json).collect()),
        toml::Value::Table(table) => {
            Value::Object(table.iter().map(|(k, v)| (k.clone(), to_json(v))).collect())
        }
    }
}
