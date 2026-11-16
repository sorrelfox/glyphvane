// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Sizes of the settings window (SPEC § Settings window and § Controls), in
//! logical pixels. The shared sizes (control height, row heights, sidebar
//! width, switch, segments) are in [`crate::metrics`].

/// The window: 880 × 660 by default.
pub mod window {
    pub const WIDTH: f32 = 880.0;
    pub const HEIGHT: f32 = 660.0;
    /// The smallest size it can be made (not designed).
    pub const MIN_WIDTH: f32 = 560.0;
    pub const MIN_HEIGHT: f32 = 400.0;
}

/// The sidebar: window controls in a 36 row, the search field (26 high,
/// margin 0 8 6, padding 0 7, gap 6), section rows 24 high (margin 0 6,
/// padding 0 8, gap 7, 14 icons), and the 30 high `settings.toml` row
/// (padding 0 14, gap 7, 13 and 11 icons, 11.5 text, the file name in mono
/// 11).
pub mod sidebar {
    pub const CONTROLS_ROW: f32 = 36.0;
    pub const SEARCH_MARGIN_X: f32 = 8.0;
    pub const SEARCH_MARGIN_BOTTOM: f32 = 6.0;
    pub const SEARCH_PAD_X: f32 = 7.0;
    pub const SEARCH_GAP: f32 = 6.0;
    pub const SEARCH_ICON: f32 = 13.0;
    pub const ROW_MARGIN_X: f32 = 6.0;
    pub const ROW_PAD_X: f32 = 8.0;
    pub const ROW_GAP: f32 = 7.0;
    pub const FILE_ROW: f32 = 30.0;
    pub const FILE_ROW_PAD_X: f32 = 14.0;
    pub const FILE_ICON: f32 = 13.0;
    pub const FILE_ARROW: f32 = 11.0;
    pub const FILE_ROW_TEXT: f32 = 11.5;
    pub const FILE_NAME_TEXT: f32 = 11.0;
    /// The close button of a client-decorated window on Linux (not
    /// designed: the round control of the main window's title row).
    pub const CLOSE_INSET: f32 = 8.0;
}

/// The head: 36 high, padding 5 12 5 24, gap 4 12.
pub mod head {
    pub const HEIGHT: f32 = 36.0;
    pub const PAD_Y: f32 = 5.0;
    pub const PAD_RIGHT: f32 = 12.0;
    pub const PAD_LEFT: f32 = 24.0;
    pub const GAP_X: f32 = 12.0;
}

/// The content: padding 10 24.
pub mod content {
    pub const PAD_TOP: f32 = 10.0;
    pub const PAD_X: f32 = 24.0;
    pub const PAD_BOTTOM: f32 = 24.0;
}

/// Rows and group headings.
pub mod row {
    pub const PAD_Y: f32 = 5.0;
    pub const GAP_X: f32 = 16.0;
    pub const GAP_Y: f32 = 4.0;
    /// Rows with several controls (font and size) put 8 between them.
    pub const CONTROL_GAP: f32 = 8.0;
    pub const LABEL_BASIS: f32 = 220.0;
    pub const HEADING: f32 = 24.0;
    pub const HEADING_GAP: f32 = 10.0;
    pub const SEGMENT_PAD_X: f32 = 10.0;
    /// The sentence under the theme cards: 8 above and below.
    pub const NOTE_MARGIN_Y: f32 = 8.0;
}

/// Selects: at least 168 wide, padding 0 6 0 9, gap 8, 11 chevrons. The
/// list (not designed) is at most 240 high with 4 padding.
pub mod select {
    pub const MIN_WIDTH: f32 = 168.0;
    pub const PAD_LEFT: f32 = 9.0;
    pub const PAD_RIGHT: f32 = 6.0;
    pub const GAP: f32 = 8.0;
    pub const ICON: f32 = 11.0;
    pub const LIST_MAX_HEIGHT: f32 = 240.0;
    pub const LIST_PAD: f32 = 4.0;
    /// The list's distance below the button.
    pub const LIST_OFFSET: f32 = 4.0;
}

/// Text and number fields: padding 0 7, number fields 62 wide with the value
/// in mono 11.5, a 3 gap before the unit.
pub mod field {
    pub const PAD_X: f32 = 7.0;
    pub const UNIT_GAP: f32 = 3.0;
    pub const NUMBER_WIDTH: f32 = 62.0;
    pub const NUMBER_TEXT: f32 = 11.5;
}

/// The stepper: 26 wide buttons with 12 icons around a 52 wide value.
pub mod stepper {
    pub const BUTTON: f32 = 26.0;
    pub const VALUE: f32 = 52.0;
    pub const VALUE_TEXT: f32 = 11.5;
    pub const ICON: f32 = 12.0;
}

/// The theme picker: four cards with a 12 gap, at least 120 wide; a
/// preview 84 high with radius 7, then the name row (gap 6, 12 dot).
pub mod theme_card {
    pub const GAP: f32 = 12.0;
    pub const BASIS: f32 = 120.0;
    pub const PREVIEW_RADIUS: f32 = 7.0;
    pub const NAME_GAP: f32 = 6.0;
    pub const DOT: f32 = 12.0;
    pub const DOT_RING_SELECTED: f32 = 4.0;
    pub const RING_SELECTED: f32 = 2.0;
    /// The tiny window inside the preview.
    pub const TITLE_BAR: f32 = 10.0;
    pub const SIDEBAR: f32 = 24.0;
    pub const LINES_GAP: f32 = 6.0;
    pub const LINES_PAD_TOP: f32 = 10.0;
    pub const LINES_PAD_SIDE: f32 = 9.0;
    pub const LINES_PAD_RIGHT: f32 = 10.0;
    pub const HEADING_LINE: f32 = 5.0;
    pub const TEXT_LINE: f32 = 3.0;
    pub const LINE_RADIUS: f32 = 2.0;
    /// Widths of the short lines, as fractions of the column.
    pub const HEADING_FRACTION: f32 = 0.78;
    pub const LINK_FRACTION: f32 = 0.56;
    pub const SHORT_FRACTION: f32 = 0.62;
}
