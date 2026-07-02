use crate::db;
use crate::error::{msg, Result};
use crate::model::*;
use crate::wikilink;
use rusqlite::{params, Connection, OptionalExtension, ToSql, Transaction, TransactionBehavior};

const PRIORITY_RANK: &str =
    "CASE i.priority WHEN 'urgent' THEN 0 WHEN 'high' THEN 1 WHEN 'medium' THEN 2 WHEN 'low' THEN 3 ELSE 4 END";

pub struct NewIssue {
    pub title: String,
    pub body: String,
    pub priority: String,
    pub project: Option<String>,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub parent: Option<String>,
    pub due: Option<String>,
    pub author: String,
}

#[derive(Default)]
pub struct IssuePatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    /// `Some(None)` clears the field.
    pub project: Option<Option<String>>,
    pub assignee: Option<Option<String>>,
    pub parent: Option<Option<String>>,
    pub due: Option<Option<String>>,
    pub add_labels: Vec<String>,
    pub remove_labels: Vec<String>,
}

#[derive(Default)]
pub struct IssueFilter {
    pub status: Vec<String>,
    pub assignee: Option<String>,
    pub project: Option<String>,
    pub label: Option<String>,
    pub claimed: Option<bool>,
    pub include_closed: bool,
    pub limit: i64,
}

#[derive(Default)]
pub struct SearchFilter {
    pub doc_type: Option<String>,
    pub status: Option<String>,
    pub tag: Option<String>,
    pub project: Option<String>,
    pub limit: i64,
}

fn immediate(conn: &mut Connection) -> Result<Transaction<'_>> {
    Ok(conn.transaction_with_behavior(TransactionBehavior::Immediate)?)
}

fn doc_id_of(conn: &Connection, id: &str) -> Result<i64> {
    conn.query_row("SELECT doc_id FROM documents WHERE id = ?1", [id], |r| {
        r.get(0)
    })
    .optional()?
    .ok_or_else(|| msg(format!("no document with id '{id}'")))
}

/// Resolve a wikilink target: exact id first (NOCASE via collation), then title.
fn resolve_target(conn: &Connection, raw: &str) -> Result<Option<i64>> {
    if let Some(id) = conn
        .query_row("SELECT doc_id FROM documents WHERE id = ?1", [raw], |r| {
            r.get::<_, i64>(0)
        })
        .optional()?
    {
        return Ok(Some(id));
    }
    Ok(conn
        .query_row(
            "SELECT doc_id FROM documents WHERE lower(title) = lower(?1) LIMIT 1",
            [raw],
            |r| r.get(0),
        )
        .optional()?)
}

/// Re-extract wikilinks and #tags from `body` for `doc_id`.
fn refresh_body_derived(conn: &Connection, doc_id: i64, body: &str) -> Result<()> {
    let extracted = wikilink::extract(body);
    conn.execute("DELETE FROM links WHERE source_doc_id = ?1", [doc_id])?;
    conn.execute(
        "DELETE FROM tags WHERE doc_id = ?1 AND src = 'body'",
        [doc_id],
    )?;
    for raw in &extracted.links {
        let target = resolve_target(conn, raw)?;
        conn.execute(
            "INSERT INTO links(source_doc_id, target_raw, target_doc_id) VALUES (?1, ?2, ?3)",
            params![doc_id, raw, target],
        )?;
    }
    for tag in &extracted.tags {
        conn.execute(
            "INSERT OR IGNORE INTO tags(doc_id, tag, src) VALUES (?1, lower(?2), 'body')",
            params![doc_id, tag],
        )?;
    }
    Ok(())
}

/// Point previously-unresolved links at a newly created/renamed document.
fn resolve_dangling(conn: &Connection, doc_id: i64, id: &str, title: &str) -> Result<()> {
    conn.execute(
        "UPDATE links SET target_doc_id = ?1
         WHERE target_doc_id IS NULL
           AND (lower(target_raw) = lower(?2) OR lower(target_raw) = lower(?3))",
        params![doc_id, id, title],
    )?;
    Ok(())
}

fn append_activity(
    conn: &Connection,
    doc_id: i64,
    author: &str,
    kind: &str,
    body: &str,
) -> Result<()> {
    let at = db::now(conn)?;
    conn.execute(
        "INSERT INTO activity(doc_id, seq, at, author, kind, body)
         VALUES (?1, (SELECT COALESCE(MAX(seq),0)+1 FROM activity WHERE doc_id = ?1), ?2, ?3, ?4, ?5)",
        params![doc_id, at, author, kind, body],
    )?;
    Ok(())
}

