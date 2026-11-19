// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `:shortcode:` emoji (gemoji names), for the profiles that have them
//! (the `emoji` switch): the views draw the emoji in place of the code.

use std::ops::Range;

/// The `:shortcode:` emoji in `text`: each code's byte range (colons
/// included) and its emoji. A code is lowercase ASCII letters, digits and
/// `_`, `+`, `-`, and must name an emoji.
pub fn emoji_shortcodes(text: &str) -> Vec<(Range<usize>, &'static str)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(open) = bytes[i..].iter().position(|&b| b == b':').map(|p| p + i) {
        let code_len = bytes[open + 1..]
            .iter()
            .take_while(|&&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_+-".contains(&b))
            .count();
        let close = open + 1 + code_len;
        if code_len > 0
            && bytes.get(close) == Some(&b':')
            && let Some(emoji) = emojis::get_by_shortcode(&text[open + 1..close])
        {
            out.push((open..close + 1, emoji.as_str()));
            i = close + 1;
            continue;
        }
        i = open + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_that_name_an_emoji() {
        assert_eq!(
            emoji_shortcodes("a :smile: b :tada::+1:"),
            [(2..9, "😄"), (12..18, "🎉"), (18..22, "👍")]
        );
        // Not a code: unknown names, capitals, spaces, an empty name.
        assert!(emoji_shortcodes(":nope_not_one: :Smile: : smile: ::").is_empty());
        // A colon that ends nothing starts the next code.
        assert_eq!(emoji_shortcodes("12:30 :smile:"), [(6..13, "😄")]);
    }
}
