# Privacy

Glyphvane has no account, collects no telemetry and sends no personal data.
Your notes, settings and history stay on your computer.

## When Glyphvane uses the network

Glyphvane connects to the network in only two cases, and both need your
consent.

**Remote images and media in notes.** A note can show images or audio from
the web. Glyphvane loads them only as the remote-content setting allows:

- **Ask** (the default): a bar in the note names the sites, with **Load
  once**, **Always allow this site**, **Allow for this vault** and **Block
  this site**.
- **Allow**: loads them, but still asks for addresses on your own computer or
  local network.
- **Block**: never loads them. Set globally, no vault can change it.

**Restricted mode**, chosen when you first open a folder, blocks all remote
content in that vault. Safe mode blocks it too. Remote content is loaded only
in the Reading view. When exporting to HTML with **Embed remote images**,
only images your setting allows are downloaded. See
[Trust and remote content](links-and-vaults.md#trust-and-remote-content).

**The update check.** It is off unless you turn it on (during onboarding, in
Settings ▸ General, or when you choose Help ▸ Check for updates, which asks
whether to check once or daily). When on, Glyphvane asks GitHub
(`api.github.com`) at most once a day which release is the newest. It
downloads and installs nothing; when there is a newer version, it shows a
notice with a link to the release page. The update check is not available in
the Flatpak, where Flatpak keeps the app up to date, or in safe mode.

Things that never touch the network: link previews of web addresses (they
show only the address), link cards on canvases, `<iframe>` elements in notes,
and the link checker.

Opening a web link hands it to your browser. In Restricted mode, and with the
setting to confirm every hand-off, Glyphvane asks first.

## What Glyphvane stores

Everything is stored on your computer, in Glyphvane's own folders (see
[where settings are stored](settings-and-shortcuts.md#where-settings-are-stored)):

| What | Where |
|---|---|
| Settings and shortcuts | The settings folder |
| Open windows and tabs, recent files, onboarding and update-check state | The data folder |
| Unsaved changes (the recovery journal) and version history | The data folder |
| Per-vault decisions (trust, settings, bookmarks) | The data folder, never inside the vault |
| Crash reports, if you turn them on | The logs folder |

Glyphvane keeps its own state outside your vaults. It writes into a vault
only the notes and files you create or change, including the links it
updates for you when you rename or move files.

**Crash reports** are off by default. When you turn them on (during
onboarding or in Settings ▸ Privacy and security), a crash writes a
plain-text report to the `crash-reports` folder in the logs folder: the
version, the operating system, where the crash happened and a backtrace.
Nothing is sent; at the next start, a notice offers **Copy report** and
**Show report**, so you can attach it to a bug report yourself. Glyphvane
keeps the 20 newest reports.

**Copy debug info** (Help menu) copies the version, operating system, display
server, graphics card, theme, zoom, safe mode, the active document's profile
and the *names* of the settings you changed, never their values, your paths
or your documents. Nothing is sent unless you paste it somewhere.

## Flatpak permissions

The Flatpak asks for these permissions:

| Permission | Why |
|---|---|
| Your home folder (read and write) | Notes link to notes, images and attachments anywhere in your home folder, not only inside the opened folder. Glyphvane reads them to follow links and show embeds, and writes when you save, rename, move or delete; renames and moves update links in other notes |
| Network | Only for images and media that notes link from the web, after you allow them; the update check is off in the Flatpak |
| PulseAudio | Playing audio embedded in notes; Glyphvane never records |
| GPU (`/dev/dri`) | Drawing the window on the graphics card |
| Wayland, X11 fallback, IPC | Showing windows |

File choosers, printing, opening links in your browser and following your
light or dark style go through the desktop portals, which need no
permission.
