<p align="center">
    <img width="128" alt="Alacritty Tabbed logo" src="../extra/logo/tabbed/alacritty-tabbed.svg">
</p>

<h1 align="center">Alacritty Tabbed</h1>

<p align="center">
    <b>Alacritty, with tabs, split panes, shell profiles and a settings panel.</b>
    <br>
    For Windows, Linux and macOS.
</p>

<p align="center">
    <a href="https://github.com/Mano-Liaoyan/alacritty-tabbed/releases/latest">Download</a>
    ·
    <a href="#features">Features</a>
    ·
    <a href="#keyboard-and-mouse">Shortcuts</a>
    ·
    <a href="#configuration">Configuration</a>
</p>

Alacritty Tabbed is a soft fork of [Alacritty](https://github.com/alacritty/alacritty).
It keeps Alacritty's terminal, renderer and `alacritty.toml`, and adds what goes around
them: a tab bar, split panes, shell profiles and a settings panel. It is not an official
Alacritty release. Upstream's own README is in [README.md](../README.md).

https://github.com/user-attachments/assets/60177fb0-3a55-42bc-ba54-cbfd2758c2e3

https://github.com/user-attachments/assets/04621226-b6f9-4bb0-a56d-64aa4cbf6436

## Compared with Alacritty

|  | Alacritty | Alacritty Tabbed |
| --- | --- | --- |
| **Tabs** | On macOS only, as system window tabs | A tab bar in the title bar, on every system |
| **Split panes** | — | Drag a tab into the terminal to split it; resize, zoom, tear off |
| **Shells** | One, set in `alacritty.toml` | Profiles, each with its logo, one click from the tab bar |
| **Settings** | Edit `alacritty.toml` | A settings panel for every option, writing back to the same file |
| **Text size** | Per window | Per pane |
| **Session** | Starts empty | Can reopen its windows, tabs, splits and their text |

Each tab and pane is a full Alacritty terminal with its own PTY. Everything else, from
escape sequences to key bindings, is upstream's.

## Install

Download the build for your system from the
[latest release](https://github.com/Mano-Liaoyan/alacritty-tabbed/releases/latest):

| System | File |
| --- | --- |
| macOS (Apple Silicon and Intel) | `Alacritty-Tabbed-…-macos-universal.dmg` |
| Windows | `Alacritty-Tabbed-…-windows-x86_64-installer.msi`, or the portable `.exe` |
| Linux | `Alacritty-Tabbed-…-linux-x86_64.tar.gz` |

The builds are not signed by Apple or Microsoft, so macOS and Windows may warn the first
time you open them.

To build it yourself, follow upstream's [INSTALL.md](../INSTALL.md), with two differences:

- **macOS:** `make app` builds `target/release/osx/Alacritty Tabbed.app`. Copy it to
  `/Applications` under that name. It keeps upstream's bundle identifier, `org.alacritty`,
  and so the preferences and permissions already given to Alacritty.
- **Linux:** where INSTALL.md installs `extra/logo/alacritty-term.svg` as the desktop icon,
  install [`extra/logo/tabbed/alacritty-tabbed.svg`](../extra/logo/tabbed/alacritty-tabbed.svg)
  instead. The Windows and macOS builds already carry it.

## Features

### The tab bar

The tab bar takes the place of the window's title bar. One row holds the logo and names,
the tabs, the **+** button with its **⌄** profile menu, the pin, settings, and the window
controls; the terminal sits in a rounded panel below it. The window controls take each
system's look: flat buttons on Windows, round ones on Linux, and the red, yellow and green
buttons on macOS.

- Drag the empty bar to move the window, double-click it to maximize, and right-click it
  for the system menu (Windows and Wayland). Drag the window's edges to resize it.
- Fullscreen keeps the bar, and lays out the panes as a maximized window does.
- **Pin** the bar with the pin left of the settings gear. Unpinned, the panes take the
  whole window and the bar slides in over them when the pointer rests at the top edge,
  during a drag, or on a tab key; the terminals are not resized as it comes and goes. On
  macOS the window buttons slide with it.
- On macOS, right-click the Dock icon for a new window.

### Tabs

- **Open** a tab with **+** (the default profile) or pick a profile from **⌄**.
- **Reorder** tabs by dragging them, and **switch** by clicking or scrolling over the bar.
- **Close** a tab with its **×** or a middle click.
- **Rename** a tab by double-clicking its title, or from its right-click menu, which also
  has Reset Name, Ungroup, Move Tab to New Window and Close Tab.

### Split panes

A tab can hold several terminals, its *panes*, side by side or stacked, like tmux's
windows. Each pane has its own profile, and panes in the background keep running. The
focused pane has a lit border.

| To | Do this |
| --- | --- |
| **Split** | Drag a tab into the terminal. It docks beside the pane under the pointer, on the nearest side; a preview shows where. Or right-click a pane: Split Vertically, Split Horizontally. |
| **Group** | Drag a tab onto the middle of another tab. |
| **Rearrange** | Drag a pane's logo from its tab, the ••• grip at the top of the pane, or anywhere on it with Ctrl+Alt (⌘⌥ on macOS) held. Drop it beside another pane, on another tab, or between tabs to make it a tab of its own. |
| **Tear off** | Drop a tab or pane outside the window: it opens in a new window and keeps running. Or choose Move … to New Window. |
| **Resize** | Drag the divider between two panes. Double-click it to go back to half and half. |
| **Zoom** | Ctrl+Shift+Z (⌘⇧Return on macOS), or the zoom button at the top of the pane, fills the tab with one pane, and back. |
| **Close** | The pane's **×**. While a program other than the shell runs in it, **×** asks first. |

More about panes:

- **Pane controls** — the ••• grip, zoom and **×** — appear at the top of a pane as the
  pointer comes near, and on every pane while Ctrl+Alt is held.
- **A tab of several panes** shows its panes' logos stacked like cards, the focused one in
  front; past five, a "+N" count. Hover the stack to fan the logos out, rest on a logo to
  see its pane's title and a live preview, and click one to focus its pane.
- **Text size per pane:** Ctrl+mouse wheel over a pane, or Ctrl+= / Ctrl+− / Ctrl+0
  (⌘ on macOS), sizes only that pane's text; the wheel goes from half to four times its
  size. The size stays with the pane, even torn off, but is never saved to the
  configuration.
- **Right-click a pane** for Split Vertically, Split Horizontally, Zoom Pane, Reset Text
  Size, Move Pane to New Tab, Move Pane to New Window and Close Pane. When the program in
  the pane takes the mouse, hold Shift.
- **A split starts where you are:** it runs the pane's profile in the directory the pane's
  shell is in. See [Splits start in the pane's directory](#splits-start-in-the-panes-directory).

### Shell profiles

The **⌄** menu beside **+**, the tabs and the settings show each profile with its logo:
the program's own icon (its executable on Windows, the icon theme on Linux, the
application on macOS), or one of about 40 bundled logos for common shells, tools,
languages and WSL distributions.

Until you save profiles, the menu offers the installed shells: PowerShell, Windows
PowerShell, Command Prompt, Git Bash and each WSL distribution on Windows, and the shells
in `/etc/shells` elsewhere. **Edit profiles…** in the menu sets each profile's name, logo,
command line, starting directory, font, environment, configuration overrides, and whether
it shows in the menu. Profiles can be added, duplicated, reordered and deleted, and saved
changes apply to the tabs already open. See [Profiles](#profiles) for the file.

### Settings

The gear opens a settings panel, like Windows Terminal's: the application's pages, then
the profiles. Together the pages cover every option of `alacritty(5)` that holds a value:

| Page | Covers |
| --- | --- |
| General | Startup (session restore, startup mode, window size and position, working directory), behaviour, live reload, IPC socket, imports, environment |
| Window | Title, class, level, decorations, opacity, blur, padding |
| Appearance | The fork's look: corners, terminal panel, pane gap and highlight, interface font, tab track, icon tiles, logo |
| Font | Family, styles, size, spacing, rendering |
| Colors | Primary, normal, bright and dim colors, cursor, selection, search, hints |
| Cursor | Shape, blinking, thickness, vi mode's cursor |
| Terminal | Shell, scrolling, selection, mouse and keyboard, bell |
| Advanced | Renderer and logging |

`alacritty.toml` stays the source of truth. Saving writes only the keys that changed,
under Alacritty's own names, keeps the file's comments, and applies them to every window.
Key and mouse bindings, hints and indexed colors are still edited in the file.

### Session restore

Under **General → Startup**, choose what Alacritty reopens when it starts:

- **Off** (the default).
- **Layout:** the windows with their size and place, tabs, names, splits, zoom, and each
  pane's profile, directory and text size.
- **Layout and text:** also the last 2,000 lines of each pane.

Programs are not resumed: each pane starts a new shell in its directory, below its
restored text. The session is saved when you close the last window; closing the last
tab instead ends it without saving. Only a plain `alacritty` launch restores (not one
with `-e`, `--working-directory`, `--hold` or daemon mode).

> [!NOTE]
> The saved text can hold secrets you typed. It stays on this computer, readable only
> by you on Linux and macOS, in `$XDG_STATE_HOME/alacritty/session.json`
> (`~/.local/state/alacritty/` by default; `%LOCALAPPDATA%\alacritty\session.json` on
> Windows), and is deleted once restored.

### Look and feel

- **Rounded corners that nest:** the window, panel, tab track, tabs and buttons are
  rounded so their curves run parallel. Or make every corner square. On Linux, round
  window corners need a compositor.
- **Interface font:** the bar, menus and settings use the system's interface font (San
  Francisco, Segoe UI, or on Linux the first of Inter, Adwaita Sans, Noto Sans and
  others), apart from the terminal's font.
- **Logo:** show Alacritty Tabbed's logo, upstream's, a bundled one, your own SVG or PNG,
  or none, and change the two names beside it. `{host}` stands for the computer's name.
- **Motion:** panes slide and pop into place. Set `ALACRITTY_ANIMATION_SCALE` to slow them
  down (`4` is four times slower) or to `0` to turn them off.

All of these are under **Appearance** in the settings, and apply live.

## Keyboard and mouse

| Windows, Linux | macOS | Action |
| --- | --- | --- |
| Ctrl+Shift+T, **+** | ⌘T, **+** | New tab with the default profile |
| **⌄** beside **+** | **⌄** beside **+** | Choose a profile (arrow keys and Enter work) |
| Ctrl+Tab / Ctrl+Shift+Tab | ⌘⇧] / ⌘⇧[ | Next / previous tab |
| Ctrl+1 … Ctrl+8, Ctrl+9 | ⌘1 … ⌘8, ⌘9 | Go to a tab, or the last tab |
| Ctrl+Shift+W | ⌘W | Close the focused pane, or its tab when it is the only one |
| **×**, middle click | **×**, middle click | Close a tab with all its panes |
| Ctrl+Shift+Z | ⌘⇧Return | Zoom the focused pane, or unzoom |
| Ctrl+wheel, Ctrl+= / Ctrl+− / Ctrl+0 | ⌘wheel, ⌘= / ⌘− / ⌘0 | Text size of the focused pane |
| Right-click a pane or a tab | Right-click a pane or a tab | Split, zoom, rename, move or close |
| Ctrl+Shift+U (Windows) | | New tab with the default WSL distribution |
| F11 (also Alt+Enter on Windows) | ⌃⌘F | Fullscreen |

On Windows and Linux, the fork's keys apply only where `keyboard.bindings` leaves the key
unbound, so your own bindings keep their meaning. The `CreateNewTab`,
`SelectNextTab`, `SelectPreviousTab`, `SelectTab1` to `SelectTab9`, `SelectLastTab` and
`TogglePaneZoom` actions work on every system. The window's close button closes all its
tabs.

## Configuration

The settings panel edits everything below; you can also edit the files by hand.

### `[tabbed]` in `alacritty.toml`

The fork's keys live in a `[tabbed]` table of their own, apart from upstream's tables:

```toml
[tabbed]
tabs = true                # false: no tab bar while a window has one tab. Restart after changing it.
corners = "round"          # "round" or "square".
panel = true               # false: the panes fill the window, without a panel or gaps.
tab_track = true           # false: the tabs sit on the bar itself, without a track.
icon_tiles = true          # false: a tab's pane logos are drawn bare, without tiles.
pane_gap = 16              # Logical pixels between panes, 0 to 48; needs panel = true.
pane_highlight = "#ff9500" # The focused pane's border; leave out for the terminal's blue.
pin_tab_bar = true         # false: the tab bar slides in at the top edge.
wheel_zoom = true          # Ctrl+wheel (⌘+wheel on macOS) sizes the focused pane's text.
restore_session = "off"    # "off", "layout", or "text" for layout and text.

[tabbed.ui_font]
family = ""                # Empty picks the system's interface font.
size = 10                  # Points; 10 by default, 13 on macOS.

[tabbed.brand]
show = true                # The logo and names at the left of the tab bar.
icon = "tabbed"            # Or "alacritty" (upstream's), "bundled:<name>",
                           # an .svg or .png path, or "" for none.
title = "Alacritty"        # "" hides the line.
subtitle = "{host}"        # {host} is the computer's name, in capitals; "" hides the line.
```

<details>
<summary>Moving from an older version</summary>

Earlier versions read these keys from `[window]`, `[window.ui_font]` and
`[window.brand]`. There they are now ignored, with an "Unused config key" warning.
Saving the settings moves them to `[tabbed]`, keeping their comments, or rename the
tables by hand:

```toml
[window]
startup_mode = "Maximized"  # Upstream's keys stay in [window].

[tabbed]                    # Was [window].
pane_gap = 6

[tabbed.brand]              # Was [window.brand].
show = false
```

</details>

### Profiles

Profiles are saved to `profiles.toml`, beside `alacritty.toml`:

```toml
default = "Bash"                # The profile for new windows, + and the new-tab keys.

[[profile]]
name = "Bash"
command = "bash --login"        # Empty uses the shell from alacritty.toml.
directory = "~/projects"        # Empty starts in the home directory.
icon = "bundled:bash"           # An image, a bundled logo, or text such as "🐧";
                                # empty uses the program's icon.
font = "JetBrains Mono"         # Empty uses the font from alacritty.toml.
env = { EDITOR = "vim" }
options = ["font.size=14"]      # Configuration overrides for this profile's panes.
hidden = false                  # true leaves it out of the menu.
```

Icons can be SVG and PNG files everywhere, `.ico`, `.exe` and `.dll` files on Windows,
and `.icns` files or application bundles on macOS. On Linux, logos come from the
program's desktop entry or icon theme. The bundled logos, from devicon, Simple Icons and
the projects themselves, are listed with their licences in
[`alacritty/res/icons/LICENSES.md`](../alacritty/res/icons/LICENSES.md).

### Splits start in the pane's directory

A split runs the pane's profile in the directory its shell is in. Most shells need no
setup:

| Shell | How the directory is found |
| --- | --- |
| Linux, macOS, BSD | The directory of the program in the foreground |
| Command Prompt, Git Bash, other MSYS and Cygwin shells | Read from the shell's process |
| WSL | Read inside the distribution, through the pane's `ALACRITTY_PANE_ID` |
| PowerShell, Windows PowerShell | **Needs setup:** the prompt has to report it |

A shell can report its directory with `OSC 7` (`ESC ] 7 ; file://host/path ST`) or
Windows Terminal's `OSC 9 ; 9` (`ESC ] 9 ; 9 ; "C:\path" ST`); a report wins over what is
read from processes on Windows. Reports from another host, such as over SSH, are ignored.
One line in the shell's startup file sends it at each prompt:

```powershell
# PowerShell and Windows PowerShell, in $PROFILE
$function:_prompt = $function:prompt; function prompt { $l = $executionContext.SessionState.Path.CurrentLocation; $(if ($l.Provider.Name -eq 'FileSystem') { "$([char]27)]9;9;`"$($l.ProviderPath)`"$([char]27)\" }) + (_prompt) }
```

<details>
<summary>bash, zsh and fish</summary>

These need no setup, but can report too:

```sh
# bash, ~/.bashrc
PROMPT_COMMAND='printf "\e]7;file://%s%s\a" "$HOSTNAME" "$PWD"'${PROMPT_COMMAND:+";$PROMPT_COMMAND"}
# zsh, ~/.zshrc
autoload -Uz add-zsh-hook; _osc7() { printf '\e]7;file://%s%s\a' "$HOST" "$PWD" }; add-zsh-hook precmd _osc7
# fish, ~/.config/fish/config.fish (recent versions already report it)
function _osc7 --on-variable PWD; printf '\e]7;file://%s%s\a' $hostname $PWD; end
```

With [Oh My Posh](https://ohmyposh.dev), set `pwd = "osc7"` in its configuration.

</details>

<details>
<summary>How WSL splits work</summary>

`WSLENV` passes each pane's `ALACRITTY_PANE_ID` into Linux. A split runs a short
`wsl.exe -e sh` that finds the pane's processes in `/proc` and reads the directory of
the one in the foreground, or else of the shell, giving up after 1.5 seconds. The split
then starts `wsl.exe` with `--cd <path>`, or, for paths with spaces, in the path's
`\\wsl.localhost\<distribution>\…` folder; without `-d <distribution>` in the profile,
those start at home. Windows paths such as `/mnt/c/…`, `/c/…` or `C:\…` in a report work
for every profile.

</details>

## Staying close to upstream

The fork keeps its changes to upstream's files small, so that merging upstream stays
cheap; the rules are in [CLAUDE.md](../CLAUDE.md). The
[Upstream sync](workflows/upstream-sync.yml) workflow tries the merge every week and
opens an issue when it conflicts.

For everything not covered here, see upstream's [README.md](../README.md),
[INSTALL.md](../INSTALL.md) and [CONTRIBUTING.md](../CONTRIBUTING.md). Like Alacritty,
Alacritty Tabbed is released under the [Apache License 2.0](../LICENSE-APACHE).
