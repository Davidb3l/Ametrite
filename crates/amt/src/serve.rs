//! `amt serve` — run the web board, and install it as a per-user login service
//! so the human dashboard outlives the terminal or agent session that started
//! it (AMT-23).
//!
//! Agents never need this: the CLI and MCP open the SQLite file directly, so
//! the board being down costs a viewport, not the system. It exists because a
//! server started inside a session dies with that session, which reads to a
//! human as "Ametrite stopped".
//!
//! The web app itself ships INSIDE the binary (AMT-26): with no checkout in
//! sight, `serve` extracts the embedded copy into the per-user data dir and
//! runs that, so `brew install amt && amt serve --install` is a complete
//! board setup. A checkout, when present, still wins — dogfooders see their
//! local edits.
//!
//! Two design choices worth knowing:
//!
//! 1. **No Rust supervisor.** On Unix `serve` `exec()`s bun, replacing its own
//!    process image, so the service manager's process IS the server: signals
//!    land on it directly and no orphaned child can survive holding the port.
//!    Crash restarts come from the platform (launchd `KeepAlive`, systemd
//!    `Restart=on-failure`, a Scheduled Task's restart policy) rather than a
//!    loop we'd have to write, test, and signal-handle without dependencies.
//! 2. **Units invoke bun directly, with values resolved at install time.**
//!    `amt serve --install` finds bun, the web app, the port, and the PATH the
//!    board needs, then writes them into the unit. It deliberately does NOT
//!    make the unit run `amt serve`: on macOS the login service's executable is
//!    the TCC-responsible process, so routing through an unsigned `amt` strips
//!    bun of a Documents/Desktop grant the user already approved and the server
//!    hangs on its first read. Re-run `--install` after moving the checkout or
//!    upgrading bun.

