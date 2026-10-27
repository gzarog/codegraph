//! React Native bridges: native code emitting an event that JavaScript
//! listens for, and Fabric components implemented by a native view class.

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

static OBJC_SEND: LazyLock<Regex> = LazyLock::new(|| re(r#"\bsendEventWithName\s*:\s*@"([^"]+)""#));
static SWIFT_SEND: LazyLock<Regex> =
    LazyLock::new(|| re(r#"\bsendEvent\s*\(\s*withName\s*:\s*"([^"]+)""#));
static JVM_EMIT: LazyLock<Regex> = LazyLock::new(|| re(r#"\.emit\s*\(\s*"([^"]+)"\s*,"#));
/// A `sendEvent(ctx, "x", body)` wrapper call (never its `{`-bodied definition).
static NATIVE_SEND_EVENT: LazyLock<Regex> =
    LazyLock::new(|| re(r#"\bsendEvent\s*\([^;{}]*?"([^"]+)""#));
static OBJC_SEND_CONST: LazyLock<Regex> =
    LazyLock::new(|| re(r"\bsendEventWithName\s*:\s*([A-Za-z_]\w*)\b"));
static SWIFT_SEND_CONST: LazyLock<Regex> =
    LazyLock::new(|| re(r"\bsendEvent\s*\(\s*withName\s*:\s*([A-Za-z_][\w.]*)"));
static JVM_EMIT_CONST: LazyLock<Regex> =
    LazyLock::new(|| re(r"\.emit\s*\(\s*([A-Za-z_][\w.]*)\s*,"));
static NATIVE_SEND_EVENT_CONST: LazyLock<Regex> =
    LazyLock::new(|| re(r"\bsendEvent\s*\(\s*[A-Za-z_][\w.]*\s*,\s*([A-Za-z_][\w.]*)\s*,"));
static JS_LISTEN_ANY: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"\.(?:on|once|addListener)\(\s*['"]([^'"]+)['"]\s*,\s*([A-Za-z_][\w.]*)"#)
});
static JS_LISTEN_INLINE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"\.(?:on|once|addListener)\(\s*['"]([^'"]+)['"]\s*,\s*(?:async\s*)?(?:\([^)]*\)\s*=>|[A-Za-z_$][\w$]*\s*=>|function\s*\()"#,
    )
});
static JS_LISTEN_CONST: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"\.(?:on|once|addListener)\(\s*((?:[A-Za-z_$][\w$]*\.)*[A-Za-z_$][\w$]*)\s*,\s*([A-Za-z_$][\w$.]*)",
    )
});
static CONST_VALUE: LazyLock<Regex> =
    LazyLock::new(|| re(r#"^\s*(?::[^=;]*)?[=:]\s*@?["'`]([^"'`]+)["'`]"#));
static DEFINE_VALUE: LazyLock<Regex> = LazyLock::new(|| re(r#"^\s+@?"([^"]+)""#));
static PLAIN_NAME: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z_$][\w$]*$"));

/// More handlers or dispatchers than this on one event name is too generic
/// to pair without types.
const EVENT_FANOUT: usize = 6;

const JS: &[Language] = &[
    Language::TypeScript,
    Language::JavaScript,
    Language::Tsx,
    Language::Jsx,
];

fn is_constant_kind(k: NodeKind) -> bool {
    matches!(
        k,
        NodeKind::Constant
            | NodeKind::Variable
            | NodeKind::Field
            | NodeKind::Property
            | NodeKind::EnumMember
    )
}

/// The string an event-name constant holds, read off its declaration line:
/// a bare name declared in the same file, or `Owner.NAME` declared inside
/// `Owner`. None unless the declarations agree on one literal.
fn constant_event_name(ctx: &Ctx<'_>, expr: &str, file: &str) -> Option<String> {
    let expr = expr.strip_suffix(".rawValue").unwrap_or(expr);
    let parts: Vec<&str> = expr.split('.').collect();
    let name = *parts.last()?;
    if !PLAIN_NAME.is_match(name) || name == "this" || (parts.len() > 2 && parts[0] == "this") {
        return None;
    }
    let owner = (parts.len() > 1).then(|| parts[parts.len() - 2]);
    let decls: Vec<usize> = match owner {
        None => ctx
            .in_file(file)
            .iter()
            .copied()
            .filter(|&i| ctx.node(i).name == name && is_constant_kind(ctx.node(i).kind))
            .collect(),
        Some(owner) => ctx
            .named(name)
            .filter(|&i| {
                let n = ctx.node(i);
                is_constant_kind(n.kind)
                    && n.qualified_name[..n.qualified_name.len().saturating_sub(name.len())]
                        .split(['.', ':'])
                        .any(|seg| seg == owner)
            })
            .collect(),
    };
    let mut values: Vec<String> = decls
        .into_iter()
        .filter_map(|i| {
            let n = ctx.node(i);
            let line = ctx.lines(&n.file_path, n.start_line, n.start_line)?;
            let rest = &line[line.find(name)? + name.len()..];
            CONST_VALUE
                .captures(rest)
                .or_else(|| DEFINE_VALUE.captures(rest))
                .map(|c| c[1].to_string())
        })
        .collect();
    values.sort();
    values.dedup();
    (values.len() == 1).then(|| values.remove(0))
}

pub fn rn_events(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    if !ctx.has_language(JS) {
        return;
    }
    let constants: RefCell<HashMap<(String, String), Option<String>>> = RefCell::default();
    let constant = |expr: &str, file: &str| -> Option<String> {
        let key = (file.to_string(), expr.to_string());
        if let Some(v) = constants.borrow().get(&key) {
            return v.clone();
        }
        let v = constant_event_name(ctx, expr, file);
        constants.borrow_mut().insert(key, v.clone());
        v
    };
    let mut dispatchers: HashMap<String, Vec<(usize, u32)>> = HashMap::new();
    let mut handlers: HashMap<String, Vec<usize>> = HashMap::new();
    for (file, lang) in &ctx.files {
        let native = matches!(
            lang,
            Language::ObjC | Language::Swift | Language::Java | Language::Kotlin
        );
        if !native && !JS.contains(lang) {
            continue;
        }
        let Some(src) = ctx.source(file) else {
            continue;
        };
        if native && !src.contains("sendEvent") && !src.contains(".emit(") {
            continue;
        }
        if !native
            && !src.contains(".on(")
            && !src.contains(".once(")
            && !src.contains("addListener(")
        {
            continue;
        }
        let lines = LineIndex::new(&src);
        let mut dispatch = |event: String, at: usize| {
            let line = lines.line(at);
            if let Some(d) = ctx.enclosing_fn(file, line) {
                let v = dispatchers.entry(event).or_default();
                if !v.iter().any(|(x, _)| *x == d) {
                    v.push((d, line));
                }
            }
        };
        let literal: &[&LazyLock<Regex>] = match lang {
            Language::ObjC => &[&OBJC_SEND],
            Language::Swift => &[&SWIFT_SEND, &NATIVE_SEND_EVENT],
            Language::Java | Language::Kotlin => &[&JVM_EMIT, &NATIVE_SEND_EVENT],
            _ => &[],
        };
        let named: &[&LazyLock<Regex>] = match lang {
            Language::ObjC => &[&OBJC_SEND_CONST],
            Language::Swift => &[&SWIFT_SEND_CONST],
            Language::Java | Language::Kotlin => &[&JVM_EMIT_CONST, &NATIVE_SEND_EVENT_CONST],
            _ => &[],
        };
        for r in literal {
            for c in r.captures_iter(&src) {
                dispatch(c[1].to_string(), c.get(0).unwrap().start());
            }
        }
        for r in named {
            for c in r.captures_iter(&src) {
                if let Some(event) = constant(&c[1], file) {
                    dispatch(event, c.get(0).unwrap().start());
                }
            }
        }
        if native {
            continue;
        }
        let mut handle = |event: &str, target: usize| {
            let v = handlers.entry(event.to_string()).or_default();
            if !v.contains(&target) {
                v.push(target);
            }
        };
        let in_value = |line: u32| -> Option<usize> {
            ctx.in_file(file)
                .iter()
                .copied()
                .filter(|&i| {
                    let n = ctx.node(i);
                    matches!(n.kind, NodeKind::Constant | NodeKind::Variable)
                        && n.start_line <= line
                        && line <= n.end_line
                })
                .max_by_key(|&i| ctx.node(i).start_line)
        };
        for c in JS_LISTEN_ANY.captures_iter(&src) {
            let arg = &c[2];
            let bare = arg.rsplit('.').next().unwrap_or(arg);
            let line = lines.line(c.get(0).unwrap().start());
            let target = ctx
                .named(bare)
                .find(|&i| matches!(ctx.node(i).kind, NodeKind::Function | NodeKind::Method))
                .or_else(|| ctx.enclosing_fn(file, line))
                .or_else(|| in_value(line));
            if let Some(t) = target {
                handle(&c[1], t);
            }
        }
        for c in JS_LISTEN_INLINE.captures_iter(&src) {
            if let Some(t) = ctx.enclosing_fn(file, lines.line(c.get(0).unwrap().start())) {
                handle(&c[1], t);
            }
        }
        for c in JS_LISTEN_CONST.captures_iter(&src) {
            let Some(event) = constant(&c[1], file) else {
                continue;
            };
            let line = lines.line(c.get(0).unwrap().start());
            let bare = c[2].rsplit('.').next().unwrap_or(&c[2]).to_string();
            let named = ctx.in_file(file).iter().copied().find(|&i| {
                let n = ctx.node(i);
                matches!(n.kind, NodeKind::Function | NodeKind::Method) && n.name == bare
            });
            if let Some(t) = named.or_else(|| ctx.enclosing_fn(file, line)) {
                handle(&event, t);
            }
        }
    }
    let mut sink = Sink::new(out);
    let mut events: Vec<&String> = dispatchers.keys().collect();
    events.sort();
    for event in events {
        let ds = &dispatchers[event];
        let Some(hs) = handlers.get(event) else {
            continue;
        };
        if ds.len() > EVENT_FANOUT || hs.len() > EVENT_FANOUT {
            continue;
        }
        for &(d, line) in ds {
            for &h in hs {
                sink.push(d, h, line, event);
            }
        }
    }
}

/// Suffixes React Native's native view classes add to a component's name.
const FABRIC_SUFFIXES: &[&str] = &["", "View", "ViewManager", "ComponentView", "Manager"];
static CODEGEN_COMPONENT: LazyLock<Regex> =
    LazyLock::new(|| re(r#"codegenNativeComponent\s*(?:<[^>]*>)?\s*\(\s*['"]([A-Za-z_]\w*)['"]"#));

/// A Fabric component spec (`codegenNativeComponent('RNSScreen')`) → the
/// native class implementing it.
pub fn fabric_native(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    let mut sink = Sink::new(out);
    for (file, lang) in &ctx.files {
        if !JS.contains(lang) {
            continue;
        }
        let Some(src) = ctx.source(file) else {
            continue;
        };
        if !src.contains("codegenNativeComponent") {
            continue;
        }
        let lines = LineIndex::new(&src);
        for c in CODEGEN_COMPONENT.captures_iter(&src) {
            let line = lines.line(c.get(0).unwrap().start());
            let from = ctx
                .in_file(file)
                .iter()
                .copied()
                .filter(|&i| {
                    let n = ctx.node(i);
                    n.kind != NodeKind::File && n.start_line <= line && line <= n.end_line
                })
                .min_by_key(|&i| ctx.node(i).end_line - ctx.node(i).start_line)
                .or_else(|| {
                    ctx.in_file(file)
                        .iter()
                        .copied()
                        .find(|&i| ctx.node(i).kind == NodeKind::File)
                });
            let Some(from) = from else { continue };
            for suffix in FABRIC_SUFFIXES {
                let name = format!("{}{suffix}", &c[1]);
                let class = ctx.named(&name).find(|&i| {
                    let n = ctx.node(i);
                    n.kind == NodeKind::Class
                        && matches!(
                            n.language,
                            Language::ObjC
                                | Language::Cpp
                                | Language::Java
                                | Language::Kotlin
                                | Language::Swift
                        )
                });
                if let Some(t) = class {
                    sink.push(from, t, line, &c[1]);
                }
            }
        }
    }
}
