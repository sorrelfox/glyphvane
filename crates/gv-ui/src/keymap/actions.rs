// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! One GPUI action per command of the keymap model.
//!
//! The action of command `area.some_name` is the unit struct `SomeName` in
//! module `area`, and its GPUI name is `area::SomeName`. Views handle the
//! actions of the commands they implement with `on_action`; the rest have no
//! handler yet, so their keys fall through to whatever else is bound and
//! their menu items are disabled.

use gpui::Action;

/// The action of one command, built fresh.
type Build = fn() -> Box<dyn Action>;

fn build<A: Action + Default>() -> Box<dyn Action> {
    Box::new(A::default())
}

macro_rules! command_actions {
    ($( $area:ident { $( $action:ident = $id:literal ),+ $(,)? } )+) => {
        $(
            #[doc = concat!("Actions of the `", stringify!($area), ".*` commands.")]
            pub mod $area {
                gpui::actions!($area, [ $( $action ),+ ]);
            }
        )+

        /// Every command's action, by command id, grouped by area.
        const ACTIONS: &[(&str, Build)] = &[
            $( $( ($id, build::<$area::$action>), )+ )+
        ];
    };
}

command_actions! {
    app {
        Quit = "app.quit",
        Settings = "app.settings",
        About = "app.about",
        OpenMenu = "app.open_menu",
        KeymapCheatsheet = "app.keymap_cheatsheet",
        OpenDocumentation = "app.open_documentation",
        CheckForUpdates = "app.check_for_updates",
        CopyDebugInfo = "app.copy_debug_info",
        Licenses = "app.licenses",
        InstallCli = "app.install_cli",
    }
    settings {
        Import = "settings.import",
        Export = "settings.export",
    }
    file {
        NewNote = "file.new_note",
        Save = "file.save",
        SaveAs = "file.save_as",
        Open = "file.open",
        OpenFolder = "file.open_folder",
        OpenRecent = "file.open_recent",
        Revert = "file.revert",
        NewFolder = "file.new_folder",
        Rename = "file.rename",
        Move = "file.move",
        Duplicate = "file.duplicate",
        Delete = "file.delete",
        RevealInFileManager = "file.reveal_in_file_manager",
        OpenWithDefaultApp = "file.open_with_default_app",
        History = "file.history",
        ExportHtml = "file.export_html",
        ExportPdf = "file.export_pdf",
        Print = "file.print",
        OpenInBrowser = "file.open_in_browser",
    }
    notes {
        DailyNote = "notes.daily_note",
        InsertTemplate = "notes.insert_template",
    }
    bookmarks {
        Toggle = "bookmarks.toggle",
    }
    tabs {
        New = "tabs.new",
        Close = "tabs.close",
        ReopenClosed = "tabs.reopen_closed",
    }
    workspace {
        CommandPalette = "workspace.command_palette",
        QuickSwitcher = "workspace.quick_switcher",
        ToggleSidebar = "workspace.toggle_sidebar",
        TogglePanel = "workspace.toggle_panel",
        FocusNextPane = "workspace.focus_next_pane",
        FocusPreviousPane = "workspace.focus_previous_pane",
        RevealInTree = "workspace.reveal_in_tree",
        ContextMenu = "workspace.context_menu",
        NewWindow = "workspace.new_window",
        ShowFiles = "workspace.show_files",
        ShowTags = "workspace.show_tags",
        ShowBookmarks = "workspace.show_bookmarks",
        ShowRecent = "workspace.show_recent",
        ShowSyncConflicts = "workspace.show_sync_conflicts",
        ShowOutline = "workspace.show_outline",
        ShowBacklinks = "workspace.show_backlinks",
        ShowOutgoingLinks = "workspace.show_outgoing_links",
        ShowLinkChecker = "workspace.show_link_checker",
    }
    window {
        Minimize = "window.minimize",
        Zoom = "window.zoom",
        BringAllToFront = "window.bring_all_to_front",
        ToggleFullScreen = "window.toggle_full_screen",
    }
    view {
        ToggleMode = "view.toggle_mode",
        ToggleSplit = "view.toggle_split",
        Preview = "view.preview",
        Edit = "view.edit",
        Split = "view.split",
        ToggleViewerMode = "view.toggle_viewer_mode",
        CycleTheme = "view.cycle_theme",
        ZoomIn = "view.zoom_in",
        ZoomOut = "view.zoom_out",
        ZoomReset = "view.zoom_reset",
        Fold = "view.fold",
        Unfold = "view.unfold",
        FoldAll = "view.fold_all",
        UnfoldAll = "view.unfold_all",
        FoldLevel1 = "view.fold_level_1",
        FoldLevel2 = "view.fold_level_2",
        FoldLevel3 = "view.fold_level_3",
        FoldLevel4 = "view.fold_level_4",
        FoldLevel5 = "view.fold_level_5",
        FoldLevel6 = "view.fold_level_6",
        ToggleSoftWrap = "view.toggle_soft_wrap",
    }
    navigation {
        Back = "navigation.back",
        Forward = "navigation.forward",
        GoToLine = "navigation.go_to_line",
        GoToHeading = "navigation.go_to_heading",
        GoToFootnote = "navigation.go_to_footnote",
        NextMisspelling = "navigation.next_misspelling",
    }
    search {
        Vault = "search.vault",
        Find = "search.find",
        Replace = "search.replace",
        FindNext = "search.find_next",
        FindPrevious = "search.find_previous",
    }
    links {
        Follow = "links.follow",
        OpenInNewTab = "links.open_in_new_tab",
        OpenInSplit = "links.open_in_split",
        OpenInNewWindow = "links.open_in_new_window",
        Preview = "links.preview",
        List = "links.list",
        CopyLink = "links.copy_link",
        CopyResolvedPath = "links.copy_resolved_path",
        CopyUrl = "links.copy_url",
        RevealInTree = "links.reveal_in_tree",
        RevealInFileManager = "links.reveal_in_file_manager",
        ShowTrace = "links.show_trace",
        Fix = "links.fix",
        CreateNote = "links.create_note",
        CopyLinkToHeading = "links.copy_link_to_heading",
        CheckNote = "links.check_note",
        CheckFolder = "links.check_folder",
        CheckVault = "links.check_vault",
    }
    refactor {
        RenameHeading = "refactor.rename_heading",
    }
    editor {
        Undo = "editor.undo",
        Redo = "editor.redo",
        Cut = "editor.cut",
        Copy = "editor.copy",
        CopyAsMarkdown = "editor.copy_as_markdown",
        CopyAsRichText = "editor.copy_as_rich_text",
        Paste = "editor.paste",
        PastePlain = "editor.paste_plain",
        SelectAll = "editor.select_all",
        Newline = "editor.newline",
        NewlinePlain = "editor.newline_plain",
        Indent = "editor.indent",
        Outdent = "editor.outdent",
        RenumberList = "editor.renumber_list",
        MoveLineUp = "editor.move_line_up",
        MoveLineDown = "editor.move_line_down",
        DuplicateLine = "editor.duplicate_line",
        JoinLines = "editor.join_lines",
        SortLines = "editor.sort_lines",
        ChangeCase = "editor.change_case",
        ExpandSelection = "editor.expand_selection",
        ShrinkSelection = "editor.shrink_selection",
        ToggleComment = "editor.toggle_comment",
        ToggleWhitespace = "editor.toggle_whitespace",
        Bold = "editor.bold",
        Italic = "editor.italic",
        Strikethrough = "editor.strikethrough",
        Highlight = "editor.highlight",
        InlineCode = "editor.inline_code",
        InsertLink = "editor.insert_link",
        ToggleTask = "editor.toggle_task",
        Heading1 = "editor.heading_1",
        Heading2 = "editor.heading_2",
        Heading3 = "editor.heading_3",
        Heading4 = "editor.heading_4",
        Heading5 = "editor.heading_5",
        Heading6 = "editor.heading_6",
        Paragraph = "editor.paragraph",
        Quote = "editor.quote",
        InsertCallout = "editor.insert_callout",
        InsertFootnote = "editor.insert_footnote",
        Table = "editor.table",
        TableInsertRowAbove = "editor.table_insert_row_above",
        TableInsertRowBelow = "editor.table_insert_row_below",
        TableInsertColumnLeft = "editor.table_insert_column_left",
        TableInsertColumnRight = "editor.table_insert_column_right",
        TableDeleteRow = "editor.table_delete_row",
        TableDeleteColumn = "editor.table_delete_column",
        TableMoveRowUp = "editor.table_move_row_up",
        TableMoveRowDown = "editor.table_move_row_down",
        TableMoveColumnLeft = "editor.table_move_column_left",
        TableMoveColumnRight = "editor.table_move_column_right",
        TableSortByColumn = "editor.table_sort_by_column",
    }
    spell {
        AddToDictionary = "spell.add_to_dictionary",
        Ignore = "spell.ignore",
    }
}

/// The action of the command `id`, or `None` for an unknown command.
pub fn action_for(id: &str) -> Option<Box<dyn Action>> {
    ACTIONS
        .iter()
        .find(|(command, _)| *command == id)
        .map(|(_, build)| build())
}

/// The command whose action `action` is, if any.
pub fn command_of(action: &dyn Action) -> Option<&'static str> {
    let name = action.name();
    ACTIONS
        .iter()
        .find(|(_, build)| build().name() == name)
        .map(|(id, _)| *id)
}

/// Every command id that has an action, grouped by area.
pub fn command_ids() -> impl Iterator<Item = &'static str> {
    ACTIONS.iter().map(|(id, _)| *id)
}

/// The GPUI action name for a command id: `view.toggle_mode` becomes
/// `view::ToggleMode`.
pub fn action_name(id: &str) -> Option<String> {
    let (area, rest) = id.split_once('.')?;
    let mut name = format!("{area}::");
    for word in rest.split(['.', '_']) {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            name.extend(first.to_uppercase());
            name.push_str(chars.as_str());
        }
    }
    Some(name)
}
