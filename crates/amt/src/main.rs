use amt::error::Result;
use amt::model::*;
use amt::{db, export, mcp, registry, store};
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
        /// Lease TTL in seconds
        #[arg(long, default_value_t = 900)]
        ttl: i64,
        /// Seconds before an issue you released can be re-served to you (0 disables)
        #[arg(long, default_value_t = 3600)]
        cooldown: i64,
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
        #[arg(long, default_value_t = 25)]
        limit: i64,
    },
    /// Show documents that link to the given document
    Backlinks { id: String },
    /// Check workspace health (unresolved links, stale claims, missing refs)
    Doctor,
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
                } => {
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
                            author: identity(None),
                        },
                    )?;
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
                    limit,
                } => {
                    let issues = store::list_issues(
                        &conn,
                        &store::IssueFilter {
                            status: status.clone(),
                            assignee: assignee.clone(),
                            project: project.clone(),
                            label: label.clone(),
                            claimed: None,
                            include_closed: *all,
                            limit: *limit,
                        },
                    )?;
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
                    if cli.json {
                        print_json(&issue);
                    } else {
                        print_issue_full(&issue);
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
                } => {
                    let clearable = |v: &Option<String>| -> Option<Option<String>> {
                        v.as_ref()
                            .map(|s| if s.is_empty() { None } else { Some(s.clone()) })
                    };
                    let issue = store::update_issue(
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
                        &identity(None),
                    )?;
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
        Cmd::Claim {
            issue,
            agent,
            project,
            label,
            ttl,
            cooldown,
        } => {
            let mut conn = open_workspace(&cli.workspace)?;
            let agent = identity(agent);
            let claimed = match issue {
                Some(key) => Some(store::claim_issue(&mut conn, &key, &agent, ttl)?),
                None => store::claim_next(
                    &mut conn,
                    &agent,
                    project.as_deref(),
                    label.as_deref(),
                    ttl,
                    cooldown,
                )?,
            };
            match claimed {
                Some(i) => {
                    if cli.json {
                        print_json(&i);
                    } else {
                        println!("claimed {}", issue_line(&i));
                    }
                }
                None => {
                    if cli.json {
                        print_json(&serde_json::json!({ "claimed": false }));
                    } else {
                        println!("nothing claimable");
                    }
                }
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
            let issue = store::release_issue(
                &mut conn,
                &id,
                &identity(agent),
                &status,
                comment.as_deref(),
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
                NoteCmd::Create { title, body, tags } => {
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
                        print_json(&doc);
                    } else {
                        println!("created note '{}' ({})", doc.title, doc.id);
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
            limit,
        } => {
            let conn = open_workspace(&cli.workspace)?;
            let hits = store::search(
                &conn,
                &query.join(" "),
                &store::SearchFilter {
                    doc_type,
                    status,
                    tag,
                    project,
                    limit,
                },
            )?;
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
