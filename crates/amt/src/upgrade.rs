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
/// dist's *default* install path IS `$CARGO_HOME/bin`, so a location there
/// proves nothing: it is `cargo install` only when cargo's own install record
/// lists amt (`cargo_lists_amt`), and a dist receipt outranks even that.
/// Re-running `cargo install` on a dist install would silently switch the
/// user to a source build, so every doubt falls back to the installer, which
/// can only reinstall the same kind of binary.
pub fn detect(
    exe: &Path,
    cargo_home: Option<&Path>,
    has_receipt: bool,
    cargo_lists_amt: bool,
) -> Install {
    detect_on(exe, cargo_home, has_receipt, cargo_lists_amt, cfg!(windows))
}

/// [`detect`] with the platform explicit, so Windows path rules are testable
/// on any host.
fn detect_on(
    exe: &Path,
    cargo_home: Option<&Path>,
    has_receipt: bool,
    cargo_lists_amt: bool,
    windows: bool,
) -> Install {
    let s = exe.to_string_lossy();
    if s.contains("/Cellar/") || s.contains("/homebrew/") || s.contains("/linuxbrew/") {
        return Install::Homebrew;
    }
    if has_receipt {
        return Install::Installer;
    }
    if let Some(home) = cargo_home {
        if cargo_lists_amt && is_under(exe, home, windows) {
            return Install::Cargo;
        }
    }
    Install::Installer
}

/// Whether cargo's install record (`$CARGO_HOME/.crates.toml` or
/// `.crates2.json`) lists the `amt` package. `cargo install` writes both; a
/// dist installer writes neither. Keys look like `amt 0.4.1 (git+https://…)`.
/// Either file counts: cargo treats `.crates.toml` as the source of truth and
/// re-syncs `.crates2.json` from it, so they only disagree after a hand edit
/// or an old cargo. Pure.
fn cargo_lists_amt(crates2_json: Option<&str>, crates_toml: Option<&str>) -> bool {
    let is_amt = |key: &str| key.starts_with("amt ");
    let in_json = crates2_json
        .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
        .and_then(|v| {
            v.get("installs")?
                .as_object()
                .map(|i| i.keys().any(|k| is_amt(k)))
        })
        .unwrap_or(false);
    in_json
        || crates_toml.is_some_and(|t| {
            t.lines()
                .any(|l| l.trim_start().strip_prefix('"').is_some_and(is_amt))
        })
}

