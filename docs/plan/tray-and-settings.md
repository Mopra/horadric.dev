# The tray menu and Settings

Steps 1 to 4 landed 2026-10-30; search, a resizable window and the DPI
check are left. Where the build differs from the plan below: the tray
keeps Accounts as a submenu (switching account is an action, done
often), the machines submenu is called Other PCs, Discord sits in
General and About holds only the version and updates. Proposed
2026-10-30. Both have grown one item at a time and it shows.
The tray menu reaches about 25 lines with no grouping, and Settings is a
fixed box of unexplained rows. This page has the research, what is wrong,
and the rewrite.

## What the research says

Sources are linked at the end.

- **Microsoft, notification area:** the icon is for status, "not intended
  for quick program or command access". Left click shows what the user
  most likely wants to see. Right click opens the menu, with the default
  command bold and first. Order: open, then a few checkmarks for core
  state, then Options, then Exit. Remove items that don't apply rather
  than greying them. The tooltip is "Name - status".
- **Microsoft, menus:** at most 15 items in a context menu, groups of 7 or
  fewer, avoid submenus in context menus. An ellipsis only when the
  command asks for more before it runs. Name items by the user's goal.
- **Windows 11 settings (WinUI, PowerToys):** a left nav, a single
  scrolling column of cards. Each card is a title, a one-line description
  and its control on the right. Rare sub-options go in an expander, one
  level deep. Changes apply at once, never a Save button. A disabled
  setting stays visible and says why. About sits at the bottom.
- **Search:** PowerToys added it in 0.94 after reviewers said "it's too
  easy to get lost". Ctrl+F jumps to it.
- **Good tray menus are short.** Docker's runs to about 12 lines and is
  already called padded. Tailscale is the one app where checkmarks in
  the menu are right, because they are the product's core switches.
- **Settings and a file:** VS Code, Zed and Windows Terminal all keep a
  file under the UI, with an "open the file" escape hatch. Zed's lesson:
  fix the settings model first, then draw it.
- **Jargon:** themed names make people look outside the panel for what a
  setting means. A name the user can't decode needs a plain description.
- **Custom drawn menus:** Steam's breaks on scaled displays. Ours must be
  checked at 100% to 200% and on every monitor.

## What is wrong now

The tray menu (`tray.rs`, `menu`):

- About 25 top level lines in one flat list.
- Recent projects appear twice, once to start a session and once under
  Quest log.
- Five window commands crowd the top (Show terminal, Bring tiles to
  front, Fit terminal beside tiles, Tidy up tiles, Show hidden tiles).
  Fit and Tidy are hard to tell apart.
- Settings live in the menu: Account switching, Can control this PC,
  Keeps my sessions, Take over after 10 minutes, Linux node join and
  forget.
- Machine linking is spread over seven lines (Signed in as, Brothers,
  Leaving, Keeps, Take over, Sign out, Linux nodes).
- Left and right click open the same menu.
- Greyed items stay in (Show terminal, Stop Warriv).
- The first line is Level and XP, a game stat, where the status should be.

Settings (`settings.rs`):

- A fixed 604 by about 400 window: no resize, no scroll, so the tallest
  section sets the height and the planned rows (secrets, identities,
  dictation) have no room.
- No descriptions. "In the project menu", "Workflow agents' model",
  "Recall across projects", "The tongue", "Ask before a click casts" mean
  nothing to a new user.
- Ten sections in a doubtful order: Projects, probably the most used, is
  last.
- Test numbers shown as settings: "Answers right 59 of 65, verbatim 52" is
  hardcoded, and the sigil stats belong in a report, not here.
- No keyboard: rows can't be focused, Tab does nothing.
- Nothing to search.

## The tray, rewritten

**Left click** brings the stage and the tiles to the front, which is what
the user wants nine times in ten. **Double click** is the same.
**Right click** opens the menu. The tooltip becomes "Horadric - 5
sessions, 2 waiting".

The menu, at most 12 lines, in four groups:

```
5 sessions, 2 waiting                      (status, not clickable)
Show terminal                              (bold, the default)
Next waiting session             (its hotkey)
────────────────────────────
New session…
Recent projects                 ▸          (start a session there)
────────────────────────────
Warriv                          ▸          drives per project ✓, ships public ✓, Stop
Arrange                         ▸          tiles to front, fit terminal, tidy, hidden tiles
Machines                        ▸          brothers, leaving, Linux nodes, sign in/out
────────────────────────────
Settings…                         Ctrl+,
Update to 0.x…                             (only when there is one)
Quit Horadric
```

Where the rest goes:

- **Quest log, Context, Stay a while and listen:** onto the project's own
  menu on its tiles, where the project is already known. Stay a while
  keeps its hotkey.
- **Account switching:** Settings, Sessions. Agent logins are settings.
- **Can control this PC, Keeps my sessions, Take over after 10 minutes:**
  Settings, Machines. The Machines submenu keeps only actions: hand off,
  ask to join, cast out, sign in, sign out. This is the same merge
  [one portal](one-portal.md) wants for the Brothers menu.
