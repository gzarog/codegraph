//! `codegraph install` / `uninstall`: register the MCP server with coding
//! agents, add a short instructions block where the agent reads one, and (for
//! Claude Code) pre-allow the codegraph tools.
//!
//! Every edit is surgical: only CodeGraph's own entry changes, everything
//! else in the user's config stays as written, and re-running is a no-op.

pub mod jsonc;
pub mod toml;
pub mod yaml;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Global,
    Local,
}

impl std::str::FromStr for Location {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "global" => Ok(Location::Global),
            "local" => Ok(Location::Local),
            other => Err(format!("unknown location {other} (global, local)")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Created,
    Updated,
    Unchanged,
    Removed,
    NotFound,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Created => "created",
            Action::Updated => "updated",
            Action::Unchanged => "unchanged",
            Action::Removed => "removed",
            Action::NotFound => "not found",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: PathBuf,
    pub action: Action,
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub files: Vec<FileChange>,
    pub notes: Vec<String>,
}

/// Where home and the current project are (injectable for tests).
#[derive(Debug, Clone)]
pub struct Env {
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub vars: std::collections::HashMap<String, String>,
}

impl Env {
    pub fn current() -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let vars = [
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "XDG_CONFIG_HOME",
            "APPDATA",
            "LOCALAPPDATA",
            "HERMES_HOME",
        ]
        .iter()
        .filter_map(|k| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .map(|v| (k.to_string(), v))
        })
        .collect();
        Env { home, cwd, vars }
    }

    fn var(&self, k: &str) -> Option<PathBuf> {
        self.vars.get(k).map(PathBuf::from)
    }

    fn xdg_config(&self) -> PathBuf {
        self.var("XDG_CONFIG_HOME")
            .unwrap_or_else(|| self.home.join(".config"))
    }
}

pub const SECTION_START: &str = "<!-- CODEGRAPH_START -->";
pub const SECTION_END: &str = "<!-- CODEGRAPH_END -->";

pub fn instructions_block() -> String {
    format!(
        "{SECTION_START}
## CodeGraph

In repositories indexed by CodeGraph (a `.codegraph/` directory exists at the repo root), reach for it BEFORE grep/find or reading files when you need to understand or locate code:

- **MCP tool** (when available): `codegraph_explore` answers most code questions in one call — the relevant symbols' verbatim source plus the call paths between them. Name a file or symbol in the query to read its current line-numbered source. If it's listed but deferred, load it by name via tool search.
- **Shell** (always works): `codegraph explore \"<symbol names or question>\"` prints the same output.

If there is no `.codegraph/` directory, skip CodeGraph entirely — indexing is the user's decision.
{SECTION_END}"
    )
}

/// The base stdio server entry.
pub fn mcp_server_config() -> Value {
    json!({ "type": "stdio", "command": "codegraph", "args": ["serve", "--mcp"] })
}

/// How a target stores its server entry.
#[derive(Debug, Clone)]
enum Store {
    /// A JSON(C) file; the entry lives at `key_path`.
    Json { key_path: &'static [&'static str] },
    /// Codex `config.toml` table.
    Toml,
    /// Hermes `config.yaml`.
    Yaml,
}

/// One supported agent.
pub struct Target {
    pub id: &'static str,
    pub display_name: &'static str,
    pub docs_url: &'static str,
    locations: &'static [Location],
    store: Store,
    config: fn(&Env, Location) -> PathBuf,
    instructions: Option<fn(&Env, Location) -> PathBuf>,
    detect_dir: fn(&Env, Location) -> PathBuf,
    entry: fn(&Env, Location) -> Value,
}

fn claude_dir(env: &Env, loc: Location) -> PathBuf {
    match loc {
        Location::Global => env
            .var("CLAUDE_CONFIG_DIR")
            .unwrap_or_else(|| env.home.join(".claude")),
        Location::Local => env.cwd.join(".claude"),
    }
}

fn claude_mcp(env: &Env, loc: Location) -> PathBuf {
    match loc {
        Location::Global => match env.var("CLAUDE_CONFIG_DIR") {
            Some(d) => d.join(".claude.json"),
            None => env.home.join(".claude.json"),
        },
        Location::Local => env.cwd.join(".mcp.json"),
    }
}

