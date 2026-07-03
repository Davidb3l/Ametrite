---
name: ametrite
description: Set up and drive an Ametrite workspace (local Linear+Obsidian for agents) in ANY repo. Trigger on "ametrite this", "ametrite this repo/project", "set up ametrite", "track this in ametrite" — or any request to create/claim/update issues, take project notes, record a decision, search the workspace, or run an agent work loop — or whenever a .ametrite/ directory exists and you are starting, finishing, or making a non-obvious choice about a piece of work. Handles first-time setup automatically; the user should never need manual installation steps.
---

# Ametrite — shared task board + knowledge base + decision log

Ametrite is a local SQLite workspace shared by humans and AI agents: Linear-style
issues, Obsidian-style wikilinked notes, and first-class decision records, all in
one link graph. You interact through the `amt` CLI (single binary). Everything
supports `--json` — always use it when you need to parse output.

## "ametrite this" — zero-friction bootstrap (do this automatically)

When the user says "ametrite this" (or similar) in a repo, run the whole setup
yourself — never hand the user a list of steps:

1. **Find the binary**: try `amt --version`. If missing, look for
   `target/release/amt` in a local Ametrite checkout, or
   `git clone https://github.com/Davidb3l/Ametrite.git && cargo build --release`
   (ask before cloning/building), then symlink it into a writable PATH dir
   (e.g. `~/.bun/bin` or `~/.local/bin`): `ln -sf <path>/amt <pathdir>/amt`.
2. **Init the workspace** (if no `.ametrite/` exists): from the repo root run
   `amt init --name <repo-name> --prefix <PREFIX>` — derive PREFIX from the repo
   name (short, uppercase, memorable: claude-app → CLAP; confirm with the user
   only if ambiguous). Init is fully self-contained: `.ametrite/` git-ignores
   itself, nothing else to configure.
3. **Seed from context**: if the user described work in the conversation, create
   the initial issues/notes for them immediately (with priorities and labels).
4. Mention (don't do unasked): `claude mcp add ametrite -- amt mcp` for MCP. The web
   board serves EVERY registered workspace from one port. **If it's already running,
   do NOT restart it** — the server watches the registry and picks up a newly
   `init`ed workspace live (it appears in the sidebar within ~1s, no restart, no
   refresh needed). Only start one if none is running:
   `bun run --cwd <ametrite-repo> web` → http://localhost:1776.

After bootstrap, just start working — the sections below are the conventions.

## Identity

Set `AMT_AGENT=<stable-name>` (e.g. `claude-main`) or pass `--agent`/`--author`
on every command. Attribution matters — humans read the activity log.

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
- Web UI for humans: `bun run web` in the Ametrite repo → http://localhost:1776 —
  one board serves every registered workspace (sidebar switcher; `amt ws list`).
- Obsidian round-trip: `amt export <dir>` / `amt import <dir>`.
