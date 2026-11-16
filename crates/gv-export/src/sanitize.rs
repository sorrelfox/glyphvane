// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The export sanitizer: ammonia with the Reading view's allowlist, and an
//! attribute filter that runs every URL through the shared classifier.
//!
//! - **Elements** outside the allowlist are unwrapped (their children
//!   stay); the dangerous ones go with their content: scripts, styles,
//!   frames, objects, form fields, inline SVG and the like. Inline SVG
//!   and iframes have no export rendering yet.
//! - **Attributes**: the plan's global and per-element lists. `on*`
//!   handlers, `srcdoc`, `formaction` and everything unlisted are dropped.
//!   `style` keeps only the validated subset (none in GitHub profiles).
//! - **Classes**: only the exporter's own (`gv-*`, `hl-*`).
//! - **Links** (`href`, `cite`): fragments and relative paths (rewritten,
//!   see [`rewrite_link`]), `http(s)` and `mailto`. Anything that would
//!   need the user's confirmation in the app (network paths, `file:`,
//!   other schemes) loses its target, because an export has no
//!   confirmation; script and data URLs too. Links get
//!   `rel="noopener noreferrer"`.
//! - **Media** (`src`, `poster`, `srcset` candidates): `https` and `http`
//!   references stay as they are (never fetched), allowed `data:` images
//!   stay, relative files are embedded (see [`crate::assets`]); anything
//!   else is dropped, LAN hosts included.
//! - **Ids** get [`ID_PREFIX`], and fragment links follow, so a document
//!   cannot clobber the page's own names.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gv_vault::url::{UrlClass, UrlContext, UrlPolicy, classify, parse_srcset};

use crate::ExportOptions;
use crate::assets::Store;

/// The prefix of every id in an export.
pub const ID_PREFIX: &str = "user-content-";

/// The page's Content Security Policy: no script, no remote styles or
/// fonts, images only inline or over HTTPS.
pub(crate) const CSP: &str =
    "default-src 'none'; img-src data: https:; style-src 'unsafe-inline'; font-src data:";

const TAGS: &[&str] = &[
    "p",
    "div",
    "span",
    "center",
    "section",
    "article",
    "aside",
    "header",
    "footer",
    "main",
    "nav",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "dl",
    "dt",
    "dd",
    "blockquote",
    "pre",
    "code",
    "kbd",
    "samp",
    "var",
    "tt",
    "b",
    "strong",
    "i",
    "em",
    "u",
    "s",
    "strike",
    "del",
    "ins",
    "mark",
    "small",
    "sub",
    "sup",
    "q",
    "cite",
    "dfn",
    "abbr",
    "font",
    "br",
    "hr",
    "wbr",
    "a",
    "img",
    "picture",
    "source",
    "details",
    "summary",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "th",
    "td",
    "caption",
    "colgroup",
    "col",
    "figure",
    "figcaption",
    "ruby",
    "rt",
    "rp",
    "audio",
    "video",
];

/// Removed together with their content.
const REMOVED: &[&str] = &[
    "script", "style", "template", "noscript", "iframe", "object", "embed", "applet", "input",
    "select", "textarea", "link", "meta", "base", "frame", "frameset", "title", "param", "dialog",
    "canvas", "map", "area", "track", "slot", "svg",
];

const TAG_ATTRIBUTES: &[(&str, &[&str])] = &[
    ("a", &["href", "name"]),
    ("img", &["src", "alt", "width", "height", "srcset"]),
    ("source", &["src", "srcset", "media", "type"]),
    ("audio", &["src", "controls"]),
    ("video", &["src", "poster", "controls"]),
    ("td", &["colspan", "rowspan", "width", "valign"]),
    ("th", &["colspan", "rowspan", "width", "valign"]),
    ("tr", &["valign"]),
    ("table", &["width"]),
    ("ol", &["start", "type"]),
    ("details", &["open"]),
    ("font", &["color"]),
    ("abbr", &["title"]),
    ("col", &["span"]),
    ("blockquote", &["cite"]),
    ("q", &["cite"]),
];

