// Ametrite web server — one board, every registered workspace.
// Reads: bun:sqlite per workspace. Writes: shell to `amt`. Zero npm deps.
import { Database } from "bun:sqlite";
import { existsSync, readFileSync, statSync } from "node:fs";
import { join, dirname, resolve, basename } from "node:path";
import { homedir } from "node:os";
import index from "./index.html";

// 1776 — a local-first declaration of independence from cloud SaaS.
const PORT = Number(process.env.AMT_PORT ?? 1776);

type Workspace = { alias: string; root: string; db: Database | null };
const workspaces = new Map<string, Workspace>();

function slug(s: string): string {
  return s.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "workspace";
}

function addWorkspace(alias: string, root: string) {
  if (existsSync(join(root, ".ametrite", "ametrite.db")) && !workspaces.has(alias)) {
    workspaces.set(alias, { alias, root, db: null });
  }
}

// Registry first (AMT_REGISTRY or ~/.ametrite/registry.json), then
// AMT_WORKSPACE / cwd walk-up. Honors the same override as the `amt` engine
// so the board and the CLI never read different registries.
const registryPath = process.env.AMT_REGISTRY ?? join(homedir(), ".ametrite", "registry.json");

function readRegistry(): Record<string, string> {
  try {
    return (JSON.parse(readFileSync(registryPath, "utf8")).workspaces ?? {}) as Record<string, string>;
  } catch {
    return {};
  }
}

// (Re-)read the registry and add any workspaces we aren't tracking yet.
// Returns the newly-added aliases so a live update can be announced (AMT-10).
function syncRegistry(): string[] {
  const before = new Set(workspaces.keys());
  for (const [alias, root] of Object.entries(readRegistry())) addWorkspace(alias, String(root));
  return [...workspaces.keys()].filter((a) => !before.has(a));
}
syncRegistry();
if (process.env.AMT_WORKSPACE) {
  const root = resolve(process.env.AMT_WORKSPACE);
  const existing = [...workspaces.values()].find((w) => w.root === root);
  if (!existing) addWorkspace(slug(basename(root)), root);
} else {
  let dir = process.cwd();
  while (true) {
    if (existsSync(join(dir, ".ametrite", "ametrite.db"))) {
      if (![...workspaces.values()].some((w) => w.root === dir)) {
        addWorkspace(slug(basename(dir)), dir);
      }
      break;
    }
    const parent = dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
}
if (workspaces.size === 0) {
  console.error("no workspaces found — run `amt init` in a repo (auto-registers) or `amt ws add <path>`");
  process.exit(1);
}
const defaultAlias =
  process.env.AMT_WORKSPACE
    ? [...workspaces.values()].find((w) => w.root === resolve(process.env.AMT_WORKSPACE!))?.alias
    : undefined;

function dbOf(ws: Workspace): Database {
  // Not readonly: a WAL database needs the connection to be able to
  // (re)create -shm/-wal sidecars. This connection still never writes —
  // all mutations shell out to `amt`.
  ws.db ??= new Database(join(ws.root, ".ametrite", "ametrite.db"));
  return ws.db;
}

function wsOf(req: Request): Workspace {
  const alias = new URL(req.url).searchParams.get("ws");
  return (alias && workspaces.get(alias)) || workspaces.get(defaultAlias ?? "") || workspaces.values().next().value!;
}

function findAmt(): string {
  if (process.env.AMT_BIN) return process.env.AMT_BIN;
  const repo = dirname(import.meta.dir);
  for (const candidate of [
    join(dirname(repo), "target", "release", "amt"),
    join(dirname(repo), "target", "debug", "amt"),
  ]) {
    if (existsSync(candidate)) return candidate;
  }
  return "amt";
}
const amtBin = findAmt();

// ---------- write path: every mutation shells to the Rust engine ----------
async function amt(ws: Workspace, args: string[]): Promise<Response> {
  const proc = Bun.spawn([amtBin, "--workspace", ws.root, "--json", ...args], {
    stdout: "pipe",
    stderr: "pipe",
  });
  const [out, err] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
  ]);
  const code = await proc.exited;
  if (code !== 0) {
    return Response.json({ error: err.replace(/^error:\s*/, "").trim() || "amt failed" }, { status: 400 });
  }
  return new Response(out, { headers: { "content-type": "application/json" } });
}

