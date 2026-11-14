// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Sizes of the design, in logical pixels at 100% zoom.
//!
//! These are the design, not suggestions: compactness is a requirement, so do
//! not round them up. View code takes every size from here instead of writing
//! numbers inline. A size that is missing belongs in the design tokens first.

/// Fixed regions of the window and of its components.
pub mod size {
    /// Default size of a main window.
    pub const MAIN_WINDOW_WIDTH: f32 = 1280.0;
    pub const MAIN_WINDOW_HEIGHT: f32 = 800.0;
    pub const TITLE_ROW: f32 = 36.0;
    pub const STATUS_BAR: f32 = 24.0;
    pub const SIDEBAR_WIDTH: f32 = 232.0;
    pub const PANEL_WIDTH: f32 = 248.0;
    pub const SETTINGS_SIDEBAR_WIDTH: f32 = 208.0;
    pub const PANEL_HEADER: f32 = 28.0;
    pub const LIST_ROW: f32 = 24.0;
    pub const LIST_ROW_INSET: f32 = 6.0;
    pub const LIST_INDENT: f32 = 14.0;
    pub const OUTLINE_ROW: f32 = 22.0;
    pub const OUTLINE_INDENT: f32 = 12.0;
    pub const SMALL_ICON_BUTTON: f32 = 22.0;
    pub const CONTROL: f32 = 24.0;
    pub const FIELD: f32 = 26.0;
    pub const INFOBAR: f32 = 30.0;
    pub const MENU_ROW: f32 = 24.0;
    /// The link menu, and the application menu drawn like it.
    pub const MENU_WIDTH: f32 = 228.0;
    pub const SWITCHER_ROW: f32 = 28.0;
    pub const AUTOCOMPLETE_WIDTH: f32 = 300.0;
    pub const HOVER_PREVIEW_WIDTH: f32 = 320.0;
    pub const TAG_PILL: f32 = 18.0;
    pub const PROFILE_CHIP: f32 = 18.0;
    pub const CHECKBOX: f32 = 14.0;
    pub const SCROLLBAR_THUMB: f32 = 6.0;
    /// The shortest the scrollbar thumb gets in a long document (not
    /// designed).
    pub const SCROLLBAR_MIN_THUMB: f32 = 24.0;
    /// Copy and wrap buttons in a code block's head.
    pub const CODE_BUTTON: f32 = 20.0;
    pub const THEME_CARD_PREVIEW: f32 = 84.0;
    /// The caret: 2 px wide, `accent.default` (SPEC § Editor).
    pub const CARET_WIDTH: f32 = 2.0;

    pub mod tab {
        pub const HEIGHT: f32 = 28.0;
        pub const PAD_LEFT: f32 = 10.0;
        pub const PAD_RIGHT: f32 = 4.0;
        pub const CLOSE_BUTTON: f32 = 18.0;
        /// The unsaved dot that replaces the close icon.
        pub const UNSAVED_DOT: f32 = 7.0;
    }

    /// Infobar buttons: action buttons 22 high, the dismiss button 22 × 22.
    pub mod infobar {
        pub const BUTTON_HEIGHT: f32 = 22.0;
        pub const DISMISS: f32 = 22.0;
    }

    /// The tooltip: 22 high.
    pub const TOOLTIP: f32 = 22.0;

    pub mod icon_button {
        pub const WIDTH: f32 = 26.0;
        pub const HEIGHT: f32 = 24.0;
        /// Back, Forward, New tab and More actions are square.
        pub const SQUARE: f32 = 24.0;
        /// The application menu button on Windows and Linux.
        pub const MENU_WIDTH: f32 = 28.0;
    }

    pub mod segment {
        pub const FRAME: f32 = 24.0;
        pub const INNER: f32 = 20.0;
    }

    pub mod switcher {
        pub const WIDTH: f32 = 540.0;
        pub const INPUT_ROW: f32 = 40.0;
        pub const TOP: f32 = 84.0;
    }

    pub mod switch {
        pub const WIDTH: f32 = 30.0;
        pub const HEIGHT: f32 = 18.0;
        pub const KNOB: f32 = 14.0;
    }

