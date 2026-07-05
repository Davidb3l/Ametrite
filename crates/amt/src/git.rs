//! Git integration (R5): shell out to `git` via `std::process::Command`.
//!
//! Every helper degrades gracefully when there is no git repo (or `git` is
//! missing): the `Command`-based functions return `Ok(None)` / `Ok(vec![])`
//! rather than erroring, so callers can treat "not a git repo" as a silent
//! no-op. The pure functions (`extract_key`, `hook_script`) carry the logic
//! that is worth unit-testing in isolation.

use crate::error::Result;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Marker line embedded in the commit-msg hook so `hook install` is idempotent
/// and `hook uninstall` can find (and only remove) the block we own.
pub const HOOK_MARKER: &str = "# >>> ametrite commit-msg (amt hook) >>>";
const HOOK_END: &str = "# <<< ametrite commit-msg (amt hook) <<<";

/// Matches an issue key anywhere in a branch name: an uppercase prefix, a dash,
/// and a number (e.g. `AMT-7`, `CLAP-12`). Case-insensitive on the branch so
/// `amt-7-fix` also resolves; the returned key is normalized to uppercase.
fn key_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b([A-Za-z][A-Za-z0-9]*-[0-9]+)\b").unwrap())
}

/// Extract the first issue key from a branch name, uppercased.
/// `AMT-7-fix-foo` → `Some("AMT-7")`; `main` → `None`.
pub fn extract_key(branch: &str) -> Option<String> {
    key_re()
        .captures(branch)
        .map(|c| c[1].to_uppercase())
}

/// The commit-msg hook body we install. It re-derives the issue key from the
/// current branch at commit time and appends `Refs: <KEY>` unless the message
/// already references it. Written as a self-contained POSIX-sh block wrapped in
/// our markers so it can be appended to a pre-existing hook without clobbering
/// it.
pub fn hook_script() -> String {
    // $1 is the path to the commit-message file (git contract for commit-msg).
    format!(
        r#"{HOOK_MARKER}
# Auto-appends `Refs: <ISSUE-KEY>` when the branch name carries an issue key.
# Managed by `amt hook`; edit inside these markers at your own risk.
# symbolic-ref resolves the branch name even on an unborn branch (the first
# commit in a fresh repo), where `rev-parse --abbrev-ref HEAD` yields "HEAD".
branch="$(git symbolic-ref --short HEAD 2>/dev/null)"
key="$(printf '%s' "$branch" | grep -oE '[A-Za-z][A-Za-z0-9]*-[0-9]+' | head -n1 | tr '[:lower:]' '[:upper:]')"
if [ -n "$key" ] && ! grep -qiE "^Refs:[[:space:]]*$key\b" "$1"; then
  printf '\nRefs: %s\n' "$key" >> "$1"
fi
{HOOK_END}
"#
    )
}

/// Find the git repo root containing `start` (or any ancestor), via
/// `git rev-parse --show-toplevel`. Returns `Ok(None)` when not in a repo or
/// git is unavailable.
pub fn repo_root(start: &Path) -> Result<Option<PathBuf>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--show-toplevel"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if path.is_empty() {
                Ok(None)
            } else {
                Ok(Some(PathBuf::from(path)))
            }
        }
        _ => Ok(None), // not a git repo, or `git` missing → silent no-op
    }
}

/// The current branch name, or `None` when detached / not a repo.
pub fn current_branch(repo: &Path) -> Result<Option<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let b = String::from_utf8_lossy(&o.stdout).trim().to_string();
            // Detached HEAD reports "HEAD"; treat that as "no branch".
            if b.is_empty() || b == "HEAD" {
                Ok(None)
            } else {
                Ok(Some(b))
            }
        }
        _ => Ok(None),
    }
}

/// A commit that references an issue key, as surfaced by `git log --oneline`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Commit {
    pub hash: String,
    pub subject: String,
}