// ---------- read path: direct SQL ----------
const PRIORITY_RANK =
  "CASE i.priority WHEN 'urgent' THEN 0 WHEN 'high' THEN 1 WHEN 'medium' THEN 2 WHEN 'low' THEN 3 ELSE 4 END";
// `blocked`: 1 when this issue has at least one OPEN blocker (a `blocks` edge
// whose blocker isn't done/canceled) — the chain-icon signal (R3), matching the
// engine's claimable predicate. The `blocks` table only exists at schema v4+;
// the web layer never migrates (only the `amt` engine does), so a workspace
// still on v3 has no `blocks` table. Detect it per-DB and fall back to
// `blocked = 0` there, so the board serves un-migrated workspaces without
// erroring. Once any `amt` command migrates that workspace, the icon appears.
const hasBlocksCache = new WeakMap<Database, boolean>();
function hasBlocks(db: Database): boolean {
  let v = hasBlocksCache.get(db);
  if (v === undefined) {
    v = !!db.query("SELECT 1 FROM sqlite_master WHERE type='table' AND name='blocks'").get();
    hasBlocksCache.set(db, v);
  }
  return v;
}
function issueSelect(db: Database): string {
  const blocked = hasBlocks(db)
    ? `EXISTS(
           SELECT 1 FROM blocks b JOIN issues bi
             ON bi.doc_id = (SELECT doc_id FROM documents WHERE id = b.blocker)
           WHERE b.blocked = d.id AND bi.status NOT IN ('done','canceled')
         )`
    : "0";
  return `
  SELECT d.doc_id, d.id, d.title, i.status, i.priority, i.project, i.assignee,
         i.parent_id AS parent, i.due, i.claimed_by, i.claim_expires_at,
         d.created_at, d.updated_at,
         ${blocked} AS blocked
  FROM documents d JOIN issues i ON i.doc_id = d.doc_id`;
}

function withLabels(db: Database, rows: any[]): any[] {
  const stmt = db.query("SELECT DISTINCT tag FROM tags WHERE doc_id = ? ORDER BY tag");
  for (const r of rows) {
    r.labels = stmt.all(r.doc_id).map((t: any) => t.tag);
    delete r.doc_id;
  }
  return rows;
}

function listIssues(db: Database, params: URLSearchParams): any[] {
  let sql = `${issueSelect(db)} WHERE 1=1`;
  const args: any[] = [];
  if (params.get("status")) {
    sql += " AND i.status = ?";
    args.push(params.get("status"));
  } else if (!params.get("all")) {
    sql += " AND i.status NOT IN ('done','canceled')";
  }
  for (const [key, col] of [["project", "i.project"], ["assignee", "i.assignee"]] as const) {
    if (params.get(key)) {
      sql += ` AND ${col} = ?`;
      args.push(params.get(key));
    }
  }
  if (params.get("label")) {
    sql += " AND EXISTS(SELECT 1 FROM tags t WHERE t.doc_id = d.doc_id AND t.tag = lower(?))";
    args.push(params.get("label"));
  }
  sql += ` ORDER BY ${PRIORITY_RANK}, d.created_at LIMIT 500`;
  return withLabels(db, db.query(sql).all(...args) as any[]);
}

function getIssue(db: Database, id: string): any | null {
  const row: any = db.query(`${issueSelect(db)} WHERE d.id = ?`).get(id);
  if (!row) return null;
  const docId = row.doc_id;
  withLabels(db, [row]);
  row.body = (db.query("SELECT body FROM documents WHERE id = ?").get(id) as any)?.body ?? "";
  row.activity = db
    .query("SELECT seq, at, author, kind, body FROM activity WHERE doc_id = ? ORDER BY seq")
    .all(docId);
  row.backlinks = backlinksOf(db, docId);
  row.decisions = listDecisions(db, id);
  return row;
}