/// The exporter's own classes, by element.
const CLASSES: &[(&str, &[&str])] = &[
    (
        "div",
        &[
            "gv-callout",
            "gv-callout-note",
            "gv-callout-tip",
            "gv-callout-important",
            "gv-callout-warning",
            "gv-callout-caution",
            // Obsidian callout types.
            "gv-callout-abstract",
            "gv-callout-info",
            "gv-callout-todo",
            "gv-callout-success",
            "gv-callout-question",
            "gv-callout-failure",
            "gv-callout-danger",
            "gv-callout-bug",
            "gv-callout-example",
            "gv-callout-quote",
            "gv-embed",
            "gv-math-block",
            "gv-diagram",
        ],
    ),
    ("p", &["gv-callout-title"]),
    (
        "span",
        &[
            "gv-unresolved",
            "gv-tag",
            "gv-math",
            "gv-math-error",
            "gv-image-alt",
            "hl-comment",
            "hl-keyword",
            "hl-operator",
            "hl-string",
            "hl-escape",
            "hl-regex",
            "hl-number",
            "hl-constant",
            "hl-function",
            "hl-type",
            "hl-variable",
            "hl-property",
            "hl-tag",
            "hl-attribute",
            "hl-punctuation",
            "hl-heading",
            "hl-emphasis",
            "hl-strong",
            "hl-link",
            "hl-code",
            "hl-inserted",
            "hl-deleted",
            "hl-changed",
        ],
    ),
    (
        "pre",
        &[
            "gv-code",
            "gv-diagram-source",
            "gv-math-error",
            "gv-html-source",
        ],
    ),
    ("ul", &["gv-tasklist"]),
    ("li", &["gv-task"]),
    ("section", &["gv-footnotes"]),
    ("sup", &["gv-fnref"]),
    ("a", &["gv-backref"]),
    ("table", &["gv-properties"]),
];

/// `style` properties kept (outside GitHub profiles).
const STYLE_PROPERTIES: &[&str] = &[
    "color",
    "background-color",
    "font-weight",
    "font-style",
    "text-decoration",
    "text-align",
    "font-size",
    "width",
    "height",
    "display",
    "vertical-align",
];

/// Sanitizes the parts of a body one by one (see
/// [`Body::parts`](crate::emit::Body::parts)), embedding images into
/// `store`.
pub(crate) fn clean(parts: &[&str], opts: &ExportOptions, store: &Arc<Store>) -> String {
    let tag_attributes: HashMap<&str, HashSet<&str>> = TAG_ATTRIBUTES
        .iter()
        .map(|(tag, attrs)| (*tag, attrs.iter().copied().collect()))
        .collect();
    let classes: HashMap<&str, HashSet<&str>> = CLASSES
        .iter()
        .map(|(tag, classes)| (*tag, classes.iter().copied().collect()))
        .collect();
    let mut generic: HashSet<&str> = ["id", "title", "lang", "dir", "align"].into();
    if !opts.strip_style {
        generic.insert("style");
    }
    let store = Arc::clone(store);
    let mut builder = ammonia::Builder::empty();
    builder
        .tags(TAGS.iter().copied().collect())
        .clean_content_tags(REMOVED.iter().copied().collect())
        .tag_attributes(tag_attributes)
        .generic_attributes(generic)
        .generic_attribute_prefixes(["aria-"].into())
        .allowed_classes(classes)
        .url_schemes(["http", "https", "mailto", "data"].into())
        .url_relative(ammonia::UrlRelative::PassThrough)
        .link_rel(Some("noopener noreferrer"))
        .strip_comments(true)
        .id_prefix(Some(ID_PREFIX))
        .attribute_filter(move |element, attribute, value| {
            filter(element, attribute, value, &store)
        });
    let mut out = String::new();
    for part in parts {
        out.push_str(&builder.clean(part).to_string());
    }
    out
}

