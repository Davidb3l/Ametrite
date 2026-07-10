# Ametrite

**A local-first issue tracker and wikilinked knowledge base for AI agent workflows** —
think Linear + Obsidian in one SQLite file, driven by a single Rust binary (`amt`) that is
both a CLI and an MCP server. No cloud, no accounts, no embeddings.

## Why

Coding agents forget everything between sessions, and two agents working the same repo have
no shared state: no backlog, no "who's doing what", no record of decisions already made.
Ametrite gives every repo a shared workspace both humans and agents can read and write:

- **Issues with race-free claims.** `amt claim` atomically leases the highest-priority open
  issue to one agent (`BEGIN IMMEDIATE`); two agents can never claim the same issue. Crashed
  agent? The lease expires and the issue becomes claimable again.
- **Knowledge that links.** Issue and note bodies are markdown with `[[wikilinks]]` and
  `#tags`, indexed into a bidirectional link graph with backlinks.
- **Decisions are first-class.** `amt decide` records an ADR-style decision (`D-1`) against
  the issue it resolves — searchable, linkable, supersedable. The "why" is one backlink hop
  from the "what".
- **Search without embeddings.** SQLite FTS5 (BM25) + tags + link graph + structured filters.
- **Everything is scriptable.** Every command takes `--json`; `amt events --follow` streams
  activity as NDJSON.

## Quickstart

Requires a Rust toolchain (CI builds on stable, Linux/macOS/Windows).

```sh
git clone https://github.com/Davidb3l/Ametrite && cd Ametrite
cargo install --path crates/amt        # installs `amt`

cd ~/code/your-project
amt init --name my-project --prefix AMT   # creates .ametrite/ (git-ignores itself)

amt issue create --title "Fix login token refresh" --priority urgent --label bug
amt issue list
amt note create --title "Session Tokens" -b "Rotation affects [[AMT-1]]. #auth"
amt search token
amt backlinks AMT-1
```

Representative output:

```
$ amt issue list
AMT-1    backlog      urgent  Fix login token refresh  [bug]
AMT-2    backlog      medium  Write session-token docs

$ amt claim --agent worker-1
claimed AMT-1    in_progress  urgent  Fix login token refresh  [bug]  🔒worker-1

$ amt release AMT-1 --agent worker-1 --status in_review -m "rotated refresh path"
released AMT-1    in_review    urgent  Fix login token refresh  [bug]

$ amt doctor
workspace healthy ✓
```

### Web UI (optional, requires [Bun](https://bun.sh))

```sh
bun run web          # → http://localhost:1776 (AMT_PORT to change)
```

One kanban board across every registered workspace (`amt ws list`), live-updating via SSE
whenever any process writes — CLI, MCP agent, or another browser tab. The web app has zero
npm runtime dependencies and never writes the database directly: reads are direct SQLite,
mutations shell out to `amt --json`, so business logic lives in exactly one place.

## Using it with AI agents

**MCP** — `amt mcp` is a stdio MCP server exposing 23 tools (issues, claims, notes,
decisions, dependencies, search, context bundles, stats, events, git commits):

```sh
claude mcp add ametrite -- amt mcp
```

**Claude Code skill** — this repo ships `.claude/skills/ametrite/`, picked up automatically
inside the repo (copy to `~/.claude/skills/` for global use). It teaches agents the claim
loop, decision recording, and wikilink etiquette. [AGENTS.md](AGENTS.md) documents the same
conventions for any agent.

**CLI loops** — `--json` everywhere. Note that "nothing claimable" returns
`{"claimed": false, ...}` with exit code 0, so test the payload:

```sh
while id=$(amt --json claim --agent worker-1 | jq -r '.id // empty'); [ -n "$id" ]; do
  amt context "$id"        # issue + activity + decisions + backlinked docs + related hits
  # … do the work …
  amt release "$id" --agent worker-1 --status in_review -m "done, see [[Session Tokens]]"
done
```

Claims are leased (default 15 min; re-claim to renew) and have a cooldown so loops don't
re-serve an issue you just released. `amt agents` shows the live roster; `amt stats` reports
throughput, cycle time, and a claim-integrity audit.

## Command reference

| Command | Purpose |
|---|---|
| `amt init` | Create `.ametrite/ametrite.db` workspace in the current directory |
| `amt issue create/list/show/update/comment` | Issue CRUD (labels, projects, parents, due dates) |
| `amt claim` / `amt release` | Atomic claim-loop primitives (`--peek`, `--project`, `--label`, `--all-workspaces`) |
| `amt dep add/rm/list` | Blocker → blocked dependencies (cycle-checked; blocked issues aren't claimable) |
| `amt decide` / `amt decision list/show` | Record ADR-style decisions against issues; supersede old ones |
| `amt note create/show/append/list` | Knowledge base (`--dedupe` warns on near-duplicate titles) |
| `amt project create/list` | Projects (first-class, wikilinkable documents) |
| `amt search <terms>` | FTS5 full-text search (`--type`, `--tag`, `--all-workspaces`) |
| `amt context <key>` | One-bundle context read for an issue, with an optional char `--budget` |
| `amt backlinks <id>` | Reverse link graph |
| `amt agents` / `amt stats` / `amt events` | Agent roster, throughput metrics, NDJSON activity stream |
| `amt branch <key>` | Create + check out a git branch named for an issue |
| `amt hook install` | git commit-msg hook that appends `Refs: <KEY>` from the branch name |
| `amt export <dir>` / `amt import <dir>` | Round-trip the workspace as an Obsidian-compatible markdown vault |
| `amt ws add/list/remove` | Global workspace registry (`~/.ametrite/registry.json`) |
| `amt doctor` | Workspace health: unresolved links, stale claims, missing refs |
| `amt seed --count N` | Bulk-insert N synthetic issues (benchmarking / demos) |
| `amt gc` | Compact the database: FTS optimize, `VACUUM`, WAL checkpoint |
| `amt mcp` | MCP stdio server |

Statuses: `backlog → todo → in_progress → in_review → done` (or `canceled`).
Priorities: `urgent > high > medium > low > none`.

## Git integration

`.ametrite/` is git-ignored and branch-invariant: switch branches, same board — claim state
stays a single source of truth for agents on different branches. Code links back the other
way: `amt branch AMT-7` names branches after issues, `amt hook install` stamps commits with
`Refs: AMT-7`, `amt issue show` lists referencing commits, and `amt release` appends the
branch's commits to the closing comment.

## Design notes

- SQLite (WAL) is the source of truth; only the Rust engine writes it.
- `[[AMT-12]]`, `[[Note Title]]`, and `[[note-slug]]` all resolve; dangling links resolve
  automatically when the target is created later (`amt doctor` lists the ones that haven't).
- The Rust engine's full dependency list: rusqlite, serde, serde_json, clap, regex.

## Status & license

v0.1.0, early but functional — the engine is exercised by a Rust test suite and CI on
Linux/macOS/Windows. MIT licensed ([LICENSE](LICENSE)).
