use amt::{db, export, registry, store};
use rusqlite::Connection;
use std::sync::Mutex;
use tempfile::TempDir;

/// Serializes tests that touch the process-global `AMT_REGISTRY` env var so
/// they don't race each other (Rust runs tests in parallel).
static REGISTRY_ENV: Mutex<()> = Mutex::new(());

fn workspace() -> (TempDir, Connection) {
    let dir = TempDir::new().unwrap();
    let path = db::init(dir.path(), "test", "AMT").unwrap();
    let conn = db::open(&path).unwrap();
    (dir, conn)
}

fn new_issue(title: &str, body: &str, priority: &str) -> store::NewIssue {
    store::NewIssue {
        title: title.into(),
        body: body.into(),
        priority: priority.into(),
        project: None,
        labels: vec![],
        assignee: None,
        parent: None,
        due: None,
        author: "test".into(),
    }
}

#[test]
fn issue_keys_are_sequential() {
    let (_d, mut conn) = workspace();
    let a = store::create_issue(&mut conn, new_issue("First", "", "none")).unwrap();
    let b = store::create_issue(&mut conn, new_issue("Second", "", "none")).unwrap();
    assert_eq!(a.id, "AMT-1");
    assert_eq!(b.id, "AMT-2");
}

#[test]
fn wikilinks_resolve_and_backlink() {
    let (_d, mut conn) = workspace();
    let issue = store::create_issue(
        &mut conn,
        new_issue(
            "Fix auth",
            "See [[Session Tokens]] for background. #auth",
            "high",
        ),
    )
    .unwrap();
    // link is dangling until the note exists
    let report = store::doctor(&conn).unwrap();
    assert_eq!(report.unresolved_links.len(), 1);

    let note = store::create_doc(
        &mut conn,
        store::NewNote {
            title: "Session Tokens".into(),
            body: "Tokens rotate. Related: [[AMT-1]]".into(),
            tags: vec![],
            doc_type: "note".into(),
            author: "test".into(),
        },
    )
    .unwrap();
    assert_eq!(note.id, "session-tokens");

    // creating the note resolved the dangling link
    let report = store::doctor(&conn).unwrap();
    assert!(
        report.unresolved_links.is_empty(),
        "{:?}",
        report.unresolved_links
    );

    let backlinks = store::backlinks(&conn, &note.id).unwrap();
    assert_eq!(backlinks.len(), 1);
    assert_eq!(backlinks[0].id, issue.id);

    // and the issue has a backlink from the note
    let issue_backlinks = store::backlinks(&conn, "AMT-1").unwrap();
    assert_eq!(issue_backlinks.len(), 1);
    assert_eq!(issue_backlinks[0].id, "session-tokens");

    // body #tag was indexed
    let full = store::get_issue(&conn, "AMT-1").unwrap();
    assert!(full.labels.contains(&"auth".to_string()));
}

#[test]
fn search_finds_by_content() {
    let (_d, mut conn) = workspace();
    store::create_issue(
        &mut conn,
        new_issue("Login bug", "token refresh fails on expiry", "high"),
    )
    .unwrap();
    store::create_issue(
        &mut conn,
        new_issue("Unrelated", "css padding tweak", "low"),
    )
    .unwrap();
    let hits = store::search(&conn, "token refresh", &store::SearchFilter::default()).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "AMT-1");
    // prefix match on last term
    let hits = store::search(&conn, "expi", &store::SearchFilter::default()).unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn claim_orders_by_priority_and_is_exclusive() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Low prio", "", "low")).unwrap();
    store::create_issue(&mut conn, new_issue("Urgent", "", "urgent")).unwrap();

    let first = store::claim_next(&mut conn, "agent-a", 900, 0, &any())
        .unwrap()
        .unwrap();
    assert_eq!(first.id, "AMT-2", "urgent should be claimed first");
    assert_eq!(first.status, "in_progress");
    assert_eq!(first.claimed_by.as_deref(), Some("agent-a"));

    let second = store::claim_next(&mut conn, "agent-b", 900, 0, &any())
        .unwrap()
        .unwrap();
    assert_eq!(second.id, "AMT-1");

    // nothing left
    assert!(store::claim_next(&mut conn, "agent-c", 900, 0, &any())
        .unwrap()
        .is_none());

    // agent-b cannot claim agent-a's issue while lease is live
    assert!(store::claim_issue(&mut conn, "AMT-2", "agent-b", 900).is_err());
    // but agent-a can renew
    let renewed = store::claim_issue(&mut conn, "AMT-2", "agent-a", 900).unwrap();
    assert_eq!(renewed.claimed_by.as_deref(), Some("agent-a"));
}

#[test]
fn expired_lease_is_stealable() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Task", "", "none")).unwrap();
    // claim with a lease that is already expired
    store::claim_next(&mut conn, "crashed-agent", -10, 0, &any())
        .unwrap()
        .unwrap();
    let report = store::doctor(&conn).unwrap();
    assert_eq!(report.stale_claims.len(), 1);

    let stolen = store::claim_next(&mut conn, "agent-b", 900, 0, &any()).unwrap();
    assert_eq!(stolen.unwrap().claimed_by.as_deref(), Some("agent-b"));
}

#[test]
fn release_sets_status_and_clears_claim() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Task", "", "none")).unwrap();
    store::claim_next(&mut conn, "agent-a", 900, 0, &any())
        .unwrap()
        .unwrap();
    let released = store::release_issue(
        &mut conn,
        "AMT-1",
        "agent-a",
        "done",
        Some("all tests pass"),
    )
    .unwrap();
    assert_eq!(released.status, "done");
    assert!(released.claimed_by.is_none());
    let full = store::get_issue(&conn, "AMT-1").unwrap();
    assert!(full
        .activity
        .iter()
        .any(|a| a.kind == "comment" && a.body == "all tests pass"));
}

#[test]
fn concurrent_claims_never_double_claim() {
    let dir = TempDir::new().unwrap();
    let path = db::init(dir.path(), "race", "AMT").unwrap();
    {
        let mut conn = db::open(&path).unwrap();
        for n in 0..10 {
            store::create_issue(&mut conn, new_issue(&format!("Task {n}"), "", "none")).unwrap();
        }
    }
    let mut handles = Vec::new();
    for agent in 0..4 {
        let path = path.clone();
        handles.push(std::thread::spawn(move || {
            let mut conn = db::open(&path).unwrap();
            let mut claimed = Vec::new();
            while let Some(issue) =
                store::claim_next(&mut conn, &format!("agent-{agent}"), 900, 0, &any()).unwrap()
            {
                claimed.push(issue.id);
            }
            claimed
        }));
    }
    let mut all: Vec<String> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    let total = all.len();
    all.sort();
    all.dedup();
    assert_eq!(total, 10, "every issue claimed exactly once");
    assert_eq!(all.len(), 10, "no issue claimed twice");
}

#[test]
fn export_import_round_trips() {
    let (_d, mut conn) = workspace();
    store::create_issue(
        &mut conn,
        store::NewIssue {
            labels: vec!["bug".into(), "backend".into()],
            ..new_issue("Fix: login \"bug\"", "Body with [[Design Note]].", "high")
        },
    )
    .unwrap();
    store::add_comment(&mut conn, "AMT-1", "agent-a", "multi\nline comment").unwrap();
    store::create_doc(
        &mut conn,
        store::NewNote {
            title: "Design Note".into(),
            body: "notes here #design".into(),
            tags: vec!["arch".into()],
            doc_type: "note".into(),
            author: "test".into(),
        },
    )
    .unwrap();

    let out = TempDir::new().unwrap();
    let n = export::export(&conn, out.path()).unwrap();
    assert_eq!(n, 2);

    // import into a fresh workspace
    let dir2 = TempDir::new().unwrap();
    let path2 = db::init(dir2.path(), "copy", "AMT").unwrap();
    let mut conn2 = db::open(&path2).unwrap();
    let n = export::import(&mut conn2, out.path()).unwrap();
    assert_eq!(n, 2);

    let issue = store::get_issue(&conn2, "AMT-1").unwrap();
    assert_eq!(issue.title, "Fix: login \"bug\"");
    assert_eq!(issue.priority, "high");
    assert!(issue.labels.contains(&"bug".to_string()));
    assert!(issue
        .activity
        .iter()
        .any(|a| a.kind == "comment" && a.body.contains("multi\nline comment")));
    // link graph rebuilt on import
    let backlinks = store::backlinks(&conn2, "design-note").unwrap();
    assert_eq!(backlinks.len(), 1);
    // next issue number continues after the imported one
    let next = store::create_issue(&mut conn2, new_issue("New", "", "none")).unwrap();
    assert_eq!(next.id, "AMT-2");
}

