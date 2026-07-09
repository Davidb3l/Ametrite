use crate::error::{msg, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: i64 = 4;
pub const DB_DIR: &str = ".ametrite";
pub const DB_FILE: &str = "ametrite.db";

const SCHEMA: &str = r#"
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE documents (
  doc_id     INTEGER PRIMARY KEY,
  id         TEXT NOT NULL UNIQUE COLLATE NOCASE,
  type       TEXT NOT NULL CHECK (type IN ('issue','note','project','decision')),
  title      TEXT NOT NULL,
  body       TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE issues (
  doc_id           INTEGER PRIMARY KEY REFERENCES documents(doc_id) ON DELETE CASCADE,
  issue_num        INTEGER NOT NULL UNIQUE,
  status           TEXT NOT NULL DEFAULT 'backlog',
  priority         TEXT NOT NULL DEFAULT 'none',
  project          TEXT,
  assignee         TEXT,
  parent_id        TEXT,
  due              TEXT,
  claimed_by       TEXT,
  claim_expires_at TEXT,
  last_released_by TEXT,
  last_released_at TEXT
);
CREATE INDEX idx_issues_status   ON issues(status);
CREATE INDEX idx_issues_assignee ON issues(assignee);
CREATE INDEX idx_issues_project  ON issues(project);

CREATE TABLE tags (
  doc_id INTEGER NOT NULL REFERENCES documents(doc_id) ON DELETE CASCADE,
  tag    TEXT NOT NULL,
  src    TEXT NOT NULL DEFAULT 'body' CHECK (src IN ('label','body')),
  PRIMARY KEY (doc_id, tag, src)
);
CREATE INDEX idx_tags_tag ON tags(tag);

CREATE TABLE links (
  source_doc_id INTEGER NOT NULL REFERENCES documents(doc_id) ON DELETE CASCADE,
  target_raw    TEXT NOT NULL,
  target_doc_id INTEGER REFERENCES documents(doc_id) ON DELETE SET NULL
);
CREATE INDEX idx_links_source ON links(source_doc_id);
CREATE INDEX idx_links_target ON links(target_doc_id);

CREATE TABLE activity (
  doc_id INTEGER NOT NULL REFERENCES documents(doc_id) ON DELETE CASCADE,
  seq    INTEGER NOT NULL,
  at     TEXT NOT NULL,
  author TEXT NOT NULL,
  kind   TEXT NOT NULL CHECK (kind IN ('comment','event')),
  body   TEXT NOT NULL,
  PRIMARY KEY (doc_id, seq)
);

CREATE VIRTUAL TABLE documents_fts USING fts5(
  title, body, content='documents', content_rowid='doc_id', tokenize='porter unicode61'
);

CREATE TRIGGER documents_ai AFTER INSERT ON documents BEGIN
  INSERT INTO documents_fts(rowid, title, body) VALUES (new.doc_id, new.title, new.body);
END;
CREATE TRIGGER documents_ad AFTER DELETE ON documents BEGIN
  INSERT INTO documents_fts(documents_fts, rowid, title, body)
  VALUES ('delete', old.doc_id, old.title, old.body);
END;
CREATE TRIGGER documents_au AFTER UPDATE OF title, body ON documents BEGIN
  INSERT INTO documents_fts(documents_fts, rowid, title, body)
  VALUES ('delete', old.doc_id, old.title, old.body);
  INSERT INTO documents_fts(rowid, title, body) VALUES (new.doc_id, new.title, new.body);
END;

CREATE TABLE decisions (
  doc_id        INTEGER PRIMARY KEY REFERENCES documents(doc_id) ON DELETE CASCADE,
  decision_num  INTEGER NOT NULL UNIQUE,
  resolves      TEXT NOT NULL,
  status        TEXT NOT NULL DEFAULT 'accepted' CHECK (status IN ('proposed','accepted','superseded')),
  superseded_by TEXT
);
CREATE INDEX idx_decisions_resolves ON decisions(resolves);

CREATE TABLE blocks (
  blocker TEXT NOT NULL,
  blocked TEXT NOT NULL,
  PRIMARY KEY (blocker, blocked)
);
CREATE INDEX idx_blocks_blocked ON blocks(blocked);
CREATE INDEX idx_blocks_blocker ON blocks(blocker);
"#;

/// v1 → v2: allow the 'decision' document type (requires rebuilding the
/// documents table — SQLite cannot alter a CHECK constraint) and add the
/// decisions table. Runs with foreign_keys OFF so the table swap does not
/// cascade-delete children.
const MIGRATE_V1_V2: &str = r#"
CREATE TABLE documents_new (
  doc_id     INTEGER PRIMARY KEY,
  id         TEXT NOT NULL UNIQUE COLLATE NOCASE,
  type       TEXT NOT NULL CHECK (type IN ('issue','note','project','decision')),
  title      TEXT NOT NULL,
  body       TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
INSERT INTO documents_new SELECT doc_id, id, type, title, body, created_at, updated_at FROM documents;
DROP TRIGGER documents_ai;
DROP TRIGGER documents_ad;
DROP TRIGGER documents_au;
DROP TABLE documents;
ALTER TABLE documents_new RENAME TO documents;
CREATE TRIGGER documents_ai AFTER INSERT ON documents BEGIN
  INSERT INTO documents_fts(rowid, title, body) VALUES (new.doc_id, new.title, new.body);
END;
CREATE TRIGGER documents_ad AFTER DELETE ON documents BEGIN
  INSERT INTO documents_fts(documents_fts, rowid, title, body)
  VALUES ('delete', old.doc_id, old.title, old.body);
END;
CREATE TRIGGER documents_au AFTER UPDATE OF title, body ON documents BEGIN
  INSERT INTO documents_fts(documents_fts, rowid, title, body)
  VALUES ('delete', old.doc_id, old.title, old.body);
  INSERT INTO documents_fts(rowid, title, body) VALUES (new.doc_id, new.title, new.body);
END;
CREATE TABLE decisions (
  doc_id        INTEGER PRIMARY KEY REFERENCES documents(doc_id) ON DELETE CASCADE,
  decision_num  INTEGER NOT NULL UNIQUE,
  resolves      TEXT NOT NULL,
  status        TEXT NOT NULL DEFAULT 'accepted' CHECK (status IN ('proposed','accepted','superseded')),
  superseded_by TEXT
);
CREATE INDEX idx_decisions_resolves ON decisions(resolves);
UPDATE meta SET value = '2' WHERE key = 'schema_version';
"#;

/// v2 -> v3: track who last released an issue so `claim` can apply a
/// same-agent requeue cooldown (found dogfooding: a scoping loop that
/// releases to todo was immediately re-served its own issue).
const MIGRATE_V2_V3: &str = r#"
ALTER TABLE issues ADD COLUMN last_released_by TEXT;
ALTER TABLE issues ADD COLUMN last_released_at TEXT;
UPDATE meta SET value = '3' WHERE key = 'schema_version';
"#;

/// v3 -> v4: issue dependencies (R3). A `blocks` edge (blocker → blocked) makes
/// an issue unclaimable while its blocker is still open, so `claim`/`peek` skip
/// it and `doctor` can flag dependency cycles. Keyed by issue keys (not doc_ids)
/// to match how parents/projects are already referenced textually.
const MIGRATE_V3_V4: &str = r#"
CREATE TABLE blocks (
  blocker TEXT NOT NULL,
  blocked TEXT NOT NULL,
  PRIMARY KEY (blocker, blocked)
);
CREATE INDEX idx_blocks_blocked ON blocks(blocked);
CREATE INDEX idx_blocks_blocker ON blocks(blocker);
UPDATE meta SET value = '4' WHERE key = 'schema_version';
"#;

/// Walk up from `start` looking for `.ametrite/ametrite.db`.
pub fn find_workspace(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start.to_path_buf());
    while let Some(d) = dir {
        let candidate = d.join(DB_DIR).join(DB_FILE);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    None
}

fn set_pragmas(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 10_000)?;
    Ok(())
}

pub fn open(db_path: &Path) -> Result<Connection> {
    if !db_path.is_file() {
        return Err(msg(format!(
            "no workspace database at {} (run `amt init` first)",
            db_path.display()
        )));
    }
    let conn = Connection::open(db_path)?;
    set_pragmas(&conn)?;
    let version: i64 = conn
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get::<_, String>(0),
        )
        .map_err(|_| msg("not an ametrite database (missing meta.schema_version)"))?
        .parse()
        .map_err(|_| msg("corrupt meta.schema_version"))?;
    if version > SCHEMA_VERSION {
        return Err(msg(format!(
            "workspace schema v{version} is newer than this amt (supports v{SCHEMA_VERSION}) — upgrade amt"
        )));
    }
    if version < SCHEMA_VERSION {
        migrate(&conn, version)?;
    }
    Ok(conn)
}

/// Open a workspace database **read-only, without running migrations**.
///
/// The cross-workspace READ fan-outs (`list`/`search`/`peek --all-workspaces`,
/// the web inbox, and the no-work aggregation) open every registered workspace.
/// Routing those through the migrating [`open`] means a mere *read* silently
/// migrates — writes to — every DB on the machine, and hard-fails on a
/// newer-schema workspace. `open_ro` avoids both: it opens with
/// `SQLITE_OPEN_READ_ONLY` and never migrates.
///
/// Because it does not migrate, it requires the workspace to already be at
/// exactly `SCHEMA_VERSION` — a read-only connection can't be brought up to the
/// schema the queries assume, and running current-schema SQL against an older DB
/// would error on missing tables/columns. A version mismatch (older *or* newer)
/// is therefore returned as an error, which the read fan-outs treat as "skip
/// this workspace".
///
/// A workspace needing migration is upgraded the next time a write verb opens it
/// via [`open`]. Note the cross-workspace *claim* survey deliberately does NOT
/// simply skip an `open_ro` refusal: claim is a write verb, so it falls back to
/// the migrating [`open`] for a stale workspace (see `registry::claim_any_workspace`),
/// otherwise `claim --all-workspaces` would permanently starve its ready work.
pub fn open_ro(db_path: &Path) -> Result<Connection> {
    if !db_path.is_file() {
        return Err(msg(format!(
            "no workspace database at {}",
            db_path.display()
        )));
    }
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.pragma_update(None, "busy_timeout", 10_000)?;
    let version: i64 = conn
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get::<_, String>(0),
        )
        .map_err(|_| msg("not an ametrite database (missing meta.schema_version)"))?
        .parse()
        .map_err(|_| msg("corrupt meta.schema_version"))?;
    if version != SCHEMA_VERSION {
        return Err(msg(format!(
            "workspace schema v{version} != v{SCHEMA_VERSION}; read-only fan-out skips it (a write verb will migrate it)"
        )));
    }
    Ok(conn)
}

