use amt::error::Result;
use amt::model::*;
use amt::{db, export, git, mcp, registry, store};
use clap::{Parser, Subcommand};
use rusqlite::Connection;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "amt",
    version,
    about = "Ametrite: local-first issues + knowledge base for AI agent workflows"
)]
struct Cli {
    /// Path to the workspace directory (defaults to walking up from cwd)
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,
    /// Emit JSON instead of human-readable output
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new workspace (.ametrite/ametrite.db) in the current directory
    Init {
        /// Workspace name
        #[arg(long)]
        name: Option<String>,
        /// Issue key prefix (AMT → AMT-1, AMT-2, …)
        #[arg(long, default_value = "AMT")]
        prefix: String,
    },
    /// Manage issues
    Issue {
        #[command(subcommand)]
        cmd: IssueCmd,
    },
    /// Manage issue dependencies (blocks / blocked-by)
    Dep {
        #[command(subcommand)]
        cmd: DepCmd,
    },
    /// Atomically claim the next available issue (agent loop primitive)
    Claim {
        /// Claim a specific issue instead of the best available
        #[arg(long)]
        issue: Option<String>,
        /// Agent name (default: $AMT_AGENT or $USER)
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        label: Option<String>,
        /// Report the best claimable issue without taking a lease or writing activity
        #[arg(long)]
        peek: bool,
        /// Restrict claimable stages (repeatable or comma-list): backlog, todo
        #[arg(long = "from", value_delimiter = ',')]
        from: Vec<String>,
        /// Lease TTL in seconds
        #[arg(long, default_value_t = 900)]
        ttl: i64,
        /// Seconds before an issue you released can be re-served to you (0 disables)
        #[arg(long, default_value_t = 3600)]
        cooldown: i64,
        /// Claim (or peek) across every registered workspace, globally priority-first
        #[arg(long)]
        all_workspaces: bool,
    },
    /// Release a claimed issue with a final status
    Release {
        id: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long, default_value = "in_review")]
        status: String,
        #[arg(long, short = 'm')]
        comment: Option<String>,
    },
    /// Record a decision that resolves an issue (ADR-for-agents)
    Decide {
        /// Issue key this decision resolves (required — decisions attach to work)
        #[arg(long)]
        issue: String,
        #[arg(long)]
        title: String,
        /// Context / decision / consequences, in markdown
        #[arg(long, short = 'b', default_value = "")]
        body: String,
        /// 'accepted' (default) or 'proposed'
        #[arg(long, default_value = "accepted")]
        status: String,
        /// Mark an earlier decision as superseded by this one
        #[arg(long)]
        supersedes: Option<String>,
        /// Author (default: $AMT_AGENT or $USER)
        #[arg(long)]
        author: Option<String>,
    },
    /// Browse recorded decisions
    Decision {
        #[command(subcommand)]
        cmd: DecisionCmd,
    },
    /// Manage notes
    Note {
        #[command(subcommand)]
        cmd: NoteCmd,
    },
    /// Manage projects
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Full-text search across issues, notes, and projects
    Search {
        query: Vec<String>,
        #[arg(long = "type")]
        doc_type: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        project: Option<String>,
        /// Search across every registered workspace (hits tagged with workspace)
        #[arg(long)]
        all_workspaces: bool,
        #[arg(long, default_value_t = 25)]
        limit: i64,
    },
    /// One-bundle context read for an issue: body + activity + decisions +
    /// backlinked doc bodies + top-k related search hits (claim → context =
    /// 2 calls to productive work)
    Context {
        /// Issue key, e.g. AMT-7
        key: String,
        /// Hard cap on total serialized chars; drops lowest-relevance items
        /// first (FTS hits, then backlink bodies, then activity)
        #[arg(long)]
        budget: Option<i64>,
    },
    /// Show documents that link to the given document
    Backlinks { id: String },
    /// Check workspace health (unresolved links, stale claims, missing refs)
    Doctor,
    /// Show the agent roster: live leases, expiry, last activity, per-agent counts
    Agents,
    /// Throughput, cycle time, and a claim-integrity audit over an optional window
    Stats {
        /// Only count work completed at/after this ISO-8601 instant
        #[arg(long)]
        since: Option<String>,
    },
    /// Stream activity events as NDJSON (one JSON object per line)
    Events {
        /// Resume after this cursor (the `cursor` field of the last event seen)
        #[arg(long)]
        since: Option<i64>,
        /// Keep the stream open and emit new events as they arrive
        #[arg(long)]
        follow: bool,
        /// Rows fetched per query (the stream is drained fully; this only
        /// bounds memory per batch)
        #[arg(long, default_value_t = 500)]
        limit: i64,
    },
    /// Export the workspace as Obsidian-compatible markdown files
    Export { dir: PathBuf },
    /// Import markdown files (previously exported or Obsidian-authored)
    Import { dir: PathBuf },
    /// Run as an MCP stdio server (for Claude Code and other agents)
    Mcp,
    /// Manage the global workspace registry (~/.ametrite/registry.json)
    Ws {
        #[command(subcommand)]
        cmd: WsCmd,
    },
    /// Manage the git commit-msg hook that appends `Refs: <KEY>` from the branch
    Hook {
        #[command(subcommand)]
        cmd: HookCmd,
    },
    /// Create and check out a git branch for an issue (`<key>-<slug>`)
    Branch {
        /// Issue key, e.g. AMT-7
        key: String,
    },
    /// Bulk-insert N synthetic issues (perf benchmarking / demos)
    Seed {
        /// Number of issues to create
        #[arg(long, default_value_t = 1000)]
        count: usize,
        /// Author for the seeded activity (default: $AMT_AGENT or $USER)
        #[arg(long)]
        agent: Option<String>,
    },
    /// Compact the workspace database (FTS optimize, VACUUM, WAL checkpoint)
    Gc,
}

#[derive(Subcommand)]
enum HookCmd {
    /// Install the commit-msg hook (idempotent; appends to a pre-existing hook)
    Install,
    /// Remove the commit-msg hook (only our marked block; keeps foreign hooks)
    Uninstall,
}