    pub mod document {
        pub const COLUMN_MAX: f32 = 680.0;
        pub const PAD_X: f32 = 40.0;
        pub const PAD_TOP: f32 = 22.0;
    }

    pub mod editor {
        pub const LINE_NUMBER_WIDTH: f32 = 26.0;
        pub const GUTTER_GAP: f32 = 14.0;
        /// The caret: 16 tall, centred in its 20 px row (SPEC § Editor).
        pub const CARET_HEIGHT: f32 = 16.0;
    }

    pub mod settings_row {
        pub const PLAIN: f32 = 36.0;
        pub const WITH_DESCRIPTION: f32 = 40.0;
    }

    /// The find popover (SPEC § Popovers, "Find and replace"): 380 wide;
    /// toggles 20 × 18; the match count at least 58 wide; previous, next
    /// and close are `SMALL_ICON_BUTTON`.
    pub mod find {
        pub const WIDTH: f32 = 380.0;
        pub const TOGGLE_WIDTH: f32 = 20.0;
        pub const TOGGLE_HEIGHT: f32 = 18.0;
        pub const COUNT_MIN_WIDTH: f32 = 58.0;
    }
}

/// Spacing inside components, as the specification gives it per component.
pub mod spacing {
    /// The title row: padding `0 6`, gap 2.
    pub mod title_row {
        pub const PAD_X: f32 = 6.0;
        pub const GAP: f32 = 2.0;
        /// Space between the history buttons and the tab strip.
        pub const TAB_STRIP_MARGIN: f32 = 4.0;
        /// Space before the "more actions" button.
        pub const MORE_MARGIN: f32 = 4.0;
        /// Narrowest the flexible drag space may get.
        pub const DRAG_MIN: f32 = 6.0;
    }

    /// Tabs: gap 2 between tabs and inside a tab.
    pub mod tab {
        pub const GAP: f32 = 2.0;
    }

    /// The mode switch: frame padding 1 and gap 1; labelled segments padding
    /// `0 8` with a 5 px icon gap; the icon-only segment is 26 wide.
    pub mod mode_switch {
        pub const FRAME_PAD: f32 = 1.0;
        pub const FRAME_GAP: f32 = 1.0;
        pub const SEGMENT_PAD_X: f32 = 8.0;
        pub const SEGMENT_GAP: f32 = 5.0;
        pub const ICON_SEGMENT_WIDTH: f32 = 26.0;
    }

    /// The document column: 10 after the title heading, 12 between paragraphs.
    pub mod document {
        pub const HEADING_GAP: f32 = 10.0;
        pub const PARAGRAPH_GAP: f32 = 12.0;
        /// Space between a list marker and its item's content (not
        /// designed).
        pub const LIST_MARKER_GAP: f32 = 6.0;
    }

    /// A code block's head in the preview: the buttons 4 apart and 4 from
    /// the right edge; the label starts at the code's own padding.
    pub mod code_head {
        pub const PAD_RIGHT: f32 = 4.0;
        pub const GAP: f32 = 4.0;
    }

    /// The preview's scrollbar: 3 from the right edge, 4 from the top and
    /// the bottom. No track.
    pub mod scrollbar {
        pub const INSET_X: f32 = 3.0;
        pub const INSET_Y: f32 = 4.0;
    }

    /// Inline content of the preview (SPEC § Preview, § Link status).
    pub mod inline {
        /// Inline code: padding `1 5` around mono 13, radius `radius::CHIP`.
        pub const CODE_PAD_X: f32 = 5.0;
        pub const CODE_PAD_Y: f32 = 1.0;
        /// Links: a 1 px underline, 3 below the baseline.
        pub const UNDERLINE_OFFSET: f32 = 3.0;
        /// Task list: gap between the checkbox and the text.
        pub const CHECKBOX_GAP: f32 = 9.0;
        /// Link status: space before a status icon (`triangle-alert`,
        /// `file-plus`, `log-out`, `lock`).
        pub const STATUS_ICON_GAP: f32 = 5.0;
        /// Link status: space before `≈` and before the external-link arrow.
        pub const STATUS_MARKER_GAP: f32 = 4.0;
        /// Link status: space between the ambiguous icon and the count.
        pub const STATUS_COUNT_GAP: f32 = 2.0;
    }

