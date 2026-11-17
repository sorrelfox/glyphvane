// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What the application menu and the context menus hold, per platform.
//! Their contents are not designed: they follow the platforms' usual menus
//! and hold every command the application is to have.
//!
//! Items name commands of the keymap, so a menu runs exactly what the keys
//! run and shows their keys. Items whose command nothing handles where the
//! focus is are shown disabled, so the menus already list commands that are
//! not built yet: they stay greyed until a view handles them. Besides
//! those, Undo, Redo, Cut, Paste, Show whitespace, Soft wrap, the fold
//! items and the Format menu are disabled outside the editor (in Preview);
//! in the editor, the Format items that cannot run in the tab's profile
//! (strikethrough, highlight and task boxes in CommonMark) or at the carets
//! (bold in a code block). In viewer mode Edit and Split are disabled, and
//! so is everything that edits.

use gv_settings::keymap::Platform;

use crate::shell::ViewMode;

/// A menu: File, Edit, Format, View, Go, Help, on macOS the application
/// and Window menus, or a context menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    pub title: &'static str,
    pub entries: Vec<Entry>,
}

impl Menu {
    fn new(title: &'static str, entries: impl IntoIterator<Item = Entry>) -> Self {
        Self {
            title,
            entries: entries.into_iter().collect(),
        }
    }

    /// The command items, in order.
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.entries.iter().filter_map(|e| match e {
            Entry::Command(item) => Some(item),
            Entry::Separator | Entry::System(_) | Entry::Suggestion(_) => None,
        })
    }

    /// Whether some item shows a check mark.
    pub fn has_checks(&self) -> bool {
        self.items().any(|i| i.mode.is_some())
    }
}

/// One entry of a menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// Runs a command.
    Command(Item),
    Separator,
    /// An entry of the system's own (macOS).
    System(SystemEntry),
    /// Spelling suggestion `n` of the word the editor's menu was opened on
    /// (only in that menu: the suggestions come with it).
    Suggestion(usize),
}

/// A menu item that runs a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// The command id, as in `keymap.toml`.
    pub command: &'static str,
    pub title: &'static str,
    /// The item is checked while the active tab shows this mode.
    pub mode: Option<ViewMode>,
    /// The item is enabled only while this holds.
    pub requires: Option<Requirement>,
    /// What the system knows the item as (macOS routes these through the
    /// text system).
    pub role: Option<Role>,
}

/// A condition for an item to be enabled, besides its command having a
/// handler where the focus is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    CanGoBack,
    CanGoForward,
    /// The active tab may be edited (viewer mode is off, or does not apply
    /// to it).
    CanEdit,
    /// The tab the menu is about shows a file (not an untitled document or
    /// an empty tab).
    HasFile,
    /// The editor's menu was opened on a misspelled word.
    Misspelled,
    /// The same, in a vault (for the vault's dictionary).
    MisspelledInVault,
}

/// A standard edit item the system recognises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

/// Entries of the macOS application menu that the system provides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemEntry {
    Services,
    Hide,
    HideOthers,
    ShowAll,
}

impl SystemEntry {
    pub fn title(self) -> &'static str {
        match self {
            Self::Services => "Services",
            Self::Hide => "Hide Glyphvane",
            Self::HideOthers => "Hide others",
            Self::ShowAll => "Show all",
        }
    }
}

/// What the window tells the menu when it opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MenuState {
    pub mode: ViewMode,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    /// Viewer mode keeps the active tab out of Edit and Split.
    pub edit_locked: bool,
    /// The tab the menu is about shows a file.
    pub has_file: bool,
    /// The editor's menu is about a misspelled word.
    pub misspelled: bool,
    /// A vault is open (it has a dictionary of its own).
    pub in_vault: bool,
}

impl MenuState {
    pub fn allows(self, requirement: Requirement) -> bool {
        match requirement {
            Requirement::CanGoBack => self.can_go_back,
            Requirement::CanGoForward => self.can_go_forward,
            Requirement::CanEdit => !self.edit_locked,
            Requirement::HasFile => self.has_file,
            Requirement::Misspelled => self.misspelled,
            Requirement::MisspelledInVault => self.misspelled && self.in_vault,
        }
    }
}

