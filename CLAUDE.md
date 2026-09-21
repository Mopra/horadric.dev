# Working on Glance

Glance shows every coding agent session you have running as a tile on your
Windows desktop, grouped by project. Read [README.md](README.md) for what it
does and [docs/PLAN.md](docs/PLAN.md) for where the work is up to and what
comes next. Read the plan before starting anything.

The original concept lives outside this repo at
`../ideas.repo/glance/glance-concept.md`. It holds the problem statement and
the reasoning. The plan supersedes it wherever the two disagree.

## The decisions that are settled

Do not reopen these without asking. They were argued through and chosen.

- **Pure Rust, no toolkit, no web view.** Win32 through the `windows` crate,
  Direct2D and DirectWrite for drawing. Performance is the reason and so is
  control: the window manager fight is the whole project, and a layer in
  between makes it unwinnable.
- **One window per project cluster, not per tile.** Tiles are drawn inside
  the cluster window. Only an expanded terminal gets a window of its own.
  Forty tiles must not mean forty windows.
- **State comes from hook events, never from parsing terminal output.**
- **Dependencies are justified one at a time.** Today: `serde`, `serde_json`,
  `windows`, `windows-numerics`. Adding one is a decision, not a reflex.

## Conventions

- No em dashes, en dashes or double hyphens anywhere, including code comments
  and commit messages. Use a full stop, a comma, a colon or parentheses.
- Comments say why, not what. If a line needs a comment to say what it does,
  rewrite the line.
- Every pure function gets a test. Anything touching Win32 gets verified on
  screen instead, see below.
- `cargo fmt --all`, then `cargo clippy --workspace --all-targets -- -D
  warnings`, then `cargo test --workspace`. All three before every commit.
- Commit messages: one line saying what changed, a blank line, then why.

## Verifying Windows code

Tests cannot tell you a window looks right. The loop that works:

1. `taskkill //IM glance.exe //F` so the binary is not locked, then build.
2. Start the tiles, then post fake sessions at the listener with a short
   Python script (`scratchpad/fake.py` in earlier sessions, recreate it).
3. Screenshot the top right corner with PowerShell and `CopyFromScreen`, then
   read the image. For a window that is behind another, `PrintWindow` with
   flag 2 captures it anyway.
4. `GLANCE_DEBUG=1` makes the app log cluster positions, sizes and paints.

Two bugs found this way that tests would never have caught: a window born
with its final layout never resized past 10 pixels, and a window created off
screen never painted after being moved into view.

## Testing against a real agent

`glance run --name x` in a project starts a tagged `claude`. For a
non-interactive check, add `-- -p "Reply with pong" --model
claude-haiku-4-5-20251001`. The hooks are in `~/.claude/settings.json` and a
backup of the pre-Glance file sits beside it.