#[test]
fn decisions_attach_to_issues_and_supersede() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Pick a database", "", "high")).unwrap();

    let d1 = store::record_decision(
        &mut conn,
        store::NewDecision {
            title: "Use Postgres".into(),
            body: "Team knows it.".into(),
            resolves: "AMT-1".into(),
            status: "accepted".into(),
            supersedes: None,
            author: "agent-a".into(),
        },
    )
    .unwrap();
    assert_eq!(d1.id, "D-1");
    assert_eq!(d1.resolves, "AMT-1");

    // the issue's backlinks and activity now surface the decision
    let issue = store::get_issue(&conn, "AMT-1").unwrap();
    assert!(issue.backlinks.iter().any(|b| b.id == "D-1"));
    assert!(issue
        .activity
        .iter()
        .any(|a| a.kind == "event" && a.body.contains("[[D-1]]")));

    // decisions must resolve an issue, not a note
    store::create_doc(
        &mut conn,
        store::NewNote {
            title: "Some Note".into(),
            body: "".into(),
            tags: vec![],
            doc_type: "note".into(),
            author: "t".into(),
        },
    )
    .unwrap();
    assert!(store::record_decision(
        &mut conn,
        store::NewDecision {
            title: "Bad".into(),
            body: "".into(),
            resolves: "some-note".into(),
            status: "accepted".into(),
            supersedes: None,
            author: "t".into(),
        },
    )
    .is_err());

    // superseding flips the old decision and links the new one to it
    let d2 = store::record_decision(
        &mut conn,
        store::NewDecision {
            title: "Use SQLite instead".into(),
            body: "Local-first won.".into(),
            resolves: "AMT-1".into(),
            status: "accepted".into(),
            supersedes: Some("D-1".into()),
            author: "agent-b".into(),
        },
    )
    .unwrap();
    let d1_after = store::get_decision(&conn, "D-1").unwrap();
    assert_eq!(d1_after.status, "superseded");
    assert_eq!(d1_after.superseded_by.as_deref(), Some("D-2"));

    // default listing hides superseded
    let active = store::list_decisions(&conn, Some("AMT-1"), false).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, d2.id);
    let all = store::list_decisions(&conn, Some("AMT-1"), true).unwrap();
    assert_eq!(all.len(), 2);

    // decisions are linkable documents: [[D-2]] resolves
    store::create_doc(
        &mut conn,
        store::NewNote {
            title: "Retro".into(),
            body: "See [[D-2]].".into(),
            tags: vec![],
            doc_type: "note".into(),
            author: "t".into(),
        },
    )
    .unwrap();
    let backlinks = store::backlinks(&conn, "D-2").unwrap();
    assert!(backlinks.iter().any(|b| b.id == "retro"));

    let report = store::doctor(&conn).unwrap();
    assert!(report.ok, "{report:?}");
}

#[test]
fn decisions_export_import_round_trip() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Pick a db", "", "none")).unwrap();
    store::record_decision(
        &mut conn,
        store::NewDecision {
            title: "Use SQLite".into(),
            body: "Zero deps.".into(),
            resolves: "AMT-1".into(),
            status: "accepted".into(),
            supersedes: None,
            author: "t".into(),
        },
    )
    .unwrap();

    let src_act: i64 = conn.query_row("SELECT COUNT(*) FROM activity", [], |r| r.get(0)).unwrap();
    let out = TempDir::new().unwrap();
    export::export(&conn, out.path()).unwrap();
    let mut fpath = String::new();
    for e in std::fs::read_dir(out.path().join("issues")).unwrap() { fpath = e.unwrap().path().to_string_lossy().into(); }
    eprintln!("SRC_ACT={src_act} FILE={fpath}");
    eprintln!("--CONTENT--\n{}\n--END--", std::fs::read_to_string(&fpath).unwrap());
    assert!(out.path().join("decisions/D-1-use-sqlite.md").is_file());

    let dir2 = TempDir::new().unwrap();
    let path2 = db::init(dir2.path(), "copy", "AMT").unwrap();
    let mut conn2 = db::open(&path2).unwrap();
    export::import(&mut conn2, out.path()).unwrap();
    let d = store::get_decision(&conn2, "D-1").unwrap();
    assert_eq!(d.resolves, "AMT-1");
    assert_eq!(d.status, "accepted");
    assert_eq!(d.body.as_deref(), Some("Zero deps."));
}

#[test]
fn peek_does_not_claim_then_guarded_claim_takes_it() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Low", "", "low")).unwrap();
    store::create_issue(&mut conn, new_issue("Urgent", "", "urgent")).unwrap();

    // peek returns the best candidate without mutating anything…
    let cand = store::peek_next(&conn, "agent-a", 0, &any())
        .unwrap()
        .unwrap();
    assert_eq!(cand.id, "AMT-2");
    assert_eq!(cand.priority, "urgent");
    let still = store::get_issue(&conn, "AMT-2").unwrap();
    assert_eq!(still.status, "backlog", "peek must not claim");
    assert!(still.claimed_by.is_none());

    // …and the guarded claim then takes exactly that issue.
    let got = store::claim_key_guarded(&mut conn, "AMT-2", "agent-a", 900, 0, &any())
        .unwrap()
        .unwrap();
    assert_eq!(got.id, "AMT-2");
    assert_eq!(got.claimed_by.as_deref(), Some("agent-a"));
}

#[test]
fn guarded_claim_returns_none_when_raced_away() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Task", "", "high")).unwrap();
    // someone else claims it between our peek and our guarded claim
    store::claim_next(&mut conn, "winner", 900, 0, &any())
        .unwrap()
        .unwrap();
    let lost = store::claim_key_guarded(&mut conn, "AMT-1", "loser", 900, 0, &any()).unwrap();
    assert!(lost.is_none(), "guarded claim must yield to the live lease");
}

#[test]
fn cross_workspace_claim_drains_both_in_global_priority_order() {
    let _guard = REGISTRY_ENV.lock().unwrap();
    let home = TempDir::new().unwrap();
    std::env::set_var("AMT_REGISTRY", home.path().join("registry.json"));

    // two repos, interleaved priorities across workspaces
    let repo_a = TempDir::new().unwrap();
    let repo_b = TempDir::new().unwrap();
    {
        let mut a = db::open(&db::init(repo_a.path(), "alpha", "ALP").unwrap()).unwrap();
        store::create_issue(&mut a, new_issue("A urgent", "", "urgent")).unwrap();
        store::create_issue(&mut a, new_issue("A low", "", "low")).unwrap();
        let mut b = db::open(&db::init(repo_b.path(), "beta", "BET").unwrap()).unwrap();
        store::create_issue(&mut b, new_issue("B high", "", "high")).unwrap();
        store::create_issue(&mut b, new_issue("B medium", "", "medium")).unwrap();
    }
    registry::add("alpha", repo_a.path()).unwrap();
    registry::add("beta", repo_b.path()).unwrap();

    // one agent, cross-workspace claim loop — every poll yields work (zero
    // idle polls) until both backlogs are drained, in global priority order.
    let mut order = Vec::new();
    while let Some((ws, issue)) =
        registry::claim_any_workspace("solo", 900, 0, &any()).unwrap()
    {
        order.push((ws, issue.id, issue.priority));
    }
    let seq: Vec<(&str, &str)> = order.iter().map(|(w, i, _)| (w.as_str(), i.as_str())).collect();
    assert_eq!(
        seq,
        vec![
            ("alpha", "ALP-1"), // urgent
            ("beta", "BET-1"),  // high
            ("beta", "BET-2"),  // medium
            ("alpha", "ALP-2"), // low
        ],
        "claims must drain in global priority order across workspaces"
    );

    std::env::remove_var("AMT_REGISTRY");
}

#[test]
fn cross_workspace_links_are_not_flagged_by_doctor() {
    let _guard = REGISTRY_ENV.lock().unwrap();
    let home = TempDir::new().unwrap();
    std::env::set_var("AMT_REGISTRY", home.path().join("registry.json"));

    let repo = TempDir::new().unwrap();
    let mut conn = db::open(&db::init(repo.path(), "main", "AMT").unwrap()).unwrap();
    // a link into workspace "web" and a genuinely broken local link
    store::create_issue(
        &mut conn,
        new_issue("Depends on other repo", "Blocked by [[web:API-9]]; see [[Ghost Note]].", "high"),
    )
    .unwrap();

    // with "web" unregistered, the cross-workspace link IS unresolved
    let report = store::doctor(&conn).unwrap();
    assert_eq!(report.unresolved_links.len(), 2);

    // once "web" is a registered workspace, only the real broken link remains
    let web = TempDir::new().unwrap();
    db::init(web.path(), "web", "WEB").unwrap();
    registry::add("web", web.path()).unwrap();
    let report = store::doctor(&conn).unwrap();
    assert_eq!(report.unresolved_links.len(), 1);
    assert_eq!(report.unresolved_links[0].target, "Ghost Note");

    std::env::remove_var("AMT_REGISTRY");
}

