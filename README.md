# Alacritty Tabbed

Alacritty Tabbed ([`alacritty-tabbed`](https://github.com/Mano-Liaoyan/alacritty-tabbed))
is a soft fork of [Alacritty](https://github.com/alacritty/alacritty) that adds
tabs and shell profiles on Windows, Linux and macOS. It is not an official
Alacritty release. Upstream's README follows the fork notes below.

Each tab is a full Alacritty terminal with its own PTY and terminal state; the
tabs of a window share its renderer. The tab bar is drawn by Alacritty's own
OpenGL renderer under the system's title bar, so it looks the same everywhere
while the window frame stays native. No crates are added.

![Tab bar with the new-tab menu open](docs/tabs/tabs.png)

## Enable

```toml
[window]
tabs = true
```

| Keys (Windows, Linux) | macOS | Action |
| --- | --- | --- |
| Ctrl+Shift+T, + | Cmd+T, + | New tab with the default profile |
| ⌄ beside + | ⌄ beside + | Choose a profile (arrow keys and Enter work) |
| Ctrl+Tab / Ctrl+Shift+Tab | Cmd+Shift+] / Cmd+Shift+[ | Next / previous tab |
| Ctrl+1 to Ctrl+8, Ctrl+9 | Cmd+1 to Cmd+8, Cmd+9 | Select a tab, or the last tab |
| Ctrl+Shift+W, ×, middle click | Cmd+W, ×, middle click | Close a tab |
| Ctrl+Shift+U (Windows) | | New tab with the default WSL distribution |
| F11 | Ctrl+Cmd+F | Toggle fullscreen |
| Mouse wheel over the bar | Mouse wheel over the bar | Switch tabs |
| Drag a tab sideways | Drag a tab sideways | Reorder tabs |

Keys bound in `keyboard.bindings` keep their meaning, and the `CreateNewTab`,
`SelectNextTab`, `SelectPreviousTab`, `SelectTab1`-`SelectTab9` and
`SelectLastTab` actions work on every platform. Closing the window closes all of
its tabs.

## Profiles

The ⌄ menu lists profiles from `profiles.toml` beside `alacritty.toml`. Until
that file exists it offers the installed shells: PowerShell, Windows
PowerShell, Command Prompt, Git Bash and each WSL distribution on Windows, and
the shells in `/etc/shells` elsewhere. The gear button, or "Edit profiles…" in
the menu, creates the file and opens it in `$VISUAL` or `$EDITOR` in a new tab,
or else in the system's text editor. Changes apply to the next new tab.

```toml
default = "Bash"                      # Profile for + and the new-tab keys.

[[profile]]
name = "Bash"
command = "bash --login"              # Empty uses the shell from alacritty.toml.
directory = "~/projects"              # Empty starts in the home directory.
icon = "🐧"                           # Text for the tab's icon; default is the initial.
env = { EDITOR = "vim" }
options = ["font.size=14"]            # Configuration overrides for this tab.
hidden = false                        # Leave out of the menu.
```

## Syncing with upstream

The fork's code lives in `alacritty/src/window_context/tabs*`; upstream files
only gain a few isolated lines, so GitHub's "Sync fork" or merging
`upstream/master` should not conflict.

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
