# Releasing Horadric

Two signatures could matter, and only one is set up:

1. **The updater signature** (ECDSA P-256 through Windows CNG). Set up.
   Without the matching private key you cannot ship an update that existing
   installs will accept.
2. **Authenticode code signing** (a real certificate). **Not set up**, and
   not needed yet. The updater downloads its binaries itself, so they never
   meet SmartScreen. It becomes needed the day people download Horadric by
   hand, together with an installer.

## 1. The updater key: set up, back it up

`horadric release keygen` made the key pair once:

```
%USERPROFILE%\.horadric\updater.key    <- private. back this up.
```

The public half is built into the source as `PUBLIC_KEY` in
`crates/horadric-core/src/release.rs`. It has no password. `keygen` refuses
to overwrite an existing key, on purpose.

**Back the private key up somewhere you will still have in two years.** If
it is lost, every installed Horadric carrying that public key can never be
updated again. It refuses anything signed by another key, and the only way
left is installing a new build by hand on every machine. The public key is
already in shipped binaries, so it cannot be changed later.

The key never goes into the repository, a CI secret or a release. Signing
happens on this machine.

## 2. Cutting a release

This is what "ship public" means (see CLAUDE.md). Plain "ship" skips it
and only reloads this machine.

1. Bump `version` under `[workspace.package]` in `Cargo.toml`. That is the
   release's version, and every install compares its own against it, so
   it must go up each time or no one is offered the release.
2. Run `cargo fmt --all` (seconds, no build) and commit what it changed
   with the version bump. No clippy or tests here: CI runs them.
3. Tag and push, before building, so CI and the Mac and Linux release
   builds run while this machine builds:

   ```sh
   git tag v0.2.0
   git push origin main v0.2.0
   ```

4. Build and sign:

   ```sh
   cargo build --release
   target/release/horadric.exe release sign target/release --notes "What changed"
   ```

   This hashes `horadric.exe` and `horadricw.exe` and writes
   `target/release/latest.json`: the version, the notes, a SHA-256 per
   binary and one signature over all of it. The notes are what a user
   reads when deciding whether to update, so write them for that user,
   not for a developer. The first paragraph goes in the notification,
   cut to about 120 characters, so make it the one line that matters.
   All of it shows in the dialog before the update. A blank line
   separates paragraphs; `--notes` takes one argument, so quote it.
5. Zip the two binaries for winget. A winget installer entry is one URL,
   and `horadric install` needs `horadricw.exe` next to `horadric.exe`, so
   winget installs both from one archive. The updater ignores the zip.

   ```powershell
   Compress-Archive -Force -DestinationPath target/release/horadric-x64.zip `
     -Path target/release/horadric.exe, target/release/horadricw.exe
   ```

6. Create a **draft** release with the four files, and the same notes
   as the manifest, so the release page and the app say the same thing:

   ```sh
   gh release create v0.2.0 --draft --title "Horadric 0.2.0" --notes "What changed" \
     target/release/horadric.exe target/release/horadricw.exe target/release/latest.json \
     target/release/horadric-x64.zip
   ```

   Once it is published, the winget manifest (`Mopra.Horadric` in
   microsoft/winget-pkgs) needs the new version, URL and the zip's
   SHA-256: `(Get-FileHash target/release/horadric-x64.zip).Hash`.

7. Sign the Mac build. CI builds `Horadric-macos.tar.gz` (the universal
   `Horadric.app`, packed with tar) and attaches it to the draft. The
   key stays here, so the Mac manifest is signed here too: download the
   archive from the draft, sign it with the same build as above, and
   upload the manifest to the draft.

   ```sh
   gh release download v0.2.0 --pattern Horadric-macos.tar.gz --dir target/mac
   target/release/horadric.exe release sign-mac target/mac/Horadric-macos.tar.gz --notes "What changed"
   gh release upload v0.2.0 target/mac/latest-macos.json
   ```

   This writes `latest-macos.json` beside the archive: the same format,
   version and key as `latest.json`, with one file, the archive and its
   SHA-256. A Mac checks
   `https://github.com/Mopra/horadric.dev/releases/latest/download/latest-macos.json`,
   downloads the archive into
   `~/Library/Caches/Horadric/updates/<version>/`, checks its hash
   against the signed manifest before unpacking it, and hands over to
   the new `Horadric.app` the way `reload` does. A release published
   without `latest-macos.json` offers Macs nothing; one with a manifest
   but no archive offers an update that fails to download, so upload
   both before publishing.