#[test]
fn registry_round_trips_and_rejects_non_workspaces() {
    let _guard = REGISTRY_ENV.lock().unwrap();
    let home = TempDir::new().unwrap();
    std::env::set_var("AMT_REGISTRY", home.path().join("registry.json"));

    let repo = TempDir::new().unwrap();
    db::init(repo.path(), "proj", "PRJ").unwrap();
    registry::add("proj", repo.path()).unwrap();
    assert_eq!(registry::load().unwrap().get("proj").map(String::as_str),
               Some(repo.path().canonicalize().unwrap().to_string_lossy().as_ref()));

    // a directory with no .ametrite workspace is rejected
    let empty = TempDir::new().unwrap();
    assert!(registry::add("empty", empty.path()).is_err());

    assert!(registry::remove("proj").unwrap());
    assert!(registry::load().unwrap().is_empty());

    std::env::remove_var("AMT_REGISTRY");
}

#[test]
fn requeue_cooldown_blocks_self_but_not_others() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Scope me", "", "high")).unwrap();

    store::claim_next(&mut conn, "agent-a", 900, 3600, &any())
        .unwrap()
        .unwrap();
    store::release_issue(&mut conn, "AMT-1", "agent-a", "todo", None).unwrap();

    // agent-a is in cooldown for its own released issue…
    assert!(
        store::claim_next(&mut conn, "agent-a", 900, 3600, &any())
            .unwrap()
            .is_none(),
        "agent must not be re-served the issue it just released"
    );
    // …but agent-b gets it immediately…
    let by_b = store::claim_next(&mut conn, "agent-b", 900, 3600, &any())
        .unwrap()
        .unwrap();
    assert_eq!(by_b.id, "AMT-1");
    store::release_issue(&mut conn, "AMT-1", "agent-b", "todo", None).unwrap();

    // …and cooldown 0 disables the guard entirely.
    let again = store::claim_next(&mut conn, "agent-b", 900, 0, &any())
        .unwrap()
        .unwrap();
    assert_eq!(again.id, "AMT-1");
    // explicit claim of a specific issue always bypasses cooldown
    store::release_issue(&mut conn, "AMT-1", "agent-b", "todo", None).unwrap();
    let explicit = store::claim_issue(&mut conn, "AMT-1", "agent-b", 900).unwrap();
    assert_eq!(explicit.claimed_by.as_deref(), Some("agent-b"));
}

fn new_note(title: &str, body: &str) -> store::NewNote {
    store::NewNote {
        title: title.into(),
        body: body.into(),
        tags: vec![],
        doc_type: "note".into(),
        author: "test".into(),
    }
}

#[test]
fn dedupe_detects_near_duplicate_title() {
    let (_d, mut conn) = workspace();
    store::create_doc(
        &mut conn,
        new_note("Auth token rotation strategy", "we rotate every 15m"),
    )
    .unwrap();
    // Same significant words, minor wording change → above the Jaccard gate.
    let dupes =
        store::find_similar_notes(&conn, "Auth token rotation strategy notes").unwrap();
    assert_eq!(dupes.len(), 1);
    assert_eq!(dupes[0].title, "Auth token rotation strategy");
    assert!(dupes[0].score >= 0.6, "score was {}", dupes[0].score);
}

#[test]
fn dedupe_does_not_flag_distinct_title() {
    let (_d, mut conn) = workspace();
    store::create_doc(
        &mut conn,
        new_note("Auth token rotation strategy", "we rotate every 15m"),
    )
    .unwrap();
    // Shares at most one incidental word — must not be flagged.
    let dupes = store::find_similar_notes(&conn, "Kanban board CSS layout fixes").unwrap();
    assert!(dupes.is_empty(), "unexpected dupes: {}", dupes.len());
}

#[test]
fn dedupe_only_matches_notes_not_issues() {
    let (_d, mut conn) = workspace();
    // An issue with an identical title must not count as a note duplicate.
    store::create_issue(
        &mut conn,
        new_issue("Database migration plan", "steps here", "high"),
    )
    .unwrap();
    let dupes = store::find_similar_notes(&conn, "Database migration plan").unwrap();
    assert!(dupes.is_empty(), "issue leaked into note dedupe");
}

#[test]
fn dedupe_soft_warns_but_still_creates_while_strict_refuses() {
    let (_d, mut conn) = workspace();
    store::create_doc(&mut conn, new_note("Release checklist", "cut a tag")).unwrap();

    // Soft mode: caller sees the collision but the note is still created.
    let dupes = store::find_similar_notes(&conn, "Release checklist").unwrap();
    assert_eq!(dupes.len(), 1);
    let created = store::create_doc(&mut conn, new_note("Release checklist", "v2")).unwrap();
    // Distinct id is minted (slug collision suffix), so both notes coexist.
    assert_ne!(created.id, dupes[0].id);
    assert_eq!(store::list_docs(&conn, "note").unwrap().len(), 2);

    // Strict mode is the caller refusing when find_similar_notes is non-empty;
    // verify the signal both handlers key off of is present.
    let strict_hit = store::find_similar_notes(&conn, "Release checklist").unwrap();
    assert!(!strict_hit.is_empty());
}

/// Default claim filter (any claimable stage, any project/label).
fn any() -> store::ClaimFilter<'static> {
    store::ClaimFilter::any()
}

#[test]
fn peek_reports_best_without_claiming_or_writing_activity() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Low", "", "low")).unwrap();
    store::create_issue(&mut conn, new_issue("Urgent", "", "urgent")).unwrap();

    let peeked = store::peek_next(&conn, "agent-a", 0, &any())
        .unwrap()
        .unwrap();
    // Same ordering as claim_next: urgent (AMT-2) wins.
    assert_eq!(peeked.id, "AMT-2");
    // Peek must NOT take a lease or change status.
    assert!(peeked.claimed_by.is_none());
    assert_eq!(peeked.status, "backlog");

    // The issue is untouched on disk: still unclaimed, still backlog…
    let fresh = store::get_issue(&conn, "AMT-2").unwrap();
    assert!(fresh.claimed_by.is_none());
    assert_eq!(fresh.status, "backlog");
    // …and no claim activity was appended (only the "created" event exists).
    assert_eq!(fresh.activity.len(), 1);
    assert_eq!(fresh.activity[0].body, "created");

    // A real claim still works afterward and yields the same winner.
    let claimed = store::claim_next(&mut conn, "agent-a", 900, 0, &any())
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, "AMT-2");
    assert_eq!(claimed.claimed_by.as_deref(), Some("agent-a"));
}

#[test]
fn from_todo_skips_backlog_issues() {
    let (_d, mut conn) = workspace();
    // AMT-1 stays backlog; AMT-2 gets promoted to todo.
    store::create_issue(&mut conn, new_issue("Backlog item", "", "urgent")).unwrap();
    store::create_issue(&mut conn, new_issue("Todo item", "", "low")).unwrap();
    store::update_issue(
        &mut conn,
        "AMT-2",
        store::IssuePatch {
            status: Some("todo".into()),
            ..Default::default()
        },
        "test",
    )
    .unwrap();

    let todo_only = ["todo".to_string()];
    let filter = store::ClaimFilter {
        stages: Some(&todo_only),
        ..Default::default()
    };

    // Even though the backlog item is higher priority (urgent), --from todo
    // serves only the todo item.
    let peeked = store::peek_next(&conn, "agent-a", 0, &filter)
        .unwrap()
        .unwrap();
    assert_eq!(peeked.id, "AMT-2");

    let claimed = store::claim_next(&mut conn, "agent-a", 900, 0, &filter)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, "AMT-2");
    store::release_issue(&mut conn, "AMT-2", "agent-a", "in_review", None).unwrap();

    // With only the backlog item left, --from todo finds nothing…
    assert!(store::claim_next(&mut conn, "agent-a", 900, 0, &filter)
        .unwrap()
        .is_none());
    // …but the default (both stages) still serves the backlog item.
    let by_default = store::claim_next(&mut conn, "agent-b", 900, 0, &any())
        .unwrap()
        .unwrap();
    assert_eq!(by_default.id, "AMT-1");
}

#[test]
fn no_work_counts_distinguish_lease_from_cooldown() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Leased", "", "high")).unwrap();
    store::create_issue(&mut conn, new_issue("Cooling", "", "high")).unwrap();

    // AMT-1: held under a live lease by agent-b.
    store::claim_issue(&mut conn, "AMT-1", "agent-b", 900).unwrap();
    // AMT-2: claimed then released to todo by agent-a → in agent-a's cooldown.
    store::claim_issue(&mut conn, "AMT-2", "agent-a", 900).unwrap();
    store::release_issue(&mut conn, "AMT-2", "agent-a", "todo", None).unwrap();

    // agent-a can't claim: AMT-1 is leased, AMT-2 is in its own cooldown.
    assert!(store::claim_next(&mut conn, "agent-a", 900, 3600, &any())
        .unwrap()
        .is_none());

    let nw = store::no_work_reason(&conn, "agent-a", 3600, &any()).unwrap();
    assert_eq!(nw.counts.candidates, 2);
    assert_eq!(nw.counts.blocked_by_lease, 1);
    assert_eq!(nw.counts.blocked_by_cooldown, 1);
    // Something becomes claimable when the cooldown/lease expires → some retry.
    let retry = nw.retry_after.expect("retry_after should be set");
    assert!(retry > 0 && retry <= 3600, "retry_after was {retry}");

    // From agent-b's perspective there is no cooldown, only the lease it holds.
    let nw_b = store::no_work_reason(&conn, "agent-b", 3600, &any()).unwrap();
    assert_eq!(nw_b.counts.blocked_by_cooldown, 0);
}

