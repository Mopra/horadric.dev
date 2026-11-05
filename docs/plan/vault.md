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

**The idea: the Vault holds, per identity, the tools' logins and named
secrets. A project names an identity, and its sessions start with that
identity's logins and can use its secrets without reading them.**
Horadric starts every session, so it decides each one's environment. The
human logs in once per identity and tool, and nobody switches again. Two
sessions of two identities run side by side and never see each other's
login. Warriv and errands go further without the human.

**The danger is the agent, not the disk.** Whatever text an agent sees it
can repeat: into a commit, a log, a web form, or to a page in the browser
pane whose text told it to. So the rule the secrets half is built on:
**an agent uses a secret without ever reading it.** It names the secret,
and Horadric hands the value to the one program that needs it. Logins
are different: a tool reads its own login files, and the Vault only
points it at the right ones.

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
  else clones the repo.
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
reads was checked on 2026-10-17.

- **GitHub CLI.** `GH_CONFIG_DIR=<identity>\gh`. Its `hosts.yml` names
  the active user, and the token stays in Windows Credential Manager,
  keyed by host and user, where `gh` puts it now. A `gh auth switch` run
  inside a session then only switches that identity.
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
  identity sets `XDG_DATA_HOME=<identity>\xdg-data`. That moves every
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
an identity does not change that in the first cut, since web logins are
the sites' own business. A profile per identity is the obvious later
step. Web logins never become secrets: an agent that needs a site it is
logged in to uses the pane, and never gets a password.

## Secrets: used, never read

- **Using one.** `horadric secret run GITHUB_TOKEN -- gh pr create ...`,
  several names joined by commas. The CLI asks the running app for the
  values, starts the program with them in its environment and nothing
  else's, passes its exit code back, and pipes its output through a mask
  that writes `[GITHUB_TOKEN]` wherever a value would have shown. The mask
  works across the chunks output arrives in, so a value split over two
  reads is still caught.
- **Asked through the app, with a key.** The values come over the local
  port, not out of the file, because the approvals, the log and which
  project a session belongs to live in the app. The port is open to
  anything on the machine, a web page included (a page may post to
  localhost), so each session gets a random `HORADRIC_SECRET_KEY` in its
  environment when it starts, and the app answers only a request carrying
  a live session's id and its key. A session's key dies with it.
- **Not in the session's environment.** Putting every secret into the
  agent's environment at start was the simple way, turned down: one
  `echo $GITHUB_TOKEN` and it is in the transcript. There is no `secret
  get` that prints a value either.
- **Asking first.** The first time a project's session uses a secret, its
  tile raises like a permission question: "Use GITHUB_TOKEN for `gh pr
  create`?" with Once, Always for `gh`, and No. Always is kept per secret
  and per program, the command's first word, so allowing `gh` does not
  allow `curl`. A shell or an interpreter (`sh`, `bash`, `cmd`,
  `powershell`, `pwsh`, `python`, `node`) is only ever offered Once,
  since it can do anything with the value, printing it in a form the mask
  does not know included.
- **Adding one.** In Settings, Projects, a Secrets row lists the picked
  project's names, its identity's and the Everywhere ones under them, and
  the names `config.json` asks for that are not kept yet. Add asks for the
  name and the value in a masked field (`field.rs` with the characters
  drawn as dots), pasting allowed. A value is never shown again: a kept
  secret has Replace, Forget, Allowed for (the programs, each with a
  cross) and when it was last used and by which session. From a terminal,
  `horadric secret set NAME [--everywhere]` reads the value without
  echoing it, for a value too long to paste into a field.
- **The log.** `secrets.log` beside it, a line a use: time, project,
  session, secret, program, and allowed, asked or refused. Never the value
  and never the whole command line, which may hold other things worth
  keeping out of a log.
- **The memory cuts them too.** The memory keeps transcripts for years
  and cuts what it knows Horadric keeps, whatever it looks like
  (`transcripts::set_secret_source`, fed by `kept_secrets` in the app:
  today the backup passphrase and the kept logins). The sealed file's
  values join that list when it is built, so a value an agent saw anyway
  is not kept.

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
plain value, not an encoded one. Logins are no safer than the tool keeps
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
name, the mask over chunked output, the program a command line names and
whether it is a shell, matching an approval, which of a project's, an
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
and a request with no key or a dead session's key is refused. Then
`horadric run` on haiku, told to open a pull request on a scratch
repository with a fine grained token kept for it, and a check that the
token is nowhere in its transcript.

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
7. The secrets prompt lines and `--on-secret`.
8. The later items, each when a project asks for it.