fn migrate(conn: &Connection, mut version: i64) -> Result<()> {
    while version < SCHEMA_VERSION {
        match version {
            1 => {
                // The table swap must not fire FK actions; foreign_keys cannot
                // change inside a transaction, so toggle it around one.
                conn.pragma_update(None, "foreign_keys", "OFF")?;
                let result = conn.execute_batch(&format!("BEGIN;{MIGRATE_V1_V2}COMMIT;"));
                conn.pragma_update(None, "foreign_keys", "ON")?;
                result?;
            }
            2 => conn.execute_batch(&format!("BEGIN;{MIGRATE_V2_V3}COMMIT;"))?,
            3 => conn.execute_batch(&format!("BEGIN;{MIGRATE_V3_V4}COMMIT;"))?,
            v => return Err(msg(format!("no migration path from schema v{v}"))),
        }
        version += 1;
    }
    Ok(())
}

/// On-disk size of the main database file in bytes (`page_count * page_size`),
/// excluding the WAL — the metric `gc` reports shrinking.
fn db_bytes(conn: &Connection) -> Result<i64> {
    let pages: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    Ok(pages * page_size)
}

/// Outcome of `gc`: main-db bytes before/after and WAL frames checkpointed.
pub struct GcReport {
    pub bytes_before: i64,
    pub bytes_after: i64,
    /// WAL frames folded into the main db by the final truncating checkpoint.
    pub wal_frames_checkpointed: i64,
}

