//! End-to-end tests for R5 git integration. Each test builds a THROWAWAY git
//! repo in a TempDir (`git init`) and runs the `amt::git` helpers against it, so
//! nothing ever touches the real repo's `.git`.

use amt::git::{self, HookAction};
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// Run a git command in `dir`, asserting success. Configures identity + a
/// deterministic default branch so hooks and merge-base resolve predictably.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A fresh repo with one commit on branch `main` and a configured identity.
fn init_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    let p = dir.path();
    git(p, &["init", "-q", "-b", "main"]);
    git(p, &["config", "user.email", "t@example.com"]);
    git(p, &["config", "user.name", "Test"]);
    std::fs::write(p.join("README.md"), "hi\n").unwrap();
    git(p, &["add", "."]);
    git(p, &["commit", "-q", "-m", "init"]);
    dir
}

#[test]
fn repo_root_and_branch_resolve() {
    let dir = init_repo();
    let root = git::repo_root(dir.path()).unwrap().expect("in a repo");
    // Canonicalize both sides — macOS /var → /private/var symlink otherwise trips.
    assert_eq!(
        std::fs::canonicalize(&root).unwrap(),
        std::fs::canonicalize(dir.path()).unwrap()
    );
    assert_eq!(git::current_branch(dir.path()).unwrap().as_deref(), Some("main"));
}

#[test]
fn repo_root_none_outside_git() {
    let dir = TempDir::new().unwrap(); // not a git repo
    assert_eq!(git::repo_root(dir.path()).unwrap(), None);
    // Downstream helpers degrade to empty rather than erroring.
    assert!(git::commits_for_key(dir.path(), "AMT-1").unwrap().is_empty());
}

#[test]
fn install_hook_is_idempotent() {
    let dir = init_repo();
    let repo = dir.path();
    assert_eq!(git::install_hook(repo).unwrap(), HookAction::Installed);
    // Second install is a no-op.
    assert_eq!(git::install_hook(repo).unwrap(), HookAction::AlreadyInstalled);
    let hook = repo.join(".git/hooks/commit-msg");
    assert!(hook.exists());
    let body = std::fs::read_to_string(&hook).unwrap();
    assert!(body.contains(git::HOOK_MARKER));
    // Marker appears exactly once (idempotent, not doubled).
    assert_eq!(body.matches(git::HOOK_MARKER).count(), 1);
}

#[test]
fn install_hook_preserves_foreign_hook() {
    let dir = init_repo();
    let repo = dir.path();
    let hook = repo.join(".git/hooks/commit-msg");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\necho foreign\n").unwrap();
    assert_eq!(git::install_hook(repo).unwrap(), HookAction::Appended);
    let body = std::fs::read_to_string(&hook).unwrap();
    assert!(body.contains("echo foreign"), "foreign hook preserved");
    assert!(body.contains(git::HOOK_MARKER), "our block appended");
    // Uninstall removes only our block, keeping theirs.
    assert_eq!(git::uninstall_hook(repo).unwrap(), HookAction::Removed);
    let after = std::fs::read_to_string(&hook).unwrap();
    assert!(after.contains("echo foreign"));
    assert!(!after.contains(git::HOOK_MARKER));
}

#[test]
fn uninstall_deletes_our_solo_hook() {
    let dir = init_repo();
    let repo = dir.path();
    git::install_hook(repo).unwrap();
    let hook = repo.join(".git/hooks/commit-msg");
    assert!(hook.exists());
    assert_eq!(git::uninstall_hook(repo).unwrap(), HookAction::Removed);
    // Our solo hook had only a shebang + our block → file removed entirely.
    assert!(!hook.exists());
    // Uninstall on a clean repo reports NotInstalled.
    assert_eq!(git::uninstall_hook(repo).unwrap(), HookAction::NotInstalled);
}