/// Run `git log --grep=<pattern> --oneline` over `range` (e.g. `base..HEAD`,
/// or `None` for the whole history reachable from HEAD) and parse the commits.
/// The grep is fixed-string and matches the message body, so `Refs: AMT-7`
/// trailers and inline mentions both count. Returns `vec![]` outside a repo.
pub fn log_grep(repo: &Path, pattern: &str, range: Option<&str>) -> Result<Vec<Commit>> {
    let mut args: Vec<String> = vec![
        "-C".into(),
        repo.to_string_lossy().into_owned(),
        "log".into(),
        "--no-color".into(),
        // Fixed-string, extended-regex-safe grep for the literal key.
        "--fixed-strings".into(),
        format!("--grep={pattern}"),
        // "<hash> <subject>" — stable, easy to split on the first space.
        "--pretty=%h %s".into(),
    ];
    if let Some(r) = range {
        args.push(r.to_string());
    }
    let out = Command::new("git").args(&args).output();
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            Ok(parse_log(&text))
        }
        _ => Ok(Vec::new()),
    }
}

/// Parse `%h %s` lines into commits. Pure so it can be unit-tested without git.
fn parse_log(text: &str) -> Vec<Commit> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_end();
            if line.is_empty() {
                return None;
            }
            let (hash, subject) = match line.split_once(' ') {
                Some((h, s)) => (h.to_string(), s.to_string()),
                None => (line.to_string(), String::new()),
            };
            Some(Commit { hash, subject })
        })
        .collect()
}

/// Commits referencing `key` reachable from HEAD (whole branch history).
/// Used by `issue show`. Silent no-op outside a repo.
pub fn commits_for_key(repo: &Path, key: &str) -> Result<Vec<Commit>> {
    log_grep(repo, key, None)
}

/// The default branch's short name (`main`, `master`, …) inferred from
/// `origin/HEAD`, falling back to whichever of `main`/`master` exists locally.
/// Returns `None` if neither can be determined.
fn default_branch(repo: &Path) -> Option<String> {
    // origin/HEAD → refs/remotes/origin/main → "main"
    if let Ok(o) = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--abbrev-ref", "origin/HEAD"])
        .output()
    {
        if o.status.success() {
            let full = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if let Some(name) = full.rsplit('/').next() {
                if !name.is_empty() && name != "HEAD" {
                    return Some(name.to_string());
                }
            }
        }
    }
    for cand in ["main", "master"] {
        let ok = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "--verify", "--quiet", cand])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some(cand.to_string());
        }
    }
    None
}

/// Commits on the current branch that reference `key`, scoped to work done
/// since it diverged from the default branch. Used by `release` to summarize
/// what landed. Falls back to the whole branch history when no merge-base with
/// a default branch can be found (e.g. a fresh repo with only `main`). Silent
/// no-op outside a repo.
pub fn commits_since_base(repo: &Path, key: &str) -> Result<Vec<Commit>> {
    let range = release_range(repo);
    log_grep(repo, key, range.as_deref())
}

/// Compute the `<base>..HEAD` range for `release`, or `None` (whole history) if
/// we're on the default branch itself or can't find a sensible base. Pure given
/// the two git lookups it delegates to, so the fallback logic is explicit.
fn release_range(repo: &Path) -> Option<String> {
    let default = default_branch(repo)?;
    let current = current_branch(repo).ok().flatten();
    // On the default branch there's no feature range — scan the whole history.
    if current.as_deref() == Some(default.as_str()) {
        return None;
    }
    // merge-base <default> HEAD — the point the branch forked from.
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", &default, "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let base = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if base.is_empty() {
        None
    } else {
        Some(format!("{base}..HEAD"))
    }
}

/// Combine an optional user comment with a markdown summary of `commits` into
/// the closing comment for `release`. Returns `None` only when there is neither
/// a user comment nor any commits (so `release` passes no comment through).
/// Pure, so the formatting is unit-testable without git.
pub fn build_release_comment(user: Option<&str>, commits: &[Commit]) -> Option<String> {
    let user = user.map(str::trim).filter(|s| !s.is_empty());
    if commits.is_empty() {
        return user.map(str::to_string);
    }
    let mut lines = String::from("Commits:\n");
    for c in commits {
        lines.push_str(&format!("- {} {}\n", c.hash, c.subject));
    }
    let commits_block = lines.trim_end();
    match user {
        Some(u) => Some(format!("{u}\n\n{commits_block}")),
        None => Some(commits_block.to_string()),
    }
}

