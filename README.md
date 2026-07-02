# ◆ Ametrite

**Local-first Linear + Obsidian for AI agent workflows.** Issue tracking and a wikilinked
markdown knowledge base in one SQLite file — driven by humans through a web UI and by AI
agents through MCP and a CLI, running claim → work → comment → release loops. No cloud, no
accounts, no embeddings, near-zero dependencies.

```
Bun web app (Bun.serve, vanilla TS)        amt (single Rust binary)
  reads:  bun:sqlite (direct SQL)   ◀──      all domain logic
  writes: shell `amt --json …`      ──▶      CLI + MCP stdio server
                                                │ rusqlite (WAL)
                                       .ametrite/ametrite.db
```

- **SQLite is the source of truth** (WAL mode; atomic cross-process claims via `BEGIN IMMEDIATE`).
- **Markdown is the content format** — issue and note bodies support `[[wikilinks]]` and `#tags`,
  indexed into a link graph with backlinks.
- **Search without embeddings** — SQLite FTS5 (BM25) + tags + link graph + structured filters.
- **Decisions are first-class** — `amt decide` records an ADR-style decision against the issue
  it resolves (`D-1`, linkable, searchable, supersedable). The "why" is one backlink hop from the "what".
- **Single-binary agent story** — `amt` is both the CLI and the MCP server. Agents never need Bun.
- **Zero npm runtime dependencies** — the web app uses only Bun built-ins.
- **Obsidian interop** — `amt export` / `amt import` round-trip the workspace as a markdown vault.

## Quickstart

```sh
# 1. Build the engine (Rust 1.75+)
cargo build --release            # → target/release/amt (put it on your PATH)

# 2. Create a workspace in any directory (fully self-contained —
#    .ametrite/ git-ignores itself, nothing else to configure)
amt init --name my-project --prefix AMT

# 3. Use it
amt issue create --title "Fix login token refresh" --priority urgent --label bug
amt issue list
amt note create --title "Session Tokens" -b "Rotation affects [[AMT-1]]. #auth"
amt search token
amt backlinks AMT-1

# 4. Web UI (requires Bun)
bun run web                      # → http://localhost:1776 (local-first independence 🇺🇸)
                                 #   one board, every registered workspace (amt ws list)
```

## AI agents

**MCP** (Claude Code, or any MCP client):

```sh
claude mcp add ametrite -- amt mcp
```

15 tools: `create_issue`, `list_issues`, `get_issue`, `claim_next_issue`, `claim_issue`,
`release_issue`, `update_issue`, `add_comment`, `record_decision`, `list_decisions`,
`create_note`, `append_to_note`, `read_doc`, `search`, `get_backlinks`.

**Claude Code skill**: this repo ships `.claude/skills/ametrite/` — inside the repo, Claude Code
picks it up automatically; for global use copy it to `~/.claude/skills/`. It teaches any agent
the workspace conventions: the claim loop, decision recording, and wikilink etiquette.

**CLI loops** — everything takes `--json`:

```sh
while issue=$(amt --json claim --agent worker-1); do
  # … do the work …
  amt release "$(echo "$issue" | jq -r .id)" --agent worker-1 --status in_review -m "done, see [[notes]]"
done
```

Claims are race-free (two agents can never claim the same issue) and leased: if an agent
crashes, its lease expires and the issue becomes claimable again. Re-claiming your own issue
renews the lease. See [AGENTS.md](AGENTS.md) for loop etiquette.

## Commands

| Command | Purpose |
|---|---|
| `amt init` | Create `.ametrite/ametrite.db` workspace |
| `amt issue create/list/show/update/comment` | Issue CRUD |
| `amt claim` / `amt release` | Atomic claim loop primitives |
| `amt decide` / `amt decision list\|show` | Record ADR-style decisions against issues; supersede old ones |
| `amt note create/show/append/list` | Knowledge base |
| `amt project create/list` | Projects |
| `amt search <terms>` | FTS5 full-text search |
| `amt backlinks <id>` | Reverse link graph |
| `amt export <dir>` / `amt import <dir>` | Obsidian-compatible markdown vault |
| `amt doctor` | Unresolved links, stale claims, missing refs |
| `amt mcp` | MCP stdio server |

## Design notes

- **One write path.** Only the Rust engine writes the database; the web server reads SQLite
  directly and shells to `amt --json` for mutations. Business logic lives in exactly one place.
- **Live updates** via SSE: the server polls SQLite's `data_version` (~400ms) and the board
  refreshes when any process writes — CLI, MCP agent, or another browser tab.
- **Wikilink resolution**: `[[AMT-12]]`, `[[Note Title]]`, and `[[note-slug]]` all resolve;
  dangling links resolve automatically when the target is later created (`amt doctor` lists
  the ones that haven't).

## How Ametrite relates to git branches

The `.ametrite/` workspace is git-ignored and **branch-invariant**: switch branches, same
board. This is a deliberate design position — "AMT-7 is in review" is a fact about your
*work*, not about a code snapshot. If the board forked with every feature branch, agents on
different branches would see different claim states and the single source of truth that
makes multi-agent coordination possible would be gone. (Nobody wants a board that's four
branches behind main; if a branch is *truly* divergent, it should be its own repo — and
therefore its own workspace, which the registry and workspace switcher already handle.)

Branch workflows are supported at the right layer instead:

| You want | Use |
|---|---|
| Link work to code | Issue keys in branch names/commits; `amt branch AMT-7` + merge detection → auto status *(roadmap R5)* |
| Branch the **plan**, not the code | `amt fork` — copy the workspace, run agents against the fork, diff, merge the winner *(roadmap W1)* |
| Workspace state in version control | `amt export` → commit the markdown vault; branches/remotes carry snapshots; `amt import` restores *(roadmap W5)* |
| A long-lived release line with its own backlog | Model it as a **project** (`release-2.x`), or a separate workspace if truly divergent |

## Projects vs labels

A **project** is a first-class document (`type: project`): a slug, a title, and a markdown
body that holds the brief. Issues attach to exactly one via `--project`; agents can run
project-scoped loops (`amt claim --project auth-revamp`); and because it's a document it's
wikilinkable — `[[auth-revamp]]` from any note or decision — so its backlinks panel becomes
a live index of everything touching the effort. **Labels** are cross-cutting adjectives
(`bug`, `backend`, as many as you like). Rule of thumb: *project = what larger effort this
belongs to (one); labels = what kind of thing it is (many).*

## v2 (planned)

- Tauri desktop wrapper — one codebase → Windows/macOS/Linux installers
- Live Obsidian vault mirroring (two-way file sync)
- Configurable workflows and statuses

MIT licensed. See [PRD.md](PRD.md) for the v1 build tracker and [ROADMAP.md](ROADMAP.md) for v1.5 (multi-workspace agents, context packs, dependencies, graph view).
