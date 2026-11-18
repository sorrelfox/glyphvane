# Changelog

All notable changes to Glyphvane are listed here. The format follows
Conventional Commits, and versions follow Semantic Versioning.

## [1.0.0] - 2026-11-18

The first stable release, with builds for Linux (x86_64 and arm64) and
Windows (x64 and arm64) on GitHub Releases. macOS builds will follow in a
later release; the code supports macOS and can be built from source.

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
