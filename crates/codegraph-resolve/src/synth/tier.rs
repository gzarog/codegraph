//! Channels between tiers that only a string connects: a job put on a queue
//! and the processor that takes it, an event on a bus and its `@OnEvent`
//! handlers, a socket message and the handler on the other side.

use super::{Ctx, LineIndex, Proposal, Sink};
use codegraph_core::{Language, NodeKind};
use regex::Regex;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::LazyLock;

/// Compile a pattern with ASCII word boundaries, which keep the regex
/// engine on its fast path over non-ASCII text.
fn re(p: &str) -> Regex {
    Regex::new(&p.replace(r"\b", r"(?-u:\b)")).unwrap()
}

const JS: &[Language] = &[
    Language::TypeScript,
    Language::JavaScript,
    Language::Tsx,
    Language::Jsx,
];
/// More handlers or dispatchers than this on one name is too generic to pair.
const FANOUT: usize = 6;

static QUEUE_BINDING: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"\b(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=;]*?)?=\s*(?:new\s+)?(?:Queue|Bull|BullQueue)\s*(?:<[^>]*>)?\s*\(\s*['"`]([^'"`]+)['"`]"#,
    )
});
static INJECT_QUEUE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"@InjectQueue\s*\(\s*['"`]([^'"`]+)['"`]\s*\)\s*(?:(?:private|public|protected|readonly)\s+)*([A-Za-z_$][\w$]*)"#,
    )
});
static QUEUE_ADD: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"((?:this\s*\.\s*)?[A-Za-z_$][\w$]*(?:\s*\.\s*[A-Za-z_$][\w$]*)*)\s*\.\s*add\s*\(\s*['"`]([^'"`]+)['"`]"#,
    )
});
static QUEUE_SHAPED: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)queue|jobs?$|worker|bull|flow|producer"));
static NEW_WORKER: LazyLock<Regex> =
    LazyLock::new(|| re(r#"\bnew\s+Worker\s*(?:<[^>]*>)?\s*\(\s*['"`]([^'"`]+)['"`]\s*,\s*"#));
static QUEUE_PROCESS: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"((?:this\s*\.\s*)?[A-Za-z_$][\w$]*(?:\s*\.\s*[A-Za-z_$][\w$]*)*)\s*\.\s*process\s*\(\s*(?:['"`]([^'"`]+)['"`]\s*,\s*)?(?:\d+\s*,\s*)?"#,
    )
});
static HANDLER_ARG: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^(?:(?:async\s+)?([A-Za-z_$][\w$.]*)\s*(?:[,)]|$)|(?:async\s*)?(?:\(|function\b|[A-Za-z_$][\w$]*\s*=>))",
    )
});
static CLASS_DECL: LazyLock<Regex> = LazyLock::new(|| re(r"\bclass\s+([A-Za-z_$][\w$]*)"));
static FIRST_LITERAL: LazyLock<Regex> = LazyLock::new(|| re(r#"['"`]([^'"`]+)['"`]"#));
static EMIT: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"((?:[\w$]+(?:\([^()]*\))?\s*\.\s*)*[\w$]+)\s*\.\s*(emit|emitAsync)\s*\(\s*['"`]([^'"`\n]+)['"`]"#,
    )
});
static SOCKET_ON: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"((?:[\w$]+(?:\([^()]*\))?\s*\.\s*)*[\w$]+)\s*\.\s*(?:on|once)\s*\(\s*['"`]([^'"`\n]+)['"`]\s*,\s*"#,
    )
});
static SOCKET_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^(?:socket|io|ws|wss|client|server|namespace|nsp|conn|connection|gateway|broadcast|to|in|of|except|volatile|local|sockets|socketServer|wsServer|room|channel|pusher|ably|ioClient|socketClient|sock)$",
    )
});
static BUS_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^(?:eventEmitter|emitter|events|eventBus|bus|dispatcher|pubsub|publisher|eventPublisher|ee|hub|mediator|broker|messageBus|appEvents|domainEvents|eventsService|eventService)$",
    )
});
static GENERIC_EVENT: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^(?:error|connect|connect_error|connect_failed|connection|disconnect|disconnecting|reconnect|reconnect_attempt|reconnecting|reconnect_error|reconnect_failed|close|open|end|data|ready|drain|finish|pipe|unpipe|listening|timeout|ping|pong|upgrade|newListener|removeListener)$",
    )
});
static SOCKET_SERVER_FILE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"@WebSocketGateway\s*\(|@SubscribeMessage\s*\(|@WebSocketServer\s*\(|\bnew\s+(?:Server|SocketIOServer|WebSocketServer|WebSocket\.Server|WSServer)\b|\.on\s*\(\s*['"]connection['"]"#,
    )
});
static CALL_ARGS: LazyLock<Regex> = LazyLock::new(|| re(r"\([^()]*\)"));
static METHOD_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^(?:\s*@[\w.]+\s*(?:\([^)]*\))?)*\s*(?:(?:public|private|protected|async|static|readonly|override)\s+)*([A-Za-z_$][\w$]*)\s*[<(]",
    )
});

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Whether a match at `at` starts a name rather than continuing one.
fn starts_name(src: &str, at: usize) -> bool {
    !src[..at].chars().next_back().is_some_and(is_ident_char)
}

