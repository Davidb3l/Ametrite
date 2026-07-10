//! `amt upgrade` (AMT-19) — delegate to whatever installed this binary.
//!
//! The engine ships no HTTP client (its whole dependency list is rusqlite,
//! serde, serde_json, clap, regex), so rather than download and swap its own
//! executable, `upgrade` re-invokes the installer that owns it: Homebrew,
//! `cargo install`, or the cargo-dist installer script.

use crate::error::{msg, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const REPO: &str = env!("CARGO_PKG_REPOSITORY");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How this binary appears to have been installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Install {
    Homebrew,
    Cargo,
    /// The cargo-dist shell/PowerShell installer (also the fallback).
    Installer,
}

impl Install {
    pub fn as_str(self) -> &'static str {
        match self {
            Install::Homebrew => "homebrew",
            Install::Cargo => "cargo",
            Install::Installer => "installer",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "brew" | "homebrew" => Ok(Install::Homebrew),
            "cargo" => Ok(Install::Cargo),
            "installer" => Ok(Install::Installer),
            other => Err(msg(format!(
                "unknown install method '{other}' (expected: brew, cargo, installer)"
            ))),
        }
    }
}

/// Classify how this binary was installed. Pure — every filesystem fact is
/// passed in, so the precedence rules are unit-testable.
///
/// A cargo-dist install receipt outranks a `CARGO_HOME` location because dist's
/// *default* install path IS `$CARGO_HOME/bin`; the two are indistinguishable by
/// path alone, and re-running `cargo install` on a dist install would silently
/// switch the user to a source build.
pub fn detect(exe: &Path, cargo_home: Option<&Path>, has_receipt: bool) -> Install {
    let s = exe.to_string_lossy();
    if s.contains("/Cellar/") || s.contains("/homebrew/") || s.contains("/linuxbrew/") {
        return Install::Homebrew;
    }
    if has_receipt {
        return Install::Installer;
    }
    if let Some(home) = cargo_home {
        if exe.starts_with(home) {
            return Install::Cargo;
        }
    }
    Install::Installer
}

/// The command that upgrades a given install kind. Pure/testable; `windows`
/// selects the PowerShell installer over the shell one.
pub fn upgrade_command(install: Install, windows: bool) -> (String, Vec<String>) {
    match install {
        Install::Homebrew => ("brew".into(), vec!["upgrade".into(), "amt".into()]),
        Install::Cargo => (
            "cargo".into(),
            vec![
                "install".into(),
                "--git".into(),
                REPO.into(),
                "amt".into(),
                "--locked".into(),
            ],
        ),
        Install::Installer if windows => (
            "powershell".into(),
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                format!("irm {REPO}/releases/latest/download/amt-installer.ps1 | iex"),
            ],
        ),
        Install::Installer => (
            "sh".into(),
            vec![
                "-c".into(),
                format!(
                    "curl --proto '=https' --tlsv1.2 -LsSf \
                     {REPO}/releases/latest/download/amt-installer.sh | sh"
                ),
            ],
        ),
    }
}

/// Where the cargo-dist installer records that it owns this binary.
fn receipt_path() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|d| Path::new(&d).join("amt").join("amt-receipt.json"))
    } else {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
        Some(base.join("amt").join("amt-receipt.json"))
    }
}

fn cargo_home() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cargo")))
}

/// Detect the install kind of the currently-running binary.
pub fn current_install() -> Result<Install> {
    let exe = std::env::current_exe()?;
    // Resolve symlinks (Homebrew links Cellar binaries into its bin dir).
    let exe = exe.canonicalize().unwrap_or(exe);
    let has_receipt = receipt_path().is_some_and(|p| p.is_file());
    Ok(detect(&exe, cargo_home().as_deref(), has_receipt))
}