use crate::error::{msg, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Service identity per platform. Kept parallel to the suite's convention
/// (`com.<tool>.<component>`) so the hub and peers can follow the same shape.
pub const MACOS_LABEL: &str = "com.ametrite.web";
pub const LINUX_UNIT: &str = "ametrite-web.service";
pub const WINDOWS_TASK: &str = "Ametrite Web";
pub const DEFAULT_PORT: u16 = 1776;

/// Everything `serve` needs, resolved from flags, env, and the filesystem.
#[derive(Debug, Clone)]
pub struct ServeConfig {
    /// Absolute path to `apps/web/server.ts`.
    pub app: PathBuf,
    /// Absolute path to the `bun` executable.
    pub bun: PathBuf,
    pub port: u16,
}

/// Locate `bun`, the web app, and the port.
///
/// The web app is a Bun project in the Ametrite repo, not something baked into
/// the released binary, so `serve` currently needs a checkout — resolved from
/// `--app`, then `$AMT_WEB_APP`, then by walking up from the current directory.
pub fn resolve(
    app_override: Option<&Path>,
    bun_override: Option<&Path>,
    cwd: &Path,
) -> Result<ServeConfig> {
    let app = resolve_app(
        app_override,
        std::env::var_os("AMT_WEB_APP").map(PathBuf::from),
        cwd,
        &web_data_dir()?,
    )?;
    if !app.is_file() {
        return Err(msg(format!("web app not found at {}", app.display())));
    }
    let app = app.canonicalize().unwrap_or(app);
    let bun = match bun_override {
        Some(p) => p.to_path_buf(),
        None => which("bun").ok_or_else(|| {
            msg("`bun` is not on PATH — the web board is a Bun app (see https://bun.sh)")
        })?,
    };
    if !bun.is_file() {
        return Err(msg(format!("bun not found at {}", bun.display())));
    }
    Ok(ServeConfig {
        app,
        bun,
        port: port_from_env(),
    })
}

/// `$AMT_PORT` or the default. Kept in one place so the unit, the running
/// server, and `doctor`'s advertised `ui` URL can never disagree.
pub fn port_from_env() -> u16 {
    std::env::var("AMT_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// The app-source resolution order, explicit flag strongest: `--app`, then
/// `$AMT_WEB_APP` (both error loudly downstream if bad — explicit config must
/// never fall through silently), then a checkout found by walking up (so
/// dogfooders see local edits), and finally the embedded copy extracted to the
/// data dir — the packaged-install path that makes `brew install amt` a
/// complete board setup. Separated from `resolve` so the ordering is testable
/// without bun on PATH.
fn resolve_app(
    app_override: Option<&Path>,
    env_app: Option<PathBuf>,
    cwd: &Path,
    data_dir: &Path,
) -> Result<PathBuf> {
    if let Some(p) = app_override {
        return Ok(p.to_path_buf());
    }
    if let Some(p) = env_app {
        return Ok(p);
    }
    if let Some(p) = find_web_app(cwd) {
        return Ok(p);
    }
    extract_embedded_web_app(data_dir)
}

/// Walk up from `start` looking for `apps/web/server.ts`.
fn find_web_app(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start.to_path_buf());
    while let Some(d) = dir {
        let candidate = d.join("apps").join("web").join("server.ts");
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    None
}

// ------------------------------------------------------------ embedded app

/// The web app's source files, compiled into the binary (AMT-26) so released
/// `amt` serves the board without a checkout. The app is deliberately tiny
/// (~124 KB, vanilla TS, zero npm dependencies — verified to serve from a bare
/// copy of exactly these files), which is what makes include_str! reasonable.
const EMBEDDED_WEB_APP: &[(&str, &str)] = &[
    ("server.ts", include_str!("../../../apps/web/server.ts")),
    ("index.html", include_str!("../../../apps/web/index.html")),
    (
        "package.json",
        include_str!("../../../apps/web/package.json"),
    ),
    ("src/app.ts", include_str!("../../../apps/web/src/app.ts")),
    ("src/time.ts", include_str!("../../../apps/web/src/time.ts")),
    (
        "src/style.css",
        include_str!("../../../apps/web/src/style.css"),
    ),
];

/// Per-user data dir for the extracted app. ONE stable, unversioned path, on
/// purpose: service units bake this path into ExecStart, so a versioned dir
/// would leave every installed service pointing at the old version after an
/// upgrade — permanently stale, or crash-looping if the dir were cleaned.
/// Freshness comes from extraction instead: every serve-path command converges
/// the dir to the running binary's embedded content, and a running service
/// picks up refreshed frontend files on its next request (Bun re-bundles from
/// disk per request in dev serving). Outside ~/Documents by construction, so
/// on macOS the login service reads it without any TCC grant — packaged users
/// skip the trap that checkout users can hit.
pub fn web_data_dir() -> Result<PathBuf> {
    let home = home_dir()?;
    let base = if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support")
    } else if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Local"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("share"))
    };
    Ok(base.join("ametrite").join("web").join("current"))
}

/// Write the embedded app under `dir`, overwriting so the extracted copy
/// always matches this binary. Returns the path to `server.ts`.
pub fn extract_embedded_web_app(dir: &Path) -> Result<PathBuf> {
    for (rel, content) in EMBEDDED_WEB_APP {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Skip the write when identical: a running service's Bun re-bundles
        // from disk per request, so gratuitous rewrites would churn it (and a
        // source-built and released binary sharing this dir would flip-flop).
        let unchanged = std::fs::read_to_string(&path)
            .map(|c| c == *content)
            .unwrap_or(false);
        if !unchanged {
            // Write-then-rename so a service racing this extraction can never
            // read a torn file (fs::write is not atomic; rename in-dir is).
            let tmp = path.with_extension("tmp-extract");
            std::fs::write(&tmp, content)?;
            std::fs::rename(&tmp, &path)?;
        }
    }
    Ok(dir.join("server.ts"))
}

/// First match for `name` on PATH.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// PATH for the server process, with the directories holding `amt` and `bun`
/// prepended. The board shells out to `amt` for every mutation, and a service
/// manager starts jobs with a minimal PATH — without this the board renders
/// fine but silently can't write. (Learned the hard way installing this by
/// hand.) Pure so the ordering is testable.
pub fn child_path(exe_dir: Option<&Path>, bun_dir: Option<&Path>, current: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for d in [exe_dir, bun_dir].into_iter().flatten() {
        let s = d.to_string_lossy().into_owned();
        if !s.is_empty() && !parts.contains(&s) {
            parts.push(s);
        }
    }
    for p in current.split(':').filter(|p| !p.is_empty()) {
        if !parts.contains(&p.to_string()) {
            parts.push(p.to_string());
        }
    }
    parts.join(":")
}

/// The directory three levels above `server.ts` — the repo root for a
/// checkout. The server itself resolves its files via `import.meta.dir`, so
/// for the extracted layout (where this lands on `~/…/ametrite`) the cwd is
/// inert; it exists so a checkout's `node_modules`, if one ever appears,
/// resolves correctly.
pub fn project_root(app: &Path) -> PathBuf {
    app.parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// macOS only: warn when the web app lives somewhere a login service can't
/// read without a TCC grant the user must approve interactively. Hit for real
/// during development — the service starts, then blocks on its first read with
/// no error anywhere.
fn warn_if_tcc_protected(app: &Path) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let home = match home_dir() {
        Ok(h) => h,
        Err(_) => return,
    };
    for guarded in ["Documents", "Desktop", "Downloads"] {
        if app.starts_with(home.join(guarded)) {
            eprintln!(
                "warning: the web app is under ~/{guarded}, which macOS protects. \
                 If the board never comes up, approve the access prompt, or move the \
                 checkout outside ~/{guarded} and re-run `amt serve --install`."
            );
            return;
        }
    }
}

/// Build the command that runs the board.
fn server_command(cfg: &ServeConfig) -> Command {
    let mut cmd = Command::new(&cfg.bun);
    cmd.arg("run").arg(&cfg.app);
    cmd.current_dir(project_root(&cfg.app));
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let bun_dir = cfg.bun.parent().map(Path::to_path_buf);
    let current = std::env::var("PATH").unwrap_or_default();
    cmd.env(
        "PATH",
        child_path(exe_dir.as_deref(), bun_dir.as_deref(), &current),
    );
    cmd.env("AMT_PORT", cfg.port.to_string());
    cmd
}

/// Run the board in the foreground. On Unix this REPLACES the current process
/// (see the module docs), so it never returns on success.
pub fn run(cfg: &ServeConfig) -> Result<()> {
    let mut cmd = server_command(cfg);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // exec() only returns on failure.
        let err = cmd.exec();
        Err(msg(format!("failed to exec {}: {err}", cfg.bun.display())))
    }
    #[cfg(not(unix))]
    {
        let status = cmd
            .status()
            .map_err(|e| msg(format!("failed to run {}: {e}", cfg.bun.display())))?;
        if !status.success() {
            return Err(msg(format!("web server exited with {status}")));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- unit files

/// Where this platform's unit file lives.
pub fn unit_path() -> Result<PathBuf> {
    let home = home_dir()?;
    Ok(if cfg!(target_os = "macos") {
        home.join("Library")
            .join("LaunchAgents")
            .join(format!("{MACOS_LABEL}.plist"))
    } else {
        home.join(".config")
            .join("systemd")
            .join("user")
            .join(LINUX_UNIT)
    })
}

/// Where the service's stdout/stderr land (systemd uses the journal instead).
pub fn log_path() -> Result<PathBuf> {
    let home = home_dir()?;
    Ok(if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Logs")
            .join("ametrite")
            .join("web.log")
    } else {
        home.join(".local")
            .join("state")
            .join("ametrite")
            .join("web.log")
    })
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| msg("cannot locate home directory (HOME/USERPROFILE unset)"))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// macOS LaunchAgent. Pure so the exact file is testable.
///
/// Runs bun directly (see the module docs on TCC) with everything resolved at
/// install time: the app path, because a service has no useful cwd; and PATH,
/// because a service starts with a minimal one and the board shells out to
/// `amt` for every mutation — without it the board renders but can't write.
pub fn render_plist(
    bun: &Path,
    app: &Path,
    cwd: &Path,
    path_env: &str,
    log: &Path,
    port: u16,
) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <!-- `amt serve` resolves bun, the web app, and PATH at run time, so this
       unit never needs editing when the checkout moves or bun is upgraded. -->
  <key>ProgramArguments</key>
  <array>
    <string>{bun}</string>
    <string>run</string>
    <string>{app}</string>
  </array>
  <key>WorkingDirectory</key><string>{cwd}</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>AMT_PORT</key><string>{port}</string>
    <key>PATH</key><string>{path_env}</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        label = MACOS_LABEL,
        bun = xml_escape(&bun.to_string_lossy()),
        app = xml_escape(&app.to_string_lossy()),
        cwd = xml_escape(&cwd.to_string_lossy()),
        path_env = xml_escape(path_env),
        log = xml_escape(&log.to_string_lossy()),
        port = port,
    )
}

