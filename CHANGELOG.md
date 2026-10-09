# Changelog

Notable changes to Ametrite are recorded here, starting after v0.2.0. Earlier
releases (v0.1.0, v0.1.1, v0.2.0) are described on
[GitHub Releases](https://github.com/Davidb3l/Ametrite/releases).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project uses [Semantic Versioning](https://semver.org/) (pre-1.0: minor versions may
break things). Issue keys (`AMT-29`) refer to the project's own Ametrite board.

## [Unreleased]

Everything below is on `main` but not yet in a release; v0.2.0 predates all of it.

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
  matches the board's `Host`. (AMT-34)

### Fixed

- `amt serve --install` restarts a board that is already running on Linux and
  Windows too (it already did on macOS), so the documented upgrade step
  (`amt upgrade`, then `amt serve --install`) actually serves the new version.
  On Windows, install and `amt serve --uninstall` also stop the board's
  `bun.exe`, which ending the scheduled task alone could leave running on the
  port. (AMT-40)
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