fn close_paren(s: &str, open: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'' | b'`') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Every `@Name(…)` decorator: its arguments, where it starts and ends.
fn decorators<'s>(safe: &'s str, name: &str) -> Vec<(&'s str, usize, usize)> {
    let head = re(&format!(r"@{name}\s*\("));
    let mut out = Vec::new();
    for m in head.find_iter(safe) {
        if let Some(close) = close_paren(safe, m.end() - 1) {
            out.push((&safe[m.end()..close], m.start(), close + 1));
        }
    }
    out
}

fn window(s: &str, from: usize, len: usize) -> &str {
    let mut end = (from + len).min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[from..end]
}

struct Facts {
    file: String,
    safe: Rc<str>,
    lines: LineIndex,
    queues: HashMap<String, String>,
    socket_server: bool,
}

/// The method a decorator ending at `end` sits on.
fn decorated_method(ctx: &Ctx<'_>, f: &Facts, end: usize, class: Option<usize>) -> Option<usize> {
    let c = METHOD_HEAD.captures(window(&f.safe, end, 400))?;
    let at = end + c.get(1).unwrap().start();
    let line = f.lines.line(at);
    let name = &c[1];
    ctx.in_file(&f.file).iter().copied().find(|&i| {
        let n = ctx.node(i);
        matches!(n.kind, NodeKind::Method | NodeKind::Function)
            && n.name == name
            && n.start_line + 1 >= line
            && n.start_line <= line + 1
            && class.is_none_or(|k| {
                let k = ctx.node(k);
                n.start_line >= k.start_line && n.end_line <= k.end_line
            })
    })
}

/// The node a handler argument names, or the function it is written in.
fn handler_node(ctx: &Ctx<'_>, f: &Facts, text: &str, line: u32) -> Option<usize> {
    let c = HANDLER_ARG.captures(text.trim_start())?;
    if let Some(name) = c.get(1) {
        let name = name.as_str().rsplit('.').next().unwrap_or(name.as_str());
        let cands: Vec<usize> = ctx
            .named(name)
            .filter(|&i| matches!(ctx.node(i).kind, NodeKind::Function | NodeKind::Method))
            .collect();
        let local: Vec<usize> = cands
            .iter()
            .copied()
            .filter(|&i| ctx.node(i).file_path == f.file)
            .collect();
        return match (local.as_slice(), cands.as_slice()) {
            ([one], _) => Some(*one),
            ([], [one]) => Some(*one),
            _ => None,
        };
    }
    ctx.enclosing_fn(&f.file, line)
}

struct Producer {
    func: usize,
    line: u32,
    queue: Option<String>,
    job: String,
}

struct Consumer {
    node: usize,
    queue: Option<String>,
    job: Option<String>,
}

