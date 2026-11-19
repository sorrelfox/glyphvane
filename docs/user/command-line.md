# The command line

The `glyphvane` command opens files and folders in the app, and it has three
subcommands that work without opening a window: `render`, `export` and
`check-links`. Run `glyphvane --help` or `glyphvane <subcommand> --help` for
the same information in your terminal.

Where the command comes from:

- **deb, rpm and Arch packages:** `glyphvane` is installed on your `PATH`.
- **Windows installer (`.msi`):** the installer adds the Glyphvane folder to
  your user `PATH`, so `glyphvane` works in a newly opened terminal. To
  install without the `PATH` entry, run
  `msiexec /i Glyphvane-<version>-windows-<arch>.msi ADD_TO_PATH=0`.
- **`.tar.gz`, `.zip` and AppImage:** run the program from where you put it,
  or add that folder to your `PATH` yourself.
- **Flatpak:** run `flatpak run io.github.sorrelfox.Glyphvane` followed by the
  same arguments.

## Opening files and folders

```text
glyphvane [OPTIONS] [PATH[:LINE[:COL]]]...
```

- A file opens in a tab, in Preview.
- `PATH:LINE` and `PATH:LINE:COL` scroll to that line (and column).
- A folder opens as a vault, with the Files panel. Files inside that folder
  then open in its window.
- When Glyphvane is already running, the files open as tabs in its most
  recently used window.

