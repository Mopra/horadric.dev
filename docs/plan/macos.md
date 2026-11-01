# macOS

The Mac's core loop shipped in 0.17.0 (tiles, stage, hosts, resume,
menu bar, install, updater). What is still not built on the Mac, each
its own step:

- **The rest of the Folio.** New Claude sessions open as a Folio on the
  Mac too, in AppKit text views (lore has how). Still Windows only:
  slash commands, images, `!` commands, review, the sign in card, MCP
  forms, undo, handing over to a terminal, and a smoke step with a fake
  headless claude so CI sees one.
- **The browser pane's rest.** It is on the Mac as a `WKWebView` per
  tab (`mac/web.rs`, lore has how): tabs per project, an address bar,
  and the `browser_*` tools, whose DevTools calls WebKit stands in for
  with script in the page (`webkit_devtools.rs`). Still Windows only:
  pages kept over a restart, docking and popping out, zoom, find,
  favicons, history suggestions, downloads, the keys for tabs, and
  `browser_devtools` beyond the calls the tools make. A page's events
  are its own there, not the user's (`isTrusted` is false).
- **The quest log runner and Warriv.** `runner.rs` and `warriv.rs` sit
  on the Windows `App`; the Mac needs them behind something both apps
  give, not a copy.
- **Switching accounts.** The usage window is on the Mac (limits and
  whose account it is); switching is not. Claude Code keeps a Mac's login
  in the Keychain, not in `.credentials.json`, so the Mac needs its own
  way to read and put it, and to keep the logins aside.
- **The rest of Settings.** The Mac's Settings window has the theme,
  Discord, Open at Login and the session defaults. Not yet: tiles on
  which screen, code font, notifications, sounds, dictation, memory,
  backup, keys, and the per project settings, each as its feature lands
  on the Mac.

The Mac release notes and the site say these are missing, rather than
showing features that do nothing. Keep saying so until each lands.

**Testing without a Mac on the desk** still holds for all of it. Nothing
builds for the Mac on Windows: the Windows build never compiles
`horadric-ui/src/mac`, so grep it for callers when a shared function
changes, and let ship public's CI find the rest. CI's `macos` job
runs clippy and the tests on a real Mac, and `.github/mac-smoke.sh` is the
on screen check: `HORADRIC_SNAPSHOT` draws each window into a PNG, kept as
a CI artifact. Each new window needs a snapshot in that run.
