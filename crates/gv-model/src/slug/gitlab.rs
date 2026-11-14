// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! GitLab heading anchors. GitLab renders Markdown with comrak and its
//! `header_ids` option; `Gitlab::HeadingSlug.from_text` mirrors comrak's
//! anchorizer for other markups (gitlab-org/gitlab `lib/gitlab/
//! heading_slug.rb`, checked 2026-11-13):
//!
//! ```ruby
//! text.downcase.gsub(/[^\p{L}\p{M}\p{N}\p{Pc} -]/, '').tr(' ', '-')
//! ```
//!
//! No runs are collapsed (GitLab's documentation still describes an older
//! filter that squeezed `-`; its own examples show `-----`). The heading
//! text is comrak's `collect_text`: text, code and image descriptions,
//! line breaks as spaces.

use super::chars::{is_connector, is_letter, is_mark, is_number};

/// The GitLab base slug.
pub(super) fn slug(text: &str) -> String {
    let lower = text.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    for c in lower.chars() {
        if c == ' ' {
            out.push('-');
        } else if c == '-' || is_letter(c) || is_mark(c) || is_number(c) || is_connector(c) {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::slug;

    #[test]
    fn documented_examples() {
        // GitLab's Markdown documentation, "Heading IDs and links".
        assert_eq!(
            slug("This heading has spaces in it"),
            "this-heading-has-spaces-in-it"
        );
        assert_eq!(
            slug("This heading has Unicode in it: 한글"),
            "this-heading-has-unicode-in-it-한글"
        );
        assert_eq!(
            slug("This heading has 3.5 in it (and parentheses)"),
            "this-heading-has-35-in-it-and-parentheses"
        );
        assert_eq!(
            slug("This heading has  multiple spaces and --- hyphens"),
            "this-heading-has--multiple-spaces-and-----hyphens"
        );
        assert_eq!(slug("x²"), "x²");
    }
}
