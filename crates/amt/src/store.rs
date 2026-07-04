use crate::db;
use crate::error::{msg, Result};
use crate::model::*;
use crate::wikilink;
use rusqlite::{params, Connection, OptionalExtension, ToSql, Transaction, TransactionBehavior};
use serde::Serialize;

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

/// Filters that scope what an agent may claim: stage(s) (`--from`, for
/// scoper→builder pipelines), project, and label. `None` fields mean "any".
/// The same-agent requeue cooldown is applied separately (it needs the agent).
#[derive(Default)]
pub struct ClaimFilter<'a> {
    /// Restrict claimable stages. `None`/empty = default (`CLAIMABLE_STATUSES`).
    /// Values are validated by the caller against `CLAIMABLE_STATUSES`.
    pub stages: Option<&'a [String]>,
    pub project: Option<&'a str>,
    pub label: Option<&'a str>,
}

impl ClaimFilter<'_> {
    /// The default filter (any claimable stage, any project/label).
    pub fn any() -> Self {
        Self::default()
    }
}

/// Emit the shared "is this issue claimable right now" predicate into `sql`
/// (leading ` WHERE`), pushing binds into `args` in positional order via bare
/// `?`. Every claim/peek/no-work path funnels through here, so "claimable" is
/// defined in exactly one place.
fn claimable_predicate(
    sql: &mut String,
    args: &mut Vec<Box<dyn ToSql>>,
    now: &str,
    agent: &str,
    cooldown_secs: i64,
    f: &ClaimFilter<'_>,
) {
    let stages: Vec<&str> = match f.stages {
        Some(s) if !s.is_empty() => s.iter().map(|s| s.as_str()).collect(),
        _ => CLAIMABLE_STATUSES.to_vec(),
    };
    let ph = vec!["?"; stages.len()].join(",");
    sql.push_str(&format!(
        " WHERE ((i.status IN ({ph}) AND i.claimed_by IS NULL)
                OR (i.claimed_by IS NOT NULL AND i.claim_expires_at < ?
                    AND i.status NOT IN ('done','canceled')))"
    ));
    for s in &stages {
        args.push(Box::new(s.to_string()));
    }
    args.push(Box::new(now.to_string()));
    if cooldown_secs > 0 {
        // Requeue cooldown: don't re-serve an issue to the agent that just
        // released it (dogfooding finding — a scoping loop was re-claiming
        // its own issue forever). `claim --issue KEY` bypasses this.
        sql.push_str(
            " AND (i.last_released_by IS NULL OR i.last_released_by != ?
                   OR i.last_released_at <= strftime('%Y-%m-%dT%H:%M:%fZ','now','-' || ? || ' seconds'))",
        );
        args.push(Box::new(agent.to_string()));
        args.push(Box::new(cooldown_secs));
    }
    if let Some(p) = f.project {
        sql.push_str(" AND i.project = ?");
        args.push(Box::new(p.to_string()));
    }
    if let Some(l) = f.label {
        sql.push_str(
            " AND EXISTS(SELECT 1 FROM tags t WHERE t.doc_id = d.doc_id AND t.tag = lower(?))",
        );
        args.push(Box::new(l.to_string()));
    }
}

/// The best claimable issue's key, in `claim_next`'s ordering, without taking a
/// lease. Shared by `claim_next`, `peek_next`, and the cross-workspace fan-out.
fn best_claimable_key(
    conn: &Connection,
    now: &str,
    agent: &str,
    cooldown_secs: i64,
    f: &ClaimFilter<'_>,
) -> Result<Option<String>> {
    let mut sql = "SELECT d.id FROM documents d JOIN issues i ON i.doc_id = d.doc_id".to_string();
    let mut args: Vec<Box<dyn ToSql>> = Vec::new();
    claimable_predicate(&mut sql, &mut args, now, agent, cooldown_secs, f);
    sql.push_str(&format!(" ORDER BY {PRIORITY_RANK}, d.created_at LIMIT 1"));
    Ok(conn
        .query_row(
            &sql,
            rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
            |r| r.get(0),
        )
        .optional()?)
}

