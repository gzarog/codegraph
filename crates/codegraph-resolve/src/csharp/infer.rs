//! A C# receiver's type: its local declaration in the calling method
//! (`inferLocalReceiverType`'s C# patterns), else a field or property of the
//! class around the call or of one it derives from (`inferMemberReceiverType`),
//! and what a type parameter is bounded by (`typeParameterBound`).

use super::scope::is_type_kind;
use crate::cpp::ctx::memo;
use crate::cpp::{js, Ctx, Ref};
use crate::symbols::SymId;
use codegraph_core::{Language, NodeKind};
use std::collections::HashMap;
use std::rc::Rc;

const NM: u16 = memo::NM;

/// `TYPE_ARGS`: optional type arguments (one level of nesting) and array ranks.
const TYPE_ARGS: &str = r"(?:<[^;=(){}<>]*(?:<[^;=(){}<>]*>[^;=(){}<>]*)*>)?\s*(?:\[[\s,]*\]\s*)*";

const NON_TYPE_RECEIVER_TOKENS: &[&str] = &[
    "this",
    "self",
    "super",
    "new",
    "return",
    "await",
    "yield",
    "typeof",
    "null",
    "nil",
    "None",
    "true",
    "false",
    "True",
    "False",
    "undefined",
];

/// `normalizeInferredTypeName`.
pub fn normalize_inferred_type_name(ctx: &Ctx, raw: &str) -> Option<String> {
    let cleaned = js::replace_with(&ctx.re(r"<[^>]*>"), raw, "");
    let cleaned = cleaned.replace(['&', '*'], "");
    let seg = cleaned
        .trim()
        .split(['.', ':'])
        .filter(|s| !s.is_empty())
        .last()?;
    (!NON_TYPE_RECEIVER_TOKENS.contains(&seg)).then(|| seg.to_string())
}

/// `localReceiverTypePatterns('csharp', r)`.
fn local_patterns(ctx: &Ctx, r: &str) -> Vec<Rc<fancy_regex::Regex>> {
    vec![
        ctx.re(&format!(r"\b{r}\b\s*=\s*new\s+([A-Za-z_][\w.]*)")),
        ctx.re(&format!(
            r"\b([A-Z][\w.]*)\s*{TYPE_ARGS}\??\s+{r}\b\s*[=;,)]"
        )),
        ctx.re(&format!(
            r"\bforeach\s*\(\s*([A-Z][\w.]*)\s*{TYPE_ARGS}\??\s+{r}\s+in\b"
        )),
    ]
}

/// `enclosingScopeStartLine`: 1-based start of the tightest function or method around the call.
fn enclosing_scope_start(ctx: &Ctx, r: &Ref) -> u32 {
    let mut start = 1;
    for &n in ctx.in_file(&r.file) {
        let s = ctx.sym(n);
        if !matches!(s.kind, NodeKind::Function | NodeKind::Method) || s.language != r.lang {
            continue;
        }
        if s.start_line <= r.line && s.end_line >= r.line && s.start_line >= start {
            start = s.start_line;
        }
    }
    start
}

/// `inferLocalReceiverType`, for a C# receiver.
pub fn infer_local_receiver_type(ctx: &Ctx, receiver: &str, r: &Ref) -> Option<String> {
    let patterns = local_patterns(ctx, &js::escape(receiver));
    let src = ctx.source(&r.file)?;
    let lines = &src.lines;
    if lines.is_empty() {
        return None;
    }
    let call = (r.line as usize).saturating_sub(1).min(lines.len() - 1);
    let start = (enclosing_scope_start(ctx, r) as usize).saturating_sub(1);
    let mut i = call as i64;
    while i >= start as i64 {
        let line = &lines[i as usize];
        i -= 1;
        if line.is_empty() || line.len() > 10_000 {
            continue;
        }
        for re in &patterns {
            if let Some(m) = js::caps(re, line) {
                let g = js::group(&m, 1);
                if g.is_empty() {
                    continue;
                }
                if let Some(t) = normalize_inferred_type_name(ctx, g) {
                    return Some(t);
                }
            }
        }
    }
    None
}

const MEMBER_TYPE_NON_TYPES: &[&str] = &[
    "return",
    "new",
    "case",
    "throw",
    "else",
    "in",
    "out",
    "ref",
    "params",
    "await",
    "yield",
    "is",
    "as",
    "using",
    "var",
    "val",
    "goto",
    "nameof",
    "typeof",
    "sizeof",
    "default",
    "when",
    "where",
    "get",
    "set",
    "init",
    "class",
    "interface",
    "enum",
    "struct",
    "record",
    "object",
    "namespace",
    "package",
    "import",
    "extends",
    "implements",
    "fun",
    "static",
    "final",
    "abstract",
    "sealed",
    "override",
    "virtual",
    "delegate",
    "event",
    "permits",
    "throws",
];

