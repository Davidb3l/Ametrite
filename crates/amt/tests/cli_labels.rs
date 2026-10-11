//! CLI-level tests for comma-joined labels and `amt doctor --fix` (AMT-45).
//!
//! These drive the real binary because the contracts are the CLI's: `--label
//! "bug,ui"` must be findable as `bug`, and `doctor --fix --json` must still
//! print exactly one JSON object (SUITE_CONTRACTS §3/§4).
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
fn json(o: &Output) -> serde_json::Value {
    assert!(o.status.success(), "{}", stderr(o));
    serde_json::from_str(&stdout(o)).expect("exactly one JSON value on stdout")
}

fn workspace() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = TempDir::new().unwrap();
    let registry = root.path().join("registry.json");
    let repo = root.path().join("labels-test");
    std::fs::create_dir_all(&repo).unwrap();
    let init = amt(
        &repo,
        &registry,
        &["init", "--name", "Labels", "--prefix", "LB"],
    );
    assert!(init.status.success(), "{}", stderr(&init));
    (root, registry, repo)
}

#[test]
fn a_comma_joined_label_is_found_by_each_part() {
    let (_root, reg, repo) = workspace();
    let created = json(&amt(
        &repo,
        &reg,
        &[
            "issue", "create", "--title", "Crash", "--label", "bug,ui", "--json",
        ],
    ));
    let mut labels: Vec<String> = serde_json::from_value(created["labels"].clone()).unwrap();
    labels.sort();
    assert_eq!(labels, vec!["bug", "ui"]);

    let bugs = json(&amt(
        &repo,
        &reg,
        &["issue", "list", "--label", "bug", "--json"],
    ));
    assert_eq!(bugs.as_array().unwrap().len(), 1);
}

#[test]
fn doctor_fix_splits_legacy_labels_and_keeps_one_json_object() {
    let (_root, reg, repo) = workspace();
    json(&amt(
        &repo,
        &reg,
        &["issue", "create", "--title", "Old", "--json"],
    ));
    // Plant a label the way pre-AMT-45 builds stored it.
    let db = rusqlite::Connection::open(repo.join(".ametrite").join("ametrite.db")).unwrap();
    db.execute(
        "INSERT INTO tags(doc_id, tag, src)
         SELECT doc_id, 'bug,ui', 'label' FROM documents WHERE id = 'LB-1'",
        [],
    )
    .unwrap();
    drop(db);

    // Reported, not changed, without --fix.
    let human = amt(&repo, &reg, &["doctor"]);
    assert!(
        stdout(&human).contains("malformed label: LB-1 'bug,ui'"),
        "{}",
        stdout(&human)
    );
    let env = json(&amt(&repo, &reg, &["doctor", "--json"]));
    assert_eq!(env["ok"], false);
    let check = env["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "malformed_labels")
        .expect("malformed_labels check");
    assert_eq!(check["detail"], "1 found");
    assert!(env.get("fixed").is_none(), "fixed only appears with --fix");

    // --fix --json: one object, the repaired state, and what changed.
    let env = json(&amt(&repo, &reg, &["doctor", "--fix", "--json"]));
    assert_eq!(env["ok"], true);
    assert_eq!(env["fixed"][0]["id"], "LB-1");
    assert_eq!(env["fixed"][0]["fixed"], serde_json::json!(["bug", "ui"]));
    let bugs = json(&amt(
        &repo,
        &reg,
        &["issue", "list", "--label", "bug", "--json"],
    ));
    assert_eq!(bugs.as_array().unwrap().len(), 1);

    // Idempotent, and the human form says the workspace is healthy.
    let again = amt(&repo, &reg, &["doctor", "--fix"]);
    assert!(
        stdout(&again).contains("workspace healthy"),
        "{}",
        stdout(&again)
    );
}
