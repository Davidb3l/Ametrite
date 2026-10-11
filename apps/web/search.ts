// Board search over the workspace's FTS5 index. Pure (db + params in, hits
// out) so it is unit-testable against an in-memory database.
import type { Database } from "bun:sqlite";

export function ftsQuery(q: string): string {
  const terms = q.split(/\s+/).filter(Boolean).map((t) => `"${t.replaceAll('"', '""')}"`);
  if (terms.length === 0) return "";
  terms[terms.length - 1] += "*";
  return terms.join(" ");
}

function searchFilters(params: URLSearchParams, args: any[]): string {
  let sql = "";
  if (params.get("type")) {
    sql += " AND d.type = ?";
    args.push(params.get("type"));
  }
  if (params.get("tag")) {
    sql += " AND EXISTS(SELECT 1 FROM tags t WHERE t.doc_id = d.doc_id AND t.tag = lower(?))";
    args.push(params.get("tag"));
  }
  return sql;
}
// A query that is exactly a document's id (an issue key like MSX-269, D-142,
// a note slug) lists that document first. Ids aren't in the FTS index, so
// otherwise searching a key found only the documents that mention it (AMT-43).
// Mirrors store::search in the engine.
export function search(db: Database, params: URLSearchParams): any[] {
  const q = (params.get("q") ?? "").trim();
  const match = ftsQuery(q);
  if (!match) return [];
  const args: any[] = [match];
  const sql = `
    SELECT d.id, d.type, d.title, snippet(documents_fts, 1, '', '', '…', 18) AS snippet
    FROM documents_fts JOIN documents d ON d.doc_id = documents_fts.rowid
    WHERE documents_fts MATCH ?${searchFilters(params, args)}
    ORDER BY bm25(documents_fts) LIMIT 50`;
  let hits: any[] = [];
  try {
    hits = db.query(sql).all(...args) as any[];
  } catch {}
  if (q && !/\s/.test(q)) {
    const exactArgs: any[] = [q];
    // documents.id is COLLATE NOCASE, so `=` is case-insensitive.
    const exact = db
      .query(
        `SELECT d.id, d.type, d.title, substr(d.body, 1, 160) AS snippet FROM documents d WHERE d.id = ?${searchFilters(params, exactArgs)}`
      )
      .get(...exactArgs) as any;
    if (exact) {
      exact.snippet = String(exact.snippet ?? "").replace(/\s+/g, " ").trim();
      hits = [exact, ...hits.filter((h) => String(h.id).toLowerCase() !== String(exact.id).toLowerCase())].slice(0, 50);
    }
  }
  return hits;
}
