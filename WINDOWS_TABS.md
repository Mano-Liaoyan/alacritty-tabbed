# Windows tabs fork

This is a local modification of Alacritty v0.17.0, based on upstream commit
94e7c8874e526b1e67b349d9ba30ddf81669119e. It is not an upstream Alacritty release.

The macOS implementation asks AppKit to group native windows. Windows has no
equivalent AppKit API. This fork implements the corresponding grouping inside
Alacritty: one top-level winit window hosts an integrated tab/title header and multiple native child
windows. Each child retains Alacritty's existing WindowContext, OpenGL surface,
terminal state, event identity, and PTY. It is not a launcher for unrelated
top-level Alacritty processes.

The Rex-inspired header replaces the separate native caption in one 48-DIP row:
Alacritty's application icon with the computer name, a rounded track holding the
tabs with a raised active tab showing each profile's icon, a + button with a
profile dropdown right after the last tab, and a settings button beside the
window controls. The terminal sits in a rounded, outlined panel inset from the window
edge, and Windows 11 rounds the window corners. In fullscreen the header stays,
without the label beside the logo, and the terminal fills the rest of the screen
edge to edge, without the panel. Shapes are anti-aliased by a
small CPU rasterizer into a GDI DIB, and text uses GDI ClearType. It uses
Alacritty's existing `winit`, `serde`, `toml`, and `windows-sys` dependencies
(GDI, GDI+, DWM, and common dialogs); no new crate or UI framework is added. Terminal rendering continues to
use Alacritty's OpenGL renderer. Header colors are derived from the configured
primary and normal colors at startup, so light and dark schemes both work.

## Enable

```toml
[window]
tabs = true
startup_mode = "Maximized"
```

`window.tabs` is Windows-only and defaults to false. Restart to change grouping.
The other supported platforms retain their existing window handling.

## Controls

| Control | Action |
| --- | --- |
| Ctrl+Shift+T or + | New tab with the default profile, or the configured shell |
| ⌄ beside + | Choose a profile from the dropdown (arrow keys and Enter also work) |
| ⚙ beside the window controls | Open profile settings |
| Drag a tab sideways | Reorder tabs |
| Ctrl+Shift+U | New tab running the default WSL distribution in its home directory |
| Ctrl+Tab | Next tab |
| Ctrl+Shift+Tab | Previous tab |
| Ctrl+1 through Ctrl+8 | Select a numbered tab |
| Ctrl+9 | Select the last tab |
| Ctrl+Shift+W, tab ×, or middle click | Close the selected/clicked tab |
| F11 or Alt+Enter | Toggle fullscreen; leaving it restores the normal or maximized window |
| Restore button in fullscreen | Leave fullscreen |
| Mouse wheel over the tab strip | Switch tabs |
| ‹ and › buttons (when tabs overflow) | Previous/next tab |
| Drag empty header space or the Alacritty label | Move the window |
| Double-click empty header space | Maximize/restore |
| Right-click empty header space | Windows system menu |
| Drag window edges/corners | Resize |

The default bindings can be overridden in the normal `keyboard.bindings` config.
The existing CreateNewTab/SelectNextTab/SelectPreviousTab/SelectTabN actions now
work on Windows when grouping is enabled. CreateNewWslTab is an added action.
CreateNewWindow and SpawnNewInstance still create a separate top-level window.
All tabs inherit the privilege level of the Alacritty process.

The title bar close button closes every terminal in that window. Closing the
last tab closes its window. Tab labels follow terminal title changes. The strip
scrolls to keep the selected tab visible when tabs overflow.

## Profiles

The first time, the dropdown offers the shells installed on the machine:
PowerShell 7, Windows PowerShell, Command Prompt, Git Bash, and each registered
WSL distribution. The settings panel edits them and adds new ones. For each
profile it sets:

| Setting | Meaning |
| --- | --- |
| Name | Label in the dropdown and settings |
| Icon | Empty uses the program's own icon; or choose an `.ico`, `.png`, `.jpg`, `.bmp`, `.gif`, `.exe`, or `.dll` |
| Command line | Program and arguments; empty uses the shell from `alacritty.toml` |
| Starting directory | Accepts `~` and `%VARIABLES%`; empty starts in the home directory (`%USERPROFILE%`), like Windows Terminal |
| Environment variables | `KEY=value`, one per line, added to the shell's environment |
| Configuration overrides | Alacritty options for this profile's tabs, in `--option` syntax such as `font.size=14` |
| Default profile | Used by +, Ctrl+Shift+T, and the first tab of a new window |
| Show in dropdown | Hide a profile without deleting it |

Automatic icons come from the program itself: the executable's icon,
`git-bash.exe` for Git Bash, and the distribution's own artwork for WSL
(`shortcut.ico` beside the disk image, or the Store package's launcher).
A profile's empty starting directory ignores `general.working_directory` from
`alacritty.toml`; tabs started without any profile, such as `alacritty -e`,
still honor it. WSL profiles open in the distribution's home through `--cd ~`.

Duplicate, beside New profile, copies the selected profile, including its
environment, overrides, icon, and visibility but not the default flag, as
"Name (copy)" right after the original. Drag profiles in the sidebar to
reorder them; the dropdown lists them in that order. Holding a dragged row
beyond the top or bottom of the list scrolls it.

Save writes `%APPDATA%\alacritty\profiles.toml`, which can also be edited by
hand. Closing with unsaved changes asks for a second close to discard them.
Tabs keep the icon they started with; new tabs use saved changes immediately.

## Build

Requires Windows, Rust 1.85 or newer with the x86_64-pc-windows-msvc target,
Visual Studio C++ build tools, and a Windows SDK.

```powershell
cargo build --release --locked --no-default-features -p alacritty
cargo test --release --locked --no-default-features -p alacritty
cargo clippy --release --locked --no-default-features -p alacritty -- -D warnings
```

The executable is `target/release/alacritty.exe`.

## Validation

The build passes 96 application unit tests and Clippy with warnings treated as
errors. Geometry tests cover selection/overflow, caption controls, resize hit
regions, and the fullscreen layout at 100%, 125%, 150%, and 200% scaling. An
explicitly invoked, ignored test renders the production header painter into
dark, light, narrow, mid-drag, and fullscreen bitmaps
(`work/header-preview-*.bmp`), and others render the dropdown with this
machine's profiles (`work/dropdown-preview.bmp`) and the settings panel mid-drag
(`work/settings-preview.bmp`), for visual inspection without a desktop:

```powershell
cargo test --release --locked --no-default-features -p alacritty preview -- --ignored
```

Keyboard/WSL input, real mouse dragging, and double-click behavior need an
interactive desktop and are checked by hand.

## Scope

This implementation provides a tab bar, session switching, and drag-to-reorder
within the visible tabs. It does not implement drag-out detachment or
cross-window tab transfer. The dropdown and settings panel are plain Win32
windows drawn with the header's renderer; settings uses native edit controls
for text entry, IME, and clipboard, and the system file and folder pickers.
Multi-monitor DPI transitions and accessibility/screen-reader integration have
not been validated. The existing font rendering, PTY, shell profiles, and
configuration machinery remain Alacritty's.

The Apache-2.0 license in LICENSE-APACHE applies to this fork. Upstream's
component-specific and MIT license notices are also retained in the source.
