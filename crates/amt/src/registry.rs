//! Global workspace registry — `~/.ametrite/registry.json`.
//!
//! Maps alias → workspace root (the directory containing `.ametrite/`).
//! `amt init` auto-registers, so every workspace on the machine shows up in
//! one web board and (R1) cross-workspace claims without extra setup.

use crate::db;
use crate::error::{msg, Result};
use crate::model::Issue;
use crate::store;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub fn registry_path() -> Result<PathBuf> {
    // AMT_REGISTRY pins the registry file explicitly (power-user override and
    // test isolation); otherwise it lives at ~/.ametrite/registry.json.
    if let Ok(path) = std::env::var("AMT_REGISTRY") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| msg("cannot locate home directory (HOME/USERPROFILE unset)"))?;
    Ok(PathBuf::from(home).join(".ametrite").join("registry.json"))
}

pub fn load() -> Result<BTreeMap<String, String>> {
    let path = registry_path()?;
    if !path.is_file() {
        return Ok(BTreeMap::new());
    }
    let text = std::fs::read_to_string(&path)?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| msg(format!("corrupt registry at {}", path.display())))?;
    let mut map = BTreeMap::new();
    if let Some(obj) = value.get("workspaces").and_then(|w| w.as_object()) {
        for (alias, root) in obj {
            if let Some(root) = root.as_str() {
                map.insert(alias.clone(), root.to_string());
            }
        }
    }
    Ok(map)
}

fn save(map: &BTreeMap<String, String>) -> Result<()> {
    let path = registry_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let value = serde_json::json!({ "workspaces": map });
    std::fs::write(&path, serde_json::to_string_pretty(&value)?)?;
    Ok(())
}

/// Register a workspace root under an alias. Overwrites the alias if the
/// path changed; no-ops if already identical.
pub fn add(alias: &str, root: &Path) -> Result<()> {
    let root = root
        .canonicalize()
        .map_err(|_| msg(format!("{} does not exist", root.display())))?;
    if !root.join(db::DB_DIR).join(db::DB_FILE).is_file() {
        return Err(msg(format!(
            "{} has no .ametrite workspace (run `amt init` there first)",
            root.display()
        )));
    }
    let mut map = load()?;
    map.insert(alias.to_string(), root.to_string_lossy().into_owned());
    save(&map)
}

pub fn remove(alias: &str) -> Result<bool> {
    let mut map = load()?;
    let existed = map.remove(alias).is_some();
    if existed {
        save(&map)?;
    }
    Ok(existed)
}

/// Best-effort auto-registration (used by `amt init`): never fails the
/// caller, since a workspace is fully usable without the registry.
pub fn try_register(alias: &str, root: &Path) {
    let _ = add(alias, root);
}

fn db_path(root: &str) -> PathBuf {
    Path::new(root).join(db::DB_DIR).join(db::DB_FILE)
}

/// Global priority rank (0 = highest) matching the SQL `PRIORITY_RANK`, so
/// cross-workspace sorting agrees with each workspace's own ordering.
fn priority_rank(priority: &str) -> usize {
    crate::model::PRIORITIES
        .iter()
        .position(|p| *p == priority)
        .unwrap_or(usize::MAX)
}

/// Open every registered workspace and run `f` against its connection,
/// returning `(alias, T)` pairs. Unreachable/stale workspaces are silently
/// skipped (that's `amt ws doctor`'s job to surface), so a fan-out over a
/// partially-broken registry still returns what it can.
pub fn for_each_workspace<T>(
    mut f: impl FnMut(&Connection) -> Result<T>,
) -> Result<Vec<(String, T)>> {
    let mut out = Vec::new();
    for (alias, root) in load()? {
        // A single unreachable-or-erroring workspace must not sink the whole
        // fan-out: skip both open failures AND per-workspace query errors
        // (corrupt FTS, schema drift) so the healthy workspaces still return.
        if let Ok(conn) = db::open(&db_path(&root)) {
            if let Ok(value) = f(&conn) {
                out.push((alias, value));
            }
        }
    }
    Ok(out)
}

/// Cross-workspace claim (R1): peek the best claimable issue in every
/// registered workspace, sort candidates globally by priority then age, then
/// claim the winner — falling through to the next candidate if a race loses
/// it. Returns `(alias, issue)`. Federated per-workspace DBs, per [[D-1]].
pub fn claim_any_workspace(
    agent: &str,
    project: Option<&str>,
    label: Option<&str>,
    ttl_secs: i64,
    cooldown_secs: i64,
) -> Result<Option<(String, Issue)>> {
    // (alias, root, best-claimable-candidate) for every workspace with work.
    let mut candidates: Vec<(String, String, store::PeekCandidate)> = Vec::new();
    for (alias, root) in load()? {
        let Ok(conn) = db::open(&db_path(&root)) else {
            continue;
        };
        if let Some(cand) = store::peek_next(&conn, agent, project, label, cooldown_secs)? {
            candidates.push((alias, root, cand));
        }
    }
    candidates.sort_by(|a, b| {
        priority_rank(&a.2.priority)
            .cmp(&priority_rank(&b.2.priority))
            .then_with(|| a.2.created_at.cmp(&b.2.created_at))
    });
    for (alias, root, cand) in candidates {
        // Tolerate a workspace that went unreachable between peek and claim —
        // fall through to the next candidate rather than failing the claim
        // (matches the peek loop's skip-broken-workspaces behavior above).
        let Ok(mut conn) = db::open(&db_path(&root)) else {
            continue;
        };
        if let Some(issue) = store::claim_key_guarded(
            &mut conn,
            &cand.key,
            agent,
            ttl_secs,
            project,
            label,
            cooldown_secs,
        )? {
            return Ok(Some((alias, issue)));
        }
    }
    Ok(None)
}
