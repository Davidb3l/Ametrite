//! Hand-rolled MCP (Model Context Protocol) stdio server.
//!
//! Newline-delimited JSON-RPC 2.0. Implements `initialize`, `ping`,
//! `tools/list`, and `tools/call` — the subset a tools-only MCP server needs.

use crate::error::Result;
use crate::store;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const PROTOCOL_VERSION: &str = "2025-06-18";

pub fn serve(mut conn: Connection) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                write_msg(&stdout, &rpc_err(Value::Null, -32700, "parse error"))?;
                continue;
            }
        };
        let id = msg.get("id").filter(|v| !v.is_null()).cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let response = match (method, id) {
            (_, None) => None, // notification — no response
            ("initialize", Some(id)) => {
                let requested = params
                    .get("protocolVersion")
                    .and_then(|v| v.as_str())
                    .unwrap_or(PROTOCOL_VERSION);
                Some(rpc_ok(
                    id,
                    json!({
                        "protocolVersion": requested,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "ametrite", "version": env!("CARGO_PKG_VERSION") }
                    }),
                ))
            }
            ("ping", Some(id)) => Some(rpc_ok(id, json!({}))),
            ("tools/list", Some(id)) => Some(rpc_ok(id, json!({ "tools": tool_defs() }))),
            ("tools/call", Some(id)) => Some(handle_call(&mut conn, id, &params)),
            (_, Some(id)) => Some(rpc_err(id, -32601, "method not found")),
        };
        if let Some(resp) = response {
            write_msg(&stdout, &resp)?;
        }
    }
    Ok(())
}

