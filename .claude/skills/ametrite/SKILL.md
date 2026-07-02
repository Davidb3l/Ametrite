---
name: ametrite
description: Track work, knowledge, and decisions in an Ametrite workspace (local Linear+Obsidian for agents). Use when asked to create/claim/update issues, take project notes, record a decision, search the workspace, or run an agent work loop — or whenever a .ametrite/ directory exists and you are starting, finishing, or making a non-obvious choice about a piece of work.
---

# Ametrite — shared task board + knowledge base + decision log

Ametrite is a local SQLite workspace shared by humans and AI agents: Linear-style
issues, Obsidian-style wikilinked notes, and first-class decision records, all in
one link graph. You interact through the `amt` CLI (single binary). Everything
supports `--json` — always use it when you need to parse output.

## Setup / discovery

- Find the binary: `amt` on PATH, or `target/release/amt` / `target/debug/amt` in this repo.
- A workspace is a `.ametrite/` directory, found by walking up from cwd (like `.git`).
  If none exists and the user wants one: `amt init --name <project> --prefix AMT`.
- Identify yourself: set `AMT_AGENT=<stable-name>` (e.g. `claude-main`) or pass `--agent`.
  Attribution matters — humans read the activity log.

## Core commands

```sh
amt issue create --title "..." [-b "markdown body"] [--priority urgent|high|medium|low|none] [--label X]... [--project slug] [--parent AMT-1] --json
amt issue list [--status todo] [--project X] [--label Y] [--all] --json
amt issue show AMT-7 --json          # body + activity + backlinks
amt issue update AMT-7 [--status in_review] [--priority high] [-b "new body"] [--add-label X] --json
amt issue comment AMT-7 -m "finding or progress note" --author $AMT_AGENT
amt note create --title "..." -b "markdown" [--tag X]... --json
amt note append <note-id> -b "## New section\n..."
amt search <terms> [--type issue|note|decision|project] [--tag X] --json   # FTS5; last term prefix-matches
amt backlinks <id> --json            # who links here
amt doctor                           # unresolved links, stale claims, dangling decisions
```

## The agent work loop

```sh
amt claim --agent $AMT_AGENT [--project X] [--label Y] [--ttl 900] --json
```

Atomically claims the best available issue (priority, then age), sets it
`in_progress`, and grants a lease. Empty result `{"claimed": false}` means no work.
Rules:

1. Read the issue fully (`amt issue show`) and follow its backlinks before starting.
2. Renew a long-running lease: `amt claim --issue AMT-7 --agent $AMT_AGENT` (same command re-claims = heartbeat).
3. Comment when you learn something or change direction — comments are shared memory
   between agents. Don't post a separate "done" comment: the release `-m` message below
   IS your closing comment (posting both duplicates the activity log).
4. Put durable knowledge in **notes**, and always wikilink: mention `[[AMT-7]]` in the note body so the graph connects work ↔ knowledge.
5. Release when done: `amt release AMT-7 --agent $AMT_AGENT --status in_review -m "what I did"`.
   Use `in_review` unless you actually verified the work end-to-end (then `done`).
6. Never touch issues claimed by other agents. If you find new work, create a new issue.
7. Requeue cooldown: `claim` will not re-serve you an issue you released within the last
   hour (`--cooldown`, default 3600s), so release-to-todo loops move on to fresh work.
   `{"claimed": false}` may just mean everything left is your own recent work.

## Decisions (important — this is the workspace's memory of "why")

Whenever a non-obvious choice is made — architecture, tradeoff, scope cut, rejected
alternative — record it against the issue it resolves:

```sh
amt decide --issue AMT-7 --title "Use SQLite as source of truth" --author $AMT_AGENT -b "## Context
...
## Decision
...
## Consequences
..." --json
```

- Decisions get ids `D-1, D-2, …`, are linkable (`[[D-1]]`), appear in the issue's
  backlinks and activity, and are full-text searchable.
- To change your mind later, don't edit the old decision — supersede it:
  `amt decide --issue AMT-7 --title "New call" --supersedes D-1`.
- Before making a choice, check precedent: `amt decision list [--issue AMT-7]` and
  `amt search <topic> --type decision`.

## Other surfaces

- MCP server (15 tools, same capabilities): `claude mcp add ametrite -- amt mcp`.
- Web UI for humans: `bun run web` in the Ametrite repo → http://localhost:1776.
- Obsidian round-trip: `amt export <dir>` / `amt import <dir>`.
