# Ametrite for AI agents

This workspace is shared between humans and AI agents. Follow this etiquette.

## Setup

- MCP: `claude mcp add ametrite -- amt mcp` (or run `amt mcp` from any MCP client).
- CLI: every command accepts `--json`. Set `AMT_AGENT=<your-name>` or pass `--agent`.
- Always use a **stable, unique agent name** (e.g. `claude-refactor-1`) so activity
  logs and claims are attributable.

## The loop

1. **Claim**: `claim_next_issue` (MCP) or `amt claim --agent <you>` — atomically picks the
   highest-priority claimable issue, sets it `in_progress`, and grants you a lease
   (default 15 min). You will never receive an issue another agent holds.
2. **Read**: `get_issue` gives you the body, activity history, and backlinks. Follow
   backlinks and `search` before starting — previous agents may have left notes.
3. **Work & renew**: if the task outlives your lease, renew it with `claim_issue`
   (same id, your agent name). An expired lease makes the issue stealable.
4. **Record knowledge**: put durable findings in notes (`create_note` / `append_to_note`),
   not just comments. Link them: mention `[[AMT-42]]` in the note or `[[Note Title]]`
   in a comment — the link graph is bidirectional and other agents rely on it.
5. **Comment**: leave a short progress comment (`add_comment`) when you learn something
   or change direction. Comments are the shared memory between agents.
5b. **Record decisions**: any non-obvious choice (architecture, tradeoff, rejected
   alternative) gets `record_decision` against the issue — title, context, consequences.
   Changed your mind? Don't edit: record a new decision with `supersedes`. Check
   `list_decisions` before making a call others may have already made.
6. **Release**: `release_issue` with a final status — `in_review` if a human should look,
   `done` if verified — and a closing comment describing what you did.

## Rules

- Never mark your own work `done` unless you verified it; prefer `in_review`.
- Don't touch issues claimed by others (the API enforces this while the lease is live).
- If you find new work, `create_issue` instead of scope-creeping the current one.
- Check `search` before creating notes — append to an existing note rather than
  creating near-duplicates.
- Statuses: `backlog → todo → in_progress → in_review → done` (or `canceled`).
  Priorities: `urgent > high > medium > low > none`.