pub fn claim_next(
    conn: &mut Connection,
    agent: &str,
    ttl_secs: i64,
    cooldown_secs: i64,
    f: &ClaimFilter<'_>,
) -> Result<Option<Issue>> {
    let tx = immediate(conn)?;
    let now = db::now(&tx)?;
    let Some(key) = best_claimable_key(&tx, &now, agent, cooldown_secs, f)? else {
        return Ok(None);
    };
    do_claim(&tx, &key, agent, ttl_secs)?;
    tx.commit()?;
    Ok(Some(load_issue(conn, &key, true)?))
}

/// Read-only: report the best claimable issue WITHOUT taking a lease or writing
/// any activity (`claim --peek`). Returns the full issue, or None when nothing
/// is claimable. The cross-workspace fan-out sorts these by priority then age.
pub fn peek_next(
    conn: &Connection,
    agent: &str,
    cooldown_secs: i64,
    f: &ClaimFilter<'_>,
) -> Result<Option<Issue>> {
    let now = db::now(conn)?;
    let Some(key) = best_claimable_key(conn, &now, agent, cooldown_secs, f)? else {
        return Ok(None);
    };
    Ok(Some(load_issue(conn, &key, false)?))
}

/// Claim `key` iff it *still* satisfies the claimable predicate. Returns
/// `Ok(None)` when the issue was taken, closed, or cooled-down in the race
/// window between peek and claim, so the cross-workspace caller can fall
/// through to its next-best candidate.
pub fn claim_key_guarded(
    conn: &mut Connection,
    key: &str,
    agent: &str,
    ttl_secs: i64,
    cooldown_secs: i64,
    f: &ClaimFilter<'_>,
) -> Result<Option<Issue>> {
    let tx = immediate(conn)?;
    let now = db::now(&tx)?;
    let mut sql = "SELECT d.id FROM documents d JOIN issues i ON i.doc_id = d.doc_id".to_string();
    let mut args: Vec<Box<dyn ToSql>> = Vec::new();
    claimable_predicate(&mut sql, &mut args, &now, agent, cooldown_secs, f);
    sql.push_str(" AND d.id = ? LIMIT 1");
    args.push(Box::new(key.to_string()));
    let found: Option<String> = tx
        .query_row(
            &sql,
            rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
            |r| r.get(0),
        )
        .optional()?;
    let Some(found) = found else {
        return Ok(None);
    };
    do_claim(&tx, &found, agent, ttl_secs)?;
    tx.commit()?;
    Ok(Some(load_issue(conn, &found, true)?))
}

/// Why nothing is claimable, with enough detail for an agent loop to decide
/// whether to back off and for how long (structured `{claimed:false}`).
#[derive(Debug, Serialize)]
pub struct NoWork {
    /// Human-readable one-liner.
    pub reason: String,
    pub counts: NoWorkCounts,
    /// Seconds until the soonest cooldown/lease expiry that would make
    /// something claimable, or null if nothing is ever coming.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<i64>,
}

/// Candidate issues (matching stage + project + label, ignoring lease/cooldown)
/// bucketed by *why* they are not claimable right now.
#[derive(Debug, Serialize)]
pub struct NoWorkCounts {
    /// Held under an unexpired lease by some agent.
    pub blocked_by_lease: i64,
    /// Excluded only by this agent's requeue cooldown.
    pub blocked_by_cooldown: i64,
    /// Total candidates in a claimable stage (regardless of lease/cooldown).
    pub candidates: i64,
}

