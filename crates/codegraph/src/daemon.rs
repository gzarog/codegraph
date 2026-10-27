//! The shared MCP daemon: one background process per project serving every
//! agent session, so they share one file watcher, one SQLite connection and
//! one warm index instead of paying for each.
//!
//! - `codegraph serve --mcp` connects to the project's daemon (starting one
//!   if none runs) and relays stdio to it. If that fails it serves in-process,
//!   as it does with `CODEGRAPH_NO_DAEMON=1` or `--no-daemon`.
//! - The daemon listens on 127.0.0.1 at a random port. Every connection must
//!   open with the secret token from `.codegraph/daemon.json` (readable only
//!   by its owner), so other local users can't talk to it.
//! - It is detached from the agent that started it, so closing one session
//!   can't sever the others. It exits after `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS`
//!   (default 5 minutes) without clients.
//! - `~/.codegraph/daemons/` lists running daemons for `codegraph daemon`.

use crate::global;
use crate::mcp::{Engine, Server};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_IDLE_MS: u64 = 300_000;
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const START_TIMEOUT: Duration = Duration::from_secs(8);

/// What a running daemon publishes in `.codegraph/daemon.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonInfo {
    pub pid: u32,
    pub port: u16,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token: String,
    pub version: String,
    pub started_at: u64,
    pub root: String,
}

pub fn info_path(root: &Path) -> PathBuf {
    crate::project::data_dir(root).join("daemon.json")
}

fn registry_dir() -> PathBuf {
    global::dir().join("daemons")
}

fn registry_path(root: &Path) -> PathBuf {
    let digest = codegraph_index::content_hash(root.to_string_lossy().as_bytes());
    registry_dir().join(format!("{}.json", &digest[..16]))
}

pub fn read_info(root: &Path) -> Option<DaemonInfo> {
    serde_json::from_str(&std::fs::read_to_string(info_path(root)).ok()?).ok()
}

fn idle_timeout() -> Duration {
    let ms = std::env::var("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_IDLE_MS);
    Duration::from_millis(ms)
}

/// Whether `serve --mcp` should use a daemon.
pub fn enabled() -> bool {
    !global::env_truthy("CODEGRAPH_NO_DAEMON")
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Write `info` so only its owner can read the token.
fn write_private(path: &Path, info: &DaemonInfo, create_new: bool) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true);
    if create_new {
        opts.create_new(true);
    } else {
        opts.create(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(&serde_json::to_vec_pretty(info).unwrap_or_default())
}

// ------------------------------------------------------------------ client

pub enum Connect {
    Ok(TcpStream),
    /// A daemon of another version answered.
    Mismatch(DaemonInfo),
    None,
}

fn hello(info: &DaemonInfo, message: serde_json::Value) -> Option<(TcpStream, serde_json::Value)> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], info.port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(800)).ok()?;
    stream.set_read_timeout(Some(HELLO_TIMEOUT)).ok()?;
    writeln!(stream, "{message}").ok()?;
    let mut line = String::new();
    BufReader::new(stream.try_clone().ok()?)
        .read_line(&mut line)
        .ok()?;
    let reply: serde_json::Value = serde_json::from_str(&line).ok()?;
    stream.set_read_timeout(None).ok()?;
    Some((stream, reply))
}

/// Connect to `root`'s daemon and complete the handshake.
pub fn connect(root: &Path) -> Connect {
    let Some(info) = read_info(root) else {
        return Connect::None;
    };
    let msg = serde_json::json!({ "hello": info.token, "version": VERSION });
    match hello(&info, msg) {
        Some((stream, reply)) if reply["ok"] == true => Connect::Ok(stream),
        Some((_, reply)) if reply["version"].is_string() => Connect::Mismatch(info),
        _ => Connect::None,
    }
}

