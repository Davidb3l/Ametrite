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

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub unresolved_links: Vec<UnresolvedLink>,
    pub stale_claims: Vec<StaleClaim>,
    pub missing_parents: Vec<MissingRef>,
    pub missing_projects: Vec<MissingRef>,
    pub dangling_decisions: Vec<MissingRef>,
    pub ok: bool,
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
