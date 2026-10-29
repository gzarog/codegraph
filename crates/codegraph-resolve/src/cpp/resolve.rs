//! A C or C++ reference end to end, as upstream's ReferenceResolver runs it:
//! resolveOneIn's gates around resolveOneInner's strategies, and
//! name-matcher.ts's matchReference.

use super::nm::*;
use super::{constructor, includes, macro_vis};

const C_BUILT_INS: &[&str] = &[
    "printf",
    "fprintf",
    "sprintf",
    "snprintf",
    "scanf",
    "fscanf",
    "sscanf",
    "malloc",
    "calloc",
    "realloc",
    "free",
    "memcpy",
    "memmove",
    "memset",
    "memcmp",
    "memchr",
    "strlen",
    "strcpy",
    "strncpy",
    "strcat",
    "strncat",
    "strcmp",
    "strncmp",
    "strstr",
    "strchr",
    "strrchr",
    "strtok",
    "strdup",
    "fopen",
    "fclose",
    "fread",
    "fwrite",
    "fgets",
    "fputs",
    "fputc",
    "fgetc",
    "feof",
    "ferror",
    "fflush",
    "fseek",
    "ftell",
    "rewind",
    "exit",
    "abort",
    "atexit",
    "atoi",
    "atol",
    "atof",
    "strtol",
    "strtoul",
    "strtod",
    "qsort",
    "bsearch",
    "abs",
    "labs",
    "rand",
    "srand",
    "sin",
    "cos",
    "tan",
    "sqrt",
    "pow",
    "log",
    "log10",
    "exp",
    "ceil",
    "floor",
    "fabs",
    "time",
    "clock",
    "difftime",
    "mktime",
    "localtime",
    "gmtime",
    "strftime",
    "asctime",
    "assert",
    "errno",
    "perror",
    "remove",
    "rename",
    "tmpfile",
    "tmpnam",
    "getenv",
    "system",
    "signal",
    "raise",
    "setjmp",
    "longjmp",
    "va_start",
    "va_end",
    "va_arg",
    "va_copy",
    "NULL",
    "EOF",
    "BUFSIZ",
    "FILENAME_MAX",
    "RAND_MAX",
    "EXIT_SUCCESS",
    "EXIT_FAILURE",
    "size_t",
    "ptrdiff_t",
    "wchar_t",
    "intptr_t",
    "uintptr_t",
    "int8_t",
    "int16_t",
    "int32_t",
    "int64_t",
    "uint8_t",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "FILE",
    "stat",
    "lstat",
    "fstat",
    "open",
    "close",
    "read",
    "write",
    "pipe",
    "fork",
    "exec",
    "waitpid",
    "getpid",
    "getppid",
    "kill",
    "sleep",
    "usleep",
    "pthread_create",
    "pthread_join",
    "pthread_mutex_lock",
    "pthread_mutex_unlock",
    "dlopen",
    "dlsym",
    "dlclose",
];

const CPP_BUILT_INS: &[&str] = &[
    "cout",
    "cin",
    "cerr",
    "clog",
    "endl",
    "flush",
    "ws",
    "std",
    "nullptr",
    "true",
    "false",
    "this",
    "sizeof",
    "alignof",
    "typeid",
    "static_cast",
    "dynamic_cast",
    "reinterpret_cast",
    "const_cast",
    "make_unique",
    "make_shared",
    "make_pair",
    "move",
    "forward",
    "swap",
];

