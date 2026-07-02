// Ametrite web server — one board, every registered workspace.
// Reads: bun:sqlite per workspace. Writes: shell to `amt`. Zero npm deps.
import { Database } from "bun:sqlite";
import { existsSync, readFileSync } from "node:fs";
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

// Registry first (~/.ametrite/registry.json), then AMT_WORKSPACE / cwd walk-up.
try {
  const reg = JSON.parse(readFileSync(join(homedir(), ".ametrite", "registry.json"), "utf8"));
  for (const [alias, root] of Object.entries(reg.workspaces ?? {})) {
    addWorkspace(alias, String(root));
  }
} catch {}
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
const ISSUE_SELECT = `
  SELECT d.doc_id, d.id, d.title, i.status, i.priority, i.project, i.assignee,
         i.parent_id AS parent, i.due, i.claimed_by, i.claim_expires_at,
         d.created_at, d.updated_at
  FROM documents d JOIN issues i ON i.doc_id = d.doc_id`;

function withLabels(db: Database, rows: any[]): any[] {
  const stmt = db.query("SELECT DISTINCT tag FROM tags WHERE doc_id = ? ORDER BY tag");
  for (const r of rows) {
    r.labels = stmt.all(r.doc_id).map((t: any) => t.tag);
    delete r.doc_id;
  }
  return rows;
}

function listIssues(db: Database, params: URLSearchParams): any[] {
  let sql = `${ISSUE_SELECT} WHERE 1=1`;
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
  const row: any = db.query(`${ISSUE_SELECT} WHERE d.id = ?`).get(id);
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

// ---------- SSE: poll every workspace's data_version ----------
const sseClients = new Set<ReadableStreamDefaultController>();
const versions = new Map<string, number>();
setInterval(() => {
  for (const ws of workspaces.values()) {
    if (!ws.db) continue; // only poll workspaces someone has looked at
    let v: number;
    try {
      v = (ws.db.query("PRAGMA data_version").get() as any).data_version;
    } catch {
      continue;
    }
    if (versions.has(ws.alias) && versions.get(ws.alias) !== v) {
      for (const c of sseClients) {
        try {
          c.enqueue(`event: change\ndata: {"ws":"${ws.alias}"}\n\n`);
        } catch {}
      }
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
    "/api/events": () => {
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
