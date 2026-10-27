//! Server-side dispatch: task queues and event buses keyed by a name or a
//! type rather than a call.

use super::{Ctx, LineIndex, Proposal, Sink};
use codegraph_core::{Language, NodeKind};
use regex::Regex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Compile a pattern with ASCII word boundaries, which keep the regex
/// engine on its fast path over non-ASCII text.
fn re(p: &str) -> Regex {
    Regex::new(&p.replace(r"\b", r"(?-u:\b)")).unwrap()
}

const FANOUT: usize = 80;

// ── Celery: `task.delay(…)` / `task.apply_async(…)` → the task's body ──

static CELERY_DISPATCH: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b([A-Za-z_]\w*)\s*\.\s*(?:delay|apply_async)\s*\("));
static CELERY_DECORATOR: LazyLock<Regex> =
    LazyLock::new(|| re(r"@\s*(?:[A-Za-z_][\w.]*\.)?(?:shared_task|task)\b"));
static PY_DECL: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:async\s+def|def|class)\b"));
const DECORATOR_LOOKBACK: u32 = 12;

/// Whether a Python function carries a Celery task decorator.
fn is_celery_task(ctx: &Ctx<'_>, i: usize) -> bool {
    let n = ctx.node(i);
    if n.kind != NodeKind::Function || n.language != Language::Python {
        return false;
    }
    if n.decorators.iter().any(|d| CELERY_DECORATOR.is_match(d)) {
        return true;
    }
    let Some(src) = ctx.source(&n.file_path) else {
        return false;
    };
    let lines: Vec<&str> = src.lines().collect();
    let def = n.start_line.saturating_sub(1) as usize;
    // Start at the `def` itself: some extractors start a function at its
    // first decorator.
    let first = (n.start_line.saturating_sub(1 + DECORATOR_LOOKBACK)) as usize;
    for idx in (first..def.min(lines.len())).rev() {
        let t = lines[idx].trim();
        if PY_DECL.is_match(t) {
            break;
        }
        if CELERY_DECORATOR.is_match(t) {
            return true;
        }
    }
    false
}

pub fn celery(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    let mut sink = Sink::new(out);
    let tasks: RefCell<HashMap<usize, bool>> = RefCell::default();
    let is_task = |i: usize| {
        *tasks
            .borrow_mut()
            .entry(i)
            .or_insert_with(|| is_celery_task(ctx, i))
    };
    for file in ctx.files_of(&[Language::Python]) {
        let Some(src) = ctx.source(file) else {
            continue;
        };
        if !src.contains(".delay(") && !src.contains(".apply_async(") {
            continue;
        }
        let Some(safe) = ctx.stripped(file, Language::Python) else {
            continue;
        };
        let lines = LineIndex::new(&safe);
        let mut added = 0;
        for c in CELERY_DISPATCH.captures_iter(&safe) {
            if added >= FANOUT {
                break;
            }
            let name = &c[1];
            let line = lines.line(c.get(0).unwrap().start());
            let Some(disp) = ctx.enclosing_fn(file, line) else {
                continue;
            };
            let cands: Vec<usize> = ctx.named(name).filter(|&i| is_task(i)).collect();
            let target = match cands.as_slice() {
                [] => continue,
                [one] => *one,
                many => match many.iter().find(|&&i| ctx.node(i).file_path == file) {
                    Some(&t) => t,
                    None => continue,
                },
            };
            if sink.push(disp, target, line, name) {
                added += 1;
            }
        }
    }
}

// ── Spring: `publishEvent(new XEvent(…))` → `@EventListener` methods of XEvent ──

static SPRING_PUBLISH: LazyLock<Regex> =
    LazyLock::new(|| re(r"\.publishEvent\s*\(\s*new\s+([A-Z][A-Za-z0-9_]*)"));
static SPRING_LISTENER: LazyLock<Regex> =
    LazyLock::new(|| re(r"@(?:EventListener|TransactionalEventListener)\b"));
static SPRING_ANNO_TYPE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"@(?:EventListener|TransactionalEventListener)\s*\(\s*([A-Z][A-Za-z0-9_]*)\.class")
});
static SPRING_APP_LISTENER: LazyLock<Regex> = LazyLock::new(|| re(r"\bApplicationListener\s*<"));
static PASCAL_TYPE: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Z][A-Za-z0-9_]*$"));