/// `hasAnyPossibleMatch`: does any node carry the name, or a part of it a strategy reads?
pub fn has_any_possible_match(ctx: &Ctx, name: &str) -> bool {
    let known = |n: &str| !ctx.named(n).is_empty();
    if known(name) {
        return true;
    }
    let capitalized = |s: &str| {
        let mut c = s.chars();
        c.next()
            .map(|f| f.to_uppercase().chain(c).collect::<String>())
            .unwrap_or_default()
    };
    if let Some(dot) = name.find('.').filter(|&d| d > 0) {
        let (receiver, member) = (&name[..dot], &name[dot + 1..]);
        if known(receiver) || known(member) || known(&capitalized(receiver)) {
            return true;
        }
        if let Some(last) = name.rfind('.').filter(|&l| l > dot) {
            let tail = &name[last + 1..];
            if !tail.is_empty() && known(tail) {
                return true;
            }
        }
    }
    if let Some(colon) = name.find("::").filter(|&c| c > 0) {
        let (receiver, member) = (&name[..colon], &name[colon + 2..]);
        if known(receiver) || known(member) {
            return true;
        }
        if let Some(last) = name.rfind("::").filter(|&l| l > colon) {
            let tail = &name[last + 2..];
            if !tail.is_empty() && known(tail) {
                return true;
            }
        }
    }
    for sep in [':', '$'] {
        if sep == ':' && name.contains("::") {
            continue;
        }
        if let Some(at) = name.find(sep).filter(|&a| a > 0) {
            let (receiver, member) = (&name[..at], &name[at + 1..]);
            if known(member) || known(receiver) || known(&capitalized(receiver)) {
                return true;
            }
        }
    }
    if let Some(slash) = name.rfind('/').filter(|&s| s > 0) {
        if known(&name[slash + 1..]) {
            return true;
        }
    }
    false
}

/// isBuiltInOrExternal's C/C++ branch.
fn is_built_in_or_external(ctx: &Ctx, r: &Ref) -> bool {
    let scoped = r.name.strip_prefix("::").unwrap_or(&r.name);
    if scoped.starts_with("std::") {
        return true;
    }
    if C_BUILT_INS.contains(&scoped) || CPP_BUILT_INS.contains(&scoped) {
        return !has_any_possible_match(ctx, scoped);
    }
    false
}

fn best_of(candidates: Vec<Match>) -> Option<Match> {
    let mut it = candidates.into_iter();
    let mut best = it.next()?;
    for c in it {
        if c.confidence > best.confidence {
            best = c;
        }
    }
    Some(best)
}

/// `resolveOneInner`, for a C or C++ reference.
fn resolve_inner(ctx: &Ctx, r: &Ref) -> Option<Match> {
    if constructor::is_constructor_ref(ctx, r) {
        return constructor::match_constructor(ctx, r);
    }
    if supertypes::is_supertype_ref(r) {
        return supertypes::match_supertype(ctx, r);
    }
    if is_built_in_or_external(ctx, r) {
        return None;
    }
    let existence = r.name.strip_prefix("::").unwrap_or(&r.name);
    if !has_any_possible_match(ctx, existence) {
        return None;
    }
    if r.kind == ReferenceKind::FunctionRef {
        if let Some(via) = gate_language_match(ctx, includes::resolve_via_import(ctx, r), r) {
            if matches!(ctx.kind(via.target), NodeKind::Function | NodeKind::Method) {
                return Some(via);
            }
        }
        return gate_language_match(ctx, match_function_ref(ctx, r), r);
    }
    let mut candidates = Vec::new();
    if let Some(import) = gate_language_match(ctx, includes::resolve_via_import(ctx, r), r) {
        if import.confidence >= 0.9 {
            return Some(import);
        }
        candidates.push(import);
    }
    let named = gate_language_match(ctx, match_reference(ctx, r), r);
    if let Some(m) = named.filter(|m| is_visible_across_files(ctx, m.target, r)) {
        candidates.push(m);
    }
    best_of(candidates)
}

