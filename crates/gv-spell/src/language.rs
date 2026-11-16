// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Language tags and a document's own languages.

use gv_syntax::{FrontMatterKind, Leaf, LeafKind, SyntaxTree, Visitor, walk_range};
use gv_text::TextSource;

/// A language tag in dictionary form: `en-us` → `en_US`, `de` → `de`,
/// `en_GB-ise` stays. `None` for an empty tag or one with characters no
/// tag has.
pub fn normalize_tag(tag: &str) -> Option<String> {
    let tag = tag.trim();
    if tag.is_empty()
        || !tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let mut parts = tag.splitn(2, ['-', '_']);
    let language = parts.next()?.to_ascii_lowercase();
    let Some(rest) = parts.next() else {
        return Some(language);
    };
    // The region is the part up to the next separator.
    let (region, variant) = match rest.find(['-', '_']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let region = if region.len() == 2 {
        region.to_ascii_uppercase()
    } else {
        region.to_owned()
    };
    Some(format!("{language}_{region}{variant}"))
}

/// The language part of a tag or dictionary id (`en` of `en_US`).
pub(crate) fn primary(tag: &str) -> &str {
    tag.split(['-', '_']).next().unwrap_or(tag)
}

/// The languages a document names in its front matter: `lang` or
/// `language`, one value or a list (`lang: de`, `lang: [en, de]`,
/// `language = "fr"`, a YAML block list). `None` when it names none.
pub fn document_languages<S: TextSource + ?Sized>(
    tree: &SyntaxTree,
    text: &S,
) -> Option<Vec<String>> {
    struct Find {
        found: Option<(FrontMatterKind, Vec<std::ops::Range<usize>>)>,
    }
    impl Visitor for Find {
        fn leaf(&mut self, leaf: &Leaf<'_>) {
            if self.found.is_none()
                && let LeafKind::FrontMatter { kind } = leaf.kind
            {
                self.found = Some((kind, leaf.lines.iter().map(|l| l.content.clone()).collect()));
            }
        }
    }
    // Front matter is the first block: look at the start only.
    let mut find = Find { found: None };
    walk_range(tree, 0..1, &mut find);
    let (kind, lines) = find.found?;
    let mut buf = Vec::new();
    let lines: Vec<String> = lines
        .into_iter()
        .map(|r| {
            buf.clear();
            text.copy_range(r, &mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        })
        .collect();
    let toml = kind == FrontMatterKind::Toml;
    let mut tags: Vec<String> = Vec::new();
    let mut in_list = false;
    for line in &lines {
        let trimmed = line.trim();
        if in_list {
            if let Some(item) = trimmed.strip_prefix('-') {
                tags.push(unquote(item.trim()).to_owned());
                continue;
            }
            in_list = false;
        }
        let Some((key, value)) = trimmed.split_once(if toml { '=' } else { ':' }) else {
            continue;
        };
        let key = unquote(key.trim());
        if !(key.eq_ignore_ascii_case("lang") || key.eq_ignore_ascii_case("language"))
            || line.starts_with([' ', '\t'])
        {
            continue;
        }
        let value = value.trim();
        if value.is_empty() {
            in_list = !toml;
            continue;
        }
        let inner = value
            .strip_prefix('[')
            .and_then(|v| v.strip_suffix(']'))
            .unwrap_or(value);
        tags.extend(inner.split(',').map(|v| unquote(v.trim()).to_owned()));
    }
    tags.retain(|t| !t.is_empty());
    (!tags.is_empty()).then_some(tags)
}

fn unquote(value: &str) -> &str {
    for q in ['"', '\''] {
        if let Some(inner) = value.strip_prefix(q).and_then(|v| v.strip_suffix(q)) {
            return inner;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use gv_syntax::{FrontMatterOptions, ParseOptions};

    use super::*;

    fn langs(text: &str) -> Option<Vec<String>> {
        let options = ParseOptions {
            front_matter: FrontMatterOptions {
                yaml: true,
                toml: true,
            },
            ..ParseOptions::default()
        };
        let tree = SyntaxTree::parse_with(text, options);
        document_languages(&tree, text)
    }

    #[test]
    fn tags_normalize() {
        assert_eq!(normalize_tag("en-us").as_deref(), Some("en_US"));
        assert_eq!(normalize_tag(" DE ").as_deref(), Some("de"));
        assert_eq!(normalize_tag("en_GB-ise").as_deref(), Some("en_GB-ise"));
        assert_eq!(normalize_tag("sr-Latn").as_deref(), Some("sr_Latn"));
        assert_eq!(normalize_tag(""), None);
        assert_eq!(normalize_tag("en US"), None);
    }

    #[test]
    fn front_matter_names_the_languages() {
        assert_eq!(langs("---\nlang: de\n---\nText\n"), Some(vec!["de".into()]));
        assert_eq!(
            langs("---\ntitle: x\nlanguage: [en-GB, \"de\"]\n---\n"),
            Some(vec!["en-GB".into(), "de".into()])
        );
        assert_eq!(
            langs("---\nlang:\n  - fr\n  - en\ntags: [a]\n---\n"),
            Some(vec!["fr".into(), "en".into()])
        );
        assert_eq!(langs("+++\nlang = \"ru\"\n+++\n"), Some(vec!["ru".into()]));
        assert_eq!(langs("---\ntitle: lang\n---\n"), None);
        assert_eq!(langs("lang: de\n"), None);
    }
}
