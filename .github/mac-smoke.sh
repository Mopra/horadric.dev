#!/bin/bash
# The Mac's "verified on screen", for a Mac nobody sits at: a dev
# instance started with a shell in place of the agent, fake sessions
# posted to its port, a real session started through `horadric new`, and
# every window drawn into a PNG by HORADRIC_SNAPSHOT, and a Folio opened
# on a stand in for claude (.github/fake-claude.py). Then the app is
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

# A stand in for claude first on the PATH, so a Folio opens with no
# Claude account, in a folder Claude Code is told it trusts, as the
# Folio asks before it starts claude in any other.
FAKE="$OUT/fake-bin"
mkdir -p "$FAKE"
cp .github/fake-claude.py "$FAKE/claude"
chmod +x "$FAKE/claude"
export PATH="$FAKE:$PATH"
FOLIO_DIR="$OUT/folio-project"
mkdir -p "$FOLIO_DIR"
printf 'first line
old line
' >"$FOLIO_DIR/notes.txt"
python3 - "$FOLIO_DIR" <<'PY'
import json, os, sys
path = os.path.expanduser("~/.claude.json")
try:
    config = json.load(open(path))
except (OSError, ValueError):
    config = {}
config.setdefault("projects", {})[sys.argv[1]] = {"hasTrustDialogAccepted": True}
json.dump(config, open(path, "w"), indent=2)
PY

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
pane=$(ls "$HORADRIC_SNAPSHOT"/pane-smoke-*.txt 2>/dev/null | grep -v tmp | head -1)
if [ -n "$pane" ]; then
  echo "--- the smoke session's screen"; cat "$pane"
  grep -q HELLO-FROM-PTY "$pane" || fail "the session's output is not on its screen"
else
  fail "no text of the smoke session's screen"
fi

# Keys through AppKit, where the runner lets System Events type.
app_pid=$(cat "$OUT/app.pid")
osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $app_pid) to true" 2>>"$OUT/osascript.log"
sleep 1
osascript -e 'tell application "System Events" to name of first process whose frontmost is true' 2>>"$OUT/osascript.log"
if osascript -e 'tell application "System Events" to keystroke "echo TYPED-OK"' -e 'tell application "System Events" to key code 36' 2>>"$OUT/osascript.log"; then
  sleep 3
  if grep -q TYPED-OK "$pane"; then echo "typing reached the session"; else
    echo "WARN: keystrokes were sent but did not reach the session"; fi
else
  echo "WARN: System Events may not type here: $(cat "$OUT/osascript.log")"
fi
# A bare start opens a Folio: the fake claude greets and asks to edit.
"$BIN" new --cwd "$FOLIO_DIR" >"$OUT/new-folio.log" 2>&1 || fail "horadric new (Folio): $(cat "$OUT/new-folio.log")"
sleep 8
folio=$(ls "$HORADRIC_SNAPSHOT"/pane-folio-project*.txt 2>/dev/null | grep -v tmp | head -1)
if [ -n "$folio" ]; then
  echo "--- the Folio's conversation"; cat "$folio"
  grep -q FAKE-CLAUDE-HELLO "$folio" || fail "the Folio did not show the fake claude's reply"
  grep -q "Claude asks to change" "$folio" || fail "the Folio did not show the edit ask"
  grep -q "+ new line" "$folio" || fail "the Folio did not show the edit's diff"
else
  fail "no text of the Folio (a Folio did not open)"
fi
cp "$HORADRIC_SNAPSHOT/stage.png" "$OUT/folio-stage.png" 2>/dev/null
# Typed into its composer, a prompt reaches the fake, which says it back.
osascript -e 'tell application "System Events" to keystroke "hello folio"' -e 'tell application "System Events" to key code 36' 2>>"$OUT/osascript.log"
sleep 3
if [ -n "$folio" ] && grep -q "FAKE-CLAUDE-HEARD: hello folio" "$folio"; then
  echo "typing reached the Folio"
else
  echo "WARN: a prompt typed into the Folio did not reach claude"
fi
cp "$HORADRIC_SNAPSHOT/stage.png" "$OUT/folio-typed.png" 2>/dev/null

for f in stage cluster-0 cluster-1 settings usage; do
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
rm -f "$HORADRIC_SNAPSHOT"/*.png "$HORADRIC_SNAPSHOT"/pane-folio-project*.txt

start_app second
sleep 8
mkdir -p "$OUT/second"
cp "$HORADRIC_SNAPSHOT"/*.png "$OUT/second/" 2>/dev/null
[ -s "$OUT/second/stage.png" ] || [ -s "$OUT/second/cluster-0.png" ] || fail "nothing drawn after the restart"
grep -q "cannot attach" "$OUT/app-second.log" && fail "the second app could not attach: $(grep 'cannot attach' "$OUT/app-second.log")"
# The Folio's host ran on too: the new app found it and read it back.
folio=$(ls "$HORADRIC_SNAPSHOT"/pane-folio-project*.txt 2>/dev/null | grep -v tmp | head -1)
if [ -n "$folio" ] && grep -q FAKE-CLAUDE-HELLO "$folio"; then
  echo "the Folio came back after the restart"
else
  fail "the Folio did not come back after the restart"
fi

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
# Through install.sh, as a user would, from the archive a release ships.
unset HORADRIC_DEV HORADRIC_SNAPSHOT
(cd "$OUT" && tar -czf Horadric-macos.tar.gz Horadric.app)
HORADRIC_DOWNLOAD="file://$OUT/Horadric-macos.tar.gz" sh install.sh >"$OUT/install.log" 2>&1   || fail "install.sh: $(cat "$OUT/install.log")"
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