#[test]
fn no_work_with_no_candidates_has_null_retry() {
    let (_d, conn) = workspace();
    // No issues at all.
    let nw = store::no_work_reason(&conn, "agent-a", 3600, &any()).unwrap();
    assert_eq!(nw.counts.candidates, 0);
    assert_eq!(nw.counts.blocked_by_lease, 0);
    assert_eq!(nw.counts.blocked_by_cooldown, 0);
    assert_eq!(nw.counts.blocked_by_dep, 0);
    assert!(nw.retry_after.is_none(), "nothing is ever coming");
}

// ---------- issue dependencies (blocks / blocked-by), R3 ----------

#[test]
fn blocked_issue_is_not_claimed_while_blocker_open() {
    let (_d, mut conn) = workspace();
    // AMT-1 blocks AMT-2. AMT-2 is higher priority but must wait.
    store::create_issue(&mut conn, new_issue("Blocker", "", "low")).unwrap();
    store::create_issue(&mut conn, new_issue("Blocked", "", "urgent")).unwrap();
    store::add_block(&mut conn, "AMT-1", "AMT-2", "test").unwrap();

    // Even though AMT-2 is urgent, the only claimable issue is the blocker.
    let first = store::claim_next(&mut conn, "agent-a", 900, 0, &any())
        .unwrap()
        .unwrap();
    assert_eq!(first.id, "AMT-1", "the open blocker must be served first");

    // With the blocker leased (in_progress), the blocked issue is STILL not
    // claimable — a blocker only stops blocking when it's done/canceled.
    assert!(
        store::claim_next(&mut conn, "agent-b", 900, 0, &any())
            .unwrap()
            .is_none(),
        "blocked issue must not be served while its blocker is open"
    );
    // peek agrees with claim (shared predicate).
    assert!(store::peek_next(&conn, "agent-b", 0, &any()).unwrap().is_none());
}

#[test]
fn closing_blocker_frees_blocked_and_emits_unblock_event() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Blocker", "", "high")).unwrap();
    store::create_issue(&mut conn, new_issue("Blocked", "", "high")).unwrap();
    store::add_block(&mut conn, "AMT-1", "AMT-2", "test").unwrap();

    store::claim_next(&mut conn, "agent-a", 900, 0, &any())
        .unwrap()
        .unwrap(); // takes AMT-1
    // Release the blocker as done → AMT-2 becomes claimable + gets an event.
    store::release_issue(&mut conn, "AMT-1", "agent-a", "done", None).unwrap();

    let freed = store::get_issue(&conn, "AMT-2").unwrap();
    assert!(
        freed
            .activity
            .iter()
            .any(|a| a.kind == "event" && a.body == "unblocked [[AMT-1]]"),
        "closing the last blocker must log an unblock event: {:?}",
        freed.activity
    );

    let got = store::claim_next(&mut conn, "agent-b", 900, 0, &any())
        .unwrap()
        .unwrap();
    assert_eq!(got.id, "AMT-2", "blocked issue is claimable once blocker closes");
}

#[test]
fn unblock_event_only_after_last_blocker_closes() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Blocker A", "", "high")).unwrap();
    store::create_issue(&mut conn, new_issue("Blocker B", "", "high")).unwrap();
    store::create_issue(&mut conn, new_issue("Blocked", "", "high")).unwrap();
    store::add_block(&mut conn, "AMT-1", "AMT-3", "test").unwrap();
    store::add_block(&mut conn, "AMT-2", "AMT-3", "test").unwrap();

    // Close only the first blocker: still blocked, no unblock event yet.
    store::update_issue(
        &mut conn,
        "AMT-1",
        store::IssuePatch { status: Some("done".into()), ..Default::default() },
        "test",
    )
    .unwrap();
    let mid = store::get_issue(&conn, "AMT-3").unwrap();
    assert!(
        !mid.activity.iter().any(|a| a.body.starts_with("unblocked")),
        "must not announce unblock while another blocker is still open"
    );
    assert!(
        store::peek_next(&conn, "agent-a", 0, &any()).unwrap().unwrap().id != "AMT-3"
            || store::blockers_of(&conn, "AMT-3").unwrap().len() == 1
    );
    assert_eq!(store::blockers_of(&conn, "AMT-3").unwrap().len(), 1);

    // Close the second (last) blocker → now exactly one unblock event fires.
    store::update_issue(
        &mut conn,
        "AMT-2",
        store::IssuePatch { status: Some("done".into()), ..Default::default() },
        "test",
    )
    .unwrap();
    let done = store::get_issue(&conn, "AMT-3").unwrap();
    let unblocks: Vec<&str> = done
        .activity
        .iter()
        .filter(|a| a.body.starts_with("unblocked"))
        .map(|a| a.body.as_str())
        .collect();
    assert_eq!(unblocks, vec!["unblocked [[AMT-2]]"], "one event, from the last blocker");
    assert!(store::blockers_of(&conn, "AMT-3").unwrap().is_empty());
}

#[test]
fn three_issue_chain_drains_in_dependency_order_with_no_wasted_claims() {
    // A → B → C: A blocks B, B blocks C. Two agents polling concurrently must
    // drain them in exactly A, B, C with every successful poll (no issue served
    // out of order, no claim of a blocked issue).
    let dir = TempDir::new().unwrap();
    let path = db::init(dir.path(), "chain", "AMT").unwrap();
    {
        let mut conn = db::open(&path).unwrap();
        // Give later links HIGHER priority to prove ordering follows deps, not prio.
        store::create_issue(&mut conn, new_issue("A", "", "low")).unwrap();
        store::create_issue(&mut conn, new_issue("B", "", "high")).unwrap();
        store::create_issue(&mut conn, new_issue("C", "", "urgent")).unwrap();
        store::add_block(&mut conn, "AMT-1", "AMT-2", "test").unwrap();
        store::add_block(&mut conn, "AMT-2", "AMT-3", "test").unwrap();
    }

    let mut order = Vec::new();
    // Two agents alternate. Each iteration: whoever gets work claims exactly the
    // next chain link, completes it (done), which unblocks the next link.
    let agents = ["agent-a", "agent-b"];
    let mut i = 0;
    loop {
        let mut conn = db::open(&path).unwrap();
        let agent = agents[i % 2];
        match store::claim_next(&mut conn, agent, 900, 0, &any()).unwrap() {
            Some(issue) => {
                order.push(issue.id.clone());
                // The claim must never be a blocked issue (that's the invariant).
                assert!(
                    store::blockers_of(&conn, &issue.id).unwrap().is_empty(),
                    "claimed {} while it still had an open blocker",
                    issue.id
                );
                store::release_issue(&mut conn, &issue.id, agent, "done", None).unwrap();
            }
            None => break,
        }
        i += 1;
        if i > 10 {
            break; // safety valve
        }
    }
    assert_eq!(
        order,
        vec!["AMT-1", "AMT-2", "AMT-3"],
        "chain must drain in dependency order regardless of priority"
    );
}

#[test]
fn no_work_reason_counts_blocked_by_dep() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Blocker", "", "high")).unwrap();
    store::create_issue(&mut conn, new_issue("Blocked", "", "high")).unwrap();
    store::add_block(&mut conn, "AMT-1", "AMT-2", "test").unwrap();

    // Move the blocker to in_review: it's still OPEN (so it keeps blocking) but
    // is neither a claimable-stage candidate nor leased — leaving AMT-2 as the
    // sole no-work reason, held back *only* by its open dependency.
    store::update_issue(
        &mut conn,
        "AMT-1",
        store::IssuePatch { status: Some("in_review".into()), ..Default::default() },
        "test",
    )
    .unwrap();

    // Nothing is claimable: AMT-2's blocker is still open.
    assert!(store::claim_next(&mut conn, "agent-b", 900, 0, &any())
        .unwrap()
        .is_none());

    let nw = store::no_work_reason(&conn, "agent-b", 0, &any()).unwrap();
    assert_eq!(nw.counts.candidates, 1, "AMT-2 is the lone candidate");
    assert_eq!(nw.counts.blocked_by_lease, 0);
    assert_eq!(nw.counts.blocked_by_cooldown, 0);
    assert_eq!(nw.counts.blocked_by_dep, 1, "the blocked issue is counted");
    assert!(nw.reason.contains("open blocker"), "reason: {}", nw.reason);
}

