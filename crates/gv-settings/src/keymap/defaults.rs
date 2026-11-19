// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The built-in commands and their default keys.
//!
//! `secondary` is Cmd on macOS and Ctrl elsewhere. Platform differences:
//!
//! - Back and forward are Alt+←/→ on Linux and Windows, Cmd+[ / Cmd+] and
//!   Cmd+Alt+←/→ on macOS, where Alt+← moves by word.
//! - Expand and shrink selection are Shift+Alt+→/← on Linux and Windows and
//!   Ctrl+Shift+Cmd+→/← on macOS, where Shift+Option+→ selects by word.
//! - Find and replace is Ctrl+H on Linux and Windows and Cmd+Alt+F on macOS,
//!   where Cmd+H hides the app; find next is F3 elsewhere and Cmd+G on macOS.
//! - Insert table (Mod+Alt+T) has no default on Linux, where Ctrl+Alt+T opens
//!   a terminal on most desktops.
//! - F10 opens the application menu on Linux and Windows, as in GNOME and
//!   Windows applications; macOS has the system menu bar instead.
//!
//! The theme chord Mod+K Mod+T does not apply in the editor, where Mod+K
//! inserts a link at once.

use super::command::{Category, Command, CommandError, Platforms};

const ALL: Platforms = Platforms::ALL;
const MAC: Platforms = Platforms::MACOS;
const NOT_MAC: Platforms = Platforms::NOT_MACOS;
const MAC_WIN: Platforms = Platforms::MACOS.union(Platforms::WINDOWS);

/// Commands that work in the editor only.
const EDITOR: &str = "editor";
/// Commands for the open document in either mode.
const DOCUMENT: &str = "editor || reader";

/// One row of the table below: id, title, category, context, default keys.
type Row = (
    &'static str,
    &'static str,
    Category,
    Option<&'static str>,
    &'static [(&'static str, Platforms)],
);

