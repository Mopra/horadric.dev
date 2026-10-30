# Working on Horadric

Horadric shows every coding agent session you have running as a tile on your
desktop (Windows first, with a macOS port), grouped by project. Read
[README.md](README.md) for what it does and [docs/PLAN.md](docs/PLAN.md) for what comes next. Read the
plan's index before starting anything, then only the parts in
`docs/plan/` that your work touches. What was decided about anything
already built is in the lore, not the plan: ask `lore_recall`. Warriv is
the part that drives a project's quest log while the human is away.

The original concept lives outside this repo at
`../ideas.repo/glance/glance-concept.md`. It holds the problem statement and
the reasoning. The plan supersedes it wherever the two disagree.

## The decisions that are settled

Do not reopen these without asking. They were argued through and chosen.

- **Pure Rust, no toolkit, no web view for Horadric's own UI.** Win32
  through the `windows` crate, Direct2D and DirectWrite for drawing (on
  macOS, AppKit through the `objc2` crates).
  Performance is the reason and so is control: the window manager fight
  is the whole project, and a layer in between makes it unwinnable. The
  one web view is the browser pane, which shows web pages, not Horadric
  (decided 2026-09-29, the lore has why).
- **One window per project cluster, not per tile.** Tiles are drawn inside
  the cluster window. Forty tiles must not mean forty windows.
- **One terminal window for all sessions, the stage.** It shows one
  project at a time, each of its sessions a pane in a grid, and a tile
  click switches the project. No session gets a window of its own. Ten
  sessions must not mean ten terminals piled up.
- **State comes from hook events, never from parsing terminal output.**
  The one exception is OSC 7501, a status the program sends on purpose
  (decided 2026-10-11, the lore has why).
  Hooks win where both speak.
- **Dependencies are justified one at a time.** The list and the why of
  each is in [docs/dependencies.md](docs/dependencies.md). Adding one is a
  decision, not a reflex.

## The crates

- `horadric`: the binary and its commands (`app`, `run`, `install`, `reload`).
- `horadric-core`: the pure logic, state, quests, merging. Most tests live here.
- `horadric-hooks`: the hook events agents post and how they are read.
- `horadric-link`: the relay and the crypto for linked machines.
- `horadric-memory`: the embedding model that finds memory by meaning, and Whisper for dictation.
- `horadric-pty`: the pseudo console and the session hosts.
- `horadric-ui`: windows, tiles, the stage, the browser pane (`src/mac/` for macOS).

Pointers, read when your work touches them: [the brotherhood](docs/plan/the-brotherhood.md),
[its ownership](docs/plan/brotherhood-ownership.md), [one portal](docs/plan/one-portal.md),
[lore](docs/plan/lore.md), [memory](docs/plan/memory.md), [macOS](docs/plan/macos.md).

## Conventions

- No em dashes, en dashes or double hyphens anywhere (code comments and commits too).
- Comments say why, not what. If a line needs a comment to say what it does,
  rewrite the line.
- Pure logic that is easy to get wrong gets a test. Run just that test
  (`cargo test -p <crate> <name>`) when it is quicker than trying it.
- Anything touching Win32 or AppKit is looked at by the human, see below.
- The Rust version is pinned in `rust-toolchain.toml`, for this machine
  and CI alike. Moving it is a commit of its own, with whatever new
  clippy lints it brings fixed in the same commit.
- Commit messages: one line saying what changed, a blank line, then why.
- Work lands on `main`. This repository is in trunk mode: sessions share
  the main checkout and commit on `main` directly, small and often,
  staging only their own files by name. Other agents may be editing
  beside you, so never `git add -A`, `git stash` or `git checkout --` a
  file you did not change. A worktree only when the human asks for one.
- When you are in a worktree anyway (you were asked, or it is a
  parallel task), finish by merging it into `main`: merge `main` into
  the branch first if it has moved, then
  `git -C <main checkout> merge --ff-only <branch>`. After resolving a
  conflict, `cargo check` is enough. No branch is left
  with work `main` lacks.

## The loop: fast by default

Speed of iteration beats ceremony. A broken lint on `main` costs a
minute to fix later; a 30 minute check run costs 30 minutes every time.

1. Write the code.
2. `cargo check -p <crate you touched>`. That is the whole check.
3. If it is UI, build and start the dev instance once and tell the human
   what to look at. Do not screenshot or inspect it yourself unless asked.
4. Commit and say what changed.

Not part of the loop: `cargo fmt`, `cargo clippy`, `cargo test --workspace`,
`horadric heavy`, building for Mac or Linux (no `--target` builds, no
`cargo zigbuild`, no waiting on CI). Workspace fmt, clippy and tests run
once, in CI, at ship public. Run more only when the human asks.

## Verifying UI code

The human looks at the dev instance; the agent builds and starts it.
Details in [docs/dev-instance.md](docs/dev-instance.md), which also
covers testing against a real agent. The rules that must not be lost:

- Always test as a dev instance: `HORADRIC_DEV=1` (port 43118, state in
  `%APPDATA%\Horadric-dev`, refuses `install`, `uninstall` and hook changes).
- Stop only the dev build, never every `horadric.exe`. You may be running
  inside a terminal of the installed one, and killing it kills you.
  `Get-Process horadric | ? Path -like '*\target\*' | Stop-Process -Force`
- Start it with `Start-Process -NoNewWindow`, never `-WindowStyle Hidden`.
- Never leave a test running unwatched that can start agents. A resume bug
  once started 167 `claude` processes in a minute.
- Each session runs in a host process that outlives the dev UI. Stop the dev
  ones when done: `Get-Process horadric-host* | ? Path -like
  '*\Horadric-dev\hosts\*' | Stop-Process -Force`. The installed Horadric's
  hosts are in `Horadric\hosts`; leave them alone, one of them runs you.
- A test leaves no tiles behind. Copy `%APPDATA%\Horadric-dev\state.json`
  aside before the test and put it back after stopping the dev instance.

## Shipping

"Ship" or "ship local" updates only this machine, nothing goes to GitHub.
"Ship public" cuts a release every install is offered, and is the human's
go ahead to publish. The agent working on Horadric never touches the
installed one except to ship, and only when the human says to ship, the
"Warriv drives" switch allows it, or an armed errand says so.

Ship local runs no checks: `cargo build --release` is the check, then
`reload`. Ship public pushes first so CI runs fmt, clippy and the tests
on all three systems while the release builds; only a red test holds
the publish. The steps (Warriv's switches, errands, the build, the Town
Portal page, `reload`, the first install) are in
[RELEASING.md](RELEASING.md), sections 2 and 7.