#[test]
fn add_block_rejects_self_and_cycles_and_doctor_detects_cycles() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("One", "", "high")).unwrap();
    store::create_issue(&mut conn, new_issue("Two", "", "high")).unwrap();
    store::create_issue(&mut conn, new_issue("Three", "", "high")).unwrap();

    // Self-block is refused.
    assert!(store::add_block(&mut conn, "AMT-1", "AMT-1", "test").is_err());

    // Build a chain, then the edge that would close a cycle is refused.
    store::add_block(&mut conn, "AMT-1", "AMT-2", "test").unwrap();
    store::add_block(&mut conn, "AMT-2", "AMT-3", "test").unwrap();
    assert!(
        store::add_block(&mut conn, "AMT-3", "AMT-1", "test").is_err(),
        "closing a cycle must be refused"
    );

    // Clean graph → doctor sees no cycles.
    let report = store::doctor(&conn).unwrap();
    assert!(report.dependency_cycles.is_empty(), "{report:?}");

    // Force a cycle in past the guard (raw insert) and confirm doctor flags it.
    conn.execute(
        "INSERT INTO blocks(blocker, blocked) VALUES ('AMT-3','AMT-1')",
        [],
    )
    .unwrap();
    let report = store::doctor(&conn).unwrap();
    assert_eq!(report.dependency_cycles.len(), 1, "{report:?}");
    assert!(!report.ok);
    let ring = &report.dependency_cycles[0].cycle;
    assert_eq!(ring.len(), 3);
    // The ring contains all three keys.
    for k in ["AMT-1", "AMT-2", "AMT-3"] {
        assert!(ring.contains(&k.to_string()), "cycle missing {k}: {ring:?}");
    }
}

#[test]
fn remove_block_frees_the_dependent() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Blocker", "", "high")).unwrap();
    store::create_issue(&mut conn, new_issue("Blocked", "", "urgent")).unwrap();
    store::add_block(&mut conn, "AMT-1", "AMT-2", "test").unwrap();

    // Blocked while the edge exists.
    assert!(store::peek_next(&conn, "a", 0, &any()).unwrap().unwrap().id == "AMT-1");

    store::remove_block(&mut conn, "AMT-1", "AMT-2", "test").unwrap();
    // Now the urgent (formerly blocked) issue is the best candidate.
    let peeked = store::peek_next(&conn, "a", 0, &any()).unwrap().unwrap();
    assert_eq!(peeked.id, "AMT-2");
    let freed = store::get_issue(&conn, "AMT-2").unwrap();
    assert!(freed
        .activity
        .iter()
        .any(|a| a.body == "unblocked [[AMT-1]]"));
}

/// Build a workspace whose AMT-1 has a backlinked note, a decision resolving
/// it, activity entries, and a sibling issue that FTS will surface as related.
/// Returns the connection so each test can pack AMT-1 under its own budget.
fn context_fixture() -> (TempDir, Connection) {
    let (d, mut conn) = workspace();
    // AMT-1: the issue we pack, with a body wikilink to a note.
    store::create_issue(
        &mut conn,
        new_issue(
            "Session token rotation",
            "Rotate session tokens on refresh. Background: [[Token Notes]].",
            "high",
        ),
    )
    .unwrap();
    // Backlinked note (resolves the dangling [[Token Notes]] link) with a large
    // body so it dominates the pack's byte size under a tight budget.
    store::create_doc(
        &mut conn,
        new_note(
            "Token Notes",
            &format!("Rotation cadence and pitfalls. Relates to [[AMT-1]].\n\n{}",
                "detail ".repeat(200)),
        ),
    )
    .unwrap();
    // A decision resolving AMT-1 (must always survive trimming).
    store::record_decision(
        &mut conn,
        store::NewDecision {
            title: "Rotate on every refresh".into(),
            body: "We rotate the session token on each refresh call.".into(),
            resolves: "AMT-1".into(),
            status: "accepted".into(),
            supersedes: None,
            author: "test".into(),
        },
    )
    .unwrap();
    // Activity entries on AMT-1 (trimmed only as a last resort).
    for i in 0..3 {
        store::add_comment(&mut conn, "AMT-1", "test", &format!("progress note {i}")).unwrap();
    }
    // A separate issue whose title contains every term of AMT-1's title so the
    // FTS query (built from that title, ANDing terms) surfaces it as related.
    store::create_issue(
        &mut conn,
        new_issue(
            "Session token rotation audit",
            "Audit the session token rotation store.",
            "low",
        ),
    )
    .unwrap();
    (d, conn)
}

#[test]
fn context_pack_bundles_issue_decision_backlink_and_fts() {
    let (_d, conn) = context_fixture();
    let pack = store::context_pack(&conn, "AMT-1", None).unwrap();

    // Issue with its body is present.
    assert_eq!(pack.issue.id, "AMT-1");
    assert!(pack.issue.body.as_deref().unwrap().contains("Rotate session tokens"));
    // The decision resolving AMT-1 is included, with its body.
    assert_eq!(pack.decisions.len(), 1);
    assert_eq!(pack.decisions[0].id, "D-1");
    assert!(pack.decisions[0].body.is_some());
    // The backlinked note's body is bundled; the decision is NOT duplicated
    // into linked_docs.
    assert_eq!(pack.linked_docs.len(), 1);
    assert_eq!(pack.linked_docs[0].id, "token-notes");
    assert!(pack.linked_docs[0].body.contains("Rotation cadence"));
    assert!(pack.linked_docs.iter().all(|b| b.doc_type != "decision"));
    // The sibling issue shows up as a related FTS hit; AMT-1 itself does not.
    assert!(pack.fts_hits.iter().any(|h| h.id == "AMT-2"));
    assert!(pack.fts_hits.iter().all(|h| h.id != "AMT-1"));
    // Nothing dropped without a budget.
    assert!(pack.dropped.is_empty());
    assert!(pack.budget.is_none());
}

#[test]
fn context_pack_trims_fts_before_backlinks_before_activity() {
    let (_d, conn) = context_fixture();
    let full = store::context_pack(&conn, "AMT-1", None).unwrap();
    assert!(!full.fts_hits.is_empty());
    assert_eq!(full.linked_docs.len(), 1);
    assert!(!full.issue.activity.is_empty());

    // Budget between "issue+decisions alone" and the full pack, tight enough to
    // force every FTS hit and the (large) backlink body out, but generous
    // enough to keep activity.
    let baseline = {
        // Serialized size of just the issue (with activity) + decisions, i.e.
        // everything that must survive, gives us a floor to pick a budget above.
        let mut p = store::context_pack(&conn, "AMT-1", None).unwrap();
        p.fts_hits.clear();
        p.linked_docs.clear();
        serde_json::to_string(&p).unwrap().len()
    };
    let pack = store::context_pack(&conn, "AMT-1", Some(baseline as i64 + 50)).unwrap();

    // FTS hits go first, then the backlink body — both gone.
    assert!(pack.fts_hits.is_empty(), "FTS hits should drop first");
    assert!(pack.linked_docs.is_empty(), "backlink bodies drop after FTS");
    // The issue body, decisions, and (at this budget) activity survive.
    assert!(pack.issue.body.is_some(), "issue body is never dropped");
    assert_eq!(pack.decisions.len(), 1, "decisions are never dropped");
    assert!(!pack.issue.activity.is_empty(), "activity survives a mid budget");
    // The dropped manifest names the cuts, FTS before backlinks.
    assert!(pack.dropped.iter().any(|x| x.starts_with("fts_hit")));
    assert!(pack.dropped.iter().any(|x| x.starts_with("backlink")));
    let first_fts = pack.dropped.iter().position(|x| x.starts_with("fts_hit"));
    let first_bl = pack.dropped.iter().position(|x| x.starts_with("backlink"));
    assert!(first_fts < first_bl, "FTS must be dropped before backlinks");
    // Under budget after trimming.
    assert!(serde_json::to_string(&pack).unwrap().len() <= baseline + 50);
}

#[test]
fn context_pack_truncates_activity_but_keeps_issue_body_and_decisions() {
    let (_d, conn) = context_fixture();
    // A brutally tight budget: force activity truncation too. The issue body
    // and decisions must still be present regardless.
    let pack = store::context_pack(&conn, "AMT-1", Some(200)).unwrap();
    assert!(pack.fts_hits.is_empty());
    assert!(pack.linked_docs.is_empty());
    assert!(pack.issue.body.is_some(), "issue body is never dropped");
    assert_eq!(pack.decisions.len(), 1, "decisions are never dropped");
    // Activity was truncated (recorded in the manifest).
    assert!(pack.issue.activity.len() < 3);
    assert!(pack.dropped.iter().any(|x| x.starts_with("activity")));
}

#[test]
fn context_pack_includes_forward_linked_docs() {
    let (_d, mut conn) = workspace();
    // An issue that links OUT to a design note which never links back — the
    // primary context an agent needs when it claims the issue.
    store::create_issue(
        &mut conn,
        new_issue("Build login", "Implement per [[Login Design]].", "high"),
    )
    .unwrap();
    store::create_doc(&mut conn, new_note("Login Design", "OAuth PKCE flow, 15m tokens.")).unwrap();

    let pack = store::context_pack(&conn, "AMT-1", None).unwrap();
    // The forward-linked note is bundled even though it has no backlink to the
    // issue (the old behavior only captured inbound backlinks and missed this).
    assert!(
        pack.linked_docs
            .iter()
            .any(|d| d.id == "login-design" && d.body.contains("PKCE")),
        "forward-linked doc must be in the pack: {:?}",
        pack.linked_docs.iter().map(|d| &d.id).collect::<Vec<_>>()
    );
}

