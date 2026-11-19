# Settings and shortcuts

## The settings window

Open the settings with **Ctrl+,** (File ▸ Settings…). The sections are listed
on the left, and a search field finds any setting by name. Switches and lists
apply at once; a text or number field applies when you press **Enter** or
leave the field. A value that is not allowed is refused, and the row says
why.

The **scope switch** at the top chooses what you change:

- **All vaults** changes your global settings.
- The vault's name changes the settings for that vault only. Settings that
  only make sense globally (such as the update check) are greyed out here.

| Section | What you find there |
|---|---|
| **General** | `open_files_in`: whether files you open from the file manager or the command line start in the Reading view (the default), the editor, or the mode you used last. `viewer_mode`: open every document read-only, so that Edit and Split are unavailable (untitled notes and `--wait` files stay editable). `check_for_updates`: the update check (off by default) |
| **Appearance** | Theme (System, Light, Semi-light, Dark), the light theme to use when the system is light, high contrast, the interface, preview and editor fonts and their sizes, readable line width (a centred column, 72 characters by default), how wide tables and diagrams may grow, zoom, and the code theme |
| **Editor** | `prose_font` (use the preview font in the editor), `larger_headings`, `caret_blink_idle_secs` (the caret stops blinking after this many idle seconds), `wrap_pasted_urls` (paste a lone URL as `<url>`), `emoji` (insert a shortcode or the character), `footnote_definitions` (where *Insert footnote* puts the footnote: end of document or after the paragraph), `sync_caret_from_reading` (clicking in the Reading view moves the editor's cursor there), what the word counts leave out, and when large-file mode starts |
| **Preview** | `hover_preview_delay_ms` (300 by default), and limits for embeds: how deep they nest (5) and how large they may get |
| **Links** | `use_markdown_links` (new links as Markdown links rather than wikilinks), `new_link_format` (shortest, relative or absolute paths), `markdown_link_encoding` (how spaces in Markdown links are written), `map_web_urls_to_files`, `history_size` (back and forward entries per tab). Vaults with Obsidian settings use Obsidian's link style instead |
| **Files** | `autosave` (on by default), `attachment_folder` (see [Attachments](links-and-vaults.md#attachments-and-drag-and-drop)), `external_files` (what happens to files dropped from outside the vault: copy, link or ask) |
| **Vault** | `obsidian_config_folders` (other names for the `.obsidian` folder), `network_poll_interval_secs` (how often folders on network drives are checked for changes) |
| **Profiles** | `default_profile` and `vault_profile`; see [Markdown and profiles](markdown-and-profiles.md#how-the-profile-is-chosen) |
| **Spelling** | `enabled`, `languages`, `skip_words_with_digits`, `skip_all_caps` |
| **Keymap** | The [keymap editor](#changing-shortcuts) |
| **Privacy and security** | Remote content (Allow, Ask, Block) and lists of allowed and blocked sites, workspace trust, confirming every link or file handed to another app, embeds from outside the vault, and local crash reports; see [Privacy](privacy.md) |
| **About** | The version, where the settings files are, safe mode, and [import and export](#importing-and-exporting-settings) |

### Where settings are stored

| | Settings (`settings.toml`, `keymap.toml`, `dictionaries/`) | Data (session, history, recovery) | Logs and crash reports |
|---|---|---|---|
| Linux | `~/.config/glyphvane/` | `~/.local/share/glyphvane/` | `~/.local/state/glyphvane/log/` |
| Flatpak | `~/.var/app/io.github.sorrelfox.Glyphvane/config/glyphvane/` | `~/.var/app/io.github.sorrelfox.Glyphvane/data/glyphvane/` | below `~/.var/app/io.github.sorrelfox.Glyphvane/` |
| Windows | `%APPDATA%\sorrelfox\Glyphvane\` | `%LOCALAPPDATA%\sorrelfox\Glyphvane\` | `%LOCALAPPDATA%\sorrelfox\Glyphvane\logs\` |

On Linux, the `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_STATE_HOME`
variables are honoured. Settings ▸ About shows the exact paths, and the
settings window has a row that opens `settings.toml` in Glyphvane. With
`--config-dir DIR`, everything goes below `DIR` instead (see
[The command line](command-line.md)).

`settings.toml` is a TOML file with one table per section, using the names
shown in the settings window:

```toml
version = 1

[appearance]
theme = "dark"
editor_font_size = 14.0

[files]
autosave = true
attachment_folder = "./attachments"

[privacy_and_security]
remote_content = "block"
```

Glyphvane notices changes to the file while it runs.

### Settings for one vault

Settings chosen in a vault's scope are kept in Glyphvane's data folder
(`vaults/<vault-id>/settings.toml`), not in the vault. A vault can also carry
shared settings for everyone who opens it, in `.glyphvane/settings.toml`
inside the vault. Glyphvane never writes that file, and it can only make
privacy and security settings stricter, never looser.

From the lowest to the highest priority, settings come from: the defaults,
your global settings, the vault's shared file, your settings for that vault,
and settings for a single file.

### Importing and exporting settings

Settings ▸ About has **Export…** and **Import…** (also *Export settings* and
*Import settings* in the command palette).

- **Export** writes your global settings (only valid values, without
  comments) to a TOML file, `glyphvane-settings.toml` by default. Vault
  settings and the keymap are not included.
- **Import** shows a preview with one row per change, its current and new
  value, and a switch to take it or not. Changes that would make a privacy or
  security setting less strict are marked and start switched off. Settings
  the file does not mention stay as they are.

## Changing shortcuts

The **Keymap** section of the settings is the keymap editor. It lists every
command by category; search by name, command ID or key.

- **Add** records a new shortcut (up to two keystrokes, such as
  **Ctrl+K Ctrl+T**); **Esc** cancels.
- If the keys are already used where the command applies, the editor says so
  and offers **Save**, **Replace** or **Keep both**.
- **×** removes a key, and **Reset** brings back the built-in keys.
- A switch chooses whether a change applies on **All platforms** or **This
  platform** only.

Help ▸ Keyboard shortcuts shows a cheat sheet of the keys in effect.

### `keymap.toml`

The keymap editor writes `keymap.toml` in the settings folder, and you can
edit that file yourself; Glyphvane applies changes at once, without a
restart.

```toml
version = 1

# Bind keys to a command (command IDs are shown in the keymap editor).
[[bind]]
keys = "ctrl-shift-k"
command = "editor.insert_link"

# Only in some places: a context expression.
[[bind]]
keys = "ctrl-k ctrl-t"
command = "view.cycle_theme"
context = "!editor"

# Remove built-in bindings, by keys, by command, or both.
[[unbind]]
keys = "f6"

[[unbind]]
command = "workspace.command_palette"
keys = "ctrl-shift-p"

# Entries for one platform only.
[[windows.bind]]
keys = "ctrl-alt-l"
command = "editor.toggle_task"
```

- **Keys:** modifiers joined to the key with `-` (`ctrl-shift-p`); the steps
  of a chord separated by a space (`ctrl-k ctrl-t`). Modifiers are `ctrl`,
  `alt`, `shift`, `super` (also `win`) and `secondary` (Ctrl on Linux and
  Windows). Keys are single characters (`a`, `,`, `[`; `ctrl--` for
  Ctrl+minus) or names: `enter`, `escape`, `tab`, `backspace`, `delete`,
  `insert`, `space`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`,
  `pagedown`, `menu`, `f1` to `f35`.
- **Contexts:** without `context`, a binding applies wherever its command
  does; `context = ""` means everywhere. Contexts name where the keyboard
  focus is: `editor`, `reader` (the Reading view), `tree` (the Files panel),
  `outline`, `search`, `palette`, `switcher`, `menu`, `infobar` and
  `settings`, plus the states `autocomplete` and `split`. Combine them with
  `!`, `&&`, `||` and parentheses.
- **Platform sections:** `[[linux.bind]]`, `[[windows.bind]]` (and
  `[[linux.unbind]]` and so on) apply on that platform only.
- **Precedence:** your bindings win over built-in ones on the same keys; then
  the more specific context wins; then the later entry.
- `[[unbind]]` removes built-in bindings only; to remove one of yours, delete
  its entry.
- Mistakes never stop the file from loading: invalid entries are left out and
  reported in the log with their line and column.

## Default shortcuts

These are the built-in shortcuts on Linux and Windows. Commands that are not
listed here have no default key: run them from the command palette
(**Ctrl+P**), or give them a key in the keymap editor. macOS is not
supported.

### Application

| Command | Keys |
|---|---|
| Quit | **Ctrl+Q** |
| Open settings | **Ctrl+,** |
| Open application menu | **F10** |

### File

| Command | Keys |
|---|---|
| New note | **Ctrl+N** |
| Save | **Ctrl+S** |
| Save as | **Ctrl+Shift+S** |

### Tabs

| Command | Keys |
|---|---|
| Close tab | **Ctrl+W** |
| Reopen closed tab | **Ctrl+Shift+T** |

### Workspace

| Command | Keys |
|---|---|
| Command palette | **Ctrl+P** or **Ctrl+Shift+P** |
| Quick switcher | **Ctrl+O** |
| Focus next pane | **F6** |
| Focus previous pane | **Shift+F6** |
| Open context menu | **Menu** or **Shift+F10** |

### View

| Command | Keys |
|---|---|
| Toggle Preview and Edit | **Ctrl+E** |
| Toggle split view | **Ctrl+Alt+E** |
| Cycle theme | **Ctrl+K Ctrl+T** |
| Zoom in | **Ctrl+=** or **Ctrl++** |
| Zoom out | **Ctrl+-** |
| Reset zoom | **Ctrl+0** |

### Navigation

| Command | Keys |
|---|---|
| Go back | **Alt+Left** |
| Go forward | **Alt+Right** |
| Go to line | **Ctrl+G** |
| Go to heading | **Ctrl+Shift+O** |

### Search

| Command | Keys |
|---|---|
| Search in vault | **Ctrl+Shift+F** |
| Find | **Ctrl+F** |
| Find and replace | **Ctrl+H** |
| Find next | **F3** |
| Find previous | **Shift+F3** |

### Links

| Command | Keys |
|---|---|
| Follow link | **Alt+Enter** |
| Open link in new tab | **Ctrl+Alt+Enter** |

### Editing

| Command | Keys |
|---|---|
| Undo | **Ctrl+Z** |
| Redo | **Ctrl+Shift+Z** or **Ctrl+Y** |
| Cut | **Ctrl+X** |
| Copy | **Ctrl+C** |
| Paste | **Ctrl+V** |
| Paste as plain text | **Ctrl+Shift+V** |
| Select all | **Ctrl+A** |
| New line, continuing the list or quote | **Enter** |
| New line without continuing the list | **Shift+Enter** |
| Indent | **Tab** |
| Outdent | **Shift+Tab** |
| Move line up | **Alt+Up** |
| Move line down | **Alt+Down** |
| Expand selection | **Shift+Alt+Right** |
| Shrink selection | **Shift+Alt+Left** |
| Toggle comment | **Ctrl+/** |

### Formatting

| Command | Keys |
|---|---|
| Bold | **Ctrl+B** |
| Italic | **Ctrl+I** |
| Strikethrough | **Ctrl+Shift+X** |
| Highlight | **Ctrl+Shift+H** |
| Inline code | **Ctrl+Shift+C** |
| Insert link | **Ctrl+K** |
| Toggle task | **Ctrl+L** |
| Heading 1 | **Ctrl+Alt+1** |
| Heading 2 | **Ctrl+Alt+2** |
| Heading 3 | **Ctrl+Alt+3** |
| Heading 4 | **Ctrl+Alt+4** |
| Heading 5 | **Ctrl+Alt+5** |
| Heading 6 | **Ctrl+Alt+6** |
| Paragraph | **Ctrl+Alt+0** |
| Quote | **Ctrl+Shift+.** |
| Insert or format table | **Ctrl+Alt+T** (Windows only) |

**Ctrl+K Ctrl+T** (Cycle theme) is a chord: press Ctrl+K, then Ctrl+T. It
does not work in the editor, where Ctrl+K inserts a link. **Ctrl+Alt+T** has
no default on Linux, because most desktops use it to open a terminal.

### Keys that are not commands

Some keys belong to the part of the window that has the focus and cannot be
changed in the keymap:

| Where | Keys |
|---|---|
| Reading view | Arrow keys, **Page Up**, **Page Down**, **Space**, **Home** and **End** scroll |
| Editor | **Alt+click** adds a cursor, **Alt+drag** selects a column; **Shift+Alt+Up** / **Shift+Alt+Down** (Linux) or **Ctrl+Alt+Up** / **Ctrl+Alt+Down** (Windows) add a cursor above or below |
| Files panel and menus | Arrow keys move, **Enter** opens, **Esc** closes a menu |
| Canvas, bases, PDF and image viewers | See [Canvas and Bases](canvas-and-bases.md) and [PDFs, images and media](pdf-and-media.md) |