#[derive(Subcommand)]
enum WsCmd {
    /// Register a workspace (defaults: path = current workspace root, alias = its name)
    Add {
        path: Option<PathBuf>,
        #[arg(long)]
        alias: Option<String>,
    },
    /// List registered workspaces
    List,
    /// Remove a workspace from the registry (does not delete anything on disk)
    Remove { alias: String },
}

#[derive(Subcommand)]
enum IssueCmd {
    /// Create an issue
    Create {
        #[arg(long)]
        title: String,
        #[arg(long, short = 'b', default_value = "")]
        body: String,
        #[arg(long, default_value = "none")]
        priority: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long = "label")]
        labels: Vec<String>,
        #[arg(long)]
        assignee: Option<String>,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        due: Option<String>,
        /// Issue key(s) that block this new issue (repeatable)
        #[arg(long = "blocked-by")]
        blocked_by: Vec<String>,
        /// Issue key(s) this new issue blocks (repeatable)
        #[arg(long = "blocks")]
        blocks: Vec<String>,
    },
    /// List issues
    List {
        #[arg(long)]
        status: Vec<String>,
        #[arg(long)]
        assignee: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        label: Option<String>,
        /// Include done/canceled issues
        #[arg(long)]
        all: bool,
        /// List across every registered workspace (rows tagged with workspace)
        #[arg(long)]
        all_workspaces: bool,
        #[arg(long, default_value_t = 200)]
        limit: i64,
    },
    /// Show one issue in full (body, activity, backlinks)
    Show { id: String },
    /// Update issue fields
    Update {
        id: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, short = 'b')]
        body: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        priority: Option<String>,
        /// Project slug; pass empty string to clear
        #[arg(long)]
        project: Option<String>,
        /// Assignee; pass empty string to clear
        #[arg(long)]
        assignee: Option<String>,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        due: Option<String>,
        #[arg(long = "add-label")]
        add_labels: Vec<String>,
        #[arg(long = "remove-label")]
        remove_labels: Vec<String>,
        /// Add a blocker: issue key(s) that must close before this one (repeatable)
        #[arg(long = "blocked-by")]
        blocked_by: Vec<String>,
        /// Add a dependent: issue key(s) this one blocks (repeatable)
        #[arg(long = "blocks")]
        blocks: Vec<String>,
    },
    /// Add a comment to an issue
    Comment {
        id: String,
        #[arg(long, short = 'm')]
        body: String,
        #[arg(long)]
        author: Option<String>,
    },
}

#[derive(Subcommand)]
enum DepCmd {
    /// Declare that BLOCKER must close before BLOCKED can be claimed
    Add {
        /// The blocking issue (must close first)
        blocker: String,
        /// The blocked issue (waits on the blocker)
        blocked: String,
    },
    /// Remove a blocker → blocked dependency edge
    Rm { blocker: String, blocked: String },
    /// List an issue's blockers (open) and the issues it blocks
    List { id: String },
}

#[derive(Subcommand)]
enum DecisionCmd {
    /// List decisions (superseded hidden unless --all)
    List {
        /// Only decisions resolving this issue
        #[arg(long)]
        issue: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Show one decision in full
    Show { id: String },
}

#[derive(Subcommand)]
enum NoteCmd {
    Create {
        #[arg(long)]
        title: String,
        #[arg(long, short = 'b', default_value = "")]
        body: String,
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Warn if a near-duplicate note already exists (still creates it).
        #[arg(long)]
        dedupe: bool,
        /// With --dedupe: refuse and exit non-zero on a near-duplicate.
        #[arg(long)]
        strict: bool,
    },
    Show {
        id: String,
    },
    Append {
        id: String,
        #[arg(long, short = 'b')]
        body: String,
    },
    List,
}

#[derive(Subcommand)]
enum ProjectCmd {
    Create {
        #[arg(long)]
        title: String,
        #[arg(long, short = 'b', default_value = "")]
        body: String,
    },
    List,
}

fn identity(explicit: Option<String>) -> String {
    explicit
        .or_else(|| std::env::var("AMT_AGENT").ok())
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "user".into())
}

/// The workspace root directory (the parent of `.ametrite`), used to locate the
/// git repo for R5 git integration. Mirrors `open_workspace`'s resolution:
/// explicit `--workspace` wins, otherwise walk up from cwd to the `.ametrite`
/// marker.
fn workspace_root(cli_workspace: &Option<PathBuf>) -> Result<PathBuf> {
    match cli_workspace {
        Some(dir) => Ok(dir.clone()),
        None => {
            let cwd = std::env::current_dir()?;
            let db_path = db::find_workspace(&cwd).ok_or_else(|| {
                amt::error::msg("no .ametrite workspace found (run `amt init` first)")
            })?;
            // db_path = <root>/.ametrite/ametrite.db → up two → <root>
            Ok(db_path
                .parent()
                .and_then(|p| p.parent())
                .map(|p| p.to_path_buf())
                .unwrap_or(cwd))
        }
    }
}

