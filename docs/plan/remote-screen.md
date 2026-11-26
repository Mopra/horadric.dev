# Remote screen: watching another machine's desktop from the browser pane

A session that works on another Windows machine (a VM in Azure, say)
drives it over SSH, and the human used to follow along only through RDP
in a window of its own, while the session saw still screenshots. Twice a
task stalled on a dialog nobody saw in time. This puts the remote
desktop, live, in a tab of the browser pane, where the human and the
session see the same picture, and the human takes over mouse and
keyboard with one click.

## The shape

The remote machine runs Horadric too. Its app serves its own desktop:
frames as JPEG, input posted back, on `127.0.0.1` only. This machine
reaches it through an SSH tunnel (`ssh -N -L`), so the remote opens no
port beyond the SSH it already has. The viewer is a page the remote
serves, so the pane shows it as any web page, and a session opens it
with `remote_open`, once the human has said yes to that destination.

Not RDP, on purpose: an RDP login takes the console session over, and
closing the window locks the desktop, which desktop control can not
work on. Watching the app's own capture of the console leaves the
console as it is.

## What this trusts

- **The SSH key and the human's yes.** Whoever can log in to the remote
  with the key can read the token, and the token is the whole key to
  the desktop. On this machine a session's tools do nothing for a
  destination until the human has allowed it, once per project and
  destination.
- **The token equals running code as that user on the remote.** It
  types and clicks, and Win+R can be typed. Treat it as a login, not as
  a view.
- **The gate is on the tools, not on the machine.** An agent with a
  shell and the same SSH key could reach the remote anyway: read the
  token, open a tunnel, post input. The question the human answers
  governs the `remote_` tools and makes their use seen; it does not
  wall the remote off from a session that has a terminal.
- **Do not run the remote's Horadric elevated on a machine you do not
  fully trust.** Elevated, it reaches administrator windows, and then
  any process of that user that reads the token file types into them
  too. Run it as the plain user; input to elevated windows is refused
  with the reason said.
- **The far end is proven before it is told anything.** The token goes
  only to something that has shown it already holds it, so a program
  that took the port, here or there, learns nothing.

## The server (the remote's app)

Off unless the user environment variable `HORADRIC_SCREEN_SHARE=1` is
set when the app starts (a normal install serves nothing new). Then the
app listens on `127.0.0.1:43119` (`43120` for a dev instance).

The token is 32 hex characters in `<state dir>\screen-token`, made
fresh at every start of the app and never used again after it. It is
sent only as the header `Authorization: Bearer <token>`; no address
carries it. A missing or wrong one: `403`. Reading it takes a login on
the machine, so a tunnel alone is not enough.

The proof: `GET /hello?n=<nonce>` needs no token. `n` is 32 hex
characters, and the answer is the HMAC-SHA256 of the nonce under the
token, as lower case hex. The client and the viewer ask it with a fresh
random nonce before the first request that carries the token, and again
after a failure, and send nothing when the answer is wrong.

Every request must name `127.0.0.1` or `localhost` as its `Host`, and
an `Origin`, when there is one, must be the same place, else `403`. All
but `GET /` and `GET /hello` need the token.

- `GET /` the viewer page (HTML, self contained). It is opened as
  `http://127.0.0.1:<port>/#t=<token>`: the fragment goes to no server,
  and the page moves it into the tab's `sessionStorage` and clears it
  from its address before anything else. It may not be framed, and is
  not cached.
- `GET /frame.jpg?w=<max width>` the whole virtual screen (all
  monitors) as a JPEG with the cursor drawn in, scaled down to at most
  `w` pixels wide when `w` is given. Headers `X-Screen-Left`,
  `X-Screen-Top`, `X-Screen-Width`, `X-Screen-Height` give the virtual
  screen in physical pixels. `423` with the reason as text while the
  desktop is locked or shows a UAC prompt, so the viewer says why
  there is no picture.
- `POST /input` one JSON event, or an array of up to 256, run in order
  as one sequence that no other request's input comes between.
  Coordinates are virtual screen pixels from its top left:
  `{"kind":"move","x":10,"y":20}`,
  `{"kind":"down"|"up","x":..,"y":..,"button":"left"|"right"|"middle"}`,
  `{"kind":"wheel","x":..,"y":..,"delta":-120}` (vertical) and
  `"hwheel"` (horizontal), both held to 12000 either way,
  `{"kind":"key","vk":13,"down":true}`,
  `{"kind":"text","text":"æøå"}` (typed as Unicode; a line break is a
  real Enter, a tab a real Tab),
  `{"kind":"alive"}` (nothing, but the sender still holds what it
  holds) and `{"kind":"release"}` (let go of every key and button).
  The sleep key, the mouse button codes and the browser, media and
  launch keys are refused.
  Answers `204`, `400` with a reason, `413` for a body over 64 KiB, or
  `409` with the real reason, found by looking before anything is sent:
  the desktop is locked or shows a UAC prompt, or the window there runs
  as administrator and this Horadric does not.
  The server remembers every key and button it put down. A sequence
  that fails midway lets go of them all, and so does a watchdog when
  something is held and no input request has come for 5 seconds.
- `GET /display` the primary display's resolution and the ones it
  offers, as JSON
  `{"current":{"width":2560,"height":1440},"modes":[{"width":2560,"height":1440},{"width":1920,"height":1080}]}`.
