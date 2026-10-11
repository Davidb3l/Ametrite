import { Database } from "bun:sqlite";
import { beforeEach, expect, test } from "bun:test";
import { search } from "./search";

// The slice of the engine's schema (crates/amt/src/db.rs) that search reads,
// with the same FTS5 table definition and sync trigger.
let db: Database;
beforeEach(() => {
  db = new Database(":memory:");
  db.run(`CREATE TABLE documents (
    doc_id INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE COLLATE NOCASE,
    type TEXT NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL DEFAULT '')`);
  db.run(`CREATE TABLE tags (doc_id INTEGER NOT NULL, tag TEXT NOT NULL, src TEXT NOT NULL DEFAULT 'body')`);
  db.run(`CREATE VIRTUAL TABLE documents_fts USING fts5(
    title, body, content='documents', content_rowid='doc_id', tokenize='porter unicode61')`);
  db.run(`CREATE TRIGGER documents_ai AFTER INSERT ON documents BEGIN
    INSERT INTO documents_fts(rowid, title, body) VALUES (new.doc_id, new.title, new.body); END`);
  const add = db.prepare("INSERT INTO documents (id, type, title, body) VALUES (?, ?, ?, ?)");
  add.run("MSX-269", "issue", "Live credentials for go-live", "Railway   and\nStripe keys.");
  add.run("MSX-279", "issue", "Go-live dry run", "Blocked on MSX-269 credentials.");
  add.run("D-142", "decision", "PITR uses WAL-G", "Unverified until **MSX-269**.");
  add.run("handoff-2026-10-10", "note", "Handoff 2026-10-10", "Live work needs MSX-269.");
});
const ids = (q: string, extra: Record<string, string> = {}) =>
  search(db, new URLSearchParams({ q, ...extra })).map((h) => h.id);

test("searching an issue key lists that issue first (AMT-43)", () => {
  const hits = ids("msx-269");
  expect(hits[0]).toBe("MSX-269");
  // The documents that mention it still follow, and the issue isn't repeated.
  expect(hits).toContain("D-142");
  expect(hits.filter((id) => id === "MSX-269")).toHaveLength(1);
});

test("exact ids match case-insensitively, for decisions and notes too", () => {
  expect(ids("MSX-269")[0]).toBe("MSX-269");
  expect(ids("d-142")[0]).toBe("D-142");
  expect(ids("handoff-2026-10-10")[0]).toBe("handoff-2026-10-10");
});

test("the pinned hit has a one-line snippet from its body", () => {
  const [top] = search(db, new URLSearchParams({ q: "msx-269" }));
  expect(top.snippet).toBe("Railway and Stripe keys.");
});

test("filters apply to the exact match too", () => {
  expect(ids("msx-269", { type: "note" })).toEqual(["handoff-2026-10-10"]);
});

test("a multi-word query or a non-id is plain full-text search", () => {
  expect(ids("msx-269 credentials")).not.toContain("MSX-269");
  expect(ids("stripe")).toEqual(["MSX-269"]);
  expect(ids("")).toEqual([]);
});