| Option | What it does |
|---|---|
| `--theme <THEME>` | The theme for this run only, over the theme in your settings: `system` (follow the operating system), `light`, `semi-light` or `dark` |
| `--config-dir <DIR>` | Keep settings and the app's other files in this folder instead of the usual places, for example to test something without touching your own settings. Settings are read from `DIR/config/settings.toml`. A separate folder is a separate instance of the app |
| `--safe-mode` | Start in safe mode: conservative graphics settings (Mesa's software renderers on Linux, no DirectComposition on Windows), the default theme and no remote content, without changing your settings. Safe mode runs on its own and does not hand its files to a running Glyphvane |
| `--new-window` | Open the files in a new window, also when Glyphvane is already running |
| `--wait` | Open the files for editing and wait until they are closed (see [below](#using-glyphvane-as-editor)) |
| `--verbose` | Log more on standard error: informational messages and Glyphvane's own debug messages. `RUST_LOG`, when set, takes precedence |
| `--licenses` | Print the third-party notices (the licences and copyright notices of the components Glyphvane includes) and exit |
| `-h`, `--help` | Print help |
| `-V`, `--version` | Print the version |

Examples:

```sh
glyphvane notes/todo.md                 # open a file in Preview
glyphvane ~/Notes                       # open a folder as a vault
glyphvane README.md:42                  # open a file at line 42
glyphvane --new-window a.md b.md        # two files in a new window
glyphvane --config-dir /tmp/gv-test     # a clean, separate setup for testing
```

## Using Glyphvane as `$EDITOR`

With `--wait`, Glyphvane opens the files for editing in a new window, in the
editor rather than in Preview, and the command returns only when you close
them. That makes it usable as `$EDITOR`, `VISUAL` or `GIT_EDITOR`:

```sh
export EDITOR="glyphvane --wait"
git config --global core.editor "glyphvane --wait"
```

- A file counts as closed when you close its tab, show another file in that
  tab, close its window or quit the app.
- With autosave on, the file is saved before Glyphvane reports it closed, so
  the calling program reads the final text.
- With autosave off, unsaved changes are not written to the file. They are
  kept for recovery at the next start, and the command prints a message
  saying so. Git, for example, then aborts a commit whose message was never
  saved.
- When Glyphvane was not running, this process starts it and quits once the
  files are closed.
- `--wait` needs at least one file.

## `glyphvane render`

Renders a Markdown file to HTML, or to a syntax tree dump, without opening a
window.

```text
glyphvane render [OPTIONS] <IN>
```

| Argument or option | What it does |
|---|---|
| `<IN>` | The Markdown file, or `-` for standard input. The encoding is detected (a byte-order mark, UTF-8 or a legacy encoding) |
| `-o`, `--output <OUT>` | Write to this file instead of standard output (`-` is standard output). The file is replaced atomically |
| `--profile <P>` | The flavor profile: `commonmark` (the default), `gfm-file`, `gfm-comment`, `obsidian` or `extended` |
| `--format <FORMAT>` | `html` (the default, HTML for reading; for now the same as `commonmark-html`), `commonmark-html` (CommonMark-exact HTML, as the reference implementation writes it) or `cst` (the concrete syntax tree as a deterministic S-expression dump) |
| `--strict` | Fail with status 3, instead of warning, when the input does not decode exactly (for example invalid UTF-8) |

Exit status: `0` on success, `1` when the input cannot be read (or is binary)
or the output cannot be written, `2` for a usage error or an unsupported
profile, `3` when the input does not decode exactly and `--strict` is given.

```sh
glyphvane render notes.md -o notes.html
glyphvane render --profile gfm-file README.md > readme.html
cat draft.md | glyphvane render - --profile obsidian
```

## `glyphvane export`

Exports a Markdown file to a standalone HTML file or to a PDF, without
opening a window. HTML export uses the light theme, includes the images in
the file, and points links to other notes to their `.html` files.

```text
glyphvane export [OPTIONS] <--html|--pdf> <IN>
```

| Argument or option | What it does |
|---|---|
| `<IN>` | The Markdown file, or `-` for standard input |
| `--html` | Export to HTML |
| `--pdf` | Export to PDF. PDFs are made by `glyphvane-helper`, which must be installed next to `glyphvane` (all packages include it) |
| `--paper <SIZE>` | The PDF's paper: `a4` (the default), `letter`, `legal`, `a5` or `a3` |
| `-o`, `--output <OUT>` | Where to write. By default the input's name with `.html` or `.pdf`, next to it (standard output when reading standard input); `-` is standard output. The file is replaced atomically |
| `--profile <P>` | The flavor profile: `commonmark`, `gfm-file`, `gfm-comment`, `obsidian` or `extended`. By default, the one detected for the file |

Exit status: `0` on success, `1` when the input cannot be read, the PDF cannot
be made (no `glyphvane-helper`, or an engine error) or the output cannot be
written, `2` for a usage error (an unsupported profile or paper).

```sh
glyphvane export --pdf notes.md                  # writes notes.pdf
glyphvane export --pdf --paper letter notes.md -o ~/out.pdf
glyphvane export --html notes.md                 # writes notes.html
```

The export dialog in the app has more options; see
[Export and print](export-and-print.md).

## `glyphvane check-links`

Checks the links of a note, a folder or a whole vault, without opening a
window. It reports unresolved and ambiguous links, missing headings and block
IDs, letter case that differs, links that leave the vault, and blocked links.

```text
glyphvane check-links [OPTIONS] <PATH>
```

| Argument or option | What it does |
|---|---|
| `<PATH>` | A note (its links), a vault (every note) or a folder in a vault (its notes). The vault is the nearest folder above that contains `.obsidian`, `.glyphvane` or `.git`, otherwise the folder itself |
| `--profile <P>` | The flavor profile whose link rules apply: `commonmark`, `gfm-file`, `gfm-comment`, `obsidian` or `extended`. By default `obsidian` in an Obsidian vault, `gfm-file` in a Git repository and `extended` otherwise |
| `--format <FORMAT>` | `text` (the default): one line per finding, `path:line:column: severity: kind: link: message`, then a summary. `json`: an array of objects with `source`, `line`, `column`, `kind`, `severity`, `raw`, `status`, `target`, `candidates` and `message` |
| `--strict` | Fail with status 2 on warnings too |

Exit status: `0` when there are no errors, `1` when there are errors, `2`
when there are warnings and `--strict` is given, `3` for a usage error or a
path that cannot be read.

```sh
glyphvane check-links ~/Notes                    # the whole vault
glyphvane check-links docs/ --strict             # fail on warnings too, for CI
glyphvane check-links notes/index.md --format json > links.json
```

The same checks are available in the app's Link checker panel; see
[Links and vaults](links-and-vaults.md#the-link-checker).

## Exit status of the app itself

| Status | When |
|---|---|
| `0` | The app quit normally, the files were handed to a running Glyphvane, or the `--wait` files were closed |
| `1` | Something failed, or with `--wait`, Glyphvane ended before the files were closed |
| `2` | A usage error, such as an unknown option or `--wait` without a file |
