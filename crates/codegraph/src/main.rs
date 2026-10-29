use anyhow::{bail, Context as _, Result};
use clap::{Args, Parser, Subcommand};
use codegraph::format::{self, FilesFormat};
use codegraph::installer::{self, Env, Location, Target};
use codegraph::project::{unsafe_root_reason, CodeGraph};
use codegraph::{ContextFormat, ContextOptions, Progress};
use codegraph_core::search::is_test_path;
use codegraph_core::SearchOptions;
use codegraph_graph::{DEPENDENCY_EDGES, FLOW_EDGES};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "codegraph",
    version,
    about = "Semantic code intelligence for AI coding agents — 100% local."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct PathOpt {
    /// Project path (defaults to the nearest indexed directory)
    #[arg(short, long)]
    path: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Create an index for a project and build it
    Init {
        path: Option<PathBuf>,
        /// Initialize even a home directory or filesystem root
        #[arg(short, long)]
        force: bool,
        /// Only create the index directory; don't index yet
        #[arg(long)]
        no_index: bool,
    },
    /// Delete a project's index
    Uninit { path: Option<PathBuf> },
    /// Rebuild the whole index
    Index {
        path: Option<PathBuf>,
        #[arg(short, long)]
        force: bool,
        #[arg(short, long)]
        quiet: bool,
    },
    /// Re-index files changed since the last sync
    Sync {
        path: Option<PathBuf>,
        #[arg(short, long)]
        quiet: bool,
    },
    /// Index statistics and pending changes
    Status {
        path: Option<PathBuf>,
        #[arg(short, long)]
        json: bool,
    },
    /// Search symbols by name
    Query {
        search: String,
        #[command(flatten)]
        path: PathOpt,
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
        /// Filter by node kind (function, class, …)
        #[arg(short, long)]
        kind: Option<String>,
        #[arg(short, long)]
        json: bool,
    },
    /// Source of the symbols a question is about, with call paths and blast radius
    Explore {
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        #[command(flatten)]
        path: PathOpt,
        #[arg(long)]
        max_files: Option<usize>,
    },
    /// Build task context (symbols, relationships, code) for an AI agent
    Context {
        #[arg(required = true, num_args = 1..)]
        task: Vec<String>,
        #[command(flatten)]
        path: PathOpt,
        /// markdown or json
        #[arg(short, long, default_value = "markdown")]
        format: String,
        #[arg(short = 'n', long)]
        max_nodes: Option<usize>,
        /// Omit code blocks
        #[arg(long)]
        no_code: bool,
    },
    /// One symbol's details, or a file's source (`--file` alone)
    Node {
        name: Option<String>,
        #[command(flatten)]
        path: PathOpt,
        #[arg(short, long)]
        file: Option<String>,
        #[arg(long)]
        offset: Option<u32>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        symbols_only: bool,
        #[arg(long)]
        line: Option<u32>,
        /// Include the symbol's source
        #[arg(short, long)]
        code: bool,
    },
    /// List indexed files
    Files {
        #[command(flatten)]
        path: PathOpt,
        /// Only files under this directory
        #[arg(long)]
        filter: Option<String>,
        #[arg(long)]
        pattern: Option<String>,
        /// tree, flat or grouped
        #[arg(long, default_value = "tree")]
        format: String,
        #[arg(long)]
        max_depth: Option<usize>,
        #[arg(long)]
        no_metadata: bool,
        #[arg(short, long)]
        json: bool,
    },
    /// Who calls a symbol
    Callers(NeighborArgs),
    /// What a symbol calls
    Callees(NeighborArgs),
    /// What is affected by changing a symbol
    Impact {
        symbol: String,
        #[command(flatten)]
        path: PathOpt,
        #[arg(short, long)]
        file: Option<String>,
        #[arg(short, long, default_value_t = 2)]
        depth: u32,
        #[arg(short, long)]
        json: bool,
    },
    /// Test files affected by changes to the given files
    Affected {
        files: Vec<String>,
        #[command(flatten)]
        path: PathOpt,
        /// Read the file list from stdin, one per line
        #[arg(long)]
        stdin: bool,
        #[arg(short, long, default_value_t = 5)]
        depth: usize,
        /// Glob for the files to report (default: test files)
        #[arg(short, long)]
        filter: Option<String>,
        #[arg(short, long)]
        json: bool,
        #[arg(short, long)]
        quiet: bool,
    },
    /// Symbols nothing references
    DeadCode {
        #[command(flatten)]
        path: PathOpt,
        #[arg(short, long, default_value_t = 50)]
        limit: usize,
    },
    /// Keep the index in sync with file changes until interrupted
    Watch { path: Option<PathBuf> },
    /// Remove a stale writer lock left by a crashed codegraph process
    Unlock { path: Option<PathBuf> },
    /// Git hooks that sync the index after commit, merge and checkout
    GitHooks {
        /// install or remove
        action: String,
        #[command(flatten)]
        path: PathOpt,
    },
    /// Run the MCP server
    Serve {
        #[command(flatten)]
        path: PathOpt,
        /// Use the stdio transport (the only one)
        #[arg(long)]
        mcp: bool,
        /// Don't watch files for changes
        #[arg(long)]
        no_watch: bool,
        /// Serve in this process instead of through the project's shared daemon
        #[arg(long)]
        no_daemon: bool,
    },
    /// List the shared background daemons, or stop them
    Daemon {
        /// list (default) or stop
        action: Option<String>,
        /// Project path (stop: defaults to the nearest indexed directory)
        path: Option<PathBuf>,
        /// stop: every running daemon
        #[arg(long)]
        all: bool,
    },
    /// Register the MCP server with your coding agents
    Install {
        /// Agent ids (comma-separated), or auto (detected), all, none
        #[arg(short, long)]
        target: Option<String>,
        /// global (your user config) or local (this project)
        #[arg(short, long)]
        location: Option<String>,
        /// Non-interactive: global location, detected agents
        #[arg(short, long)]
        yes: bool,
        /// Also index the current directory
        #[arg(short, long)]
        init: bool,
        /// Don't pre-allow the codegraph tools (Claude Code)
        #[arg(long)]
        no_permissions: bool,
        /// Also add a Claude Code prompt hook that front-loads explore output on structural prompts
        #[arg(long)]
        prompt_hook: bool,
        /// Print the config snippet for one agent and exit
        #[arg(long, value_name = "ID")]
        print_config: Option<String>,
    },
    /// Remove CodeGraph from your coding agents' configuration
    Uninstall {
        #[arg(short, long)]
        target: Option<String>,
        #[arg(short, long)]
        location: Option<String>,
        #[arg(short, long)]
        yes: bool,
    },
    /// Claude Code UserPromptSubmit hook (reads {prompt, cwd} JSON on stdin)
    #[command(hide = true)]
    PromptHook,
    /// Open the browser viewer for this project (served on 127.0.0.1)
    Ui {
        #[command(flatten)]
        path: PathOpt,
        /// Port to listen on (default: a free one)
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Don't open a browser
        #[arg(long)]
        no_open: bool,
    },
    /// Upgrade codegraph to the latest release
    Upgrade {
        /// Only report whether a newer release exists
        #[arg(long)]
        check: bool,
        /// Install this release tag (e.g. v0.2.0) instead of the latest
        #[arg(long, value_name = "TAG")]
        version: Option<String>,
        /// Reinstall even when already up to date
        #[arg(long)]
        force: bool,
    },
    /// Anonymous usage telemetry: on, off or status
    Telemetry {
        /// on, off or status
        action: Option<String>,
    },
    /// Print the version
    Version,
    /// Extract one file read from stdin and print the result as JSON (parity runner)
    #[command(name = "__extract", hide = true)]
    Extract {
        /// The file's path, as the project would name it
        file_path: String,
        /// The language, when not detected from the path and content
        #[arg(long)]
        language: Option<String>,
    },
    /// Run one C/C++ pre-parse pass on stdin and print the result (parity runner)
    #[command(name = "__ccpp", hide = true)]
    Ccpp {
        /// The pass, by its upstream name (`blankLoneMacroLines`, `cppPreParse`, …)
        pass: String,
        /// The file's path, for the passes that read it
        #[arg(long, default_value = "")]
        file: String,
        /// The pass's one option (Objective-C rules; keep C return types)
        #[arg(long)]
        option: bool,
    },
}