- **End all sessions:** a button in the Quit dialog, which already asks.
- **Level and XP:** off the menu. It can live on the About page or the
  tooltip of a tile, not where status belongs.
- Items that don't apply are left out, not greyed.

Warriv stays in the menu, as the one exception the current code already
argues for: the go ahead to ship should be one click and in plain sight.
It is Tailscale's case: core switches, as checkmarks.

## Settings, rewritten

**The window:** a normal, resizable window that remembers its size. Not
modal. Opens from the tray, from Ctrl+, in the stage, and from
`horadric settings`.

**The layout:** a left nav, a search box above it, and one scrolling
column of cards on the right. Each card:

```
┌──────────────────────────────────────────────────────────┐
│ Switch account when one runs out              [  On ●]   │
│ When a Claude account hits its limit, new turns go to     │
│ the next account instead of waiting.                      │
└──────────────────────────────────────────────────────────┘
```

Title in the normal weight, description one line in the muted colour,
control right aligned. A setting that can't be changed now (Permissions
for Codex, Tiles on screen with one monitor) stays and says why in its
description. Rare options sit in an expander.

**Themed names stay, with the plain meaning beside them.** Horadric's
names are its character and are not reopened here. But each one gets a
description a stranger can read: "Show Tristram: the town drawn behind
your tiles when no session is running". The section names in the nav are
plain words.

**The sections,** eight, in order of use:

1. **General:** Start with Windows, Open in Horadric in Explorer,
   Notify when a session waits, Loot sounds.
2. **Appearance:** Theme, Terminal font, Tiles on screen, Show Tristram,
   Animations.
3. **Sessions:** Agent, Model, Effort, Permissions, Workflow agents'
   model, Accounts (moved from the tray), Switch account when one runs
   out, Open as a chat (experimental), In the project menu.
4. **Projects:** the project picker on top, then Colour, Work mode, SSH
   hosts, Warriv drives, And ships public, and later secrets and
   identities.
5. **Keys:** the four hotkeys, then dictation when it lands.
6. **Machines:** signed in as, Can control this PC, Keeps my sessions,
   Take over by itself, Linux nodes. All of linking in one place.
7. **Memory:** Recall across projects, The tongue, Runetome's Ask before
   a click casts and its stones. The Scroll's numbers go into an
   expander called "How it is doing", and the hardcoded score goes.
8. **Backup:** as now.

Pinned at the bottom of the nav: **About**, with version, Check for
updates, Show on Discord (privacy), Level and XP, and links.

**Search** filters the cards across every section as you type, by title,
description and the plain meaning of themed names. Ctrl+F focuses it.

**Keyboard:** Tab and Shift+Tab move through the cards, Space flips a
switch, Enter or Alt+Down opens a list, Up and Down in the nav change
section. A focus ring the theme draws.

**Saving:** as now, every change applies at once. No Save button.

## The work, in steps

Each step lands on its own and is looked at by the human.

1. **The tray menu.** Regroup `tray::menu` into the shape above, move
   Quest log, Context and Stay a while to the project's menu, make left
   click show the stage, write the tooltip. Small, all in `tray.rs` and
   `app.rs`. The settings moved out of the menu wait for step 3.
2. **Descriptions.** A `description` on every `Field` in `settings.rs`,
   drawn under the label. The window grows tall enough for now. The
   biggest gain for the least code.
3. **Sections and the window.** The eight sections plus About, a
   resizable window, a scrolling content column, the expander. The
   Machines and Accounts rows move in from the tray. The Scroll's score
   goes.
4. **Keyboard focus.** Tab order, focus ring, Space and Enter.
5. **Search.** A text field in the nav, filtering cards.
6. **The DPI check** of the custom menu and Settings at 100%, 150%, 200%
   and across two monitors.

Not proposed: a settings file the UI writes to. Settings already persist
in the state file; an "open the file" link can come later if anyone asks.

## Sources

- Microsoft, notification area: <https://learn.microsoft.com/en-us/windows/win32/uxguide/winenv-notification>
- Microsoft, menus: <https://learn.microsoft.com/en-us/windows/win32/uxguide/cmd-menus>
- Microsoft, app settings: <https://learn.microsoft.com/en-us/windows/apps/design/app-settings/guidelines-for-app-settings>
- Microsoft, toggles: <https://learn.microsoft.com/en-us/windows/apps/develop/ui/controls/toggles>
- Apple, menu bar extras: <https://developer.apple.com/design/human-interface-guidelines/the-menu-bar>
- PowerToys runner: <https://microsoft.github.io/PowerToys/core/runner/>
- PowerToys 0.94 search: <https://winaero.com/powertoys-0-94-adds-search-to-settings-and-keyboard-shortcut-conflicts-detector>
- Docker Desktop: <https://docs.docker.com/desktop/use-desktop/>
- Tailscale menu policy: <https://tailscale.com/docs/install/windows/msi>
- Zed settings UI: <https://zed.dev/blog/settings-ui>
- VS Code settings: <https://code.visualstudio.com/docs/configure/settings>
- NN/g on toggles: <https://www.nngroup.com/articles/toggle-switch-guidelines/>
- Toptal on settings UX: <https://www.toptal.com/designers/ux/settings-ux>
