# Alacritty Tabbed

A soft fork of Alacritty (`upstream` = alacritty/alacritty, branch `master`).
Fork documentation is in `.github/README.md`; the root `README.md` is upstream's.

## Soft fork rules

Keep the fork's diff against upstream files minimal so upstream merges stay cheap.

- Put fork logic in new files and modules, e.g. `alacritty/src/window_context/tabs/`
  or other `*tabbed*` modules.
- Give upstream files only thin call-in hooks. Mark each hook with a one-line
  comment before it: `// tabbed: <short reason>`.
- Do not reformat, rename, reorder or move code in upstream files.
- Add new dependencies to the fork's deps crate `alacritty_tabbed_deps`, not to
  upstream crates' `Cargo.toml`.
- Write fork docs in `.github/README.md`. Leave the root `README.md`, `INSTALL.md`,
  `CHANGELOG.md` and upstream's workflows (`ci.yml`, `release.yml`) untouched.
- Bring upstream in with `git merge upstream/master`. Never rebase: the history
  is public and GPG-signed.
- Run `scripts/patch-surface.sh` before merging; it lists the upstream files the
  fork changes and the lines changed. CI fails a PR over the budget in
  `.github/workflows/patch-surface.yml`.
- Do not send pull requests upstream.

## Commits

- Commit with the repository-local git identity and signing; do not override them.
- Do not add `Co-Authored-By` or other AI attribution lines.
