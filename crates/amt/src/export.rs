//! Obsidian-compatible markdown export/import.
//!
//! Layout: `<dir>/issues/AMT-1-slug.md`, `<dir>/notes/<id>.md`,
//! `<dir>/projects/<id>.md`. Frontmatter is a flat YAML subset (scalars and
//! `[a, b]` lists) that `import` parses back; activity round-trips through an
//! append-only `## Activity` section.

use crate::db;
use crate::error::{msg, Result};
use crate::model::ActivityEntry;
use crate::wikilink::{self, slugify};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

fn yq(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn push_kv(out: &mut String, key: &str, value: Option<&str>, quote: bool) {
    if let Some(v) = value {
        if quote {
            out.push_str(&format!("{key}: {}\n", yq(v)));
        } else {
            out.push_str(&format!("{key}: {v}\n"));
        }
    }
}

pub fn export(conn: &Connection, dir: &Path) -> Result<usize> {
    let mut count = 0;
    let mut stmt = conn.prepare(
        "SELECT doc_id, id, type, title, body, created_at, updated_at FROM documents ORDER BY id",
    )?;
    let rows: Vec<(i64, String, String, String, String, String, String)> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })?
        .collect::<std::result::Result<_, _>>()?;
    for (doc_id, id, doc_type, title, body, created, updated) in rows {
        let mut fm = String::from("---\n");
        push_kv(&mut fm, "id", Some(&id), false);
        push_kv(&mut fm, "type", Some(&doc_type), false);
        push_kv(&mut fm, "title", Some(&title), true);

        if doc_type == "issue" {
            let (status, priority, project, assignee, parent, due, claimed_by, claim_expires_at) =
                conn.query_row(
                    "SELECT status, priority, project, assignee, parent_id, due, claimed_by, claim_expires_at
                     FROM issues WHERE doc_id = ?1",
                    [doc_id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, Option<String>>(3)?,
                            r.get::<_, Option<String>>(4)?,
                            r.get::<_, Option<String>>(5)?,
                            r.get::<_, Option<String>>(6)?,
                            r.get::<_, Option<String>>(7)?,
                        ))
                    },
                )?;
            push_kv(&mut fm, "status", Some(&status), false);
            push_kv(&mut fm, "priority", Some(&priority), false);
            push_kv(&mut fm, "project", project.as_deref(), false);
            push_kv(&mut fm, "assignee", assignee.as_deref(), false);
            push_kv(&mut fm, "parent", parent.as_deref(), false);
            push_kv(&mut fm, "due", due.as_deref(), false);
            push_kv(&mut fm, "claimed_by", claimed_by.as_deref(), false);
            push_kv(
                &mut fm,
                "claim_expires_at",
                claim_expires_at.as_deref(),
                false,
            );
        }

        if doc_type == "decision" {
            let (resolves, status, superseded_by) = conn.query_row(
                "SELECT resolves, status, superseded_by FROM decisions WHERE doc_id = ?1",
                [doc_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )?;
            push_kv(&mut fm, "resolves", Some(&resolves), false);
            push_kv(&mut fm, "status", Some(&status), false);
            push_kv(&mut fm, "superseded_by", superseded_by.as_deref(), false);
        }

        let labels: Vec<String> = {
            let mut s = conn
                .prepare("SELECT tag FROM tags WHERE doc_id = ?1 AND src = 'label' ORDER BY tag")?;
            let v = s.query_map([doc_id], |r| r.get(0))?;
            v.collect::<std::result::Result<_, _>>()?
        };
        if !labels.is_empty() {
            fm.push_str(&format!("labels: [{}]\n", labels.join(", ")));
        }
        push_kv(&mut fm, "created", Some(&created), false);
        push_kv(&mut fm, "updated", Some(&updated), false);
        fm.push_str("---\n\n");

        let mut content = fm;
        content.push_str(body.trim_end());
        content.push('\n');

        let activity = {
            let mut s = conn.prepare(
                "SELECT seq, at, author, kind, body FROM activity WHERE doc_id = ?1 ORDER BY seq",
            )?;
            let v = s.query_map([doc_id], |r| {
                Ok(ActivityEntry {
                    seq: r.get(0)?,
                    at: r.get(1)?,
                    author: r.get(2)?,
                    kind: r.get(3)?,
                    body: r.get(4)?,
                })
            })?;
            v.collect::<std::result::Result<Vec<_>, _>>()?
        };
        if !activity.is_empty() {
            content.push_str("\n## Activity\n");
            for a in &activity {
                if a.kind == "event" {
                    content.push_str(&format!("- {} @{} {}\n", a.at, a.author, a.body));
                } else {
                    // Indent every comment-body line by 4 spaces so none can
                    // start at column 0 with `### @` or `## Activity` and be
                    // re-parsed as a new comment / section header on import
                    // (import dedents to restore the exact text).
                    let indented = a
                        .body
                        .trim_end()
                        .lines()
                        .map(|l| format!("    {l}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    content.push_str(&format!("\n### @{} · {}\n{}\n", a.author, a.at, indented));
                }
            }
        }

        let subdir = match doc_type.as_str() {
            "issue" => "issues",
            "project" => "projects",
            "decision" => "decisions",
            _ => "notes",
        };
        let filename = if doc_type == "issue" || doc_type == "decision" {
            format!("{id}-{}.md", slugify(&title))
        } else {
            format!("{id}.md")
        };
        let out_dir = dir.join(subdir);
        fs::create_dir_all(&out_dir)?;
        fs::write(out_dir.join(filename), content)?;
        count += 1;
    }
    Ok(count)
}

struct Frontmatter {
    fields: BTreeMap<String, String>,
    labels: Vec<String>,
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        v[1..v.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        v.to_string()
    }
}

fn parse_frontmatter(text: &str) -> Option<(Frontmatter, String)> {
    let rest = text.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let (fm_text, body) = (&rest[..end], &rest[end + 4..]);
    let mut fields = BTreeMap::new();
    let mut labels = Vec::new();
    for line in fm_text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if key == "labels" || key == "tags" {
            let inner = value.trim_start_matches('[').trim_end_matches(']');
            labels.extend(
                inner
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
            );
        } else if !value.is_empty() {
            fields.insert(key.to_string(), unquote(value));
        }
    }
    Some((
        Frontmatter { fields, labels },
        body.trim_start_matches('\n').to_string(),
    ))
}

