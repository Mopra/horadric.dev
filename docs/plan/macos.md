# macOS

The Mac's core loop shipped in 0.17.0 (tiles, stage, hosts, resume,
menu bar, install, updater). What is still not built on the Mac, each
its own step:

- **The browser pane**, as a `WKWebView` (WebView2's counterpart).
- **The quest log runner and Warriv.**
- **The files tile and the file viewer.**
- **The usage window and accounts.**
- **Discord.**
- **The Settings window.**

The Mac release notes and the site say these are missing, rather than
showing features that do nothing. Keep saying so until each lands.

**Testing without a Mac on the desk** still holds for all of it. Nothing
builds for the Mac on Windows: the Windows build never compiles
`horadric-ui/src/mac`, so grep it for callers when a shared function
changes, and let ship public's CI find the rest. CI's `macos` job
runs clippy and the tests on a real Mac, and `.github/mac-smoke.sh` is the
on screen check: `HORADRIC_SNAPSHOT` draws each window into a PNG, kept as
a CI artifact. Each new window needs a snapshot in that run.
