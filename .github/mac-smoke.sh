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
echo "--- app logs"
cat "$OUT"/app-*.log
exit $failed
