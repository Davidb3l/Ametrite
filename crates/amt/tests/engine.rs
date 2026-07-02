use amt::{db, export, store};
use rusqlite::Connection;
use tempfile::TempDir;

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

    let first = store::claim_next(&mut conn, "agent-a", None, None, 900)
        .unwrap()
        .unwrap();
    assert_eq!(first.id, "AMT-2", "urgent should be claimed first");
    assert_eq!(first.status, "in_progress");
    assert_eq!(first.claimed_by.as_deref(), Some("agent-a"));

    let second = store::claim_next(&mut conn, "agent-b", None, None, 900)
        .unwrap()
        .unwrap();
    assert_eq!(second.id, "AMT-1");

    // nothing left
    assert!(store::claim_next(&mut conn, "agent-c", None, None, 900)
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
    store::claim_next(&mut conn, "crashed-agent", None, None, -10)
        .unwrap()
        .unwrap();
    let report = store::doctor(&conn).unwrap();
    assert_eq!(report.stale_claims.len(), 1);

    let stolen = store::claim_next(&mut conn, "agent-b", None, None, 900).unwrap();
    assert_eq!(stolen.unwrap().claimed_by.as_deref(), Some("agent-b"));
}

#[test]
fn release_sets_status_and_clears_claim() {
    let (_d, mut conn) = workspace();
    store::create_issue(&mut conn, new_issue("Task", "", "none")).unwrap();
    store::claim_next(&mut conn, "agent-a", None, None, 900)
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
                store::claim_next(&mut conn, &format!("agent-{agent}"), None, None, 900).unwrap()
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

    let out = TempDir::new().unwrap();
    export::export(&conn, out.path()).unwrap();
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
