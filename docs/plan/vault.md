# Vault

Two asks, decided on 2026-11-05 to be one part. On 2026-10-11:
projects that run on their own stop at authentication. A quest that has
to push to GitHub, call an API or deploy needs a token, and today it
either sits in a plain `.env` file, gets pasted into the conversation, or
the quest blocks until the human comes. On 2026-10-17: the human's
projects belong to three worlds, Optipeople, personal and Pradsgaard
Labs, each with its own GitHub, Vercel and npm accounts. The command line
tools keep one "current account" each, for the whole machine, so a
session that runs `gh auth switch` for its project switches every other
session too, and a quest stops on a push that went to the wrong account
or a deploy that landed in the wrong team. The daily case: `gh` must
stay on Mopra for horadric.dev while MP-OPTI is the work account. Logging
in is not the problem. Switching is.

**The idea: the Vault holds, per identity, the tools' logins, named
secrets and the websites' logins. A project names an identity, and its
sessions start with that identity's logins, can use its secrets without
reading them, and can have Horadric sign in to a site in the browser
pane without seeing the password.** Identities are the human's own, made
in Settings and kept in this install; none come built in, and someone
with one account per tool never makes one.
Horadric starts every session, so it decides each one's environment. The
human logs in once per identity and tool, and nobody switches again. Two
sessions of two identities run side by side and never see each other's
login. Warriv and errands go further without the human.

**The danger is the agent, not the disk.** Whatever text an agent sees it
can repeat: into a commit, a log, a web form, or to a page in the browser
pane whose text told it to. So the rule the secrets half is built on:
**an agent uses a secret without ever reading it.** It names the secret,
and Horadric hands the value to the one program that needs it, or types
it into the one site it belongs to. Tool logins are different: a tool
reads its own login files, and the Vault only points it at the right
ones.

## What is kept, and where

- **An identity.** A name ("Optipeople"), a short slug for its folder,
  and the tools it has set up. Kept in `identities.json` beside
  `state.json`, the tools' config under `identities\<slug>\` in the app
  data.
- **Secrets.** `secrets.dat` beside `accounts.dat`, sealed with DPAPI for
  this Windows user by the same `seal` and `unseal` as kept logins. Not
  Windows Credential Manager: any tool can list its entries, and a blob
  stops at 2560 bytes, which a service account's JSON key passes.
- **A dev instance** keeps its own identities and its own secrets file in
  its own app data and never reads the installed one's, as with accounts.

## Whose is what