/// Compact a workspace database (`amt gc`): optimize the FTS index, run
/// `PRAGMA optimize`, `VACUUM` to reclaim free pages, then a truncating WAL
/// checkpoint. Requires a read-write connection with no open transaction
/// (`VACUUM` cannot run inside one). Returns before/after sizes so the caller
/// can report the space reclaimed.
pub fn gc(conn: &Connection) -> Result<GcReport> {
    let bytes_before = db_bytes(conn)?;
    // Merge the FTS5 b-tree segments so search touches fewer pages.
    conn.execute("INSERT INTO documents_fts(documents_fts) VALUES('optimize')", [])?;
    // Let SQLite refresh stale query-planner statistics.
    conn.execute_batch("PRAGMA optimize;")?;
    // Rewrite the file without free pages (must be outside any transaction).
    conn.execute_batch("VACUUM;")?;
    // Fold the WAL (VACUUM writes a fresh one) back in and truncate it to ~0.
    let wal_frames_checkpointed: i64 = conn.query_row(
        "PRAGMA wal_checkpoint(TRUNCATE)",
        [],
        // columns: (busy, log_frames, checkpointed_frames)
        |r| r.get(2),
    )?;
    let bytes_after = db_bytes(conn)?;
    Ok(GcReport {
        bytes_before,
        bytes_after,
        wal_frames_checkpointed,
    })
}

