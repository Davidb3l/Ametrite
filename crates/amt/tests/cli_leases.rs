//! CLI-level tests for status changes vs live leases (AMT-46).
//!
//! The CLI applies `--blocks`/`--blocked-by` edges before the field update, so
//! a status change another agent's lease forbids must be refused before any
//! edge commits — otherwise a refused command still blocks claims.
//!
//! Every run passes `AMT_REGISTRY` as a CHILD-process env var, so these tests
//! never touch the developer's real `~/.ametrite/registry.json`.

use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn amt_as(dir: &Path, registry: &Path, agent: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_amt"))
        .current_dir(dir)
        .env("AMT_REGISTRY", registry)
        .env("AMT_AGENT", agent)
        .args(args)
        .output()
        .expect("run amt")
}

#[test]
fn a_refused_status_change_leaves_no_dependency_edge() {
    let root = TempDir::new().unwrap();
    let reg = root.path().join("registry.json");
    let repo = root.path().join("lease-test");
    std::fs::create_dir_all(&repo).unwrap();
    let ok = |o: Output| assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    ok(amt_as(
        &repo,
        &reg,
        "op",
        &["init", "--name", "Lease", "--prefix", "LS"],
    ));
    ok(amt_as(
        &repo,
        &reg,
        "op",
        &["issue", "create", "--title", "Held"],
    ));
    ok(amt_as(
        &repo,
        &reg,
        "op",
        &["issue", "create", "--title", "Other"],
    ));
    ok(amt_as(&repo, &reg, "alice", &["claim", "--issue", "LS-1"]));

    let refused = amt_as(
        &repo,
        &reg,
        "bob",
        &[
            "issue", "update", "LS-1", "--status", "done", "--blocks", "LS-2",
        ],
    );
    assert!(!refused.status.success());
    let err = String::from_utf8_lossy(&refused.stderr);
    assert!(err.contains("claimed by alice"), "{err}");

    let deps = amt_as(&repo, &reg, "op", &["dep", "list", "LS-1", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&deps.stdout).unwrap();
    assert!(
        v["blocks"].as_array().is_none_or(|b| b.is_empty()),
        "no edge committed: {v}"
    );

    // The holder may; the operator may with --force.
    ok(amt_as(
        &repo,
        &reg,
        "op",
        &[
            "issue",
            "update",
            "LS-1",
            "--status",
            "in_review",
            "--force",
        ],
    ));
}
