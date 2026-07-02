//! Global workspace registry — `~/.ametrite/registry.json`.
//!
//! Maps alias → workspace root (the directory containing `.ametrite/`).
//! `amt init` auto-registers, so every workspace on the machine shows up in
//! one web board and (R1) cross-workspace claims without extra setup.

use crate::db;
use crate::error::{msg, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub fn registry_path() -> Result<PathBuf> {
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