/// Create a new workspace under `dir/.ametrite/ametrite.db`.
pub fn init(dir: &Path, name: &str, prefix: &str) -> Result<PathBuf> {
    // The prefix becomes part of every issue id (`PREFIX-1`), which flows into
    // URLs and the web UI — restrict it to a safe, id-shaped token so an id can
    // never carry markup/path characters (prevents stored-XSS / route breakage).
    if prefix.is_empty()
        || prefix.len() > 16
        || !prefix.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Err(msg(format!(
            "invalid prefix '{prefix}': use 1-16 ASCII letters/digits (e.g. AMT)"
        )));
    }
    let db_dir = dir.join(DB_DIR);
    let db_path = db_dir.join(DB_FILE);
    if db_path.exists() {
        return Err(msg(format!(
            "workspace already exists at {}",
            db_path.display()
        )));
    }
    std::fs::create_dir_all(&db_dir)?;
    // Self-ignoring: git skips the whole directory without touching the
    // host repo's .gitignore — `amt init` needs no follow-up steps.
    std::fs::write(db_dir.join(".gitignore"), "*\n")?;
    let conn = Connection::open(&db_path)?;
    set_pragmas(&conn)?;
    conn.execute_batch(SCHEMA)?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES
           ('schema_version', ?1), ('workspace_name', ?2), ('id_prefix', ?3)",
        rusqlite::params![SCHEMA_VERSION.to_string(), name, prefix],
    )?;
    Ok(db_path)
}

pub fn id_prefix(conn: &Connection) -> Result<String> {
    Ok(
        conn.query_row("SELECT value FROM meta WHERE key='id_prefix'", [], |r| {
            r.get(0)
        })?,
    )
}

/// Current UTC time as ISO-8601 with millisecond precision, from SQLite
/// (keeps the binary free of a time dependency).
pub fn now(conn: &Connection) -> Result<String> {
    Ok(
        conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
            r.get(0)
        })?,
    )
}
