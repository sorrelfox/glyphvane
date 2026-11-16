// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The status bar: where the document is at the left (folder, file, the
//! heading being read), facts about it at the right: in Preview words and
//! reading time; in Edit and Split the caret's line and column, words, the
//! encoding and the line endings ([`EditorFacts`]); the profile chip
//! ([`super::profile_chip`]).

use gpui::{
    AnyElement, Corners, Div, ParentElement as _, Pixels, SharedString, Styled as _, div, px,
};
use gv_profile::ProfileId;
use unicode_segmentation::UnicodeSegmentation as _;

use super::{DocFacts, Tab, ViewMode};
use crate::editor::EditorFacts;
use crate::icons::Icon;
use crate::metrics::spacing::status_bar as spacing;
use crate::metrics::{icon, size, text};
use crate::theme::Theme;

/// Reading speed for "M min read", in words per minute.
const WORDS_PER_MINUTE: usize = 200;

pub(super) fn status_bar(
    tab: &Tab,
    doc: Option<&DocFacts>,
    editor: Option<&EditorFacts>,
    profile_chip: AnyElement,
    theme: &Theme,
    radii: Corners<Pixels>,
) -> Div {
    div()
        .h(px(size::STATUS_BAR))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(spacing::GAP))
        .px(px(spacing::PAD_X))
        .bg(theme.surface.window)
        .border_t_1()
        .border_color(theme.border.default)
        .rounded_bl(radii.bottom_left)
        .rounded_br(radii.bottom_right)
        .text_size(px(text::UI_SMALL.size))
        .line_height(px(text::UI_SMALL.line))
        .text_color(theme.text.secondary)
        .whitespace_nowrap()
        .overflow_hidden()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .flex()
                .items_center()
                .gap(px(spacing::GAP))
                .child(breadcrumb(breadcrumb_items(tab, doc), theme)),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(spacing::GAP))
                .children(facts(tab, doc, editor))
                .child(profile_chip),
        )
}

/// The breadcrumb's items: folder, file and the heading being read.
pub(super) fn breadcrumb_items(tab: &Tab, doc: Option<&DocFacts>) -> Vec<SharedString> {
    let mut items = Vec::with_capacity(3);
    items.extend(tab.label.folder.clone());
    items.push(tab.label.name.clone());
    if let Some(heading) = doc.and_then(DocFacts::heading_text) {
        items.push(heading);
    }
    items
}

/// The texts at the right before the profile chip. Preview: words and
/// reading time. Edit and Split: "Ln L, Col C" (with the characters
/// selected), words, encoding and line endings, from the Source view once
/// it is loaded.
pub(super) fn facts(
    tab: &Tab,
    doc: Option<&DocFacts>,
    editor: Option<&EditorFacts>,
) -> Vec<SharedString> {
    let preview_words = doc.and_then(|d| d.words);
    let mut out = Vec::new();
    match (tab.mode(), editor) {
        (ViewMode::Preview, _) => out.extend(preview_words.map(|n| reading_stats(n).into())),
        (ViewMode::Edit | ViewMode::Split, Some(facts)) => {
            out.push(caret_place(facts).into());
            // Until the editor's first count, Preview's count of the file.
            out.extend(facts.words.or(preview_words).map(|n| word_count(n).into()));
            out.push(facts.encoding.into());
            out.push(facts.line_endings.into());
        }
        (ViewMode::Edit | ViewMode::Split, None) => {
            // Until the editor is loaded, where the file was asked to open.
            out.extend(tab.position().map(|p| {
                SharedString::from(match p.column {
                    Some(column) => format!("Ln {}, Col {column}", p.line),
                    None => format!("Ln {}", p.line),
                })
            }));
            out.extend(preview_words.map(|n| word_count(n).into()));
        }
    }
    out
}

/// "Ln L, Col C", with "(N selected)" while characters are selected.
fn caret_place(facts: &EditorFacts) -> String {
    let place = format!(
        "Ln {}, Col {}",
        group_digits(facts.line),
        group_digits(facts.column)
    );
    match facts.selected {
        0 => place,
        n => format!("{place} ({} selected)", group_digits(n)),
    }
}

