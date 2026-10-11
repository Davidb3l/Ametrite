# Changelog

Notable changes to Ametrite are recorded here, starting after v0.2.0. Earlier
releases (v0.1.0, v0.1.1, v0.2.0) are described on
[GitHub Releases](https://github.com/Davidb3l/Ametrite/releases).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project uses [Semantic Versioning](https://semver.org/) (pre-1.0: minor versions may
break things). Issue keys (`AMT-29`) refer to the project's own Ametrite board.

## [Unreleased]

### Fixed

- Windows: `amt serve --install` now bakes the board's port into the logon
  task, as it already did on macOS and Linux. The task used to take
  `AMT_PORT` from the logon environment instead, so a port set only in the
  installing shell was ignored. The task now runs a small launcher,
  `%LOCALAPPDATA%\ametrite\board.cmd`, which also keeps the task command
  under the 261-character limit of `schtasks /TR` for long user names.
  `--install` prints the launcher's path (the `unit` field in `--json`), and
  `--status` no longer shows a Linux unit path on Windows. Once a launcher
  exists, reinstalling, `--uninstall`, and `--status` use the port it
  records; the first reinstall over an older task also stops a board left on
  the default port. Re-run `amt serve --install` to pick this up. (AMT-41)
- Windows: `amt serve` now finds Bun. It looked only for a file named
  `bun`, so it missed `bun.exe` and failed with "bun is not on PATH" unless
  you passed `--bun`. It now tries the `PATHEXT` extensions, as Windows
  does. The foreground board's PATH is no longer split on `:`, which broke
  every `C:\` entry, and the installed board finds `amt` through `AMT_BIN`.
  (AMT-55)

## [0.4.1] - 2026-10-11

A maintenance release: no behavior changes. It updates the bundled SQLite
engine and the toolchain that builds the release. Existing workspaces need no
migration, and the web board is unchanged, so a running board needs no restart.

### Changed

- The bundled SQLite engine moves from 3.46.0 to 3.53.2 (rusqlite 0.40).
  A database written by this version still opens in v0.4.0 and in the web
  board.
  (AMT-52)
- Release binaries are built with Rust 1.99.0 and cargo-dist 0.33.0, with
  updates to clap, regex, serde, and serde_json. CI now pins Bun 1.4.3
  instead of floating to the newest release. (AMT-52)

## [0.4.0] - 2026-10-11

**Upgrade notes.**

- **Behavior change for agents:** while another agent holds a live lease
  on an issue, only that agent can change the issue's status. Others are
  refused (operators can use `amt issue update --force`). Leaving
  in_progress now ends the lease. On the web board, dragging a card another
  agent is actively working on is refused with a message; the board has no
  override, so use `amt issue update --force` if you mean it.
- **Run `amt doctor --fix` once in each workspace.** It splits labels that
  earlier versions stored comma-joined (e.g. `bug,ui`, which `--label bug`
  couldn't find) and ends leases that closed issues still hold. `amt doctor`
  lists both first if you want to look before fixing. Until you do,
  `amt doctor --json` reports `ok: false` for a workspace that has either
  (the exit code stays 0), which tools gating on doctor will notice.
- **If you run the web board,** restart it to get the new search and label
  filter: `amt serve --install` (service) or restart `amt serve`.

### Added

- Community health files: CONTRIBUTING (with a DCO sign-off requirement, checked
  in CI), CODE_OF_CONDUCT, SECURITY, SUPPORT, and issue and pull request
  templates. The README gains a header with badges and links. (AMT-32)
- Web board: a row of label chips filters the board in one click, with `bug`
  first and each chip showing its count; click again to clear. Bug labels on
  cards are tinted so bugs stand out even unfiltered. The label filter is kept
  when you pick a project. (AMT-44)
- `amt claim` and `amt issue list` take `--label` more than once (or
  comma-separated); an issue must carry all of them. `--not-label` excludes,
  e.g. `amt claim --not-label question,owner` skips owner questions. The MCP
  `list_issues` and `claim_next_issue` tools take the same as `label` and
  `not_label`, each a comma-separated string or an array. (AMT-48)

### Changed

- Only the holder of a live lease can change its issue's status; others are
  refused, and an operator can override with `amt issue update --force`,
  which is logged. (AMT-46)
- An empty label filter (`--label ""`) now means no filter, where it used to
  match nothing. (AMT-48)
- `--agent` is accepted by every command except `amt serve` (which refuses
  it, since the board records each request's author), before or after it, with
  `--author` as an alias, so every write records who did it. `issue
  create/update`, `note create/append` and `dep add/rm` used to accept
  neither and silently recorded `$USER`. `amt mcp --agent X` makes `X` the
  author of tool calls that don't pass their own `agent`. An empty
  `--agent`, `AMT_AGENT` or MCP `agent` is treated as unset instead of being
  recorded as an empty author, and a blank comment is refused. (AMT-47,
  AMT-36)

### Fixed

- Labels written comma-joined (`--label "bug,ui"`, or the same through MCP or
  the board) are split into separate labels, so `--label bug` and the board's
  bug filter find them. `amt doctor` reports labels stored that way by earlier
  versions, and `amt doctor --fix` splits them, logging each repair on the
  issue, note or project. Until `--fix` runs, a workspace with such labels
  reports `ok: false` in `amt doctor --json`. (AMT-45)
- Changing an issue's status ends its lease when it leaves in_progress, so
  done issues no longer stay claimed (including cards dragged on the board).
  `amt doctor` reports closed issues still holding a lease, and `--fix` ends
  those leases. In `amt doctor --json` these are the `malformed_labels` and
  `closed_leases` checks, and `--fix` adds a `fixed` object listing what it
  changed. (AMT-45, AMT-46)
- `amt stats` and `amt agents` count forced completions (`release --force`
  to done, and the new forced status change). They used to miss any
  completion logged as forced, including ones already in your history,
  which now count. (AMT-46)
- Searching for a document's key (`MSX-269`, `D-142`, a note slug) finds that
  document, listed first. Keys weren't in the full-text index, so search only
  found other documents that mention the key. Applies to `amt search`, the MCP
  `search` tool, and the web board's search and ⌘K palette. (AMT-43)

## [0.3.0] - 2026-10-09

**Security release.** If you run the web board (`amt serve`), upgrade. In v0.2.0
and earlier, other machines on your network, and web pages open in your browser,
could read and change your workspaces through it (see Security below).

Upgrading the binary does not restart a board that is already running. After
`amt upgrade`, run `amt serve --install` (if you installed the board service) or
restart `amt serve`. Then check the running board (on Windows PowerShell, type
`curl.exe`, not `curl`; use your port instead of 1776 if you set `AMT_PORT`):

```sh
curl -s -o /dev/null -w "%{http_code}\n" -H "Host: example.com" http://127.0.0.1:1776/api/workspaces
curl -s -o /dev/null -w "%{http_code}\n" "http://[::1]:1776/api/workspaces"
```

You are protected when the first prints `403` and the second prints `000`
(nothing answers over IPv6). A `200` from either means an older board is still
running: stop every `amt serve` / board process and start it again (on Windows,
signing out and back in also works once you have run `amt serve --install`).
If the first also prints `000`, no board answered on that port: it isn't
running, or it uses another port.

### Added

- `amt brief`: a read-only orientation bundle for the start of an agent session.
  It shows the issues you hold (with lease expiry), what else is in flight, recent
  activity and decisions, the latest handoff note in full, and what `claim` would
  serve you next. `--since` takes an ISO-8601 instant or a duration such as `24h`
  (default 72h). `--budget` is a target size in bytes of the JSON actually
  delivered: it drops whole low-value sections, naming each cut in `dropped`, but
  never your own work or the handoff note, so a budget below that floor is still
  exceeded. Also exposed as the `brief` MCP tool. (AMT-29; 7ad9186, 9d26ad4, dfea18c)
- Handoff notes: AGENTS.md and the Claude Code skill teach a session-end note
  tagged `handoff`, which the next session reads through `amt brief`. Only an
  explicit `--tag handoff` counts; a `#handoff` mention in a body does not.
  (AMT-30; 9d26ad4, dfea18c)