/// The first parameter's type in a Java signature: `void (XEvent e)` → `XEvent`.
fn first_param_type(sig: &str) -> Option<String> {
    let open = sig.find('(')?;
    let rest = &sig[open + 1..];
    let inner = rest[..rest.find(')').unwrap_or(rest.len())].trim();
    let first = inner.split(',').next()?.trim();
    let toks: Vec<&str> = first
        .split_whitespace()
        .filter(|t| *t != "final" && !t.starts_with('@'))
        .collect();
    if toks.len() < 2 {
        return None;
    }
    let ty = toks[toks.len() - 2];
    let ty = &ty[..ty.find('<').unwrap_or(ty.len())];
    PASCAL_TYPE.is_match(ty).then(|| ty.to_string())
}

pub fn spring_events(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    let mut listeners: HashMap<String, Vec<usize>> = HashMap::new();
    let mut publishers = Vec::new();
    for file in ctx.files_of(&[Language::Java]) {
        let Some(src) = ctx.source(file) else {
            continue;
        };
        if src.contains(".publishEvent(") {
            publishers.push(file);
        }
        let has_anno =
            src.contains("@EventListener") || src.contains("@TransactionalEventListener");
        let has_app = SPRING_APP_LISTENER.is_match(&src);
        if !has_anno && !has_app {
            continue;
        }
        let lines: Vec<&str> = src.lines().collect();
        for &i in ctx.in_file(file) {
            let n = ctx.node(i);
            if n.kind != NodeKind::Method {
                continue;
            }
            // The method's own leading annotations (its range starts at them).
            let mut head = String::new();
            let start = n.start_line.saturating_sub(1) as usize;
            for line in lines.iter().skip(start).take(8) {
                let t = line.trim();
                if !t.starts_with('@') {
                    break;
                }
                head.push_str(t);
                head.push('\n');
            }
            for d in &n.decorators {
                head.push_str(d);
                head.push('\n');
            }
            let annotated = has_anno && SPRING_LISTENER.is_match(&head);
            let app = has_app && n.name == "onApplicationEvent";
            if !annotated && !app {
                continue;
            }
            let mut ty = n.signature.as_deref().and_then(first_param_type);
            if ty.is_none() && annotated {
                ty = SPRING_ANNO_TYPE.captures(&head).map(|c| c[1].to_string());
            }
            if let Some(ty) = ty {
                listeners.entry(ty).or_default().push(i);
            }
        }
    }
    if listeners.is_empty() {
        return;
    }
    let mut sink = Sink::new(out);
    for file in publishers {
        let Some(safe) = ctx.stripped(file, Language::Java) else {
            continue;
        };
        let lines = LineIndex::new(&safe);
        let mut added = 0;
        for c in SPRING_PUBLISH.captures_iter(&safe) {
            let Some(targets) = listeners.get(&c[1]) else {
                continue;
            };
            let line = lines.line(c.get(0).unwrap().start());
            let Some(disp) = ctx.enclosing_fn(file, line) else {
                continue;
            };
            for &t in targets {
                if added < FANOUT && sink.push(disp, t, line, &c[1]) {
                    added += 1;
                }
            }
        }
    }
}

// ── MediatR: `mediator.Send(x)` → the `Handle` of x's IRequestHandler ──

static MEDIATR_HANDLER_BASE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?:IRequestHandler|INotificationHandler)\s*<\s*([A-Za-z_]\w*)"));
static MEDIATR_DISPATCH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"([A-Za-z_][\w.]*)\s*\.\s*(?:Send|Publish)\s*\(\s*(new\s+[A-Z]\w*|[A-Za-z_]\w*)")
});
static MEDIATR_RECEIVER: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)mediator|sender|publisher"));
static INLINE_NEW: LazyLock<Regex> = LazyLock::new(|| re(r"^new\s+([A-Z]\w*)"));
static IDENT: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z_]\w*$"));

/// The type sent at `.Send(arg)`: `new X(…)`, or `arg` assigned `new X` or
/// declared `X arg` earlier in the method.
fn mediatr_arg_type(arg: &str, lines: &[&str], method_start: u32, line: u32) -> Option<String> {
    if let Some(c) = INLINE_NEW.captures(arg) {
        return Some(c[1].to_string());
    }
    if !IDENT.is_match(arg) {
        return None;
    }
    let a = regex::escape(arg);
    let assign = re(&format!(r"\b{a}\b\s*=\s*new\s+([A-Z]\w*)"));
    let decl = re(&format!(r"\b([A-Z]\w*)\b\s+{a}\b"));
    let mut declared = None;
    let from = method_start.saturating_sub(1) as usize;
    for l in lines.iter().take(line as usize).skip(from) {
        if let Some(c) = assign.captures(l) {
            return Some(c[1].to_string());
        }
        if declared.is_none() {
            declared = decl.captures(l).map(|c| c[1].to_string());
        }
    }
    declared
}