/// `matchReferenceInner`, for a C or C++ reference.
fn match_reference_inner(ctx: &Ctx, r: &Ref) -> Option<Match> {
    if r.kind == ReferenceKind::FunctionRef {
        return match_function_ref(ctx, r);
    }
    if r.is_cpp() && r.kind == ReferenceKind::Calls && js::is_ident(&r.name) {
        match match_cpp_unspelled_receiver_call(ctx, r) {
            Tri::Found(m) => return Some(m),
            Tri::Null => return None,
            Tri::Undefined => {}
        }
        match match_cpp_member_chain_call(ctx, r) {
            Tri::Found(m) => return Some(m),
            Tri::Null => return None,
            Tri::Undefined => {}
        }
    }
    if let Some(m) = includes::match_by_file_path(ctx, r) {
        return Some(m);
    }
    if let Some(m) = match_by_qualified_name(ctx, r) {
        return Some(m);
    }
    if let Some(m) = match_cpp_call_chain(ctx, r) {
        return Some(m);
    }
    if r.is_cpp() && r.kind == ReferenceKind::Instantiates {
        match match_cpp_construction(ctx, r) {
            Tri::Found(m) => return Some(m),
            Tri::Null => return None,
            Tri::Undefined => {}
        }
    }
    if let Some(m) = match_method_call(ctx, r) {
        return Some(m);
    }
    if let Some(m) = match_by_exact_name(ctx, r) {
        return Some(m);
    }
    if let Some(m) = match_fuzzy(ctx, r) {
        return Some(m);
    }
    if r.name.contains("::") {
        return match_cpp_macro_namespaced(ctx, r);
    }
    None
}

/// `matchReference`, for a C or C++ reference.
pub fn match_reference(ctx: &Ctx, r: &Ref) -> Option<Match> {
    let result = gate_language_match(ctx, match_reference_inner(ctx, r), r)?;
    if result.target == r.from && is_collapsed_non_recursion(ctx, r) {
        return None;
    }
    if r.is_cpp() && r.kind == ReferenceKind::Calls {
        let t = ctx.sym(result.target);
        let c = ctx.sym(r.from);
        let own = t.file == c.file
            && t.start_line >= c.start_line
            && t.start_line <= c.end_line.max(c.start_line);
        if !own && is_cpp_local_callable_call(ctx, r) {
            return None;
        }
    }
    if r.is_c() && r.kind == ReferenceKind::Calls && ctx.kind(result.target) == NodeKind::Method {
        return None;
    }
    if result.target == r.from
        && matches!(r.kind, ReferenceKind::Extends | ReferenceKind::Implements)
    {
        return other_supertype_named(ctx, r);
    }
    if result.target == r.from
        && r.kind == ReferenceKind::Calls
        && matches!(
            ctx.kind(r.from),
            NodeKind::Variable | NodeKind::Constant | NodeKind::Field | NodeKind::Property
        )
    {
        return None;
    }
    Some(retarget_self_overload(ctx, result, r))
}

/// `otherSupertypeNamed`: the supertype an inheritance ref names when the name is the declaring type's own.
pub fn other_supertype_named(ctx: &Ctx, r: &Ref) -> Option<Match> {
    let name = r.name.rsplit("::").next().unwrap_or(&r.name);
    let name = name.rsplit('.').next().unwrap_or(name);
    let line = ctx.line(&r.file, r.line);
    let written = js::caps(
        &ctx.re(&format!(r"^\s*((?:[\w$]+\.)+){}\b", js::escape(name))),
        js::slice_from(&line, r.column as usize),
    );
    let qualifier = written.map(|w| {
        let q = js::group(&w, 1);
        q[..q.len() - 1].to_string()
    });
    let owner_path = |n: SymId| -> String {
        let s = ctx.sym(n);
        let cut = s.qualified_name.rfind("::").filter(|&c| c > 0);
        cut.map(|c| s.qualified_name[..c].replace("::", "."))
            .unwrap_or_default()
    };
    let candidates: Vec<SymId> = ctx
        .named(name)
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            n != r.from
                && matches!(
                    s.kind,
                    NodeKind::Class
                        | NodeKind::Struct
                        | NodeKind::Interface
                        | NodeKind::Trait
                        | NodeKind::Protocol
                        | NodeKind::Enum
                        | NodeKind::Union
                        | NodeKind::TypeAlias
                        | NodeKind::Component
                        | NodeKind::Module
                        | NodeKind::Namespace
                )
                && same_language_family(s.language, r.lang)
                && qualifier.as_deref().is_none_or(|q| {
                    let p = owner_path(n);
                    q.is_empty() || p == q || p.ends_with(&format!(".{q}"))
                })
        })
        .collect();
    let mut pool = candidates;
    if pool.len() > 1 {
        let same: Vec<SymId> = pool
            .iter()
            .copied()
            .filter(|&n| ctx.sym(n).file == r.file)
            .collect();
        if !same.is_empty() {
            pool = same;
        }
    }
    if pool.len() > 1 && r.kind == ReferenceKind::Implements {
        let conformable: Vec<SymId> = pool
            .iter()
            .copied()
            .filter(|&n| {
                matches!(
                    ctx.kind(n),
                    NodeKind::Protocol | NodeKind::Interface | NodeKind::Trait
                )
            })
            .collect();
        if !conformable.is_empty() {
            pool = conformable;
        }
    }
    (pool.len() == 1).then(|| Match::new(pool[0], "qualified-name", 0.8))
}

