# Changelog

All notable changes to Glyphvane are listed here. The format follows
Conventional Commits, and versions follow Semantic Versioning.

## [Unreleased]

### PDF

- Text selection in the PDF viewer runs across pages: drag or Shift+click
  from one page to another to select and copy the text between; pages in
  two columns are read column by column, so find, selection and copying
  follow the reading order.
- Find in a PDF matches across line ends, page ends and a hyphen at a line
  end: a phrase continuing on the next line or page is found (and
  highlighted on both pages), and "infor-" followed by "mation" is found
  as "information".
  Copying joins words split at a soft hyphen.

### Reading

- Canvas cards show query results, base tables and media cards instead of
  placeholders, filled in off the UI thread and kept up to date with the
  vault; a click on a result opens it. Embedded canvases stay placeholders.

### Editing

- A text or code file switched between the viewer and Edit keeps the line
  at the top, both ways, edited or not.

## [1.2.0] - 2026-11-19

### PDF

- A built-in PDF viewer: pages drawn in the separate helper process, fit
  width, fit page or a zoom from 25 to 400 %, Go to page (Ctrl+G), and PDF
  embeds in notes.
- Find in a PDF (Ctrl+F, F3 and Shift+F3), with matches highlighted on the
  page; select text by dragging, double-click for a word, triple-click for a
  line, Shift+click to extend, and Ctrl+C to copy.
- A PDF tab reopens at its page, position and zoom after a restart, and Back
  and Forward return to where a PDF was left.

### Reading

- Note embeds show formatted, followable links, block content, footnotes,
  HTML, remote images and folds; they load off the UI thread.
- Inline math is typeset in search results and in backlink and mention
  snippets.
- Emoji shortcodes show in the Reading view and in exports, in profiles that
  support them.
- Canvas cards highlight code, draw diagrams, follow the Reading view for
  HTML blocks and show a placeholder for bases, queries and media.

### Commands and settings

- View > Viewer mode switches the current tab, and the choice is kept with
  the session. Fold to heading levels 4 to 6 work.
- "Confirm web links" asks once per site before opening a web link. Open link
  in split and the high-contrast setting are hidden until they exist.
- Help > Documentation opens the new user guide.

### Accessibility

- The editor reports its caret and selection to screen readers, and the
  Reading view and Live Preview text can be read past the visible screen.

### Fixes

- Scrolling to the end of a document no longer overshoots.
- Search finds inline tags in folder vaults, and its excerpts no longer show
  Markdown markup; backlink snippets, canvas cards, the compare view and
  Bases durations are fixed.
- In the Flatpak, the default-app setting explains how to set it and the
  update check is hidden.

### Packaging

- Third-party notices are generated from the dependency tree and checked
  before packaging; they include FreeType.
- The Flatpak can be installed from Glyphvane's own signed repository
  (`flatpak install https://sorrelfox.github.io/glyphvane/glyphvane.flatpakref`)
  and updated with `flatpak update`; the `.flatpak` bundle updates from it
  too.

## [1.1.0] - 2026-11-18

### Live Preview

- A new editing style in Edit mode (View > Live Preview, per tab, kept in
  the session): Markdown markers are hidden except where the caret or a
  selection touches them, and the caret moves over hidden text as one unit.
- Inline widgets: images, inline math, task checkboxes (click to toggle),
  emoji shortcodes and tags.
- Block widgets drawn like the Reading view: code blocks, tables, display
  math, diagrams, note and canvas/base embeds, callouts, horizontal rules,
  front matter and images on their own line. A block shows its source while
  the caret is inside it; links in blocks follow with Mod+click, and wide
  blocks scroll sideways.

### Zoom

- Clicking the zoom level in the status bar opens a menu of levels with Zoom
  in, Zoom out and Reset.
- Mod++ and Mod+wheel or pinch over the document zoom too; the zoom level is
  kept between launches.

### Fixes

- Code files in plain-text mode use a monospace font; viewer tabs take the
  keyboard when they open; the compare view header wraps in narrow windows.
- Load once approvals are shared between notes and bases; folds in the Split
  preview and renamed callouts are kept with the session.
- The home folder check ignores case on Windows.
- Typing before a byte-order mark at the start of a file no longer leaves an
  invalid interim parse.

### Packaging

- A Flatpak bundle (`io.github.sorrelfox.Glyphvane`, x86_64 and arm64, on
  the freedesktop 26.08 runtime) with access to the home folder. Inside a
  Flatpak the update check is off, since Flatpak updates the app.
- An Arch Linux package (x86_64); the helper is in `/usr/lib/glyphvane/`.

## [1.0.0] - 2026-11-18

The first stable release, with builds for Linux (x86_64 and arm64) and
Windows (x64 and arm64) on GitHub Releases.

### Reading and editing

- Reading view, Source view (Edit mode) and Split view with scroll sync and a
  draggable divider.
- A lossless, incremental parser with selectable flavor profiles: CommonMark,
  GitHub Flavored Markdown (file and comment), Obsidian and Extended.
- Tables, task lists, footnotes, alerts, details, pictures, front matter, math,
  Mermaid diagrams, highlighted code blocks, local and (opt-in) remote images,
  and audio embeds.
- Wide tables, diagrams and math fit the text column, grow beside it, or open
  in a view over the window.
- Editing commands for Markdown, lists, tables and footnotes; line commands,
  smart pairs, autocomplete, folding, soft wrap, find and replace, and spell
  check.
- Smart paste converts HTML from the clipboard to Markdown.
- Save, autosave, crash recovery with a compare and merge view, and handling
  of changes made on disk and of sync conflicts.

### Notes and vaults

- Precise link navigation across a folder of notes: relative links, wikilinks,
  headings and block references.
- Files panel with rename, move, duplicate and trash that update links, with
  undo; renaming headings and block ids updates links too.
- Outline, backlinks, unlinked mentions, tags, bookmarks, file history, daily
  notes and templates.
- Link checker panel, vault search with a query language, quick switcher and
  command palette.
- Read-only Canvas and Bases views, also embedded in notes.
- Git status, branch and badges.

### Export and print

- HTML and PDF export with an options dialog, page previews and bundled fonts.
- Printing through the desktop portal on Linux and natively on macOS and
  Windows.

### Application

- Light, Semi-light and Dark themes; interface and editor font settings; zoom.
- Settings window, keymap editor, shortcut cheatsheet, onboarding, settings
  import and export, update notice and crash reports.
- Session restore, single instance, `--wait` for use as `$EDITOR`, safe mode
  and a message when no GPU can be used.
- Accessibility support for views, dialogs, panels and tab lists.
- Command line: `glyphvane render`, `glyphvane export` and
  `glyphvane check-links` work without opening a window.

### Packaging

- `.deb`, `.rpm`, AppImage and `.tar.gz` for Linux; `.msi` and `.zip` for
  Windows.
- Each release carries SHA-256 checksums, build-provenance attestations and
  CycloneDX SBOMs.