/// Ask `root`'s daemon to exit; true when one acknowledged.
pub fn stop(root: &Path) -> bool {
    let Some(info) = read_info(root) else {
        return false;
    };
    let msg = serde_json::json!({ "hello": info.token, "shutdown": true });
    let stopped = hello(&info, msg).is_some_and(|(_, r)| r["ok"] == true);
    if stopped {
        // Wait for it to clean up so a successor can start.
        let until = Instant::now() + Duration::from_secs(3);
        while info_path(root).exists() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    stopped
}

/// Start a detached daemon for `root`.
fn spawn(root: &Path) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.args(["daemon", "run"])
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own session: closing the agent's terminal doesn't reach it.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn().map(|_| ())
}

/// A handshaken connection to `root`'s daemon, starting (or replacing an
/// outdated) one when needed.
pub fn connect_or_spawn(root: &Path) -> Option<TcpStream> {
    match connect(root) {
        Connect::Ok(s) => return Some(s),
        Connect::Mismatch(info) => {
            eprintln!(
                "[CodeGraph] replacing daemon v{} with v{VERSION}",
                info.version
            );
            stop(root);
        }
        Connect::None => {}
    }
    // A record left by a dead daemon would block the new one.
    if let Some(info) = read_info(root) {
        if !crate::lock::alive(info.pid) {
            let _ = std::fs::remove_file(info_path(root));
        }
    }
    spawn(root).ok()?;
    let until = Instant::now() + START_TIMEOUT;
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(60));
        if let Connect::Ok(s) = connect(root) {
            return Some(s);
        }
    }
    None
}

/// Relay stdio to the daemon until either side closes.
pub fn proxy(
    stream: TcpStream,
    lines: std::sync::mpsc::Receiver<String>,
    stdout: std::sync::Arc<std::sync::Mutex<std::io::Stdout>>,
) -> std::io::Result<()> {
    let mut to_daemon = stream.try_clone()?;
    std::thread::spawn(move || {
        for line in lines {
            if writeln!(to_daemon, "{line}").is_err() {
                break;
            }
        }
        let _ = to_daemon.shutdown(Shutdown::Write);
    });
    for line in BufReader::new(stream).lines() {
        let line = line?;
        let mut out = stdout.lock().unwrap_or_else(|p| p.into_inner());
        writeln!(out, "{line}")?;
        out.flush()?;
    }
    Ok(())
}

// ------------------------------------------------------------------ daemon

/// Running daemons from the registry (dead entries are pruned).
pub fn list() -> Vec<DaemonInfo> {
    let Ok(entries) = std::fs::read_dir(registry_dir()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let info = std::fs::read_to_string(e.path())
            .ok()
            .and_then(|t| serde_json::from_str::<DaemonInfo>(&t).ok());
        match info {
            Some(i) if crate::lock::alive(i.pid) && info_path(Path::new(&i.root)).exists() => {
                out.push(i)
            }
            _ => {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    out.sort_by(|a, b| a.root.cmp(&b.root));
    out
}

struct Cleanup {
    root: PathBuf,
    pid: u32,
}

impl Cleanup {
    fn run(&self) {
        if read_info(&self.root).is_some_and(|i| i.pid == self.pid) {
            let _ = std::fs::remove_file(info_path(&self.root));
        }
        let _ = std::fs::remove_file(registry_path(&self.root));
        crate::telemetry::persist();
    }
}

/// Run the daemon for `root` in this process until it goes idle or is
/// told to stop. Returns early when another live daemon already serves it.
pub fn run(root: &Path) -> std::io::Result<()> {
    let root = crate::project::find_root(root)
        .ok_or_else(|| std::io::Error::other(format!("{} is not indexed", root.display())))?;
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let info = DaemonInfo {
        pid: std::process::id(),
        port: listener.local_addr()?.port(),
        token: global::hex(&global::random_bytes::<24>()),
        version: VERSION.to_string(),
        started_at: global::now_ms(),
        root: root.to_string_lossy().into_owned(),
    };
    // Exactly one daemon per project: the info file is created exclusively.
    if write_private(&info_path(&root), &info, true).is_err() {
        if matches!(connect(&root), Connect::Ok(_)) {
            return Ok(());
        }
        let _ = std::fs::remove_file(info_path(&root));
        write_private(&info_path(&root), &info, true)?;
    }
    let _ = std::fs::create_dir_all(registry_dir());
    let public = DaemonInfo {
        token: String::new(),
        ..info.clone()
    };
    let _ = write_private(&registry_path(&root), &public, false);
    let cleanup = Arc::new(Cleanup {
        root: root.clone(),
        pid: info.pid,
    });

    let engine = Engine::new(true);
    let active = Arc::new(AtomicUsize::new(0));
    let last_seen = Arc::new(AtomicU64::new(global::now_ms()));
    crate::mcp::start_telemetry_thread();

    // Idle shutdown.
    {
        let (active, last_seen, cleanup) = (active.clone(), last_seen.clone(), cleanup.clone());
        let idle = idle_timeout();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(
                500.min(idle.as_millis() as u64).max(50),
            ));
            let quiet = global::now_ms().saturating_sub(last_seen.load(Ordering::Relaxed));
            if active.load(Ordering::Relaxed) == 0 && quiet >= idle.as_millis() as u64 {
                cleanup.run();
                std::process::exit(0);
            }
        });
    }

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (engine, active, last_seen, cleanup) = (
            engine.clone(),
            active.clone(),
            last_seen.clone(),
            cleanup.clone(),
        );
        let (token, root) = (info.token.clone(), root.clone());
        std::thread::spawn(move || {
            let _ = serve_connection(stream, &token, &root, engine, &active, &cleanup);
            last_seen.store(global::now_ms(), Ordering::Relaxed);
        });
    }
    Ok(())
}