const fn item(command: &'static str, title: &'static str) -> Entry {
    Entry::Command(Item {
        command,
        title,
        mode: None,
        requires: None,
        role: None,
    })
}

const fn mode_item(command: &'static str, title: &'static str, mode: ViewMode) -> Entry {
    let requires = match mode {
        ViewMode::Preview => None,
        ViewMode::Edit | ViewMode::Split => Some(Requirement::CanEdit),
    };
    Entry::Command(Item {
        command,
        title,
        mode: Some(mode),
        requires,
        role: None,
    })
}

const fn requiring(command: &'static str, title: &'static str, requires: Requirement) -> Entry {
    Entry::Command(Item {
        command,
        title,
        mode: None,
        requires: Some(requires),
        role: None,
    })
}

const fn with_role(command: &'static str, title: &'static str, role: Role) -> Entry {
    Entry::Command(Item {
        command,
        title,
        mode: None,
        requires: None,
        role: Some(role),
    })
}

const SEPARATOR: Entry = Entry::Separator;

/// The application's menus on `platform`, in menu bar order. On macOS the
/// application menu comes first and holds About, Check for updates,
/// Settings, the command-line install and Quit, and a Window menu comes
/// before Help (the system adds its window list). On Windows and Linux,
/// Settings and Quit end the File menu, and About and Check for updates
/// are under Help.
pub fn app_menus(platform: Platform) -> Vec<Menu> {
    let mac = platform == Platform::MacOs;
    let mut menus = Vec::new();
    if mac {
        menus.push(Menu::new(
            "Glyphvane",
            [
                item("app.about", "About Glyphvane"),
                item("app.check_for_updates", "Check for updates"),
                SEPARATOR,
                item("app.settings", "Settings…"),
                item("app.install_cli", "Install glyphvane command…"),
                SEPARATOR,
                Entry::System(SystemEntry::Services),
                SEPARATOR,
                Entry::System(SystemEntry::Hide),
                Entry::System(SystemEntry::HideOthers),
                Entry::System(SystemEntry::ShowAll),
                SEPARATOR,
                item("app.quit", "Quit Glyphvane"),
            ],
        ));
    }
    let mut file = vec![
        item("file.new_note", "New note"),
        item("tabs.new", "New tab"),
        item("workspace.new_window", "New window"),
        SEPARATOR,
        item("file.open", "Open file…"),
        item("file.open_folder", "Open folder…"),
        item("file.open_recent", "Open recent…"),
        SEPARATOR,
        item("file.save", "Save"),
        item("file.save_as", "Save as…"),
        item("file.revert", "Revert"),
        SEPARATOR,
        item("file.rename", "Rename…"),
        item("file.move", "Move to…"),
        item("file.duplicate", "Duplicate"),
        item("file.delete", "Move to trash"),
        SEPARATOR,
        item("workspace.reveal_in_tree", "Reveal in file tree"),
        item("file.reveal_in_file_manager", "Reveal in file manager"),
        item("file.open_with_default_app", "Open with default app"),
        item("bookmarks.toggle", "Bookmark"),
        item("file.history", "File history"),
        SEPARATOR,
        item("file.export_html", "Export as HTML…"),
        item("file.export_pdf", "Export as PDF…"),
        item("file.open_in_browser", "Open in browser"),
        item("file.print", "Print…"),
        SEPARATOR,
        item("tabs.close", "Close tab"),
        item("tabs.reopen_closed", "Reopen closed tab"),
    ];
    if !mac {
        file.extend([
            SEPARATOR,
            item("app.settings", "Settings…"),
            SEPARATOR,
            item("app.quit", "Quit"),
        ]);
    }
    menus.push(Menu::new("File", file));
    menus.push(Menu::new(
        "Edit",
        [
            with_role("editor.undo", "Undo", Role::Undo),
            with_role("editor.redo", "Redo", Role::Redo),
            SEPARATOR,
            with_role("editor.cut", "Cut", Role::Cut),
            with_role("editor.copy", "Copy", Role::Copy),
            item("editor.copy_as_markdown", "Copy as Markdown"),
            item("editor.copy_as_rich_text", "Copy as rich text"),
            item("links.copy_link_to_heading", "Copy link to heading"),
            with_role("editor.paste", "Paste", Role::Paste),
            item("editor.paste_plain", "Paste as plain text"),
            with_role("editor.select_all", "Select all", Role::SelectAll),
            SEPARATOR,
            item("editor.expand_selection", "Expand selection"),
            item("editor.shrink_selection", "Shrink selection"),
            SEPARATOR,
            item("search.find", "Find"),
            item("search.replace", "Replace"),
            item("search.find_next", "Find next"),
            item("search.find_previous", "Find previous"),
            item("search.vault", "Search in vault"),
            SEPARATOR,
            item("editor.move_line_up", "Move line up"),
            item("editor.move_line_down", "Move line down"),
            item("editor.duplicate_line", "Duplicate line"),
            item("editor.join_lines", "Join lines"),
            item("editor.sort_lines", "Sort lines"),
            item("editor.change_case", "Change case"),
        ],
    ));
    menus.push(Menu::new(
        "Format",
        [
            item("editor.bold", "Bold"),
            item("editor.italic", "Italic"),
            item("editor.strikethrough", "Strikethrough"),
            item("editor.highlight", "Highlight"),
            item("editor.inline_code", "Inline code"),
            SEPARATOR,
            item("editor.insert_link", "Insert link"),
            SEPARATOR,
            item("editor.heading_1", "Heading 1"),
            item("editor.heading_2", "Heading 2"),
            item("editor.heading_3", "Heading 3"),
            item("editor.heading_4", "Heading 4"),
            item("editor.heading_5", "Heading 5"),
            item("editor.heading_6", "Heading 6"),
            item("editor.paragraph", "Paragraph"),
            SEPARATOR,
            item("editor.quote", "Quote"),
            item("editor.insert_callout", "Insert callout…"),
            item("editor.toggle_task", "Toggle task"),
            SEPARATOR,
            item("editor.indent", "Indent"),
            item("editor.outdent", "Outdent"),
            item("editor.renumber_list", "Renumber list"),
            SEPARATOR,
            item("editor.insert_footnote", "Insert footnote"),
            item("editor.table", "Insert or format table"),
            item("notes.insert_template", "Insert template…"),
            SEPARATOR,
            item("editor.toggle_comment", "Toggle comment"),
        ],
    ));
    menus.push(Menu::new(
        "View",
        [
            mode_item("view.preview", "Preview", ViewMode::Preview),
            mode_item("view.edit", "Edit", ViewMode::Edit),
            mode_item("view.split", "Split", ViewMode::Split),
            SEPARATOR,
            item("view.toggle_mode", "Toggle Preview and Edit"),
            item("view.toggle_split", "Toggle split view"),
            item("view.toggle_viewer_mode", "Viewer mode"),
            SEPARATOR,
            item("workspace.toggle_sidebar", "Toggle sidebar"),
            item("workspace.toggle_panel", "Toggle right panel"),
            SEPARATOR,
            item("workspace.show_files", "Files"),
            item("workspace.show_tags", "Tags"),
            item("workspace.show_bookmarks", "Bookmarks"),
            item("workspace.show_recent", "Recent"),
            item("workspace.show_sync_conflicts", "Sync conflicts"),
            SEPARATOR,
            item("workspace.show_outline", "Outline"),
            item("workspace.show_backlinks", "Backlinks"),
            item("workspace.show_outgoing_links", "Outgoing links"),
            item("workspace.show_link_checker", "Link checker"),
            item("workspace.show_base", "Base"),
            SEPARATOR,
            item("editor.toggle_whitespace", "Show whitespace"),
            item("view.toggle_soft_wrap", "Soft wrap"),
            SEPARATOR,
            item("view.fold", "Fold"),
            item("view.unfold", "Unfold"),
            item("view.fold_all", "Fold all"),
            item("view.unfold_all", "Unfold all"),
            SEPARATOR,
            item("view.cycle_theme", "Cycle theme"),
            SEPARATOR,
            item("view.zoom_in", "Zoom in"),
            item("view.zoom_out", "Zoom out"),
            item("view.zoom_reset", "Reset zoom"),
            item("view.zoom_to_fit", "Zoom to fit"),
            SEPARATOR,
            item("window.toggle_full_screen", "Full screen"),
        ],
    ));
    menus.push(Menu::new(
        "Go",
        [
            requiring("navigation.back", "Back", Requirement::CanGoBack),
            requiring("navigation.forward", "Forward", Requirement::CanGoForward),
            SEPARATOR,
            item("workspace.quick_switcher", "Quick switcher…"),
            item("navigation.go_to_line", "Go to line…"),
            item("navigation.go_to_heading", "Go to heading…"),
            item("navigation.go_to_footnote", "Go to footnote or back"),
            SEPARATOR,
            item("links.follow", "Follow link"),
            item("links.open_in_new_tab", "Open link in new tab"),
            item("links.open_in_split", "Open link in split"),
            item("links.open_in_new_window", "Open link in new window"),
            item("links.preview", "Preview link"),
            item("links.list", "Links in this note…"),
            SEPARATOR,
            item("notes.daily_note", "Today's note"),
            item("notes.previous_daily_note", "Previous daily note"),
            item("notes.next_daily_note", "Next daily note"),
            item("navigation.next_misspelling", "Next misspelling"),
            SEPARATOR,
            item("workspace.focus_next_pane", "Next pane"),
            item("workspace.focus_previous_pane", "Previous pane"),
        ],
    ));
    if mac {
        menus.push(Menu::new(
            "Window",
            [
                item("window.minimize", "Minimize"),
                item("window.zoom", "Zoom"),
                SEPARATOR,
                item("window.bring_all_to_front", "Bring all to front"),
            ],
        ));
    }
    let mut help = vec![
        item("app.keymap_cheatsheet", "Keyboard shortcuts"),
        item("app.open_documentation", "Documentation"),
        SEPARATOR,
        item("app.copy_debug_info", "Copy debug info"),
        item("app.licenses", "Third-party licenses"),
    ];
    if !mac {
        help.extend([
            SEPARATOR,
            item("app.check_for_updates", "Check for updates"),
            item("app.about", "About Glyphvane"),
        ]);
    }
    menus.push(Menu::new("Help", help));
    menus
}

