//! CLI-level tests for `amt brief` (AMT-29).
//!
//! These drive the real binary because the contract under test is the CLI's:
//! `--json` is exactly one JSON object on stdout (SUITE_CONTRACTS §4), an empty
//! workspace is a state rather than an error, and the default agent comes from
//! the environment. Library tests can't see any of that.
//!
//! Every run passes `AMT_REGISTRY` as a CHILD-process env var, so these tests
//! never touch the developer's real `~/.ametrite/registry.json`.

use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn amt(dir: &Path, registry: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_amt"))
        .current_dir(dir)
        .env("AMT_REGISTRY", registry)
        .env("AMT_AGENT", "worker-1")
        .args(args)
        .output()
        .expect("run amt")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// An initialized workspace with its own registry.
fn workspace() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = TempDir::new().unwrap();
    let registry = root.path().join("registry.json");
    let repo = root.path().join("brief-test");
    std::fs::create_dir_all(&repo).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_amt"))
        .current_dir(&repo)
        .env("AMT_REGISTRY", &registry)
        .args(["init", "--name", "Brief Test", "--prefix", "BT"])
        .output()
        .expect("init");
    assert!(out.status.success(), "{}", stderr(&out));
    (root, registry, repo)
}

#[test]
fn brief_json_is_exactly_one_object_with_every_section() {
    let (_root, reg, repo) = workspace();
    assert!(
        amt(&repo, &reg, &["issue", "create", "--title", "Do the thing"])
            .status
            .success()
    );
    assert!(amt(
        &repo,
        &reg,
        &[
            "note",
            "create",
            "--title",
            "Handoff Friday",
            "--tag",
            "handoff",
            "-b",
            "Stopped mid-thing."
        ]
    )
    .status
    .success());

    let out = amt(&repo, &reg, &["brief", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("stdout is one object");
    assert!(v.is_object());
    // Default agent comes from $AMT_AGENT.
    assert_eq!(v["agent"], "worker-1");
    for key in [
        "since",
        "my_work",
        "in_flight",
        "activity",
        "decisions",
        "backlog",
    ] {
        assert!(v.get(key).is_some(), "missing section '{key}'");
    }
    assert_eq!(v["backlog"][0]["id"], "BT-1");
    assert_eq!(v["handoff"]["title"], "Handoff Friday");
    assert!(v["handoff"]["body"].as_str().unwrap().contains("mid-thing"));
}

#[test]
fn brief_of_an_empty_workspace_succeeds() {
    let (_root, reg, repo) = workspace();
    let human = amt(&repo, &reg, &["brief"]);
    assert!(human.status.success(), "{}", stderr(&human));
    assert!(stdout(&human).contains("brief for @worker-1"));

    let json = amt(&repo, &reg, &["--json", "brief"]);
    assert!(json.status.success(), "{}", stderr(&json));
    let v: serde_json::Value = serde_json::from_str(&stdout(&json)).expect("stdout is one object");
    assert_eq!(v["my_work"].as_array().unwrap().len(), 0);
    assert!(v.get("handoff").is_none(), "absent handoff is omitted");
}

#[test]
fn brief_rejects_an_unparseable_since() {
    let (_root, reg, repo) = workspace();
    let out = amt(&repo, &reg, &["brief", "--since", "yesterday"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("invalid since"), "{}", stderr(&out));
    assert!(stdout(&out).is_empty(), "no half-brief on stdout");
}

#[test]
fn brief_takes_no_lease_and_writes_no_activity() {
    let (_root, reg, repo) = workspace();
    assert!(
        amt(&repo, &reg, &["issue", "create", "--title", "Untouched"])
            .status
            .success()
    );
    let before = stdout(&amt(&repo, &reg, &["--json", "issue", "show", "BT-1"]));
    assert!(amt(&repo, &reg, &["brief"]).status.success());
    let after = stdout(&amt(&repo, &reg, &["--json", "issue", "show", "BT-1"]));
    assert_eq!(before, after, "brief must not mutate the workspace");
}