/// Create and check out `branch` in `repo` via `git checkout -b`.
/// Errors (surfaced to the user) when the branch already exists or git fails.
pub fn create_branch(repo: &Path, branch: &str) -> Result<()> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["checkout", "-b", branch])
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(crate::error::msg(if err.is_empty() {
            format!("git checkout -b {branch} failed")
        } else {
            err
        }))
    }
}

/// Path to the commit-msg hook, honoring `core.hooksPath` if configured,
/// otherwise `<repo>/.git/hooks/commit-msg`. Uses `git rev-parse --git-path`
/// so worktrees and custom hook dirs resolve correctly.
fn commit_msg_hook_path(repo: &Path) -> Result<PathBuf> {
    // `git config core.hooksPath` wins when set.
    if let Ok(o) = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["config", "--get", "core.hooksPath"])
        .output()
    {
        if o.status.success() {
            let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !p.is_empty() {
                let base = PathBuf::from(&p);
                let dir = if base.is_absolute() {
                    base
                } else {
                    repo.join(base)
                };
                return Ok(dir.join("commit-msg"));
            }
        }
    }
    // Default: <git-dir>/hooks/commit-msg. `--git-path hooks` resolves the real
    // hooks dir even inside a linked worktree.
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--git-path", "hooks"])
        .output()?;
    let hooks = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let hooks_dir = if Path::new(&hooks).is_absolute() {
        PathBuf::from(hooks)
    } else {
        repo.join(hooks)
    };
    Ok(hooks_dir.join("commit-msg"))
}

/// Outcome of `hook install` / `uninstall`, so the CLI can report precisely.
#[derive(Debug, PartialEq)]
pub enum HookAction {
    Installed,
    AlreadyInstalled,
    Appended, // added our block to a pre-existing (foreign) hook
    Removed,
    NotInstalled,
}

/// Install the commit-msg hook idempotently. If a hook already exists:
/// - contains our marker → no-op (`AlreadyInstalled`);
/// - foreign hook → append our block, preserving theirs (`Appended`).
///
/// A fresh hook file is created executable (`Installed`).
pub fn install_hook(repo: &Path) -> Result<HookAction> {
    let path = commit_msg_hook_path(repo)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let script = hook_script();
    if path.exists() {
        let existing = std::fs::read_to_string(&path)?;
        if existing.contains(HOOK_MARKER) {
            return Ok(HookAction::AlreadyInstalled);
        }
        // Append our block to the foreign hook, keeping a clean separator.
        let mut merged = existing;
        if !merged.ends_with('\n') {
            merged.push('\n');
        }
        merged.push('\n');
        merged.push_str(&script);
        std::fs::write(&path, merged)?;
        set_executable(&path)?;
        Ok(HookAction::Appended)
    } else {
        let contents = format!("#!/bin/sh\n{script}");
        std::fs::write(&path, contents)?;
        set_executable(&path)?;
        Ok(HookAction::Installed)
    }
}

/// Remove only our marked block. If that leaves an empty-ish hook (just a
/// shebang / whitespace), delete the file. Foreign content is preserved.
pub fn uninstall_hook(repo: &Path) -> Result<HookAction> {
    let path = commit_msg_hook_path(repo)?;
    if !path.exists() {
        return Ok(HookAction::NotInstalled);
    }
    let existing = std::fs::read_to_string(&path)?;
    if !existing.contains(HOOK_MARKER) {
        return Ok(HookAction::NotInstalled);
    }
    let stripped = strip_block(&existing);
    // If nothing meaningful survives (only a shebang/blank lines), drop the file.
    let meaningful = stripped
        .lines()
        .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with("#!"));
    if meaningful {
        std::fs::write(&path, stripped)?;
    } else {
        std::fs::remove_file(&path)?;
    }
    Ok(HookAction::Removed)
}