/// Compute the structured no-work report. Called only when a claim/peek came
/// back empty, so the extra reads are off the hot path.
pub fn no_work_reason(
    conn: &Connection,
    agent: &str,
    cooldown_secs: i64,
    f: &ClaimFilter<'_>,
) -> Result<NoWork> {
    let now = db::now(conn)?;
    let stages: Vec<String> = match f.stages {
        Some(s) if !s.is_empty() => s.to_vec(),
        _ => CLAIMABLE_STATUSES.iter().map(|s| s.to_string()).collect(),
    };
    let ph = vec!["?"; stages.len()].join(",");

    // Base scope: candidates for claiming ignoring lease and cooldown,
    // mirroring the two arms of `claimable_predicate` — (a) in a claimable
    // stage, or (b) currently leased in a non-terminal stage (claimable once
    // the lease expires) — plus any project/label restriction.
    let mut scope = format!(
        "FROM documents d JOIN issues i ON i.doc_id = d.doc_id
         WHERE ((i.status IN ({ph}))
                OR (i.claimed_by IS NOT NULL AND i.status NOT IN ('done','canceled')))"
    );
    let mut scope_args: Vec<Box<dyn ToSql>> = stages
        .iter()
        .map(|s| Box::new(s.clone()) as Box<dyn ToSql>)
        .collect();
    if let Some(p) = f.project {
        scope.push_str(" AND i.project = ?");
        scope_args.push(Box::new(p.to_string()));
    }
    if let Some(l) = f.label {
        scope.push_str(
            " AND EXISTS(SELECT 1 FROM tags t WHERE t.doc_id = d.doc_id AND t.tag = lower(?))",
        );
        scope_args.push(Box::new(l.to_string()));
    }

    let count = |extra: &str, extra_args: &[Box<dyn ToSql>]| -> Result<i64> {
        let sql = format!("SELECT COUNT(*) {scope}{extra}");
        let mut all: Vec<&dyn ToSql> = scope_args.iter().map(|a| a.as_ref()).collect();
        all.extend(extra_args.iter().map(|a| a.as_ref()));
        Ok(conn.query_row(&sql, rusqlite::params_from_iter(all), |r| r.get(0))?)
    };

    let candidates = count("", &[])?;
    let lease_args: Vec<Box<dyn ToSql>> = vec![Box::new(now.clone())];
    let blocked_by_lease = count(
        " AND i.claimed_by IS NOT NULL AND i.claim_expires_at >= ?",
        &lease_args,
    )?;
    // Excluded only by *this agent's* cooldown: unclaimed (or expired lease),
    // released by this agent, still inside the cooldown window.
    let blocked_by_cooldown = if cooldown_secs > 0 {
        let cd_args: Vec<Box<dyn ToSql>> = vec![
            Box::new(now.clone()),
            Box::new(agent.to_string()),
            Box::new(cooldown_secs),
        ];
        count(
            " AND (i.claimed_by IS NULL OR i.claim_expires_at < ?)
              AND i.last_released_by = ?
              AND i.last_released_at > strftime('%Y-%m-%dT%H:%M:%fZ','now','-' || ? || ' seconds')",
            &cd_args,
        )?
    } else {
        0
    };

    let retry_after = soonest_retry(conn, &now, agent, cooldown_secs, &scope, &scope_args)?;

    Ok(NoWork {
        reason: no_work_reason_text(candidates, blocked_by_lease, blocked_by_cooldown),
        counts: NoWorkCounts {
            blocked_by_lease,
            blocked_by_cooldown,
            candidates,
        },
        retry_after,
    })
}

/// The human-readable no-work one-liner for a set of bucket counts. Shared by
/// the single-workspace `no_work_reason` and the cross-workspace aggregator so
/// the CLI and MCP emit identical phrasing for the same situation.
pub fn no_work_reason_text(candidates: i64, blocked_by_lease: i64, blocked_by_cooldown: i64) -> String {
    if candidates == 0 {
        "no candidate issues in a claimable stage".to_string()
    } else if blocked_by_lease > 0 && blocked_by_cooldown > 0 {
        format!("{blocked_by_lease} held by active leases, {blocked_by_cooldown} in your cooldown")
    } else if blocked_by_lease > 0 {
        format!("{blocked_by_lease} candidate(s) held by active leases")
    } else if blocked_by_cooldown > 0 {
        format!("{blocked_by_cooldown} candidate(s) in your requeue cooldown")
    } else {
        "no claimable issues match".to_string()
    }
}