/// `gateTargetKind`, for a C or C++ reference.
pub fn gate_target_kind(ctx: &Ctx, m: Option<Match>, r: &Ref) -> Option<Match> {
    let m = m?;
    if r.kind == ReferenceKind::Calls && is_define_constant(ctx, m.target) {
        return None;
    }
    if r.kind == ReferenceKind::Imports {
        let t = ctx.sym(m.target);
        if t.kind == NodeKind::Import && t.file == r.file {
            return None;
        }
        return (!matches!(
            t.kind,
            NodeKind::Property
                | NodeKind::Field
                | NodeKind::Method
                | NodeKind::EnumMember
                | NodeKind::Parameter
        ))
        .then_some(m);
    }
    if !matches!(r.kind, ReferenceKind::Extends | ReferenceKind::Implements) {
        return Some(m);
    }
    let t = ctx.sym(m.target);
    let supertype = matches!(
        t.kind,
        NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Interface
            | NodeKind::Trait
            | NodeKind::Protocol
            | NodeKind::Enum
            | NodeKind::Union
            | NodeKind::TypeAlias
            | NodeKind::Component
            | NodeKind::Module
            | NodeKind::Namespace
    ) && !(t.language == Language::Scala && t.kind == NodeKind::Module);
    // (sameNamedTypeOfValue answers only for TypeScript.)
    supertype.then_some(m)
}

/// The declarations around a line of a file whose kind declares type parameters.
pub fn enclosing_declarations(ctx: &Ctx, file: &str, line: u32) -> Vec<SymId> {
    ctx.in_file(file)
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            matches!(
                s.kind,
                NodeKind::Class
                    | NodeKind::Struct
                    | NodeKind::Interface
                    | NodeKind::Trait
                    | NodeKind::Protocol
                    | NodeKind::Enum
                    | NodeKind::TypeAlias
                    | NodeKind::Function
                    | NodeKind::Method
                    | NodeKind::Union
                    | NodeKind::Module
            ) && s.start_line <= line
                && s.end_line >= line
        })
        .collect()
}

pub fn is_opener(c: u8) -> bool {
    matches!(c, b'[' | b'(' | b'<')
}

pub fn is_closer(c: u8, previous: u8) -> bool {
    c == b']' || c == b')' || (c == b'>' && previous != b'-' && previous != b'=')
}

