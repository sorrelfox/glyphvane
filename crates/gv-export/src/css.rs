// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The exported stylesheet: the light theme's tokens as CSS custom
//! properties, and the rules that use them. Exports are always light and
//! print-friendly, whatever theme the app shows.

use std::fmt::Write as _;

use gv_highlight::TokenClass;

/// The design tokens the stylesheet uses, by name.
pub const THEME_TOKENS: &[&str] = &[
    "text.primary",
    "text.secondary",
    "text.tertiary",
    "accent.default",
    "accent.tint",
    "surface.page",
    "surface.sunken",
    "border.default",
    "border.strong",
    "status.error",
    "status.error_tint",
    "status.warning",
    "status.warning_tint",
    "status.success",
    "status.success_tint",
    "status.info",
    "status.info_tint",
    "syntax.key",
    "syntax.string",
    "syntax.number",
    "syntax.tag",
    "syntax.url",
    "syntax.code",
];

/// The colours of the theme an export is styled with (the light theme in
/// the app): one RGBA colour per token of [`THEME_TOKENS`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExportTheme {
    colors: Vec<(&'static str, [u8; 4])>,
}

impl ExportTheme {
    /// The theme whose token `name` has colour `color(name)`; tokens it
    /// has no colour for are left to the browser's defaults.
    pub fn from_fn(mut color: impl FnMut(&str) -> Option<[u8; 4]>) -> Self {
        Self {
            colors: THEME_TOKENS
                .iter()
                .filter_map(|name| color(name).map(|c| (*name, c)))
                .collect(),
        }
    }

    /// The colour of token `name`.
    pub fn color(&self, name: &str) -> Option<[u8; 4]> {
        self.colors
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, c)| *c)
    }
}

/// `text.primary` → `--text-primary`.
fn var_name(token: &str) -> String {
    format!("--{}", token.replace(['.', '_'], "-"))
}