/// `MEMBER_CLASS_KINDS` (no `record` kind is indexed).
fn is_member_class_kind(k: NodeKind) -> bool {
    is_type_kind(k)
}

/// `inferMemberReceiverType`: the type a C# receiver has as a member of the class around the call.
pub fn infer_member_receiver_type(ctx: &Ctx, receiver: &str, r: &Ref) -> Option<String> {
    let name = receiver
        .strip_prefix("this.")
        .or_else(|| receiver.strip_prefix("self."))
        .unwrap_or(receiver);
    if !js::is_ident(name) {
        return None;
    }
    let mut cls: Option<SymId> = None;
    let mut func: Option<SymId> = None;
    for &n in ctx.in_file(&r.file) {
        let s = ctx.sym(n);
        if s.language != r.lang || s.start_line > r.line || s.end_line < r.line {
            continue;
        }
        if is_member_class_kind(s.kind) && cls.is_none_or(|c| s.start_line >= ctx.sym(c).start_line)
        {
            cls = Some(n);
        } else if matches!(s.kind, NodeKind::Method | NodeKind::Function)
            && func.is_none_or(|f| s.start_line >= ctx.sym(f).start_line)
        {
            func = Some(n);
        }
    }
    let found = member_type_through_hierarchy(ctx, cls?, name)?;
    if func.is_some_and(|f| binds_name_itself(ctx, f, name)) {
        None
    } else {
        Some(found)
    }
}

/// `bindsNameItself`: whether a function body binds `name` itself.
fn binds_name_itself(ctx: &Ctx, f: SymId, name: &str) -> bool {
    ctx.memo(NM, format!("cs-binds\0{f}\0{name}"), || {
        let s = ctx.sym(f);
        let Some(src) = ctx.source(&s.file) else {
            return false;
        };
        let from = (s.start_line as usize).saturating_sub(1).min(src.lines.len());
        let to = (s.end_line as usize).min(src.lines.len()).max(from);
        let body = src.lines[from..to].join("\n");
        let r = js::escape(name);
        js::test(
            &ctx.re(&format!(
                r"\b(?:var|val|let|out\s+[\w.<>?]+|foreach\s*\(\s*[\w.<>?,\s]+?)\s+{r}\b|\bfor\s*\([^;)]*\s{r}\s*:|\b{r}\s*=>|[(,]\s*{r}\s*(?:,[^()]*)?\)\s*=>|\b{r}\s*(?:,[^{{}}]*)?->"
            )),
            &body,
        )
    })
}

/// `memberTypeThroughHierarchy`: the type `cls` or one of its supertypes declares a member `name` with.
fn member_type_through_hierarchy(ctx: &Ctx, cls: SymId, name: &str) -> Option<String> {
    ctx.memo(NM, format!("cs-mth\0{cls}\0{name}"), || {
        let mut seen: Vec<SymId> = Vec::new();
        let mut queue: std::collections::VecDeque<(SymId, HashMap<String, String>)> =
            std::collections::VecDeque::from([(cls, HashMap::new())]);
        while let Some((ty, args)) = queue.pop_front() {
            if seen.len() >= 8 {
                break;
            }
            if seen.contains(&ty) {
                continue;
            }
            seen.push(ty);
            if let Some(found) = class_member_type(ctx, ty, name) {
                if ty == cls {
                    return Some(found);
                }
                if let Some(given) = args.get(&found) {
                    return Some(given.clone());
                }
                return Some(match type_parameter_bound_in(ctx, &found, &[ty]) {
                    Bound::NotAParameter => found,
                    Bound::Unbounded => "object".to_string(),
                    Bound::Bound(b) => b,
                });
            }
            let lang = ctx.sym(ty).language;
            for sup in class_head_supertypes(ctx, ty).iter() {
                let given: Vec<String> = head_type_arguments(ctx, ty, sup)
                    .into_iter()
                    .map(|a| args.get(&a).cloned().unwrap_or(a))
                    .collect();
                for &d in ctx.named(sup) {
                    let s = ctx.sym(d);
                    if s.language != lang || !is_member_class_kind(s.kind) {
                        continue;
                    }
                    let params = declared_class_type_parameters(ctx, d);
                    let map: HashMap<String, String> = params
                        .iter()
                        .enumerate()
                        .filter_map(|(i, p)| {
                            given
                                .get(i)
                                .filter(|a| !a.is_empty())
                                .map(|a| (p.clone(), a.clone()))
                        })
                        .collect();
                    queue.push_back((d, map));
                }
            }
        }
        None
    })
}

