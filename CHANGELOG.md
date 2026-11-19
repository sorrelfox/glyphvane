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

### Features

- Ctrl+Alt+Shift+click on a link opens it in a new window, in the Reading
  view and the editor, and on canvas cards, base rows and links, search
  results, query blocks and hover previews.
- Query blocks mark the matched words in their context lines, as the
  Search panel does.
- The hover preview of a link to a heading shows only that heading's
  section, up to the next heading of the same or a higher level.
### Documentation

- A sample vault (`docs/sample-vault`) tours the features with one short
  note each, plus a base, a canvas, an image and a PDF; the user guide
  says how to open it.

### Fixes

- A note renamed or moved by another program (a file manager, `git mv`,
  a sync client) keeps its version history when the vault watcher sees
  the rename and the file still holds the note's last kept version.
- New note (Ctrl+N) and notes created from the quick switcher go to the
  vault's default location for new notes (Obsidian's `newFileLocation`:
  the vault folder, the current note's folder, or a chosen folder).
- The Files panel's sort order is kept with the session.
- A callout written `[!type|meta]` shows as its type; the metadata is no
  longer part of its name or title.
- An image that cannot be drawn shows its alt text without the `|300` size.
- Property values chosen from autocomplete are quoted when YAML needs it
  (a `: `, a leading `#` or `[`, list separators in `[a, b]` lists).
- A PDF embed whose page cannot be drawn is not asked for again on every
  refresh.
- Screen readers read a Live Preview task checkbox as `[ ]` or `[x]`
  instead of a space.
- The link checker's *Create note* puts a note named by a bare link where
  the vault's `newFileLocation` says, as following the link does.
- A `%%…%%` comment that runs over a line break inside a paragraph is
  hidden in the Reading view, and its markers in Live Preview, as on one
  line.
- The mouse wheel scrolls the autocomplete list.
- The trust prompt names plugins, themes, snippets and shared settings
  added to the folder while it shows.
- Find in a PDF scrolls sideways to a match off the side of a zoomed page.
- Moving or renaming a folder updates `file.inFolder("…")` in bases.
- Renaming a note keeps the front matter valid YAML: a new name with `'`
  in a single-quoted link is written `''` (and escaped in double quotes).
- Renaming a heading or block ID updates `link("Note#Heading")` in bases.
- A click on a matching line of a query block opens the note at that line.
- A canvas, base or media card inside an embedded note opens its file as
  linked from the embedded note.
- Screen readers can scroll the editor sideways and to a position.
- A heading written over several lines (setext) can be renamed.
- Ctrl+wheel in the PDF viewer zooms at the pointer instead of the top of
  the view.
- An image inside an inline footnote (`^[…]`) takes its size in the
  footnote section when the size is known after the note is shown.
- Without a vault, an image that changes on disk shows its new version
  even while the Reading view is idle.
- A PDF embed's page is drawn again, sharper, after zooming in.
- The Bookmarks panel follows changes to Obsidian's `bookmarks.json`.
- Two Glyphvane windows of separate processes saving the same note at the
  same moment both keep their version in the file history.
- Performance: a remote image in a `<picture>` takes its place without
  building the whole document again.
- Performance: a query block stops its search when it leaves the view or
  another document opens.
- Performance: `[[##` and `[[^^` autocomplete gather the vault's headings
  and block IDs once per index update instead of on every key.
- Performance: a PDF page the viewer stopped waiting for no longer
  restarts the helper (and reopens the file) when it ends soon after.
- Renaming or moving a note or folder, and renaming a heading or block ID,
  update the `href` of HTML links (`<a href="Note.md#Heading">`), written
  as a URL (`%20` for a space). As in Obsidian, they are still not counted
  as links in backlinks or the link checker.
- Formulas in backlink context lines and search results use the math
  macros (`\newcommand`, `\def`) their note defines before them.
- Windows: files on ReFS volumes are told apart by their full 128-bit file
  ID, so renames by other programs and replaced files are recognised
  there as on NTFS.
- Screen readers reading the editor line by line no longer stop about a
  screen past the visible lines: the lines around the caret are always
  described, also when the view was scrolled away from it.
- Performance: query blocks and bases in a window without a vault pick up
  the vault's content index as soon as it opens or changes, instead of
  checking every 2 seconds.

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
