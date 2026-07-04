use crate::error::{msg, Result};
use rusqlite::Connection;
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

/// Create a new workspace under `dir/.ametrite/ametrite.db`.
pub fn init(dir: &Path, name: &str, prefix: &str) -> Result<PathBuf> {
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