fn write_msg(stdout: &std::io::Stdout, msg: &Value) -> Result<()> {
    let mut out = stdout.lock();
    writeln!(out, "{msg}")?;
    out.flush()?;
    Ok(())
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_err(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn text_result(id: Value, payload: &impl serde::Serialize) -> Value {
    let text = serde_json::to_string_pretty(payload).unwrap_or_else(|_| "{}".into());
    rpc_ok(id, json!({ "content": [{ "type": "text", "text": text }] }))
}

/// Structured no-work result: the `NoWork` report plus `"claimed": false`.
fn no_work_result(id: Value, nw: &store::NoWork) -> Value {
    let mut v = serde_json::to_value(nw).unwrap_or_else(|_| json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert("claimed".into(), Value::Bool(false));
    }
    text_result(id, &v)
}

fn tool_error(id: Value, message: &str) -> Value {
    rpc_ok(
        id,
        json!({ "content": [{ "type": "text", "text": message }], "isError": true }),
    )
}

fn opt_s(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn opt_i(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(|v| v.as_i64())
}

fn strings(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn agent_of(args: &Value) -> String {
    opt_s(args, "agent")
        .or_else(|| std::env::var("AMT_AGENT").ok())
        .unwrap_or_else(|| "agent".into())
}

fn handle_call(conn: &mut Connection, id: Value, params: &Value) -> Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let req = |key: &str| -> std::result::Result<String, String> {
        opt_s(&args, key).ok_or_else(|| format!("missing required argument '{key}'"))
    };

    macro_rules! try_arg {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(m) => return tool_error(id, &m),
            }
        };
    }
    macro_rules! run {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(e) => return tool_error(id, &e.to_string()),
            }
        };
    }

    match name {
        "create_issue" => {
            let issue = run!(store::create_issue(
                conn,
                store::NewIssue {
                    title: try_arg!(req("title")),
                    body: opt_s(&args, "description").unwrap_or_default(),
                    priority: opt_s(&args, "priority").unwrap_or_else(|| "none".into()),
                    project: opt_s(&args, "project"),
                    labels: strings(&args, "labels"),
                    assignee: opt_s(&args, "assignee"),
                    parent: opt_s(&args, "parent"),
                    due: opt_s(&args, "due"),
                    author: agent_of(&args),
                }
            ));
            text_result(id, &issue)
        }
        "list_issues" => {
            let filter = store::IssueFilter {
                status: opt_s(&args, "status").map(|s| vec![s]).unwrap_or_default(),
                assignee: opt_s(&args, "assignee"),
                project: opt_s(&args, "project"),
                label: opt_s(&args, "label"),
                claimed: args.get("claimed").and_then(|v| v.as_bool()),
                include_closed: args
                    .get("include_closed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                limit: opt_i(&args, "limit").unwrap_or(50),
            };
            let issues = run!(store::list_issues(conn, &filter));
            text_result(id, &issues)
        }
        "get_issue" => text_result(
            id.clone(),
            &run!(store::get_issue(conn, &try_arg!(req("id")))),
        ),
        "claim_next_issue" => {
            let agent = agent_of(&args);
            let ttl = opt_i(&args, "ttl_seconds").unwrap_or(900);
            let cooldown = opt_i(&args, "cooldown_seconds").unwrap_or(3600);
            let peek = args.get("peek").and_then(|v| v.as_bool()).unwrap_or(false);
            let any_ws = args
                .get("any_workspace")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Accept "stages": [..] or a comma-list "from": "todo,backlog".
            let mut stages = strings(&args, "stages");
            if stages.is_empty() {
                if let Some(from) = opt_s(&args, "from") {
                    stages = from
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
            }
            for s in &stages {
                if !crate::model::CLAIMABLE_STATUSES.contains(&s.as_str()) {
                    return tool_error(
                        id,
                        &format!(
                            "invalid stage '{s}' (one of {:?})",
                            crate::model::CLAIMABLE_STATUSES
                        ),
                    );
                }
            }
            let project = opt_s(&args, "project");
            let label = opt_s(&args, "label");
            let filter = store::ClaimFilter {
                stages: if stages.is_empty() {
                    None
                } else {
                    Some(&stages)
                },
                project: project.as_deref(),
                label: label.as_deref(),
            };

            if peek {
                let peeked = if any_ws {
                    run!(crate::registry::peek_any_workspace(&agent, cooldown, &filter))
                        .map(|(ws, issue)| (Some(ws), issue))
                } else {
                    run!(store::peek_next(conn, &agent, cooldown, &filter)).map(|i| (None, i))
                };
                return match peeked {
                    Some((ws, issue)) => {
                        let mut v = serde_json::to_value(&issue).unwrap_or_else(|_| json!({}));
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert("peek".into(), Value::Bool(true));
                            if let Some(ws) = ws {
                                obj.insert("workspace".into(), json!(ws));
                            }
                        }
                        text_result(id, &v)
                    }
                    None if any_ws => {
                        let nw = run!(crate::registry::no_work_any_workspace(&agent, cooldown, &filter));
                        no_work_result(id, &nw)
                    }
                    None => {
                        let nw = run!(store::no_work_reason(conn, &agent, cooldown, &filter));
                        no_work_result(id, &nw)
                    }
                };
            }

            if any_ws {
                let won = run!(crate::registry::claim_any_workspace(&agent, ttl, cooldown, &filter));
                return match won {
                    Some((ws, issue)) => {
                        let mut v = json!(issue);
                        v["workspace"] = json!(ws);
                        text_result(id, &v)
                    }
                    None => {
                        let nw = run!(crate::registry::no_work_any_workspace(&agent, cooldown, &filter));
                        no_work_result(id, &nw)
                    }
                };
            }

            match run!(store::claim_next(conn, &agent, ttl, cooldown, &filter)) {
                Some(issue) => text_result(id, &issue),
                None => {
                    let nw = run!(store::no_work_reason(conn, &agent, cooldown, &filter));
                    no_work_result(id, &nw)
                }
            }
        }
        "claim_issue" => {
            let issue = run!(store::claim_issue(
                conn,
                &try_arg!(req("id")),
                &agent_of(&args),
                opt_i(&args, "ttl_seconds").unwrap_or(900)
            ));
            text_result(id, &issue)
        }
        "release_issue" => {
            let issue = run!(store::release_issue(
                conn,
                &try_arg!(req("id")),
                &agent_of(&args),
                &opt_s(&args, "status").unwrap_or_else(|| "in_review".into()),
                opt_s(&args, "comment").as_deref()
            ));
            text_result(id, &issue)
        }
        "update_issue" => {
            let patch = store::IssuePatch {
                title: opt_s(&args, "title"),
                body: opt_s(&args, "description"),
                status: opt_s(&args, "status"),
                priority: opt_s(&args, "priority"),
                project: args.get("project").map(|v| v.as_str().map(String::from)),
                assignee: args.get("assignee").map(|v| v.as_str().map(String::from)),
                parent: args.get("parent").map(|v| v.as_str().map(String::from)),
                due: args.get("due").map(|v| v.as_str().map(String::from)),
                add_labels: strings(&args, "add_labels"),
                remove_labels: strings(&args, "remove_labels"),
            };
            let issue = run!(store::update_issue(
                conn,
                &try_arg!(req("id")),
                patch,
                &agent_of(&args)
            ));
            text_result(id, &issue)
        }
        "add_comment" => {
            run!(store::add_comment(
                conn,
                &try_arg!(req("id")),
                &agent_of(&args),
                &try_arg!(req("body"))
            ));
            text_result(id, &json!({ "ok": true }))
        }
        "create_note" => {
            let title = try_arg!(req("title"));
            let dedupe = args
                .get("dedupe")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let strict = args
                .get("strict")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let dupes = if dedupe || strict {
                run!(store::find_similar_notes(conn, &title))
            } else {
                Vec::new()
            };
            if strict && !dupes.is_empty() {
                let list = dupes
                    .iter()
                    .map(|d| format!("{} ({:.0}% match) {}", d.id, d.score * 100.0, d.title))
                    .collect::<Vec<_>>()
                    .join("; ");
                return tool_error(
                    id,
                    &format!("refusing to create note: near-duplicate(s) exist: {list}"),
                );
            }
            let doc = run!(store::create_doc(
                conn,
                store::NewNote {
                    title,
                    body: opt_s(&args, "body").unwrap_or_default(),
                    tags: strings(&args, "tags"),
                    doc_type: "note".into(),
                    author: agent_of(&args),
                }
            ));
            if dupes.is_empty() {
                text_result(id, &doc)
            } else {
                let duplicates: Vec<Value> = dupes
                    .iter()
                    .map(|d| json!({ "id": d.id, "title": d.title, "score": d.score }))
                    .collect();
                text_result(
                    id,
                    &json!({
                        "doc": doc,
                        "warning": "near-duplicate note(s) already exist",
                        "duplicates": duplicates,
                    }),
                )
            }
        }
        "append_to_note" => {
            let doc = run!(store::append_to_doc(
                conn,
                &try_arg!(req("id")),
                &try_arg!(req("body")),
                &agent_of(&args)
            ));
            text_result(id, &doc)
        }
        "record_decision" => {
            let decision = run!(store::record_decision(
                conn,
                store::NewDecision {
                    title: try_arg!(req("title")),
                    body: opt_s(&args, "body").unwrap_or_default(),
                    resolves: try_arg!(req("issue")),
                    status: opt_s(&args, "status").unwrap_or_else(|| "accepted".into()),
                    supersedes: opt_s(&args, "supersedes"),
                    author: agent_of(&args),
                }
            ));
            text_result(id, &decision)
        }
        "list_decisions" => {
            let decisions = run!(store::list_decisions(
                conn,
                opt_s(&args, "issue").as_deref(),
                args.get("include_superseded")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            ));
            text_result(id, &decisions)
        }
        "read_doc" => text_result(
            id.clone(),
            &run!(store::get_doc(conn, &try_arg!(req("id")))),
        ),
        "search" => {
            let filter = store::SearchFilter {
                doc_type: opt_s(&args, "type"),
                status: opt_s(&args, "status"),
                tag: opt_s(&args, "tag"),
                project: opt_s(&args, "project"),
                limit: opt_i(&args, "limit").unwrap_or(25),
            };
            let hits = run!(store::search(conn, &try_arg!(req("query")), &filter));
            text_result(id, &hits)
        }
        "get_context" => {
            let key = try_arg!(req("id"));
            let budget = opt_i(&args, "budget");
            text_result(
                id.clone(),
                &run!(store::context_pack(conn, &key, budget)),
            )
        }
        "get_backlinks" => text_result(
            id.clone(),
            &run!(store::backlinks(conn, &try_arg!(req("id")))),
        ),
        _ => rpc_err(id, -32602, &format!("unknown tool '{name}'")),
    }
}