#[test]
fn installed_hook_appends_refs_from_branch() {
    let dir = init_repo();
    let repo = dir.path();
    git::install_hook(repo).unwrap();
    // Branch carries the key; commit message doesn't reference it.
    git::create_branch(repo, "amt-7-fix-foo").unwrap();
    std::fs::write(repo.join("f.txt"), "x").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "do the work"]);
    let body = git(repo, &["log", "-1", "--pretty=%B"]);
    assert!(
        body.contains("Refs: AMT-7"),
        "hook should append `Refs: AMT-7`, got: {body:?}"
    );
    // A second commit whose message already has the ref must NOT double it.
    std::fs::write(repo.join("g.txt"), "y").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "more\n\nRefs: AMT-7"]);
    let body2 = git(repo, &["log", "-1", "--pretty=%B"]);
    assert_eq!(body2.matches("Refs: AMT-7").count(), 1, "no duplicate ref");
}

#[test]
fn hook_noop_on_keyless_branch() {
    let dir = init_repo();
    let repo = dir.path();
    git::install_hook(repo).unwrap();
    // main has no key → no Refs appended.
    std::fs::write(repo.join("f.txt"), "x").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "plain commit"]);
    let body = git(repo, &["log", "-1", "--pretty=%B"]);
    assert!(!body.contains("Refs:"), "keyless branch adds nothing: {body:?}");
}

#[test]
fn create_branch_slug_and_checkout() {
    let dir = init_repo();
    let repo = dir.path();
    git::create_branch(repo, "amt-3-add-git-integration").unwrap();
    assert_eq!(
        git::current_branch(repo).unwrap().as_deref(),
        Some("amt-3-add-git-integration")
    );
    // Re-creating an existing branch errors (surfaced to the user).
    assert!(git::create_branch(repo, "amt-3-add-git-integration").is_err());
}

#[test]
fn commits_for_key_greps_message() {
    let dir = init_repo();
    let repo = dir.path();
    std::fs::write(repo.join("a.txt"), "1").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "AMT-9: first\n\nRefs: AMT-9"]);
    std::fs::write(repo.join("b.txt"), "2").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "unrelated change"]);
    let hits = git::commits_for_key(repo, "AMT-9").unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].subject.contains("AMT-9: first"));
    // A key with no commits yields an empty list, not an error.
    assert!(git::commits_for_key(repo, "AMT-999").unwrap().is_empty());
}

#[test]
fn commits_since_base_scopes_to_branch() {
    let dir = init_repo();
    let repo = dir.path();
    // A commit on main that references the key should NOT appear in the range.
    std::fs::write(repo.join("base.txt"), "0").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "AMT-5: on main (excluded)"]);
    // Branch off and add two referencing commits.
    git::create_branch(repo, "amt-5-git").unwrap();
    std::fs::write(repo.join("c.txt"), "1").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "AMT-5: feature one"]);
    std::fs::write(repo.join("d.txt"), "2").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "work\n\nRefs: AMT-5"]);
    let scoped = git::commits_since_base(repo, "AMT-5").unwrap();
    // Only the two branch commits — the main-branch one is before the merge-base.
    assert_eq!(scoped.len(), 2, "got: {scoped:?}");
    assert!(scoped.iter().all(|c| !c.subject.contains("on main")));
}

#[test]
fn hook_appends_refs_on_first_commit_of_unborn_branch() {
    // A fresh repo with NO commits: the branch is "unborn", where
    // `git rev-parse --abbrev-ref HEAD` returns "HEAD" (no key). The hook uses
    // symbolic-ref, which still resolves the branch name.
    let dir = TempDir::new().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["config", "user.email", "t@example.com"]);
    git(repo, &["config", "user.name", "Test"]);
    git::install_hook(repo).unwrap();
    git::create_branch(repo, "amt-9-first").unwrap();
    std::fs::write(repo.join("f.txt"), "x").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "first commit"]);
    let body = git(repo, &["log", "-1", "--pretty=%B"]);
    assert!(body.contains("Refs: AMT-9"), "unborn-branch first commit: {body:?}");
}