struct Dispatch {
    func: usize,
    line: u32,
    event: String,
    bus: bool,
    server: bool,
}

struct Handler {
    node: usize,
    pattern: String,
    bus: bool,
    server: bool,
}

/// `user.*` matches one segment, `**` any; anything else is exact.
fn event_matches(pattern: &str, event: &str) -> bool {
    if pattern == event {
        return true;
    }
    if !pattern.contains('*') {
        return false;
    }
    let body = pattern
        .split("**")
        .map(|part| {
            part.split('*')
                .map(regex::escape)
                .collect::<Vec<_>>()
                .join("[^.]+")
        })
        .collect::<Vec<_>>()
        .join(".*");
    Regex::new(&format!("^{body}$")).is_ok_and(|r| r.is_match(event))
}

fn shape_of(receiver: &str) -> Option<bool> {
    let cleaned = CALL_ARGS.replace_all(receiver, "");
    let segs: Vec<String> = cleaned
        .split('.')
        .map(|s| s.trim().to_string())
        .filter(|s| s != "this")
        .collect();
    if segs.iter().any(|s| SOCKET_WORDS.is_match(s)) {
        Some(false)
    } else if segs.iter().any(|s| BUS_WORDS.is_match(s)) {
        Some(true)
    } else {
        None
    }
}

/// The files that could hold a queue, bus or socket site, read once.
fn collect_facts(ctx: &Ctx<'_>) -> (Vec<Facts>, HashMap<String, Vec<String>>) {
    let mut facts = Vec::new();
    // Queue names bound anywhere, for receivers imported from another file.
    let mut exported_queues: HashMap<String, Vec<String>> = HashMap::new();
    if !ctx.has_language(JS) {
        return (facts, exported_queues);
    }
    for (file, lang) in &ctx.files {
        if !JS.contains(lang)
            || codegraph_core::search::is_test_path(file)
            || ctx.is_generated(file)
        {
            continue;
        }
        let Some(src) = ctx.source(file) else {
            continue;
        };
        let interesting = [
            ".add(",
            "@Processor",
            "new Worker",
            ".process(",
            ".emit",
            ".on(",
            ".once(",
            "@OnEvent",
            "@SubscribeMessage",
        ]
        .iter()
        .any(|p| src.contains(p));
        if !interesting {
            continue;
        }
        let Some(safe) = ctx.stripped(file, *lang) else {
            continue;
        };
        let mut queues = HashMap::new();
        for c in QUEUE_BINDING.captures_iter(&safe) {
            queues.insert(c[1].to_string(), c[2].to_string());
            exported_queues
                .entry(c[1].to_string())
                .or_default()
                .push(c[2].to_string());
        }
        for c in INJECT_QUEUE.captures_iter(&safe) {
            queues.insert(c[2].to_string(), c[1].to_string());
        }
        facts.push(Facts {
            file: file.clone(),
            lines: LineIndex::new(&safe),
            socket_server: SOCKET_SERVER_FILE.is_match(&safe),
            safe,
            queues,
        });
    }
    (facts, exported_queues)
}

fn last_segment(receiver: &str) -> String {
    receiver
        .rsplit('.')
        .next()
        .unwrap_or(receiver)
        .trim()
        .to_string()
}

