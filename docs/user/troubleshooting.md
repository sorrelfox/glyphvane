# Troubleshooting

## Glyphvane does not start: "the graphics driver could not be used"

Glyphvane draws its windows with the graphics card: through Vulkan or OpenGL
ES 3 on Linux, and Direct3D 11 (feature level 10.1 or later) on Windows. When
it cannot, it shows the message **Glyphvane cannot open a window: the graphics
driver could not be used.** with advice for your system, and offers **Start
in Safe Mode**. The details are saved in `gpu-failure.txt` in the logs folder
(see [where settings are stored](settings-and-shortcuts.md#where-settings-are-stored)).

On **Linux**:

- Install or update the Vulkan driver for your graphics card. With Mesa, that
  is your distribution's Mesa Vulkan drivers package.
- In a virtual machine or without a graphics card, install Mesa's software
  Vulkan driver (lavapipe) or software OpenGL driver (llvmpipe). Glyphvane uses
  them when no graphics card works.
- Start Glyphvane from a terminal with `RUST_LOG=info glyphvane` to see which
  graphics adapters were tried.

On **Windows**, install or update the driver for your graphics card. Without
a usable graphics card, Windows normally provides a software renderer
(Microsoft Basic Render Driver) that Glyphvane uses.

## Safe mode

Safe mode starts Glyphvane with conservative graphics settings (Mesa's
software renderers on Linux, no DirectComposition on Windows), the default
theme and no remote content, without changing your settings. Use it when the
window is drawn wrongly, the app crashes at start, or you suspect a setting.

```sh
glyphvane --safe-mode
```

In safe mode nothing is saved to your settings, the keymap or the app's
state, and no version history is recorded; Settings ▸ About says that safe
mode is on. Safe mode runs as its own instance: it does not hand files to a
Glyphvane that is already running.

## The Flatpak cannot see some files

The Flatpak has access to your home folder. Files elsewhere, such as on a
second drive mounted under `/mnt` or `/media`, are not visible to it. Grant
access to such a folder with:

```sh
flatpak override --user --filesystem=/mnt/data io.github.sorrelfox.Glyphvane
```

The Flatpak keeps its settings and data in
`~/.var/app/io.github.sorrelfox.Glyphvane/`, separate from a Glyphvane
installed from another package.

## Windows says "Windows protected your PC"

The Windows builds may not be code-signed yet, so SmartScreen can warn about
the installer or the program the first time you run it. Choose **More info**,
then **Run anyway**. You can check the download first against the
`SHA256SUMS` file published with each release:

```powershell
Get-FileHash .\Glyphvane-<version>-windows-x64.msi
```

## Fonts look different than expected

Glyphvane uses Inter for the interface and the Reading view and JetBrains
Mono for the editor when they are installed; otherwise it falls back to the
system font. Install these fonts, or choose other fonts in Settings ▸
Appearance (the interface font can also be `system`, the operating system's
interface font).

On Linux, colour emoji need the Noto Color Emoji font. If the installed one
cannot be drawn, Glyphvane uses the copy that comes with its packages.

PDF export always uses the fonts bundled with Glyphvane, so PDFs look the
same everywhere.

## Wayland and X11

On Linux, Glyphvane uses Wayland when the `WAYLAND_DISPLAY` variable is set,
and X11 otherwise. To try X11 in a Wayland session (through XWayland), start
it with that variable removed:

```sh
env -u WAYLAND_DISPLAY glyphvane
```

## A file shows strange characters or opens read-only

- Glyphvane detects the encoding of each file. If a file in a legacy encoding
  cannot be saved in it, Glyphvane offers **Save as UTF-8**.
- Very large files open in large-file mode, with some features off, or
  read-only when they are too large to edit safely. The limits are settings
  in the Editor section.

## Links do not resolve

Open the [link checker](links-and-vaults.md#the-link-checker), or right-click
the link and choose **Show resolution trace** to see how Glyphvane looked for
the target. Check that the document's [flavor
profile](markdown-and-profiles.md) supports the kind of link you use; for
example, CommonMark and GitHub have no wikilinks.

## Remote images do not show

Images from the web load only with your consent; see
[Trust and remote content](links-and-vaults.md#trust-and-remote-content).
They never load in safe mode or Restricted mode.

## Logs

Glyphvane writes its log messages to standard error. To see them, start it
from a terminal:

```sh
glyphvane --verbose                # more detail
RUST_LOG=debug glyphvane           # any level; RUST_LOG takes precedence
```

Crash reports, if you turned them on, are in the `crash-reports` folder in
the logs folder; see [Privacy](privacy.md#what-glyphvane-stores).

## Reporting a bug

1. Choose **Help ▸ Copy debug info**. It copies the version, operating system,
   display server, graphics card, theme, zoom, safe mode, the document's
   profile and the names (not the values) of settings you changed.
2. Open an issue at
   [github.com/sorrelfox/glyphvane/issues](https://github.com/sorrelfox/glyphvane/issues),
   describe what you did, what you expected and what happened, and paste the
   debug info.
3. If there is a crash report, attach it as well, after checking it contains
   nothing private.

To report a security problem, follow
[the security policy](../../SECURITY.md) instead of opening a public issue.