    /// Infobars: padding `4 6 4 12`, gap 8; action buttons padding `0 9`.
    pub mod infobar {
        pub const PAD_TOP: f32 = 4.0;
        pub const PAD_RIGHT: f32 = 6.0;
        pub const PAD_BOTTOM: f32 = 4.0;
        pub const PAD_LEFT: f32 = 12.0;
        pub const GAP: f32 = 8.0;
        pub const BUTTON_PAD_X: f32 = 9.0;
    }

    /// The tooltip: padding `0 8`.
    pub mod tooltip {
        pub const PAD_X: f32 = 8.0;
    }

    /// The status bar: padding `0 8`, gap 12.
    pub mod status_bar {
        pub const PAD_X: f32 = 8.0;
        pub const GAP: f32 = 12.0;
        /// Gap inside the breadcrumb and between an icon and its label.
        pub const CRUMB_GAP: f32 = 3.0;
        pub const ICON_GAP: f32 = 4.0;
    }

    /// The profile chip: padding `0 5 0 6`, gap 5.
    pub mod profile_chip {
        pub const PAD_LEFT: f32 = 6.0;
        pub const PAD_RIGHT: f32 = 5.0;
        pub const GAP: f32 = 5.0;
    }

    /// The sidebar head: padding `0 6`, gap 8; the panel switcher's buttons
    /// are 2 apart. On macOS the window controls take the left of the head.
    pub mod sidebar_head {
        pub const PAD_X: f32 = 6.0;
        pub const GAP: f32 = 8.0;
        pub const SWITCHER_GAP: f32 = 2.0;
    }

    /// A panel head: padding `0 6 0 12`, gap 2.
    pub mod panel_head {
        pub const PAD_LEFT: f32 = 12.0;
        pub const PAD_RIGHT: f32 = 6.0;
        pub const GAP: f32 = 2.0;
        /// Between the Files panel's vault name and its chevron.
        pub const TITLE_GAP: f32 = 4.0;
    }

    /// List rows (Files, Tags): margin `0 6` (`size::LIST_ROW_INSET`), gap 5,
    /// padding right 8, padding left 6 plus `size::LIST_INDENT` per level.
    pub mod list_row {
        pub const PAD_LEFT: f32 = 6.0;
        pub const PAD_RIGHT: f32 = 8.0;
        pub const GAP: f32 = 5.0;
    }

    /// A panel's list: 2 above the first row and below the last.
    pub mod list {
        pub const PAD_Y: f32 = 2.0;
    }

    /// A right panel section head: padding `0 10`, gap 4.
    pub mod section_head {
        pub const PAD_X: f32 = 10.0;
        pub const GAP: f32 = 4.0;
    }

    /// Outline rows: margin `0 6` (`size::LIST_ROW_INSET`), padding right 8,
    /// padding left 10 plus `size::OUTLINE_INDENT` per level; 8 below the
    /// last row of a section.
    pub mod outline_row {
        pub const PAD_LEFT: f32 = 10.0;
        pub const PAD_RIGHT: f32 = 8.0;
        pub const SECTION_PAD_BOTTOM: f32 = 8.0;
    }

    /// The find popover: padding 4, rows 4 apart, 2 between the controls
    /// of a row; the field has padding `0 2 0 7` and 4 between its text
    /// and its toggles. The popover sits 8 below the top of the document
    /// area and 16 from its right edge, clear of the scrollbar (not
    /// designed).
    pub mod find {
        pub const PAD: f32 = 4.0;
        pub const ROW_GAP: f32 = 4.0;
        pub const GAP: f32 = 2.0;
        pub const FIELD_PAD_LEFT: f32 = 7.0;
        pub const FIELD_PAD_RIGHT: f32 = 2.0;
        pub const FIELD_GAP: f32 = 4.0;
        pub const INSET_TOP: f32 = 8.0;
        pub const INSET_RIGHT: f32 = 16.0;
    }