/// type-parameters.ts `declaredTypeParameters` for a C++ declaration: each `template <…>` parameter on the lines above it, with its default.
fn declared_type_parameters(ctx: &Ctx, n: SymId) -> Rc<Vec<(String, Option<String>)>> {
    ctx.memo(memo::NM, format!("tp\0{n}"), || {
        let mut names: Vec<(String, Option<String>)> = Vec::new();
        let s = ctx.sym(n);
        if s.language != Language::Cpp {
            return Rc::new(names);
        }
        let Some(src) = ctx.source(&s.file) else {
            return Rc::new(names);
        };
        let from = (s.start_line as usize)
            .saturating_sub(3)
            .min(src.lines.len());
        let to = (s.start_line as usize).min(src.lines.len());
        let above = src.lines[from..to].join("\n");
        let b = above.as_bytes();
        for m in js::all(&ctx.re(r"\btemplate\s*<"), &above) {
            let open = m.get(0).unwrap().end() - 1;
            // matchingBracket
            let mut depth = 0i32;
            let mut close = None;
            for i in open..b.len() {
                let c = b[i];
                if c == b'{' || c == b';' {
                    break;
                }
                if is_opener(c) {
                    depth += 1;
                } else if is_closer(c, if i > 0 { b[i - 1] } else { 0 }) {
                    depth -= 1;
                    if depth == 0 {
                        if c == b'>' {
                            close = Some(i);
                        }
                        break;
                    }
                }
            }
            let Some(close) = close else { continue };
            let inner = &above[open + 1..close];
            let ib = inner.as_bytes();
            let mut items = Vec::new();
            let mut depth = 0i32;
            let mut start = 0;
            for i in 0..ib.len() {
                let c = ib[i];
                if is_opener(c) {
                    depth += 1;
                } else if is_closer(c, if i > 0 { ib[i - 1] } else { 0 }) {
                    depth -= 1;
                } else if c == b',' && depth == 0 {
                    items.push(&inner[start..i]);
                    start = i + 1;
                }
            }
            items.push(&inner[start..]);
            for item in items {
                let at = item.find('=');
                let head = at.map_or(item, |a| &item[..a]);
                let Some(p) = js::caps(&ctx.re(r"([A-Za-z_]\w*)\s*$"), head) else {
                    continue;
                };
                let param = js::group(&p, 1);
                if matches!(param, "typename" | "class" | "struct") {
                    continue;
                }
                let default = at.map(|a| item[a + 1..].trim().to_string());
                if let Some(slot) = names.iter_mut().find(|(n, _)| n == param) {
                    slot.1 = default;
                } else {
                    names.push((param.to_string(), default));
                }
            }
        }
        Rc::new(names)
    })
}

/// type-parameters.ts `gateTypeParameter`, for a C or C++ reference.
fn gate_type_parameter(ctx: &Ctx, m: Option<Match>, r: &Ref) -> Option<Match> {
    let m = m?;
    if r.is_cpp() {
        if let Some(q) = js::caps(&ctx.re(r"^([A-Za-z_]\w*)::"), &r.name) {
            let qualifier = js::group(&q, 1);
            for d in enclosing_declarations(ctx, &r.file, r.line) {
                let params = declared_type_parameters(ctx, d);
                let Some((_, default)) = params.iter().find(|(n, _)| n == qualifier) else {
                    continue;
                };
                let ty = default.as_deref().and_then(|f| {
                    js::caps(&ctx.re(r"([A-Za-z_]\w*)\s*(?:<[\s\S]*>)?\s*$"), f)
                        .map(|c| js::group(&c, 1).to_string())
                });
                let segments: Vec<&str> = ctx.sym(m.target).qualified_name.split("::").collect();
                let member =
                    ty.is_some_and(|t| segments.len() >= 2 && segments[segments.len() - 2] == t);
                return member.then_some(m);
            }
        }
    }
    if !r.is_cpp()
        || !matches!(
            r.kind,
            ReferenceKind::References
                | ReferenceKind::TypeOf
                | ReferenceKind::Returns
                | ReferenceKind::Instantiates
        )
    {
        return Some(m);
    }
    if !js::test(&ctx.re(r"^[A-Z][A-Za-z0-9_]*$"), &r.name) {
        return Some(m);
    }
    for d in enclosing_declarations(ctx, &r.file, r.line) {
        if declared_type_parameters(ctx, d)
            .iter()
            .any(|(n, _)| *n == r.name)
        {
            return None;
        }
    }
    Some(m)
}

