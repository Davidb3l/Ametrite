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
    detect_on(exe, cargo_home, has_receipt, cfg!(windows))
}

/// [`detect`] with the platform explicit, so Windows path rules are testable
/// on any host.
fn detect_on(exe: &Path, cargo_home: Option<&Path>, has_receipt: bool, windows: bool) -> Install {
    let s = exe.to_string_lossy();
    if s.contains("/Cellar/") || s.contains("/homebrew/") || s.contains("/linuxbrew/") {
        return Install::Homebrew;
    }
    if has_receipt {
        return Install::Installer;
    }
    if let Some(home) = cargo_home {
        if is_under(exe, home, windows) {
            return Install::Cargo;
        }
    }
    Install::Installer
}

/// Whether `path` lies inside `dir`. Windows paths compare case-insensitively,
/// with either slash, and ignoring a `\\?\` verbatim prefix.
fn is_under(path: &Path, dir: &Path, windows: bool) -> bool {
    if !windows {
        return path.starts_with(dir);
    }
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('/', "\\").to_lowercase();
        s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
    };
    let dir = norm(dir);
    let dir = dir.trim_end_matches('\\');
    norm(path)
        .strip_prefix(dir)
        .is_some_and(|rest| rest.starts_with('\\'))
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

/// A set, non-empty environment variable (the installers treat an empty one
/// as unset).
fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Where the cargo-dist installer records that it owns this binary. Pure;
/// mirrors the installers: the PowerShell one uses `$XDG_CONFIG_HOME\amt`
/// when that is set and `%LOCALAPPDATA%\amt` otherwise (AMT-54); the shell
/// one uses `${XDG_CONFIG_HOME:-$HOME/.config}/amt`.
fn receipt_path_from(
    windows: bool,
    xdg_config_home: Option<PathBuf>,
    local_app_data: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    let base = match xdg_config_home {
        Some(xdg) => xdg,
        None if windows => local_app_data?,
        None => home?.join(".config"),
    };
    Some(base.join("amt").join("amt-receipt.json"))
}

fn receipt_path() -> Option<PathBuf> {
    receipt_path_from(
        cfg!(windows),
        env_dir("XDG_CONFIG_HOME"),
        env_dir("LOCALAPPDATA"),
        env_dir("HOME"),
    )
}

/// `$CARGO_HOME`, else `.cargo` in the home directory, which on Windows is
/// the user profile (`HOME` is usually unset there). Pure.
fn cargo_home_from(
    windows: bool,
    cargo_home: Option<PathBuf>,
    home: Option<PathBuf>,
    user_profile: Option<PathBuf>,
) -> Option<PathBuf> {
    cargo_home.or_else(|| {
        let home = if windows { user_profile.or(home) } else { home };
        Some(home?.join(".cargo"))
    })
}

fn cargo_home() -> Option<PathBuf> {
    cargo_home_from(
        cfg!(windows),
        env_dir("CARGO_HOME"),
        env_dir("HOME"),
        env_dir("USERPROFILE"),
    )
}

/// Detect the install kind of the currently-running binary.
pub fn current_install() -> Result<Install> {
    let exe = std::env::current_exe()?;
    // Resolve symlinks (Homebrew links Cellar binaries into its bin dir). Not
    // on Windows: there is no Homebrew, and canonicalize returns a `\\?\`
    // verbatim path.
    let exe = if cfg!(windows) {
        exe
    } else {
        exe.canonicalize().unwrap_or(exe)
    };
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
    fn windows_cargo_installs_match_regardless_of_case_slashes_or_verbatim_prefix() {
        let cargo = PathBuf::from(r"C:\Users\Ana\.cargo");
        for exe in [
            r"C:\Users\Ana\.cargo\bin\amt.exe",
            r"\\?\C:\Users\Ana\.cargo\bin\amt.exe",
            r"c:\users\ana\.CARGO\bin\amt.exe",
            "C:/Users/Ana/.cargo/bin/amt.exe",
        ] {
            assert_eq!(
                detect_on(Path::new(exe), Some(&cargo), false, true),
                Install::Cargo,
                "{exe}"
            );
        }
        let with_slash = PathBuf::from(r"C:\Users\Ana\.cargo\");
        let exe = Path::new(r"C:\Users\Ana\.cargo\bin\amt.exe");
        assert_eq!(
            detect_on(exe, Some(&with_slash), false, true),
            Install::Cargo
        );
        // A sibling that merely shares the prefix is not inside it.
        let exe = Path::new(r"C:\Users\Ana\.cargo-old\bin\amt.exe");
        assert_eq!(
            detect_on(exe, Some(&cargo), false, true),
            Install::Installer
        );
        // The receipt still wins, as on Unix.
        let exe = Path::new(r"C:\Users\Ana\.cargo\bin\amt.exe");
        assert_eq!(detect_on(exe, Some(&cargo), true, true), Install::Installer);
    }

    #[test]
    fn receipt_path_matches_where_each_installer_writes_it() {
        let p = |s: &str| Some(PathBuf::from(s));
        let receipt = |base: &str| Some(Path::new(base).join("amt").join("amt-receipt.json"));
        // PowerShell installer: XDG_CONFIG_HOME when set, else LOCALAPPDATA (AMT-54).
        assert_eq!(
            receipt_path_from(true, p(r"D:\xdg"), p(r"C:\L"), p(r"C:\H")),
            receipt(r"D:\xdg")
        );
        assert_eq!(
            receipt_path_from(true, None, p(r"C:\L"), p(r"C:\H")),
            receipt(r"C:\L")
        );
        assert_eq!(receipt_path_from(true, None, None, p(r"C:\H")), None);
        // Shell installer: ${XDG_CONFIG_HOME:-$HOME/.config}.
        assert_eq!(
            receipt_path_from(false, p("/xdg"), p("/L"), p("/home/u")),
            receipt("/xdg")
        );
        assert_eq!(
            receipt_path_from(false, None, p("/L"), p("/home/u")),
            receipt("/home/u/.config")
        );
        assert_eq!(receipt_path_from(false, None, None, None), None);
    }

    #[test]
    fn cargo_home_falls_back_to_the_user_profile_on_windows() {
        let p = |s: &str| Some(PathBuf::from(s));
        let dot_cargo = |s: &str| Some(Path::new(s).join(".cargo"));
        assert_eq!(
            cargo_home_from(true, p(r"E:\cargo"), None, p(r"C:\Users\a")),
            p(r"E:\cargo")
        );
        assert_eq!(
            cargo_home_from(true, None, None, p(r"C:\Users\a")),
            dot_cargo(r"C:\Users\a")
        );
        assert_eq!(
            cargo_home_from(true, None, p(r"C:\h"), p(r"C:\Users\a")),
            dot_cargo(r"C:\Users\a"),
            "the profile wins: it's where rustup and the installer put .cargo"
        );
        assert_eq!(
            cargo_home_from(false, None, p("/home/u"), p("/ignored")),
            dot_cargo("/home/u")
        );
        assert_eq!(cargo_home_from(false, None, None, p("/ignored")), None);
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