/// Split the body at a trailing `## Activity` section and parse its entries.
fn split_activity(body: &str) -> (String, Vec<(String, String, String, String)>) {
    // The "## Activity" header sits either at the very start of the body (an
    // empty-body doc — parse_frontmatter trims the leading newline) or after
    // real content ("\n## Activity"). `act` is normalized to start AT the
    // header line so the per-line loop below can just skip that one line.
    let (main, act) = if body.starts_with("## Activity") {
        ("", body)
    } else if let Some(pos) = body.find("\n## Activity") {
        (&body[..pos], &body[pos + 1..])
    } else {
        return (body.to_string(), Vec::new());
    };
    let mut entries: Vec<(String, String, String, String)> = Vec::new(); // (at, author, kind, body)
    let mut comment: Option<(String, String, String)> = None; // (at, author, buf)
    let flush = |c: &mut Option<(String, String, String)>,
                 entries: &mut Vec<(String, String, String, String)>| {
        if let Some((at, author, buf)) = c.take() {
            entries.push((at, author, "comment".into(), buf.trim().to_string()));
        }
    };
    for line in act.lines().skip(1) {
        if let Some(rest) = line.strip_prefix("### @") {
            flush(&mut comment, &mut entries);
            if let Some((author, at)) = rest.split_once(" · ") {
                comment = Some((
                    at.trim().to_string(),
                    author.trim().to_string(),
                    String::new(),
                ));
            }
        } else if let Some((_, at_rest)) = line.split_once("- ").filter(|_| comment.is_none()) {
            // "- <ts> @<author> <text>"
            let mut parts = at_rest.splitn(3, ' ');
            if let (Some(at), Some(author), Some(text)) = (parts.next(), parts.next(), parts.next())
            {
                if let Some(author) = author.strip_prefix('@') {
                    entries.push((
                        at.to_string(),
                        author.to_string(),
                        "event".into(),
                        text.to_string(),
                    ));
                }
            }
        } else if let Some((_, _, buf)) = comment.as_mut() {
            // Reverse the export-side 4-space indent (older un-indented exports
            // just pass through unchanged).
            buf.push_str(line.strip_prefix("    ").unwrap_or(line));
            buf.push('\n');
        }
    }
    flush(&mut comment, &mut entries);
    (main.trim_end().to_string(), entries)
}