/// Remove our marker-delimited block (inclusive) from a hook file's text.
/// Pure, so the surgery is unit-testable. Leaves everything outside the markers
/// intact and collapses the blank line we inserted before the block.
fn strip_block(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        if line.trim() == HOOK_MARKER {
            in_block = true;
            // Drop a single trailing blank separator we added on append.
            if out.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
                out.pop();
            }
            continue;
        }
        if line.trim() == HOOK_END {
            in_block = false;
            continue;
        }
        if !in_block {
            out.push(line);
        }
    }
    let mut joined = out.join("\n");
    if !joined.is_empty() && !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(()) // no-op on non-unix; git for Windows runs hooks via sh regardless
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_key_from_branch() {
        assert_eq!(extract_key("AMT-7-fix-foo"), Some("AMT-7".into()));
        assert_eq!(extract_key("feature/AMT-42"), Some("AMT-42".into()));
        assert_eq!(extract_key("amt-7-fix"), Some("AMT-7".into()));
        assert_eq!(extract_key("CLAP-12"), Some("CLAP-12".into()));
        assert_eq!(extract_key("main"), None);
        assert_eq!(extract_key("release-v2"), None); // no digits-after-dash key
        assert_eq!(extract_key(""), None);
    }

    #[test]
    fn extracts_first_key_only() {
        // A branch mentioning two keys resolves to the first.
        assert_eq!(extract_key("AMT-1-then-AMT-2"), Some("AMT-1".into()));
    }

    #[test]
    fn hook_script_is_idempotent_and_marked() {
        let s = hook_script();
        assert!(s.contains(HOOK_MARKER));
        assert!(s.contains(HOOK_END));
        // Guards against double-appending: greps for an existing Refs: line.
        assert!(s.contains("grep -qiE"));
        assert!(s.contains("Refs: %s"));
    }

    #[test]
    fn parses_oneline_log() {
        let commits = parse_log("abc1234 AMT-7: do the thing\ndef5678 fix: tidy up\n");
        assert_eq!(
            commits,
            vec![
                Commit { hash: "abc1234".into(), subject: "AMT-7: do the thing".into() },
                Commit { hash: "def5678".into(), subject: "fix: tidy up".into() },
            ]
        );
        assert!(parse_log("").is_empty());
    }

    #[test]
    fn release_comment_merges_user_and_commits() {
        let commits = vec![
            Commit { hash: "abc1234".into(), subject: "AMT-7: part one".into() },
            Commit { hash: "def5678".into(), subject: "AMT-7: part two".into() },
        ];
        // No comment, no commits → None (release passes nothing through).
        assert_eq!(build_release_comment(None, &[]), None);
        // Blank user comment is treated as absent.
        assert_eq!(build_release_comment(Some("   "), &[]), None);
        // Only a user comment survives verbatim.
        assert_eq!(build_release_comment(Some("done"), &[]), Some("done".into()));
        // Only commits → a Commits: block.
        let only_commits = build_release_comment(None, &commits).unwrap();
        assert!(only_commits.starts_with("Commits:\n"));
        assert!(only_commits.contains("- abc1234 AMT-7: part one"));
        assert!(only_commits.contains("- def5678 AMT-7: part two"));
        // Both → user text, blank line, then the block.
        let both = build_release_comment(Some("shipped"), &commits).unwrap();
        assert!(both.starts_with("shipped\n\nCommits:\n"));
    }

    #[test]
    fn strip_block_removes_only_our_block() {
        let foreign = "#!/bin/sh\necho hi\n";
        let combined = format!("{foreign}\n{}", hook_script());
        let stripped = strip_block(&combined);
        assert!(stripped.contains("echo hi"));
        assert!(!stripped.contains(HOOK_MARKER));
        assert!(!stripped.contains("Refs: %s"));
    }

    #[test]
    fn strip_block_leaves_bare_shebang_when_only_our_block() {
        let only = format!("#!/bin/sh\n{}", hook_script());
        let stripped = strip_block(&only);
        assert!(!stripped.contains(HOOK_MARKER));
        // Only a shebang survives — the caller treats this as "delete the file".
        assert!(stripped
            .lines()
            .all(|l| l.trim().is_empty() || l.trim_start().starts_with("#!")));
    }
}