8. Check the Linux node. The tag also runs `release-linux.yml`, which
   builds `horadric node` as one static musl binary, checks that it
   links nothing shared and answers `--help`, packs it as
   `Horadric-linux-x86_64.tar.gz` and uploads that to the draft (it
   waits up to 30 minutes for the draft to exist, and otherwise leaves
   the archive in the run's artifacts, to upload by hand with `gh
   release upload v0.2.0 Horadric-linux-x86_64.tar.gz`). `install.sh`
   downloads exactly that name from the latest release, so a release
   published without it leaves new Linux installs on the old node. A
   node has no updater and nothing is signed. See that it is on the
   draft (`gh release view v0.2.0 --json assets --jq '.assets[].name'`).
   No ssh trial: CI's smoke run already starts the node.

   Never build the node, or anything for Mac or Linux, on this machine.
   CI does it on the tag.
9. No dev instance trial: it puts windows over the human's work and
   takes minutes. The guard is the rollback instead. This machine
   reloads the same build when the ship ends, and every install that
   updates goes back to its old build by itself if the new one does not
   come up in 20 seconds. Try a dev instance only when the release
   changes reloading or updating itself.
10. Look at CI for the pushed commit (`gh run list --commit <sha>`). A red
    test in the Windows `test` job holds the publish: fix it, bump the
    patch and ship again. Red fmt or clippy, or a red Mac or Linux job,
    does not hold it; fix those in the next commit. Do not wait for the
    Mac and Linux CI jobs to finish, only for the Mac archive.
11. Publish the draft (below).
12. Bump the Chocolatey package (below).
13. Deploy the Town Portal page (below).

## 3. Publishing is shipping

Every install checks
`https://github.com/Mopra/horadric.dev/releases/latest/download/latest.json`
on start, once a day, and from "Check for updates" in the tray. A draft is
not `latest`, so it is invisible to them. That is the point of the draft.

**Publishing the draft is the moment every install is offered the
release.** Each one shows a notification, "Horadric 0.2.0 is out", with
the first paragraph of the notes, once per release (the state remembers
it was told). A click on it, or on "Update to 0.2.0" in the tray, shows
all the notes with "Update now" and "Not now". Nothing installs on its
own: "Update now" downloads both binaries into
`%LOCALAPPDATA%\Horadric\updates\<version>\`, checks each hash against the
signed manifest, and only then hands over through `reload`, the same way
`horadric reload` does from a checkout. Sessions keep running in their
hosts, and a build that does not come up in 20 seconds is rolled back by
itself. What happened is in `%APPDATA%\Horadric\reload.log`.

A bad release cannot be taken back from installs that already took it.
The fix is a new release with a higher version. Deleting or turning a
published release back into a draft only stops installs that have not
updated yet.

## 4. Chocolatey

Horadric is on Chocolatey as
[`horadric`](https://community.chocolatey.org/packages/horadric). The
package lives in the socialsandbox repo, in
`products/horadric/chocolatey/horadric`, and downloads
`horadric-x64.zip` from the release. After each published release:

1. In `horadric.nuspec`, set `version` and the tag in `releaseNotes`.
2. In `tools/chocolateyInstall.ps1`, set `url64bit` to the new tag and
   `checksum64` to the zip's SHA-256:

   ```powershell
   gh release download v0.2.0 --repo Mopra/horadric.dev --pattern horadric-x64.zip --dir $env:TEMP\choco
   (Get-FileHash $env:TEMP\choco\horadric-x64.zip -Algorithm SHA256).Hash
   ```

3. From the package folder, pack and push:

   ```powershell
   choco pack
   choco push horadric.0.2.0.nupkg --source https://push.chocolatey.org/
   ```

Every version is moderated, so it shows on Chocolatey hours or days
after the release. Installs from Chocolatey update themselves like any
other, which is why the README says the app can run ahead of the
Chocolatey version. A skipped bump costs nothing but a staler first
install.

## 5. This machine

The Horadric on this machine ships from the checkout with `horadric
reload`, as section 7 says, and does not need the release. After a
release it is at the same version, so its tray offers nothing. If it is
behind, its tray offers the published release like any other install.

## 6. The Town Portal page

The phone page at `portal.horadric.dev` lives in `../horadric.portal` and
ships on its own, so a PC feature for phones (colours, the inbox, push
buttons) does nothing until the page is deployed too. Every ship, local
or public, ends by checking it:

```sh
cd ../horadric.portal
git log --oneline origin/main..main
curl -s https://portal.horadric.dev/sw.js | grep "CACHE ="
grep "CACHE =" public/sw.js
```

If there are unpushed commits or the live `CACHE` differs from the repo's,
run `npm test`, then `sh deploy.sh`, then `git push`. Saying "ship" is the
go ahead for this deploy, since the page only adds what the PC sends and
still works against an older PC.

## 7. Shipping from inside Horadric

The agent working on Horadric runs in a terminal of the installed Horadric. It
builds and tests dev instances as above and never touches the installed
one, with one exception: shipping, and only when the human says to ship.
There are two kinds, and the human's words pick one.

- **"Ship" or "ship local"** updates only this machine. Nothing goes to
  GitHub.
- **"Ship public"** cuts a release every install is offered: bump the
  version, sign, tag, push, draft the GitHub release with notes, publish
  it, then ship local. The steps
  are in [RELEASING.md](RELEASING.md). Saying "ship public" is the
  human's go ahead to publish, which is the moment every install sees
  it, so the agent does not stop at the draft to ask again. It picks the
  version (patch for fixes, minor for new features), writes the notes
  for users from the commits since the last tag, and says both before
  publishing.

Two things besides the human's words are a go ahead. The first is the
"Warriv drives" switch. Flipping it on is the human's go ahead for
Warriv to cast "Ship Local" itself. "And ships public", the second
switch, is the go ahead for public releases too. Since Warriv's rounds
were removed (2026-10-30) nothing tells it when shipping is due, so a
driven project ships when the human or an armed errand casts the stone.
A build that rolled back, or checks red on two landings in a
row, holds shipping ("shipping held" on the switch) until the fix-up
quest Horadric files lands on green.

The second is an errand. A stone with `"every"` runs only once the
human has armed it in the Runetome, after reading its steps and its
schedule. Arming an errand that publishes (a push, a release, a post,
"ship public") is the human's standing go ahead for it: an errand session
told to ship public does so without the human saying it that day. A
stone whose steps or mode change is disarmed until the human arms it
again, so that go ahead covers only what they read.

One ship at a time per project. If another Ship Local or Ship Public
session of the project is open, do not ship: say so and stop. Its build
will carry everything on `main` when it reloads.

Ship local runs no checks at all: no fmt, no clippy, no tests, no
`horadric heavy`. The release build is the compile check; if it fails,
the ship stops and the error is reported. The checks belong to ship
public, and run in CI (section 2).

Build from a tree with no one
else's uncommitted edits in code: `cargo build --release`, which is
the compile check, then `target\release\horadric.exe reload`. Before `reload`, deploy the Town Portal page if it is behind
(RELEASING.md, "The Town Portal page"), for ship local and ship public
alike. The installed Horadric hands over to the new build at once, and
the new build attaches to every session's host, including the agent's
own, which keeps running through it. An installed Horadric from before
session hosts waits until no session is mid turn and resumes them
instead. Either way the agent says what it shipped before it runs
`reload`, ends its turn, and does nothing after. A build that does not
come up is rolled back to the old binaries by itself. What happened is in
`%APPDATA%\Horadric\reload.log`.

`reload` with `HORADRIC_DEV=1` restarts the dev instance the same way, from
its own build, which is how to test a change to reloading itself.

The first time, and whenever the installed Horadric is older than `reload`,
it has to be installed by hand from a terminal outside Horadric: Quit from
the tray (sessions pause), `cargo build --release`,
`target\release\horadric.exe install`, then click each tile to resume.
