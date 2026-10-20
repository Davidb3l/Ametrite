# Changelog

Notable changes to Ametrite are recorded here, starting after v0.2.0. Earlier
releases (v0.1.0, v0.1.1, v0.2.0) are described on
[GitHub Releases](https://github.com/Davidb3l/Ametrite/releases).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project uses [Semantic Versioning](https://semver.org/) (pre-1.0: minor versions may
break things). Issue keys (`AMT-29`) refer to the project's own Ametrite board.

## [Unreleased]

## [0.5.0] - 2026-10-20

Hardening for fleets that drive `amt` from a foreman. `claim --issue` can no
longer reopen finished work, the Windows board launcher stops tripping over
verbatim paths, and the registry and schema migration are safe when several
`amt` processes start at once. `amt doctor`'s `ok` no longer flips for stale
claims or unresolved links.

### Changed

- `amt doctor --json`: top-level `ok` (and `report.ok`) is now true when every
  gating check passes. `stale_claims` and `unresolved_links` are reported but
  no longer flip `ok`; they are normal states of a busy fleet (a crashed
  agent's lease, a link to a note not written yet), and gates that trust `ok`
  used to flap on them. Every entry in `checks` gains a boolean `gating`
  (`false` for those two, `true` for the other six: `missing_parents`,
  `missing_projects`, `dangling_decisions`, `dependency_cycles`,
  `malformed_labels`, `closed_leases`). `schemaVersion` stays 1 (additive), and
  the exit code is unchanged. Plain `amt doctor` still lists every finding, and
  only says "workspace healthy" when there are none.
- `claim --ttl` (and the MCP `ttl_seconds`) must be at least 1 second. `--ttl 0`
  used to create a lease that was already expired.
- Release binaries are built with Rust 1.99.1 (was 1.99.0), and the lockfile
  moves to the latest compatible versions of every dependency (cc, libc, syn,
  memchr, and others). The bundled SQLite engine is unchanged. No behavior
  change.

### Fixed

- `claim --issue` (and the MCP `claim_issue`) reopened work. A done or
  in_review issue became in_progress again, a dependency-blocked issue could be
  claimed, and a heartbeat sent after the lease holder moved the issue to
  in_review (which ends the lease) put it back in progress with a fresh lease.
  Now it renews only a live lease you hold on an in_progress issue. A fresh
  claim of a named issue follows the rules `claim` applies to the queue: a
  claimable stage (or a stalled in_progress lease), no open blocker, no other
  agent's live lease, and outside your requeue cooldown. Anything else is
  refused with exit 0 and `{"claimed": false, "reason": "..."}`, the same shape
  as "no work", which Sirius Forester reads as "lease lost". Taking an issue
  another agent holds used to be an error (non-zero exit); it is now that
  refusal too. `claim --issue` no longer bypasses the cooldown; pass
  `--cooldown 0` to claim an issue you just released. The MCP `claim_issue` tool
  takes `cooldown_seconds` as well.
- Windows: `amt serve --install` wrote `cd /d "\\?\C:\..."` into the board
  launcher, which `cmd` rejects ("CMD does not support UNC paths as current
  directories"), so the board never started. `\\?\` prefixes are now stripped
  (`\\?\C:\x` becomes `C:\x`, `\\?\UNC\srv\share` becomes `\\srv\share`)
  before paths reach the launcher. `--install` also waits up to 10 seconds for
  the board to answer on its port and reports an error, with the log and
  launcher paths, if it does not. Re-run `amt serve --install` to pick up the
  fixed launcher.
- Windows: the workspace registry stored `\\?\` paths. New entries use the
  plain form; entries already in the file are read in the plain form too, so
  an existing registry keeps working and is cleaned up the next time it is
  written.
- Two `amt` processes opening an older workspace at the same moment could both
  run the same schema migration step, and the second failed with "table ...
  already exists". Each step now re-reads the schema version under the write
  lock and skips steps already applied.
- The workspace registry lost entries when several `amt init` or `amt ws add`
  commands ran at once (each read the old file, and the last write won), and a
  crash mid-write could leave a truncated file. Changes now hold a lock file
  (`registry.json.lock` next to the registry) and are written to a temporary
  file and renamed into place.
- `amt mcp` exited on the first stdin line that was not valid UTF-8. It now
  answers that line with a JSON-RPC parse error (-32700) and keeps serving.
- `amt serve` looks for Bun in more places. After PATH it tries
  `$BUN_INSTALL/bin` and `~/.bun/bin` (where Bun's installer puts it, often not
  yet on PATH), and when none has it, the error lists every location it
  searched.
- Opening an old (v1) `archive.db` from two `amt` processes at once could fail
  or rebuild the table twice. The archive upgrade now takes the write lock,
  re-checks the archive's version under it, skips a step another process
  already applied, and rolls back on error, as the main database's upgrade
  already did.
- `claim --issue` refusals for an issue that carries a lease while not
  in_progress (rows from before AMT-46) wrongly said it was in your requeue
  cooldown. The message now names the leftover lease, and says "not claimable
  right now" if no known cause applies. The refusal itself is unchanged.

## [0.4.2] - 2026-10-11

Windows fixes. On Windows, `amt serve` now finds Bun, the board service
keeps the port you installed it with, and `amt upgrade` recognizes how
`amt` was installed. If you installed the board service on Windows, run
`amt serve --install` again after upgrading, with `AMT_PORT` set as it was
when you first installed if you chose a port. Nothing changes on macOS or
Linux beyond `amt upgrade`'s detection.

### Fixed

- Windows: `amt serve --install` now bakes the board's port into the logon
  task, as it already did on macOS and Linux. The task used to take
  `AMT_PORT` from the logon environment instead, so a port set only in the
  installing shell was ignored. The task now runs a small launcher,
  `%LOCALAPPDATA%\ametrite\board.cmd`, which also keeps the task command
  under the 261-character limit of `schtasks /TR` for long user names.
  `--install` prints the launcher's path (the `unit` field in `--json`), and
  `--status` no longer shows a Linux unit path on Windows. Once a launcher
  exists, a reinstall stops the board on the port it records, and
  `--uninstall` and `--status` use that port; the first reinstall over an
  older task also stops a board left on the default port. A reinstall binds
  the current `AMT_PORT` (or 1776), as on macOS and Linux. Re-run
  `amt serve --install` to pick this up. (AMT-41)
- Windows: `amt serve` now finds Bun. It looked only for a file named
  `bun`, so it missed `bun.exe` and failed with "bun is not on PATH" unless
  you passed `--bun`. It now tries the `PATHEXT` extensions, as Windows
  does. The foreground board's PATH is no longer split on `:`, which broke
  `C:\` entries apart, and the installed board finds `amt` through `AMT_BIN`.
  (AMT-55)
- `amt upgrade` now picks the right installer in more cases, especially on
  Windows, where it could pick the wrong one:
  - It finds the installer's receipt wherever an installer writes it. On
    Windows it used to look only under `%LOCALAPPDATA%`, but the PowerShell
    installer uses `%XDG_CONFIG_HOME%` when that is set.
  - It recognizes a `cargo install` build on Windows. It used to need
    `CARGO_HOME` or `HOME` to be set, and even then compared the two paths
    in incompatible forms. On macOS and Linux, a `CARGO_HOME` reached
    through a symlink now matches too.
  - It treats a binary as a `cargo install` build only when cargo's own
    install record lists `amt`. An installer install without a receipt
    (`AMT_DISABLE_UPDATE`) in `~/.cargo/bin` used to be upgraded with
    `cargo install`, a source build; it now reruns the installer.
  - An empty `XDG_CONFIG_HOME`, `CARGO_HOME`, or `HOME` counts as unset, as
    it does for the installers. (AMT-54)

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

[Unreleased]: https://github.com/Davidb3l/Ametrite/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/Davidb3l/Ametrite/compare/v0.4.2...v0.5.0
[0.4.2]: https://github.com/Davidb3l/Ametrite/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/Davidb3l/Ametrite/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/Davidb3l/Ametrite/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/Davidb3l/Ametrite/compare/v0.2.0...v0.3.0