#[rustfmt::skip]
const BUILTIN: &[Row] = &[
    // Application
    ("app.quit", "Quit", Category::Application, None, &[("secondary-q", ALL)]),
    ("app.settings", "Open settings", Category::Application, None, &[("secondary-,", ALL)]),
    ("app.about", "About Glyphvane", Category::Application, None, &[]),
    ("app.open_menu", "Open application menu", Category::Application, None, &[("f10", NOT_MAC)]),
    ("app.keymap_cheatsheet", "Keyboard shortcuts", Category::Application, None, &[]),
    ("app.open_documentation", "Open documentation", Category::Application, None, &[]),
    ("app.check_for_updates", "Check for updates", Category::Application, None, &[]),
    ("app.copy_debug_info", "Copy debug info", Category::Application, None, &[]),
    ("app.licenses", "Third-party licenses", Category::Application, None, &[]),
    ("app.install_cli", "Install the glyphvane command", Category::Application, None, &[]),
    ("settings.import", "Import settings", Category::Application, None, &[]),
    ("settings.export", "Export settings", Category::Application, None, &[]),

    // File
    ("file.new_note", "New note", Category::File, None, &[("secondary-n", ALL)]),
    ("file.save", "Save", Category::File, None, &[("secondary-s", ALL)]),
    ("file.save_as", "Save as", Category::File, None, &[("secondary-shift-s", ALL)]),
    ("file.open", "Open file", Category::File, None, &[]),
    ("file.open_folder", "Open folder", Category::File, None, &[]),
    ("file.open_recent", "Open recent", Category::File, None, &[]),
    ("file.revert", "Revert", Category::File, None, &[]),
    ("file.new_folder", "New folder", Category::File, None, &[]),
    ("file.rename", "Rename file", Category::File, None, &[]),
    ("file.move", "Move file", Category::File, None, &[]),
    ("file.duplicate", "Duplicate file", Category::File, None, &[]),
    ("file.delete", "Move file to trash", Category::File, None, &[]),
    ("file.reveal_in_file_manager", "Reveal in file manager", Category::File, None, &[]),
    ("file.open_with_default_app", "Open with default app", Category::File, None, &[]),
    ("file.copy_path", "Copy file path", Category::File, None, &[]),
    ("file.history", "File history", Category::File, None, &[]),
    ("file.export_html", "Export as HTML", Category::File, None, &[]),
    ("file.export_pdf", "Export as PDF", Category::File, None, &[]),
    ("file.print", "Print", Category::File, None, &[]),
    ("file.open_in_browser", "Open in browser", Category::File, None, &[]),
    ("notes.daily_note", "Open today's note", Category::File, None, &[]),
    ("notes.previous_daily_note", "Open previous daily note", Category::File, None, &[]),
    ("notes.next_daily_note", "Open next daily note", Category::File, None, &[]),
    ("bookmarks.toggle", "Bookmark this note", Category::File, None, &[]),

    // Tabs
    ("tabs.new", "New tab", Category::Tabs, None, &[]),
    ("tabs.close", "Close tab", Category::Tabs, None, &[("secondary-w", ALL)]),
    ("tabs.reopen_closed", "Reopen closed tab", Category::Tabs, None, &[("secondary-shift-t", ALL)]),
    ("tabs.close_others", "Close other tabs", Category::Tabs, None, &[]),
    ("tabs.close_to_right", "Close tabs to the right", Category::Tabs, None, &[]),

    // Workspace
    ("workspace.command_palette", "Command palette", Category::Workspace, None,
        &[("secondary-p", ALL), ("secondary-shift-p", ALL)]),
    ("workspace.quick_switcher", "Quick switcher", Category::Workspace, None, &[("secondary-o", ALL)]),
    ("workspace.toggle_sidebar", "Toggle sidebar", Category::Workspace, None, &[]),
    ("workspace.toggle_panel", "Toggle right panel", Category::Workspace, None, &[]),
    ("workspace.focus_next_pane", "Focus next pane", Category::Workspace, None, &[("f6", ALL)]),
    ("workspace.focus_previous_pane", "Focus previous pane", Category::Workspace, None, &[("shift-f6", ALL)]),
    ("workspace.reveal_in_tree", "Reveal in file tree", Category::Workspace, None, &[]),
    ("workspace.context_menu", "Open context menu", Category::Workspace, Some("editor || reader || tree"),
        &[("menu", ALL), ("shift-f10", ALL)]),
    ("workspace.new_window", "New window", Category::Workspace, None, &[]),
    ("workspace.show_files", "Show files", Category::Workspace, None, &[]),
    ("workspace.show_tags", "Show tags", Category::Workspace, None, &[]),
    ("workspace.show_bookmarks", "Show bookmarks", Category::Workspace, None, &[]),
    ("workspace.show_recent", "Show recent", Category::Workspace, None, &[]),
    ("workspace.show_sync_conflicts", "Show sync conflicts", Category::Workspace, None, &[]),
    ("workspace.show_outline", "Show outline", Category::Workspace, None, &[]),
    ("workspace.show_backlinks", "Show backlinks", Category::Workspace, None, &[]),
    ("workspace.show_outgoing_links", "Show outgoing links", Category::Workspace, None, &[]),
    ("workspace.show_link_checker", "Show link checker", Category::Workspace, None, &[]),
    ("workspace.show_base", "Show base in sidebar", Category::Workspace, None, &[]),
    ("window.minimize", "Minimize window", Category::Workspace, None, &[]),
    ("window.zoom", "Zoom window", Category::Workspace, None, &[]),
    ("window.bring_all_to_front", "Bring all windows to front", Category::Workspace, None, &[]),

    // View
    ("view.toggle_mode", "Toggle Preview and Edit", Category::View, None, &[("secondary-e", ALL)]),
    ("view.toggle_split", "Toggle split view", Category::View, None, &[("secondary-alt-e", ALL)]),
    ("view.toggle_live_preview", "Toggle Live Preview", Category::View, None, &[]),
    ("view.preview", "Show Preview", Category::View, None, &[]),
    ("view.edit", "Show Edit", Category::View, None, &[]),
    ("view.split", "Show Edit and Preview side by side", Category::View, None, &[]),
    ("view.toggle_viewer_mode", "Toggle viewer mode", Category::View, None, &[]),
    ("view.cycle_theme", "Cycle theme", Category::View, Some("!editor"),
        &[("secondary-k secondary-t", ALL)]),
    ("view.zoom_in", "Zoom in", Category::View, None,
        &[("secondary-=", ALL), ("secondary-+", ALL)]),
    ("view.zoom_out", "Zoom out", Category::View, None, &[("secondary--", ALL)]),
    ("view.zoom_reset", "Reset zoom", Category::View, None, &[("secondary-0", ALL)]),
    ("view.zoom_to_fit", "Zoom to fit", Category::View, None, &[]),
    ("view.fold", "Fold", Category::View, Some(EDITOR), &[]),
    ("view.unfold", "Unfold", Category::View, Some(EDITOR), &[]),
    ("view.fold_all", "Fold all", Category::View, Some(EDITOR), &[]),
    ("view.unfold_all", "Unfold all", Category::View, Some(EDITOR), &[]),
    ("view.fold_level_1", "Fold to heading level 1", Category::View, Some(EDITOR), &[]),
    ("view.fold_level_2", "Fold to heading level 2", Category::View, Some(EDITOR), &[]),
    ("view.fold_level_3", "Fold to heading level 3", Category::View, Some(EDITOR), &[]),
    ("view.fold_level_4", "Fold to heading level 4", Category::View, Some(EDITOR), &[]),
    ("view.fold_level_5", "Fold to heading level 5", Category::View, Some(EDITOR), &[]),
    ("view.fold_level_6", "Fold to heading level 6", Category::View, Some(EDITOR), &[]),
    ("view.toggle_soft_wrap", "Toggle soft wrap", Category::View, Some(EDITOR), &[]),
    ("window.toggle_full_screen", "Toggle full screen", Category::View, None, &[]),

    // Navigation
    ("navigation.back", "Go back", Category::Navigation, None,
        &[("alt-left", NOT_MAC), ("cmd-[", MAC), ("cmd-alt-left", MAC)]),
    ("navigation.forward", "Go forward", Category::Navigation, None,
        &[("alt-right", NOT_MAC), ("cmd-]", MAC), ("cmd-alt-right", MAC)]),
    ("navigation.go_to_line", "Go to line", Category::Navigation, Some(EDITOR), &[("ctrl-g", ALL)]),
    ("navigation.go_to_heading", "Go to heading", Category::Navigation, Some(DOCUMENT),
        &[("secondary-shift-o", ALL)]),
    ("navigation.go_to_footnote", "Go to footnote or back", Category::Navigation, Some(DOCUMENT), &[]),
    ("navigation.next_misspelling", "Next misspelling", Category::Navigation, Some(EDITOR), &[]),

    // Search
    ("search.vault", "Search in vault", Category::Search, None, &[("secondary-shift-f", ALL)]),
    ("search.find", "Find", Category::Search, Some(DOCUMENT), &[("secondary-f", ALL)]),
    ("search.replace", "Find and replace", Category::Search, Some(EDITOR),
        &[("ctrl-h", NOT_MAC), ("cmd-alt-f", MAC)]),
    ("search.find_next", "Find next", Category::Search, Some(DOCUMENT),
        &[("f3", NOT_MAC), ("cmd-g", MAC)]),
    ("search.find_previous", "Find previous", Category::Search, Some(DOCUMENT),
        &[("shift-f3", NOT_MAC), ("cmd-shift-g", MAC)]),

    // Links
    ("links.follow", "Follow link", Category::Links, Some(DOCUMENT), &[("alt-enter", ALL)]),
    ("links.open_in_new_tab", "Open link in new tab", Category::Links, Some(DOCUMENT),
        &[("secondary-alt-enter", ALL)]),
    ("links.open_in_new_window", "Open link in new window", Category::Links, Some(DOCUMENT), &[]),
    ("links.preview", "Preview link under caret", Category::Links, Some(DOCUMENT), &[]),
    ("links.list", "Links in this note", Category::Links, Some(DOCUMENT), &[]),
    ("links.copy_link", "Copy link", Category::Links, Some(DOCUMENT), &[]),
    ("links.copy_resolved_path", "Copy resolved path", Category::Links, Some(DOCUMENT), &[]),
    ("links.copy_url", "Copy URL", Category::Links, Some(DOCUMENT), &[]),
    ("links.reveal_in_tree", "Reveal link target in file tree", Category::Links, Some(DOCUMENT), &[]),
    ("links.reveal_in_file_manager", "Reveal link target in file manager", Category::Links,
        Some(DOCUMENT), &[]),
    ("links.show_trace", "Show resolution trace", Category::Links, Some(DOCUMENT), &[]),
    ("links.fix", "Fix link", Category::Links, Some(DOCUMENT), &[]),
    ("links.create_note", "Create note for link", Category::Links, Some(DOCUMENT), &[]),
    ("links.copy_link_to_heading", "Copy link to heading or block", Category::Links,
        Some(DOCUMENT), &[]),
    ("links.check_note", "Check links in this note", Category::Links, None, &[]),
    ("links.check_folder", "Check links in this folder", Category::Links, None, &[]),
    ("links.check_vault", "Check links in the vault", Category::Links, None, &[]),
    ("refactor.rename_heading", "Rename heading or block ID", Category::Links, Some(EDITOR), &[]),

    // Editing
    ("editor.undo", "Undo", Category::Editing, Some(EDITOR), &[("secondary-z", ALL)]),
    ("editor.redo", "Redo", Category::Editing, Some(EDITOR),
        &[("secondary-shift-z", ALL), ("ctrl-y", NOT_MAC)]),
    ("editor.cut", "Cut", Category::Editing, Some(EDITOR), &[("secondary-x", ALL)]),
    ("editor.copy", "Copy", Category::Editing, Some(DOCUMENT), &[("secondary-c", ALL)]),
    ("editor.copy_as_markdown", "Copy as Markdown", Category::Editing, Some(DOCUMENT), &[]),
    ("editor.copy_as_rich_text", "Copy as rich text", Category::Editing, Some(DOCUMENT), &[]),
    ("editor.paste", "Paste", Category::Editing, Some(EDITOR), &[("secondary-v", ALL)]),
    ("editor.paste_plain", "Paste as plain text", Category::Editing, Some(EDITOR),
        &[("secondary-shift-v", ALL)]),
    ("editor.select_all", "Select all", Category::Editing, Some(DOCUMENT), &[("secondary-a", ALL)]),
    ("editor.newline", "New line, continuing the list or quote", Category::Editing, Some(EDITOR),
        &[("enter", ALL)]),
    ("editor.newline_plain", "New line without continuing the list", Category::Editing, Some(EDITOR),
        &[("shift-enter", ALL)]),
    ("editor.indent", "Indent", Category::Editing, Some(EDITOR), &[("tab", ALL)]),
    ("editor.outdent", "Outdent", Category::Editing, Some(EDITOR), &[("shift-tab", ALL)]),
    ("editor.renumber_list", "Renumber list", Category::Editing, Some(EDITOR), &[]),
    ("editor.move_line_up", "Move line up", Category::Editing, Some(EDITOR), &[("alt-up", ALL)]),
    ("editor.move_line_down", "Move line down", Category::Editing, Some(EDITOR), &[("alt-down", ALL)]),
    ("editor.duplicate_line", "Duplicate line", Category::Editing, Some(EDITOR), &[]),
    ("editor.join_lines", "Join lines", Category::Editing, Some(EDITOR), &[]),
    ("editor.sort_lines", "Sort lines", Category::Editing, Some(EDITOR), &[]),
    ("editor.change_case", "Change case", Category::Editing, Some(EDITOR), &[]),
    ("editor.expand_selection", "Expand selection", Category::Editing, Some(EDITOR),
        &[("shift-alt-right", NOT_MAC), ("ctrl-shift-cmd-right", MAC)]),
    ("editor.shrink_selection", "Shrink selection", Category::Editing, Some(EDITOR),
        &[("shift-alt-left", NOT_MAC), ("ctrl-shift-cmd-left", MAC)]),
    ("editor.toggle_comment", "Toggle comment", Category::Editing, Some(EDITOR), &[("secondary-/", ALL)]),
    ("editor.toggle_whitespace", "Show whitespace", Category::Editing, Some(EDITOR), &[]),
    ("spell.add_to_dictionary", "Add word to dictionary", Category::Editing, Some(EDITOR), &[]),
    ("spell.add_to_vault_dictionary", "Add word to vault dictionary", Category::Editing, Some(EDITOR), &[]),
    ("spell.ignore", "Ignore word", Category::Editing, Some(EDITOR), &[]),

    // Formatting
    ("editor.bold", "Bold", Category::Formatting, Some(EDITOR), &[("secondary-b", ALL)]),
    ("editor.italic", "Italic", Category::Formatting, Some(EDITOR), &[("secondary-i", ALL)]),
    ("editor.strikethrough", "Strikethrough", Category::Formatting, Some(EDITOR),
        &[("secondary-shift-x", ALL)]),
    ("editor.highlight", "Highlight", Category::Formatting, Some(EDITOR), &[("secondary-shift-h", ALL)]),
    ("editor.inline_code", "Inline code", Category::Formatting, Some(EDITOR), &[("secondary-shift-c", ALL)]),
    ("editor.insert_link", "Insert link", Category::Formatting, Some(EDITOR), &[("secondary-k", ALL)]),
    ("editor.toggle_task", "Toggle task", Category::Formatting, Some(EDITOR), &[("secondary-l", ALL)]),
    ("editor.heading_1", "Heading 1", Category::Formatting, Some(EDITOR), &[("secondary-alt-1", ALL)]),
    ("editor.heading_2", "Heading 2", Category::Formatting, Some(EDITOR), &[("secondary-alt-2", ALL)]),
    ("editor.heading_3", "Heading 3", Category::Formatting, Some(EDITOR), &[("secondary-alt-3", ALL)]),
    ("editor.heading_4", "Heading 4", Category::Formatting, Some(EDITOR), &[("secondary-alt-4", ALL)]),
    ("editor.heading_5", "Heading 5", Category::Formatting, Some(EDITOR), &[("secondary-alt-5", ALL)]),
    ("editor.heading_6", "Heading 6", Category::Formatting, Some(EDITOR), &[("secondary-alt-6", ALL)]),
    ("editor.paragraph", "Paragraph", Category::Formatting, Some(EDITOR), &[("secondary-alt-0", ALL)]),
    ("editor.quote", "Quote", Category::Formatting, Some(EDITOR), &[("secondary-shift-.", ALL)]),
    ("editor.insert_callout", "Insert callout", Category::Formatting, Some(EDITOR), &[]),
    ("editor.insert_footnote", "Insert footnote", Category::Formatting, Some(EDITOR), &[]),
    ("notes.insert_template", "Insert template", Category::Formatting, Some(EDITOR), &[]),
    ("editor.table", "Insert or format table", Category::Formatting, Some(EDITOR),
        &[("secondary-alt-t", MAC_WIN)]),
    ("editor.table_insert_row_above", "Insert table row above", Category::Formatting, Some(EDITOR), &[]),
    ("editor.table_insert_row_below", "Insert table row below", Category::Formatting, Some(EDITOR), &[]),
    ("editor.table_insert_column_left", "Insert table column left", Category::Formatting, Some(EDITOR), &[]),
    ("editor.table_insert_column_right", "Insert table column right", Category::Formatting, Some(EDITOR), &[]),
    ("editor.table_delete_row", "Delete table row", Category::Formatting, Some(EDITOR), &[]),
    ("editor.table_delete_column", "Delete table column", Category::Formatting, Some(EDITOR), &[]),
    ("editor.table_move_row_up", "Move table row up", Category::Formatting, Some(EDITOR), &[]),
    ("editor.table_move_row_down", "Move table row down", Category::Formatting, Some(EDITOR), &[]),
    ("editor.table_move_column_left", "Move table column left", Category::Formatting, Some(EDITOR), &[]),
    ("editor.table_move_column_right", "Move table column right", Category::Formatting, Some(EDITOR), &[]),
    ("editor.table_align_left", "Align table column left", Category::Formatting, Some(EDITOR), &[]),
    ("editor.table_align_center", "Center table column", Category::Formatting, Some(EDITOR), &[]),
    ("editor.table_align_right", "Align table column right", Category::Formatting, Some(EDITOR), &[]),
    ("editor.table_align_none", "Remove table column alignment", Category::Formatting, Some(EDITOR), &[]),
    ("editor.table_sort_by_column", "Sort table by column", Category::Formatting, Some(EDITOR), &[]),
];

/// The built-in commands, in registry order. Every entry is `Ok` (checked by
/// tests); an `Err` would be a mistake in the table above.
pub fn builtin_commands() -> Vec<Result<Command, CommandError>> {
    BUILTIN
        .iter()
        .map(|&(id, title, category, context, keys)| {
            let mut command = Command::new(id, title, category)?;
            if let Some(context) = context {
                command = command.with_context(context)?;
            }
            for &(keys, platforms) in keys {
                command = command.with_keys(keys, platforms)?;
            }
            Ok(command)
        })
        .collect()
}