/// Seconds until the soonest lease-expiry or cooldown-expiry within `scope`
/// that would unblock a claim, or None if nothing is pending.
fn soonest_retry(
    conn: &Connection,
    now: &str,
    agent: &str,
    cooldown_secs: i64,
    scope: &str,
    scope_args: &[Box<dyn ToSql>],
) -> Result<Option<i64>> {
    let lease_sql = format!(
        "SELECT MIN(i.claim_expires_at) {scope}
         AND i.claimed_by IS NOT NULL AND i.claim_expires_at >= ?"
    );
    let mut lease_args: Vec<&dyn ToSql> = scope_args.iter().map(|a| a.as_ref()).collect();
    lease_args.push(&now);
    let lease_at: Option<String> = conn
        .query_row(&lease_sql, rusqlite::params_from_iter(lease_args), |r| {
            r.get(0)
        })
        .optional()?
        .flatten();

    let cd_at: Option<String> = if cooldown_secs > 0 {
        let cd_sql = format!(
            "SELECT MIN(i.last_released_at) {scope}
             AND (i.claimed_by IS NULL OR i.claim_expires_at < ?)
             AND i.last_released_by = ?
             AND i.last_released_at > strftime('%Y-%m-%dT%H:%M:%fZ','now','-' || ? || ' seconds')"
        );
        let mut cd_args: Vec<&dyn ToSql> = scope_args.iter().map(|a| a.as_ref()).collect();
        let agent_s = agent.to_string();
        cd_args.push(&now);
        cd_args.push(&agent_s);
        cd_args.push(&cooldown_secs);
        conn.query_row(&cd_sql, rusqlite::params_from_iter(cd_args), |r| r.get(0))
            .optional()?
            .flatten()
    } else {
        None
    };

    let mut best: Option<i64> = None;
    if let Some(exp) = lease_at {
        if let Some(secs) = secs_until(conn, now, &exp, 0)? {
            best = Some(best.map_or(secs, |b: i64| b.min(secs)));
        }
    }
    if let Some(rel) = cd_at {
        if let Some(secs) = secs_until(conn, now, &rel, cooldown_secs)? {
            best = Some(best.map_or(secs, |b: i64| b.min(secs)));
        }
    }
    Ok(best)
}