- **A project names one identity, or none.** None is today's behaviour:
  the session gets the machine's global logins, so nothing changes for a
  project until the human picks. The pick is in Settings, Projects, a row
  of its own, and may also be written `"identity": "optipeople"` in
  `.horadric/config.json`, by slug, so a fresh checkout says whose it is.
  The setting wins over the file, since the file is shared with whoever
  else clones the repo. A slug this install has no identity for (a
  colleague's clone) means none, never an error, and the Projects row
  offers to make an identity by that slug.
- **By folder.** An identity may claim folders, `Documents\Github\opti\`,
  so a new project under one is that identity's from its first session.
  The longest claim wins. A project's own pick beats any claim.
- **A town** names an identity like a project does (see
  [towns](towns.md)), so `gh` and git follow it too.
- **A secret belongs to one project** (by its folder), **to an identity**,
  or **to Everywhere** (for a token several share). Its name is an
  environment variable's name, `GITHUB_TOKEN`, upper case, digits and `_`.
  A project's own wins over its identity's, which wins over Everywhere. So
  Pradsgaard Labs' `CLOUDFLARE_API_TOKEN` serves every Pradsgaard project
  without being added to each.
- **Names may be in the repository, values never.** `"secrets":
  ["GITHUB_TOKEN", "CLOUDFLARE_API_TOKEN"]` in `.horadric/config.json`
  says what the project needs. It is safe to commit, it lets a fresh
  checkout on another machine say what is missing, and it is what the
  settings list offers to fill.
- **A session's identity is fixed at its start.** A session host keeps
  the environment it was born with, so changing a project's identity
  applies to its next sessions. The Projects row says how many running
  sessions still have the old one, with Restart them.

## Logins: the tools, one at a time

An identity is a folder of the tools' own config (their login files
where they keep them), and a session's environment points each tool at
its identity's folder. Each tool has its own way to be pointed at a
folder, and an identity only sets what it has a login for. What each
reads was checked on 2026-10-17. A session with an identity also has
`GH_TOKEN`, `GITHUB_TOKEN`, `GH_ENTERPRISE_TOKEN`, `NPM_TOKEN` and
`VERCEL_TOKEN` removed from the environment it inherits, since any of
them set for the whole machine beats the identity's login without a
word.

- **GitHub CLI.** `GH_CONFIG_DIR=<identity>\gh`. Its `hosts.yml` names
  the active user, and the token stays in Windows Credential Manager,
  keyed by host and user, where `gh` puts it now. A `gh auth switch` run
  inside a session then only switches that identity. Checked in `gh`'s
  source on 2026-11-06: it reads the active user's token by user name
  first, and the blank user entry every login and switch overwrites is
  only its fallback.
- **git.** The name, the email and which account the credential helper
  uses, set through `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_<n>` and
  `GIT_CONFIG_VALUE_<n>` (git 2.31 and later): `user.name`,
  `user.email`, and `credential.https://github.com.username`, which tells
  Git Credential Manager which of its kept GitHub accounts to answer
  with. Nothing is written into any `.gitconfig`, global or the
  repository's, so the human's own terminals outside Horadric are as
  they were. The commit author is the part that matters most: a commit
  on an Optipeople repository should never carry the personal email.
- **npm.** `NPM_CONFIG_USERCONFIG=<identity>\npm\.npmrc`, its registry
  tokens inside, plain text as npm keeps them anywhere. A project's own
  `.npmrc` still applies on top, as npm means it to.
- **Vercel.** The CLI has no variable for its folder. It looks for
  `auth.json` in its XDG data folder, `--global-config` aside, so the
  identity sets `XDG_DATA_HOME=<identity>\xdg-data`. An old `~\.vercel`
  or `~\.now` folder may still win over it on some versions, so the
  verify step checks `vercel whoami` per identity before this is
  trusted. That moves every
  XDG minded tool's data in that session too, which is why it is set only
  for an identity with a Vercel login. The other way, a `VERCEL_TOKEN`
  kept as the identity's secret, works for every command that takes a
  token and is offered beside it. Which team a project deploys to is
  already in its `.vercel\project.json`; the identity only says who is
  logged in.
- **Claude Code is not one of them.** Accounts switches the Claude login
  for every session on purpose: a usage limit is spent per subscription,
  not per project, and `CLAUDE_CONFIG_DIR` per account was turned down
  there (reopened for towns only, see [towns](towns.md)). An identity may
  later say which account it prefers, never force one.
- **More tools** go in as a project asks for them, each one line of what
  variable points it where: `AZURE_CONFIG_DIR` for the Azure CLI,
  `CLOUDSDK_CONFIG` for gcloud, `AWS_PROFILE` for AWS. A tool that takes
  a token, like wrangler's `CLOUDFLARE_API_TOKEN`, is a secret instead. A
  tool with no variable is left global, said so in the row, never hacked
  by copying files around.

**Logging in.** Each identity's row in Settings lists its tools with who
is logged in to each. Log in opens the tool's own login in a console of
its own, with the identity's environment: `gh auth login`, `npm login`,
`vercel login`, as Add account does for Claude. For git there is no
login, only the name, the email and the GitHub user to pick from the
ones Git Credential Manager keeps. Who is logged in comes from the files
where a tool keeps it (`hosts.yml`, the `.npmrc`'s registries, Vercel's
`auth.json` holds no name so `vercel whoami` is run on a click), never
from asking a tool every paint.

**Plain terminals and the browser pane.** A plain terminal in the stage
gets its project's identity too, so the human's own `git push` there
goes as the agent's would. The browser pane keeps its own login profile;
an identity does not change that in the first cut. A profile per
identity is the obvious later step. Signing in to a site is the Vault's
third half, below.

## Secrets: used, never read

- **Using one.** `horadric secret run GITHUB_TOKEN -- gh pr create ...`,
  several names joined by commas. The CLI asks the running app for the
  values, starts the program with them in its environment and nothing
  else's, passes its exit code back, and pipes its output through a mask
  that writes `[GITHUB_TOKEN]` wherever a value would have shown. The mask
  works across the chunks output arrives in, so a value split over two
  reads is still caught. `--stdin NAME` also writes that value to the
  program's input, for a tool that asks for a password at a prompt
  rather than reading a variable.
- **The app starts the program, the CLI only relays.** The values never
  leave the app: `secret run` sends the command line, its folder and its
  input over the local port, the app starts the program with the values
  in its environment, and streams the masked output and the exit code
  back. A design where the app hands the CLI the values was turned down:
  the agent holds whatever the CLI holds, and could ask the port itself
  with `curl` and read them plain. The approvals, the log and which
  project a session belongs to live in the app too.
- **A key per session.** The port is open to anything on the machine, a
  web page included (a page may post to localhost), so each session gets
  a random `HORADRIC_SECRET_KEY` in its environment when it starts, and
  the app runs a program only for a request carrying a live session's id
  and its key. A session's key dies with it. The agent can read its own
  key; that buys it nothing it could not do through `secret run`.
- **The program is a full path.** The app finds the program on the
  session's `PATH` itself, never in the current folder (Windows looks
  there first, and an agent can drop a `gh.cmd` into the repository),
  and an approval is kept for that full path. A program that moved is
  asked about again.