    /// Menus: the link menu (SPEC § Popovers) and the application menu drawn
    /// like it. Padding 4; rows with padding `0 8`; separators with margin
    /// `4 8`. Not designed: the check mark slot of a row (7 before the
    /// label, as in autocomplete rows), the least space between a label and
    /// its shortcut, the 4 between the menu button and the menu, and the 2
    /// between a menu and its submenu.
    pub mod menu {
        pub const PAD: f32 = 4.0;
        pub const ROW_PAD_X: f32 = 8.0;
        pub const SEPARATOR_MARGIN_Y: f32 = 4.0;
        pub const SEPARATOR_MARGIN_X: f32 = 8.0;
        pub const ICON_GAP: f32 = 7.0;
        pub const SHORTCUT_GAP: f32 = 16.0;
        pub const OFFSET_Y: f32 = 4.0;
        pub const SUBMENU_GAP: f32 = 2.0;
    }

    /// Window controls drawn by the application on Linux: 8 apart, 8 from the
    /// right of the title row, 10 after the panel toggle.
    pub mod linux_controls {
        pub const DIAMETER: f32 = 22.0;
        pub const GAP: f32 = 8.0;
        pub const INSET_RIGHT: f32 = 8.0;
        pub const MARGIN_LEFT: f32 = 10.0;
        pub const GLYPH: f32 = 10.0;
    }

    /// The macOS window controls ("traffic lights"), drawn by the system:
    /// three 12 px circles, 8 apart, 13 from the window's left edge and
    /// centred in the title row. The window keeps their space free.
    pub mod macos_controls {
        pub const DIAMETER: f32 = 12.0;
        pub const GAP: f32 = 8.0;
        pub const INSET_LEFT: f32 = 13.0;
        /// Space after the controls at the start of the title row, when the
        /// sidebar is hidden.
        pub const MARGIN_RIGHT: f32 = 10.0;
        /// The three circles and the two gaps between them.
        pub const WIDTH: f32 = 3.0 * DIAMETER + 2.0 * GAP;
        /// AppKit's standard window button: a 14 × 16 frame with the circle
        /// centred in it. GPUI positions the frame, not the circle.
        pub const BUTTON_FRAME_WIDTH: f32 = 14.0;
        pub const BUTTON_FRAME_HEIGHT: f32 = 16.0;
    }

    /// The Windows caption buttons, drawn by the application: 46 wide, the
    /// full height of the title row, at its far right, 4 after the panel
    /// toggle. Glyphs 12, stroke 1.5 on the 24 grid.
    pub mod windows_controls {
        pub const WIDTH: f32 = 46.0;
        pub const MARGIN_LEFT: f32 = 4.0;
        pub const GLYPH: f32 = 12.0;
    }
}

/// Corner radii.
pub mod radius {
    pub const CONTROL: f32 = 5.0;
    /// Buttons inside other controls: tab close, field toggles, code block
    /// buttons.
    pub const SMALL_CONTROL: f32 = 4.0;
    pub const TAB: f32 = 6.0;
    pub const SEGMENT_FRAME: f32 = 6.0;
    pub const SEGMENT: f32 = 4.0;
    pub const BLOCK: f32 = 6.0;
    pub const POPOVER: f32 = 8.0;
    pub const SWITCHER: f32 = 10.0;
    pub const PILL: f32 = 9.0;
    pub const CHIP: f32 = 4.0;
    pub const KEYCAP: f32 = 4.0;
    pub const SCROLLBAR: f32 = 3.0;

    pub mod window {
        pub const MACOS: f32 = 12.0;
        pub const WINDOWS: f32 = 8.0;
        pub const LINUX: f32 = 12.0;
    }
}

/// Icon sizes.
pub mod icon {
    /// Title row buttons, panel switcher, menu button.
    pub const TOOLBAR: f32 = 15.0;
    /// List rows, panel head actions, mode switch, popover rows.
    pub const ROW: f32 = 14.0;
    /// Finding severity, code block buttons, inline status icons.
    pub const INLINE: f32 = 13.0;
    /// Status bar, profile chip, summary counts.
    pub const SMALL: f32 = 12.0;
    pub const CHEVRON: f32 = 12.0;
    /// Tab close.
    pub const TAB_CLOSE: f32 = 11.0;
    /// Breadcrumb chevrons, fold markers, profile chip chevrons.
    pub const TINY: f32 = 10.0;
}