pub fn mediatr(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    let mut handlers: HashMap<String, Vec<usize>> = HashMap::new();
    for file in ctx.files_of(&[Language::CSharp]) {
        let Some(src) = ctx.source(file) else {
            continue;
        };
        if !src.contains("IRequestHandler<") && !src.contains("INotificationHandler<") {
            continue;
        }
        for &cls in ctx.in_file(file) {
            let n = ctx.node(cls);
            if n.kind != NodeKind::Class {
                continue;
            }
            let Some(decl) = ctx.lines(file, n.start_line, n.start_line + 3) else {
                continue;
            };
            let Some(c) = MEDIATR_HANDLER_BASE.captures(&decl) else {
                continue;
            };
            if let Some(h) = ctx.member_of(cls, NodeKind::Method, "Handle") {
                handlers.entry(c[1].to_string()).or_default().push(h);
            }
        }
    }
    if handlers.is_empty() {
        return;
    }
    let mut sink = Sink::new(out);
    for file in ctx.files_of(&[Language::CSharp]) {
        let Some(src) = ctx.source(file) else {
            continue;
        };
        if !src.contains(".Send(") && !src.contains(".Publish(") {
            continue;
        }
        let Some(safe) = ctx.stripped(file, Language::CSharp) else {
            continue;
        };
        let lines = LineIndex::new(&safe);
        let text: Vec<&str> = safe.lines().collect();
        let mut added = 0;
        for c in MEDIATR_DISPATCH.captures_iter(&safe) {
            if !MEDIATR_RECEIVER.is_match(&c[1]) {
                continue;
            }
            let line = lines.line(c.get(0).unwrap().start());
            let Some(disp) = ctx.enclosing_fn(file, line) else {
                continue;
            };
            let Some(ty) = mediatr_arg_type(&c[2], &text, ctx.node(disp).start_line, line) else {
                continue;
            };
            for &t in handlers.get(&ty).into_iter().flatten() {
                if added < FANOUT && sink.push(disp, t, line, &ty) {
                    added += 1;
                }
            }
        }
    }
}

// ── Sidekiq: `Worker.perform_async(…)` → `Worker#perform` ──

static SIDEKIQ_DISPATCH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"([A-Z][A-Za-z0-9_]*(?:::[A-Z][A-Za-z0-9_]*)*)\s*\.\s*perform_(?:async|in|at)\b")
});
static SIDEKIQ_WORKER: LazyLock<Regex> =
    LazyLock::new(|| re(r"\binclude\s+Sidekiq::(?:Job|Worker)\b"));

pub fn sidekiq(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    let performs: RefCell<HashMap<usize, Option<usize>>> = RefCell::default();
    let perform_of = |cls: usize| -> Option<usize> {
        if let Some(v) = performs.borrow().get(&cls) {
            return *v;
        }
        let n = ctx.node(cls);
        let v = ctx
            .lines(&n.file_path, n.start_line, n.end_line)
            .filter(|body| SIDEKIQ_WORKER.is_match(body))
            .and_then(|_| ctx.member_of(cls, NodeKind::Method, "perform"));
        performs.borrow_mut().insert(cls, v);
        v
    };
    let classes = |name: &str| -> Vec<usize> {
        ctx.named(name)
            .filter(|&i| {
                ctx.node(i).kind == NodeKind::Class && ctx.node(i).language == Language::Ruby
            })
            .collect()
    };
    let resolve = |r: &str| -> Option<usize> {
        if r.contains("::") {
            for i in ctx.qualified(r) {
                if ctx.node(i).kind == NodeKind::Class {
                    if let Some(p) = perform_of(i) {
                        return Some(p);
                    }
                }
            }
        }
        let tail = r.rsplit("::").next().unwrap_or(r);
        let workers: Vec<usize> = classes(tail).into_iter().filter_map(&perform_of).collect();
        (workers.len() == 1).then(|| workers[0])
    };
    let mut sink = Sink::new(out);
    for file in ctx.files_of(&[Language::Ruby]) {
        let Some(src) = ctx.source(file) else {
            continue;
        };
        if !src.contains(".perform_") {
            continue;
        }
        let Some(safe) = ctx.stripped(file, Language::Ruby) else {
            continue;
        };
        let lines = LineIndex::new(&safe);
        let mut added = 0;
        for c in SIDEKIQ_DISPATCH.captures_iter(&safe) {
            if added >= FANOUT {
                break;
            }
            let line = lines.line(c.get(0).unwrap().start());
            let Some(disp) = ctx.enclosing_fn(file, line) else {
                continue;
            };
            if let Some(t) = resolve(&c[1]) {
                if sink.push(disp, t, line, &c[1]) {
                    added += 1;
                }
            }
        }
    }
}

// ── Laravel: `event(new X(…))` → listeners' `handle(X $e)` ──

