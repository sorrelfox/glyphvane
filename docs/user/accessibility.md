# Accessibility

## Using Glyphvane with the keyboard

Everything in Glyphvane can be reached from the keyboard.

| Key | What it does |
|---|---|
| **Tab** / **Shift+Tab** | Move between controls |
| **F6** / **Shift+F6** | Move to the next or previous part of the window (sidebar, document, right panel) |
| **F10** | Open the application menu; arrow keys move, **Enter** runs, **Esc** closes |
| **Shift+F10** or **Menu** | Open the context menu at the cursor or the selected item |
| **Ctrl+P** | The command palette: every command, by name |
| **Ctrl+O** | The quick switcher: every file, by name |
| **Enter** / **Space** | Press the focused button |

- The focused control shows a focus ring when you use the keyboard.
- In tab lists (such as the settings sections), one tab is a Tab stop; the
  arrow keys move between the tabs, and **Home** and **End** go to the first
  and last.
- All shortcuts can be changed in the keymap editor; see
  [Settings and shortcuts](settings-and-shortcuts.md#changing-shortcuts).

## Screen readers

Glyphvane describes its interface to screen readers through AccessKit, which
uses the platform's accessibility interface (AT-SPI on Linux, UI Automation on
Windows).

- **Controls** have a role, a name and their state (for example, whether a
  switch is on or a section is expanded).
- **The Reading view** exposes the document's structure: headings with their
  level, paragraphs, lists, links (with their destination and whether they
  resolve), images by their alt text, tables, code, math and embedded blocks.
- **The editor** exposes its text, the cursor and the selection, and
  scrolls when a screen reader asks: up and down, sideways on long lines
  without soft wrap, or to a position.
- **Canvases** expose their cards as a list.
- **Announcements:** information bars are announced when they appear
  (warnings and errors at once, other bars politely), and so are the number
  of matches in the find bar and other status changes such as a base's
  result count.

## Reduced motion

Glyphvane follows your system's reduced-motion preference: with it on, moving
to a card on a canvas does not animate, and animated images do not play.

## Themes and contrast

Glyphvane has three themes: **Light**, **Semi-light** (a warmer, paper-like
light theme) and **Dark**. **System** (the default) follows your
desktop's light or dark style, using Dark when it is dark and your preferred
light theme otherwise. Choose in Settings ▸ Appearance, cycle with **Ctrl+K
Ctrl+T**, or start with `glyphvane --theme dark`.

## Interface size

- **Interface font size** (Settings ▸ Appearance, 12 by default, from 8 to
  32) scales all interface text and line heights.
- **Zoom** (**Ctrl+=** and **Ctrl+-**) scales only the documents, from 50 % to
  300 %; see [Reading and editing](reading-and-editing.md#zoom).
- **Preview font** and **Editor font** set the document fonts and sizes.
- **Readable line width** keeps text in a centred column of a set width (72
  characters by default); turn it off to use the full window width.