- `POST /display` with `{"width":1920,"height":1080}` tries the size
  first, then switches to it without writing the registry, and answers
  `{"keep_within":15}`. Unless `POST /display/keep` arrives within
  those 15 seconds, the display goes back to what it was, so a size the
  screen can not show undoes itself. `400` when the size is not
  offered, `409` with the name of the `DISP_CHANGE` code Windows
  refused it with. This changes the remote machine's own desktop,
  unlike the viewer's zoom, which only scales the picture here.

At most 16 connections at once (more get `503`), 10 seconds to send a
request's head, one capture at a time.

On the remote itself, `<state dir>\screen-share.log` (about 1 MiB at
most) says when the server started and on which port, when a viewer
arrived, when input began and when the display changed, never the
token. Its Horadric shows a toast when a viewer arrives and when input
begins, so whoever sits there knows.

## The client (this machine)

`horadric remote open <ssh destination>`, typed by the human, and the
tool `remote_open {"ssh": "<destination>"}`:

1. Read the token with
   `ssh <destination> cmd /c type "%APPDATA%\Horadric\screen-token"`,
   killed if it has not answered in 30 seconds. The token first, so a
   host that shares no screen is left with no tunnel.
2. Start `ssh -N -L 127.0.0.1:<free local port>:127.0.0.1:43119 -o
   ExitOnForwardFailure=yes <destination>`, kept for the destination
   and used again while it lives, under a lock file so two sessions
   opening one machine at once share one tunnel.
3. Ask `/hello` through it. Up means our `ssh` and a right proof, not a
   port that takes connections: another program can take the local
   port before `ssh` has logged in, and it is the proof that keeps the
   token from it. A wrong proof closes the tunnel and says that
   something else answers on the port.
4. Ask `/display` with the token, to see the remote really serves, and
   open `http://127.0.0.1:<port>/#t=<token>` in a tab of the browser
   pane.

A destination is letters, digits and `. _ @ : [ ] -` only, and does not
start with `-`. The token goes in no log, address or reply text.

Each tunnel is remembered with its pid and with when its process was
created. Before a tunnel is killed that must still match (and on
Windows the program must still be `ssh.exe`), so a pid that went to
another program is left alone. A tunnel remembered without it, by an
older Horadric, is forgotten and not killed.

The MCP server keeps the token it read in memory, per destination, so a
click is not an SSH login. When the remote stops taking it (its app
started again, so it made a new one), the token is read once more and
the call tried again.

Every request has a deadline for the whole of it (20 seconds, 30 for a
frame) and a cap on its answer (64 KiB, 32 MiB for a frame), and
reading stops at the length the answer gave.

`horadric remote close <ssh destination>` ends the tunnel kept for it,
forgets it, and has the app forget its yes for that destination, so a
session can not quietly open it again. The tunnel to a dev instance is
closed with `--dev`.

### The gate

Before `remote_open`, `remote_screenshot` or an input tool does
anything, before any `ssh` starts, the MCP server asks the app
(`{"op":"remote_allow","ssh":"<destination>"}`). The app asks the human
once per project and destination ("Session X of project Y wants to
watch and drive the desktop of ..."), remembers a yes until the app
restarts and a no for 5 minutes. A no, or no answer, is the tool's
error, and nothing was started. The tools work only in a session
Horadric started, since only there is an app to ask.
`{"op":"remote_forget"}` drops the yes, which is what `remote close`
sends. The command the human types is not gated: typing it is the yes.

### The tools

`remote_screenshot {"ssh", "width"?}` fetches `/frame.jpg` through the
tunnel and gives it to the session as an image: the picture the human
sees. Its text line gives the screen's size and, read from the JPEG's
own header, the picture's, with the factor that turns a spot in the
picture into screen pixels ("multiply image coordinates by 1.6"),
since the picture is scaled down to `width` (1600 unless asked).

The session operates the desktop the way it drives a web page. Each
act is one request, one sequence for the remote to run whole:

- `remote_click {ssh, x, y, button?: "left"|"right"|"middle", double?}`
  a move, then down and up (twice for a double click).
- `remote_type {ssh, text}` `text` events, a piece of at most 4000
  characters per request. A line break presses Enter, a tab Tab.
- `remote_press {ssh, key}` a key ("Enter", "Tab", "Escape", "F5",
  "PageDown", "PrintScreen", a letter, a digit, one of `- = , . /`) or
  a chord ("Ctrl+S", "Alt+F4", "Ctrl+Shift+Esc", "Win+R"): the
  modifiers go down in order, then the key, and all come up in
  reverse.
- `remote_scroll {ssh, x, y, delta}` one `wheel` event, `delta` in
  notches (times 120), negative scrolling down, 100 at most; the
  answer says what was sent.

x and y are screen pixels from the top left of the whole virtual
screen, any number, rounded. Each answers a short line ("clicked at
812,440") and no picture: the descriptions tell the session to look
with `remote_screenshot`, act, then look again. Anything but `204`
stops there, the remote's own words (500 characters of them at most)
become the tool's error, and a `release` is posted after it, so
nothing stays held down whatever went wrong.

## Limits now

- Input from the app reaches programs at its own integrity level. A
  program that runs as administrator takes clicks only when the
  remote's Horadric runs elevated too, which "What this trusts" says
  not to do on a machine you do not fully trust.
- The viewer is frames polled a few times a second, not video.
- Watching does not count as use for the VM's idle shutdown.
- A yes is per project and destination and lasts until the app
  restarts; there is no list of them to look at yet.
- The tools scroll vertically only; `hwheel` is the viewer's.

## Left

The human's test against a real remote: open, watch, take over, a
click and a chord from a session, the question and a no, `remote
close`, and the remote's app started again while a tunnel is up.
