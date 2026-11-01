# macOS

The Mac's core loop shipped in 0.17.0 (tiles, stage, hosts, resume,
menu bar, install, updater). What is still not built on the Mac, each
its own step:

- **The rest of the Folio.** New Claude sessions open as a Folio on the
  Mac too, in AppKit text views (lore has how). Still Windows only:
  slash commands, images, `!` commands, review, the sign in card, MCP
  forms, undo, handing over to a terminal, and a smoke step with a fake
  headless claude so CI sees one.
- **The browser pane**, as a `WKWebView` (WebView2's counterpart).
- **The quest log runner and Warriv.** `runner.rs` and `warriv.rs` sit
  on the Windows `App`; the Mac needs them behind something both apps
  give, not a copy.
- **The files tile.** The viewer is on the Mac (a project's menu, Open
  file); the tile that lists a project's changed files is not.
- **The usage window and accounts.**
- **The Settings window.** The Mac's few settings are in its menu bar
  menu meanwhile.

The Mac release notes and the site say these are missing, rather than
showing features that do nothing. Keep saying so until each lands.

**Testing without a Mac on the desk** still holds for all of it. Nothing
builds for the Mac on Windows: the Windows build never compiles
`horadric-ui/src/mac`, so grep it for callers when a shared function
changes, and let ship public's CI find the rest. CI's `macos` job
runs clippy and the tests on a real Mac, and `.github/mac-smoke.sh` is the
on screen check: `HORADRIC_SNAPSHOT` draws each window into a PNG, kept as
a CI artifact. Each new window needs a snapshot in that run.
