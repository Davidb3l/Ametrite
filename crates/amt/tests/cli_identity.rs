//! CLI-level tests for who an `amt` command acts as (AMT-47, AMT-36).
//!
//! AGENTS.md tells agents to pass `--agent` on every command, but before
//! AMT-47 only claim/release/brief/seed accepted it (comment/decide took
//! `--author`, and issue/note/dep writes took neither and fell back to
//! $USER). `--agent` is now one global flag, `--author` its alias, and an
//! empty identity falls through instead of being recorded (AMT-36).
//!
//! Every run passes `AMT_REGISTRY` as a CHILD-process env var, so these tests
//! never touch the developer's real `~/.ametrite/registry.json`.

use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn amt_env(dir: &Path, registry: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amt"));
    cmd.current_dir(dir)
        .env("AMT_REGISTRY", registry)
        .env("USER", "human")
        .env_remove("AMT_AGENT");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.args(args).output().expect("run amt")
}
fn amt(dir: &Path, registry: &Path, args: &[&str]) -> Output {
    amt_env(dir, registry, &[], args)
}
fn ok(o: Output) -> Output {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    o
}

fn workspace() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = TempDir::new().unwrap();
    let registry = root.path().join("registry.json");
    let repo = root.path().join("identity-test");
    std::fs::create_dir_all(&repo).unwrap();
    ok(amt(
        &repo,
        &registry,
        &["init", "--name", "Identity", "--prefix", "ID"],
    ));
    (root, registry, repo)
}

/// (author, body) of each activity entry on an issue.
fn activity(repo: &Path, reg: &Path, key: &str) -> Vec<(String, String)> {
    let out = ok(amt(repo, reg, &["issue", "show", key, "--json"]));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    v["activity"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["author"].as_str().unwrap().to_string(),
                a["body"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn every_write_command_takes_agent_before_or_after_the_subcommand() {
    let (_root, reg, repo) = workspace();
    ok(amt(
        &repo,
        &reg,
        &["issue", "create", "--title", "One", "--agent", "bot-create"],
    ));
    ok(amt(&repo, &reg, &["issue", "create", "--title", "Two"]));
    ok(amt(
        &repo,
        &reg,
        &[
            "--agent",
            "bot-update",
            "issue",
            "update",
            "ID-1",
            "--priority",
            "high",
        ],
    ));
    ok(amt(
        &repo,
        &reg,
        &["dep", "add", "ID-1", "ID-2", "--agent", "bot-dep"],
    ));
    ok(amt(
        &repo,
        &reg,
        &[
            "comment",
            "ID-1",
            "-m",
            "progress",
            "--agent",
            "bot-comment",
        ],
    ));
    ok(amt(
        &repo,
        &reg,
        &[
            "decide",
            "--issue",
            "ID-1",
            "--title",
            "Pick A",
            "--agent",
            "bot-decide",
        ],
    ));
    ok(amt(
        &repo,
        &reg,
        &["claim", "--issue", "ID-1", "--agent", "bot-claim"],
    ));
    ok(amt(
        &repo,
        &reg,
        &[
            "release",
            "ID-1",
            "--agent",
            "bot-claim",
            "--status",
            "done",
        ],
    ));

    let authors: Vec<String> = activity(&repo, &reg, "ID-1")
        .into_iter()
        .map(|(a, _)| a)
        .collect();
    for who in [
        "bot-create",
        "bot-update",
        "bot-comment",
        "bot-decide",
        "bot-claim",
    ] {
        assert!(
            authors.iter().any(|a| a == who),
            "{who} missing: {authors:?}"
        );
    }
    assert!(activity(&repo, &reg, "ID-2")
        .iter()
        .any(|(a, b)| a == "bot-dep" && b.contains("blocked by")));
    assert!(
        !authors.iter().any(|a| a == "human"),
        "no write fell back to $USER: {authors:?}"
    );
}

#[test]
fn author_is_an_alias_and_env_is_the_default() {
    let (_root, reg, repo) = workspace();
    ok(amt_env(
        &repo,
        &reg,
        &[("AMT_AGENT", "env-agent")],
        &["issue", "create", "--title", "One"],
    ));
    ok(amt(
        &repo,
        &reg,
        &["comment", "ID-1", "-m", "old flag", "--author", "legacy"],
    ));
    let log = activity(&repo, &reg, "ID-1");
    assert_eq!(log[0].0, "env-agent");
    assert!(log.iter().any(|(a, b)| a == "legacy" && b == "old flag"));
}

/// AMT-36: `AMT_AGENT=` (or `--agent ""`) used to record an empty author.
#[test]
fn an_empty_identity_falls_through_to_user() {
    let (_root, reg, repo) = workspace();
    ok(amt_env(
        &repo,
        &reg,
        &[("AMT_AGENT", "")],
        &["issue", "create", "--title", "One"],
    ));
    ok(amt(
        &repo,
        &reg,
        &["comment", "ID-1", "-m", "hi", "--agent", "  "],
    ));
    let log = activity(&repo, &reg, "ID-1");
    assert!(log.iter().all(|(a, _)| a == "human"), "{log:?}");
}

/// AMT-36: a blank comment is refused, not logged as an empty entry.
#[test]
fn a_blank_comment_is_refused() {
    let (_root, reg, repo) = workspace();
    ok(amt(&repo, &reg, &["issue", "create", "--title", "One"]));
    let out = amt(&repo, &reg, &["comment", "ID-1", "-m", "   "]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("comment body is empty"));
    assert!(!activity(&repo, &reg, "ID-1")
        .iter()
        .any(|(_, b)| b.trim().is_empty()));
}