/// `gateSuperSelfCall`.
pub fn gate_super_self_call(ctx: &Ctx, m: Option<Match>, r: &Ref) -> Option<Match> {
    let m = m?;
    if r.kind != ReferenceKind::Calls || m.target != r.from {
        return Some(m);
    }
    let cut = r.name.rfind(['.', ':']).map_or(0, |i| i + 1);
    let name = &r.name[cut..];
    if !js::test(&ctx.re(r"^[A-Za-z_$][\w$]*$"), name) {
        return Some(m);
    }
    let line = ctx.line(&r.file, r.line);
    if line.is_empty() {
        return Some(m);
    }
    let escaped = name.replace('$', r"\$");
    let via_super = ctx.re(&format!(
        r"(?:\b(?:super|base)\s*(?:\(\s*(?:[\w.]+\s*,\s*\w+)?\s*\))?\s*\??\.\s*|\[\s*super\s+|\bparent\s*::\s*){escaped}\b"
    ));
    (!js::test(&via_super, &line)).then_some(m)
}

/// alias-binding.ts `resolveAliasBinding`, for a C or C++ target.
pub fn resolve_alias_binding(ctx: &Ctx, alias: SymId, member: Option<&str>) -> Option<SymId> {
    let a = ctx.sym(alias);
    if !matches!(
        a.kind,
        NodeKind::Constant | NodeKind::Variable | NodeKind::Property
    ) {
        return None;
    }
    let init = a.signature.as_deref()?.trim();
    let target = match member {
        Some(key) => {
            let key = js::escape(key);
            if let Some(c) = js::caps(
                &ctx.re(&format!(r"[{{,]\s*{key}\s*:\s*([A-Za-z_$][\w$]*)\s*[,}}]")),
                init,
            ) {
                js::group(&c, 1).to_string()
            } else {
                let c = js::caps(&ctx.re(&format!(r"[{{,]\s*({key})\s*[,}}]")), init)?;
                js::group(&c, 1).to_string()
            }
        }
        None => {
            let c = js::caps(
                &ctx.re(r"^=\s*([A-Za-z_$][\w$]*)\s*(?:as\s+[\w.<>\[\]]+\s*)?;?$"),
                init,
            )?;
            js::group(&c, 1).to_string()
        }
    };
    if target == a.name {
        return None;
    }
    let candidates: Vec<SymId> = ctx
        .named(&target)
        .iter()
        .copied()
        .filter(|&n| {
            matches!(
                ctx.kind(n),
                NodeKind::Function | NodeKind::Method | NodeKind::Class | NodeKind::Component
            )
        })
        .collect();
    let same: Vec<SymId> = candidates
        .iter()
        .copied()
        .filter(|&n| ctx.sym(n).file == a.file)
        .collect();
    match same.len() {
        1 => Some(same[0]),
        0 => (candidates.len() == 1).then(|| candidates[0]),
        _ => None,
    }
}

/// `resolveOneIn`, for a C or C++ reference.
pub fn resolve(ctx: &Ctx, r: &Ref) -> Option<Match> {
    if macro_vis::is_visible_cpp_macro(ctx, r) {
        return None;
    }
    let m = gate_type_parameter(ctx, gate_target_kind(ctx, resolve_inner(ctx, r), r), r);
    let call = gate_super_self_call(ctx, gate_language_match(ctx, m, r), r)?;
    if r.kind != ReferenceKind::Calls {
        return Some(call);
    }
    let member = r.name.rfind('.').map(|d| &r.name[d + 1..]);
    match resolve_alias_binding(ctx, call.target, member) {
        Some(forwarded) if forwarded != call.target => gate_language_match(
            ctx,
            Some(Match::new(
                forwarded,
                call.strategy,
                call.confidence.min(0.85),
            )),
            r,
        ),
        _ => Some(call),
    }
}
