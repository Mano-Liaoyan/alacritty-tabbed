# OpenScreen demo plan (hand-over)

Do not commit this file, the video, or anything under `target/demo/`.

## The request

> Install the OpenScreen demo skill, then follow it to make a demo video of this
> application compared to upstream. Also update .github/README.md to remove the old
> screenshots and features, organized well and nice to read.

Skill: `.agents/skills/openscreen-demo/SKILL.md` (installed; symlinked into `.claude/skills/`).
Kind of video: a **launch demo** (skill step 9's format: 20 to 60 s, 1080p, 16:9),
also shown at the top of `.github/README.md`. **Nothing is posted on X**; the user did
not ask for a post.

## Done in the first session

- OpenScreen 2.1.2 installed at `/Applications/Openscreen.app` (notarized, Developer ID
  Etienne Lescot). MCP server `openscreen` registered in this project's local Claude Code
  config (`Openscreen mcp`, stdio). Its tools load in the next session.
- Release builds, both 0.18.0-dev:
  - upstream Alacritty (`upstream/master` 54214706, already merged into `main`):
    `target/upstream-build/release/alacritty`, source in `target/upstream-src/`. Its
    `--version` says 2f258607 only because that source sits inside this repository.
  - Alacritty Tabbed (`main` 2f258607): `target/release/alacritty`
- `.github/README.md` rewritten: old screenshots removed, sections reorganized. It has
  **no video yet**: step 6 below adds it.

## Stage (`target/demo/`)

- `./launch.sh upstream` and `./launch.sh tabbed` start each build with its own config
  folder (`upstream/`, `tabbed/`) and state folder, so the user's own Alacritty config,
  `profiles.toml` and saved session are never touched. Both: 150×42 cells, Menlo 15,
  default colors, `zsh -f` with the prompt `alacritty-tabbed ❯` in this repo.
- Window titles: upstream's stays `Upstream Alacritty demo` (`dynamic_title = false`).
  The fork's starts as `Alacritty Tabbed demo` but follows the focused pane's title (the
  profile's name, such as `zsh`) once tabs show, so read it from `listSources` right
  before `startRecording`, with other Alacritty windows closed.
- The fork's brand shows "Alacritty / Tabbed", not the computer's name.
- Profiles in the ⌄ menu, each with its logo: zsh, Bash, Python, Node.js, Ruby, SQLite.
- `./drive` (source `drive.swift`) plays a beat with the real pointer and keyboard:
  `./drive windows` lists windows with pid and bounds; `./drive play beat.txt --pid <pid>
  [--log pointer.json] [--dry]`. Steps are documented at the top of `drive.swift`; points
  are relative to the window's top-left, in points. Real pointer, so OpenScreen draws the
  cursor and zooms on clicks; `--log` writes a `setPointerTrack` fallback. It posts
  events, so Claude needs the Accessibility permission. Never tested live yet: try
  `--dry` first, then a short beat.
- Display: 1920×1080 points (4K at 2×).

## Storyboard (shown to the user; carry on unless they object)

**Message:** Alacritty, with tabs, split panes, profiles and a settings panel.

The comparison needs both apps in one take, so record **the display** with the two
windows side by side (upstream on the left, about a third of the width; Alacritty Tabbed
on the right), then crop to the windows with `setClipRange` `cropRegion` so the menu
bar, Dock and other apps stay out. If `startRecording` takes only a window, record two
takes and lead with the fork's, with upstream's as a short second project.

Before recording, seed the Tabbed window so the first frame shows the result: three
tabs, the first a group of two panes (`git log --oneline --graph` beside `top -o cpu`).

1. **Result first (0–4 s).** Both windows. Title callout, typewriter: "Alacritty, now
   with tabs". Small labels over each window: "Upstream" / "Tabbed".
2. **Upstream (4–9 s).** Cmd+T in upstream: a macOS window tab, one terminal each, no
   splits. Callout: "Upstream: one terminal per tab".
3. **Profiles (9–17 s).** In Tabbed, click ⌄ beside +: the menu with each shell's logo;
   pick Python. Callout: "Tabs for every shell".
4. **Split by dragging (17–27 s).** Drag the Python tab into the right half of the
   terminal: the preview, then the split. Drag another into the bottom half to stack.
   Callout: "Drag a tab in to split".
5. **Settings (27–36 s).** Click the gear: General, then Appearance; flip a switch.
   Callout: "Every option, in settings".
6. **End (36–40 s).** Wide on both. Callout: "github.com/Mano-Liaoyan/alacritty-tabbed".

Keep every callout to 6 words or fewer. No sound.

## Next steps (in the new session)

1. Check the `openscreen` tools are loaded (`startRecording`, `stopRecording`,
   `exportVideo`, `getFrames`). If recording is refused, the user must grant Claude
   Screen Recording (and Accessibility, for clicks and for `./drive`) in System Settings
   → Privacy & Security; ask them, do not change it.
2. Close any other Alacritty windows from these builds. `./launch.sh upstream &` and
   `./launch.sh tabbed &`, place them side by side (AppleScript `set bounds`, or drag
   once by hand), seed the Tabbed window, `listSources`.
3. Write one beat file per storyboard beat (or one for the whole take), dry-run each,
   then record (skill steps 4 and 5) and check the take with `getFrames`.
4. Edit (skill step 6): 16:9, trim the setup and end, check zooms land on the fork's
   action, callouts per beat, blur anything private (paths with the user name, the
   computer's name).
5. Export to an absolute path outside the repo, such as `target/demo/demo.mp4`, at
   1080p, and review the frames (skill step 7).
6. Put the video in `.github/README.md` under the intro: upload it with `gh` (a comment
   or the README pull request, `--attach`) to get a `github.com/user-attachments/...`
   URL, and reference that URL, never a file in the repository. Ask before posting to
   GitHub.

## Done in the second session

- Recorded through the OpenScreen app (`/Applications/Openscreen.app`, which has Screen
  Recording and Accessibility); "Claude Code" itself has no Screen Recording.
- OpenScreen project `Alacritty Tabbed vs upstream` (`proj_c497c3f7-54ba-4792-88f2-529a4ad12faa`):
  cropped to the stage, 16:9, four cuts, three zooms, seven callouts. It is the only
  OpenScreen project left: the 16 `Stage check (scratch)` test takes went to the Trash.
- `target/demo/demo.mp4`: 35.4 s, 1920×1080, 30 fps, no sound, 6.9 MB. OpenScreen's export
  is 19.7 MB; `./transcode <export> demo.mp4 1900000` brings it under GitHub's 10 MB limit.
- Both videos are hosted on PR #53 (`readme-rewrite`, the README rewrite and screenshot
  deletions, from main in the worktree `target/worktrees/readme-rewrite`), and embedded in
  `.github/README.md` under the intro: demo.mp4 as
  https://github.com/user-attachments/assets/24b23c01-c1cd-4202-bbf7-5b289cd60036
  (the comments that first hosted them on PR #52 are deleted).
- `gh` in this checkout now defaults to `Mano-Liaoyan/alacritty-tabbed`
  (`gh repo set-default`, stored as `remote.origin.gh-resolved` in `.git/config`).
- To retake: `./backdrop &`, `./launch.sh upstream|tabbed`, place the windows at
  (160,53 520×882) and (700,53 1060×882), play `beats/seed-upstream.txt` and
  `beats/seed-tabbed.txt` with `./drive play … --pid <pid>`, then `./stage.sh` and
  `./take.sh` while recording display 0. Nothing is committed.

## Second video: "Alacritty Tabbed, a closer look"

- `target/demo/closer-look.mp4`: 53.7 s, 1920×1080, 30 fps, no sound, 8.6 MB. OpenScreen
  project `proj_34c5f5ad-fffc-44d5-a85c-2c2b431df98c`. On PR #53 and in `.github/README.md`
  under the first video as
  https://github.com/user-attachments/assets/e14ff37e-833b-4914-b4b2-3c39b6dedb09
- Beats: drag panes by the grip and with ⌘⌥; resize and reset the divider; zoom a pane and
  size its text; settings (restore "Layout and text", Square corners, 32 px pane gap, tab
  track off, one Save); close and reopen with the layout and text restored; fullscreen.
- To retake: `./backdrop &`, `./launch.sh tabbed`, window at (160,84 1600×882),
  `./drive play beats/seed-wide.txt --pid <pid>`, `./stage.sh`, then `./take2.sh` while
  recording display 0. Afterwards restore `tabbed/alacritty.toml` (the take saves settings
  into it) and delete `state/alacritty/session.json`.