fn serve_connection(
    stream: TcpStream,
    token: &str,
    root: &Path,
    engine: Arc<Engine>,
    active: &AtomicUsize,
    cleanup: &Cleanup,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(HELLO_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let hello: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
    let given = hello["hello"].as_str().unwrap_or("");
    if !constant_time_eq(given, token) {
        return Ok(()); // not ours: say nothing
    }
    if hello["shutdown"] == true {
        writeln!(writer, "{}", serde_json::json!({ "ok": true }))?;
        cleanup.run();
        std::process::exit(0);
    }
    if hello["version"].as_str() != Some(VERSION) {
        writeln!(
            writer,
            "{}",
            serde_json::json!({ "ok": false, "version": VERSION })
        )?;
        return Ok(());
    }
    writeln!(
        writer,
        "{}",
        serde_json::json!({ "ok": true, "version": VERSION })
    )?;
    writer.set_read_timeout(None)?;
    active.fetch_add(1, Ordering::Relaxed);
    let mut session = Server::with_engine(Some(root.to_path_buf()), engine);
    let result = session.run(reader, writer);
    active.fetch_sub(1, Ordering::Relaxed);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn sessions_share_one_daemon_and_strangers_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("CODEGRAPH_HOME", home.path());
        std::fs::write(
            dir.path().join("a.ts"),
            "export function hello() { return 1; }\n",
        )
        .unwrap();
        let mut cg = crate::project::CodeGraph::init(dir.path()).unwrap();
        cg.index_all(None).unwrap();
        drop(cg);
        let root = dir.path().canonicalize().unwrap();
        let r = root.clone();
        std::thread::spawn(move || run(&r));
        let until = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            if let Connect::Ok(s) = connect(&root) {
                break s;
            }
            assert!(Instant::now() < until, "daemon did not start");
            std::thread::sleep(Duration::from_millis(50));
        };
        // A second, concurrent session.
        let Connect::Ok(mut other) = connect(&root) else {
            panic!("second session refused")
        };
        let call = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "codegraph_search", "arguments": { "query": "hello" } } });
        for s in [&mut stream, &mut other] {
            writeln!(s, "{call}").unwrap();
            let mut line = String::new();
            BufReader::new(s.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            assert!(line.contains("hello"), "{line}");
        }
        // Without the token nothing is answered.
        let info = read_info(&root).unwrap();
        let mut stranger = TcpStream::connect(("127.0.0.1", info.port)).unwrap();
        writeln!(
            stranger,
            "{}",
            serde_json::json!({ "hello": "guess", "version": VERSION })
        )
        .unwrap();
        let mut buf = String::new();
        stranger
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let _ = stranger.read_to_string(&mut buf);
        assert!(buf.is_empty(), "{buf}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(info_path(&root))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }
    }
}
