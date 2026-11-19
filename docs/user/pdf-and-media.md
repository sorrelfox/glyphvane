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
| **Ctrl+wheel** | Zoom in or out, keeping the point under the pointer in place |
| **Ctrl+F** | Find in the PDF |
| **Enter** / **Shift+Enter** (in the find field), **F3** / **Shift+F3** | Next / previous match |
| **Esc** | Close find |
| **Ctrl+A** | Select the text of the page shown |
| Double-click / triple-click | Select a word / a line |
| **Shift**+click | Extend the selection |
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
moment after you type, and says "(searching…)" until every page has been
read. The current match is highlighted more strongly than
the others, and the PDF scrolls to it.

A search treats the end of a line as a space, so a phrase is found even
when it continues on the next line (with **.\*** on, a line end stays a line
break, for `^`, `$` and `\n`). The end of a page counts as a line end too: a
phrase that starts at the bottom of one page and continues at the top of the
next is found, counted once, and highlighted on both pages. This works once
Glyphvane has read the text of both pages.

Words split at the end of a line are found whole. A hyphen at the end of a
line, after a letter and before a lowercase letter on the next line, is
read both ways: "infor-" followed by "mation" is found when you search for
"information" and for "infor-mation" (a split compound such as "well-" and
"known" is found as "well-known"). Before a capital letter or a digit the
hyphen is kept ("Jean-Paul", "1990-2000"). Soft hyphens (invisible break
points some PDFs mark words with) are ignored.

**Selecting and copying text.** Drag across text on a page to select it,
then press **Ctrl+C** to copy it. Double-click to select a word, or
triple-click to select a line; keep the button down and drag to select more
whole words or lines. **Shift**+click moves the end of the selection to
where you click. While you drag, move the pointer to the top or bottom edge
of the view to scroll; the closer to the edge (or past it), the faster it
scrolls.
**Ctrl+A** selects all the text of the page you are on. A selection can run
from one page to another: drag past the end of a page, or **Shift**+click on
another page, and the text of every page between is selected too. Copied
text has a line break between pages. Copied text keeps the PDF's
hyphens and line breaks as they are, with one exception: a word split at a
soft hyphen at the end of a line (or page) is copied whole, without the
hyphen. Glyphvane reads the text of those pages
as the selection reaches them; if you copy before it has, the copy waits a
moment for it.

Text is read in the order the PDF draws it, which is usually reading order.
A page in two columns is read one column after the other, with a title or
anything else across both columns where it stands, so find, selection and
copying follow the columns. Other layouts (three columns, or text drawn out
of order) can still make a search or a selection jump. A scanned page has
text only if the PDF includes a text layer (from text recognition), and a
PDF can only be searched as far as it says which characters its fonts draw.

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