/// `angleArguments`: the first `<…>` of a declaration's head, split at its top-level commas.
fn angle_arguments(text: &str) -> Vec<String> {
    let Some(open) = text.find('<') else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for ch in text[open + 1..].chars() {
        if ch == '<' {
            depth += 1;
        } else if ch == '>' {
            let was = depth;
            depth -= 1;
            if was == 0 {
                break;
            }
        }
        if ch == ',' && depth == 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
    }
    out.push(cur);
    out.into_iter().map(|a| a.trim().to_string()).collect()
}

fn lines_of(ctx: &Ctx, n: SymId, before: usize, after: usize) -> String {
    let s = ctx.sym(n);
    let Some(src) = ctx.source(&s.file) else {
        return String::new();
    };
    let from = (s.start_line as usize)
        .saturating_sub(1 + before)
        .min(src.lines.len());
    let to = (s.start_line as usize + after)
        .min(src.lines.len())
        .max(from);
    src.lines[from..to].join(" ")
}

/// `declaredTypeParameters` (member walk): the type parameters a class declares.
fn declared_class_type_parameters(ctx: &Ctx, d: SymId) -> Vec<String> {
    let head = lines_of(ctx, d, 0, 3);
    let name = &ctx.sym(d).name;
    let Some(at) = js::search(&ctx.re(&format!(r"\b{}\s*<", js::escape(name))), &head) else {
        return Vec::new();
    };
    let param = ctx.re(r"([A-Za-z_]\w*)\s*(?:extends\b.*|:.*)?$");
    let variance = ctx.re(r"^(?:in|out|reified)\s+");
    angle_arguments(&head[at..])
        .iter()
        .map(|p| {
            let p = js::replace_with(&variance, p, "");
            js::caps(&param, &p)
                .map(|c| js::group(&c, 1).to_string())
                .unwrap_or_default()
        })
        .collect()
}

/// `headTypeArguments`: the type arguments a class's head gives a supertype, as simple names.
fn head_type_arguments(ctx: &Ctx, cls: SymId, sup: &str) -> Vec<String> {
    let head = lines_of(ctx, cls, 0, 8);
    let head = head.split('{').next().unwrap_or("");
    let Some(at) = js::search(
        &ctx.re(&format!(
            r"(?:[:,]|\bextends|\bimplements)\s*(?:[\w.]+\.)?{}\s*<",
            js::escape(sup)
        )),
        head,
    ) else {
        return Vec::new();
    };
    let tail = ctx.re(r"<[\s\S]*$");
    angle_arguments(&head[at..])
        .iter()
        .map(|a| {
            let a = js::replace_with(&tail, a, "");
            a.rsplit('.').next().unwrap_or("").trim().to_string()
        })
        .collect()
}

/// `classHeadSupertypes`: the simple names a C# class declaration's head extends or implements.
pub fn class_head_supertypes(ctx: &Ctx, cls: SymId) -> Rc<Vec<String>> {
    ctx.memo(NM, format!("cs-chs\0{cls}"), || {
        let text = js::replace_with(
            &ctx.re(r"//[^\n]*|/\*.*?\*/"),
            &lines_of(ctx, cls, 0, 8),
            " ",
        );
        let mut depth = 0i32;
        let mut head = String::new();
        for ch in text.chars() {
            if ch == '{' && depth == 0 {
                break;
            }
            match ch {
                '<' | '(' => depth += 1,
                '>' | ')' => depth = (depth - 1).max(0),
                _ if depth == 0 => head.push(ch),
                _ => {}
            }
        }
        let clause = js::caps(&ctx.re(r"\b(?:extends|implements)\b([\s\S]*)$"), &head)
            .map(|c| js::group(&c, 1).to_string())
            .or_else(|| {
                js::caps(
                    &ctx.re(r"\b(?:class|interface|struct|record|object)\s+\w+[^:]*:([\s\S]*)$"),
                    &head,
                )
                .map(|c| js::group(&c, 1).to_string())
            })
            .unwrap_or_default();
        let clause = js::replace_with(&ctx.re(r"\bwhere\b[\s\S]*$"), &clause, "");
        Rc::new(
            js::all(&ctx.re(r"([A-Z]\w*)\s*(?=,|$|\bimplements\b)"), &clause)
                .iter()
                .map(|m| js::group(m, 1).to_string())
                .collect(),
        )
    })
}

