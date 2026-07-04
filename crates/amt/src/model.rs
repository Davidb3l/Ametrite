use serde::Serialize;

pub const STATUSES: &[&str] = &[
    "backlog",
    "todo",
    "in_progress",
    "in_review",
    "done",
    "canceled",
];
pub const PRIORITIES: &[&str] = &["urgent", "high", "medium", "low", "none"];
/// Statuses an agent may claim from.
pub const CLAIMABLE_STATUSES: &[&str] = &["todo", "backlog"];

pub fn valid_status(s: &str) -> bool {
    STATUSES.contains(&s)
}
pub fn valid_priority(p: &str) -> bool {
    PRIORITIES.contains(&p)
}

#[derive(Debug, Serialize)]
pub struct Issue {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claimed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub activity: Vec<ActivityEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub backlinks: Vec<DocRef>,
    /// Open (non-terminal) issues blocking this one. Populated on full load.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub blockers: Vec<DocRef>,
    /// Issues this one blocks (its dependents). Populated on full load.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub blocks: Vec<DocRef>,
}

#[derive(Debug, Serialize)]
pub struct Doc {
    pub id: String,
    #[serde(rename = "type")]
    pub doc_type: String,
    pub title: String,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub backlinks: Vec<DocRef>,
}

#[derive(Debug, Serialize)]
pub struct ActivityEntry {
    pub seq: i64,
    pub at: String,
    pub author: String,
    pub kind: String, // "comment" | "event"
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocRef {
    pub id: String,
    #[serde(rename = "type")]
    pub doc_type: String,
    pub title: String,
}

pub const DECISION_STATUSES: &[&str] = &["proposed", "accepted", "superseded"];

#[derive(Debug, Serialize)]
pub struct Decision {
    pub id: String,
    pub title: String,
    /// Issue key this decision resolves (the "why" is one hop from the "what").
    pub resolves: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub id: String,
    #[serde(rename = "type")]
    pub doc_type: String,
    pub title: String,
    pub snippet: String,
    pub score: f64,
}

/// A backlinked document's full body, resolved for a `ContextPack`. Decisions
/// are surfaced separately (in `ContextPack.decisions`), so these are the
/// note/project/issue docs that link to the issue.
#[derive(Debug, Serialize)]
pub struct LinkedDoc {
    pub id: String,
    #[serde(rename = "type")]
    pub doc_type: String,
    pub title: String,
    pub body: String,
    /// Used only to order trimming (oldest backlink bodies drop first); not a
    /// primary field agents read, but cheap to expose.
    pub updated_at: String,
}

/// Everything an agent needs to start work on an issue, as one object:
/// the full issue (body + activity + backlinks), the decisions resolving it,
/// the bodies of backlinked docs, and top-k FTS hits for related context.
///
/// `claim` → `context` = 2 calls to productive work. When `--budget <chars>`
/// is set, the lowest-relevance items are dropped (FTS hits first, then
/// backlink bodies, then activity is truncated — never the issue body or
/// decisions) and `dropped` names what was cut.
#[derive(Debug, Serialize)]
pub struct ContextPack {
    pub issue: Issue,
    pub decisions: Vec<Decision>,
    pub linked_docs: Vec<LinkedDoc>,
    pub fts_hits: Vec<SearchHit>,
    /// Char budget applied, if any (echoed for the agent's benefit).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<i64>,
    /// Human-readable manifest of what budget-trimming removed, in drop order.
    /// Empty when nothing was cut.
    pub dropped: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub unresolved_links: Vec<UnresolvedLink>,
    pub stale_claims: Vec<StaleClaim>,
    pub missing_parents: Vec<MissingRef>,
    pub missing_projects: Vec<MissingRef>,
    pub dangling_decisions: Vec<MissingRef>,
    /// Cycles in the `blocks` dependency graph (each is an issue-key ring that
    /// can never drain, since every member waits on another). R3.
    pub dependency_cycles: Vec<DependencyCycle>,
    pub ok: bool,
}

/// One cycle in the dependency graph: the issue keys forming the ring, in
/// blocker→blocked order (the first key blocks the second, … the last blocks
/// the first).
#[derive(Debug, Serialize)]
pub struct DependencyCycle {
    pub cycle: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct UnresolvedLink {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Serialize)]
pub struct StaleClaim {
    pub id: String,
    pub claimed_by: String,
    pub expired_at: String,
}

#[derive(Debug, Serialize)]
pub struct MissingRef {
    pub id: String,
    pub references: String,
}