fn codex_dir(env: &Env, loc: Location) -> PathBuf {
    match loc {
        Location::Global => env
            .var("CODEX_HOME")
            .unwrap_or_else(|| env.home.join(".codex")),
        Location::Local => env.cwd.join(".codex"),
    }
}

fn opencode_dir(env: &Env, loc: Location) -> PathBuf {
    match loc {
        Location::Global => env.xdg_config().join("opencode"),
        Location::Local => env.cwd.clone(),
    }
}

fn opencode_config(env: &Env, loc: Location) -> PathBuf {
    let dir = opencode_dir(env, loc);
    let json = dir.join("opencode.json");
    if json.exists() && !dir.join("opencode.jsonc").exists() {
        json
    } else {
        dir.join("opencode.jsonc")
    }
}

fn vscode_user_dir(env: &Env) -> PathBuf {
    if cfg!(target_os = "macos") {
        env.home.join("Library/Application Support/Code/User")
    } else if cfg!(windows) {
        env.var("APPDATA")
            .unwrap_or_else(|| env.home.join("AppData/Roaming"))
            .join("Code/User")
    } else {
        env.xdg_config().join("Code/User")
    }
}

fn copilot_jetbrains_dir(env: &Env) -> PathBuf {
    if cfg!(windows) {
        env.var("LOCALAPPDATA")
            .unwrap_or_else(|| env.home.join("AppData/Local"))
            .join("github-copilot/intellij")
    } else {
        env.xdg_config().join("github-copilot/intellij")
    }
}

fn hermes_home(env: &Env) -> PathBuf {
    env.var("HERMES_HOME")
        .unwrap_or_else(|| env.home.join(".hermes"))
}

/// Cursor launches MCP servers with the wrong cwd and no rootUri, so the
/// project path is passed explicitly.
fn with_path_arg(env: &Env, loc: Location) -> Value {
    let mut v = mcp_server_config();
    let path = match loc {
        Location::Local => env.cwd.display().to_string(),
        Location::Global => "${workspaceFolder}".to_string(),
    };
    v["args"] = json!(["serve", "--mcp", "--path", path]);
    v
}

const BOTH: &[Location] = &[Location::Global, Location::Local];
const GLOBAL_ONLY: &[Location] = &[Location::Global];