fn touch(conn: &Connection, doc_id: i64) -> Result<()> {
    let at = db::now(conn)?;
    conn.execute(
        "UPDATE documents SET updated_at = ?1 WHERE doc_id = ?2",
        params![at, doc_id],
    )?;
    Ok(())
}

fn labels_of(conn: &Connection, doc_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT tag FROM tags WHERE doc_id = ?1 ORDER BY tag")?;
    let rows = stmt.query_map([doc_id], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn backlinks_of(conn: &Connection, doc_id: i64) -> Result<Vec<DocRef>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT d.id, d.type, d.title
         FROM links l JOIN documents d ON d.doc_id = l.source_doc_id
         WHERE l.target_doc_id = ?1 ORDER BY d.id",
    )?;
    let rows = stmt.query_map([doc_id], |r| {
        Ok(DocRef {
            id: r.get(0)?,
            doc_type: r.get(1)?,
            title: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn activity_of(conn: &Connection, doc_id: i64) -> Result<Vec<ActivityEntry>> {
    let mut stmt = conn.prepare(
        "SELECT seq, at, author, kind, body FROM activity WHERE doc_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map([doc_id], |r| {
        Ok(ActivityEntry {
            seq: r.get(0)?,
            at: r.get(1)?,
            author: r.get(2)?,
            kind: r.get(3)?,
            body: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn issue_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, Issue)> {
    let doc_id: i64 = r.get(0)?;
    Ok((
        doc_id,
        Issue {
            id: r.get(1)?,
            title: r.get(2)?,
            status: r.get(3)?,
            priority: r.get(4)?,
            project: r.get(5)?,
            assignee: r.get(6)?,
            parent: r.get(7)?,
            due: r.get(8)?,
            claimed_by: r.get(9)?,
            claim_expires_at: r.get(10)?,
            created_at: r.get(11)?,
            updated_at: r.get(12)?,
            labels: Vec::new(),
            body: None,
            activity: Vec::new(),
            backlinks: Vec::new(),
        },
    ))
}

const ISSUE_COLS: &str = "d.doc_id, d.id, d.title, i.status, i.priority, i.project, i.assignee,
    i.parent_id, i.due, i.claimed_by, i.claim_expires_at, d.created_at, d.updated_at";

fn load_issue(conn: &Connection, key: &str, full: bool) -> Result<Issue> {
    let sql = format!(
        "SELECT {ISSUE_COLS}, d.body FROM documents d JOIN issues i ON i.doc_id = d.doc_id WHERE d.id = ?1"
    );
    let (doc_id, mut issue, body): (i64, Issue, String) = conn
        .query_row(&sql, [key], |r| {
            let (doc_id, issue) = issue_from_row(r)?;
            Ok((doc_id, issue, r.get(13)?))
        })
        .optional()?
        .ok_or_else(|| msg(format!("no issue '{key}'")))?;
    issue.labels = labels_of(conn, doc_id)?;
    if full {
        issue.body = Some(body);
        issue.activity = activity_of(conn, doc_id)?;
        issue.backlinks = backlinks_of(conn, doc_id)?;
    }
    Ok(issue)
}

pub fn create_issue(conn: &mut Connection, ni: NewIssue) -> Result<Issue> {
    if !valid_priority(&ni.priority) {
        return Err(msg(format!(
            "invalid priority '{}' (one of {:?})",
            ni.priority, PRIORITIES
        )));
    }
    let tx = immediate(conn)?;
    if let Some(parent) = &ni.parent {
        doc_id_of(&tx, parent).map_err(|_| msg(format!("parent issue '{parent}' not found")))?;
    }
    let prefix = db::id_prefix(&tx)?;
    let num: i64 = tx.query_row(
        "SELECT COALESCE(MAX(issue_num), 0) + 1 FROM issues",
        [],
        |r| r.get(0),
    )?;
    let key = format!("{prefix}-{num}");
    let at = db::now(&tx)?;
    tx.execute(
        "INSERT INTO documents(id, type, title, body, created_at, updated_at)
         VALUES (?1, 'issue', ?2, ?3, ?4, ?4)",
        params![key, ni.title, ni.body, at],
    )?;
    let doc_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO issues(doc_id, issue_num, status, priority, project, assignee, parent_id, due)
         VALUES (?1, ?2, 'backlog', ?3, ?4, ?5, ?6, ?7)",
        params![
            doc_id,
            num,
            ni.priority,
            ni.project,
            ni.assignee,
            ni.parent,
            ni.due
        ],
    )?;
    for label in &ni.labels {
        tx.execute(
            "INSERT OR IGNORE INTO tags(doc_id, tag, src) VALUES (?1, lower(?2), 'label')",
            params![doc_id, label],
        )?;
    }
    refresh_body_derived(&tx, doc_id, &ni.body)?;
    resolve_dangling(&tx, doc_id, &key, &ni.title)?;
    append_activity(&tx, doc_id, &ni.author, "event", "created")?;
    tx.commit()?;
    load_issue(conn, &key, false)
}

pub fn get_issue(conn: &Connection, key: &str) -> Result<Issue> {
    load_issue(conn, key, true)
}

pub fn list_issues(conn: &Connection, f: &IssueFilter) -> Result<Vec<Issue>> {
    let mut sql = format!(
        "SELECT {ISSUE_COLS} FROM documents d JOIN issues i ON i.doc_id = d.doc_id WHERE 1=1"
    );
    let mut args: Vec<Box<dyn ToSql>> = Vec::new();
    if !f.status.is_empty() {
        let ph = vec!["?"; f.status.len()].join(",");
        sql.push_str(&format!(" AND i.status IN ({ph})"));
        for s in &f.status {
            args.push(Box::new(s.clone()));
        }
    } else if !f.include_closed {
        sql.push_str(" AND i.status NOT IN ('done','canceled')");
    }
    if let Some(a) = &f.assignee {
        sql.push_str(" AND i.assignee = ?");
        args.push(Box::new(a.clone()));
    }
    if let Some(p) = &f.project {
        sql.push_str(" AND i.project = ?");
        args.push(Box::new(p.clone()));
    }
    if let Some(l) = &f.label {
        sql.push_str(
            " AND EXISTS(SELECT 1 FROM tags t WHERE t.doc_id = d.doc_id AND t.tag = lower(?))",
        );
        args.push(Box::new(l.clone()));
    }
    if let Some(claimed) = f.claimed {
        if claimed {
            sql.push_str(" AND i.claimed_by IS NOT NULL");
        } else {
            sql.push_str(" AND i.claimed_by IS NULL");
        }
    }
    sql.push_str(&format!(" ORDER BY {PRIORITY_RANK}, d.created_at LIMIT ?"));
    args.push(Box::new(if f.limit > 0 { f.limit } else { 200 }));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
        issue_from_row,
    )?;
    let mut issues = Vec::new();
    for row in rows {
        let (doc_id, mut issue) = row?;
        issue.labels = labels_of(conn, doc_id)?;
        issues.push(issue);
    }
    Ok(issues)
}

pub fn update_issue(
    conn: &mut Connection,
    key: &str,
    patch: IssuePatch,
    author: &str,
) -> Result<Issue> {
    let tx = immediate(conn)?;
    let before = load_issue(&tx, key, true)?;
    let doc_id = doc_id_of(&tx, &before.id)?;

    if let Some(status) = &patch.status {
        if !valid_status(status) {
            return Err(msg(format!(
                "invalid status '{status}' (one of {STATUSES:?})"
            )));
        }
        if *status != before.status {
            tx.execute(
                "UPDATE issues SET status = ?1 WHERE doc_id = ?2",
                params![status, doc_id],
            )?;
            append_activity(
                &tx,
                doc_id,
                author,
                "event",
                &format!("status: {} → {}", before.status, status),
            )?;
        }
    }
    if let Some(priority) = &patch.priority {
        if !valid_priority(priority) {
            return Err(msg(format!(
                "invalid priority '{priority}' (one of {PRIORITIES:?})"
            )));
        }
        if *priority != before.priority {
            tx.execute(
                "UPDATE issues SET priority = ?1 WHERE doc_id = ?2",
                params![priority, doc_id],
            )?;
            append_activity(
                &tx,
                doc_id,
                author,
                "event",
                &format!("priority: {} → {}", before.priority, priority),
            )?;
        }
    }
    if let Some(assignee) = &patch.assignee {
        tx.execute(
            "UPDATE issues SET assignee = ?1 WHERE doc_id = ?2",
            params![assignee, doc_id],
        )?;
        append_activity(
            &tx,
            doc_id,
            author,
            "event",
            &format!(
                "assignee: {} → {}",
                before.assignee.as_deref().unwrap_or("nobody"),
                assignee.as_deref().unwrap_or("nobody")
            ),
        )?;
    }
    if let Some(project) = &patch.project {
        tx.execute(
            "UPDATE issues SET project = ?1 WHERE doc_id = ?2",
            params![project, doc_id],
        )?;
    }
    if let Some(parent) = &patch.parent {
        if let Some(p) = parent {
            doc_id_of(&tx, p).map_err(|_| msg(format!("parent issue '{p}' not found")))?;
        }
        tx.execute(
            "UPDATE issues SET parent_id = ?1 WHERE doc_id = ?2",
            params![parent, doc_id],
        )?;
    }
    if let Some(due) = &patch.due {
        tx.execute(
            "UPDATE issues SET due = ?1 WHERE doc_id = ?2",
            params![due, doc_id],
        )?;
    }
    if let Some(title) = &patch.title {
        tx.execute(
            "UPDATE documents SET title = ?1 WHERE doc_id = ?2",
            params![title, doc_id],
        )?;
        resolve_dangling(&tx, doc_id, &before.id, title)?;
    }
    if let Some(body) = &patch.body {
        tx.execute(
            "UPDATE documents SET body = ?1 WHERE doc_id = ?2",
            params![body, doc_id],
        )?;
        refresh_body_derived(&tx, doc_id, body)?;
    }
    for label in &patch.add_labels {
        tx.execute(
            "INSERT OR IGNORE INTO tags(doc_id, tag, src) VALUES (?1, lower(?2), 'label')",
            params![doc_id, label],
        )?;
    }
    for label in &patch.remove_labels {
        tx.execute(
            "DELETE FROM tags WHERE doc_id = ?1 AND tag = lower(?2)",
            params![doc_id, label],
        )?;
    }
    touch(&tx, doc_id)?;
    tx.commit()?;
    load_issue(conn, key, false)
}

pub fn add_comment(conn: &mut Connection, id: &str, author: &str, body: &str) -> Result<()> {
    let tx = immediate(conn)?;
    let doc_id = doc_id_of(&tx, id)?;
    append_activity(&tx, doc_id, author, "comment", body)?;
    touch(&tx, doc_id)?;
    tx.commit()?;
    Ok(())
}

/// Atomically claim the best available issue. Returns None when nothing is claimable.
pub fn claim_next(
    conn: &mut Connection,
    agent: &str,
    project: Option<&str>,
    label: Option<&str>,
    ttl_secs: i64,
) -> Result<Option<Issue>> {
    let tx = immediate(conn)?;
    let now = db::now(&tx)?;
    let mut sql = "SELECT d.id FROM documents d JOIN issues i ON i.doc_id = d.doc_id
         WHERE ((i.status IN ('todo','backlog') AND i.claimed_by IS NULL)
                OR (i.claimed_by IS NOT NULL AND i.claim_expires_at < ?1
                    AND i.status NOT IN ('done','canceled')))"
        .to_string();
    let mut args: Vec<Box<dyn ToSql>> = vec![Box::new(now.clone())];
    if let Some(p) = project {
        sql.push_str(" AND i.project = ?");
        args.push(Box::new(p.to_string()));
    }
    if let Some(l) = label {
        sql.push_str(
            " AND EXISTS(SELECT 1 FROM tags t WHERE t.doc_id = d.doc_id AND t.tag = lower(?))",
        );
        args.push(Box::new(l.to_string()));
    }
    sql.push_str(&format!(" ORDER BY {PRIORITY_RANK}, d.created_at LIMIT 1"));
    let key: Option<String> = tx
        .query_row(
            &sql,
            rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
            |r| r.get(0),
        )
        .optional()?;
    let Some(key) = key else {
        return Ok(None);
    };
    do_claim(&tx, &key, agent, ttl_secs)?;
    tx.commit()?;
    Ok(Some(load_issue(conn, &key, true)?))
}

/// Claim (or renew, when already held by `agent`) a specific issue.
pub fn claim_issue(conn: &mut Connection, key: &str, agent: &str, ttl_secs: i64) -> Result<Issue> {
    let tx = immediate(conn)?;
    let now = db::now(&tx)?;
    let issue = load_issue(&tx, key, false)?;
    if let (Some(holder), Some(expires)) = (&issue.claimed_by, &issue.claim_expires_at) {
        if holder != agent && *expires >= now {
            return Err(msg(format!(
                "'{}' is claimed by {holder} until {expires}",
                issue.id
            )));
        }
    }
    do_claim(&tx, &issue.id, agent, ttl_secs)?;
    tx.commit()?;
    load_issue(conn, key, true)
}

fn do_claim(tx: &Transaction<'_>, key: &str, agent: &str, ttl_secs: i64) -> Result<()> {
    let doc_id = doc_id_of(tx, key)?;
    let prev_status: String = tx.query_row(
        "SELECT status FROM issues WHERE doc_id = ?1",
        [doc_id],
        |r| r.get(0),
    )?;
    let prev_holder: Option<String> = tx.query_row(
        "SELECT claimed_by FROM issues WHERE doc_id = ?1",
        [doc_id],
        |r| r.get(0),
    )?;
    tx.execute(
        "UPDATE issues SET claimed_by = ?1, assignee = ?1,
            claim_expires_at = strftime('%Y-%m-%dT%H:%M:%fZ','now', ?2 || ' seconds'),
            status = 'in_progress'
         WHERE doc_id = ?3",
        params![agent, ttl_secs, doc_id],
    )?;
    let event = match prev_holder.as_deref() {
        Some(h) if h == agent => format!("claim renewed (+{ttl_secs}s)"),
        Some(h) => format!("claim taken over from {h} (expired lease, +{ttl_secs}s)"),
        None => format!("claimed (+{ttl_secs}s)"),
    };
    append_activity(tx, doc_id, agent, "event", &event)?;
    if prev_status != "in_progress" {
        append_activity(
            tx,
            doc_id,
            agent,
            "event",
            &format!("status: {prev_status} → in_progress"),
        )?;
    }
    touch(tx, doc_id)?;
    Ok(())
}

pub fn release_issue(
    conn: &mut Connection,
    key: &str,
    agent: &str,
    status: &str,
    comment: Option<&str>,
) -> Result<Issue> {
    if !valid_status(status) {
        return Err(msg(format!(
            "invalid status '{status}' (one of {STATUSES:?})"
        )));
    }
    let tx = immediate(conn)?;
    let now = db::now(&tx)?;
    let issue = load_issue(&tx, key, false)?;
    if let (Some(holder), Some(expires)) = (&issue.claimed_by, &issue.claim_expires_at) {
        if holder != agent && *expires >= now {
            return Err(msg(format!(
                "'{}' is claimed by {holder} until {expires}",
                issue.id
            )));
        }
    }
    let doc_id = doc_id_of(&tx, &issue.id)?;
    tx.execute(
        "UPDATE issues SET claimed_by = NULL, claim_expires_at = NULL, status = ?1 WHERE doc_id = ?2",
        params![status, doc_id],
    )?;
    if let Some(c) = comment {
        append_activity(&tx, doc_id, agent, "comment", c)?;
    }
    append_activity(
        &tx,
        doc_id,
        agent,
        "event",
        &format!("released; status: {} → {status}", issue.status),
    )?;
    touch(&tx, doc_id)?;
    tx.commit()?;
    load_issue(conn, key, false)
}

pub struct NewNote {
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub doc_type: String, // "note" | "project"
    pub author: String,
}

pub fn create_doc(conn: &mut Connection, nn: NewNote) -> Result<Doc> {
    let tx = immediate(conn)?;
    let base = wikilink::slugify(&nn.title);
    let mut id = base.clone();
    let mut n = 1;
    while tx
        .query_row("SELECT 1 FROM documents WHERE id = ?1", [&id], |_| Ok(()))
        .optional()?
        .is_some()
    {
        n += 1;
        id = format!("{base}-{n}");
    }
    let at = db::now(&tx)?;
    tx.execute(
        "INSERT INTO documents(id, type, title, body, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        params![id, nn.doc_type, nn.title, nn.body, at],
    )?;
    let doc_id = tx.last_insert_rowid();
    for tag in &nn.tags {
        tx.execute(
            "INSERT OR IGNORE INTO tags(doc_id, tag, src) VALUES (?1, lower(?2), 'label')",
            params![doc_id, tag],
        )?;
    }
    refresh_body_derived(&tx, doc_id, &nn.body)?;
    resolve_dangling(&tx, doc_id, &id, &nn.title)?;
    append_activity(&tx, doc_id, &nn.author, "event", "created")?;
    tx.commit()?;
    get_doc(conn, &id)
}

pub fn append_to_doc(conn: &mut Connection, id: &str, text: &str, author: &str) -> Result<Doc> {
    let tx = immediate(conn)?;
    let doc_id = doc_id_of(&tx, id)?;
    let body: String = tx.query_row(
        "SELECT body FROM documents WHERE doc_id = ?1",
        [doc_id],
        |r| r.get(0),
    )?;
    let new_body = if body.trim().is_empty() {
        text.to_string()
    } else {
        format!("{}\n\n{}", body.trim_end(), text)
    };
    tx.execute(
        "UPDATE documents SET body = ?1 WHERE doc_id = ?2",
        params![new_body, doc_id],
    )?;
    refresh_body_derived(&tx, doc_id, &new_body)?;
    append_activity(&tx, doc_id, author, "event", "appended content")?;
    touch(&tx, doc_id)?;
    tx.commit()?;
    get_doc(conn, id)
}

pub fn get_doc(conn: &Connection, id_or_title: &str) -> Result<Doc> {
    let row = conn
        .query_row(
            "SELECT doc_id, id, type, title, body, created_at, updated_at
             FROM documents WHERE id = ?1
             UNION ALL
             SELECT doc_id, id, type, title, body, created_at, updated_at
             FROM documents WHERE lower(title) = lower(?1)
             LIMIT 1",
            [id_or_title],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    Doc {
                        id: r.get(1)?,
                        doc_type: r.get(2)?,
                        title: r.get(3)?,
                        body: Some(r.get(4)?),
                        created_at: r.get(5)?,
                        updated_at: r.get(6)?,
                        tags: Vec::new(),
                        backlinks: Vec::new(),
                    },
                ))
            },
        )
        .optional()?;
    let (doc_id, mut doc) = row.ok_or_else(|| msg(format!("no document '{id_or_title}'")))?;
    doc.tags = labels_of(conn, doc_id)?;
    doc.backlinks = backlinks_of(conn, doc_id)?;
    Ok(doc)
}

pub fn list_docs(conn: &Connection, doc_type: &str) -> Result<Vec<Doc>> {
    let mut stmt = conn.prepare(
        "SELECT doc_id, id, type, title, created_at, updated_at
         FROM documents WHERE type = ?1 ORDER BY updated_at DESC",
    )?;
    let rows = stmt.query_map([doc_type], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            Doc {
                id: r.get(1)?,
                doc_type: r.get(2)?,
                title: r.get(3)?,
                created_at: r.get(4)?,
                updated_at: r.get(5)?,
                body: None,
                tags: Vec::new(),
                backlinks: Vec::new(),
            },
        ))
    })?;
    let mut docs = Vec::new();
    for row in rows {
        let (doc_id, mut doc) = row?;
        doc.tags = labels_of(conn, doc_id)?;
        docs.push(doc);
    }
    Ok(docs)
}