fn tool(name: &str, desc: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": desc,
        "inputSchema": {
            "type": "object",
            "properties": props,
            "required": required
        }
    })
}

fn tool_defs() -> Vec<Value> {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let i = |d: &str| json!({ "type": "integer", "description": d });
    let b = |d: &str| json!({ "type": "boolean", "description": d });
    let arr = |d: &str| json!({ "type": "array", "items": { "type": "string" }, "description": d });
    let status = json!({ "type": "string", "enum": ["backlog","todo","in_progress","in_review","done","canceled"] });
    let priority = json!({ "type": "string", "enum": ["urgent","high","medium","low","none"] });

    vec![
        tool("create_issue", "Create a new issue. Returns the issue with its assigned key (e.g. AMT-7).",
            json!({ "title": s("Issue title"), "description": s("Markdown body; [[wikilinks]] to notes/issues are indexed"),
                    "priority": priority, "project": s("Project slug"), "labels": arr("Labels"),
                    "assignee": s("Assignee"), "parent": s("Parent issue key"), "due": s("Due date YYYY-MM-DD"),
                    "agent": s("Acting agent name") }),
            &["title"]),
        tool("list_issues", "List issues with filters. Excludes done/canceled unless include_closed.",
            json!({ "status": status, "assignee": s("Filter by assignee"), "project": s("Filter by project slug"),
                    "label": s("Filter by label"), "claimed": b("Filter by claimed state"),
                    "include_closed": b("Include done/canceled"), "limit": i("Max results (default 50)") }),
            &[]),
        tool("get_issue", "Get one issue with body, activity log, and backlinks.",
            json!({ "id": s("Issue key, e.g. AMT-7") }), &["id"]),
        tool("claim_next_issue", "Atomically claim the highest-priority claimable issue (todo/backlog, unclaimed or expired lease). Sets status to in_progress with a lease. Race-free across agents. When nothing is claimable, returns {claimed:false} with a reason, counts, and retry_after.",
            json!({ "agent": s("Your agent name (required for meaningful attribution)"),
                    "project": s("Only from this project"), "label": s("Only with this label"),
                    "peek": b("Report the best claimable issue WITHOUT taking a lease or writing activity; result has 'peek': true"),
                    "stages": arr("Restrict claimable stages (subset of ['backlog','todo']); default both"),
                    "from": s("Comma-list alternative to stages, e.g. 'todo,backlog'"),
                    "ttl_seconds": i("Lease duration, default 900. Re-claim to renew before it expires."),
                    "cooldown_seconds": i("Won't re-serve an issue you released within this window (default 3600; 0 disables)"),
                    "any_workspace": b("Claim (or peek) across every registered workspace, globally priority-first; the result includes a 'workspace' field") }),
            &["agent"]),
        tool("claim_issue", "Claim a specific issue, or renew your existing lease (heartbeat).",
            json!({ "id": s("Issue key"), "agent": s("Your agent name"), "ttl_seconds": i("Lease duration, default 900") }),
            &["id", "agent"]),
        tool("release_issue", "Release your claim, setting a final status (default in_review) and optional closing comment.",
            json!({ "id": s("Issue key"), "agent": s("Your agent name"), "status": status, "comment": s("Closing comment") }),
            &["id", "agent"]),
        tool("update_issue", "Update issue fields. Only provided fields change.",
            json!({ "id": s("Issue key"), "title": s("New title"), "description": s("New markdown body"),
                    "status": status, "priority": priority, "project": s("Project slug (empty/null clears)"),
                    "assignee": s("Assignee (null clears)"), "parent": s("Parent key"), "due": s("Due date"),
                    "add_labels": arr("Labels to add"), "remove_labels": arr("Labels to remove"),
                    "agent": s("Acting agent name") }),
            &["id"]),
        tool("add_comment", "Append a comment to an issue's (or note's) activity log.",
            json!({ "id": s("Issue key or doc id"), "body": s("Comment markdown"), "agent": s("Author") }),
            &["id", "body"]),
        tool("create_note", "Create a knowledge-base note. Body wikilinks and #tags are indexed. With dedupe, checks existing note titles for a near-duplicate first: soft mode still creates but returns a 'warning'+'duplicates' field; strict refuses with a tool error listing the collisions.",
            json!({ "title": s("Note title"), "body": s("Markdown body"), "tags": arr("Tags"), "agent": s("Author"),
                    "dedupe": b("Check for a near-duplicate note title; on a hit, still create but include warning+duplicates"),
                    "strict": b("With dedupe: refuse to create (tool error) if a near-duplicate exists") }),
            &["title"]),
        tool("append_to_note", "Append a markdown section to an existing note.",
            json!({ "id": s("Note id or title"), "body": s("Markdown to append"), "agent": s("Author") }),
            &["id", "body"]),
        tool("record_decision", "Record a decision that resolves an issue (ADR-for-agents). Creates a linkable [[D-n]] document, logs it on the issue's activity, and appears in the issue's backlinks. Use whenever you make a non-obvious choice future agents should know about.",
            json!({ "title": s("Decision title, e.g. 'Use SQLite as source of truth'"),
                    "issue": s("Issue key this decision resolves (required)"),
                    "body": s("Markdown: context, the decision, consequences. Wikilinks indexed."),
                    "status": json!({ "type": "string", "enum": ["accepted","proposed"], "description": "Default accepted" }),
                    "supersedes": s("Earlier decision id (D-n) this replaces"),
                    "agent": s("Acting agent name") }),
            &["title", "issue"]),
        tool("list_decisions", "List recorded decisions, optionally for one issue. Superseded decisions hidden unless include_superseded.",
            json!({ "issue": s("Filter by issue key"), "include_superseded": b("Include superseded decisions") }),
            &[]),
        tool("read_doc", "Read any document (issue, note, project, or decision) by id, key, or title.",
            json!({ "id": s("Document id, issue key, or title") }), &["id"]),
        tool("search", "Full-text search (FTS5/BM25) across all issues, notes, and projects. No embeddings — exact terms work best; last term is prefix-matched.",
            json!({ "query": s("Search terms"), "type": json!({ "type": "string", "enum": ["issue","note","project"] }),
                    "status": status, "tag": s("Filter by tag/label"), "project": s("Filter by project"),
                    "limit": i("Max results, default 25") }),
            &["query"]),
        tool("get_context", "One-bundle context read for an issue: the full issue (body, activity, backlinks), the decisions resolving it, the bodies of backlinked docs, and top-k related FTS hits — everything needed to start work in a single call. Use right after claiming. Optional budget caps total serialized chars, dropping lowest-relevance items first (FTS hits, then backlink bodies, then activity) and naming each cut in 'dropped'; the issue body and decisions are never dropped.",
            json!({ "id": s("Issue key, e.g. AMT-7"),
                    "budget": i("Hard cap on total serialized characters; drops lowest-relevance items first") }),
            &["id"]),
        tool("get_backlinks", "List all documents whose bodies link to the given document ([[wikilink]] graph).",
            json!({ "id": s("Document id, issue key, or title") }), &["id"]),
    ]
}
