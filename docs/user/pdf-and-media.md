# PDFs, images, audio and other files

Besides Markdown, canvases and bases, Glyphvane has built-in viewers for PDFs,
images and text files, and plays audio embedded in notes.

| Files | Open in |
|---|---|
| `.pdf` | The PDF viewer (up to 200 MB) |
| `.png`, `.jpg`, `.jpeg`, `.gif`, `.webp`, `.bmp`, `.ico`, `.svg`, `.tif`, `.tiff` | The image viewer (up to 8192 pixels on a side) |
| Text and code files (many extensions, and names such as `Makefile`, `Dockerfile`, `LICENSE`) | The text viewer (up to 4 MB) |
| Video, audio, office documents, archives and other known binary files | Your default app, after asking |
| Anything else | The text viewer if it looks like text; otherwise your default app, after asking |

Glyphvane never starts programs: executables and launchers are only shown in
the file manager.

## The PDF viewer

The PDF viewer opens at the width of the window (**Fit width**). You can
switch to **Fit page** or a zoom level from 25 % to 400 %. When Glyphvane
starts again with your last session, a PDF tab comes back where you left it:
the same page, scroll position and zoom (the last page if the PDF has since
become shorter). The same goes for **Back** and **Forward**: after you
leave a PDF for another file, Back returns to the page, scroll position and
zoom you left.

| Key | What it does |
|---|---|
| **Up** / **Down** | Scroll |
| **Left** / **Right** | Previous / next page |
| **Page Up** / **Page Down**, **Space** / **Shift+Space** | Move by a screen |
| **Home** / **End** | First / last page |
| **Ctrl+G** | Go to page… |
| **0** | Fit width |
| **1** | 100 % |
| **Ctrl+F** | Find in the PDF |
| **Enter** / **Shift+Enter** (in the find field), **F3** / **Shift+F3** | Next / previous match |
| **Esc** | Close find |
| **Ctrl+A** | Select the text of the page shown |
| **Ctrl+C** | Copy the selected text |

**Linking to a page.** A link to `file.pdf#page=5` opens the PDF viewer at
page 5. In profiles with embeds, `![[file.pdf#page=5]]` shows a card with
**Open in PDF viewer**.

**Finding text.** **Ctrl+F** opens the find box at the top right of the
pages, with the same options as find in a note (**Aa** match case, **ab**
whole word, **.\*** regular expression); case is ignored unless you turn on
**Aa**. The count shows the current match and the number of matches, for
example "3 of 17". Glyphvane reads the text of the pages in the background,
starting with the page you are on, so in a long PDF the count grows for a
moment after you type. The current match is highlighted more strongly than
the others, and the PDF scrolls to it. A match cannot run from one page to
the next.

**Selecting and copying text.** Drag across text on a page to select it,
then press **Ctrl+C** to copy it. **Ctrl+A** selects all the text of the
page you are on. A selection stays on one page.

Text is read in the order the PDF draws it, which is usually reading order.
Some PDFs draw text in another order, for example a page in two columns, so
a search or a selection can run across columns. A scanned page has text only
if the PDF includes a text layer (from text recognition), and a PDF can only
be searched as far as it says which characters its fonts draw.

## The image viewer

Images open fitted to the window, never enlarged beyond their size.

| Key | What it does |
|---|---|
| **=** or **+** / **-** | Zoom in / out (10 % to 800 %) |
| **0** | Fit |
| **1** | 100 % |
| Arrow keys | Pan |
| **Ctrl+wheel** | Zoom |

Clicking an image in the Reading view opens it in the image viewer in a new
tab.

## The text viewer

Text and code files open read-only, with syntax highlighting for known
languages. You can select, copy and find text (**Ctrl+F**). Switch to Edit
(**Ctrl+E**) to edit the file as plain text; see
[Reading and editing](reading-and-editing.md#text-and-code-files).

## Audio

Audio files embedded in a note (`![[recording.mp3]]`, or an HTML `<audio>`
element) get a player with play and pause, a seek bar and mute.

| Key (player focused) | What it does |
|---|---|
| **Space** | Play or pause |
| **Left** / **Right** | Back or forward 5 seconds |
| **Home** / **End** | Start / end |

Supported formats: MP3, AAC (`.m4a`), ALAC, FLAC, Ogg Vorbis, Opus and WAV.

## Video

Glyphvane does not play video itself. An embedded video shows a card with its
type and size and **Open externally**, which opens it in your default video
player.