pub fn targets() -> Vec<Target> {
    vec![
        Target {
            id: "claude",
            display_name: "Claude Code",
            docs_url: "https://docs.claude.com/en/docs/claude-code",
            locations: BOTH,
            store: Store::Json { key_path: &["mcpServers", "codegraph"] },
            config: claude_mcp,
            instructions: Some(|e, l| claude_dir(e, l).join("CLAUDE.md")),
            detect_dir: claude_dir,
            entry: |_, _| {
                let mut v = mcp_server_config();
                v["alwaysLoad"] = json!(true);
                v
            },
        },
        Target {
            id: "cursor",
            display_name: "Cursor",
            docs_url: "https://docs.cursor.com/context/model-context-protocol",
            locations: BOTH,
            store: Store::Json { key_path: &["mcpServers", "codegraph"] },
            config: |e, l| match l {
                Location::Global => e.home.join(".cursor/mcp.json"),
                Location::Local => e.cwd.join(".cursor/mcp.json"),
            },
            instructions: None,
            detect_dir: |e, l| match l {
                Location::Global => e.home.join(".cursor"),
                Location::Local => e.cwd.join(".cursor"),
            },
            entry: with_path_arg,
        },
        Target {
            id: "codex",
            display_name: "Codex CLI",
            docs_url: "https://github.com/openai/codex",
            locations: BOTH,
            store: Store::Toml,
            config: |e, l| codex_dir(e, l).join("config.toml"),
            instructions: Some(|e, l| match l {
                Location::Global => codex_dir(e, l).join("AGENTS.md"),
                Location::Local => e.cwd.join("AGENTS.md"),
            }),
            detect_dir: codex_dir,
            entry: |_, _| mcp_server_config(),
        },
        Target {
            id: "opencode",
            display_name: "opencode",
            docs_url: "https://opencode.ai/docs/mcp-servers",
            locations: BOTH,
            store: Store::Json { key_path: &["mcp", "servers", "codegraph"] },
            config: opencode_config,
            instructions: Some(|e, l| opencode_dir(e, l).join("AGENTS.md")),
            detect_dir: opencode_dir,
            entry: |_, _| json!({ "type": "local", "command": ["codegraph", "serve", "--mcp"], "disabled": false, "codemode": false }),
        },
        Target {
            id: "gemini",
            display_name: "Gemini CLI",
            docs_url: "https://github.com/google-gemini/gemini-cli",
            locations: BOTH,
            store: Store::Json { key_path: &["mcpServers", "codegraph"] },
            config: |e, l| match l {
                Location::Global => e.home.join(".gemini/settings.json"),
                Location::Local => e.cwd.join(".gemini/settings.json"),
            },
            instructions: Some(|e, l| match l {
                Location::Global => e.home.join(".gemini/GEMINI.md"),
                Location::Local => e.cwd.join("GEMINI.md"),
            }),
            detect_dir: |e, l| match l {
                Location::Global => e.home.join(".gemini"),
                Location::Local => e.cwd.join(".gemini"),
            },
            entry: |_, _| mcp_server_config(),
        },
        Target {
            id: "kiro",
            display_name: "Kiro",
            docs_url: "https://kiro.dev/docs/mcp",
            locations: BOTH,
            store: Store::Json { key_path: &["mcpServers", "codegraph"] },
            config: |e, l| match l {
                Location::Global => e.home.join(".kiro/settings/mcp.json"),
                Location::Local => e.cwd.join(".kiro/settings/mcp.json"),
            },
            instructions: None,
            detect_dir: |e, l| match l {
                Location::Global => e.home.join(".kiro"),
                Location::Local => e.cwd.join(".kiro"),
            },
            entry: |_, _| mcp_server_config(),
        },
        Target {
            id: "antigravity",
            display_name: "Antigravity",
            docs_url: "https://antigravity.google",
            locations: GLOBAL_ONLY,
            store: Store::Json { key_path: &["mcpServers", "codegraph"] },
            config: |e, _| e.home.join(".gemini/config/mcp_config.json"),
            instructions: None,
            detect_dir: |e, _| e.home.join(".gemini"),
            entry: |_, _| json!({ "command": "codegraph", "args": ["serve", "--mcp"] }),
        },
        Target {
            id: "hermes",
            display_name: "Hermes Agent",
            docs_url: "https://github.com/NousResearch/hermes-agent",
            locations: GLOBAL_ONLY,
            store: Store::Yaml,
            config: |e, _| hermes_home(e).join("config.yaml"),
            instructions: None,
            detect_dir: |e, _| hermes_home(e),
            entry: |_, _| mcp_server_config(),
        },
        Target {
            id: "copilot-vscode",
            display_name: "GitHub Copilot (VS Code)",
            docs_url: "https://code.visualstudio.com/docs/copilot/customization/mcp-servers",
            locations: BOTH,
            store: Store::Json { key_path: &["servers", "codegraph"] },
            config: |e, l| match l {
                Location::Global => vscode_user_dir(e).join("mcp.json"),
                Location::Local => e.cwd.join(".vscode/mcp.json"),
            },
            instructions: None,
            detect_dir: |e, l| match l {
                Location::Global => vscode_user_dir(e),
                Location::Local => e.cwd.join(".vscode"),
            },
            entry: |e, l| match l {
                Location::Local => {
                    let mut v = mcp_server_config();
                    v["args"] = json!(["serve", "--mcp", "--path", e.cwd.display().to_string()]);
                    v
                }
                Location::Global => mcp_server_config(),
            },
        },
        Target {
            id: "copilot-cli",
            display_name: "GitHub Copilot CLI",
            docs_url: "https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers",
            locations: GLOBAL_ONLY,
            store: Store::Json { key_path: &["mcpServers", "codegraph"] },
            config: |e, _| e.home.join(".copilot/mcp-config.json"),
            instructions: None,
            detect_dir: |e, _| e.home.join(".copilot"),
            entry: |_, _| {
                let mut v = mcp_server_config();
                v["tools"] = json!(["*"]);
                v["deferTools"] = json!("never");
                v
            },
        },
        Target {
            id: "copilot-jetbrains",
            display_name: "GitHub Copilot (JetBrains)",
            docs_url: "https://docs.github.com/en/copilot",
            locations: GLOBAL_ONLY,
            store: Store::Json { key_path: &["servers", "codegraph"] },
            config: |e, _| copilot_jetbrains_dir(e).join("mcp.json"),
            instructions: None,
            detect_dir: |e, _| copilot_jetbrains_dir(e),
            entry: |_, _| mcp_server_config(),
        },
    ]
}