function listDecisions(db: Database, issue?: string | null): any[] {
  let sql = `
    SELECT d.id, d.title, dc.resolves, dc.status, dc.superseded_by, d.created_at
    FROM decisions dc JOIN documents d ON d.doc_id = dc.doc_id`;
  const args: any[] = [];
  if (issue) {
    sql += " WHERE dc.resolves = ?";
    args.push(issue);
  }
  sql += " ORDER BY dc.decision_num";
  return db.query(sql).all(...args) as any[];
}

function backlinksOf(db: Database, docId: number): any[] {
  return db
    .query(
      `SELECT DISTINCT d.id, d.type, d.title FROM links l
       JOIN documents d ON d.doc_id = l.source_doc_id
       WHERE l.target_doc_id = ? ORDER BY d.id`
    )
    .all(docId);
}

function getDoc(db: Database, id: string): any | null {
  const row: any = db
    .query("SELECT doc_id, id, type, title, body, created_at, updated_at FROM documents WHERE id = ? OR lower(title) = lower(?) LIMIT 1")
    .get(id, id);
  if (!row) return null;
  row.tags = db.query("SELECT DISTINCT tag FROM tags WHERE doc_id = ? ORDER BY tag").all(row.doc_id).map((t: any) => t.tag);
  row.backlinks = backlinksOf(db, row.doc_id);
  delete row.doc_id;
  return row;
}

function ftsQuery(q: string): string {
  const terms = q.split(/\s+/).filter(Boolean).map((t) => `"${t.replaceAll('"', '""')}"`);
  if (terms.length === 0) return "";
  terms[terms.length - 1] += "*";
  return terms.join(" ");
}

function search(db: Database, params: URLSearchParams): any[] {
  const match = ftsQuery(params.get("q") ?? "");
  if (!match) return [];
  let sql = `
    SELECT d.id, d.type, d.title, snippet(documents_fts, 1, '', '', '…', 18) AS snippet
    FROM documents_fts JOIN documents d ON d.doc_id = documents_fts.rowid
    WHERE documents_fts MATCH ?`;
  const args: any[] = [match];
  if (params.get("type")) {
    sql += " AND d.type = ?";
    args.push(params.get("type"));
  }
  if (params.get("tag")) {
    sql += " AND EXISTS(SELECT 1 FROM tags t WHERE t.doc_id = d.doc_id AND t.tag = lower(?))";
    args.push(params.get("tag"));
  }
  sql += " ORDER BY bm25(documents_fts) LIMIT 50";
  try {
    return db.query(sql).all(...args) as any[];
  } catch {
    return [];
  }
}

// ---------- SSE: poll the registry + every workspace's data_version ----------
const sseClients = new Set<ReadableStreamDefaultController>();
const versions = new Map<string, number>();

function broadcast(event: string, data: string) {
  for (const c of sseClients) {
    try {
      c.enqueue(`event: ${event}\ndata: ${data}\n\n`);
    } catch {}
  }
}

let registryMtime = (() => {
  try {
    return statSync(registryPath).mtimeMs;
  } catch {
    return 0;
  }
})();

// Activity events with rowid > since (the R4 event stream), oldest first.
// `activity.rowid` is a global monotonic cursor, matching store::events.
function eventsSince(db: Database, since: number, limit: number): any[] {
  return db
    .query(
      `SELECT a.rowid AS cursor, d.id, d.type, a.seq, a.at, a.author, a.kind, a.body
       FROM activity a JOIN documents d ON d.doc_id = a.doc_id
       WHERE a.rowid > ? ORDER BY a.rowid LIMIT ?`
    )
    .all(since, limit);
}
function eventsTip(db: Database): number {
  return (db.query("SELECT COALESCE(MAX(rowid),0) AS c FROM activity").get() as any).c;
}
const cursors = new Map<string, number>(); // per-workspace last-emitted event cursor