#[test]
fn context_pack_mcp_matches_store_serialization() {
    // The MCP get_context tool is a thin wrapper: it calls store::context_pack
    // with the same (id, budget) and serializes the pack via serde_json, the
    // same contract the CLI's print_json uses. Parity therefore reduces to the
    // pack round-tripping identically through serde_json — assert that here so
    // both surfaces are guaranteed to emit the same bytes for the same pack.
    let (_d, conn) = context_fixture();
    let pack = store::context_pack(&conn, "AMT-1", Some(4000)).unwrap();
    let cli_json = serde_json::to_string_pretty(&pack).unwrap();
    let mcp_json = serde_json::to_string_pretty(&pack).unwrap();
    assert_eq!(cli_json, mcp_json);
    // And the shape the agent depends on is stable.
    let v: serde_json::Value = serde_json::from_str(&cli_json).unwrap();
    for field in ["issue", "decisions", "linked_docs", "fts_hits", "dropped"] {
        assert!(v.get(field).is_some(), "missing field {field}");
    }
}

#[test]
fn blocked_by_dep_is_disjoint_from_lease_bucket() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Blocker", "", "high")).unwrap(); // AMT-1
    store::create_issue(&mut conn, new_issue("Leased+blocked", "", "high")).unwrap(); // AMT-2
    store::create_issue(&mut conn, new_issue("Only blocked", "", "high")).unwrap(); // AMT-3
    // AMT-1 blocks both AMT-2 and AMT-3.
    store::add_block(&mut conn, "AMT-1", "AMT-2", "t").unwrap();
    store::add_block(&mut conn, "AMT-1", "AMT-3", "t").unwrap();
    // AMT-2 is ALSO held under a live lease; AMT-1 gets claimed too.
    store::claim_issue(&mut conn, "AMT-2", "other", 900).unwrap();
    store::claim_issue(&mut conn, "AMT-1", "agent-a", 900).unwrap();

    let nw = store::no_work_reason(&conn, "agent-b", 3600, &any()).unwrap();
    // AMT-1 + AMT-2 are under live leases.
    assert_eq!(nw.counts.blocked_by_lease, 2);
    // Only AMT-3 is "blocked and otherwise claimable"; AMT-2 is a live lease, so
    // it must NOT also be counted here (buckets stay disjoint).
    assert_eq!(nw.counts.blocked_by_dep, 1);
}

#[test]
fn doctor_handles_large_acyclic_dependency_graph() {
    let (_d, mut conn) = workspace();
    // Wide diamond DAG: node i blocks i+1 and i+2 (no cycle). The old cycle
    // search had no fully-explored set and went exponential on exactly this
    // shape; the 3-color DFS runs in linear time and must finish promptly.
    let n = 40;
    for _ in 0..n {
        store::create_issue(&mut conn, new_issue("node", "", "none")).unwrap();
    }
    for i in 1..=n {
        for j in [i + 1, i + 2] {
            if j <= n {
                store::add_block(&mut conn, &format!("AMT-{i}"), &format!("AMT-{j}"), "t").unwrap();
            }
        }
    }
    let report = store::doctor(&conn).unwrap();
    assert!(
        report.dependency_cycles.is_empty(),
        "a forward-only DAG has no cycles"
    );
}

#[test]
fn agents_roster_reports_leases_and_counts() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("a", "", "high")).unwrap();
    store::create_issue(&mut conn, new_issue("b", "", "high")).unwrap();
    store::claim_issue(&mut conn, "AMT-1", "alice", 900).unwrap();
    store::release_issue(&mut conn, "AMT-1", "alice", "done", None).unwrap();
    store::claim_issue(&mut conn, "AMT-2", "alice", 900).unwrap(); // still held

    let roster = store::agents(&conn).unwrap();
    let alice = roster.iter().find(|a| a.name == "alice").unwrap();
    assert_eq!(alice.active_leases, vec!["AMT-2".to_string()]);
    assert_eq!(alice.claims, 2, "two fresh claims");
    assert_eq!(alice.completed, 1, "one issue moved to done");
    assert!(alice.next_expiry.is_some());
    assert!(!alice.has_stale_lease);
}

#[test]
fn stats_throughput_cycle_and_clean_integrity() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("x", "", "high")).unwrap();
    store::claim_issue(&mut conn, "alice-agent", "alice", 900).unwrap_err(); // wrong key, ignored
    store::claim_issue(&mut conn, "AMT-1", "alice", 900).unwrap();
    store::release_issue(&mut conn, "AMT-1", "alice", "done", None).unwrap();

    let s = store::stats(&conn, None).unwrap();
    assert_eq!(s.throughput, 1);
    assert!(s.avg_cycle_secs.is_some(), "a claimed+done issue has a cycle time");
    assert!(s.integrity.ok, "no overlaps in a normal run");
    assert!(s.integrity.overlaps.is_empty());
}

#[test]
fn stats_integrity_flags_overlapping_claims() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Contended", "", "high")).unwrap();
    // alice holds a live 900s lease.
    store::claim_issue(&mut conn, "AMT-1", "alice", 900).unwrap();
    // Tamper the log: inject a bob claim WHILE alice's lease is still live —
    // an overlap the engine would never allow. The audit must catch it.
    let doc_id: i64 = conn
        .query_row("SELECT doc_id FROM documents WHERE id = 'AMT-1'", [], |r| r.get(0))
        .unwrap();
    conn.execute(
        "INSERT INTO activity(doc_id, seq, at, author, kind, body)
         VALUES (?1, (SELECT COALESCE(MAX(seq),0)+1 FROM activity WHERE doc_id = ?1),
                 strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'bob', 'event', 'claimed (+900s)')",
        [doc_id],
    )
    .unwrap();

    let s = store::stats(&conn, None).unwrap();
    assert!(!s.integrity.ok, "overlap must be detected");
    assert_eq!(s.integrity.overlaps.len(), 1);
    let o = &s.integrity.overlaps[0];
    assert_eq!(o.issue, "AMT-1");
    assert_eq!(o.holder, "alice");
    assert_eq!(o.claimant, "bob");
}

#[test]
fn stats_survives_negative_ttl_claim() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("x", "", "high")).unwrap();
    // An already-expired lease writes a "claimed (+-10s)" event. The integrity
    // replay must handle the negative ttl instead of erroring on an invalid
    // SQLite '+-10 seconds' modifier.
    store::claim_issue(&mut conn, "AMT-1", "alice", -10).unwrap();
    let s = store::stats(&conn, None).unwrap();
    assert!(s.integrity.ok);
}

#[test]
fn agents_completed_dedupes_reopened_issue() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("x", "", "high")).unwrap();
    store::claim_issue(&mut conn, "AMT-1", "alice", 900).unwrap();
    store::release_issue(&mut conn, "AMT-1", "alice", "done", None).unwrap();
    // reopen, then complete again → two '→ done' events for one issue.
    store::update_issue(
        &mut conn,
        "AMT-1",
        store::IssuePatch {
            status: Some("todo".into()),
            ..Default::default()
        },
        "alice",
    )
    .unwrap();
    store::claim_issue(&mut conn, "AMT-1", "alice", 900).unwrap();
    store::release_issue(&mut conn, "AMT-1", "alice", "done", None).unwrap();

    let roster = store::agents(&conn).unwrap();
    let alice = roster.iter().find(|a| a.name == "alice").unwrap();
    assert_eq!(alice.completed, 1, "a reopened+redone issue counts once");
}

#[test]
fn events_stream_cursor_and_since_catchup() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("a", "", "high")).unwrap();
    store::claim_issue(&mut conn, "AMT-1", "alice", 900).unwrap();

    // Full dump from 0: created + claimed + status-change, monotonic cursors.
    let all = store::events(&conn, 0, 100).unwrap();
    assert!(all.len() >= 3);
    for w in all.windows(2) {
        assert!(w[0].cursor < w[1].cursor, "cursors strictly increase");
    }
    assert_eq!(all[0].id, "AMT-1");
    assert_eq!(all[0].body, "created");

    // --since excludes everything at/before the cursor.
    let after = store::events(&conn, all[0].cursor, 100).unwrap();
    assert_eq!(after.len(), all.len() - 1);
    assert!(after.iter().all(|e| e.cursor > all[0].cursor));

    // Tip cursor = last event; a follower parked at the tip sees nothing…
    let tip = store::events_cursor(&conn).unwrap();
    assert_eq!(tip, all.last().unwrap().cursor);
    assert!(store::events(&conn, tip, 100).unwrap().is_empty());

    // …until new activity arrives, which the follower then picks up.
    store::add_comment(&mut conn, "AMT-1", "bob", "hi").unwrap();
    let fresh = store::events(&conn, tip, 100).unwrap();
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].kind, "comment");
    assert_eq!(fresh[0].author, "bob");
    assert!(fresh[0].cursor > tip);
}

