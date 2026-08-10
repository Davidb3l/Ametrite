//! CLI-level tests for `amt init`'s prefix behavior (AMT-24).
//!
//! These drive the real binary because the derive / refuse / warn / proceed
//! decisions live in the CLI layer, not the library — the branch that made a
//! corrupt registry fatal, and the one that reported a workspace colliding with
//! itself, were both invisible to library tests.
//!
//! Every run passes `AMT_REGISTRY` as a CHILD-process env var, so these tests
//! never touch the developer's real `~/.ametrite/registry.json` and don't race
//! each other through the process-global environment.

use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn amt(dir: &Path, registry: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_amt"))
        .current_dir(dir)
        .env("AMT_REGISTRY", registry)
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

/// A scratch home for the registry plus a repo directory to init in.
fn scratch(name: &str) -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = TempDir::new().unwrap();
    let registry = root.path().join("registry.json");
    let repo = root.path().join(name);
    std::fs::create_dir_all(&repo).unwrap();
    (root, registry, repo)
}

#[test]
fn init_derives_the_prefix_from_the_workspace_name() {
    let (_root, reg, repo) = scratch("PIN Golfing");
    let out = amt(&repo, &reg, &["init", "--name", "PIN Golfing"]);
    assert!(out.status.success(), "{}", stderr(&out));
    // The chosen prefix is announced, so a surprising derivation is visible now.
    assert!(stdout(&out).contains("PG-1"), "got: {}", stdout(&out));

    let issue = amt(&repo, &reg, &["issue", "create", "--title", "first"]);
    assert!(issue.status.success(), "{}", stderr(&issue));
    assert!(stdout(&issue).contains("PG-1"), "got: {}", stdout(&issue));
}

#[test]
fn init_refuses_a_derived_prefix_another_workspace_owns() {
    let (root, reg, first) = scratch("Pin Golf");
    assert!(amt(&first, &reg, &["init", "--name", "Pin Golf"])
        .status
        .success());

    // A different repo whose name derives the same PG.
    let second = root.path().join("Paper Goods");
    std::fs::create_dir_all(&second).unwrap();
    let out = amt(&second, &reg, &["init", "--name", "Paper Goods"]);
    assert!(!out.status.success(), "derived collision must refuse");
    let err = stderr(&out);
    assert!(err.contains("already used by workspace"), "got: {err}");
    assert!(
        err.contains("pin-golf"),
        "names the conflicting workspace: {err}"
    );
    // No half-built workspace left behind.
    assert!(!second.join(".ametrite").join("ametrite.db").exists());

    // The suggested alternative actually works — an error that hands out a
    // prefix the next run would also reject is worse than no hint at all.
    let suggested = err
        .rsplit("--prefix ")
        .next()
        .and_then(|s| s.split_whitespace().next())
        .expect("error names a concrete prefix to try")
        .to_string();
    let retry = amt(
        &second,
        &reg,
        &["init", "--name", "Paper Goods", "--prefix", &suggested],
    );
    assert!(
        retry.status.success(),
        "suggestion {suggested} failed: {}",
        stderr(&retry)
    );
}

#[test]
fn explicit_prefix_collision_warns_but_proceeds() {
    let (root, reg, first) = scratch("Pin Golf");
    assert!(amt(&first, &reg, &["init", "--name", "Pin Golf"])
        .status
        .success());

    let second = root.path().join("Paper Goods");
    std::fs::create_dir_all(&second).unwrap();
    let out = amt(
        &second,
        &reg,
        &["init", "--name", "Paper Goods", "--prefix", "PG"],
    );
    assert!(
        out.status.success(),
        "an explicit prefix is the human's call"
    );
    assert!(stderr(&out).contains("warning:"), "got: {}", stderr(&out));
    assert!(second.join(".ametrite").join("ametrite.db").exists());
}

#[test]
fn json_init_keeps_stdout_to_one_object_even_when_warning() {
    let (root, reg, first) = scratch("Pin Golf");
    assert!(amt(&first, &reg, &["init", "--name", "Pin Golf"])
        .status
        .success());

    let second = root.path().join("Paper Goods");
    std::fs::create_dir_all(&second).unwrap();
    let out = amt(
        &second,
        &reg,
        &["--json", "init", "--name", "Paper Goods", "--prefix", "PG"],
    );
    assert!(out.status.success());
    // SUITE_CONTRACTS §4: --json is exactly one JSON object on stdout, logs to
    // stderr. The collision warning must not pollute stdout.
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("stdout is one object");
    assert_eq!(v["prefix"], "PG");
    assert!(stderr(&out).contains("warning:"));
}

#[test]
fn re_init_reports_the_existing_workspace_not_a_self_collision() {
    let (_root, reg, repo) = scratch("PIN Golfing");
    assert!(amt(&repo, &reg, &["init", "--name", "PIN Golfing"])
        .status
        .success());

    // Running init again must say the workspace exists. Reporting a prefix
    // collision here would be the workspace conflicting with ITSELF, and its
    // "pass --prefix" advice leads nowhere (the retry fails too).
    let out = amt(&repo, &reg, &["init", "--name", "PIN Golfing"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("already exists"), "got: {err}");
    assert!(
        !err.contains("--prefix"),
        "must not give prefix advice: {err}"
    );
}

#[test]
fn a_broken_registry_never_blocks_init() {
    let (_root, reg, repo) = scratch("Solo Repo");
    std::fs::write(&reg, "{ this is not json").unwrap();
    // The workspace is fully usable without a registry, so a corrupt registry
    // file must degrade to "skip the collision check", not fail the command.
    let out = amt(&repo, &reg, &["init", "--name", "Solo Repo"]);
    assert!(
        out.status.success(),
        "corrupt registry blocked init: {}",
        stderr(&out)
    );
    assert!(repo.join(".ametrite").join("ametrite.db").exists());
}

#[test]
fn init_works_with_no_home_at_all() {
    let (_root, _reg, repo) = scratch("Container Repo");
    // Containers/CI/`env -i` have no HOME; the registry path can't even be
    // resolved. Init must still succeed.
    let out = Command::new(env!("CARGO_BIN_EXE_amt"))
        .current_dir(&repo)
        .env_remove("HOME")
        .env_remove("USERPROFILE")
        .env_remove("AMT_REGISTRY")
        .args(["init", "--name", "Container Repo"])
        .output()
        .expect("run amt");
    assert!(
        out.status.success(),
        "no-HOME init failed: {}",
        stderr(&out)
    );
    assert!(repo.join(".ametrite").join("ametrite.db").exists());
}