pub fn target(id: &str) -> Option<Target> {
    targets().into_iter().find(|t| t.id == id)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Write via a temp file + rename so a crash never leaves half a config.
fn write(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!(
        "{}.codegraph-tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Insert or replace the marked instructions block.
pub fn upsert_instructions(path: &Path) -> std::io::Result<FileChange> {
    let existing = read(path);
    let block = instructions_block();
    let new = match (existing.find(SECTION_START), existing.find(SECTION_END)) {
        (Some(s), Some(e)) if e > s => format!(
            "{}{}{}",
            &existing[..s],
            block,
            &existing[e + SECTION_END.len()..]
        ),
        _ if existing.trim().is_empty() => format!("{block}\n"),
        _ => format!("{}\n\n{block}\n", existing.trim_end()),
    };
    let action = if new == existing {
        Action::Unchanged
    } else if path.exists() {
        Action::Updated
    } else {
        Action::Created
    };
    if action != Action::Unchanged {
        write(path, &new)?;
    }
    Ok(FileChange {
        path: path.to_path_buf(),
        action,
    })
}

/// Remove the marked instructions block (and the file, if nothing else remains).
pub fn remove_instructions(path: &Path) -> std::io::Result<FileChange> {
    let existing = read(path);
    let (Some(s), Some(e)) = (existing.find(SECTION_START), existing.find(SECTION_END)) else {
        return Ok(FileChange {
            path: path.to_path_buf(),
            action: Action::NotFound,
        });
    };
    let rest = format!(
        "{}{}",
        existing[..s].trim_end(),
        &existing[e + SECTION_END.len()..]
    );
    if rest.trim().is_empty() {
        std::fs::remove_file(path)?;
    } else {
        write(path, &(rest.trim_end().to_string() + "\n"))?;
    }
    Ok(FileChange {
        path: path.to_path_buf(),
        action: Action::Removed,
    })
}

/// Remove `path` from a JSON(C) text, then any ancestors it left empty.
/// Returns `Some("")` when nothing but an empty object would remain.
fn remove_and_prune(text: &str, path: &[&str]) -> Option<String> {
    let mut out = jsonc::remove(text, path)?;
    for depth in (1..path.len()).rev() {
        let parent = &path[..depth];
        let empty = jsonc::parse(&out)
            .and_then(|v| parent.iter().try_fold(v, |cur, k| cur.get(*k).cloned()))
            .is_some_and(|v| v.as_object().is_some_and(|m| m.is_empty()));
        if !empty {
            break;
        }
        out = jsonc::remove(&out, parent).unwrap_or(out);
    }
    if jsonc::parse(&out).is_some_and(|v| v.as_object().is_some_and(|m| m.is_empty()))
        && !out.contains("//")
        && !out.contains("/*")
    {
        return Some(String::new());
    }
    Some(out)
}

/// Write `text`, or delete the file when `text` is empty.
fn write_or_delete(path: &Path, text: &str) -> std::io::Result<()> {
    if text.is_empty() {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        write(path, text)
    }
}

const PERMISSION: &str = "mcp__codegraph__*";

fn claude_settings(env: &Env, loc: Location) -> PathBuf {
    claude_dir(env, loc).join("settings.json")
}

fn add_permission(path: &Path) -> std::io::Result<FileChange> {
    let text = read(path);
    let v = jsonc::parse(&text).unwrap_or(json!({}));
    let mut allow: Vec<Value> = v["permissions"]["allow"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if allow.iter().any(|a| a == PERMISSION) {
        return Ok(FileChange {
            path: path.to_path_buf(),
            action: Action::Unchanged,
        });
    }
    allow.push(json!(PERMISSION));
    let new =
        jsonc::set(&text, &["permissions", "allow"], &Value::Array(allow)).unwrap_or(text.clone());
    let action = if path.exists() {
        Action::Updated
    } else {
        Action::Created
    };
    write(path, &new)?;
    Ok(FileChange {
        path: path.to_path_buf(),
        action,
    })
}

fn remove_permission(path: &Path) -> std::io::Result<FileChange> {
    let text = read(path);
    let Some(v) = jsonc::parse(&text) else {
        return Ok(FileChange {
            path: path.to_path_buf(),
            action: Action::NotFound,
        });
    };
    let Some(allow) = v["permissions"]["allow"].as_array() else {
        return Ok(FileChange {
            path: path.to_path_buf(),
            action: Action::NotFound,
        });
    };
    let kept: Vec<Value> = allow
        .iter()
        .filter(|a| {
            !a.as_str()
                .is_some_and(|s| s.starts_with("mcp__codegraph__"))
        })
        .cloned()
        .collect();
    if kept.len() == allow.len() {
        return Ok(FileChange {
            path: path.to_path_buf(),
            action: Action::NotFound,
        });
    }
    let new = if kept.is_empty() {
        remove_and_prune(&text, &["permissions", "allow"]).unwrap_or(text.clone())
    } else {
        jsonc::set(&text, &["permissions", "allow"], &Value::Array(kept)).unwrap_or(text.clone())
    };
    write_or_delete(path, &new)?;
    Ok(FileChange {
        path: path.to_path_buf(),
        action: Action::Removed,
    })
}

impl Target {
    pub fn supports(&self, loc: Location) -> bool {
        self.locations.contains(&loc)
    }

    pub fn config_path(&self, env: &Env, loc: Location) -> PathBuf {
        (self.config)(env, loc)
    }

    /// The agent appears to be installed (its config directory exists).
    pub fn detected(&self, env: &Env, loc: Location) -> bool {
        (self.detect_dir)(env, loc).exists() || self.config_path(env, loc).exists()
    }

    pub fn is_configured(&self, env: &Env, loc: Location) -> bool {
        let text = read(&self.config_path(env, loc));
        match &self.store {
            Store::Json { key_path } => {
                let v = jsonc::parse(&text).unwrap_or(Value::Null);
                key_path.iter().try_fold(&v, |cur, k| cur.get(*k)).is_some()
            }
            Store::Toml => text.lines().any(|l| l.trim() == "[mcp_servers.codegraph]"),
            Store::Yaml => text
                .lines()
                .any(|l| l.trim_start().starts_with("codegraph:")),
        }
    }

    /// What `install` would add, for `--print-config`.
    pub fn print_config(&self, env: &Env, loc: Location) -> String {
        let path = self.config_path(env, loc);
        let body = match &self.store {
            Store::Json { key_path } => {
                let mut v = (self.entry)(env, loc);
                for k in key_path.iter().rev() {
                    v = json!({ *k: v });
                }
                serde_json::to_string_pretty(&v).unwrap_or_default()
            }
            Store::Toml => {
                toml::render_table("mcp_servers.codegraph", "codegraph", &["serve", "--mcp"])
            }
            Store::Yaml => format!("mcp_servers:\n{}", yaml::server_block(2).join("\n")),
        };
        format!("# Add to {}\n\n{body}\n", path.display())
    }

    pub fn install(&self, env: &Env, loc: Location, auto_allow: bool) -> std::io::Result<Outcome> {
        let mut out = Outcome::default();
        let path = self.config_path(env, loc);
        let existing = read(&path);
        let new = match &self.store {
            Store::Json { key_path } => {
                let entry = (self.entry)(env, loc);
                let current = jsonc::parse(&existing)
                    .and_then(|v| key_path.iter().try_fold(v, |cur, k| cur.get(*k).cloned()));
                if current.as_ref() == Some(&entry) {
                    existing.clone()
                } else {
                    match jsonc::set(&existing, key_path, &entry) {
                        Some(t) => t,
                        None => {
                            out.notes.push(format!("{} isn't a JSON object CodeGraph can edit; add the entry yourself:\n{}", path.display(), self.print_config(env, loc)));
                            existing.clone()
                        }
                    }
                }
            }
            Store::Toml => toml::upsert_table(
                &existing,
                "mcp_servers.codegraph",
                &toml::render_table("mcp_servers.codegraph", "codegraph", &["serve", "--mcp"]),
            ),
            Store::Yaml => match yaml::upsert(&existing) {
                Some(t) => t,
                None => {
                    out.notes.push(format!("{} writes mcp_servers as a one-line mapping; add `codegraph: {{command: codegraph, args: [serve, --mcp]}}` to it yourself.", path.display()));
                    existing.clone()
                }
            },
        };
        let action = if new == existing {
            Action::Unchanged
        } else if path.exists() {
            Action::Updated
        } else {
            Action::Created
        };
        if action != Action::Unchanged {
            write(&path, &new)?;
        }
        out.files.push(FileChange { path, action });
        if self.id == "claude" && auto_allow {
            out.files.push(add_permission(&claude_settings(env, loc))?);
        }
        if let Some(instr) = self.instructions {
            out.files.push(upsert_instructions(&instr(env, loc))?);
        }
        if self.id == "codex" && loc == Location::Local {
            out.notes.push("Codex only applies a project's .codex/config.toml in a trusted project; trust this project in Codex to activate it.".into());
        }
        Ok(out)
    }

    pub fn uninstall(&self, env: &Env, loc: Location) -> std::io::Result<Outcome> {
        let mut out = Outcome::default();
        let path = self.config_path(env, loc);
        let existing = read(&path);
        let new = match &self.store {
            Store::Json { key_path } => remove_and_prune(&existing, key_path),
            Store::Toml => toml::remove_table(&existing, "mcp_servers.codegraph"),
            Store::Yaml => yaml::remove(&existing),
        };
        match new {
            Some(t) => {
                write_or_delete(&path, &t)?;
                out.files.push(FileChange {
                    path,
                    action: Action::Removed,
                });
            }
            None => out.files.push(FileChange {
                path,
                action: Action::NotFound,
            }),
        }
        if self.id == "claude" {
            out.files
                .push(remove_permission(&claude_settings(env, loc))?);
        }
        if let Some(instr) = self.instructions {
            out.files.push(remove_instructions(&instr(env, loc))?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> (tempfile::TempDir, Env) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let cwd = dir.path().join("proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        (
            dir,
            Env {
                home,
                cwd,
                vars: Default::default(),
            },
        )
    }

    #[test]
    fn every_target_round_trips() {
        let (_d, env) = env();
        for t in targets() {
            for &loc in t.locations {
                let first = t.install(&env, loc, true).unwrap();
                assert_ne!(first.files[0].action, Action::Unchanged, "{} {loc:?}", t.id);
                assert!(t.is_configured(&env, loc), "{} {loc:?}", t.id);
                // Re-running changes nothing.
                let again = t.install(&env, loc, true).unwrap();
                assert!(
                    again.files.iter().all(|f| f.action == Action::Unchanged),
                    "{} {loc:?}: {:?}",
                    t.id,
                    again.files
                );
                t.uninstall(&env, loc).unwrap();
                assert!(
                    !t.is_configured(&env, loc),
                    "{} {loc:?} still configured",
                    t.id
                );
            }
        }
    }

    #[test]
    fn preserves_existing_config() {
        let (_d, env) = env();
        let claude = target("claude").unwrap();
        let mcp = claude.config_path(&env, Location::Local);
        std::fs::write(
            &mcp,
            "{\n  \"mcpServers\": {\n    \"other\": { \"command\": \"x\" }\n  }\n}\n",
        )
        .unwrap();
        claude.install(&env, Location::Local, true).unwrap();
        let v = jsonc::parse(&std::fs::read_to_string(&mcp).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["mcpServers"]["codegraph"]["command"], "codegraph");
        let settings = std::fs::read_to_string(env.cwd.join(".claude/settings.json")).unwrap();
        assert!(settings.contains(PERMISSION));
        let md = std::fs::read_to_string(env.cwd.join(".claude/CLAUDE.md")).unwrap();
        assert!(md.contains(SECTION_START));
        claude.uninstall(&env, Location::Local).unwrap();
        let v = jsonc::parse(&std::fs::read_to_string(&mcp).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert!(v["mcpServers"].get("codegraph").is_none());
        assert!(
            !env.cwd.join(".claude/CLAUDE.md").exists(),
            "instructions-only file removed"
        );
        assert!(
            !env.cwd.join(".claude/settings.json").exists(),
            "settings emptied by uninstall are removed"
        );
    }

    #[test]
    fn instructions_block_is_replaced_in_place() {
        let (_d, env) = env();
        let p = env.cwd.join("AGENTS.md");
        std::fs::write(
            &p,
            format!("# Project\n\n{SECTION_START}\nold\n{SECTION_END}\n\nMore notes\n"),
        )
        .unwrap();
        upsert_instructions(&p).unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(
            t.starts_with("# Project")
                && t.ends_with("More notes\n")
                && !t.contains("old")
                && t.contains("codegraph_explore")
        );
        remove_instructions(&p).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "# Project\n\nMore notes\n"
        );
    }
}