fn filter<'u>(
    element: &str,
    attribute: &str,
    value: &'u str,
    store: &Store,
) -> Option<Cow<'u, str>> {
    match attribute {
        "href" | "cite" => rewrite_link(value).map(Cow::Owned),
        "src" | "poster" => media(value, store).map(Cow::Owned),
        "srcset" => {
            let kept: Vec<String> = parse_srcset(value)
                .into_iter()
                .filter_map(|(url, desc)| {
                    // Candidates are only kept as references; relative
                    // ones would need one file each.
                    let class = classify(&url);
                    let keep = matches!(
                        class,
                        UrlClass::Web { lan: false, .. } | UrlClass::DataImage { .. }
                    ) && class.policy(UrlContext::Media) == UrlPolicy::Allow;
                    keep.then(|| {
                        if desc.is_empty() {
                            url
                        } else {
                            format!("{url} {desc}")
                        }
                    })
                })
                .collect();
            (!kept.is_empty()).then(|| Cow::Owned(kept.join(", ")))
        }
        "style" => style(element, value).map(Cow::Owned),
        "id" => Some(Cow::Borrowed(value)),
        _ => Some(Cow::Borrowed(value)),
    }
}

/// What an export makes of link target `href` (a Markdown destination or
/// an `href`): fragments get the id prefix, relative links to Markdown
/// files point to their `.html` (spaces encoded), `http(s)` and `mailto`
/// stay; `None` for everything else, which becomes plain text.
pub fn rewrite_link(href: &str) -> Option<String> {
    let class = classify(href);
    if class.policy(UrlContext::Link) != UrlPolicy::Allow {
        return None;
    }
    match class {
        UrlClass::Fragment(id) => Some(format!("#{}", prefixed(&id))),
        UrlClass::Relative(path) => Some(relative_link(&path)),
        UrlClass::Web { .. } | UrlClass::Mailto(_) => Some(href.trim().to_owned()),
        _ => None,
    }
}

fn prefixed(id: &str) -> String {
    if id.is_empty() || id.starts_with(ID_PREFIX) {
        id.to_owned()
    } else {
        format!("{ID_PREFIX}{id}")
    }
}

/// A relative link: `.md` and `.markdown` files become `.html`, the
/// fragment gets the id prefix, spaces are encoded.
fn relative_link(path: &str) -> String {
    let (main, fragment) = match path.split_once('#') {
        Some((m, f)) => (m, Some(f)),
        None => (path, None),
    };
    let (file, query) = match main.split_once('?') {
        Some((f, q)) => (f, Some(q)),
        None => (main, None),
    };
    let lower = file.to_ascii_lowercase();
    let mut out = if let Some(stem) = [".md", ".markdown"]
        .iter()
        .find_map(|ext| {
            lower
                .ends_with(ext)
                .then(|| &file[..file.len() - ext.len()])
        })
        .filter(|stem| !stem.is_empty() && !stem.ends_with('/'))
    {
        format!("{stem}.html")
    } else {
        file.to_owned()
    };
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(&prefixed(f));
    }
    out.replace(' ', "%20")
}

fn media(value: &str, store: &Store) -> Option<String> {
    let class = classify(value);
    if class.policy(UrlContext::Media) != UrlPolicy::Allow {
        return None;
    }
    match class {
        UrlClass::Web { .. } | UrlClass::DataImage { .. } => Some(value.trim().to_owned()),
        UrlClass::Relative(path) => store.image(&path),
        _ => None,
    }
}