fn css_color([r, g, b, a]: [u8; 4]) -> String {
    if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

/// The rules of an exported document. Fixed text, so the output is the
/// same for the same theme.
const RULES: &str = r#"*, *::before, *::after { box-sizing: border-box; }
html { color-scheme: light; }
body { margin: 0; background: var(--surface-page); color: var(--text-primary); font: 16px/1.6 "Inter", "Noto Sans", "Segoe UI", system-ui, -apple-system, sans-serif; }
.gv-document { max-width: 46rem; margin: 0 auto; padding: 2.5rem 1.5rem 4rem; overflow-wrap: break-word; }
h1, h2, h3, h4, h5, h6 { line-height: 1.25; margin: 1.6em 0 0.6em; }
h1 { font-size: 1.75em; letter-spacing: -0.02em; }
h2 { font-size: 1.35em; letter-spacing: -0.01em; }
h3 { font-size: 1.15em; }
h4, h5, h6 { font-size: 1em; }
h6 { color: var(--text-secondary); }
p, ul, ol, dl, table, pre, blockquote, details, .gv-callout, .gv-diagram, .gv-math-block { margin: 0 0 1em; }
a { color: var(--accent-default); text-decoration: underline; text-underline-offset: 0.15em; }
.gv-unresolved { color: var(--status-error); text-decoration: underline dotted; }
code, kbd, samp, pre { font-family: "JetBrains Mono", "Cascadia Mono", "Menlo", "Consolas", "DejaVu Sans Mono", monospace; font-size: 0.875em; }
:not(pre) > code { background: var(--surface-sunken); border-radius: 4px; padding: 0.1em 0.3em; }
kbd { border: 1px solid var(--border-strong); border-radius: 4px; padding: 0 0.3em; background: var(--surface-sunken); }
pre { background: var(--surface-sunken); border: 1px solid var(--border-default); border-radius: 6px; padding: 0.75em 1em; overflow-x: auto; line-height: 1.5; }
pre code { font-size: inherit; }
blockquote { margin-left: 0; padding: 0 1em; border-left: 3px solid var(--border-strong); color: var(--text-secondary); }
hr { border: 0; border-top: 1px solid var(--border-default); margin: 2em 0; }
table { border-collapse: collapse; display: block; overflow-x: auto; max-width: 100%; }
th, td { border: 1px solid var(--border-default); padding: 0.35em 0.75em; }
th { background: var(--surface-sunken); font-weight: 600; }
img { max-width: 100%; height: auto; }
mark { background: var(--status-warning-tint); color: inherit; }
.gv-tag { background: var(--accent-tint); color: var(--accent-default); border-radius: 999px; padding: 0 0.45em; font-size: 0.875em; }
ul.gv-tasklist, li.gv-task { list-style: none; }
li.gv-task > input { margin: 0 0.4em 0 -1.3em; vertical-align: middle; }
.gv-callout { border-left: 4px solid var(--accent-default); padding: 0.5em 1em; border-radius: 4px; background: var(--accent-tint); }
.gv-callout > :last-child { margin-bottom: 0; }
.gv-callout-title { font-weight: 600; margin-bottom: 0.4em; }
.gv-callout-note { border-color: var(--status-info); background: var(--status-info-tint); }
.gv-callout-tip { border-color: var(--status-success); background: var(--status-success-tint); }
.gv-callout-important { border-color: var(--accent-default); background: var(--accent-tint); }
.gv-callout-warning { border-color: var(--status-warning); background: var(--status-warning-tint); }
.gv-callout-caution { border-color: var(--status-error); background: var(--status-error-tint); }
.gv-callout-info, .gv-callout-todo { border-color: var(--status-info); background: var(--status-info-tint); }
.gv-callout-abstract, .gv-callout-success { border-color: var(--status-success); background: var(--status-success-tint); }
.gv-callout-question { border-color: var(--status-warning); background: var(--status-warning-tint); }
.gv-callout-failure, .gv-callout-danger, .gv-callout-bug { border-color: var(--status-error); background: var(--status-error-tint); }
.gv-callout-example { border-color: var(--syntax-tag); background: var(--accent-tint); }
.gv-callout-quote { border-color: var(--text-secondary); background: var(--surface-sunken); }
.gv-embed { border-left: 2px solid var(--border-strong); padding-left: 1em; margin: 0 0 1em; }
.gv-callout-note > .gv-callout-title { color: var(--status-info); }
.gv-callout-tip > .gv-callout-title { color: var(--status-success); }
.gv-callout-important > .gv-callout-title { color: var(--accent-default); }
.gv-callout-warning > .gv-callout-title { color: var(--status-warning); }
.gv-callout-caution > .gv-callout-title { color: var(--status-error); }
.gv-math-block { text-align: center; overflow-x: auto; }
.gv-math-error { color: var(--status-error); }
.gv-diagram { text-align: center; overflow-x: auto; }
.gv-diagram svg { max-width: 100%; height: auto; }
.gv-properties { font-size: 0.875em; }
.gv-properties th { text-align: left; }
.gv-footnotes { font-size: 0.875em; color: var(--text-secondary); }
.gv-fnref { line-height: 0; }
.gv-toc { border-left: 2px solid var(--border-default); padding-left: 1em; margin-bottom: 2em; }
.gv-toc ul { list-style: none; padding-left: 1em; margin: 0; }
.gv-toc > ul { padding-left: 0; }
details > summary { cursor: pointer; }
@media print {
  body { background: none; font-size: 11pt; }
  .gv-document { max-width: none; padding: 0; }
  pre, table, .gv-callout, .gv-diagram, .gv-math-block, img { break-inside: avoid; }
  h1, h2, h3, h4, h5, h6 { break-after: avoid; }
  a { color: inherit; }
}
"#;

/// The stylesheet for `theme`.
pub(crate) fn stylesheet(theme: &ExportTheme) -> String {
    let mut css = String::from(":root {\n");
    for (name, color) in &theme.colors {
        let _ = writeln!(css, "  {}: {};", var_name(name), css_color(*color));
    }
    css.push_str("}\n");
    css.push_str(RULES);
    for class in TokenClass::ALL {
        if class == TokenClass::Plain {
            continue;
        }
        let style = class.font_style();
        let _ = write!(
            css,
            ".hl-{} {{ color: var({});",
            class.name(),
            var_name(class.token())
        );
        if style.bold {
            css.push_str(" font-weight: 600;");
        }
        if style.italic {
            css.push_str(" font-style: italic;");
        }
        css.push_str(" }\n");
    }
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_become_custom_properties() {
        let theme = ExportTheme::from_fn(|name| (name == "text.primary").then_some([1, 2, 3, 255]));
        let css = stylesheet(&theme);
        assert!(css.starts_with(":root {\n  --text-primary: #010203;\n}\n"));
        assert!(css.contains(".hl-keyword { color: var(--syntax-tag); }"));
        assert!(css.contains(".hl-comment { color: var(--text-tertiary); font-style: italic; }"));
        assert_eq!(var_name("status.error_tint"), "--status-error-tint");
        assert_eq!(css_color([0, 0, 0, 128]), "#00000080");
    }

    #[test]
    fn every_class_token_is_a_theme_token() {
        for class in TokenClass::ALL {
            assert!(THEME_TOKENS.contains(&class.token()), "{}", class.token());
        }
    }
}