/// The context menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenu {
    /// On a link, in Preview and in the editor.
    Link,
    /// In the editor, off a link. The spelling items apply to a misspelled
    /// word (its suggestions come first, added when the menu opens on one)
    /// and the table items inside a table.
    Editor,
    /// On an entry of the file tree.
    FileTree,
    /// On a tab of the title row. The plan gives no contents (SPEC § Not
    /// designed yet): the usual tab menu, from existing commands.
    Tab,
    /// The title row's More actions button (`ellipsis`). Its menu is not
    /// designed: commands about the active document.
    MoreActions,
}

/// The items of a context menu. They are the same on every platform.
pub fn context_menu(kind: ContextMenu) -> Menu {
    match kind {
        ContextMenu::Link => Menu::new(
            "Link",
            [
                item("links.follow", "Open"),
                item("links.open_in_new_tab", "Open in new tab"),
                item("links.open_in_split", "Open in split"),
                item("links.open_in_new_window", "Open in new window"),
                SEPARATOR,
                item("links.copy_link", "Copy link"),
                item("links.copy_resolved_path", "Copy resolved path"),
                item("links.copy_url", "Copy URL"),
                SEPARATOR,
                item("links.reveal_in_tree", "Reveal in file tree"),
                item("links.reveal_in_file_manager", "Reveal in file manager"),
                SEPARATOR,
                item("links.show_trace", "Show resolution trace"),
                item("links.fix", "Fix link…"),
            ],
        ),
        ContextMenu::Editor => Menu::new(
            "Editor",
            [
                with_role("editor.cut", "Cut", Role::Cut),
                with_role("editor.copy", "Copy", Role::Copy),
                item("editor.copy_as_markdown", "Copy as Markdown"),
                with_role("editor.paste", "Paste", Role::Paste),
                item("editor.paste_plain", "Paste as plain text"),
                SEPARATOR,
                requiring(
                    "spell.add_to_dictionary",
                    "Add to dictionary",
                    Requirement::Misspelled,
                ),
                requiring(
                    "spell.add_to_vault_dictionary",
                    "Add to vault dictionary",
                    Requirement::MisspelledInVault,
                ),
                requiring("spell.ignore", "Ignore", Requirement::Misspelled),
                SEPARATOR,
                item("editor.table", "Format table"),
                item("editor.table_insert_row_above", "Insert row above"),
                item("editor.table_insert_row_below", "Insert row below"),
                item("editor.table_insert_column_left", "Insert column left"),
                item("editor.table_insert_column_right", "Insert column right"),
                item("editor.table_delete_row", "Delete row"),
                item("editor.table_delete_column", "Delete column"),
                item("editor.table_move_row_up", "Move row up"),
                item("editor.table_move_row_down", "Move row down"),
                item("editor.table_move_column_left", "Move column left"),
                item("editor.table_move_column_right", "Move column right"),
                item("editor.table_align_left", "Align column left"),
                item("editor.table_align_center", "Center column"),
                item("editor.table_align_right", "Align column right"),
                item("editor.table_align_none", "Remove column alignment"),
                item("editor.table_sort_by_column", "Sort by column"),
            ],
        ),
        ContextMenu::FileTree => Menu::new(
            "File tree",
            [
                item("file.new_note", "New note"),
                item("file.new_folder", "New folder"),
                SEPARATOR,
                item("file.rename", "Rename…"),
                item("file.move", "Move to…"),
                item("file.duplicate", "Duplicate"),
                item("file.delete", "Move to trash"),
                SEPARATOR,
                item("file.reveal_in_file_manager", "Reveal in file manager"),
                item("file.open_with_default_app", "Open with default app"),
            ],
        ),
        ContextMenu::Tab => Menu::new(
            "Tab",
            [
                item("tabs.close", "Close"),
                item("tabs.close_others", "Close others"),
                item("tabs.close_to_right", "Close to the right"),
                SEPARATOR,
                requiring("file.copy_path", "Copy path", Requirement::HasFile),
                requiring(
                    "workspace.reveal_in_tree",
                    "Reveal in file tree",
                    Requirement::HasFile,
                ),
                requiring(
                    "file.reveal_in_file_manager",
                    "Reveal in file manager",
                    Requirement::HasFile,
                ),
            ],
        ),
        ContextMenu::MoreActions => Menu::new(
            "More actions",
            [
                requiring("file.copy_path", "Copy path", Requirement::HasFile),
                requiring(
                    "workspace.reveal_in_tree",
                    "Reveal in file tree",
                    Requirement::HasFile,
                ),
                requiring(
                    "file.reveal_in_file_manager",
                    "Reveal in file manager",
                    Requirement::HasFile,
                ),
                item("file.open_with_default_app", "Open with default app"),
                SEPARATOR,
                item("bookmarks.toggle", "Bookmark"),
                item("file.history", "File history"),
                item("links.list", "Links in this note…"),
                SEPARATOR,
                item("search.find", "Find"),
                item("view.toggle_viewer_mode", "Viewer mode"),
                SEPARATOR,
                item("file.export_html", "Export as HTML…"),
                item("file.export_pdf", "Export as PDF…"),
                item("file.print", "Print…"),
            ],
        ),
    }
}