impl Command {
    /// The command's name, for usage counts.
    fn name(&self) -> &'static str {
        match self {
            Command::Init { .. } => "init",
            Command::Uninit { .. } => "uninit",
            Command::Index { .. } => "index",
            Command::Sync { .. } => "sync",
            Command::Status { .. } => "status",
            Command::Query { .. } => "query",
            Command::Explore { .. } => "explore",
            Command::Context { .. } => "context",
            Command::Node { .. } => "node",
            Command::Files { .. } => "files",
            Command::Callers(_) => "callers",
            Command::Callees(_) => "callees",
            Command::Impact { .. } => "impact",
            Command::Affected { .. } => "affected",
            Command::DeadCode { .. } => "dead-code",
            Command::Watch { .. } => "watch",
            Command::Unlock { .. } => "unlock",
            Command::GitHooks { .. } => "git-hooks",
            Command::Serve { .. } => "serve",
            Command::Daemon { .. } => "daemon",
            Command::Install { .. } => "install",
            Command::Uninstall { .. } => "uninstall",
            Command::PromptHook => "prompt-hook",
            Command::Ui { .. } => "ui",
            Command::Upgrade { .. } => "upgrade",
            Command::Telemetry { .. } => "telemetry",
            Command::Version => "version",
            Command::Extract { .. } => "extract",
            Command::Ccpp { .. } => "ccpp",
        }
    }
}

