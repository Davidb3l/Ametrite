# Changelog

Notable changes to Ametrite are recorded here, starting after v0.2.0. Earlier
releases (v0.1.0, v0.1.1, v0.2.0) are described on
[GitHub Releases](https://github.com/Davidb3l/Ametrite/releases).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project uses [Semantic Versioning](https://semver.org/) (pre-1.0: minor versions may
break things). Issue keys (`AMT-29`) refer to the project's own Ametrite board.

## [Unreleased]

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
- Community health files: CONTRIBUTING (with a DCO sign-off requirement, checked
  in CI), CODE_OF_CONDUCT, SECURITY, SUPPORT, and issue and pull request
  templates. The README gains a header with badges and links. (AMT-32)

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

[Unreleased]: https://github.com/Davidb3l/Ametrite/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/Davidb3l/Ametrite/compare/v0.2.0...v0.3.0