/// Folder, file and heading separated by chevrons; the last item in
/// `text.primary`.
fn breadcrumb(items: Vec<SharedString>, theme: &Theme) -> Div {
    let last = items.len().saturating_sub(1);
    div()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(spacing::CRUMB_GAP))
        .children(items.into_iter().enumerate().flat_map(|(ix, item)| {
            let chevron = (ix > 0).then(|| {
                div()
                    .flex_none()
                    .child(Icon::ChevronRight.sized(icon::TINY))
            });
            let label = div().min_w_0().truncate().child(item);
            let label = if ix == last {
                label.text_color(theme.text.primary)
            } else {
                label.flex_shrink_0()
            };
            chevron.into_iter().chain([label])
        }))
}

/// A profile as the interface names it.
pub(super) fn profile_name(profile: ProfileId) -> &'static str {
    match profile {
        ProfileId::CommonMark => "CommonMark",
        ProfileId::GfmFile => "GitHub",
        ProfileId::GfmComment => "GitHub comment",
        ProfileId::Obsidian => "Obsidian",
        ProfileId::Extended => "Extended",
    }
}

/// "N words · M min read", reading at [`WORDS_PER_MINUTE`].
pub(super) fn reading_stats(words: usize) -> String {
    let minutes = words.div_ceil(WORDS_PER_MINUTE);
    format!("{} · {} min read", word_count(words), group_digits(minutes))
}

fn word_count(words: usize) -> String {
    let unit = if words == 1 { "word" } else { "words" };
    format!("{} {unit}", group_digits(words))
}

/// `1234567` as `1,234,567`.
fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (ix, ch) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Words by UAX #29 word boundaries; Chinese and Japanese text counts one
/// word per character (it has no spaces between words).
pub(crate) fn count_words(text: &str) -> usize {
    text.unicode_words()
        .map(|word| match word.chars().filter(|&c| is_cjk(c)).count() {
            0 => 1,
            cjk => cjk + usize::from(word.chars().any(|c| !is_cjk(c))),
        })
        .sum()
}

/// Han ideographs, Hiragana and Katakana.
fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{3040}'..='\u{30FF}'
            | '\u{31F0}'..='\u{31FF}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FF66}'..='\u{FF9D}'
            | '\u{20000}'..='\u{2FA1F}'
            | '\u{30000}'..='\u{323AF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_follow_uax29_and_count_cjk_per_character() {
        assert_eq!(count_words(""), 0);
        assert_eq!(count_words("Hello, world!"), 2);
        assert_eq!(count_words("don't stop — 3.14 is a number"), 6);
        assert_eq!(count_words("  - list   item\n\nnext"), 3);
        assert_eq!(count_words("日本語のテキスト"), 8);
        assert_eq!(count_words("Rust 很好"), 3);
        assert_eq!(count_words("한국어 문장"), 2);
    }

    #[test]
    fn reading_time_rounds_up_at_200_words_a_minute() {
        assert_eq!(reading_stats(0), "0 words · 0 min read");
        assert_eq!(reading_stats(1), "1 word · 1 min read");
        assert_eq!(reading_stats(200), "200 words · 1 min read");
        assert_eq!(reading_stats(486), "486 words · 3 min read");
        assert_eq!(reading_stats(1_234_567), "1,234,567 words · 6,173 min read");
    }

    #[test]
    fn digits_group_by_thousands() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(12_345), "12,345");
    }

    #[test]
    fn the_caret_place_shows_the_selection() {
        let mut facts = EditorFacts {
            line: 12,
            column: 1_005,
            selected: 0,
            words: Some(3),
            encoding: "UTF-8",
            line_endings: "LF",
        };
        assert_eq!(caret_place(&facts), "Ln 12, Col 1,005");
        facts.selected = 42;
        assert_eq!(caret_place(&facts), "Ln 12, Col 1,005 (42 selected)");
    }

    #[test]
    fn profiles_have_interface_names() {
        assert_eq!(profile_name(ProfileId::CommonMark), "CommonMark");
    }
}