fn prompt(question: &str, default: &str) -> String {
    if !std::io::stdin().is_terminal() {
        return default.to_string();
    }
    eprint!("{question} [{default}]: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    let t = line.trim();
    if t.is_empty() {
        default.to_string()
    } else {
        t.to_string()
    }
}

/// Resolve `--target` to agent ids for `loc`.
fn select_targets(
    spec: Option<&str>,
    loc: Location,
    env: &Env,
    yes: bool,
    installing: bool,
) -> Result<Vec<Target>> {
    let all = installer::targets();
    let supported: Vec<&Target> = all.iter().filter(|t| t.supports(loc)).collect();
    let detected: Vec<&str> = supported
        .iter()
        .filter(|t| {
            if installing {
                t.detected(env, loc)
            } else {
                t.is_configured(env, loc)
            }
        })
        .map(|t| t.id)
        .collect();
    let spec = match spec {
        Some(s) => s.to_string(),
        None if yes => "auto".into(),
        None => {
            eprintln!(
                "Agents: {}",
                supported
                    .iter()
                    .map(|t| format!("{} ({})", t.id, t.display_name))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let default = if detected.is_empty() {
                "none".to_string()
            } else {
                detected.join(",")
            };
            prompt(
                if installing {
                    "Install for which agents"
                } else {
                    "Remove from which agents"
                },
                &default,
            )
        }
    };
    let ids: Vec<String> = match spec.as_str() {
        "auto" => detected.iter().map(|s| s.to_string()).collect(),
        "all" => supported.iter().map(|t| t.id.to_string()).collect(),
        "none" => vec![],
        list => list
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    };
    let mut out = Vec::new();
    for id in ids {
        match installer::target(&id) {
            Some(t) if t.supports(loc) => out.push(t),
            Some(t) => eprintln!(
                "{} doesn't support a {} install; skipped.",
                t.display_name,
                if loc == Location::Local {
                    "local"
                } else {
                    "global"
                }
            ),
            None => bail!(
                "unknown agent `{id}` (known: {})",
                all.iter().map(|t| t.id).collect::<Vec<_>>().join(", ")
            ),
        }
    }
    Ok(out)
}

fn resolve_location(spec: Option<&str>, yes: bool) -> Result<Location> {
    let s = match spec {
        Some(s) => s.to_string(),
        None if yes => "global".into(),
        None => prompt("Install location (global/local)", "global"),
    };
    s.parse().map_err(|e: String| anyhow::anyhow!(e))
}

fn print_outcome(name: &str, o: &installer::Outcome) {
    println!("{name}:");
    for f in &o.files {
        println!("  {:<9} {}", f.action.as_str(), f.path.display());
    }
    for n in &o.notes {
        println!("  note: {n}");
    }
}

#[derive(Args)]
struct NeighborArgs {
    symbol: String,
    #[command(flatten)]
    path: PathOpt,
    #[arg(short, long)]
    file: Option<String>,
    #[arg(short, long, default_value_t = 20)]
    limit: usize,
    #[arg(short, long)]
    json: bool,
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn open(path: Option<&Path>) -> Result<CodeGraph> {
    let start = path.map(Path::to_path_buf).unwrap_or_else(cwd);
    CodeGraph::open_nearest(&start)
        .with_context(|| format!("no CodeGraph index at or above {}", start.display()))
}

fn progress_printer(quiet: bool) -> impl Fn(Progress) + Sync {
    let tty = std::io::stderr().is_terminal();
    move |p: Progress| {
        if quiet {
            return;
        }
        let msg = match p {
            Progress::Scanning => "Scanning files…".to_string(),
            Progress::Extracting { done, total } => format!("Parsing {done}/{total} files"),
            Progress::Storing { done, total } => format!("Storing {done}/{total} files"),
            Progress::Resolving { done, total } => format!("Resolving references {done}/{total}"),
            Progress::Done => {
                if tty {
                    eprint!("\r\x1b[2K");
                }
                return;
            }
        };
        if tty {
            eprint!("\r\x1b[2K{msg}");
            let _ = std::io::stderr().flush();
        }
    }
}

fn print_report(r: &codegraph::IndexReport, verb: &str) {
    println!(
        "{verb} {} file(s) ({} added, {} changed, {} removed, {} unchanged) in {:.1}s — {} symbols, {} references resolved, {} unresolved",
        r.files_indexed(),
        r.files_added.len(),
        r.files_changed.len(),
        r.files_removed.len(),
        r.files_unchanged,
        r.duration_ms as f64 / 1000.0,
        r.nodes_extracted,
        r.refs_resolved,
        r.refs_failed
    );
    if r.files_with_errors > 0 {
        println!(
            "{} file(s) had extraction errors; see `codegraph status`.",
            r.files_with_errors
        );
    }
}

fn neighbors(a: NeighborArgs, callers: bool) -> Result<()> {
    let cg = open(a.path.path.as_deref())?;
    let (defs, file_ok) = cg.find_symbols(&a.symbol, a.file.as_deref())?;
    if defs.is_empty() {
        bail!("no symbol named `{}` in the index", a.symbol);
    }
    let g = cg.graph();
    let mut out = Vec::new();
    for d in defs {
        let list = if callers {
            g.callers(&d.id, DEPENDENCY_EDGES)?
        } else {
            g.callees(&d.id, FLOW_EDGES)?
        };
        out.push((d, list));
    }
    if a.json {
        let v: Vec<_> = out
            .iter()
            .map(|(d, l)| serde_json::json!({ "definition": d, "results": l.iter().take(a.limit).collect::<Vec<_>>() }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        let note = (!file_ok).then(|| {
            format!(
                "No definition in a file matching `{}`; showing all.",
                a.file.as_deref().unwrap_or("")
            )
        });
        print!(
            "{}",
            format::neighbor_list(
                if callers { "callers" } else { "callees" },
                &out,
                a.limit,
                note.as_deref()
            )
        );
    }
    Ok(())
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Init {
            path,
            force,
            no_index,
        } => {
            let root = path.unwrap_or_else(cwd);
            if let Some(reason) = unsafe_root_reason(&root).filter(|_| !force) {
                bail!(
                    "refusing to index {}: {reason} (use --force to override)",
                    root.display()
                );
            }
            let mut cg = CodeGraph::init(&root)?;
            println!(
                "Initialized CodeGraph in {}",
                codegraph::project::data_dir(cg.root()).display()
            );
            if !no_index {
                let report = cg.index_all(Some(&progress_printer(false)))?;
                print_report(&report, "Indexed");
                record_index(&cg, &report);
            }
        }
        Command::Uninit { path } => {
            let root = path.unwrap_or_else(cwd);
            if codegraph::githooks::installed(&root) {
                codegraph::githooks::remove(&root)?;
                println!("Removed the CodeGraph git hooks.");
            }
            if CodeGraph::uninit(&root)? {
                println!("Removed the CodeGraph index from {}", root.display());
            } else {
                println!("{} has no CodeGraph index.", root.display());
            }
        }
        Command::Unlock { path } => {
            let root = codegraph::find_root(&path.unwrap_or_else(cwd))
                .context("no CodeGraph index found")?;
            match codegraph::lock::force_unlock(&root)? {
                Some(pid) if pid > 0 => println!("Removed the writer lock held by pid {pid}."),
                Some(_) => println!("Removed the writer lock."),
                None => println!("The index isn't locked."),
            }
        }
        Command::GitHooks { action, path } => {
            let root = match path.path {
                Some(p) => p,
                None => codegraph::find_root(&cwd()).unwrap_or_else(cwd),
            };
            match action.as_str() {
                "install" => {
                    for p in codegraph::githooks::install(&root)? {
                        println!("  updated {}", p.display());
                    }
                    println!("The index now syncs after commit, merge and checkout.");
                }
                "remove" | "uninstall" => {
                    let removed = codegraph::githooks::remove(&root)?;
                    if removed.is_empty() {
                        println!("No CodeGraph git hooks found.");
                    }
                    for p in removed {
                        println!("  cleaned {}", p.display());
                    }
                }
                other => bail!("unknown action `{other}` (install, remove)"),
            }
        }
        Command::Index { path, force, quiet } => {
            let root = path.unwrap_or_else(cwd);
            if let Some(reason) = unsafe_root_reason(&root).filter(|_| !force) {
                bail!(
                    "refusing to index {}: {reason} (use --force to override)",
                    root.display()
                );
            }
            let mut cg = if codegraph::is_initialized(&root) {
                CodeGraph::open(&root)?
            } else {
                CodeGraph::init(&root)?
            };
            let report = cg.index_all(Some(&progress_printer(quiet)))?;
            if !quiet {
                print_report(&report, "Indexed");
            }
            record_index(&cg, &report);
        }
        Command::Sync { path, quiet } => {
            let mut cg = open(path.as_deref())?;
            let report = cg.sync(Some(&progress_printer(quiet)))?;
            if !quiet {
                if report.changed_anything() {
                    print_report(&report, "Synced");
                } else {
                    println!("Already up to date.");
                }
            }
        }
        Command::Status { path, json } => {
            let cg = open(path.as_deref())?;
            let s = cg.status()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                print!("{}", format::status(&s));
                if let Some(n) = codegraph::upgrade::cached_notice() {
                    println!("\n{n}");
                }
            }
        }
        Command::Query {
            search,
            path,
            limit,
            kind,
            json,
        } => {
            let cg = open(path.path.as_deref())?;
            let kinds = match kind {
                Some(k) => vec![k.parse().map_err(|e| anyhow::anyhow!("{e}"))?],
                None => vec![],
            };
            let results = cg.store().search_nodes(
                &search,
                &SearchOptions {
                    kinds,
                    limit: Some(limit),
                    ..Default::default()
                },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&results)?);
            } else {
                println!("{}", format::search_results(&results));
            }
        }
        Command::Explore {
            query,
            path,
            max_files,
        } => {
            let cg = open(path.path.as_deref())?;
            print!("{}", cg.explore(&query.join(" "), max_files)?);
        }
        Command::Context {
            task,
            path,
            format,
            max_nodes,
            no_code,
        } => {
            let cg = open(path.path.as_deref())?;
            let mut opts = ContextOptions {
                include_code: !no_code,
                ..Default::default()
            };
            opts.format = match format.as_str() {
                "json" => ContextFormat::Json,
                "markdown" | "md" => ContextFormat::Markdown,
                other => bail!("unknown format {other} (markdown, json)"),
            };
            if let Some(n) = max_nodes {
                opts.max_nodes = n;
            }
            print!("{}", cg.context(&task.join(" "), &opts)?);
        }
        Command::Node {
            name,
            path,
            file,
            offset,
            limit,
            symbols_only,
            line,
            code,
        } => {
            let cg = open(path.path.as_deref())?;
            let mut args = serde_json::json!({ "includeCode": code, "symbolsOnly": symbols_only });
            if let Some(n) = name {
                args["symbol"] = n.into();
            }
            if let Some(f) = file {
                args["file"] = f.into();
            }
            for (k, v) in [("offset", offset), ("limit", limit), ("line", line)] {
                if let Some(v) = v {
                    args[k] = v.into();
                }
            }
            match codegraph::mcp::run_tool(&cg, "codegraph_node", &args) {
                Ok(text) => print!("{text}"),
                Err(e) => bail!(e),
            }
        }
        Command::Files {
            path,
            filter,
            pattern,
            format,
            max_depth,
            no_metadata,
            json,
        } => {
            let cg = open(path.path.as_deref())?;
            let files = cg.store().all_files()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&files)?);
            } else {
                let f: FilesFormat = format.parse().map_err(|e: String| anyhow::anyhow!(e))?;
                print!(
                    "{}",
                    format::files(
                        &files,
                        filter.as_deref(),
                        pattern.as_deref(),
                        f,
                        !no_metadata,
                        max_depth
                    )
                );
            }
        }
        Command::Callers(a) => neighbors(a, true)?,
        Command::Callees(a) => neighbors(a, false)?,
        Command::Impact {
            symbol,
            path,
            file,
            depth,
            json,
        } => {
            let cg = open(path.path.as_deref())?;
            let (defs, _) = cg.find_symbols(&symbol, file.as_deref())?;
            if defs.is_empty() {
                bail!("no symbol named `{symbol}` in the index");
            }
            let mut out = Vec::new();
            for d in defs {
                let imp = cg.graph().impact(&d.id, depth, 500)?;
                out.push((d, imp));
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&out.iter().map(|(_, i)| i).collect::<Vec<_>>())?
                );
            } else {
                print!("{}", format::impact(&out));
            }
        }
        Command::Affected {
            mut files,
            path,
            stdin,
            depth,
            filter,
            json,
            quiet,
        } => {
            let cg = open(path.path.as_deref())?;
            if stdin {
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                files.extend(
                    buf.lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(str::to_string),
                );
            }
            let root = cg.root().to_path_buf();
            let files: Vec<String> = files
                .into_iter()
                .map(|f| {
                    let p = Path::new(&f);
                    let abs = if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        cwd().join(p)
                    };
                    abs.strip_prefix(&root)
                        .map(|r| r.to_string_lossy().replace('\\', "/"))
                        .unwrap_or(f)
                })
                .collect();
            let matcher = match &filter {
                Some(g) => Some(globset::Glob::new(g)?.compile_matcher()),
                None => None,
            };
            let is_target = |p: &str| match &matcher {
                Some(m) => m.is_match(p),
                None => is_test_path(p),
            };
            let hits = cg.affected(&files, depth, &is_target)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&hits)?);
            } else if quiet {
                for h in &hits {
                    println!("{h}");
                }
            } else if hits.is_empty() {
                println!("No affected test files.");
            } else {
                println!("{} affected file(s):", hits.len());
                for h in &hits {
                    println!("  {h}");
                }
            }
        }
        Command::DeadCode { path, limit } => {
            let cg = open(path.path.as_deref())?;
            let dead = cg.graph().dead_code(limit)?;
            if dead.is_empty() {
                println!("No unreferenced symbols found.");
            }
            for n in dead {
                println!("{}", format::node_line(&n));
            }
        }
        Command::Watch { path } => {
            let cg = open(path.as_deref())?;
            let root = cg.root().to_path_buf();
            let cg = std::sync::Arc::new(std::sync::Mutex::new(cg));
            let _w = codegraph::watch::watch(cg.clone(), |r| {
                if r.changed_anything() {
                    print_report(r, "Synced");
                }
            })?;
            println!("Watching {} for changes (Ctrl-C to stop)…", root.display());
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
        Command::Serve {
            path,
            mcp: _,
            no_watch,
            no_daemon,
        } => {
            let root = path.path.unwrap_or_else(cwd);
            codegraph::upgrade::check_in_background();
            codegraph::mcp::serve_stdio(Some(&root), !no_watch, !no_daemon)?;
        }
        Command::Daemon { action, path, all } => {
            use codegraph::daemon;
            match action.as_deref().unwrap_or("list") {
                "list" | "status" => {
                    let running = daemon::list();
                    if running.is_empty() {
                        println!("No CodeGraph daemons running.");
                    }
                    let now = codegraph::global::now_ms();
                    for d in running {
                        let up = now.saturating_sub(d.started_at) / 1000;
                        println!(
                            "pid {:<7} v{:<8} up {:>3}m{:02}s  {}",
                            d.pid,
                            d.version,
                            up / 60,
                            up % 60,
                            d.root
                        );
                    }
                }
                "stop" => {
                    let roots: Vec<PathBuf> = if all {
                        daemon::list()
                            .into_iter()
                            .map(|d| PathBuf::from(d.root))
                            .collect()
                    } else {
                        let start = path.unwrap_or_else(cwd);
                        vec![codegraph::find_root(&start).with_context(|| {
                            format!("no CodeGraph index at or above {}", start.display())
                        })?]
                    };
                    for root in roots {
                        if daemon::stop(&root) {
                            println!("Stopped the daemon for {}", root.display());
                        } else {
                            println!("No daemon running for {}", root.display());
                        }
                    }
                }
                // Internal: what `serve --mcp` starts in the background.
                "run" => {
                    let root = path.context("daemon run needs a project path")?;
                    daemon::run(&root)?;
                }
                other => bail!("unknown daemon action `{other}` (use list or stop)"),
            }
        }
        Command::Install {
            target,
            location,
            yes,
            init,
            no_permissions,
            prompt_hook,
            print_config,
        } => {
            let env = Env::current();
            if let Some(id) = print_config {
                let t = installer::target(&id).with_context(|| format!("unknown agent `{id}`"))?;
                let loc = match location.as_deref() {
                    Some(l) => l.parse().map_err(|e: String| anyhow::anyhow!(e))?,
                    None => Location::Global,
                };
                print!("{}", t.print_config(&env, loc));
                return Ok(());
            }
            let loc = resolve_location(location.as_deref(), yes)?;
            let targets = select_targets(target.as_deref(), loc, &env, yes, true)?;
            if targets.is_empty() {
                println!("No agents selected. Pass --target (e.g. --target claude,cursor) or --target all.");
            }
            let mut outcomes = Vec::new();
            for t in &targets {
                let opts = installer::InstallOptions {
                    auto_allow: !no_permissions,
                    prompt_hook,
                };
                let o = t.install(&env, loc, opts)?;
                print_outcome(t.display_name, &o);
                outcomes.push(o);
            }
            if !targets.is_empty() {
                println!("Restart your agent(s) to load CodeGraph. Index a project with `codegraph init`.");
            }
            if !targets.is_empty() {
                codegraph::telemetry::record_event(
                    "install",
                    serde_json::json!({
                        "targets": targets.iter().map(|t| t.id).collect::<Vec<_>>(),
                        "scope": loc_name(loc),
                        "kind": install_kind(&outcomes),
                    }),
                );
            }
            if init {
                let mut cg = CodeGraph::init(&cwd())?;
                let report = cg.index_all(Some(&progress_printer(false)))?;
                print_report(&report, "Indexed");
                record_index(&cg, &report);
            }
        }
        Command::Uninstall {
            target,
            location,
            yes,
        } => {
            let env = Env::current();
            let loc = resolve_location(location.as_deref(), yes)?;
            let targets = select_targets(target.as_deref(), loc, &env, yes, false)?;
            if targets.is_empty() {
                println!("CodeGraph isn't configured for any agent at that location.");
            }
            for t in &targets {
                let o = t.uninstall(&env, loc)?;
                print_outcome(t.display_name, &o);
            }
            if !targets.is_empty() {
                codegraph::telemetry::record_event(
                    "uninstall",
                    serde_json::json!({ "targets": targets.iter().map(|t| t.id).collect::<Vec<_>>() }),
                );
            }
        }
        Command::PromptHook => {
            if std::env::var("CODEGRAPH_NO_PROMPT_HOOK").as_deref() == Ok("1")
                || std::io::stdin().is_terminal()
            {
                return Ok(());
            }
            let mut raw = String::new();
            std::io::stdin().read_to_string(&mut raw)?;
            // A hook must never break the user's prompt: any failure is silent.
            let Ok(input) = serde_json::from_str::<serde_json::Value>(&raw) else {
                return Ok(());
            };
            let prompt = input.get("prompt").and_then(|p| p.as_str()).unwrap_or("");
            let cwd = input
                .get("cwd")
                .and_then(|c| c.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(cwd);
            if let Some(text) = codegraph::prompt::run(prompt, &cwd) {
                print!("{text}");
            }
        }
        Command::Ui {
            path,
            port,
            no_open,
        } => {
            let cg = open(path.path.as_deref())?;
            if !codegraph::ui::viewer_built() {
                eprintln!("This build has no browser viewer (the wasm32-unknown-unknown target was missing when it was built).");
            }
            // Counted now: the server runs until interrupted.
            codegraph::telemetry::record_usage(
                codegraph::telemetry::UsageKind::CliCommand,
                "ui",
                false,
                None,
            );
            codegraph::telemetry::flush();
            codegraph::ui::serve(cg, port, |url| {
                println!("CodeGraph viewer: {url}  (Ctrl-C to stop)");
                if !no_open && !codegraph::ui::open_browser(url) {
                    println!("Open that address in your browser.");
                }
            })?;
        }
        Command::Upgrade {
            check,
            version,
            force,
        } => {
            let code = codegraph::upgrade::run(
                &codegraph::upgrade::UpgradeOptions {
                    check,
                    version,
                    force,
                },
                &mut |line| println!("{line}"),
            )?;
            if code != 0 {
                std::process::exit(code);
            }
        }
        Command::Telemetry { action } => {
            use codegraph::telemetry;
            let st = match action.as_deref().unwrap_or("status") {
                "on" => telemetry::set_enabled(true, "cli"),
                "off" => telemetry::set_enabled(false, "cli"),
                "status" => telemetry::status(),
                other => bail!("unknown telemetry action `{other}` (use on, off or status)"),
            };
            println!(
                "Telemetry is {} ({}).",
                if st.enabled { "on" } else { "off" },
                st.decided_by.describe()
            );
            match (&st.endpoint, st.enabled) {
                (Some(url), true) => println!("Endpoint: {url}"),
                (None, true) => println!("No endpoint is configured, so nothing is sent (set CODEGRAPH_TELEMETRY_ENDPOINT)."),
                _ => {}
            }
            if let Some(id) = &st.machine_id {
                println!("Machine ID: {id}");
            }
            println!("Stored in {}", st.config_path.display());
            println!("What is collected: {}", telemetry::DOCS);
        }
        Command::Version => println!("codegraph {}", env!("CARGO_PKG_VERSION")),
        Command::Extract {
            file_path,
            language,
        } => {
            use std::io::Read;
            let mut content = String::new();
            std::io::stdin().read_to_string(&mut content)?;
            let language = match language {
                Some(l) => l.parse().map_err(|e| anyhow::anyhow!("{e}"))?,
                None => codegraph_core::detect_language(&file_path, Some(&content)),
            };
            let result = codegraph_extract::extract_file(&file_path, &content, language);
            println!("{}", serde_json::to_string(&result)?);
        }
        Command::Ccpp { pass, file, option } => {
            use std::io::{Read, Write};
            let mut content = String::new();
            std::io::stdin().read_to_string(&mut content)?;
            let out = codegraph_extract::ccpp_preparse_pass(&pass, &content, &file, option)
                .ok_or_else(|| anyhow::anyhow!("no C/C++ pass named `{pass}`"))?;
            std::io::stdout().write_all(out.as_bytes())?;
        }
    }
    Ok(())
}