/// `queue.add('job')` → the processor of that job on that queue.
pub fn queues(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    let (facts, exported_queues) = collect_facts(ctx);
    let queue_for = |f: &Facts, receiver: &str| -> Option<String> {
        let compact: String = receiver.chars().filter(|c| !c.is_whitespace()).collect();
        let head = compact
            .strip_prefix("this.")
            .unwrap_or(&compact)
            .split('.')
            .next()
            .unwrap_or("")
            .to_string();
        if let Some(q) = f.queues.get(&head) {
            return Some(q.clone());
        }
        match exported_queues.get(&head).map(Vec::as_slice) {
            Some([one]) => Some(one.clone()),
            _ => None,
        }
    };
    let mut producers = Vec::new();
    let mut consumers = Vec::new();
    for f in &facts {
        let safe = &*f.safe;
        for c in QUEUE_ADD.captures_iter(safe) {
            let r = c.get(1).unwrap();
            if !starts_name(safe, r.start()) {
                continue;
            }
            let queue = queue_for(f, r.as_str());
            if queue.is_none() && !QUEUE_SHAPED.is_match(&last_segment(r.as_str())) {
                continue;
            }
            let line = f.lines.line(r.start());
            if let Some(func) = ctx.enclosing_fn(&f.file, line) {
                producers.push(Producer {
                    func,
                    line,
                    queue,
                    job: c[2].to_string(),
                });
            }
        }
        for (args, _, end) in decorators(safe, "Processor") {
            let queue = FIRST_LITERAL.captures(args).map(|c| c[1].to_string());
            let Some(cm) = CLASS_DECL.captures(&safe[end..]) else {
                continue;
            };
            let class_line = f.lines.line(end + cm.get(0).unwrap().start());
            let Some(class) = ctx.in_file(&f.file).iter().copied().find(|&i| {
                let n = ctx.node(i);
                n.kind == NodeKind::Class && n.name == cm[1] && n.start_line + 2 >= class_line
            }) else {
                continue;
            };
            let (start, stop) = (ctx.node(class).start_line, ctx.node(class).end_line);
            let mut any = false;
            for (jargs, jat, jend) in decorators(safe, "Process") {
                let line = f.lines.line(jat);
                if line < start || line > stop {
                    continue;
                }
                if let Some(m) = decorated_method(ctx, f, jend, Some(class)) {
                    any = true;
                    consumers.push(Consumer {
                        node: m,
                        queue: queue.clone(),
                        job: FIRST_LITERAL.captures(jargs).map(|c| c[1].to_string()),
                    });
                }
            }
            if !any {
                if let Some(p) = ctx.member_of(class, NodeKind::Method, "process") {
                    consumers.push(Consumer {
                        node: p,
                        queue,
                        job: None,
                    });
                }
            }
        }
        for c in NEW_WORKER.captures_iter(safe) {
            let m = c.get(0).unwrap();
            let line = f.lines.line(m.start());
            if let Some(node) = handler_node(ctx, f, window(safe, m.end(), 200), line) {
                consumers.push(Consumer {
                    node,
                    queue: Some(c[1].to_string()),
                    job: None,
                });
            }
        }
        for c in QUEUE_PROCESS.captures_iter(safe) {
            let r = c.get(1).unwrap();
            if !starts_name(safe, r.start()) {
                continue;
            }
            let queue = queue_for(f, r.as_str());
            if queue.is_none() && !QUEUE_SHAPED.is_match(&last_segment(r.as_str())) {
                continue;
            }
            let m = c.get(0).unwrap();
            let line = f.lines.line(m.start());
            if let Some(node) = handler_node(ctx, f, window(safe, m.end(), 200), line) {
                consumers.push(Consumer {
                    node,
                    queue,
                    job: c.get(2).map(|j| j.as_str().to_string()),
                });
            }
        }
    }
    let mut sink = Sink::new(out);
    for p in &producers {
        let fits = |c: &&Consumer| {
            (p.queue.is_none() || c.queue.is_none() || c.queue == p.queue)
                && c.job.as_ref().is_none_or(|j| *j == p.job)
        };
        let all: Vec<&Consumer> = consumers.iter().filter(fits).collect();
        // The most specific pairing wins: the job on its named queue, then
        // the job by name, then the queue's default consumer.
        let exact: Vec<&Consumer> = all
            .iter()
            .copied()
            .filter(|c| c.job.as_deref() == Some(&p.job) && c.queue == p.queue && p.queue.is_some())
            .collect();
        let by_job: Vec<&Consumer> = all
            .iter()
            .copied()
            .filter(|c| c.job.as_deref() == Some(&p.job))
            .collect();
        let chosen = if !exact.is_empty() {
            exact
        } else if !by_job.is_empty() {
            by_job
        } else if p.queue.is_none() {
            continue;
        } else {
            all.into_iter().filter(|c| c.queue == p.queue).collect()
        };
        if chosen.is_empty() || chosen.len() > FANOUT {
            continue;
        }
        for c in chosen {
            sink.push(p.func, c.node, p.line, &p.job);
        }
    }
}