/// Resolve which install owns this binary and the command that upgrades it.
/// Performs no IO beyond detection, and never prints — the caller owns output
/// so `--json` can keep stdout to a single object.
pub fn plan(method: Option<&str>) -> Result<(Install, String, Vec<String>)> {
    let install = match method {
        Some(m) => Install::parse(m)?,
        None => current_install()?,
    };
    let (program, args) = upgrade_command(install, cfg!(windows));
    Ok((install, program, args))
}

/// Run the upgrade command. With `capture_stdout` (i.e. `--json`) the child's
/// output is forwarded to stderr instead of stdout, so the caller's JSON object
/// remains the only thing on stdout.
pub fn execute(program: &str, args: &[String], capture_stdout: bool) -> Result<()> {
    let rendered = format!("{program} {}", args.join(" "));
    let fail = |e: std::io::Error| {
        msg(format!(
            "failed to run '{program}': {e} — upgrade manually with: {rendered}"
        ))
    };
    let status = if capture_stdout {
        let out = Command::new(program).args(args).output().map_err(fail)?;
        if !out.stdout.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&out.stdout));
        }
        if !out.stderr.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&out.stderr));
        }
        out.status
    } else {
        Command::new(program).args(args).status().map_err(fail)?
    };
    if !status.success() {
        return Err(msg(format!(
            "upgrade command failed ({status}) — try manually: {rendered}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn homebrew_wins_on_cellar_paths() {
        let cargo = PathBuf::from("/home/u/.cargo");
        for p in [
            "/opt/homebrew/Cellar/amt/0.1.0/bin/amt",
            "/usr/local/Cellar/amt/0.1.0/bin/amt",
            "/home/linuxbrew/.linuxbrew/bin/amt",
        ] {
            assert_eq!(
                detect(Path::new(p), Some(&cargo), false),
                Install::Homebrew,
                "{p}"
            );
        }
    }

    #[test]
    fn receipt_outranks_cargo_home_because_dist_installs_there_too() {
        // dist's default install path is $CARGO_HOME/bin, so the receipt is the
        // only thing distinguishing a dist install from `cargo install`.
        let cargo = PathBuf::from("/home/u/.cargo");
        let exe = PathBuf::from("/home/u/.cargo/bin/amt");
        assert_eq!(detect(&exe, Some(&cargo), true), Install::Installer);
        assert_eq!(detect(&exe, Some(&cargo), false), Install::Cargo);
    }

    #[test]
    fn unknown_location_falls_back_to_installer() {
        let cargo = PathBuf::from("/home/u/.cargo");
        let exe = PathBuf::from("/usr/local/bin/amt");
        assert_eq!(detect(&exe, Some(&cargo), false), Install::Installer);
        // No CARGO_HOME at all is still safe.
        assert_eq!(detect(&exe, None, false), Install::Installer);
    }

    #[test]
    fn commands_match_the_install_kind() {
        let (p, a) = upgrade_command(Install::Homebrew, false);
        assert_eq!(p, "brew");
        assert_eq!(a, vec!["upgrade", "amt"]);

        let (p, a) = upgrade_command(Install::Cargo, false);
        assert_eq!(p, "cargo");
        assert!(a.contains(&"--locked".to_string()));
        assert!(a.iter().any(|s| s.contains("github.com/Davidb3l/Ametrite")));

        let (p, a) = upgrade_command(Install::Installer, false);
        assert_eq!(p, "sh");
        assert!(a.last().unwrap().contains("amt-installer.sh"));

        let (p, a) = upgrade_command(Install::Installer, true);
        assert_eq!(p, "powershell");
        assert!(a.last().unwrap().contains("amt-installer.ps1"));
    }

    #[test]
    fn method_parsing_round_trips_and_rejects_junk() {
        assert_eq!(Install::parse("brew").unwrap(), Install::Homebrew);
        assert_eq!(Install::parse("homebrew").unwrap(), Install::Homebrew);
        assert_eq!(Install::parse("cargo").unwrap(), Install::Cargo);
        assert_eq!(Install::parse("installer").unwrap(), Install::Installer);
        assert!(Install::parse("apt").is_err());
    }
}
