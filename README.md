# Alacritty Tabbed

Alacritty Tabbed ([`alacritty-tabbed`](https://github.com/Mano-Liaoyan/alacritty-tabbed))
is a soft fork of [Alacritty](https://github.com/alacritty/alacritty) that adds
tabs, shell profiles and a settings panel on Windows, Linux and macOS. It is not
an official Alacritty release. Upstream's README follows the fork notes below.

![Tabs in the caption, with the new-tab menu open](docs/tabs/tabs.png)

## Features

**Tabs in the caption.** The tab bar replaces the window's title bar: the logo
and computer name, the tabs on a rounded track, the + button with its profile
menu, settings, and the window controls share one row, and the terminal sits in
a rounded panel below it. Drag the empty bar to move the window, double-click it
to maximize, right-click it for the system menu (Windows and Wayland), and drag
the edges to resize. Fullscreen keeps the bar and fills the rest of the screen
with the terminal. The layout is the same everywhere; the window controls take
each system's look: flat buttons on Windows, round ones on Linux, and the red,
yellow and green lights on macOS.

**Tabs.** Each tab is a full Alacritty terminal with its own PTY and state;
the tabs of a window share its renderer. Drag tabs to reorder them, close them
with × or a middle click, and scroll over the bar to switch.

**Tab groups.** A tab can hold several terminals, its panes, split side by side
or stacked, like tmux's windows. Each pane is its own terminal with its own
profile; panes in the background keep running and drawing. The focused pane has
a lit border, and a grouped tab shows its panes' logos stacked like cards, the
focused one in front. Groups have no size limit: past four panes the stack
shows three cards and a "+N" count.

- **Group** tabs by dragging one onto the middle of another, or of a group.
- **Split** by dragging a tab into the terminal: it docks beside the pane under
  the pointer, on the side nearest it, which decides between side by side and
  stacked; a preview shows where it will land. A group dragged there docks as it
  is. The tab dragged is shown while it is pressed, so pick the tab to dock into
  first.
- **Rearrange** by dragging a pane's logo from its tab's stack, the ••• handle
  on top of a pane, or anywhere on a pane with Ctrl+Alt held (⌘⌥ on macOS;
  plain Ctrl+drag still selects a block): into the terminal to dock it beside
  another pane, onto another tab to move it there, or between tabs to make it a
  tab of its own again. Merging is refused when a pane would get too small.
- **Tear off** a tab or a pane by dropping it outside the window: it opens in a
  new window and keeps running. "Move … to New Window" in the menus does the
  same.
- **Resize** by dragging the divider between two panes; double-click it to go
  back to half and half. Ctrl+Shift+Z (Cmd+Shift+Enter on macOS) or "Zoom
  Pane" zooms the focused pane to the whole tab and back.
- **Hover** a tab's stack to fan its logos out; large groups wrap into a shelf
  below the bar. Resting on a logo shows its pane's own title, and for a pane
  that is not on screen a live preview of it. Click a logo to focus its pane.
- **Right-click** a pane for its menu: Split Vertically (side by side), Split
  Horizontally (stacked), Zoom Pane, Move Pane to New Tab, Move Pane to New
  Window and Close Pane. Arrow keys, Enter and Escape work. When the program in
  the pane takes the mouse, hold Shift; Ctrl+right-click still expands the
  selection.
- **Right-click** a tab for its menu: Rename Group… (or double-click the
  title), Reset Name, Ungroup, Move Tab to New Window and Close Tab.
- Click a pane to focus it. Ctrl+Shift+W closes the focused pane; × and a
  middle click close the whole tab.

Under Appearance in the settings, or in `alacritty.toml`, set the space between
panes and the focused pane's border colour, which the settings pick with a
colour picker:

```toml
[window]
pane_gap = 16                # Logical pixels between panes, 0 to 48.
pane_highlight = "#ff9500"   # Leave out for the terminal's blue.
```

Panes slide and pop into place with Hyprland-like curves. Set
`ALACRITTY_ANIMATION_SCALE` to slow them down (`4` is four times slower) or to
`0` to turn them off.

**Profiles with logos.** Tabs, the ⌄ menu beside + and the settings show each
profile's logo, drawn sharp at the screen's scale: the executable's largest icon
on Windows, with each WSL distribution's own artwork and Git Bash's logo; the
icon theme's logo on Linux, preferring SVG; the application's icon on macOS.
About 40 common shells, tools, languages and WSL distributions have bundled logos
where the system has none, and other programs get a terminal. A profile can set its own SVG or PNG, or an emoji.

**Settings panel.** Like Windows Terminal's, the settings list the
application's pages above the profiles. The gear opens **Appearance**: the
corners, the interface font and its size, the background opacity, the
terminal panel, the pane gap, the focus highlight and the tab track, saved to
`alacritty.toml` (changing only those keys, and keeping its comments) and
applied to every window. "Edit profiles…" in the ⌄ menu opens the profiles:
name, icon, command line, starting directory, font, environment variables, configuration overrides,
the default profile, and whether a profile shows in the menu. The font field
lists the system's monospaced fonts as you type; "Show all fonts" lists every
font. Clicking the profile's icon picks one of the bundled logos, and Browse
picks your own. Saved changes apply to the tabs already open with a profile. Browse buttons use the
system's file pickers. Profiles can be added, duplicated, deleted, and dragged
to reorder them. They are saved to `profiles.toml` beside `alacritty.toml`,
which can also be edited by hand.

![Profile settings](docs/tabs/settings.png)

**Interface font.** The bar, its menus and the settings use a proportional
interface font of their own, apart from the terminal's font: the system's
interface font on macOS (San Francisco, else Helvetica Neue) and Windows
(Segoe UI), and on Linux the first installed of Inter, Adwaita Sans, Noto Sans
and other common interface fonts. Emphasized text uses the family's SemiBold or Medium face when it has
one. Choose another family or size under Appearance in the settings, or in
`alacritty.toml`; changes apply live:

```toml
[window.ui_font]
family = "Inter"  # Empty picks the system's interface font.
size = 10         # Points; the default is 10, or 13 on macOS.
```

**Corners.** The window, the terminal's panel, the tab track, the tabs, the
buttons and the settings are all rounded, with radii that nest: each surface
inside another is rounded by the outer radius less the space between them, so
their curves run parallel. Or make every corner square, under Appearance in the
settings or in `alacritty.toml`; changes apply live:

```toml
[window]
corners = "square"  # "round" (default) or "square".
```

On Linux, round window corners are cut out of the window, which needs a
compositor; without one, choose square corners. Windows rounds the window
itself, and macOS always does.

## Disable

Tabs are on by default. To turn them off:

```toml
[window]
tabs = false
```

Restart Alacritty after changing `window.tabs`.

| Keys (Windows, Linux) | macOS | Action |
| --- | --- | --- |
| Ctrl+Shift+T, + | Cmd+T, + | New tab with the default profile |
| ⌄ beside + | ⌄ beside + | Choose a profile (arrow keys and Enter work) |
| Ctrl+Tab / Ctrl+Shift+Tab | Cmd+Shift+] / Cmd+Shift+[ | Next / previous tab |
| Ctrl+1 to Ctrl+8, Ctrl+9 | Cmd+1 to Cmd+8, Cmd+9 | Select a tab, or the last tab |
| Ctrl+Shift+W | Cmd+W | Close the focused pane, or its tab when it is the only one |
| ×, middle click | ×, middle click | Close a tab with all its panes |
| Right click in a pane | Right click in a pane | Split, zoom, move or close the pane |
| Ctrl+Shift+Z | Cmd+Shift+Enter | Zoom the focused pane, or unzoom |
| Right click a tab | Right click a tab | Rename, ungroup, move or close the tab |
| Ctrl+Shift+U (Windows) | | New tab with the default WSL distribution |
| F11 | Ctrl+Cmd+F | Toggle fullscreen |

Keys bound in `keyboard.bindings` keep their meaning, and the `CreateNewTab`,
`SelectNextTab`, `SelectPreviousTab`, `SelectTab1`-`SelectTab9`,
`SelectLastTab` and `TogglePaneZoom` actions work on every platform. The
window's close button closes all of its tabs.

## Profiles

Until profiles are saved, the menu offers the installed shells: PowerShell,
Windows PowerShell, Command Prompt, Git Bash and each WSL distribution on
Windows, and the shells in `/etc/shells` elsewhere. The default profile starts
the first tab of each window and every new tab.

```toml
default = "Bash"                      # Profile for new windows, + and the new-tab keys.

[[profile]]
name = "Bash"
command = "bash --login"              # Empty uses the shell from alacritty.toml.
directory = "~/projects"              # Empty starts in the home directory.
icon = "~/icons/bash.png"             # An image, a bundled logo such as "bundled:python", or text
                                      # such as "🐧"; empty uses the program's icon.
font = "JetBrains Mono"               # Font family; empty uses the font from alacritty.toml.
env = { EDITOR = "vim" }
options = ["font.size=14"]            # Configuration overrides for this profile's tabs.
hidden = false                        # Leave out of the menu.
```

Icons can be SVG and PNG files everywhere, `.ico`, `.exe` and `.dll` files on
Windows, and `.icns` files or application bundles on macOS. On Linux, logos come
from the profile program's desktop entry or are named after it, in the icon
theme set for KDE or GTK, the themes it inherits, and `hicolor`. The bundled
logos, from devicon, Simple Icons and the projects' own repositories, are listed
with their licences in `alacritty/res/icons/LICENSES.md`.

## Syncing with upstream

The fork's code lives in `alacritty/src/window_context/tabs*`. Upstream files
only gain a few isolated lines, and its dependencies are declared in one block at
the end of `alacritty/Cargo.toml` without adding packages to `Cargo.lock`, so
GitHub's "Sync fork" or merging `upstream/master` should not conflict.

---

<p align="center">
    <img width="200" alt="Alacritty Logo" src="https://raw.githubusercontent.com/alacritty/alacritty/master/extra/logo/compat/alacritty-term%2Bscanlines.png">
</p>

<h1 align="center">Alacritty - A fast, cross-platform, OpenGL terminal emulator</h1>

<p align="center">
  <img alt="Alacritty - A fast, cross-platform, OpenGL terminal emulator"
       src="https://raw.githubusercontent.com/alacritty/alacritty/master/extra/promo/alacritty-readme.png">
</p>

## About

Alacritty is a modern terminal emulator that comes with sensible defaults, but
allows for extensive [configuration](#configuration). By integrating with other
applications, rather than reimplementing their functionality, it manages to
provide a flexible set of [features](./docs/features.md) with high performance.
The supported platforms currently consist of BSD, Linux, macOS and Windows.

The software is considered to be at a **beta** level of readiness; there are
a few missing features and bugs to be fixed, but it is already used by many as
a daily driver.

Precompiled binaries are available from the [GitHub releases page](https://github.com/alacritty/alacritty/releases).

Join [`#alacritty`] on libera.chat if you have questions or looking for a quick help.

[`#alacritty`]: https://web.libera.chat/gamja/?channels=#alacritty

## Features

You can find an overview over the features available in Alacritty [here](./docs/features.md).

## Further information

- [Announcing Alacritty, a GPU-Accelerated Terminal Emulator](https://jwilm.io/blog/announcing-alacritty/) January 6, 2017
- [A talk about Alacritty at the Rust Meetup January 2017](https://www.youtube.com/watch?v=qHOdYO3WUTk) January 19, 2017
- [Alacritty Lands Scrollback, Publishes Benchmarks](https://jwilm.io/blog/alacritty-lands-scrollback/) September 17, 2018

## Installation

Alacritty can be installed by using various package managers on Linux, BSD,
macOS and Windows.

Prebuilt binaries for macOS and Windows can also be downloaded from the
[GitHub releases page](https://github.com/alacritty/alacritty/releases).

For everyone else, the detailed instructions to install Alacritty can be found
[here](INSTALL.md).

### Requirements

- At least OpenGL ES 2.0
- [Windows] ConPTY support (Windows 10 version 1809 or higher)

## Configuration

You can find the documentation for Alacritty's configuration in `man 5
alacritty`, or by looking at [the website] if you do not have the manpages
installed.

[the website]: https://alacritty.org/config-alacritty.html

Alacritty doesn't create the config file for you, but it looks for one in the
following locations:

1. `$XDG_CONFIG_HOME/alacritty/alacritty.toml`
2. `$XDG_CONFIG_HOME/alacritty.toml`
3. `$HOME/.config/alacritty/alacritty.toml`
4. `$HOME/.alacritty.toml`
5. `/etc/alacritty/alacritty.toml`

On Windows, the config file will be looked for in:

* `%APPDATA%\alacritty\alacritty.toml`

## Contributing

A guideline about contributing to Alacritty can be found in the
[`CONTRIBUTING.md`](CONTRIBUTING.md) file.

## FAQ

**_Is it really the fastest terminal emulator?_**

Benchmarking terminal emulators is complicated. Alacritty uses
[vtebench](https://github.com/alacritty/vtebench) to quantify terminal emulator
throughput and manages to consistently score better than the competition using
it. If you have found an example where this is not the case, please report a
bug.

Other aspects like latency or framerate and frame consistency are more difficult
to quantify. Some terminal emulators also intentionally slow down to save
resources, which might be preferred by some users.

If you have doubts about Alacritty's performance or usability, the best way to
quantify terminal emulators is always to test them with **your** specific
usecases.

**_Why isn't feature X implemented?_**

Alacritty has many great features, but not every feature from every other
terminal. This could be for a number of reasons, but sometimes it's just not a
good fit for Alacritty. This means you won't find things like tabs or splits
(which are best left to a window manager or [terminal multiplexer][tmux]) nor
niceties like a GUI config editor.

[tmux]: https://github.com/tmux/tmux

## License

Alacritty is released under the [Apache License, Version 2.0].

[Apache License, Version 2.0]: https://github.com/alacritty/alacritty/blob/master/LICENSE-APACHE