/// The allowed part of a `style` value: declarations of the allowed
/// properties with plain values (`url()`, `expression()`, `@import`,
/// escapes and comments rejected); `font-size` within 50–300 %;
/// `width` and `height` on images and tables only; `display` only
/// `none`.
fn style(element: &str, value: &str) -> Option<String> {
    let mut kept = Vec::new();
    for decl in value.split(';') {
        let Some((prop, val)) = decl.split_once(':') else {
            continue;
        };
        let prop = prop.trim().to_ascii_lowercase();
        let val = val.trim();
        let lower = val.to_ascii_lowercase();
        if !STYLE_PROPERTIES.contains(&prop.as_str())
            || val.is_empty()
            || lower.contains("url")
            || lower.contains("expression")
            || lower.contains('@')
            || lower.contains("/*")
            || !val
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || " #%.,()-+!".contains(c))
        {
            continue;
        }
        let ok = match prop.as_str() {
            "font-size" => font_size_ok(&lower),
            "width" | "height" => matches!(element, "img" | "table"),
            "display" => lower == "none",
            _ => true,
        };
        if ok {
            kept.push(format!("{prop}: {val}"));
        }
    }
    (!kept.is_empty()).then(|| kept.join("; "))
}

fn font_size_ok(v: &str) -> bool {
    let number = |s: &str| s.trim().parse::<f32>().ok();
    if let Some(p) = v.strip_suffix('%').and_then(number) {
        (50.0..=300.0).contains(&p)
    } else if let Some(e) = v.strip_suffix("em").and_then(number) {
        (0.5..=3.0).contains(&e)
    } else {
        matches!(
            v,
            "smaller" | "larger" | "small" | "medium" | "large" | "x-large" | "x-small"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_rewritten() {
        assert_eq!(
            rewrite_link("#Intro").as_deref(),
            Some("#user-content-Intro")
        );
        assert_eq!(
            rewrite_link("#user-content-x").as_deref(),
            Some("#user-content-x")
        );
        assert_eq!(
            rewrite_link("notes/a b.md#h").as_deref(),
            Some("notes/a%20b.html#user-content-h")
        );
        assert_eq!(rewrite_link("README.MD").as_deref(), Some("README.html"));
        assert_eq!(
            rewrite_link("doc.markdown?x=1").as_deref(),
            Some("doc.html?x=1")
        );
        assert_eq!(rewrite_link("image.png").as_deref(), Some("image.png"));
        assert_eq!(rewrite_link(".md").as_deref(), Some(".md"));
        assert_eq!(
            rewrite_link("https://example.com/a.md").as_deref(),
            Some("https://example.com/a.md")
        );
        assert_eq!(
            rewrite_link("mailto:a@b.c").as_deref(),
            Some("mailto:a@b.c")
        );
        for blocked in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            // Values arrive with character references decoded.
            "jav\tascript:alert(1)",
            " java\nscript:alert(1)",
            "vbscript:x",
            "data:text/html,<script>",
            "\\\\host\\share",
            "file://host/share",
            "file:////host/share",
            "%5C%5Chost/x",
            "smb://host/x",
            "ms-msdt:x",
            "tel:123",
            "obsidian://open",
        ] {
            assert_eq!(rewrite_link(blocked), None, "{blocked}");
        }
    }

    #[test]
    fn styles_keep_the_subset() {
        assert_eq!(
            style("span", "color: red; position: fixed").as_deref(),
            Some("color: red")
        );
        assert_eq!(style("span", "background-color: url(x)"), None);
        assert_eq!(style("span", "color: expression(alert(1))"), None);
        assert_eq!(style("span", "font-size: 1000%"), None);
        assert_eq!(
            style("span", "font-size: 120%").as_deref(),
            Some("font-size: 120%")
        );
        assert_eq!(style("span", "width: 10px"), None);
        assert_eq!(style("img", "width: 10px").as_deref(), Some("width: 10px"));
        assert_eq!(style("div", "display: block"), None);
        assert_eq!(
            style("div", "display: none").as_deref(),
            Some("display: none")
        );
        assert_eq!(style("div", "color: \\72 ed"), None);
    }
}