/// systemd user unit. Pure so the exact file is testable. The resolved
/// web-app path is baked in for the same reason as the plist.
pub fn render_systemd_unit(
    bun: &Path,
    app: &Path,
    cwd: &Path,
    path_env: &str,
    port: u16,
) -> String {
    format!(
        "[Unit]\n\
         Description=Ametrite web board\n\
         After=default.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         Environment=AMT_PORT={port}\n\
         Environment=PATH={path_env}\n\
         WorkingDirectory={cwd}\n\
         ExecStart={bun} run {app}\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        bun = bun.display(),
        app = app.display(),
        cwd = cwd.display(),
        path_env = path_env,
        port = port,
    )
}

/// The `schtasks` argv that registers the Windows logon task. Pure/testable.
/// A user-level Scheduled Task, deliberately not a Windows Service: a service
/// needs administrator rights, which is far too much for a local dashboard.
pub fn schtasks_create_args(bun: &Path, app: &Path, cwd: &Path, log: &Path) -> Vec<String> {
    let cmd = format!(
        "cmd /c \"cd /d \"{}\" && \"{}\" run \"{}\" >> \"{}\" 2>&1\"",
        cwd.display(),
        bun.display(),
        app.display(),
        log.display()
    );
    vec![
        "/Create".into(),
        "/F".into(), // replace an existing task — makes install idempotent
        "/SC".into(),
        "ONLOGON".into(),
        "/TN".into(),
        WINDOWS_TASK.into(),
        "/TR".into(),
        cmd,
    ]
}