/// The lines of a class at its body's own brace depth (or its header), comments and strings dropped.
fn class_member_lines(ctx: &Ctx, cls: SymId) -> Rc<Vec<(String, i32)>> {
    ctx.memo(NM, format!("cs-cml\0{cls}"), || {
        let s = ctx.sym(cls);
        let Some(src) = ctx.source(&s.file) else {
            return Rc::new(Vec::new());
        };
        let strings = ctx.re(r#""(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'"#);
        let block = ctx.re(r"/\*.*?\*/");
        let tail = ctx.re(r"//.*$");
        let mut out = Vec::new();
        let mut depth = 0i32;
        let mut in_comment = false;
        for line in s.start_line..=s.end_line {
            let mut raw: &str = src
                .lines
                .get((line as usize).wrapping_sub(1))
                .map(String::as_str)
                .unwrap_or("");
            if in_comment {
                let Some(close) = raw.find("*/") else {
                    continue;
                };
                raw = &raw[close + 2..];
                in_comment = false;
            }
            let mut text = js::replace_with(
                &tail,
                &js::replace_with(&block, &js::replace_with(&strings, raw, "\"\""), " "),
                "",
            );
            if let Some(open) = text.find("/*") {
                text.truncate(open);
                in_comment = true;
            }
            if depth <= 1 && !text.trim().is_empty() {
                out.push((text.clone(), depth));
            }
            for ch in text.chars() {
                if ch == '{' {
                    depth += 1;
                } else if ch == '}' {
                    depth = (depth - 1).max(0);
                }
            }
        }
        Rc::new(out)
    })
}

/// `classMemberType`: the type a class declares a member `name` with.
fn class_member_type(ctx: &Ctx, cls: SymId, name: &str) -> Option<String> {
    ctx.memo(NM, format!("cs-cmt\0{cls}\0{name}"), || {
        let r = js::escape(name);
        let lang = ctx.sym(cls).language;
        let pattern = if matches!(lang, Language::Kotlin | Language::Swift) {
            ctx.re(&format!(r"\b(?:val|var|let)\s+{r}\s*:\s*([A-Z][\w.]*)"))
        } else {
            ctx.re(&format!(
                r"(?:^|[\s(,])([A-Za-z_][\w.]*)\s*{TYPE_ARGS}\??\s+{r}\s*(?:[=;,)]|\{{)"
            ))
        };
        for (text, depth) in class_member_lines(ctx, cls).iter() {
            if !text.contains(name) {
                continue;
            }
            let Some(m) = js::caps(&pattern, text) else {
                continue;
            };
            let at = m.get(0).unwrap().start();
            let before = &text[..at];
            let in_parens =
                *depth == 1 && before.matches('(').count() > before.matches(')').count();
            let ty = js::group(&m, 1);
            if !in_parens && !MEMBER_TYPE_NON_TYPES.contains(&ty) {
                return normalize_inferred_type_name(ctx, ty);
            }
        }
        None
    })
}

/// What `typeParameterBound` says of a name.
pub enum Bound {
    /// `undefined`: not a type parameter there.
    NotAParameter,
    /// `null`: a type parameter with no named bound.
    Unbounded,
    Bound(String),
}

/// `typeParameterBound`: what a type parameter of the class or method around the call is bounded by.
pub fn type_parameter_bound(ctx: &Ctx, type_name: &str, r: &Ref) -> Bound {
    let around: Vec<SymId> = ctx
        .in_file(&r.file)
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            s.start_line <= r.line
                && s.end_line >= r.line
                && (is_member_class_kind(s.kind)
                    || matches!(s.kind, NodeKind::Method | NodeKind::Function))
        })
        .collect();
    type_parameter_bound_in(ctx, type_name, &around)
}

fn type_parameter_bound_in(ctx: &Ctx, type_name: &str, decls: &[SymId]) -> Bound {
    if !js::test(&ctx.re(r"^[A-Z]\w*$"), type_name) {
        return Bound::NotAParameter;
    }
    let t = type_name;
    let mut declared = false;
    for &n in decls {
        let s = ctx.sym(n);
        let head = match ctx.source(&s.file) {
            Some(src) => {
                let from = (s.start_line as usize)
                    .saturating_sub(1)
                    .min(src.lines.len());
                let to = (s.start_line as usize + 5).min(src.lines.len()).max(from);
                src.lines[from..to].join(" ")
            }
            None => String::new(),
        };
        let head = head.split('{').next().unwrap_or("");
        let bound = js::caps(
            &ctx.re(&format!(
                r"[<,]\s*(?:in\s+|out\s+|reified\s+)?{t}\s*(?:extends|:)\s*([A-Z][\w.]*)"
            )),
            head,
        )
        .or_else(|| {
            js::caps(
                &ctx.re(&format!(r"\bwhere\s+{t}\s*:\s*([A-Z][\w.]*)")),
                head,
            )
        })
        .map(|c| js::group(&c, 1).to_string());
        if let Some(b) = bound {
            return Bound::Bound(b.rsplit('.').next().unwrap_or(&b).to_string());
        }
        if js::test(
            &ctx.re(&format!(r"[<,]\s*(?:in\s+|out\s+|reified\s+)?{t}\s*[,>]")),
            head,
        ) {
            declared = true;
        }
    }
    if declared {
        Bound::Unbounded
    } else {
        Bound::NotAParameter
    }
}