fn main() {
    // Exit quietly when stdout is closed early (`codegraph files | head`).
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    let name = cli.command.name();
    // The MCP server counts its own tool calls; `telemetry` itself is never counted.
    let counted = !matches!(name, "serve" | "telemetry" | "ui" | "daemon");
    let result = run(cli);
    if counted {
        codegraph::telemetry::record_usage(
            codegraph::telemetry::UsageKind::CliCommand,
            name,
            result.is_err(),
            None,
        );
        codegraph::telemetry::flush();
    }
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn record_index(cg: &CodeGraph, report: &codegraph::IndexReport) {
    if let Ok(s) = cg.status() {
        codegraph::telemetry::record_index(
            &s.stats.files_by_language,
            report.files_scanned,
            report.duration_ms,
        );
    }
}

fn loc_name(loc: Location) -> &'static str {
    match loc {
        Location::Global => "global",
        Location::Local => "local",
    }
}

/// fresh (nothing was configured), upgrade (an entry changed) or reinstall.
fn install_kind(outcomes: &[installer::Outcome]) -> &'static str {
    use installer::Action;
    let actions: Vec<Action> = outcomes
        .iter()
        .flat_map(|o| o.files.iter().map(|f| f.action))
        .collect();
    if actions.contains(&Action::Updated) {
        "upgrade"
    } else if actions.contains(&Action::Created) {
        "fresh"
    } else {
        "reinstall"
    }
}
