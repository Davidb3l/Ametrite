//! CLI-level tests for the top-level `amt comment` alias (AMT-31).
//!
//! Agents kept typing `amt comment <KEY> …`, hitting "unrecognized subcommand",
//! and concluding amt had no comment command at all. The alias must behave
//! exactly like `amt issue comment` — same flags, same `--json` contract, same
//! failures — so these drive the real binary and compare the two side by side.
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

/// An initialized workspace holding one issue, CT-1, with its own registry.
fn workspace() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = TempDir::new().unwrap();
    let registry = root.path().join("registry.json");
    let repo = root.path().join("comment-test");
    std::fs::create_dir_all(&repo).unwrap();
    let init = amt(
        &repo,
        &registry,
        &["init", "--name", "Comment Test", "--prefix", "CT"],
    );
    assert!(init.status.success(), "{}", stderr(&init));
    let issue = amt(&repo, &registry, &["issue", "create", "--title", "target"]);
    assert!(issue.status.success(), "{}", stderr(&issue));
    (root, registry, repo)
}

/// The comment entries on CT-1's activity log, as (author, body) pairs.
fn comments(repo: &Path, reg: &Path) -> Vec<(String, String)> {
    let out = amt(repo, reg, &["issue", "show", "CT-1", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    v["activity"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["kind"] == "comment")
        .map(|a| {
            (
                a["author"].as_str().unwrap().to_string(),
                a["body"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn comment_alias_writes_the_same_comment_as_issue_comment() {
    let (_root, reg, repo) = workspace();

    let alias = amt(&repo, &reg, &["comment", "CT-1", "-m", "via alias"]);
    assert!(alias.status.success(), "{}", stderr(&alias));
    let long = amt(
        &repo,
        &reg,
        &["issue", "comment", "CT-1", "-m", "via issue comment"],
    );
    assert!(long.status.success(), "{}", stderr(&long));
    // Same human output, modulo nothing.
    assert_eq!(stdout(&alias), "commented on CT-1\n");
    assert_eq!(stdout(&alias), stdout(&long));

    // Both land as ordinary comments, authored from $AMT_AGENT by default.
    assert_eq!(
        comments(&repo, &reg),
        vec![
            ("worker-1".to_string(), "via alias".to_string()),
            ("worker-1".to_string(), "via issue comment".to_string()),
        ]
    );
}

#[test]
fn comment_alias_takes_the_same_flags() {
    let (_root, reg, repo) = workspace();
    // --body (long form) and --author, with the key after the flags.
    let out = amt(
        &repo,
        &reg,
        &[
            "comment",
            "--body",
            "long form",
            "--author",
            "reviewer",
            "CT-1",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    // --workspace is honored too, from a directory that is not the workspace.
    let elsewhere = TempDir::new().unwrap();
    let out = amt(
        elsewhere.path(),
        &reg,
        &[
            "comment",
            "CT-1",
            "-m",
            "from elsewhere",
            "--workspace",
            repo.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        comments(&repo, &reg),
        vec![
            ("reviewer".to_string(), "long form".to_string()),
            ("worker-1".to_string(), "from elsewhere".to_string()),
        ]
    );
}

#[test]
fn comment_alias_json_matches_issue_comment_json() {
    let (_root, reg, repo) = workspace();
    let alias = amt(&repo, &reg, &["comment", "CT-1", "-m", "a", "--json"]);
    let long = amt(
        &repo,
        &reg,
        &["issue", "comment", "CT-1", "-m", "b", "--json"],
    );
    assert!(alias.status.success(), "{}", stderr(&alias));
    assert!(long.status.success(), "{}", stderr(&long));
    // Exactly one JSON object on stdout (SUITE_CONTRACTS §4), identical bytes.
    let v: serde_json::Value = serde_json::from_str(&stdout(&alias)).unwrap();
    assert_eq!(v, serde_json::json!({ "ok": true }));
    assert_eq!(stdout(&alias), stdout(&long));
}

#[test]
fn comment_alias_fails_like_issue_comment() {
    let (_root, reg, repo) = workspace();
    // Unknown issue: same exit status, same error, nothing on stdout.
    let alias = amt(&repo, &reg, &["comment", "CT-9", "-m", "x", "--json"]);
    let long = amt(
        &repo,
        &reg,
        &["issue", "comment", "CT-9", "-m", "x", "--json"],
    );
    assert!(!alias.status.success());
    assert_eq!(alias.status.code(), long.status.code());
    assert_eq!(stderr(&alias), stderr(&long));
    assert!(stdout(&alias).is_empty(), "got: {}", stdout(&alias));

    // Missing body is a usage error, not a silent empty comment.
    let out = amt(&repo, &reg, &["comment", "CT-1"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--body"), "got: {}", stderr(&out));
    assert!(comments(&repo, &reg).is_empty());
}

#[test]
fn top_level_help_lists_the_comment_alias() {
    let (_root, reg, repo) = workspace();
    let out = amt(&repo, &reg, &["--help"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let help = stdout(&out);
    let line = help
        .lines()
        .find(|l| l.trim_start().starts_with("comment "))
        .unwrap_or_else(|| panic!("no `comment` row in --help:\n{help}"));
    assert!(line.contains("issue comment"), "got: {line}");
}