- `amt note list --tag <tag>` filters notes by tag, including non-ASCII tags.
  (AMT-30; 9d26ad4, dfea18c)
- `list_notes` MCP tool, so MCP-only agents can browse past handoffs. (AMT-30; dfea18c)
- The `list_issues` MCP tool takes a `claimed_by` parameter: only the issues one
  agent currently holds. (AMT-29; 7ad9186)
- `amt comment <KEY> -m …`: a top-level alias of `amt issue comment`, with the
  same flags and the same `--json` output. (AMT-31; #1, 1efd864)
- This changelog. (AMT-37)

### Security

- Web board: `amt serve` now listens on 127.0.0.1 only, refuses API requests
  whose `Host` isn't the board itself (DNS rebinding), and refuses writes from
  another origin. Before this, any machine on your network, and any web page
  open in your browser, could read and change every registered workspace.
  If you reach the board through an SSH port forward or a proxy, keep its port
  number (for example `ssh -L 1776:127.0.0.1:1776`); a different port no longer
  matches the board's `Host`. (AMT-34; #4, 5c39a17, 3b7f15e)

### Fixed

- `amt serve --install` restarts a board that is already running on Linux and
  Windows too (it already did on macOS), so the documented upgrade step
  (`amt upgrade`, then `amt serve --install`) actually serves the new version.
  On Windows, install and `amt serve --uninstall` also stop the board's
  `bun.exe`, which ending the scheduled task alone could leave running on the
  port, including an older board listening on IPv6. (AMT-40, AMT-42; #5, #7)
- Web board: a live update no longer destroys a comment or new-issue draft you
  are typing. Drafts are kept in localStorage until they post, and focus and
  caret survive the re-render. (AMT-28; f1ef40b)
- Web board: live updates no longer stall while an agent writes continuously,
  and a change to the workspace you are viewing is no longer missed when another
  workspace changes right after it.
  (AMT-28; f1ef40b)
- Web board: a workspace re-created while the board is running no longer fails
  every request until a restart. The stale connection is reopened and the read
  retried once. (AMT-27; f1ef40b)

[Unreleased]: https://github.com/Davidb3l/Ametrite/compare/v0.4.1...HEAD
[0.4.1]: https://github.com/Davidb3l/Ametrite/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/Davidb3l/Ametrite/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/Davidb3l/Ametrite/compare/v0.2.0...v0.3.0