static LARAVEL_DISPATCH: LazyLock<Regex> =
    LazyLock::new(|| re(r"\bevent\s*\(\s*new\s+\\?([A-Za-z_][\w\\]*)"));
static LARAVEL_STATIC_DISPATCH: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b([A-Z]\w*)::dispatch\s*\("));
static HANDLE_DECL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"function\s+handle\s*\(\s*(?:\.\.\.\s*)?(\??[A-Za-z_\\][\w\\|]*)\s+&?\s*(?:\.\.\.\s*)?\$")
});
static LISTEN_DECL: LazyLock<Regex> = LazyLock::new(|| re(r"\$listen\s*=\s*\["));
static LISTEN_ENTRY: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"(?:([A-Za-z_\\][\w\\]*)::class|'([^']+)'|"([^"]+)")\s*=>\s*\[([^\]]*)\]"#)
});
static LISTEN_CLASS: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?:([A-Za-z_\\][\w\\]*)::class|'([^']+)'|"([^"]+)")"#));
static UPPER_IDENT: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Z]\w*$"));

/// `\App\Events\Foo` → `Foo`.
fn php_simple_name(s: &str) -> &str {
    let s = s.trim_start_matches('\\');
    let s = s.rsplit('\\').next().unwrap_or(s);
    s.rsplit("::").next().unwrap_or(s).trim()
}

fn bracket_body(src: &str, open: usize) -> Option<&str> {
    let mut depth = 0;
    for (i, ch) in src[open..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&src[open + 1..open + i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn any_group<'t>(c: &regex::Captures<'t>) -> &'t str {
    c.get(1)
        .or_else(|| c.get(2))
        .or_else(|| c.get(3))
        .map_or("", |m| m.as_str())
}

pub fn laravel_events(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    let mut listeners: HashMap<String, Vec<usize>> = HashMap::new();
    let mut add = |event: &str, handle: usize| {
        let v = listeners.entry(event.to_string()).or_default();
        if !v.contains(&handle) {
            v.push(handle);
        }
    };
    let handle_of = |cls: usize| ctx.member_of(cls, NodeKind::Method, "handle");
    for file in ctx.files_of(&[Language::Php]) {
        let Some(src) = ctx.source(file) else {
            continue;
        };
        if src.contains("function handle") {
            for &i in ctx.in_file(file) {
                let n = ctx.node(i);
                if n.kind != NodeKind::Method || n.name != "handle" {
                    continue;
                }
                let Some(decl) = ctx.lines(file, n.start_line, n.start_line + 2) else {
                    continue;
                };
                let Some(c) = HANDLE_DECL.captures(&decl) else {
                    continue;
                };
                for t in c[1].trim_start_matches('?').split('|') {
                    let t = php_simple_name(t);
                    if UPPER_IDENT.is_match(t) {
                        add(t, i);
                    }
                }
            }
        }
        if src.contains("$listen") {
            let Some(safe) = ctx.stripped(file, Language::Php) else {
                continue;
            };
            let Some(m) = LISTEN_DECL.find(&safe) else {
                continue;
            };
            let Some(body) = bracket_body(&safe, m.end() - 1) else {
                continue;
            };
            for e in LISTEN_ENTRY.captures_iter(body) {
                let event = php_simple_name(any_group(&e));
                for l in LISTEN_CLASS.captures_iter(&e[4]) {
                    let name = php_simple_name(any_group(&l));
                    let handle = ctx
                        .named(name)
                        .filter(|&i| ctx.node(i).kind == NodeKind::Class)
                        .find_map(handle_of);
                    if let Some(h) = handle {
                        add(event, h);
                    }
                }
            }
        }
    }
    if listeners.is_empty() {
        return;
    }
    let mut sink = Sink::new(out);
    for file in ctx.files_of(&[Language::Php]) {
        let Some(src) = ctx.source(file) else {
            continue;
        };
        if !src.contains("event(") && !src.contains("::dispatch(") {
            continue;
        }
        let Some(safe) = ctx.stripped(file, Language::Php) else {
            continue;
        };
        let lines = LineIndex::new(&safe);
        let mut added = 0;
        let sites = LARAVEL_DISPATCH
            .captures_iter(&safe)
            .chain(LARAVEL_STATIC_DISPATCH.captures_iter(&safe));
        for c in sites {
            let event = php_simple_name(&c[1]);
            let Some(targets) = listeners.get(event) else {
                continue;
            };
            let line = lines.line(c.get(0).unwrap().start());
            let Some(disp) = ctx.enclosing_fn(file, line) else {
                continue;
            };
            for &t in targets {
                if added < 200 && sink.push(disp, t, line, event) {
                    added += 1;
                }
            }
        }
    }
}