setInterval(() => {
  // Pick up workspaces registered since boot (e.g. `amt init` in a new repo)
  // without a restart, then tell open boards to refresh their sidebar (AMT-10).
  try {
    const m = statSync(registryPath).mtimeMs;
    if (m !== registryMtime) {
      registryMtime = m;
      const added = syncRegistry();
      if (added.length) broadcast("workspaces", JSON.stringify({ added }));
    }
  } catch {}

  for (const ws of workspaces.values()) {
    if (!ws.db) continue; // only poll workspaces someone has looked at
    let v: number;
    try {
      v = (ws.db.query("PRAGMA data_version").get() as any).data_version;
    } catch {
      continue;
    }
    if (!cursors.has(ws.alias)) cursors.set(ws.alias, eventsTip(ws.db)); // start at tip
    if (versions.has(ws.alias) && versions.get(ws.alias) !== v) {
      broadcast("change", `{"ws":"${ws.alias}"}`);
      // Push the actual new activity rows so live consumers (R4 event stream)
      // react without re-fetching the whole board.
      try {
        for (const e of eventsSince(ws.db, cursors.get(ws.alias)!, 200)) {
          broadcast("activity", JSON.stringify({ ws: ws.alias, ...e }));
          cursors.set(ws.alias, e.cursor);
        }
      } catch {}
    }
    versions.set(ws.alias, v);
  }
}, 400);

function json(data: any, status = 200): Response {
  return Response.json(data, { status });
}

const flag = (name: string, v: any): string[] => (v === undefined || v === null ? [] : [`--${name}`, String(v)]);