- **Not in the session's environment.** Putting every secret into the
  agent's environment at start was the simple way, turned down: one
  `echo $GITHUB_TOKEN` and it is in the transcript. There is no `secret
  get` that prints a value either.
- **Asking first.** The first time a project's session uses a secret, its
  tile raises like a permission question: "Use STRIPE_KEY for `stripe
  listen`?" with Once, Always for `stripe listen`, and No. Always is kept
  per secret, per program, per subcommand and per `--stdin` names, and
  covers a run only while that run could be allowed Always now. Always
  is offered only for a short list of single-purpose programs Horadric
  knows (`stripe`, `aws`, `az`, `gcloud`), each with its pins and refused
  arguments; everything else is Once only, `curl` included, since a list
  of what may not would never be complete (an alias, a plugin folder, a
  project's deploy hook). Anything that runs git (`gh`, `vercel`,
  `netlify`, `wrangler`) says so, and unattended work with gh or vercel
  uses the identity's own login (`horadric quest blocked --on-login
  gh`), not a Vault secret. `docker`, `kubectl`, `helm` and every npm
  shim, judged by its package rather than its file name, are Once only.
  The listed programs pin their own config, aliases, extensions and
  plugins by SHA-256 (a config that is not readable UTF-8, or a plugin
  folder that is not empty, leaves only Once), with the proxy, CA and
  home variables they inherit; a change asks again, naming what changed.
  No subcommand, a flag before it, or arguments that reach past it
  (another API host, a file of arguments, a subcommand that installs
  code or logs in) leave only Once. An Always run gets a PATH without
  folders the user can write to, and its pinned files are held so none
  can be written until it ends. Settings marks an Always for a program
  no longer on the list as no longer used (decided 2026-11-06).
- **Adding one.** In Settings, Projects, a Secrets row lists the picked
  project's names, its identity's and the Everywhere ones under them, and
  the names `config.json` asks for that are not kept yet. Add asks for the
  name and the value in a masked field (`field.rs` with the characters
  drawn as dots), pasting allowed. A value is never shown again: a kept
  secret has Replace, Forget, Allowed for (the programs, each with a
  cross) and when it was last used and by which session. From a terminal,
  `horadric secret set NAME [--everywhere]` reads the value without
  echoing it, for a value too long to paste into a field. That holds in
  a session's own terminal, whose key names the session in the question.
  A terminal with no session key proves nothing about who runs it, so
  there the app takes no value: the human types it into Horadric's own
  masked field, and the question says the asker may be an agent
  (decided 2026-11-06).
- **The log.** `secrets.log` beside it, a line a use: time, project,
  session, secret, program, and allowed, asked or refused. Never the value
  and never the whole command line, which may hold other things worth
  keeping out of a log.
- **The memory cuts them too.** The memory keeps transcripts for years
  and cuts what it knows Horadric keeps, whatever it looks like
  (`transcripts::set_secret_source`, fed by `kept_secrets` in the app:
  today the backup passphrase and the kept logins). The sealed file's
  values, web passwords included, join that list when it is built, so a
  value an agent saw anyway is not kept.

## Web logins: filled, never read

Decided on 2026-11-06, in the first cut and not later: an agent can sign
in to a site in the browser pane with a password it never sees, as a
password manager fills one. Without it a quest on a site the pane is not
logged in to stops until the human comes, and a pasted password ends up
in a transcript.

- **What is kept.** A web login is a name (`OPTICLOUD`), the site it
  belongs to, a user name and a password, in the same sealed
  `secrets.dat` and owned the same way: by a project, an identity or
  Everywhere. The site is one or more origins, `https://app.opticloud.dk`,
  scheme and host exactly, no wildcards; plain `http` only for
  `localhost`. The user name is not secret and the agent may see it.
