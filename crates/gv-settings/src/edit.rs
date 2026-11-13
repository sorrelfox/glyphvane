// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Format-preserving edits of settings text.
//!
//! Changing an existing value replaces exactly the bytes of that value, so
//! comments, spacing, key order and quoting elsewhere stay byte for byte. A
//! new key is inserted with `toml_edit` into its section (created at the end
//! when missing); the rest of the text is unchanged.

use toml_edit::{DocumentMut, Item, Table};

/// Why an edit was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// The text is not valid TOML. Editing it could lose the user's text, so
    /// it must be fixed first.
    #[error("the file is not valid TOML ({0}); fix it before changing settings")]
    Syntax(String),
    /// The path is empty or deeper than `section.key`.
    #[error("`{0}` is not a settings path")]
    BadPath(String),
    /// A section name is used for something other than a table.
    #[error("`{0}` is not a table in the file")]
    NotATable(String),
    /// The key holds a table, not a value.
    #[error("`{0}` holds a table, not a value")]
    NotAValue(String),
}

fn segments(path: &str) -> Result<(Option<&str>, &str), EditError> {
    let bad = || EditError::BadPath(path.to_owned());
    match path.split_once('.') {
        None if !path.is_empty() => Ok((None, path)),
        Some((section, key)) if !section.is_empty() && !key.is_empty() && !key.contains('.') => {
            Ok((Some(section), key))
        }
        _ => Err(bad()),
    }
}

/// Sets `path` (`section.key`, or a root key such as `version`) to `value`.
pub fn set_value(text: &str, path: &str, value: &toml::Value) -> Result<String, EditError> {
    let (section, key) = segments(path)?;
    let doc =
        toml_edit::Document::parse(text).map_err(|e| EditError::Syntax(e.message().to_owned()))?;
    let parent = match section {
        None => Some(doc.as_table() as &dyn toml_edit::TableLike),
        Some(section) => match doc.get(section) {
            None => None,
            Some(item) => Some(
                item.as_table_like()
                    .ok_or_else(|| EditError::NotATable(section.to_owned()))?,
            ),
        },
    };
    if let Some(item) = parent.and_then(|p| p.get(key)) {
        return match item {
            Item::Value(old) => match old.span() {
                Some(span) => {
                    let mut out = String::with_capacity(text.len() + 16);
                    out.push_str(&text[..span.start]);
                    out.push_str(&value.to_string());
                    out.push_str(&text[span.end..]);
                    Ok(out)
                }
                None => insert(text, section, key, value),
            },
            _ => Err(EditError::NotAValue(path.to_owned())),
        };
    }
    insert(text, section, key, value)
}

fn render(value: &toml::Value) -> toml_edit::Value {
    // `toml::Value` displays as a TOML value, which `toml_edit` parses back.
    value
        .to_string()
        .parse()
        .unwrap_or_else(|_| toml_edit::Value::from(value.to_string()))
}

fn insert(
    text: &str,
    section: Option<&str>,
    key: &str,
    value: &toml::Value,
) -> Result<String, EditError> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| EditError::Syntax(e.message().to_owned()))?;
    let item = Item::Value(render(value));
    match section {
        None => {
            doc.insert(key, item);
        }
        Some(section) => {
            if !doc.contains_key(section) {
                let mut table = Table::new();
                if !text.trim().is_empty() {
                    table.decor_mut().set_prefix("\n");
                }
                doc.insert(section, Item::Table(table));
            }
            let table = doc
                .get_mut(section)
                .and_then(Item::as_table_like_mut)
                .ok_or_else(|| EditError::NotATable(section.to_owned()))?;
            table.insert(key, item);
        }
    }
    let mut out = doc.to_string();
    // A file without a final newline would otherwise run the new table's
    // header into its last line.
    if !text.is_empty() && !text.ends_with('\n') && out.starts_with(text) {
        let rest = &out[text.len()..];
        if !rest.starts_with('\n') {
            out = format!("{text}\n{rest}");
        }
    }
    Ok(out)
}