/// Whether `path` lies inside `dir`. Windows paths compare case-insensitively,
/// with either slash, ignoring a `\\?\` verbatim prefix (`\\?\UNC\` is the
/// verbatim form of `\\`). Anything else that differs (8.3 short names, `..`)
/// reads as "not inside", which [`detect`] treats as the installer.
fn is_under(path: &Path, dir: &Path, windows: bool) -> bool {
    if !windows {
        return path.starts_with(dir);
    }
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('/', "\\").to_lowercase();
        if let Some(unc) = s.strip_prefix(r"\\?\unc\") {
            format!(r"\\{unc}")
        } else {
            s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
        }
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

/// Every place a cargo-dist installer may have recorded that it owns this
/// binary. Pure. The PowerShell installer writes `$XDG_CONFIG_HOME\amt` when
/// that is set and `%LOCALAPPDATA%\amt` otherwise (AMT-54); the shell
/// installer writes `${XDG_CONFIG_HOME:-$HOME/.config}/amt`, except under
/// Git Bash/MSYS on Windows, where it uses `$LOCALAPPDATA/amt`. XDG may also
/// have been set or unset since the install, so every candidate counts.
fn receipt_candidates(
    windows: bool,
    xdg_config_home: Option<PathBuf>,
    local_app_data: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Vec<PathBuf> {
    let fallback = if windows {
        local_app_data
    } else {
        home.map(|h| h.join(".config"))
    };
    [xdg_config_home, fallback]
        .into_iter()
        .flatten()
        .map(|base| base.join("amt").join("amt-receipt.json"))
        .collect()
}

fn receipt_candidates_here() -> Vec<PathBuf> {
    receipt_candidates(
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
    let has_receipt = receipt_candidates_here().iter().any(|p| p.is_file());
    let cargo_home = cargo_home();
    let read = |name: &str| {
        cargo_home
            .as_ref()
            .and_then(|h| std::fs::read_to_string(h.join(name)).ok())
    };
    let listed = cargo_lists_amt(
        read(".crates2.json").as_deref(),
        read(".crates.toml").as_deref(),
    );
    // Compare like with like: the exe was canonicalized above, so resolve
    // CARGO_HOME the same way (a symlinked home, or macOS's /var → /private/var).
    let cargo_home = if cfg!(windows) {
        cargo_home
    } else {
        cargo_home.map(|h| h.canonicalize().unwrap_or(h))
    };
    Ok(detect(&exe, cargo_home.as_deref(), has_receipt, listed))
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
                detect(Path::new(p), Some(&cargo), false, true),
                Install::Homebrew,
                "{p}"
            );
        }
    }

    #[test]
    fn cargo_needs_both_the_location_and_cargos_own_record() {
        // dist's default install path is $CARGO_HOME/bin, so the location
        // alone can't tell a dist install from `cargo install`.
        let cargo = PathBuf::from("/home/u/.cargo");
        let exe = PathBuf::from("/home/u/.cargo/bin/amt");
        assert_eq!(detect(&exe, Some(&cargo), false, true), Install::Cargo);
        // A dist install with no receipt (AMT_DISABLE_UPDATE/unmanaged) is
        // not in cargo's record: the installer, not a source build.
        assert_eq!(detect(&exe, Some(&cargo), false, false), Install::Installer);
        // A receipt outranks even cargo's record.
        assert_eq!(detect(&exe, Some(&cargo), true, true), Install::Installer);
        // Listed in cargo's record but running from elsewhere: not this binary.
        let elsewhere = PathBuf::from("/usr/local/bin/amt");
        assert_eq!(
            detect(&elsewhere, Some(&cargo), false, true),
            Install::Installer
        );
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
                detect_on(Path::new(exe), Some(&cargo), false, true, true),
                Install::Cargo,
                "{exe}"
            );
        }
        let with_slash = PathBuf::from(r"C:\Users\Ana\.cargo\");
        let exe = Path::new(r"C:\Users\Ana\.cargo\bin\amt.exe");
        assert_eq!(
            detect_on(exe, Some(&with_slash), false, true, true),
            Install::Cargo
        );
        // A sibling that merely shares the prefix is not inside it.
        let exe = Path::new(r"C:\Users\Ana\.cargo-old\bin\amt.exe");
        assert_eq!(
            detect_on(exe, Some(&cargo), false, true, true),
            Install::Installer
        );
        // The receipt still wins, and cargo's record is still required.
        let exe = Path::new(r"C:\Users\Ana\.cargo\bin\amt.exe");
        assert_eq!(
            detect_on(exe, Some(&cargo), true, true, true),
            Install::Installer
        );
        assert_eq!(
            detect_on(exe, Some(&cargo), false, false, true),
            Install::Installer
        );
    }

    #[test]
    fn windows_unc_paths_match_their_verbatim_form() {
        let share = PathBuf::from(r"\\srv\home\ana\.cargo");
        for exe in [
            r"\\srv\home\ana\.cargo\bin\amt.exe",
            r"\\?\UNC\srv\home\ana\.cargo\bin\amt.exe",
        ] {
            assert!(is_under(Path::new(exe), &share, true), "{exe}");
        }
        let verbatim = PathBuf::from(r"\\?\UNC\srv\home\ana\.cargo");
        let exe = Path::new(r"\\srv\home\ana\.cargo\bin\amt.exe");
        assert!(is_under(exe, &verbatim, true));
        assert!(!is_under(exe, Path::new(r"\\srv\home\bob"), true));
    }

    #[test]
    fn cargo_record_lists_amt_by_package_name() {
        // The real formats, captured from `cargo install --root`.
        let json = r#"{"installs":{"amt 0.4.1 (git+https://github.com/Davidb3l/Ametrite#043db39)":{"bins":["amt"]}}}"#;
        let toml = "[v1]\n\"amt 0.4.1 (registry+https://github.com/rust-lang/crates.io-index)\" = [\"amt\"]\n";
        assert!(cargo_lists_amt(Some(json), None));
        assert!(cargo_lists_amt(None, Some(toml)));
        // Other packages, including ones whose name starts with "amt", don't count.
        let other = r#"{"installs":{"amtool 1.0.0 (registry+x)":{"bins":["amt"]},"ripgrep 14.0.0 (registry+x)":{}}}"#;
        assert!(!cargo_lists_amt(Some(other), None));
        assert!(!cargo_lists_amt(
            None,
            Some("[v1]\n\"amtool 1.0.0 (x)\" = [\"amt\"]\n")
        ));
        // Either file counts (cargo re-syncs .crates2.json from .crates.toml).
        assert!(cargo_lists_amt(Some(r#"{"installs":{}}"#), Some(toml)));
        assert!(cargo_lists_amt(Some(json), Some("[v1]\n")));
        assert!(cargo_lists_amt(Some("not json"), Some(toml)));
        assert!(!cargo_lists_amt(Some(r#"{"installs":{}}"#), Some("[v1]\n")));
        assert!(!cargo_lists_amt(None, None));
    }

    #[test]
    fn receipt_candidates_cover_every_place_an_installer_writes_one() {
        let p = |s: &str| Some(PathBuf::from(s));
        let receipt = |base: &str| Path::new(base).join("amt").join("amt-receipt.json");
        // PowerShell installer: XDG_CONFIG_HOME when set, else LOCALAPPDATA
        // (AMT-54); the shell installer under Git Bash uses LOCALAPPDATA even
        // with XDG set, and XDG may have changed since: check both.
        assert_eq!(
            receipt_candidates(true, p(r"D:\xdg"), p(r"C:\L"), p(r"C:\H")),
            vec![receipt(r"D:\xdg"), receipt(r"C:\L")]
        );
        assert_eq!(
            receipt_candidates(true, None, p(r"C:\L"), p(r"C:\H")),
            vec![receipt(r"C:\L")]
        );
        assert!(receipt_candidates(true, None, None, p(r"C:\H")).is_empty());
        // Shell installer: ${XDG_CONFIG_HOME:-$HOME/.config}.
        assert_eq!(
            receipt_candidates(false, p("/xdg"), p("/L"), p("/home/u")),
            vec![receipt("/xdg"), receipt("/home/u/.config")]
        );
        assert_eq!(
            receipt_candidates(false, None, p("/L"), p("/home/u")),
            vec![receipt("/home/u/.config")]
        );
        assert!(receipt_candidates(false, None, None, None).is_empty());
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
            cargo_home_from(true, None, p(r"C:\h"), None),
            dot_cargo(r"C:\h"),
            "HOME (Git Bash) when there is no profile"
        );
        assert_eq!(cargo_home_from(true, None, None, None), None);
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
        assert_eq!(detect(&exe, Some(&cargo), false, false), Install::Installer);
        // No CARGO_HOME at all is still safe.
        assert_eq!(detect(&exe, None, false, true), Install::Installer);
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