// ------------------------------------------------------------- install/status

fn run_tool(program: &str, args: &[String]) -> Result<()> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| msg(format!("failed to run {program}: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(msg(format!(
            "{program} {} failed: {}",
            args.join(" "),
            err.trim()
        )));
    }
    Ok(())
}

/// Install (or refresh) the login service and start it. Idempotent.
pub fn install(cfg: &ServeConfig) -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let log = log_path()?;
    // Resolve now what the service can't resolve later: the project root (a
    // service has no useful cwd) and a PATH that can find `amt` and `bun`.
    let cwd = project_root(&cfg.app);
    let path_env = child_path(
        exe.parent(),
        cfg.bun.parent(),
        &std::env::var("PATH").unwrap_or_default(),
    );
    if let Some(d) = log.parent() {
        std::fs::create_dir_all(d)?;
    }
    if cfg!(target_os = "windows") {
        run_tool(
            "schtasks",
            &schtasks_create_args(&cfg.bun, &cfg.app, &cwd, &log),
        )?;
        let _ = run_tool(
            "schtasks",
            &["/Run".into(), "/TN".into(), WINDOWS_TASK.into()],
        );
        return Ok(PathBuf::from(WINDOWS_TASK));
    }
    let unit = unit_path()?;
    if let Some(d) = unit.parent() {
        std::fs::create_dir_all(d)?;
    }
    if cfg!(target_os = "macos") {
        std::fs::write(
            &unit,
            render_plist(&cfg.bun, &cfg.app, &cwd, &path_env, &log, cfg.port),
        )?;
        warn_if_tcc_protected(&cfg.app);
        let target = format!("gui/{}", uid());
        // Unload first so a re-install picks up the new file rather than
        // erroring with "service already loaded".
        let _ = run_tool(
            "launchctl",
            &["bootout".into(), format!("{target}/{MACOS_LABEL}")],
        );
        // bootout is asynchronous: bootstrapping while the old job is still
        // tearing down fails with "Input/output error". Retry briefly.
        let args = [
            "bootstrap".to_string(),
            target,
            unit.to_string_lossy().into_owned(),
        ];
        let mut last = Ok(());
        for attempt in 0..6 {
            last = run_tool("launchctl", &args);
            if last.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250 * (attempt + 1)));
        }
        last?;
    } else {
        std::fs::write(
            &unit,
            render_systemd_unit(&cfg.bun, &cfg.app, &cwd, &path_env, cfg.port),
        )?;
        run_tool("systemctl", &["--user".into(), "daemon-reload".into()])?;
        run_tool(
            "systemctl",
            &[
                "--user".into(),
                "enable".into(),
                "--now".into(),
                LINUX_UNIT.into(),
            ],
        )?;
    }
    Ok(unit)
}