- **Filling.** A pane tool, `browser_fill_login`, takes the login's name
  and the refs of the user name and password fields from a
  `browser_snapshot`. Horadric checks the tab's top level origin is one
  of the login's, that the password ref is a password field in the top
  document or a frame of the same origin, and types the values in as
  key input, so the site's own scripts see what they would from a
  person. The agent gets back filled or why not, never the value. It
  then clicks the sign in button itself.
- **The origin is the guard.** A page's text can tell the agent to fill
  the Opticloud login on `opticloud-login.example`; the origin check
  refuses, whatever the agent was told. This is the reason the fill is
  Horadric's and not the agent's. A redirect between the check and the
  typing is caught by checking the origin again before each field.
- **The pane hides what it filled.** After a fill, until that document
  is left, `browser_evaluate`, `browser_console` and `browser_devtools`
  are refused on the tab, since a script could read the field's value
  and encode it past the mask. `browser_snapshot` never reports a
  password field's value at all, and any other text it returns passes
  the same mask as `secret run`.
- **Asking first.** The first fill of a login from a project's sessions
  raises the tile like `secret run` does: "Sign in to app.opticloud.dk
  as mpg@...?" with Once, Always and No.
- **One time codes.** A login may also keep a TOTP seed, filled by
  `browser_fill_login` into a code field the same way. Off unless the
  human adds one, and the row says what it costs: the second factor then
  lives on this machine beside the first. Without a seed, a site asking
  for a code blocks the quest on the human as today (`quest blocked`
  with `--you`), saying which site wants one.
- **Adding one.** The Secrets row lists web logins beside secrets, Add
  asks for the site, the user name and the password in the masked field.
  The pane may later offer Keep this login when the human signs in by
  hand.
- **Missing.** A fill with no login kept for the tab's origin fails with
  the line the agent needs, and `--on-secret OPTICLOUD` blocks a quest
  on a web login as on a secret.

## When something is missing

Neither half waits on a tile nobody watches. Both block the quest with a
new kind of wait a blocked quest can name, so Warriv and armed errands
move on and the quest goes on by itself once the human fixes it.

- **A secret not yet allowed** for that program fails `secret run` at
  once and says why. A secret already allowed just works.
- **A missing secret** fails the same way, with the line the agent needs:
  "GITHUB_TOKEN is not kept for this project. Ask the human to add it in
  Project settings, or block the quest with --on-secret GITHUB_TOKEN."
- **`horadric quest blocked --on-secret GITHUB_TOKEN`** holds until the
  secret is kept and allowed. The row reads "needs GITHUB_TOKEN" and a
  click opens Project settings with the name filled in.
- **`horadric quest blocked --on-login gh`**, for a tool that says it is
  not logged in, holds until that identity has a `gh` login. The row reads
  "needs gh login for Optipeople" and a click opens the identity's row in
  Settings.

**Telling the agent.** A session gets a few lines joined into its
`--append-system-prompt`. With an identity: whose project this is, who it
is logged in as for each tool, never to run a tool's login, logout or
switch, and the `--on-login` wait. With secrets kept or asked for: the
names (never the values), the `secret run` form, never to print, write
or pass a secret on in any other way, and the `--on-secret` wait.

## What it does not protect against

