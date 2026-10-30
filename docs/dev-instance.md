# The dev instance

How to verify Windows code and test against a real agent. Read it when you build, run or screenshot Horadric.

## Verifying Windows code

Tests cannot tell you a window looks right. The loop that works:

1. Always test as a dev instance: `HORADRIC_DEV=1`. It listens on 43118,
   keeps its state in `%APPDATA%\Horadric-dev`, never turns on autostart, has
   a red lit tray icon, and refuses `install`, `uninstall` and hook or Explorer
   changes. The installed Horadric keeps running beside it.
2. Stop only the dev build, never every `horadric.exe`. You may be running
   inside a terminal of the installed one, and killing it kills you.
   `Get-Process horadric | ? Path -like '*\target\*' | Stop-Process -Force`,
   then build. The installed copy lives in `%LOCALAPPDATA%\Programs\Horadric`,
   so it never locks `target`.
   A dev instance leaves the quest log of every project the installed
   Horadric knows (its recent list) to the installed one: its runner and
   Warriv skip them, so trying a build never takes a real quest and
   strands it when the dev instance stops. Set `HORADRIC_DEV_QUESTS=1`
   to test the runner itself, or use a folder only the dev instance knows.
3. Start the tiles with `target\debug\horadric.exe app` in the background
   (plain `horadric` starts it hidden and returns). Use `Start-Process
   -NoNewWindow`, never `-WindowStyle Hidden`: Windows turns the first
   window the app shows into a hidden one, and the stage never appears.
   Then `horadric new` or post
   fake sessions at port 43118 with a short Python script. `HORADRIC_AGENT=cmd.exe`
   puts a shell in the terminals instead of `claude`.
4. Never leave a test running unwatched that can start agents. A resume bug
   once started 167 `claude` processes in a minute. Count `claude.exe`
   children of the dev instance's `horadric-host-*.exe` processes after any
   change to how sessions start.
5. Each session runs in a host process, `horadric-host-<build>.exe` in
   `%LOCALAPPDATA%\Horadric-dev\hosts`, which outlives the dev UI on
   purpose. When a test is done, stop those too: `Get-Process
   horadric-host* | ? Path -like '*\Horadric-dev\hosts\*' | Stop-Process
   -Force`. The installed Horadric's hosts are in `Horadric\hosts`; leave
   them alone, one of them runs you.
   A test also leaves no tiles behind. Before starting the dev instance,
   copy `%APPDATA%\Horadric-dev\state.json` to `state.json.before-test`.
   When done, stop the dev instance and its hosts, then copy it back.
   The human opens the dev instance too, and forty leftover test
   projects bury what they came to look at.
6. Screenshot the top right corner with PowerShell and `CopyFromScreen`, then
   read the image. For a window that is behind another, `PrintWindow` with
   flag 2 captures it anyway.
7. `HORADRIC_DEBUG=1` makes the app log cluster positions, sizes and paints.

Two bugs found this way that tests would never have caught: a window born
with its final layout never resized past 10 pixels, and a window created off
screen never painted after being moved into view.

## Testing against a real agent

`horadric run --name x` in a project starts a tagged `claude`. For a
non-interactive check, add `-- -p "Reply with pong" --model
claude-haiku-4-5-20251001`. The hooks are in `~/.claude/settings.json` and a
backup of the pre-Horadric file sits beside it. A `claude` started by a dev
instance still posts to the installed Horadric, which passes the event on to
the dev one by the `X-Horadric-Port` header.