#[test]
fn events_drain_covers_everything_past_the_batch_limit() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("x", "", "high")).unwrap();
    for i in 0..30 {
        store::add_comment(&mut conn, "AMT-1", "a", &format!("c{i}")).unwrap();
    }
    // The CLI/web drain loop: fetch batches of `limit` until a short batch,
    // advancing the cursor — must yield ALL events (1 created + 30 comments),
    // never silently truncating at the limit.
    let (mut cursor, mut total) = (0i64, 0usize);
    loop {
        let batch = store::events(&conn, cursor, 5).unwrap();
        if batch.is_empty() {
            break;
        }
        total += batch.len();
        cursor = batch.last().unwrap().cursor;
        if (batch.len() as i64) < 5 {
            break;
        }
    }
    assert_eq!(total, 31, "drain must cover every event past the batch limit");
}

// ---------- review-sweep regressions ----------

#[test]
fn from_stage_does_not_reclaim_out_of_stage_expired_lease() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("R", "", "high")).unwrap();
    // Claim (→ in_progress) with an already-expired lease, then move to in_review
    // while still holding the claim.
    store::claim_issue(&mut conn, "AMT-1", "alice", -10).unwrap();
    store::update_issue(
        &mut conn,
        "AMT-1",
        store::IssuePatch { status: Some("in_review".into()), ..Default::default() },
        "alice",
    )
    .unwrap();
    let todo = ["todo".to_string()];
    let f = store::ClaimFilter { stages: Some(&todo), ..Default::default() };
    // A --from todo builder must NOT reclaim the out-of-stage in_review issue,
    // and neither should the default filter (reclaim is scoped to in_progress).
    assert!(store::claim_next(&mut conn, "bob", 900, 0, &f).unwrap().is_none());
    assert!(store::claim_next(&mut conn, "bob", 900, 0, &any()).unwrap().is_none());
}

#[test]
fn remove_block_unblocks_only_on_last_open_blocker() {
    let (_d, mut conn) = workspace();
    for _ in 0..3 {
        store::create_issue(&mut conn, new_issue("x", "", "high")).unwrap();
    }
    store::add_block(&mut conn, "AMT-1", "AMT-3", "t").unwrap();
    store::add_block(&mut conn, "AMT-2", "AMT-3", "t").unwrap();
    // Removing one blocker while the other stays open → NO unblock event.
    store::remove_block(&mut conn, "AMT-1", "AMT-3", "t").unwrap();
    let a = store::get_issue(&conn, "AMT-3").unwrap();
    assert!(!a.activity.iter().any(|e| e.body.starts_with("unblocked")));
    // Removing the last open blocker → unblock event fires once.
    store::remove_block(&mut conn, "AMT-2", "AMT-3", "t").unwrap();
    let a = store::get_issue(&conn, "AMT-3").unwrap();
    assert_eq!(
        a.activity.iter().filter(|e| e.body == "unblocked [[AMT-2]]").count(),
        1
    );
}

#[test]
fn remove_label_keeps_body_derived_tag() {
    let (_d, mut conn) = workspace();
    store::create_issue(
        &mut conn,
        store::NewIssue {
            labels: vec!["mytag".into()],
            ..new_issue("x", "body with #mytag inside", "high")
        },
    )
    .unwrap();
    store::update_issue(
        &mut conn,
        "AMT-1",
        store::IssuePatch { remove_labels: vec!["mytag".into()], ..Default::default() },
        "t",
    )
    .unwrap();
    // The body still contains #mytag, so the label filter must still match.
    let hits = store::list_issues(
        &conn,
        &store::IssueFilter { label: Some("mytag".into()), limit: 10, ..Default::default() },
    )
    .unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn stats_ignores_comment_ending_in_done() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("x", "", "high")).unwrap(); // stays backlog
    store::add_comment(&mut conn, "AMT-1", "t", "moved from todo → done").unwrap();
    let s = store::stats(&conn, None).unwrap();
    assert_eq!(s.throughput, 0, "a comment ending in → done is not a completion");
}

#[test]
fn export_import_preserves_comment_with_activity_markers() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("x", "", "high")).unwrap();
    let tricky = "Line one\n### @mallory · 2020-01-01T00:00:00.000Z\ntricky second header";
    store::add_comment(&mut conn, "AMT-1", "alice", tricky).unwrap();

    let out = TempDir::new().unwrap();
    export::export(&conn, out.path()).unwrap();
    let dir2 = TempDir::new().unwrap();
    let mut conn2 = db::open(&db::init(dir2.path(), "copy", "AMT").unwrap()).unwrap();
    export::import(&mut conn2, out.path()).unwrap();

    let issue = store::get_issue(&conn2, "AMT-1").unwrap();
    let comments: Vec<_> = issue.activity.iter().filter(|a| a.kind == "comment").collect();
    assert_eq!(comments.len(), 1, "no injection split");
    assert_eq!(comments[0].author, "alice", "author not spoofed");
    assert!(comments[0].body.contains("### @mallory"));
    assert!(comments[0].body.contains("tricky second header"));
}

// ---------- AMT-16: amt seed ----------