DPAPI opens for any program running as this user, so malware already on
the machine can read `secrets.dat` just as it can read
`.credentials.json`. An agent that means to leak a secret can, through a
program it was allowed for that sends it somewhere. The mask catches the
plain value, not an encoded one. A web password goes to the site it is
kept for and to nothing else, but that site's own scripts read it, as
they would from the human; a compromised site is not the Vault's to
catch. Logins are no safer than the tool keeps
them: `gh`'s token is in Credential Manager either way and npm's in a
plain file either way, just a different file, and an agent can still
read what its identity's folder holds, as it can read the global one
today.

What it does buy: the login an agent finds is the right one, and one
project's switch never stops another's quest. No secrets in repositories
or `.env` files, none in transcripts by accident, one place to see and
revoke them, a log of every use, and a question before any new program
gets one. The advice it gives the human in the Secrets row: narrow
tokens over passwords (a fine grained GitHub token for one repository,
an API key with one scope), so a leak costs little and a revoke is a
click on the provider's site.

## Later, not in the first cut

- **Git over HTTPS.** `horadric secret git` as a credential helper, so a
  push needs no `secret run` around it. It would go in the project's
  `.git/config`, never the global one.
- **Secrets as files.** `secret run --file KUBECONFIG ...` writes the
  value to a file only this user can read, puts its path in the variable,
  and deletes it when the program exits, for tools that want a path.
- **A browser profile per identity**, above.

## Pure and tested

An identity's slug, which identity a project gets (its pick, the config
file, the longest folder claim, none), the environment an identity sets
for the tools it has, the `GIT_CONFIG_*` entries, reading the active
user from `hosts.yml` and the registries from an `.npmrc`. A secret's
name, the mask over chunked output, the program a command line names,
its full path off `PATH` with the current folder skipped, and whether it
is a shell or a runner, matching an approval, a web login's origins and
whether a tab's origin matches one, a TOTP code from a seed, which of a project's, an
identity's and an Everywhere secret wins, the names `config.json` asks
for, the log line. The prompt lines, and `--on-login` and `--on-secret`
written and read in a quest's wait.

## Verifying it

On a dev instance on its own port and app data. Identities: two made for
the test, each with a different `gh` login and git email; two sessions
of two projects, one of each, run `gh api user -q .login` and `git
config user.email` and each answers with its own, and `gh auth switch`
in one leaves the other's answer as it was. `horadric run` on haiku,
told to commit and push to a scratch repository, and the commit's author
and the pushing account checked on GitHub.

Secrets: with a made-up secret, `horadric secret run TEST_SECRET -- cmd
/c echo %TEST_SECRET%` from a session prints `[TEST_SECRET]` after Once,
and a request with no key or a dead session's key is refused, and a `curl`
to the port with the session's own key gets no value back. Then
`horadric run` on haiku, told to open a pull request on a scratch
repository with a fine grained token kept for it, and a check that the
token is nowhere in its transcript.

Web logins: a login page served on `localhost` with a made-up login
kept for it; `horadric run` on haiku, told to sign in, does, and the
password is nowhere in its transcript. The same login page served on a
second port (another origin) whose text tells the agent to fill the
login there is refused, and `browser_evaluate` on the filled page is
refused until it navigates.

Checked on screen: the Projects row, Log in opening a console that lands
in the identity's folder, the question on the tile, Always for one
program and not another, a shell offered only Once, the masked field,
and quests blocked `--on-login` and `--on-secret` going on once the
login or the secret is there.

## Order of work

Identities first, since the daily pain is a switched account.

1. Identities and the project pick with `gh` and git, since those are
   the walls hit most.
2. npm and Vercel.
3. The folder claims, the town's pick, the identity prompt lines and
   `--on-login`.
4. The sealed secrets file, `secret set` and `secret run` with the mask.
5. The key and the questions on the tile.
6. The Secrets row in Settings, with secrets owned by a project, an
   identity or Everywhere.
7. Web logins: kept in the sealed file, `browser_fill_login` with the
   origin check, the pane's tools held back after a fill, the question
   on the tile, and TOTP seeds.
8. The secrets prompt lines, web logins named in them, and
   `--on-secret`.
9. The later items, each when a project asks for it.