pub fn import(conn: &mut Connection, dir: &Path) -> Result<usize> {
    let mut files = Vec::new();
    collect_md(dir, &mut files)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut count = 0;
    for file in &files {
        let text = fs::read_to_string(file)?;
        let Some((fm, body)) = parse_frontmatter(&text) else {
            continue;
        };
        let Some(id) = fm.fields.get("id") else {
            continue;
        };
        let doc_type = fm
            .fields
            .get("type")
            .cloned()
            .unwrap_or_else(|| "note".into());
        let title = fm
            .fields
            .get("title")
            .cloned()
            .unwrap_or_else(|| id.clone());
        let (body, activity) = split_activity(&body);
        let now = db::now(&tx)?;
        let created = fm
            .fields
            .get("created")
            .cloned()
            .unwrap_or_else(|| now.clone());
        let updated = fm
            .fields
            .get("updated")
            .cloned()
            .unwrap_or_else(|| now.clone());

        let existing: Option<i64> = tx
            .query_row("SELECT doc_id FROM documents WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        let doc_id = match existing {
            Some(doc_id) => {
                tx.execute(
                    "UPDATE documents SET title=?1, body=?2, created_at=?3, updated_at=?4 WHERE doc_id=?5",
                    params![title, body, created, updated, doc_id],
                )?;
                tx.execute("DELETE FROM activity WHERE doc_id = ?1", [doc_id])?;
                tx.execute(
                    "DELETE FROM tags WHERE doc_id = ?1 AND src = 'label'",
                    [doc_id],
                )?;
                doc_id
            }
            None => {
                tx.execute(
                    "INSERT INTO documents(id, type, title, body, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, doc_type, title, body, created, updated],
                )?;
                tx.last_insert_rowid()
            }
        };

        if doc_type == "issue" {
            let suffix: i64 = id
                .rsplit('-')
                .next()
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| msg(format!("issue id '{id}' has no numeric suffix")))?;
            // issue_num is globally UNIQUE, but the id suffix can collide with an
            // issue of a *different* prefix (cross-prefix import / Obsidian
            // authoring). Keep the id as-is and, on collision, assign the next
            // free number so the whole import batch doesn't roll back.
            let taken: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM issues WHERE issue_num = ?1 AND doc_id != ?2)",
                params![suffix, doc_id],
                |r| r.get(0),
            )?;
            let num: i64 = if taken {
                tx.query_row(
                    "SELECT COALESCE(MAX(issue_num), 0) + 1 FROM issues",
                    [],
                    |r| r.get(0),
                )?
            } else {
                suffix
            };
            let get = |k: &str| fm.fields.get(k).cloned();
            tx.execute(
                "INSERT INTO issues(doc_id, issue_num, status, priority, project, assignee, parent_id, due, claimed_by, claim_expires_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                 ON CONFLICT(doc_id) DO UPDATE SET issue_num=?2, status=?3, priority=?4, project=?5,
                   assignee=?6, parent_id=?7, due=?8, claimed_by=?9, claim_expires_at=?10",
                params![
                    doc_id,
                    num,
                    get("status").unwrap_or_else(|| "backlog".into()),
                    get("priority").unwrap_or_else(|| "none".into()),
                    get("project"),
                    get("assignee"),
                    get("parent"),
                    get("due"),
                    get("claimed_by"),
                    get("claim_expires_at")
                ],
            )?;
        }

        if doc_type == "decision" {
            let num: i64 = id
                .rsplit('-')
                .next()
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| msg(format!("decision id '{id}' has no numeric suffix")))?;
            let get = |k: &str| fm.fields.get(k).cloned();
            tx.execute(
                "INSERT INTO decisions(doc_id, decision_num, resolves, status, superseded_by)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(doc_id) DO UPDATE SET decision_num=?2, resolves=?3, status=?4, superseded_by=?5",
                params![
                    doc_id,
                    num,
                    get("resolves").unwrap_or_default(),
                    get("status").unwrap_or_else(|| "accepted".into()),
                    get("superseded_by")
                ],
            )?;
        }

        for label in &fm.labels {
            tx.execute(
                "INSERT OR IGNORE INTO tags(doc_id, tag, src) VALUES (?1, lower(?2), 'label')",
                params![doc_id, label],
            )?;
        }
        for (seq, (at, author, kind, text)) in activity.iter().enumerate() {
            tx.execute(
                "INSERT INTO activity(doc_id, seq, at, author, kind, body) VALUES (?1,?2,?3,?4,?5,?6)",
                params![doc_id, (seq + 1) as i64, at, author, kind, text],
            )?;
        }
        count += 1;
    }
    // Second pass: rebuild the link graph now that every document exists.
    let ids: Vec<(i64, String)> = {
        let mut stmt = tx.prepare("SELECT doc_id, body FROM documents")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    for (doc_id, body) in ids {
        let extracted = wikilink::extract(&body);
        tx.execute("DELETE FROM links WHERE source_doc_id = ?1", [doc_id])?;
        tx.execute(
            "DELETE FROM tags WHERE doc_id = ?1 AND src = 'body'",
            [doc_id],
        )?;
        for raw in &extracted.links {
            let target: Option<i64> = tx
                .query_row(
                    "SELECT doc_id FROM documents WHERE id = ?1
                     UNION ALL
                     SELECT doc_id FROM documents WHERE lower(title) = lower(?1) LIMIT 1",
                    [raw],
                    |r| r.get(0),
                )
                .optional()?;
            tx.execute(
                "INSERT INTO links(source_doc_id, target_raw, target_doc_id) VALUES (?1, ?2, ?3)",
                params![doc_id, raw, target],
            )?;
        }
        for tag in &extracted.tags {
            tx.execute(
                "INSERT OR IGNORE INTO tags(doc_id, tag, src) VALUES (?1, lower(?2), 'body')",
                params![doc_id, tag],
            )?;
        }
    }
    tx.commit()?;
    Ok(count)
}

fn collect_md(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Err(msg(format!("{} is not a directory", dir.display())));
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            if !path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            {
                collect_md(&path, out)?;
            }
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
    Ok(())
}