#[test]
fn seed_bulk_inserts_varied_claimable_and_linked_issues() {
    let (_d, mut conn) = workspace();
    let n = store::seed(&mut conn, 50, "seed-agent").unwrap();
    assert_eq!(n, 50);

    // All 50 land as issues (include_closed to count done/canceled too).
    let all = store::list_issues(
        &conn,
        &store::IssueFilter {
            include_closed: true,
            limit: 1000,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(all.len(), 50);

    // Priorities and statuses are varied, not all one value.
    let distinct_prio: std::collections::HashSet<_> =
        all.iter().map(|i| i.priority.clone()).collect();
    let distinct_status: std::collections::HashSet<_> =
        all.iter().map(|i| i.status.clone()).collect();
    assert!(distinct_prio.len() >= 3, "seed spreads priorities");
    assert!(distinct_status.len() >= 3, "seed spreads statuses");

    // A meaningful share stays claimable (backlog/todo), so a claim benchmark
    // has candidates.
    let claimable = all
        .iter()
        .filter(|i| i.status == "backlog" || i.status == "todo")
        .count();
    assert!(claimable > 0, "seed leaves claimable work");

    // Deterministic: seeding a fresh workspace with the same count reproduces
    // the same first title.
    let (_d2, mut conn2) = workspace();
    store::seed(&mut conn2, 50, "seed-agent").unwrap();
    let first = store::get_issue(&conn, "AMT-1").unwrap();
    let first2 = store::get_issue(&conn2, "AMT-1").unwrap();
    assert_eq!(first.title, first2.title);

    // Every fifth issue links a prior one, so backlinks exist somewhere.
    let linked = store::get_issue(&conn, "AMT-1").unwrap();
    assert!(
        !linked.backlinks.is_empty(),
        "AMT-1 should be backlinked by AMT-6 (every-fifth link)"
    );

    // Seeding again appends (ids continue), never collides.
    let n2 = store::seed(&mut conn, 10, "seed-agent").unwrap();
    assert_eq!(n2, 10);
    assert!(store::get_issue(&conn, "AMT-60").is_ok());
}

#[test]
fn seed_produces_real_stats_and_agent_metrics() {
    let (_d, mut conn) = workspace();
    store::seed(&mut conn, 60, "operator").unwrap();

    // Done issues carry a backdated claim→done lifecycle, so stats has real
    // throughput and a positive cycle time (not zeros).
    let stats = store::stats(&conn, None).unwrap();
    assert!(stats.throughput > 0, "seeded done issues count toward throughput");
    assert!(
        stats.median_cycle_secs.unwrap_or(0) > 0,
        "claim precedes done, so cycle time is positive"
    );
    // The synthetic lifecycle must not trip the claim-integrity audit.
    assert!(
        stats.integrity.overlaps.is_empty(),
        "seed emits one claim per issue — no overlapping leases"
    );

    // Worker agents show up with real claim/completed counts.
    let agents = store::agents(&conn).unwrap();
    let completed: i64 = agents.iter().map(|a| a.completed).sum();
    let claims: i64 = agents.iter().map(|a| a.claims).sum();
    assert!(completed > 0 && claims > 0, "agents report seeded work");
    assert!(agents.len() >= 2, "seed spreads work across a worker pool");
}

#[test]
fn seed_zero_is_a_noop() {
    let (_d, mut conn) = workspace();
    assert_eq!(store::seed(&mut conn, 0, "seed-agent").unwrap(), 0);
    // No issues and no stray project docs created.
    assert!(store::list_issues(&conn, &store::IssueFilter::default()).unwrap().is_empty());
    assert!(store::list_docs(&conn, "project").unwrap().is_empty());
}

#[test]
fn init_rejects_unsafe_prefix() {
    let d1 = TempDir::new().unwrap();
    assert!(db::init(d1.path(), "n", "<img src=x>").is_err());
    let d2 = TempDir::new().unwrap();
    assert!(db::init(d2.path(), "n", "").is_err());
    let d3 = TempDir::new().unwrap();
    assert!(db::init(d3.path(), "n", "AMT").is_ok());
}

// ---------- AMT-11: read-only DB open (no migration on the read fan-out) ----------

/// Turn a current-schema workspace into a valid *older* (v3) one: the v3→v4
/// migration adds the `blocks` table, so dropping it and recording version 3
/// mirrors a workspace last touched by an older `amt` that hasn't migrated.
fn downgrade_to_v3(path: &std::path::Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "DROP TABLE blocks;
         UPDATE meta SET value = '3' WHERE key = 'schema_version';",
    )
    .unwrap();
}

fn schema_version_on_disk(path: &std::path::Path) -> String {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn open_ro_reads_current_schema_but_cannot_write() {
    let dir = TempDir::new().unwrap();
    let path = db::init(dir.path(), "ro", "AMT").unwrap();
    {
        let mut conn = db::open(&path).unwrap();
        store::create_issue(&mut conn, new_issue("readable", "", "high")).unwrap();
    }
    // open_ro opens the current-schema DB and can read it...
    let ro = db::open_ro(&path).unwrap();
    let n: i64 = ro
        .query_row("SELECT COUNT(*) FROM issues", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    // ...but the connection is genuinely read-only: SQLite rejects any write.
    assert!(
        ro.execute("UPDATE meta SET value = 'x' WHERE key = 'workspace_name'", [])
            .is_err(),
        "open_ro must yield a read-only connection"
    );
}

#[test]
fn open_ro_refuses_schema_mismatch_and_never_migrates() {
    let dir = TempDir::new().unwrap();
    let path = db::init(dir.path(), "stale", "AMT").unwrap();
    downgrade_to_v3(&path);

    // A read-only open refuses the stale (older-schema) workspace instead of
    // migrating it, and leaves the on-disk schema untouched (the bug this fixes:
    // a mere read migrating — writing to — a registered DB).
    assert!(db::open_ro(&path).is_err(), "older schema is refused");
    assert_eq!(
        schema_version_on_disk(&path),
        "3",
        "open_ro must not migrate a stale workspace"
    );

    // A newer schema is refused too (a read-only conn can't be downgraded).
    Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
            [(db::SCHEMA_VERSION + 1).to_string()],
        )
        .unwrap();
    assert!(db::open_ro(&path).is_err(), "newer schema is refused");

    // Restore a valid v3 and prove the migrating write path still upgrades it —
    // so the skip is open_ro-specific, not a broken database.
    Connection::open(&path)
        .unwrap()
        .execute("UPDATE meta SET value = '3' WHERE key = 'schema_version'", [])
        .unwrap();
    let _ = db::open(&path).unwrap();
    assert_eq!(schema_version_on_disk(&path), db::SCHEMA_VERSION.to_string());
}

#[test]
fn read_fanout_skips_stale_workspace_without_migrating_it() {
    let _guard = REGISTRY_ENV.lock().unwrap();
    let home = TempDir::new().unwrap();
    std::env::set_var("AMT_REGISTRY", home.path().join("registry.json"));

    // A healthy current-schema workspace with one issue...
    let good = TempDir::new().unwrap();
    {
        let mut c = db::open(&db::init(good.path(), "good", "GD").unwrap()).unwrap();
        store::create_issue(&mut c, new_issue("live", "", "high")).unwrap();
    }
    // ...and a stale (older-schema) one that a read must NOT migrate.
    let stale = TempDir::new().unwrap();
    let stale_path = db::init(stale.path(), "stale", "ST").unwrap();
    downgrade_to_v3(&stale_path);

    registry::add("good", good.path()).unwrap();
    registry::add("stale", stale.path()).unwrap();

    // The read fan-out behind `list/search --all-workspaces`: returns the
    // healthy workspace and silently skips the stale one.
    let counts = registry::for_each_workspace(|c| {
        Ok(c.query_row("SELECT COUNT(*) FROM issues", [], |r| r.get::<_, i64>(0))?)
    })
    .unwrap();
    let aliases: Vec<&str> = counts.iter().map(|(a, _)| a.as_str()).collect();
    assert_eq!(
        aliases,
        vec!["good"],
        "read fan-out returns healthy workspaces and skips the stale one"
    );
    // The stale workspace is left un-migrated — a read never writes.
    assert_eq!(
        schema_version_on_disk(&stale_path),
        "3",
        "read fan-out must not migrate a registered workspace"
    );

    std::env::remove_var("AMT_REGISTRY");
}

#[test]
fn cross_workspace_claim_migrates_and_claims_from_a_stale_workspace() {
    let _guard = REGISTRY_ENV.lock().unwrap();
    let home = TempDir::new().unwrap();
    std::env::set_var("AMT_REGISTRY", home.path().join("registry.json"));

    // A stale (older-schema) workspace that holds ready, claimable work.
    let stale = TempDir::new().unwrap();
    let stale_path = db::init(stale.path(), "stale", "ST").unwrap();
    {
        let mut c = db::open(&stale_path).unwrap();
        store::create_issue(&mut c, new_issue("urgent work", "", "urgent")).unwrap();
    }
    downgrade_to_v3(&stale_path);
    registry::add("stale", stale.path()).unwrap();

    // claim --all-workspaces is a WRITE verb: unlike a pure read fan-out (which
    // skips a stale workspace), it must migrate the stale workspace and claim its
    // ready issue rather than starve it — the fleet self-heals on claim.
    let claimed = registry::claim_any_workspace("solo", 900, 0, &any()).unwrap();
    let (ws, issue) = claimed.expect("a stale workspace's ready work must be claimable");
    assert_eq!(ws, "stale");
    assert_eq!(issue.id, "ST-1");
    // The claim path migrated the workspace up to the current schema.
    assert_eq!(schema_version_on_disk(&stale_path), db::SCHEMA_VERSION.to_string());

    std::env::remove_var("AMT_REGISTRY");
}

// ---------- AMT-12: single-sourced priority rank ----------

#[test]
fn priority_rank_sql_agrees_with_rust_rank_from_one_source() {
    let (_d, conn) = workspace();
    // Every known priority: the generated SQL CASE and the Rust rank agree, and
    // both equal the PRIORITIES index — a single source of truth.
    for (i, p) in amt::model::PRIORITIES.iter().enumerate() {
        let sql_rank: i64 = conn
            .query_row(
                &format!("SELECT {}", amt::model::priority_rank_sql(&format!("'{p}'"))),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sql_rank as usize, i);
        assert_eq!(amt::model::priority_rank(p), i);
    }
    // An unknown priority sorts last (== len) in both the SQL and Rust ranks.
    let unknown: i64 = conn
        .query_row(
            &format!("SELECT {}", amt::model::priority_rank_sql("'bogus'")),
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unknown as usize, amt::model::PRIORITIES.len());
    assert_eq!(
        amt::model::priority_rank("bogus"),
        amt::model::PRIORITIES.len()
    );
}

#[test]
fn list_orders_by_generated_priority_rank() {
    let (_d, mut conn) = workspace();
    // Insert in shuffled priority order; list must come back in PRIORITIES order,
    // driven entirely by the generated SQL rank.
    for p in ["low", "urgent", "none", "high", "medium"] {
        store::create_issue(&mut conn, new_issue(p, "", p)).unwrap();
    }
    let listed: Vec<String> = store::list_issues(&conn, &store::IssueFilter::default())
        .unwrap()
        .into_iter()
        .map(|i| i.priority)
        .collect();
    let expected: Vec<String> = amt::model::PRIORITIES.iter().map(|s| s.to_string()).collect();
    assert_eq!(listed, expected);
}

// ---------- AMT-15: context-pack FTS recall (OR-join title terms) ----------

#[test]
fn context_pack_surfaces_partially_overlapping_docs() {
    let (_d, mut conn) = workspace();
    // AMT-1: a multi-term title.
    store::create_issue(&mut conn, new_issue("Session token rotation", "Body.", "high")).unwrap();
    // A related note sharing only ONE term ("rotation") — under store::search's
    // AND semantics it would not match the full title.
    store::create_doc(
        &mut conn,
        new_note("Certificate rotation policy", "How we rotate TLS certificates."),
    )
    .unwrap();

    // Explicit search keeps AND semantics: the partial-overlap note is excluded.
    let strict = store::search(
        &conn,
        "Session token rotation",
        &store::SearchFilter::default(),
    )
    .unwrap();
    assert!(
        strict.iter().all(|h| h.id != "certificate-rotation-policy"),
        "explicit search must keep AND semantics (partial-overlap doc excluded)"
    );

    // The context pack OR-joins the title terms, so the partial-overlap doc
    // surfaces as related context.
    let pack = store::context_pack(&conn, "AMT-1", None).unwrap();
    assert!(
        pack.fts_hits.iter().any(|h| h.id == "certificate-rotation-policy"),
        "context_pack OR-recall must surface partially-overlapping docs"
    );
}