pub fn activity(conn: &Connection, id: &str) -> Result<Vec<ActivityEntry>> {
    let doc_id = doc_id_of(conn, id)?;
    activity_of(conn, doc_id)
}

pub fn backlinks(conn: &Connection, id: &str) -> Result<Vec<DocRef>> {
    let doc_id = doc_id_of(conn, id)?;
    backlinks_of(conn, doc_id)
}

/// Quote each whitespace-separated term so user input is never parsed as
/// FTS5 syntax; the final term gets prefix matching.
fn fts_query(q: &str) -> String {
    let terms: Vec<String> = q
        .split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    match terms.split_last() {
        Some((last, rest)) if !rest.is_empty() => {
            format!("{} {last}*", rest.join(" "))
        }
        Some((last, _)) => format!("{last}*"),
        None => String::new(),
    }
}

pub fn search(conn: &Connection, query: &str, f: &SearchFilter) -> Result<Vec<SearchHit>> {
    let match_expr = fts_query(query);
    if match_expr.is_empty() {
        return Ok(Vec::new());
    }
    let mut sql = String::from(
        "SELECT d.id, d.type, d.title,
                snippet(documents_fts, 1, '**', '**', '…', 18),
                bm25(documents_fts)
         FROM documents_fts
         JOIN documents d ON d.doc_id = documents_fts.rowid
         WHERE documents_fts MATCH ?",
    );
    let mut args: Vec<Box<dyn ToSql>> = vec![Box::new(match_expr)];
    if let Some(t) = &f.doc_type {
        sql.push_str(" AND d.type = ?");
        args.push(Box::new(t.clone()));
    }
    if let Some(s) = &f.status {
        sql.push_str(
            " AND EXISTS(SELECT 1 FROM issues i WHERE i.doc_id = d.doc_id AND i.status = ?)",
        );
        args.push(Box::new(s.clone()));
    }
    if let Some(t) = &f.tag {
        sql.push_str(
            " AND EXISTS(SELECT 1 FROM tags t WHERE t.doc_id = d.doc_id AND t.tag = lower(?))",
        );
        args.push(Box::new(t.clone()));
    }
    if let Some(p) = &f.project {
        sql.push_str(
            " AND EXISTS(SELECT 1 FROM issues i WHERE i.doc_id = d.doc_id AND i.project = ?)",
        );
        args.push(Box::new(p.clone()));
    }
    sql.push_str(" ORDER BY bm25(documents_fts) LIMIT ?");
    args.push(Box::new(if f.limit > 0 { f.limit } else { 25 }));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
        |r| {
            Ok(SearchHit {
                id: r.get(0)?,
                doc_type: r.get(1)?,
                title: r.get(2)?,
                snippet: r.get(3)?,
                score: r.get(4)?,
            })
        },
    )?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub struct NewDecision {
    pub title: String,
    pub body: String,
    /// Issue key this decision resolves. Required — a decision without an
    /// issue is just a note.
    pub resolves: String,
    pub status: String,
    pub supersedes: Option<String>,
    pub author: String,
}