Bun.serve({
  port: PORT,
  idleTimeout: 0,
  routes: {
    "/": index,
    "/api/workspaces": () =>
      json(
        [...workspaces.values()].map((ws) => {
          const meta = Object.fromEntries(
            (dbOf(ws).query("SELECT key, value FROM meta").all() as any[]).map((r) => [r.key, r.value])
          );
          const open = (dbOf(ws).query(
            "SELECT COUNT(*) AS n FROM issues WHERE status NOT IN ('done','canceled')"
          ).get() as any).n;
          return { alias: ws.alias, name: meta.workspace_name, prefix: meta.id_prefix, root: ws.root, open_issues: open };
        })
      ),
    // Inbox: every workspace's open issues in one globally priority-sorted
    // list, each row tagged with its workspace (R1 cross-workspace view).
    "/api/inbox": () => {
        const rank: Record<string, number> = { urgent: 0, high: 1, medium: 2, low: 3, none: 4 };
        const out: any[] = [];
        for (const ws of workspaces.values()) {
            const db = dbOf(ws);
            const name = (db.query("SELECT value FROM meta WHERE key = 'workspace_name'").get() as any)?.value ?? ws.alias;
            for (const r of listIssues(db, new URLSearchParams())) {
                r.workspace = ws.alias;
                r.workspace_name = name;
                out.push(r);
            }
        }
        out.sort((a, b) => (rank[a.priority] ?? 9) - (rank[b.priority] ?? 9) || a.created_at.localeCompare(b.created_at));
        return json(out);
    },
    "/api/workspace": (req) => {
      const ws = wsOf(req);
      const meta = Object.fromEntries(
        (dbOf(ws).query("SELECT key, value FROM meta").all() as any[]).map((r) => [r.key, r.value])
      );
      return json({ name: meta.workspace_name, prefix: meta.id_prefix, alias: ws.alias });
    },
    "/api/issues": {
      GET: (req) => json(listIssues(dbOf(wsOf(req)), new URL(req.url).searchParams)),
      POST: async (req) => {
        const b: any = await req.json();
        if (!b.title) return json({ error: "title required" }, 400);
        return amt(wsOf(req), [
          "issue", "create", "--title", b.title,
          ...flag("body", b.body), ...flag("priority", b.priority),
          ...flag("project", b.project), ...flag("assignee", b.assignee),
          ...(b.labels ?? []).flatMap((l: string) => ["--label", l]),
        ]);
      },
    },
    "/api/issues/:id": {
      GET: (req) => {
        const issue = getIssue(dbOf(wsOf(req)), req.params.id);
        return issue ? json(issue) : json({ error: "not found" }, 404);
      },
      PATCH: async (req) => {
        const b: any = await req.json();
        return amt(wsOf(req), [
          "issue", "update", req.params.id,
          ...flag("status", b.status), ...flag("priority", b.priority),
          ...flag("title", b.title), ...flag("body", b.body),
          ...flag("assignee", b.assignee), ...flag("project", b.project),
          ...flag("due", b.due),
          ...(b.add_labels ?? []).flatMap((l: string) => ["--add-label", l]),
          ...(b.remove_labels ?? []).flatMap((l: string) => ["--remove-label", l]),
        ]);
      },
    },
    "/api/issues/:id/comments": {
      POST: async (req) => {
        const b: any = await req.json();
        if (!b.body) return json({ error: "body required" }, 400);
        return amt(wsOf(req), ["issue", "comment", req.params.id, "-m", b.body, ...flag("author", b.author)]);
      },
    },
    "/api/decisions": {
      GET: (req) => json(listDecisions(dbOf(wsOf(req)), new URL(req.url).searchParams.get("issue"))),
      POST: async (req) => {
        const b: any = await req.json();
        if (!b.title || !b.issue) return json({ error: "title and issue required" }, 400);
        return amt(wsOf(req), [
          "decide", "--issue", b.issue, "--title", b.title,
          ...flag("body", b.body), ...flag("status", b.status),
          ...flag("supersedes", b.supersedes), ...flag("author", b.author),
        ]);
      },
    },
    "/api/notes": {
      GET: (req) => {
        const db = dbOf(wsOf(req));
        return json(
          withLabels(
            db,
            db.query(
              "SELECT doc_id, id, title, updated_at FROM documents WHERE type = 'note' ORDER BY updated_at DESC"
            ).all() as any[]
          )
        );
      },
      POST: async (req) => {
        const b: any = await req.json();
        if (!b.title) return json({ error: "title required" }, 400);
        return amt(wsOf(req), [
          "note", "create", "--title", b.title, ...flag("body", b.body),
          ...(b.tags ?? []).flatMap((t: string) => ["--tag", t]),
        ]);
      },
    },
    "/api/projects": (req) =>
      json(dbOf(wsOf(req)).query("SELECT id, title FROM documents WHERE type = 'project' ORDER BY title").all()),
    "/api/docs/:id": (req) => {
      const doc = getDoc(dbOf(wsOf(req)), decodeURIComponent(req.params.id));
      return doc ? json(doc) : json({ error: "not found" }, 404);
    },
    "/api/search": (req) => json(search(dbOf(wsOf(req)), new URL(req.url).searchParams)),
    // Fleet visibility (R9) — reuse the engine so the roster/stats logic lives
    // in exactly one place. `amt` shells out and already emits JSON.
    "/api/agents": (req) => amt(wsOf(req), ["agents"]),
    "/api/stats": (req) => {
      const since = new URL(req.url).searchParams.get("since");
      return amt(wsOf(req), ["stats", ...(since ? ["--since", since] : [])]);
    },
    "/api/events": (req) => {
      // REST catch-up: `?since=<cursor>` returns the events after that cursor as
      // a JSON array (poll with the highest cursor to resume). No `since` opens
      // the live SSE stream (hello + change/activity/workspaces frames).
      const sinceParam = new URL(req.url).searchParams.get("since");
      if (sinceParam !== null) {
        const since = Number(sinceParam) || 0;
        return json(eventsSince(dbOf(wsOf(req)), since, 500));
      }
      let ctrl: ReadableStreamDefaultController;
      const stream = new ReadableStream({
        start(c) {
          ctrl = c;
          sseClients.add(c);
          c.enqueue("event: hello\ndata: {}\n\n");
        },
        cancel() {
          sseClients.delete(ctrl);
        },
      });
      return new Response(stream, {
        headers: {
          "content-type": "text/event-stream",
          "cache-control": "no-cache",
          connection: "keep-alive",
        },
      });
    },
  },
});

console.log(`ametrite ▸ ${workspaces.size} workspace(s): ${[...workspaces.keys()].join(", ")}`);
console.log(`ametrite ▸ http://localhost:${PORT}`);