/// Removes `path` from the text, with the comments attached to its line.
/// Removing a key that is not there leaves the text unchanged.
pub fn remove_value(text: &str, path: &str) -> Result<String, EditError> {
    let (section, key) = segments(path)?;
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| EditError::Syntax(e.message().to_owned()))?;
    let removed = match section {
        None => doc.remove(key).is_some(),
        Some(section) => match doc.get_mut(section) {
            None => false,
            Some(item) => item
                .as_table_like_mut()
                .ok_or_else(|| EditError::NotATable(section.to_owned()))?
                .remove(key)
                .is_some(),
        },
    };
    Ok(if removed {
        doc.to_string()
    } else {
        text.to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> toml::Value {
        toml::Value::String(text.to_owned())
    }

    #[test]
    fn replaces_only_the_value_bytes() {
        let text = "# My settings\nversion = 1\n\n[appearance]\n# picked in 2026\ntheme   =   'light'   # keep\nzoom_percent = 110\n";
        let out = set_value(text, "appearance.theme", &s("dark")).unwrap_or_default();
        assert_eq!(out, text.replace("'light'", "\"dark\""));
    }

    #[test]
    fn edits_dotted_and_inline_forms() {
        let text = "appearance.theme = \"light\" # a\n";
        let out = set_value(text, "appearance.theme", &s("dark")).unwrap_or_default();
        assert_eq!(out, "appearance.theme = \"dark\" # a\n");

        let text = "appearance = { theme = \"light\", zoom_percent = 90 }\n";
        let out = set_value(text, "appearance.zoom_percent", &toml::Value::Integer(120))
            .unwrap_or_default();
        assert_eq!(
            out,
            "appearance = { theme = \"light\", zoom_percent = 120 }\n"
        );
    }

    #[test]
    fn inserts_into_existing_section() {
        let text = "[appearance]\ntheme = \"dark\" # mine\n\n[editor]\nprose_font = true\n";
        let out = set_value(text, "appearance.zoom_percent", &toml::Value::Integer(120))
            .unwrap_or_default();
        assert_eq!(
            out,
            "[appearance]\ntheme = \"dark\" # mine\nzoom_percent = 120\n\n[editor]\nprose_font = true\n"
        );
    }

    #[test]
    fn creates_missing_section_at_end() {
        let text = "version = 1\n\n[appearance]\ntheme = \"dark\"\n";
        let out =
            set_value(text, "editor.prose_font", &toml::Value::Boolean(true)).unwrap_or_default();
        assert_eq!(out, format!("{text}\n[editor]\nprose_font = true\n"));

        let out =
            set_value("", "editor.prose_font", &toml::Value::Boolean(true)).unwrap_or_default();
        assert_eq!(out, "[editor]\nprose_font = true\n");

        let text = "version = 1";
        let out =
            set_value(text, "editor.prose_font", &toml::Value::Boolean(true)).unwrap_or_default();
        assert!(out.parse::<DocumentMut>().is_ok(), "{out}");
        assert!(out.starts_with("version = 1\n"), "{out}");
    }

    #[test]
    fn refuses_broken_files_and_bad_paths() {
        assert!(matches!(
            set_value("[appearance\n", "appearance.theme", &s("dark")),
            Err(EditError::Syntax(_))
        ));
        assert!(matches!(
            set_value("appearance = 3\n", "appearance.theme", &s("dark")),
            Err(EditError::NotATable(_))
        ));
        assert!(matches!(
            set_value("", "a.b.c", &s("x")),
            Err(EditError::BadPath(_))
        ));
        assert!(matches!(
            set_value("[appearance.theme]\n", "appearance.theme", &s("x")),
            Err(EditError::NotAValue(_))
        ));
    }

    #[test]
    fn removes_a_key_and_keeps_the_rest() {
        let text = "[appearance]\n# theme comment\ntheme = \"dark\"\nzoom_percent = 120 # z\n";
        let out = remove_value(text, "appearance.theme").unwrap_or_default();
        assert_eq!(out, "[appearance]\nzoom_percent = 120 # z\n");
        assert_eq!(remove_value(text, "editor.prose_font").as_deref(), Ok(text));
    }
}