/// Whole seconds from `now` until (`instant` + `offset_secs`), clamped at 0.
/// Uses SQLite's julianday for the diff to avoid a time crate.
fn secs_until(conn: &Connection, now: &str, instant: &str, offset_secs: i64) -> Result<Option<i64>> {
    let secs: f64 = conn.query_row(
        "SELECT (julianday(?1, '+' || ?2 || ' seconds') - julianday(?3)) * 86400.0",
        params![instant, offset_secs, now],
        |r| r.get(0),
    )?;
    Ok(Some(secs.ceil().max(0.0) as i64))
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
        "UPDATE issues SET claimed_by = NULL, claim_expires_at = NULL, status = ?1,
            last_released_by = ?2, last_released_at = ?3
         WHERE doc_id = ?4",
        params![status, agent, now, doc_id],
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

/// Lowercased alphanumeric tokens of a title, for cheap similarity.
fn norm_tokens(title: &str) -> Vec<String> {
    title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// Jaccard overlap of two token sets: |A∩B| / |A∪B|, in [0.0, 1.0].
fn jaccard(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let sa: std::collections::HashSet<&String> = a.iter().collect();
    let sb: std::collections::HashSet<&String> = b.iter().collect();
    let inter = sa.intersection(&sb).count();
    let union = sa.union(&sb).count();
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

/// A note flagged as a likely duplicate of a candidate title.
pub struct SimilarNote {
    pub id: String,
    pub title: String,
    /// Normalized-title token Jaccard similarity in (0.0, 1.0]; 1.0 = identical
    /// after normalization.
    pub score: f64,
}

/// Minimum normalized-title token Jaccard for two note titles to count as
/// near-duplicates. 0.6 means the titles share ~60%+ of their significant
/// words (e.g. "Auth token rotation" vs "Auth token rotation notes" ≈ 0.75),
/// while distinct titles stay well below it. We use FTS only to cheaply narrow
/// candidates (any shared term), then confirm with this overlap so an
/// incidental one-word FTS hit is never treated as a duplicate. No fuzzy-match
/// crate — pure std token overlap keeps deps at zero.
const DEDUPE_JACCARD_THRESHOLD: f64 = 0.6;

/// Find existing notes whose titles are near-duplicates of `title`.
///
/// Cheap two-stage filter: FTS5 over `title` (restricted to `type='note'`) to
/// pull candidates sharing any term, then a normalized-token Jaccard gate.
/// Returns matches sorted by descending similarity. Shared by the CLI
/// `--dedupe` flag and the MCP `create_note` tool.
pub fn find_similar_notes(conn: &Connection, title: &str) -> Result<Vec<SimilarNote>> {
    let cand_tokens = norm_tokens(title);
    if cand_tokens.is_empty() {
        return Ok(Vec::new());
    }
    // OR the title's tokens so FTS returns any note sharing at least one term
    // (store::search ANDs terms, which would miss a note whose title omits one
    // word of the candidate). We then apply the Jaccard gate below; FTS is only
    // the cheap candidate-narrowing pass.
    let match_expr = cand_tokens
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut stmt = conn.prepare(
        "SELECT d.id, d.title
         FROM documents_fts
         JOIN documents d ON d.doc_id = documents_fts.rowid
         WHERE documents_fts MATCH ?1 AND d.type = 'note'
         ORDER BY bm25(documents_fts) LIMIT 50",
    )?;
    let candidates: Vec<(String, String)> = stmt
        .query_map([match_expr], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let mut out: Vec<SimilarNote> = Vec::new();
    for (cid, ctitle) in candidates {
        let score = jaccard(&cand_tokens, &norm_tokens(&ctitle));
        if score >= DEDUPE_JACCARD_THRESHOLD {
            out.push(SimilarNote {
                id: cid,
                title: ctitle,
                score,
            });
        }
    }
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
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

/// Serialized-char size of a `ContextPack` — the metric `--budget` caps.
fn pack_size(pack: &ContextPack) -> usize {
    serde_json::to_string(pack).map(|s| s.len()).unwrap_or(0)
}

/// One-command context bundle for an issue: the full issue (body + activity +
/// backlinks), the decisions resolving it, the bodies of backlinked docs, and
/// top-k FTS hits for related context — all reusing existing store primitives.
///
/// `budget` (chars) is a hard cap on the serialized pack. When exceeded, whole
/// lowest-relevance items are dropped in order — (1) FTS hits, worst-bm25
/// first; (2) backlink bodies, oldest first; (3) truncate activity — never the
/// issue body or decisions. Each cut is named in `pack.dropped`.
pub fn context_pack(conn: &Connection, key: &str, budget: Option<i64>) -> Result<ContextPack> {
    // Issue with body, activity, and backlinks already populated.
    let issue = get_issue(conn, key)?;

    // Decisions resolving this issue (non-superseded), each with its body.
    let mut decisions = list_decisions(conn, Some(&issue.id), false)?;
    for d in &mut decisions {
        if d.body.is_none() {
            d.body = get_decision(conn, &d.id)?.body;
        }
    }

    // Backlinked docs' bodies. Decisions are surfaced separately above, and the
    // issue links to itself via decision edges, so exclude decisions and the
    // issue itself here. FTS hits below also exclude these ids.
    let mut excluded: std::collections::HashSet<String> = std::collections::HashSet::new();
    excluded.insert(issue.id.clone());
    for d in &decisions {
        excluded.insert(d.id.clone());
    }
    let mut backlink_docs: Vec<BacklinkDoc> = Vec::new();
    for b in &issue.backlinks {
        if b.doc_type == "decision" || b.id == issue.id {
            continue;
        }
        let doc = get_doc(conn, &b.id)?;
        excluded.insert(doc.id.clone());
        backlink_docs.push(BacklinkDoc {
            id: doc.id,
            doc_type: doc.doc_type,
            title: doc.title,
            body: doc.body.unwrap_or_default(),
            updated_at: doc.updated_at,
        });
    }

    // Top-k FTS hits by the issue title, excluding docs already in the pack.
    let query = issue.title.clone();
    let raw_hits = search(
        conn,
        &query,
        &SearchFilter {
            limit: 5 + excluded.len() as i64,
            ..Default::default()
        },
    )?;
    let fts_hits: Vec<SearchHit> = raw_hits
        .into_iter()
        .filter(|h| !excluded.contains(&h.id))
        .take(5)
        .collect();

    let mut pack = ContextPack {
        issue,
        decisions,
        backlink_docs,
        fts_hits,
        budget,
        dropped: Vec::new(),
    };

    if let Some(cap) = budget {
        trim_to_budget(&mut pack, cap);
    }
    Ok(pack)
}

/// Drop whole lowest-relevance items until the pack serializes under `cap`
/// chars, recording each cut in `pack.dropped`. Order: FTS hits worst-bm25
/// first, then backlink bodies oldest first, then truncate activity. Never
/// drops the issue body or decisions.
fn trim_to_budget(pack: &mut ContextPack, cap: i64) {
    let cap = cap.max(0) as usize;

    // (1) FTS hits: search returns them best-first (bm25 ascending), so the
    // worst match is the last element — pop from the end.
    while pack_size(pack) > cap {
        let Some(hit) = pack.fts_hits.pop() else {
            break;
        };
        pack.dropped.push(format!("fts_hit {}", hit.id));
    }

    // (2) Backlink bodies: oldest first (ascending updated_at).
    if pack_size(pack) > cap && !pack.backlink_docs.is_empty() {
        pack.backlink_docs
            .sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        while pack_size(pack) > cap {
            if pack.backlink_docs.is_empty() {
                break;
            }
            let doc = pack.backlink_docs.remove(0);
            pack.dropped.push(format!("backlink {}", doc.id));
        }
    }

    // (3) Truncate activity (oldest entries first) as a last resort. The issue
    // body and decisions are never touched.
    if pack_size(pack) > cap && !pack.issue.activity.is_empty() {
        let before = pack.issue.activity.len();
        while pack_size(pack) > cap && !pack.issue.activity.is_empty() {
            pack.issue.activity.remove(0);
        }
        let cut = before - pack.issue.activity.len();
        if cut > 0 {
            pack.dropped
                .push(format!("activity ({cut} of {before} entries)"));
        }
    }
}

pub fn doctor(conn: &Connection) -> Result<DoctorReport> {
    let now = db::now(conn)?;
    let mut stmt = conn.prepare(
        "SELECT d.id, l.target_raw FROM links l
         JOIN documents d ON d.doc_id = l.source_doc_id
         WHERE l.target_doc_id IS NULL ORDER BY d.id",
    )?;
    // `[[alias:KEY]]` links point into another registered workspace's DB, so
    // they can never resolve locally — don't flag them as broken.
    let registered = crate::registry::load().unwrap_or_default();
    let unresolved_links: Vec<UnresolvedLink> = stmt
        .query_map([], |r| {
            Ok(UnresolvedLink {
                source: r.get(0)?,
                target: r.get(1)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|l| {
            !wikilink::cross_workspace(&l.target)
                .is_some_and(|(alias, _)| registered.contains_key(alias))
        })
        .collect();

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
