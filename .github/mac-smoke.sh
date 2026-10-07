#!/bin/bash
# The Mac's "verified on screen", for a Mac nobody sits at: a dev
# instance started with a shell in place of the agent, fake sessions
# posted to its port, a real session started through `horadric new`, and
# every window drawn into a PNG by HORADRIC_SNAPSHOT. Then the app is
# killed and started again, to see the session's host outlive it and the
# new app attach to it. Checks what it can and leaves the PNGs in $OUT.
set -u
BIN="${BIN:-target/debug/horadric}"
OUT="${OUT:-$PWD/mac-smoke}"
PORT=43118
export HORADRIC_DEV=1
export HORADRIC_AGENT=/bin/zsh
export HORADRIC_SNAPSHOT="$OUT/snap"
STATE="$HOME/Library/Application Support/Horadric-dev"
rm -rf "$OUT" "$STATE"
mkdir -p "$OUT"
failed=0
fail() { echo "FAIL: $*"; failed=1; }

up() {
  for _ in $(seq 1 100); do
    if nc -z 127.0.0.1 "$PORT" 2>/dev/null; then return 0; fi
    sleep 0.2
  done
  return 1
}

start_app() {
  "$BIN" app >"$OUT/app-$1.log" 2>&1 &
  echo $! >"$OUT/app.pid"
  up || fail "the app did not listen on $PORT ($1)"
}

post() { # id event [extra json]
  curl -s -m 3 -o /dev/null -X POST "http://127.0.0.1:$PORT/horadric/hook" \
    -H "X-Horadric-Session: $1" -H 'Content-Type: application/json' \
    -d "{\"session_id\":\"\",\"hook_event_name\":\"$2\",\"cwd\":\"$3\",\"name\":\"$1\"$4}"
}

start_app first
sleep 2

# Two projects of fake sessions, one working, one waiting, one done.
post alpha HoradricRegister "$PWD" ''
post alpha UserPromptSubmit "$PWD" ',"prompt":"fix the build"'
post alpha PreToolUse "$PWD" ',"tool_name":"Bash"'
post beta HoradricRegister "/tmp" ''
post beta Notification "/tmp" ',"message":"Claude needs your permission to use Bash","notification_type":"permission_prompt"'
post gamma HoradricRegister "/tmp" ''
post gamma Stop "/tmp" ''

# A real session through the command line: zsh in a host's pty.
"$BIN" new --name smoke --cwd "$PWD" -- -c 'echo HELLO-FROM-PTY; stty size; exec /bin/zsh -i' \
  >"$OUT/new.log" 2>&1 || fail "horadric new: $(cat "$OUT/new.log")"
sleep 8

ls -la "$HORADRIC_SNAPSHOT" || true
pane=$(ls "$HORADRIC_SNAPSHOT"/pane-smoke-*.txt 2>/dev/null | head -1)
if [ -n "$pane" ]; then
  echo "--- the smoke session's screen"; cat "$pane"
  grep -q HELLO-FROM-PTY "$pane" || fail "the session's output is not on its screen"
else
  fail "no text of the smoke session's screen"
fi

# Keys through AppKit, where the runner lets System Events type.
if osascript -e 'tell application "System Events" to keystroke "echo TYPED-OK"'   -e 'tell application "System Events" to key code 36' 2>"$OUT/osascript.log"; then
  sleep 3
  if grep -q TYPED-OK "$pane"; then echo "typing reached the session"; else
    echo "WARN: keystrokes were sent but did not reach the session"; fi
else
  echo "WARN: System Events may not type here: $(cat "$OUT/osascript.log")"
fi
for f in stage cluster-0 cluster-1; do
  [ -s "$HORADRIC_SNAPSHOT/$f.png" ] || fail "no $f.png"