/// Commits referencing `key` reachable from HEAD, or an empty list when not in
/// a git repo (or git errors). Never fails the caller — `issue show` degrades
/// silently outside a repo.
fn git_commits_for(cli_workspace: &Option<PathBuf>, key: &str) -> Vec<git::Commit> {
    let root = match workspace_root(cli_workspace) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    match git::repo_root(&root) {
        Ok(Some(repo)) => git::commits_for_key(&repo, key).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Commits on the current branch (since the default-branch merge-base) that
/// reference `key`, for `release`'s auto-comment. Empty outside a git repo.
fn git_commits_since_base(cli_workspace: &Option<PathBuf>, key: &str) -> Vec<git::Commit> {
    let root = match workspace_root(cli_workspace) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    match git::repo_root(&root) {
        Ok(Some(repo)) => git::commits_since_base(&repo, key).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Thin wrapper around `git::build_release_comment` (kept pure/testable there).
fn build_release_comment(user: Option<&str>, commits: &[git::Commit]) -> Option<String> {
    git::build_release_comment(user, commits)
}

fn open_workspace(cli_workspace: &Option<PathBuf>) -> Result<Connection> {
    let db_path = match cli_workspace {
        Some(dir) => dir.join(db::DB_DIR).join(db::DB_FILE),
        None => {
            let cwd = std::env::current_dir()?;
            db::find_workspace(&cwd).ok_or_else(|| {
                amt::error::msg("no .ametrite workspace found (run `amt init` first)")
            })?
        }
    };
    db::open(&db_path)
}

fn print_json(value: &impl serde::Serialize) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("serialize")
    );
}

/// Serialize an issue with a top-level `"workspace"` field so cross-workspace
/// JSON stays a flat issue object — agents keep reading `.id` while gaining
/// `.workspace` to know which board it came from.
fn issue_with_workspace(i: &Issue, workspace: &str) -> Result<serde_json::Value> {
    let mut v = serde_json::to_value(i)?;
    if let Some(obj) = v.as_object_mut() {
        obj.insert("workspace".into(), serde_json::json!(workspace));
    }
    Ok(v)
}

/// Render a `claim --peek` result: the issue plus `"peek": true`, optionally
/// tagged with the workspace it came from.
fn print_peek(json: bool, i: &Issue, workspace: Option<&str>) {
    if json {
        let mut v = serde_json::to_value(i).expect("serialize");
        if let Some(obj) = v.as_object_mut() {
            obj.insert("peek".into(), serde_json::Value::Bool(true));
            if let Some(ws) = workspace {
                obj.insert("workspace".into(), serde_json::json!(ws));
            }
        }
        print_json(&v);
    } else {
        match workspace {
            Some(ws) => println!("peekable [{ws}] {}", issue_line(i)),
            None => println!("peekable {}", issue_line(i)),
        }
    }
}

/// Render the structured no-work result (reason + counts + retry_after).
fn print_no_work(json: bool, nw: &store::NoWork) {
    if json {
        let mut v = serde_json::to_value(nw).expect("serialize");
        if let Some(obj) = v.as_object_mut() {
            obj.insert("claimed".into(), serde_json::Value::Bool(false));
        }
        print_json(&v);
    } else {
        print!("nothing claimable: {}", nw.reason);
        match nw.retry_after {
            Some(s) => println!(" (retry after {s}s)"),
            None => println!(),
        }
    }
}

/// Compact human duration: "45s", "12m", "3h 20m", "2d 4h".
fn fmt_secs(s: i64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        let (h, m) = (s / 3600, (s % 3600) / 60);
        if m > 0 {
            format!("{h}h {m}m")
        } else {
            format!("{h}h")
        }
    } else {
        let (d, h) = (s / 86400, (s % 86400) / 3600);
        if h > 0 {
            format!("{d}d {h}h")
        } else {
            format!("{d}d")
        }
    }
}

fn issue_line(i: &Issue) -> String {
    let claim = match (&i.claimed_by, &i.claim_expires_at) {
        (Some(by), Some(_)) => format!("  🔒{by}"),
        _ => String::new(),
    };
    let labels = if i.labels.is_empty() {
        String::new()
    } else {
        format!("  [{}]", i.labels.join(","))
    };
    format!(
        "{:<8} {:<12} {:<7} {}{}{}",
        i.id, i.status, i.priority, i.title, labels, claim
    )
}

fn print_issue_full(i: &Issue) {
    println!("{} — {}", i.id, i.title);
    println!("status: {}   priority: {}", i.status, i.priority);
    if let Some(p) = &i.project {
        println!("project: {p}");
    }
    if let Some(a) = &i.assignee {
        println!("assignee: {a}");
    }
    if let Some(p) = &i.parent {
        println!("parent: {p}");
    }
    if let Some(d) = &i.due {
        println!("due: {d}");
    }
    if !i.labels.is_empty() {
        println!("labels: {}", i.labels.join(", "));
    }
    if let (Some(by), Some(exp)) = (&i.claimed_by, &i.claim_expires_at) {
        println!("claimed by: {by} (lease expires {exp})");
    }
    if !i.blockers.is_empty() {
        let list = i
            .blockers
            .iter()
            .map(|b| b.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!("blocked by: {list}");
    }
    if !i.blocks.is_empty() {
        let list = i
            .blocks
            .iter()
            .map(|b| b.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!("blocks: {list}");
    }
    println!("created: {}   updated: {}", i.created_at, i.updated_at);
    if let Some(body) = &i.body {
        if !body.trim().is_empty() {
            println!("\n{}", body.trim_end());
        }
    }
    if !i.backlinks.is_empty() {
        println!("\nbacklinks:");
        for b in &i.backlinks {
            println!("  {} ({}) {}", b.id, b.doc_type, b.title);
        }
    }
    if !i.activity.is_empty() {
        println!("\nactivity:");
        for a in &i.activity {
            match a.kind.as_str() {
                "comment" => println!(
                    "  {} @{}:\n    {}",
                    a.at,
                    a.author,
                    a.body.replace('\n', "\n    ")
                ),
                _ => println!("  {} @{} {}", a.at, a.author, a.body),
            }
        }
    }
}

fn print_context_pack(pack: &ContextPack) {
    print_issue_full(&pack.issue);
    if !pack.decisions.is_empty() {
        println!("\n=== decisions ===");
        for d in &pack.decisions {
            println!("\n{} — {} ({})", d.id, d.title, d.status);
            if let Some(b) = &d.body {
                if !b.trim().is_empty() {
                    println!("{}", b.trim_end());
                }
            }
        }
    }
    if !pack.linked_docs.is_empty() {
        println!("\n=== backlinked docs ===");
        for b in &pack.linked_docs {
            println!("\n{} ({}) — {}", b.id, b.doc_type, b.title);
            if !b.body.trim().is_empty() {
                println!("{}", b.body.trim_end());
            }
        }
    }
    if !pack.fts_hits.is_empty() {
        println!("\n=== related (search) ===");
        for h in &pack.fts_hits {
            println!(
                "{:<12} {:<8} {}\n             {}",
                h.id, h.doc_type, h.title, h.snippet
            );
        }
    }
    if let Some(budget) = pack.budget {
        println!("\nbudget: {budget} chars");
    }
    if !pack.dropped.is_empty() {
        println!("dropped (over budget): {}", pack.dropped.join(", "));
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Init { name, prefix } => {
            let dir = cli.workspace.clone().unwrap_or(std::env::current_dir()?);
            let name = name.unwrap_or_else(|| {
                dir.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "workspace".into())
            });
            let path = db::init(&dir, &name, &prefix)?;
            registry::try_register(&amt::wikilink::slugify(&name), &dir);
            if cli.json {
                print_json(&serde_json::json!({ "workspace": name, "db": path }));
            } else {
                println!("initialized workspace '{name}' at {}", path.display());
            }
            Ok(())
        }
        Cmd::Issue { ref cmd } => {
            // `list --all-workspaces` fans out over the registry and needs no
            // local workspace, matching `search`/`claim --all-workspaces`.
            if let IssueCmd::List {
                status,
                assignee,
                project,
                label,
                all,
                all_workspaces: true,
                limit,
            } = cmd
            {
                let filter = store::IssueFilter {
                    status: status.clone(),
                    assignee: assignee.clone(),
                    project: project.clone(),
                    label: label.clone(),
                    claimed: None,
                    include_closed: *all,
                    limit: *limit,
                };
                let per = registry::for_each_workspace(|c| store::list_issues(c, &filter))?;
                if cli.json {
                    let mut rows: Vec<serde_json::Value> = Vec::new();
                    for (ws, issues) in &per {
                        for i in issues {
                            rows.push(issue_with_workspace(i, ws)?);
                        }
                    }
                    print_json(&rows);
                } else if per.iter().all(|(_, v)| v.is_empty()) {
                    println!("no issues in any workspace");
                } else {
                    for (ws, issues) in &per {
                        for i in issues {
                            println!("[{ws}] {}", issue_line(i));
                        }
                    }
                }
                return Ok(());
            }
            let mut conn = open_workspace(&cli.workspace)?;
            match cmd {
                IssueCmd::Create {
                    title,
                    body,
                    priority,
                    project,
                    labels,
                    assignee,
                    parent,
                    due,
                    blocked_by,
                    blocks,
                } => {
                    // Validate dependency targets up front: create_issue and
                    // add_block are separate transactions, so a bad --blocks /
                    // --blocked-by key would otherwise leave an orphan issue
                    // behind after the command exits non-zero. A brand-new issue
                    // can't cycle or self-block, so existence is the only risk.
                    let me = identity(None);
                    for b in blocked_by.iter().chain(blocks) {
                        store::get_issue(&conn, b)
                            .map_err(|_| amt::error::msg(format!("no issue '{b}' to depend on")))?;
                    }
                    let issue = store::create_issue(
                        &mut conn,
                        store::NewIssue {
                            title: title.clone(),
                            body: body.clone(),
                            priority: priority.clone(),
                            project: project.clone(),
                            labels: labels.clone(),
                            assignee: assignee.clone(),
                            parent: parent.clone(),
                            due: due.clone(),
                            author: me.clone(),
                        },
                    )?;
                    for b in blocked_by {
                        store::add_block(&mut conn, b, &issue.id, &me)?;
                    }
                    for b in blocks {
                        store::add_block(&mut conn, &issue.id, b, &me)?;
                    }
                    let issue = store::get_issue(&conn, &issue.id)?;
                    if cli.json {
                        print_json(&issue);
                    } else {
                        println!("created {}", issue_line(&issue));
                    }
                }
                IssueCmd::List {
                    status,
                    assignee,
                    project,
                    label,
                    all,
                    all_workspaces: _, // handled above (needs no local workspace)
                    limit,
                } => {
                    let filter = store::IssueFilter {
                        status: status.clone(),
                        assignee: assignee.clone(),
                        project: project.clone(),
                        label: label.clone(),
                        claimed: None,
                        include_closed: *all,
                        limit: *limit,
                    };
                    let issues = store::list_issues(&conn, &filter)?;
                    if cli.json {
                        print_json(&issues);
                    } else if issues.is_empty() {
                        println!("no issues");
                    } else {
                        for i in &issues {
                            println!("{}", issue_line(i));
                        }
                    }
                }
                IssueCmd::Show { id } => {
                    let issue = store::get_issue(&conn, id)?;
                    // R5: list commits referencing this key when inside a git
                    // repo; degrade silently (empty) otherwise.
                    let commits = git_commits_for(&cli.workspace, &issue.id);
                    if cli.json {
                        let mut v = serde_json::to_value(&issue)?;
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert("commits".into(), serde_json::to_value(&commits)?);
                        }
                        print_json(&v);
                    } else {
                        print_issue_full(&issue);
                        if !commits.is_empty() {
                            println!("\ncommits:");
                            for c in &commits {
                                println!("  {} {}", c.hash, c.subject);
                            }
                        }
                    }
                }
                IssueCmd::Update {
                    id,
                    title,
                    body,
                    status,
                    priority,
                    project,
                    assignee,
                    parent,
                    due,
                    add_labels,
                    remove_labels,
                    blocked_by,
                    blocks,
                } => {
                    let clearable = |v: &Option<String>| -> Option<Option<String>> {
                        v.as_ref()
                            .map(|s| if s.is_empty() { None } else { Some(s.clone()) })
                    };
                    let me = identity(None);
                    // Apply dependency edges first: add_block validates target
                    // existence, self-blocks, and cycles, so a bad --blocks /
                    // --blocked-by aborts before any field change is committed.
                    for b in blocked_by {
                        store::add_block(&mut conn, b, id, &me)?;
                    }
                    for b in blocks {
                        store::add_block(&mut conn, id, b, &me)?;
                    }
                    store::update_issue(
                        &mut conn,
                        id,
                        store::IssuePatch {
                            title: title.clone(),
                            body: body.clone(),
                            status: status.clone(),
                            priority: priority.clone(),
                            project: clearable(project),
                            assignee: clearable(assignee),
                            parent: clearable(parent),
                            due: clearable(due),
                            add_labels: add_labels.clone(),
                            remove_labels: remove_labels.clone(),
                        },
                        &me,
                    )?;
                    let issue = store::get_issue(&conn, id)?;
                    if cli.json {
                        print_json(&issue);
                    } else {
                        println!("updated {}", issue_line(&issue));
                    }
                }
                IssueCmd::Comment { id, body, author } => {
                    store::add_comment(&mut conn, id, &identity(author.clone()), body)?;
                    if cli.json {
                        print_json(&serde_json::json!({ "ok": true }));
                    } else {
                        println!("commented on {id}");
                    }
                }
            }
            Ok(())
        }
        Cmd::Dep { ref cmd } => {
            let mut conn = open_workspace(&cli.workspace)?;
            match cmd {
                DepCmd::Add { blocker, blocked } => {
                    store::add_block(&mut conn, blocker, blocked, &identity(None))?;
                    if cli.json {
                        print_json(&serde_json::json!({
                            "ok": true, "blocker": blocker, "blocked": blocked
                        }));
                    } else {
                        println!("{blocker} now blocks {blocked}");
                    }
                }
                DepCmd::Rm { blocker, blocked } => {
                    store::remove_block(&mut conn, blocker, blocked, &identity(None))?;
                    if cli.json {
                        print_json(&serde_json::json!({
                            "ok": true, "blocker": blocker, "blocked": blocked
                        }));
                    } else {
                        println!("{blocker} no longer blocks {blocked}");
                    }
                }
                DepCmd::List { id } => {
                    let blockers = store::blockers_of(&conn, id)?;
                    let blocks = store::blocked_by(&conn, id)?;
                    if cli.json {
                        print_json(&serde_json::json!({
                            "id": id, "blocked_by": blockers, "blocks": blocks
                        }));
                    } else {
                        if blockers.is_empty() {
                            println!("{id} is blocked by: (nothing open)");
                        } else {
                            println!(
                                "{id} is blocked by: {}",
                                blockers
                                    .iter()
                                    .map(|b| b.id.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                        }
                        if blocks.is_empty() {
                            println!("{id} blocks: (nothing)");
                        } else {
                            println!(
                                "{id} blocks: {}",
                                blocks
                                    .iter()
                                    .map(|b| b.id.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                        }
                    }
                }
            }
            Ok(())
        }
        Cmd::Claim {
            issue,
            agent,
            project,
            label,
            peek,
            from,
            ttl,
            cooldown,
            all_workspaces,
        } => {
            let agent = identity(agent);
            // `--issue KEY` is inherently single-workspace; combining it with
            // --all-workspaces is a mistake, so reject it rather than silently
            // ignoring the flag and claiming from the local workspace.
            if all_workspaces && issue.is_some() {
                return Err(amt::error::msg(
                    "--all-workspaces can't combine with --issue (a key is workspace-specific); \
                     drop one",
                ));
            }
            if peek && issue.is_some() {
                return Err(amt::error::msg(
                    "--peek reports the best claimable issue; it can't combine with --issue",
                ));
            }
            for s in &from {
                if !CLAIMABLE_STATUSES.contains(&s.as_str()) {
                    return Err(amt::error::msg(format!(
                        "invalid --from stage '{s}' (one of {CLAIMABLE_STATUSES:?})"
                    )));
                }
            }
            let stages: Option<&[String]> = if from.is_empty() { None } else { Some(&from) };
            let filter = store::ClaimFilter {
                stages,
                project: project.as_deref(),
                label: label.as_deref(),
            };

            // --peek: read-only, never claims, never writes activity.
            if peek {
                if all_workspaces {
                    match registry::peek_any_workspace(&agent, cooldown, &filter)? {
                        Some((ws, i)) => print_peek(cli.json, &i, Some(&ws)),
                        None => print_no_work(
                            cli.json,
                            &registry::no_work_any_workspace(&agent, cooldown, &filter)?,
                        ),
                    }
                    return Ok(());
                }
                let conn = open_workspace(&cli.workspace)?;
                match store::peek_next(&conn, &agent, cooldown, &filter)? {
                    Some(i) => print_peek(cli.json, &i, None),
                    None => print_no_work(
                        cli.json,
                        &store::no_work_reason(&conn, &agent, cooldown, &filter)?,
                    ),
                }
                return Ok(());
            }

            // Cross-workspace claim: fan out over the registry, no local
            // workspace required.
            if all_workspaces {
                match registry::claim_any_workspace(&agent, ttl, cooldown, &filter)? {
                    Some((ws, i)) => {
                        if cli.json {
                            print_json(&issue_with_workspace(&i, &ws)?);
                        } else {
                            println!("claimed [{ws}] {}", issue_line(&i));
                        }
                    }
                    None => print_no_work(
                        cli.json,
                        &registry::no_work_any_workspace(&agent, cooldown, &filter)?,
                    ),
                }
                return Ok(());
            }

            let mut conn = open_workspace(&cli.workspace)?;
            let claimed = match issue {
                Some(key) => Some(store::claim_issue(&mut conn, &key, &agent, ttl)?),
                None => store::claim_next(&mut conn, &agent, ttl, cooldown, &filter)?,
            };
            match claimed {
                Some(i) => {
                    if cli.json {
                        print_json(&i);
                    } else {
                        println!("claimed {}", issue_line(&i));
                    }
                }
                None => print_no_work(
                    cli.json,
                    &store::no_work_reason(&conn, &agent, cooldown, &filter)?,
                ),
            }
            Ok(())
        }
        Cmd::Release {
            id,
            agent,
            status,
            comment,
        } => {
            let mut conn = open_workspace(&cli.workspace)?;
            // Resolve the canonical key first so the git grep matches how commits
            // reference it (get_issue validates existence and normalizes case).
            let issue_key = store::get_issue(&conn, &id)?.id;
            // R5: append the commits on this branch that reference the issue to
            // the closing comment. Graceful no-op outside a git repo.
            let commits = git_commits_since_base(&cli.workspace, &issue_key);
            let final_comment = build_release_comment(comment.as_deref(), &commits);
            let issue = store::release_issue(
                &mut conn,
                &id,
                &identity(agent),
                &status,
                final_comment.as_deref(),
            )?;
            if cli.json {
                print_json(&issue);
            } else {
                println!("released {}", issue_line(&issue));
            }
            Ok(())
        }
        Cmd::Decide {
            issue,
            title,
            body,
            status,
            supersedes,
            author,
        } => {
            let mut conn = open_workspace(&cli.workspace)?;
            let decision = store::record_decision(
                &mut conn,
                store::NewDecision {
                    title,
                    body,
                    resolves: issue,
                    status,
                    supersedes,
                    author: identity(author),
                },
            )?;
            if cli.json {
                print_json(&decision);
            } else {
                println!(
                    "recorded {} — {} (resolves {})",
                    decision.id, decision.title, decision.resolves
                );
            }
            Ok(())
        }
        Cmd::Decision { ref cmd } => {
            let conn = open_workspace(&cli.workspace)?;
            match cmd {
                DecisionCmd::List { issue, all } => {
                    let decisions = store::list_decisions(&conn, issue.as_deref(), *all)?;
                    if cli.json {
                        print_json(&decisions);
                    } else if decisions.is_empty() {
                        println!("no decisions");
                    } else {
                        for d in &decisions {
                            let sup = d
                                .superseded_by
                                .as_ref()
                                .map(|s| format!("  → superseded by {s}"))
                                .unwrap_or_default();
                            println!(
                                "{:<6} {:<11} {}  (resolves {}){}",
                                d.id, d.status, d.title, d.resolves, sup
                            );
                        }
                    }
                }
                DecisionCmd::Show { id } => {
                    let d = store::get_decision(&conn, id)?;
                    if cli.json {
                        print_json(&d);
                    } else {
                        println!("{} — {}", d.id, d.title);
                        println!("resolves: {}   status: {}", d.resolves, d.status);
                        if let Some(s) = &d.superseded_by {
                            println!("superseded by: {s}");
                        }
                        println!("recorded: {}", d.created_at);
                        if let Some(b) = &d.body {
                            println!("\n{}", b.trim_end());
                        }
                    }
                }
            }
            Ok(())
        }
        Cmd::Note { ref cmd } => {
            let mut conn = open_workspace(&cli.workspace)?;
            match cmd {
                NoteCmd::Create {
                    title,
                    body,
                    tags,
                    dedupe,
                    strict,
                } => {
                    let dupes = if *dedupe || *strict {
                        store::find_similar_notes(&conn, title)?
                    } else {
                        Vec::new()
                    };
                    // --strict implies dedupe checking; on a hit, refuse (the
                    // Err becomes exit code 1 via main()).
                    if *strict && !dupes.is_empty() {
                        let list = dupes
                            .iter()
                            .map(|d| {
                                format!("{} ({:.0}% match) {}", d.id, d.score * 100.0, d.title)
                            })
                            .collect::<Vec<_>>()
                            .join("; ");
                        return Err(amt::error::msg(format!(
                            "refusing to create note: near-duplicate(s) exist: {list}"
                        )));
                    }
                    let doc = store::create_doc(
                        &mut conn,
                        store::NewNote {
                            title: title.clone(),
                            body: body.clone(),
                            tags: tags.clone(),
                            doc_type: "note".into(),
                            author: identity(None),
                        },
                    )?;
                    if cli.json {
                        if dupes.is_empty() {
                            print_json(&doc);
                        } else {
                            let duplicates: Vec<_> = dupes
                                .iter()
                                .map(|d| {
                                    serde_json::json!({
                                        "id": d.id, "title": d.title, "score": d.score
                                    })
                                })
                                .collect();
                            print_json(&serde_json::json!({
                                "doc": doc,
                                "warning": "near-duplicate note(s) already exist",
                                "duplicates": duplicates,
                            }));
                        }
                    } else {
                        println!("created note '{}' ({})", doc.title, doc.id);
                        if !dupes.is_empty() {
                            eprintln!("warning: near-duplicate note(s) already exist:");
                            for d in &dupes {
                                eprintln!("  {} ({:.0}% match) {}", d.id, d.score * 100.0, d.title);
                            }
                        }
                    }
                }
                NoteCmd::Show { id } => {
                    let doc = store::get_doc(&conn, id)?;
                    if cli.json {
                        print_json(&doc);
                    } else {
                        println!("{} — {}", doc.id, doc.title);
                        if !doc.tags.is_empty() {
                            println!("tags: {}", doc.tags.join(", "));
                        }
                        if let Some(b) = &doc.body {
                            println!("\n{}", b.trim_end());
                        }
                        if !doc.backlinks.is_empty() {
                            println!("\nbacklinks:");
                            for b in &doc.backlinks {
                                println!("  {} ({}) {}", b.id, b.doc_type, b.title);
                            }
                        }
                    }
                }
                NoteCmd::Append { id, body } => {
                    let doc = store::append_to_doc(&mut conn, id, body, &identity(None))?;
                    if cli.json {
                        print_json(&doc);
                    } else {
                        println!("appended to {}", doc.id);
                    }
                }
                NoteCmd::List => {
                    let docs = store::list_docs(&conn, "note")?;
                    if cli.json {
                        print_json(&docs);
                    } else {
                        for d in &docs {
                            println!("{:<28} {}", d.id, d.title);
                        }
                    }
                }
            }
            Ok(())
        }
        Cmd::Project { ref cmd } => {
            let mut conn = open_workspace(&cli.workspace)?;
            match cmd {
                ProjectCmd::Create { title, body } => {
                    let doc = store::create_doc(
                        &mut conn,
                        store::NewNote {
                            title: title.clone(),
                            body: body.clone(),
                            tags: Vec::new(),
                            doc_type: "project".into(),
                            author: identity(None),
                        },
                    )?;
                    if cli.json {
                        print_json(&doc);
                    } else {
                        println!("created project '{}' ({})", doc.title, doc.id);
                    }
                }
                ProjectCmd::List => {
                    let docs = store::list_docs(&conn, "project")?;
                    if cli.json {
                        print_json(&docs);
                    } else {
                        for d in &docs {
                            println!("{:<28} {}", d.id, d.title);
                        }
                    }
                }
            }
            Ok(())
        }
        Cmd::Search {
            query,
            doc_type,
            status,
            tag,
            project,
            all_workspaces,
            limit,
        } => {
            let q = query.join(" ");
            let filter = store::SearchFilter {
                doc_type,
                status,
                tag,
                project,
                limit,
            };
            if all_workspaces {
                let per = registry::for_each_workspace(|c| store::search(c, &q, &filter))?;
                if cli.json {
                    let mut rows: Vec<serde_json::Value> = Vec::new();
                    for (ws, hits) in &per {
                        for h in hits {
                            let mut v = serde_json::to_value(h)?;
                            if let Some(o) = v.as_object_mut() {
                                o.insert("workspace".into(), serde_json::json!(ws));
                            }
                            rows.push(v);
                        }
                    }
                    print_json(&rows);
                } else if per.iter().all(|(_, v)| v.is_empty()) {
                    println!("no results");
                } else {
                    for (ws, hits) in &per {
                        for h in hits {
                            println!(
                                "[{ws}] {:<12} {:<8} {}\n             {}",
                                h.id, h.doc_type, h.title, h.snippet
                            );
                        }
                    }
                }
                return Ok(());
            }
            let conn = open_workspace(&cli.workspace)?;
            let hits = store::search(&conn, &q, &filter)?;
            if cli.json {
                print_json(&hits);
            } else if hits.is_empty() {
                println!("no results");
            } else {
                for h in &hits {
                    println!(
                        "{:<12} {:<8} {}\n             {}",
                        h.id, h.doc_type, h.title, h.snippet
                    );
                }
            }
            Ok(())
        }
        Cmd::Context { key, budget } => {
            let conn = open_workspace(&cli.workspace)?;
            let pack = store::context_pack(&conn, &key, budget)?;
            if cli.json {
                print_json(&pack);
            } else {
                print_context_pack(&pack);
            }
            Ok(())
        }
        Cmd::Backlinks { id } => {
            let conn = open_workspace(&cli.workspace)?;
            let links = store::backlinks(&conn, &id)?;
            if cli.json {
                print_json(&links);
            } else if links.is_empty() {
                println!("no backlinks");
            } else {
                for b in &links {
                    println!("{:<12} {:<8} {}", b.id, b.doc_type, b.title);
                }
            }
            Ok(())
        }
        Cmd::Doctor => {
            let conn = open_workspace(&cli.workspace)?;
            let report = store::doctor(&conn)?;
            if cli.json {
                print_json(&report);
            } else if report.ok {
                println!("workspace healthy ✓");
            } else {
                for l in &report.unresolved_links {
                    println!("unresolved link: {} → [[{}]]", l.source, l.target);
                }
                for c in &report.stale_claims {
                    println!(
                        "stale claim: {} held by {} (lease expired {})",
                        c.id, c.claimed_by, c.expired_at
                    );
                }
                for m in &report.missing_parents {
                    println!("missing parent: {} → {}", m.id, m.references);
                }
                for m in &report.missing_projects {
                    println!("missing project: {} → {}", m.id, m.references);
                }
                for m in &report.dangling_decisions {
                    println!(
                        "dangling decision: {} resolves missing issue {}",
                        m.id, m.references
                    );
                }
                for c in &report.dependency_cycles {
                    println!("dependency cycle: {}", c.cycle.join(" → "));
                }
            }
            Ok(())
        }
        Cmd::Agents => {
            let conn = open_workspace(&cli.workspace)?;
            let roster = store::agents(&conn)?;
            if cli.json {
                print_json(&roster);
            } else if roster.is_empty() {
                println!("no agents have acted yet");
            } else {
                println!(
                    "{:<18} {:>7} {:>6} {:>5}  LAST ACTIVITY",
                    "AGENT", "LEASES", "CLAIMS", "DONE"
                );
                for a in &roster {
                    let leases = if a.active_leases.is_empty() {
                        "-".to_string()
                    } else {
                        format!(
                            "{}{}",
                            a.active_leases.len(),
                            if a.has_stale_lease { "⚠" } else { "🔒" }
                        )
                    };
                    println!(
                        "{:<18} {:>7} {:>6} {:>5}  {}",
                        a.name,
                        leases,
                        a.claims,
                        a.completed,
                        a.last_activity.as_deref().unwrap_or("-")
                    );
                }
            }
            Ok(())
        }
        Cmd::Stats { since } => {
            let conn = open_workspace(&cli.workspace)?;
            let stats = store::stats(&conn, since.as_deref())?;
            if cli.json {
                print_json(&stats);
            } else {
                println!("Stats ({})", stats.since.as_deref().unwrap_or("all time"));
                println!("  throughput:  {} issue(s) done", stats.throughput);
                match (stats.avg_cycle_secs, stats.median_cycle_secs) {
                    (Some(a), Some(m)) => {
                        println!("  cycle time:  avg {}, median {}", fmt_secs(a), fmt_secs(m))
                    }
                    _ => println!("  cycle time:  —"),
                }
                if stats.integrity.ok {
                    println!("  integrity:   ✓ no overlapping claims");
                } else {
                    println!(
                        "  integrity:   ✗ {} overlapping claim(s):",
                        stats.integrity.overlaps.len()
                    );
                    for o in &stats.integrity.overlaps {
                        println!(
                            "    {} — {} claimed while {} held the lease (at {})",
                            o.issue, o.claimant, o.holder, o.at
                        );
                    }
                }
            }
            Ok(())
        }
        Cmd::Events {
            since,
            follow,
            limit,
        } => {
            use std::io::Write;
            let conn = open_workspace(&cli.workspace)?;
            // Guard the drain loop: LIMIT 0 returns 0 rows forever (0 < 0 never
            // breaks), and a negative LIMIT means "no cap" in SQLite — pin to >= 1.
            let limit = limit.max(1);
            // Start cursor: explicit --since wins; otherwise a follower tails
            // from the current tip (new events only), while a one-shot dump
            // replays the whole log.
            let mut cursor = match since {
                Some(c) => c,
                None if follow => store::events_cursor(&conn)?,
                None => 0,
            };
            let mut out = std::io::stdout();
            // Drain fully: `limit` bounds each fetch (memory), but we loop until
            // caught up so nothing past the first batch is ever silently dropped
            // — a one-shot dump emits the whole log, and a follow poll flushes
            // an entire burst even if it exceeds `limit`.
            let mut emit = |cursor: &mut i64| -> Result<()> {
                loop {
                    let batch = store::events(&conn, *cursor, limit)?;
                    for e in &batch {
                        // NDJSON: one compact JSON object per line.
                        writeln!(out, "{}", serde_json::to_string(e).expect("serialize"))
                            .map_err(|e| amt::error::msg(e.to_string()))?;
                        *cursor = e.cursor;
                    }
                    if (batch.len() as i64) < limit {
                        break;
                    }
                }
                out.flush().ok();
                Ok(())
            };
            emit(&mut cursor)?;
            if follow {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    emit(&mut cursor)?;
                }
            }
            Ok(())
        }
        Cmd::Export { dir } => {
            let conn = open_workspace(&cli.workspace)?;
            let n = export::export(&conn, &dir)?;
            if cli.json {
                print_json(&serde_json::json!({ "exported": n }));
            } else {
                println!("exported {n} documents to {}", dir.display());
            }
            Ok(())
        }
        Cmd::Import { dir } => {
            let mut conn = open_workspace(&cli.workspace)?;
            let n = export::import(&mut conn, &dir)?;
            if cli.json {
                print_json(&serde_json::json!({ "imported": n }));
            } else {
                println!("imported {n} documents from {}", dir.display());
            }
            Ok(())
        }
        Cmd::Mcp => {
            let conn = open_workspace(&cli.workspace)?;
            mcp::serve(conn)
        }
        Cmd::Ws { ref cmd } => match cmd {
            WsCmd::Add { path, alias } => {
                let root = match path {
                    Some(p) => p.clone(),
                    None => {
                        let cwd = std::env::current_dir()?;
                        let db_path = db::find_workspace(&cwd).ok_or_else(|| {
                            amt::error::msg(
                                "no .ametrite workspace here — pass a path or run `amt init`",
                            )
                        })?;
                        db_path.parent().unwrap().parent().unwrap().to_path_buf()
                    }
                };
                let alias = alias.clone().unwrap_or_else(|| {
                    amt::wikilink::slugify(
                        &root
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "workspace".into()),
                    )
                });
                registry::add(&alias, &root)?;
                println!("registered '{alias}' → {}", root.display());
                Ok(())
            }
            WsCmd::List => {
                let map = registry::load()?;
                if cli.json {
                    print_json(&map);
                } else if map.is_empty() {
                    println!("no workspaces registered (amt ws add [path])");
                } else {
                    for (alias, root) in &map {
                        println!("{alias:<20} {root}");
                    }
                }
                Ok(())
            }
            WsCmd::Remove { alias } => {
                if registry::remove(alias)? {
                    println!("removed '{alias}'");
                } else {
                    println!("'{alias}' was not registered");
                }
                Ok(())
            }
        },
        Cmd::Hook { ref cmd } => {
            let root = workspace_root(&cli.workspace)?;
            let repo = git::repo_root(&root)?.ok_or_else(|| {
                amt::error::msg(format!(
                    "not a git repository at {} — `amt hook` needs one",
                    root.display()
                ))
            })?;
            match cmd {
                HookCmd::Install => {
                    let action = git::install_hook(&repo)?;
                    let msg = match action {
                        git::HookAction::Installed => "installed commit-msg hook",
                        git::HookAction::AlreadyInstalled => "commit-msg hook already installed",
                        git::HookAction::Appended => {
                            "appended amt block to existing commit-msg hook"
                        }
                        _ => "installed commit-msg hook",
                    };
                    if cli.json {
                        print_json(&serde_json::json!({
                            "ok": true, "action": format!("{action:?}").to_lowercase(),
                            "repo": repo,
                        }));
                    } else {
                        println!("{msg} in {}", repo.display());
                    }
                }
                HookCmd::Uninstall => {
                    let action = git::uninstall_hook(&repo)?;
                    let msg = match action {
                        git::HookAction::Removed => "removed commit-msg hook",
                        _ => "no amt commit-msg hook to remove",
                    };
                    if cli.json {
                        print_json(&serde_json::json!({
                            "ok": true, "action": format!("{action:?}").to_lowercase(),
                            "repo": repo,
                        }));
                    } else {
                        println!("{msg} in {}", repo.display());
                    }
                }
            }
            Ok(())
        }
        Cmd::Branch { key } => {
            let conn = open_workspace(&cli.workspace)?;
            // Validate the key and derive a slug from the real title.
            let issue = store::get_issue(&conn, &key)?;
            let root = workspace_root(&cli.workspace)?;
            let repo = git::repo_root(&root)?.ok_or_else(|| {
                amt::error::msg(format!(
                    "not a git repository at {} — `amt branch` needs one",
                    root.display()
                ))
            })?;
            let slug = amt::wikilink::slugify(&issue.title);
            let branch = format!("{}-{}", issue.id.to_lowercase(), slug);
            git::create_branch(&repo, &branch)?;
            if cli.json {
                print_json(&serde_json::json!({
                    "ok": true, "branch": branch, "issue": issue.id,
                }));
            } else {
                println!("created and checked out branch {branch} (for {})", issue.id);
            }
            Ok(())
        }
        Cmd::Seed { count, agent } => {
            let me = identity(agent);
            let mut conn = open_workspace(&cli.workspace)?;
            let start = std::time::Instant::now();
            let n = store::seed(&mut conn, count, &me)?;
            let elapsed_ms = start.elapsed().as_millis();
            if cli.json {
                print_json(&serde_json::json!({ "seeded": n, "elapsed_ms": elapsed_ms }));
            } else {
                println!("seeded {n} issues in {elapsed_ms}ms");
            }
            Ok(())
        }
        Cmd::Gc => {
            let conn = open_workspace(&cli.workspace)?;
            let r = db::gc(&conn)?;
            let reclaimed = (r.bytes_before - r.bytes_after).max(0);
            if cli.json {
                print_json(&serde_json::json!({
                    "bytes_before": r.bytes_before,
                    "bytes_after": r.bytes_after,
                    "bytes_reclaimed": reclaimed,
                    "wal_frames_checkpointed": r.wal_frames_checkpointed,
                }));
            } else {
                println!(
                    "gc: {} → {} ({} reclaimed, {} WAL frames checkpointed)",
                    fmt_bytes(r.bytes_before),
                    fmt_bytes(r.bytes_after),
                    fmt_bytes(reclaimed),
                    r.wal_frames_checkpointed,
                );
            }
            Ok(())
        }
    }
}

/// Compact human byte size: "512 B", "8.0 KB", "3.2 MB".
fn fmt_bytes(n: i64) -> String {
    const KB: f64 = 1024.0;
    let f = n as f64;
    if f < KB {
        format!("{n} B")
    } else if f < KB * KB {
        format!("{:.1} KB", f / KB)
    } else {
        format!("{:.1} MB", f / (KB * KB))
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