const DECISION_COLS: &str = "d.id, d.title, dc.resolves, dc.status, dc.superseded_by,
    d.created_at, d.updated_at";

fn decision_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Decision> {
    Ok(Decision {
        id: r.get(0)?,
        title: r.get(1)?,
        resolves: r.get(2)?,
        status: r.get(3)?,
        superseded_by: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
        body: None,
    })
}

/// Record a decision against an issue (ADR-for-agents). The decision becomes
/// a linkable document ([[D-1]]) and shows up in the issue's backlinks and
/// activity log; optionally supersedes an earlier decision.
pub fn record_decision(conn: &mut Connection, nd: NewDecision) -> Result<Decision> {
    if nd.status != "proposed" && nd.status != "accepted" {
        return Err(msg(format!(
            "invalid decision status '{}' (new decisions are 'proposed' or 'accepted')",
            nd.status
        )));
    }
    let tx = immediate(conn)?;
    let (issue_doc_id, issue_type): (i64, String) = tx
        .query_row(
            "SELECT doc_id, type FROM documents WHERE id = ?1",
            [&nd.resolves],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| msg(format!("issue '{}' not found", nd.resolves)))?;
    if issue_type != "issue" {
        return Err(msg(format!(
            "'{}' is a {issue_type}, decisions must resolve an issue",
            nd.resolves
        )));
    }
    let issue_key: String = tx.query_row(
        "SELECT id FROM documents WHERE doc_id = ?1",
        [issue_doc_id],
        |r| r.get(0),
    )?;

    let num: i64 = tx.query_row(
        "SELECT COALESCE(MAX(decision_num), 0) + 1 FROM decisions",
        [],
        |r| r.get(0),
    )?;
    let id = format!("D-{num}");
    let at = db::now(&tx)?;
    tx.execute(
        "INSERT INTO documents(id, type, title, body, created_at, updated_at)
         VALUES (?1, 'decision', ?2, ?3, ?4, ?4)",
        params![id, nd.title, nd.body, at],
    )?;
    let doc_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO decisions(doc_id, decision_num, resolves, status) VALUES (?1, ?2, ?3, ?4)",
        params![doc_id, num, issue_key, nd.status],
    )?;
    refresh_body_derived(&tx, doc_id, &nd.body)?;
    // Guarantee the graph edge decision → issue even if the body never
    // mentions it, so the issue's backlinks always surface its decisions.
    tx.execute(
        "INSERT INTO links(source_doc_id, target_raw, target_doc_id) VALUES (?1, ?2, ?3)",
        params![doc_id, issue_key, issue_doc_id],
    )?;
    resolve_dangling(&tx, doc_id, &id, &nd.title)?;
    append_activity(&tx, doc_id, &nd.author, "event", "recorded")?;
    append_activity(
        &tx,
        issue_doc_id,
        &nd.author,
        "event",
        &format!("decision recorded: [[{id}]] {}", nd.title),
    )?;
    touch(&tx, issue_doc_id)?;

    if let Some(old_id) = &nd.supersedes {
        let old_doc: Option<(i64, String)> = tx
            .query_row(
                "SELECT d.doc_id, d.id FROM documents d JOIN decisions dc ON dc.doc_id = d.doc_id
                 WHERE d.id = ?1",
                [old_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (old_doc_id, old_key) =
            old_doc.ok_or_else(|| msg(format!("decision '{old_id}' not found")))?;
        tx.execute(
            "UPDATE decisions SET status = 'superseded', superseded_by = ?1 WHERE doc_id = ?2",
            params![id, old_doc_id],
        )?;
        append_activity(
            &tx,
            old_doc_id,
            &nd.author,
            "event",
            &format!("superseded by [[{id}]]"),
        )?;
        touch(&tx, old_doc_id)?;
        // graph edge new decision → superseded decision
        tx.execute(
            "INSERT INTO links(source_doc_id, target_raw, target_doc_id) VALUES (?1, ?2, ?3)",
            params![doc_id, old_key, old_doc_id],
        )?;
    }
    tx.commit()?;
    get_decision(conn, &id)
}

pub fn get_decision(conn: &Connection, id: &str) -> Result<Decision> {
    let sql = format!(
        "SELECT {DECISION_COLS}, d.body FROM documents d JOIN decisions dc ON dc.doc_id = d.doc_id
         WHERE d.id = ?1"
    );
    let mut decision: Decision = conn
        .query_row(&sql, [id], |r| {
            let mut dec = decision_from_row(r)?;
            dec.body = Some(r.get(7)?);
            Ok(dec)
        })
        .optional()?
        .ok_or_else(|| msg(format!("no decision '{id}'")))?;
    if decision
        .body
        .as_deref()
        .is_some_and(|b| b.trim().is_empty())
    {
        decision.body = None;
    }
    Ok(decision)
}

pub fn list_decisions(
    conn: &Connection,
    issue: Option<&str>,
    include_superseded: bool,
) -> Result<Vec<Decision>> {
    let mut sql = format!(
        "SELECT {DECISION_COLS} FROM documents d JOIN decisions dc ON dc.doc_id = d.doc_id WHERE 1=1"
    );
    let mut args: Vec<Box<dyn ToSql>> = Vec::new();
    if let Some(issue) = issue {
        sql.push_str(" AND dc.resolves = ?");
        args.push(Box::new(issue.to_string()));
    }
    if !include_superseded {
        sql.push_str(" AND dc.status != 'superseded'");
    }
    sql.push_str(" ORDER BY dc.decision_num");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
        decision_from_row,
    )?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn doctor(conn: &Connection) -> Result<DoctorReport> {
    let now = db::now(conn)?;
    let mut stmt = conn.prepare(
        "SELECT d.id, l.target_raw FROM links l
         JOIN documents d ON d.doc_id = l.source_doc_id
         WHERE l.target_doc_id IS NULL ORDER BY d.id",
    )?;
    let unresolved_links: Vec<UnresolvedLink> = stmt
        .query_map([], |r| {
            Ok(UnresolvedLink {
                source: r.get(0)?,
                target: r.get(1)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;

    let mut stmt = conn.prepare(
        "SELECT d.id, i.claimed_by, i.claim_expires_at
         FROM issues i JOIN documents d ON d.doc_id = i.doc_id
         WHERE i.claimed_by IS NOT NULL AND i.claim_expires_at < ?1",
    )?;
    let stale_claims: Vec<StaleClaim> = stmt
        .query_map([&now], |r| {
            Ok(StaleClaim {
                id: r.get(0)?,
                claimed_by: r.get(1)?,
                expired_at: r.get(2)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;

    let mut stmt = conn.prepare(
        "SELECT d.id, i.parent_id FROM issues i JOIN documents d ON d.doc_id = i.doc_id
         WHERE i.parent_id IS NOT NULL
           AND NOT EXISTS(SELECT 1 FROM documents p WHERE p.id = i.parent_id)",
    )?;
    let missing_parents: Vec<MissingRef> = stmt
        .query_map([], |r| {
            Ok(MissingRef {
                id: r.get(0)?,
                references: r.get(1)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;

    let mut stmt = conn.prepare(
        "SELECT d.id, i.project FROM issues i JOIN documents d ON d.doc_id = i.doc_id
         WHERE i.project IS NOT NULL
           AND NOT EXISTS(SELECT 1 FROM documents p WHERE p.id = i.project AND p.type = 'project')",
    )?;
    let missing_projects: Vec<MissingRef> = stmt
        .query_map([], |r| {
            Ok(MissingRef {
                id: r.get(0)?,
                references: r.get(1)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;

    let mut stmt = conn.prepare(
        "SELECT d.id, dc.resolves FROM decisions dc JOIN documents d ON d.doc_id = dc.doc_id
         WHERE NOT EXISTS(
           SELECT 1 FROM documents i WHERE i.id = dc.resolves AND i.type = 'issue')",
    )?;
    let dangling_decisions: Vec<MissingRef> = stmt
        .query_map([], |r| {
            Ok(MissingRef {
                id: r.get(0)?,
                references: r.get(1)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;

    let ok = unresolved_links.is_empty()
        && stale_claims.is_empty()
        && missing_parents.is_empty()
        && missing_projects.is_empty()
        && dangling_decisions.is_empty();
    Ok(DoctorReport {
        unresolved_links,
        stale_claims,
        missing_parents,
        missing_projects,
        dangling_decisions,
        ok,
    })
}