/// Remove the service, leaving no trace. Succeeds even if nothing was installed.
pub fn uninstall() -> Result<()> {
    if cfg!(target_os = "windows") {
        let _ = run_tool(
            "schtasks",
            &[
                "/Delete".into(),
                "/F".into(),
                "/TN".into(),
                WINDOWS_TASK.into(),
            ],
        );
        return Ok(());
    }
    if cfg!(target_os = "macos") {
        let _ = run_tool(
            "launchctl",
            &["bootout".into(), format!("gui/{}/{MACOS_LABEL}", uid())],
        );
    } else {
        let _ = run_tool(
            "systemctl",
            &[
                "--user".into(),
                "disable".into(),
                "--now".into(),
                LINUX_UNIT.into(),
            ],
        );
        let _ = run_tool("systemctl", &["--user".into(), "daemon-reload".into()]);
    }
    if let Ok(unit) = unit_path() {
        if unit.exists() {
            std::fs::remove_file(&unit)?;
        }
    }
    Ok(())
}

fn uid() -> String {
    // `id -u` avoids a libc dependency for the one number launchctl needs.
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// What `serve --status` reports.
#[derive(Debug)]
pub struct ServeStatus {
    /// A unit/task file is installed.
    pub installed: bool,
    /// The board is actually answering on its port — the fact a human cares
    /// about, and independent of what the service manager believes.
    pub responding: bool,
    pub port: u16,
    pub unit: Option<PathBuf>,
    pub log: Option<PathBuf>,
}

pub fn status() -> Result<ServeStatus> {
    let port = port_from_env();
    let unit = unit_path().ok();
    let installed = if cfg!(target_os = "windows") {
        Command::new("schtasks")
            .args(["/Query", "/TN", WINDOWS_TASK])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    } else {
        unit.as_ref().is_some_and(|u| u.exists())
    };
    Ok(ServeStatus {
        installed,
        responding: port_responds(port),
        port,
        unit: unit.filter(|_| installed),
        log: log_path().ok(),
    })
}

/// True if something accepts a TCP connection on the board's port.
fn port_responds(port: u16) -> bool {
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_path_puts_amt_and_bun_first() {
        // The board shells out to `amt`; a service manager's bare PATH would
        // otherwise leave it readable but unable to write.
        let p = child_path(
            Some(Path::new("/opt/amt/bin")),
            Some(Path::new("/home/u/.bun/bin")),
            "/usr/bin:/bin",
        );
        assert_eq!(p, "/opt/amt/bin:/home/u/.bun/bin:/usr/bin:/bin");
    }

    #[test]
    fn child_path_does_not_duplicate_entries() {
        let p = child_path(
            Some(Path::new("/usr/bin")),
            Some(Path::new("/usr/bin")),
            "/usr/bin:/bin",
        );
        assert_eq!(p, "/usr/bin:/bin");
    }

    #[test]
    fn child_path_survives_an_empty_environment() {
        let p = child_path(Some(Path::new("/opt/amt/bin")), None, "");
        assert_eq!(p, "/opt/amt/bin");
        assert_eq!(child_path(None, None, "/bin"), "/bin");
    }

    #[test]
    fn plist_runs_amt_serve_and_keeps_it_alive() {
        let x = render_plist(
            Path::new("/home/u/.bun/bin/bun"),
            Path::new("/repo/apps/web/server.ts"),
            Path::new("/repo"),
            "/opt/amt/bin:/usr/bin",
            Path::new("/tmp/web.log"),
            1776,
        );
        // Runs bun directly — routing through `amt` costs bun its TCC identity
        // on macOS (see the module docs).
        assert!(x.contains("<string>/home/u/.bun/bin/bun</string>"));
        assert!(x.contains("<string>run</string>"));
        assert!(x.contains("/repo/apps/web/server.ts"));
        // Everything a service can't work out for itself is recorded.
        assert!(x.contains("<key>WorkingDirectory</key><string>/repo</string>"));
        assert!(x.contains("<key>PATH</key><string>/opt/amt/bin:/usr/bin</string>"));
        // Restart-on-crash and start-at-login come from launchd, not from a
        // supervisor loop of ours.
        assert!(x.contains("<key>KeepAlive</key><true/>"));
        assert!(x.contains("<key>RunAtLoad</key><true/>"));
        assert!(x.contains("<key>AMT_PORT</key><string>1776</string>"));
        assert!(x.contains("/tmp/web.log"));
        assert!(x.contains(MACOS_LABEL));
    }

    #[test]
    fn plist_escapes_xml_in_paths() {
        // A checkout under a directory with an ampersand must not produce a
        // malformed plist that launchd silently refuses.
        let x = render_plist(
            Path::new("/Users/a/R&D/bun"),
            Path::new("/Users/a/R&D/apps/web/server.ts"),
            Path::new("/Users/a/R&D"),
            "/usr/bin",
            Path::new("/tmp/l.log"),
            1776,
        );
        assert!(x.contains("/Users/a/R&amp;D/bun"));
        assert!(x.contains("/Users/a/R&amp;D/apps/web/server.ts"));
        assert!(
            !x.contains("R&D"),
            "a raw & would make launchd reject the plist"
        );
    }

    #[test]
    fn systemd_unit_restarts_and_starts_at_login() {
        let u = render_systemd_unit(
            Path::new("/usr/local/bin/bun"),
            Path::new("/repo/apps/web/server.ts"),
            Path::new("/repo"),
            "/opt/amt/bin:/usr/bin",
            1776,
        );
        assert!(u.contains("ExecStart=/usr/local/bin/bun run /repo/apps/web/server.ts"));
        assert!(u.contains("WorkingDirectory=/repo"));
        assert!(u.contains("Environment=PATH=/opt/amt/bin:/usr/bin"));
        assert!(u.contains("Restart=on-failure"));
        assert!(u.contains("WantedBy=default.target"));
        assert!(u.contains("Environment=AMT_PORT=1776"));
    }

    #[test]
    fn port_override_flows_into_every_unit() {
        assert!(render_plist(
            Path::new("/bun"),
            Path::new("/app.ts"),
            Path::new("/r"),
            "/p",
            Path::new("/b"),
            4242
        )
        .contains("<string>4242</string>"));
        assert!(render_systemd_unit(
            Path::new("/bun"),
            Path::new("/app.ts"),
            Path::new("/r"),
            "/p",
            4242
        )
        .contains("AMT_PORT=4242"));
    }

    #[test]
    fn schtasks_args_are_idempotent_and_logged() {
        let a = schtasks_create_args(
            Path::new("C:\\bun.exe"),
            Path::new("C:\\repo\\apps\\web\\server.ts"),
            Path::new("C:\\repo"),
            Path::new("C:\\log.txt"),
        );
        assert!(
            a.contains(&"/F".to_string()),
            "/F makes re-install idempotent"
        );
        assert!(a.contains(&"ONLOGON".to_string()));
        assert!(a.contains(&WINDOWS_TASK.to_string()));
        assert!(a.last().unwrap().contains("server.ts"), "app path baked in");
        assert!(a.last().unwrap().contains("bun.exe"), "bun path baked in");
        assert!(
            a.last().unwrap().contains("cd /d"),
            "runs from the project root"
        );
        assert!(a.last().unwrap().contains("log.txt"));
    }

    #[test]
    fn embedded_app_mirrors_the_real_apps_web_tree() {
        // NOT a hardcoded list: walk the actual apps/web directory so adding a
        // new source file without embedding it fails THIS test instead of
        // shipping released binaries whose board 500s on a missing import.
        let web_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/web");
        let mut expected: Vec<(String, String)> = Vec::new();
        let mut walk = vec![web_root.clone()];
        while let Some(dir) = walk.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n == "node_modules") {
                        continue;
                    }
                    walk.push(path);
                    continue;
                }
                let rel = path
                    .strip_prefix(&web_root)
                    .unwrap()
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                // Tests and lockfiles aren't needed to serve (the app has zero
                // npm deps; verified against a live bare-copy serve).
                if rel.ends_with(".test.ts") || rel == "bun.lock" {
                    continue;
                }
                expected.push((rel, std::fs::read_to_string(&path).unwrap()));
            }
        }
        assert!(!expected.is_empty(), "walk found nothing — wrong root?");
        for (rel, content) in &expected {
            let embedded = EMBEDDED_WEB_APP
                .iter()
                .find(|(n, _)| n == rel)
                .unwrap_or_else(|| panic!("{rel} exists in apps/web but is NOT embedded"));
            assert_eq!(embedded.1, content, "{rel} embedded stale vs the tree");
        }
        for (name, _) in EMBEDDED_WEB_APP {
            assert!(
                expected.iter().any(|(rel, _)| rel == name),
                "{name} is embedded but no longer exists in apps/web"
            );
        }

        // And extraction reproduces the set verbatim.
        let dir = tempfile::TempDir::new().unwrap();
        let server = extract_embedded_web_app(dir.path()).unwrap();
        assert_eq!(server, dir.path().join("server.ts"));
        for (name, content) in EMBEDDED_WEB_APP {
            let on_disk = std::fs::read_to_string(dir.path().join(name)).unwrap();
            assert_eq!(&on_disk, content, "{name} extracted differently");
        }
    }

    #[test]
    fn resolution_order_is_flag_env_checkout_embedded() {
        let data = tempfile::TempDir::new().unwrap();
        let no_checkout = tempfile::TempDir::new().unwrap();

        // 1. Explicit flag wins over everything.
        let flag = Path::new("/explicit/server.ts");
        assert_eq!(
            resolve_app(
                Some(flag),
                Some("/env/server.ts".into()),
                no_checkout.path(),
                data.path()
            )
            .unwrap(),
            flag
        );
        // 2. Env beats discovery — and is returned verbatim even when bad, so
        //    a typo errors loudly downstream instead of silently falling back.
        assert_eq!(
            resolve_app(
                None,
                Some("/env/server.ts".into()),
                no_checkout.path(),
                data.path()
            )
            .unwrap(),
            Path::new("/env/server.ts")
        );
        // 3. A checkout above cwd wins over the embedded copy.
        let repo = tempfile::TempDir::new().unwrap();
        let app_dir = repo.path().join("apps").join("web");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join("server.ts"), "// local").unwrap();
        let deep = repo.path().join("crates").join("x");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(
            resolve_app(None, None, &deep, data.path()).unwrap(),
            app_dir.join("server.ts")
        );
        // 4. Nothing anywhere: the embedded copy is extracted and used — the
        //    packaged-install path.
        let resolved = resolve_app(None, None, no_checkout.path(), data.path()).unwrap();
        assert_eq!(resolved, data.path().join("server.ts"));
        assert!(resolved.is_file(), "fallback must actually extract");
    }

    #[test]
    fn re_extract_leaves_identical_files_untouched() {
        // The service manager restarts `serve` after a crash; rewriting
        // unchanged sources would spin bun's file watcher in a reload loop.
        let dir = tempfile::TempDir::new().unwrap();
        extract_embedded_web_app(dir.path()).unwrap();
        let before = std::fs::metadata(dir.path().join("server.ts"))
            .unwrap()
            .modified()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        extract_embedded_web_app(dir.path()).unwrap();
        let after = std::fs::metadata(dir.path().join("server.ts"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after, "identical content must not be rewritten");

        // A tampered file is healed back to the embedded content.
        std::fs::write(dir.path().join("server.ts"), "corrupted").unwrap();
        extract_embedded_web_app(dir.path()).unwrap();
        let healed = std::fs::read_to_string(dir.path().join("server.ts")).unwrap();
        assert!(healed.len() > 100, "extraction heals a tampered copy");
    }

    #[test]
    fn web_data_dir_is_stable_and_outside_protected_folders() {
        let d = web_data_dir().unwrap();
        // Stable on purpose: service units bake this path into ExecStart, so a
        // versioned dir would strand every installed service on upgrade.
        assert!(
            d.ends_with(PathBuf::from("ametrite").join("web").join("current")),
            "unit paths must survive upgrades: {}",
            d.display()
        );
        // The whole point of the data dir on macOS: no TCC-protected segment,
        // so the login service can read the app without a grant.
        for guarded in ["Documents", "Desktop", "Downloads"] {
            assert!(
                !d.components().any(|c| c.as_os_str() == guarded),
                "{} sits in a protected folder",
                d.display()
            );
        }
    }

    #[test]
    fn find_web_app_walks_up_from_a_subdirectory() {
        let root = tempfile::TempDir::new().unwrap();
        let app = root.path().join("apps").join("web");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("server.ts"), "//").unwrap();
        let deep = root.path().join("crates").join("amt").join("src");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(
            find_web_app(&deep),
            Some(app.join("server.ts")),
            "serve works from anywhere inside a checkout"
        );
        let outside = tempfile::TempDir::new().unwrap();
        assert_eq!(find_web_app(outside.path()), None);
    }
}