/// Type scale: size and line height.
pub mod text {
    /// A size and its line height.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct Scale {
        pub size: f32,
        pub line: f32,
    }

    /// Interface text.
    pub const UI: Scale = Scale {
        size: 12.0,
        line: 16.0,
    };
    /// Status bar, counts, descriptions, paths.
    pub const UI_SMALL: Scale = Scale {
        size: 11.0,
        line: 14.0,
    };
    /// Panel section headers, settings group headings (weight 600).
    pub const UI_SECTION: Scale = Scale {
        size: 11.5,
        line: 16.0,
    };
    /// Settings page title, quick switcher results (weight 600).
    pub const UI_TITLE: Scale = Scale {
        size: 13.0,
        line: 20.0,
    };
    pub const PREVIEW_BODY: Scale = Scale {
        size: 15.0,
        line: 24.0,
    };
    /// Weight 700, tracking [`H1_TRACKING`].
    pub const PREVIEW_H1: Scale = Scale {
        size: 26.0,
        line: 32.0,
    };
    /// Weight 600, tracking [`H2_TRACKING`].
    pub const PREVIEW_H2: Scale = Scale {
        size: 18.0,
        line: 24.0,
    };
    /// Callout body.
    pub const PREVIEW_SMALL: Scale = Scale {
        size: 14.0,
        line: 22.0,
    };
    pub const PREVIEW_TABLE: Scale = Scale {
        size: 13.5,
        line: 20.0,
    };
    pub const PREVIEW_TABLE_HEADER_SIZE: f32 = 12.0;
    pub const PREVIEW_PROPERTIES: Scale = Scale {
        size: 12.5,
        line: 22.0,
    };
    pub const CODE: Scale = Scale {
        size: 12.5,
        line: 20.0,
    };
    pub const EDITOR: Scale = Scale {
        size: 12.5,
        line: 20.0,
    };
    pub const EDITOR_LINE_NUMBER: Scale = Scale {
        size: 11.0,
        line: 20.0,
    };
    pub const KEYCAP_SIZE: f32 = 11.5;
    /// The find field's toggles (`Aa`, `ab`, `.*`), mono.
    pub const FIELD_TOGGLE_SIZE: f32 = 10.5;
    /// Shortcuts at the right of menu rows, mono.
    pub const MENU_SHORTCUT_SIZE: f32 = 10.5;
    /// Inline code in the preview (mono).
    pub const INLINE_CODE_SIZE: f32 = 13.0;
    /// Tooltips.
    pub const TOOLTIP_SIZE: f32 = 11.5;

    /// Letter spacing of H1 and H2, in em.
    pub const H1_TRACKING: f32 = -0.02;
    pub const H2_TRACKING: f32 = -0.01;

    /// Font weights.
    pub mod weight {
        pub const REGULAR: f32 = 400.0;
        pub const ACTIVE: f32 = 500.0;
        pub const STRONG: f32 = 600.0;
        pub const BOLD: f32 = 700.0;
    }
}

/// Font families. Inter and JetBrains Mono are not bundled yet; when they are
/// not installed, text falls back to the system interface font.
pub mod font {
    pub const INTERFACE: &str = "Inter";
    pub const EDITOR: &str = "JetBrains Mono";
    /// GPUI's name for the platform's interface font.
    pub const SYSTEM_FALLBACK: &str = ".SystemUIFont";
}

/// Width of every line: borders, rings, separators.
pub const HAIRLINE: f32 = 1.0;

/// The focus halo around a focused field: 2 px of `accent.tint` outside
/// its 1 px `accent.default` border (SPEC § Controls).
pub const FOCUS_HALO: f32 = 2.0;

/// Opacity of disabled controls.
pub const DISABLED_OPACITY: f32 = 0.5;

/// Opacity of the `selection.text` tint drawn over a selected image, math
/// formula or checkbox in the preview, as browsers do for replaced content.
pub const SELECTED_BOX_TINT_OPACITY: f32 = 0.45;