/// A bus event → its `@OnEvent` handlers; a socket message → the handler
/// on the other side.
pub fn events(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    let (facts, _) = collect_facts(ctx);
    let mut dispatches = Vec::new();
    let mut handlers = Vec::new();
    for f in &facts {
        let safe = &*f.safe;
        for c in EMIT.captures_iter(safe) {
            let r = c.get(1).unwrap();
            if !starts_name(safe, r.start()) || GENERIC_EVENT.is_match(&c[3]) {
                continue;
            }
            let Some(bus) = shape_of(r.as_str()) else {
                continue;
            };
            let line = f.lines.line(r.start());
            if let Some(func) = ctx.enclosing_fn(&f.file, line) {
                dispatches.push(Dispatch {
                    func,
                    line,
                    event: c[3].to_string(),
                    bus,
                    server: f.socket_server,
                });
            }
        }
        for (args, _, end) in decorators(safe, "OnEvent") {
            let Some(m) = decorated_method(ctx, f, end, None) else {
                continue;
            };
            let patterns: Vec<String> = if args.trim_start().starts_with('[') {
                FIRST_LITERAL
                    .captures_iter(args)
                    .map(|c| c[1].to_string())
                    .collect()
            } else {
                FIRST_LITERAL
                    .captures(args)
                    .map(|c| vec![c[1].to_string()])
                    .unwrap_or_default()
            };
            for pattern in patterns {
                handlers.push(Handler {
                    node: m,
                    pattern,
                    bus: true,
                    server: f.socket_server,
                });
            }
        }
        for (args, _, end) in decorators(safe, "SubscribeMessage") {
            let (Some(m), Some(event)) = (
                decorated_method(ctx, f, end, None),
                FIRST_LITERAL.captures(args),
            ) else {
                continue;
            };
            handlers.push(Handler {
                node: m,
                pattern: event[1].to_string(),
                bus: false,
                server: true,
            });
        }
        for c in SOCKET_ON.captures_iter(safe) {
            let r = c.get(1).unwrap();
            if !starts_name(safe, r.start())
                || shape_of(r.as_str()) != Some(false)
                || GENERIC_EVENT.is_match(&c[2])
            {
                continue;
            }
            let m = c.get(0).unwrap();
            let line = f.lines.line(m.start());
            if let Some(node) = handler_node(ctx, f, window(safe, m.end(), 200), line) {
                handlers.push(Handler {
                    node,
                    pattern: c[2].to_string(),
                    bus: false,
                    server: f.socket_server,
                });
            }
        }
    }
    let mut sink = Sink::new(out);
    let mut by_event: HashMap<(bool, &str), usize> = HashMap::new();
    for d in &dispatches {
        *by_event.entry((d.bus, d.event.as_str())).or_default() += 1;
    }
    for d in &dispatches {
        if by_event[&(d.bus, d.event.as_str())] > FANOUT {
            continue;
        }
        let matched: Vec<&Handler> = handlers
            .iter()
            .filter(|h| {
                if d.bus {
                    h.bus && event_matches(&h.pattern, &d.event)
                } else {
                    // A client's emit reaches the server's handler, and back.
                    !h.bus && h.server != d.server && h.pattern == d.event
                }
            })
            .collect();
        if matched.is_empty() || matched.len() > FANOUT {
            continue;
        }
        for h in matched {
            sink.push(d.func, h.node, d.line, &d.event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::event_matches;

    #[test]
    fn event_globs() {
        assert!(event_matches("user.*", "user.created"));
        assert!(!event_matches("user.*", "user.a.b"));
        assert!(event_matches("user.**", "user.a.b"));
        assert!(!event_matches("order.created", "user.created"));
    }
}