done
hosts=$(pgrep -f "horadric host" | wc -l | tr -d ' ')
echo "hosts running: $hosts"
[ "$hosts" -ge 1 ] || fail "no session host is running"
ls -la /tmp/horadric-$(id -u)/ || fail "no socket folder"
cp "$STATE/state.json" "$OUT/state-first.json" 2>/dev/null || fail "no state.json"
grep -q '"smoke' "$OUT/state-first.json" || fail "state.json does not hold the smoke session"
mkdir -p "$OUT/first"
cp "$HORADRIC_SNAPSHOT"/*.png "$OUT/first/" 2>/dev/null

# The app goes; the host must not.
kill -9 "$(cat "$OUT/app.pid")"
sleep 2
hosts=$(pgrep -f "horadric host" | wc -l | tr -d ' ')
[ "$hosts" -ge 1 ] || fail "the host ended with the app"
rm -f "$HORADRIC_SNAPSHOT"/*.png

start_app second
sleep 8
mkdir -p "$OUT/second"
cp "$HORADRIC_SNAPSHOT"/*.png "$OUT/second/" 2>/dev/null
[ -s "$OUT/second/stage.png" ] || [ -s "$OUT/second/cluster-0.png" ] || fail "nothing drawn after the restart"
grep -q "cannot attach" "$OUT/app-second.log" && fail "the second app could not attach: $(grep 'cannot attach' "$OUT/app-second.log")"

# Quit with the sessions ended, as the human would from the menu.
kill -9 "$(cat "$OUT/app.pid")" 2>/dev/null
pkill -f "horadric host" 2>/dev/null
sleep 1

# The bundle a release ships, made the way CI's release build makes it.
"$BIN" bundle "$OUT/Horadric.app" || fail "horadric bundle"
for f in Contents/Info.plist Contents/MacOS/horadric Contents/Resources/horadric.icns; do
  [ -s "$OUT/Horadric.app/$f" ] || fail "the bundle has no $f"
done
plutil -lint "$OUT/Horadric.app/Contents/Info.plist" || fail "Info.plist does not parse"
codesign --verify --verbose "$OUT/Horadric.app" || fail "the bundle's signature does not verify"
sips -g pixelWidth "$OUT/Horadric.app/Contents/Resources/horadric.icns" || fail "the icon does not read"

# A real install, which this throwaway Mac can take: the app in
# ~/Applications, the command linked, the LaunchAgent, the hooks.
unset HORADRIC_DEV HORADRIC_SNAPSHOT
"$OUT/Horadric.app/Contents/MacOS/horadric" install >"$OUT/install.log" 2>&1 || fail "install: $(cat "$OUT/install.log")"
cat "$OUT/install.log"
[ -x "$HOME/Applications/Horadric.app/Contents/MacOS/horadric" ] || fail "not installed in ~/Applications"
[ -L "$HOME/.local/bin/horadric" ] || fail "the command is not linked"
[ -f "$HOME/Library/LaunchAgents/dev.horadric.app.plist" ] || fail "no LaunchAgent"
plutil -lint "$HOME/Library/LaunchAgents/dev.horadric.app.plist" || fail "the LaunchAgent does not parse"
grep -q horadric "$HOME/.claude/settings.json" || fail "no hooks in ~/.claude/settings.json"
nc -z 127.0.0.1 43117 || fail "the installed app is not running"
"$HOME/.local/bin/horadric" hooks status || fail "hooks status"
pkill -f "Horadric.app/Contents/MacOS/horadric app" ; sleep 2
"$HOME/.local/bin/horadric" uninstall >"$OUT/uninstall.log" 2>&1 || fail "uninstall: $(cat "$OUT/uninstall.log")"
cat "$OUT/uninstall.log"
[ ! -e "$HOME/Applications/Horadric.app" ] || fail "uninstall left the app"
[ ! -e "$HOME/Library/LaunchAgents/dev.horadric.app.plist" ] || fail "uninstall left the LaunchAgent"
echo "--- app logs"
cat "$OUT"/app-*.log
exit $failed
