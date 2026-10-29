//! What a C or C++ translation unit can see: the files its includes reach,
//! what a source file keeps to itself, and the shared visibility rule the
//! name-matching pipeline's pick goes through.

use super::super::features;
use super::*;

/// `C_SOURCE_EXT`: a C/C++ (or Objective-C) file that is a translation unit.
pub fn is_c_source(ctx: &Ctx, path: &str) -> bool {
    js::test(&ctx.re(r"(?i)\.(c|cc|cpp|cxx|c\+\+|m|mm)$"), path)
}

fn file_stem(path: &str) -> &str {
    let name = &path[path.rfind('/').map_or(0, |i| i + 1)..];
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

/// `isTestSuitePath`.
pub fn is_test_suite_path(ctx: &Ctx, file_path: &str) -> bool {
    if !codegraph_core::search::is_test_path(file_path) {
        return false;
    }
    let lower = file_path.to_lowercase();
    let name = &lower[lower.rfind('/').map_or(0, |i| i + 1)..];
    let original = &file_path[file_path.rfind('/').map_or(0, |i| i + 1)..];
    if name.starts_with("test_")
        || js::test(&ctx.re(r"[._-](?:test|tests|unittest|unittests)\.[a-z0-9]+$|[._](?:spec|specs)\.[a-z0-9]+$"), name)
        || js::test(&ctx.re(r"(?:Test|Tests|TestCase)\.(?:java|kt|kts|swift|cs|scala|groovy|m|mm|vb|fs)$"), original)
        || name == "conftest.py"
    {
        return true;
    }
    js::test(
        &ctx.re(r"(?:^|/)(?:tests?|__tests__|specs?|e2e|unittests|[\w.]+[-_]unittests?)/"),
        &lower,
    ) || js::test(
        &ctx.re(r"(?:^|/)[A-Za-z0-9]*(?:Test|Tests|Spec)/"),
        file_path,
    )
}

fn is_test_suite_file(ctx: &Ctx, path: &str) -> bool {
    ctx.memo(memo::NM, format!("ts\0{path}"), || {
        is_test_suite_path(ctx, path)
    })
}

/// The headers a C or C++ file includes under its own name.
pub fn cpp_same_stem_headers(ctx: &Ctx, file: &str) -> Rc<Vec<String>> {
    ctx.memo(memo::NM, format!("sh\0{file}"), || {
        let stem = file_stem(file);
        let mut found = Vec::new();
        for &inc in ctx.in_file(file) {
            let s = ctx.sym(inc);
            if s.kind != NodeKind::Import || file_stem(&s.name) != stem {
                continue;
            }
            if let Some(header) = includers::cpp_included_file(ctx, inc) {
                if header != file && file_stem(&header) == stem {
                    found.push(header);
                }
            }
        }
        let dir = &file[..file.rfind('/').map_or(0, |i| i + 1)];
        let mut beside: Vec<String> = found
            .iter()
            .filter(|h| &h[..h.rfind('/').map_or(0, |i| i + 1)] == dir)
            .cloned()
            .collect();
        if beside.is_empty() {
            beside = includers::included_headers_beside(ctx, file);
        }
        Rc::new(if beside.is_empty() { found } else { beside })
    })
}

/// Whether `candidate`'s test suite file is one `r`'s translation unit includes.
pub fn is_included_cpp_test_suite(ctx: &Ctx, candidate: SymId, r: &Ref) -> bool {
    let c = ctx.sym(candidate);
    if !matches!(c.language, Language::C | Language::Cpp)
        || !matches!(r.lang, Language::C | Language::Cpp)
    {
        return false;
    }
    if includers::includers(ctx, &c.file).contains(&r.file) {
        return true;
    }
    cpp_same_stem_headers(ctx, &c.file)
        .iter()
        .any(|h| is_test_suite_file(ctx, h) && includers::includers(ctx, h).contains(&r.file))
}

/// Whether a C or C++ definition is in `r`'s translation unit, as its includes show it.
pub fn is_in_cpp_translation_unit(ctx: &Ctx, candidate: SymId, r: &Ref) -> bool {
    let c = ctx.sym(candidate);
    if c.file == r.file {
        return true;
    }
    if !matches!(c.language, Language::C | Language::Cpp) {
        return false;
    }
    if includers::includers(ctx, &c.file).contains(&r.file) {
        return true;
    }
    if !is_c_source(ctx, &c.file)
        || is_cpp_translation_unit_local(ctx, candidate, r)
        || (matches!(
            c.kind,
            NodeKind::Function | NodeKind::Variable | NodeKind::Constant
        ) && is_static_c_function(ctx, candidate))
    {
        return false;
    }
    let suite = is_test_suite_file(ctx, &c.file);
    cpp_same_stem_headers(ctx, &c.file).iter().any(|h| {
        (!suite || is_test_suite_file(ctx, h))
            && (*h == r.file || includers::includers(ctx, h).contains(&r.file))
    })
}

/// What a C or C++ name written alone can mean outside a class, by its kind.
pub fn is_cpp_name_alone_target(ctx: &Ctx, n: SymId, r: &Ref) -> bool {
    let s = ctx.sym(n);
    let constructor =
        s.kind == NodeKind::Method && s.qualified_name.ends_with(&format!("{0}::{0}", s.name));
    let fits = match r.kind {
        ReferenceKind::Calls => {
            s.kind == NodeKind::Function || constructor || CPP_TYPE_KINDS.contains(&s.kind)
        }
        ReferenceKind::Instantiates => constructor || CPP_TYPE_KINDS.contains(&s.kind),
        _ => {
            constructor
                || !matches!(
                    s.kind,
                    NodeKind::Method | NodeKind::Field | NodeKind::Property
                )
        }
    };
    if !fits {
        return false;
    }
    let inner = if constructor {
        aliases::parent_scope(&s.qualified_name)
    } else {
        s.qualified_name.as_str()
    };
    let scope = aliases::parent_scope(inner);
    !cpp_type_decls(ctx, scope)
        .iter()
        .any(|&d| CPP_CLASS_KINDS.contains(&ctx.kind(d)))
}

/// Same-named candidates narrowed to the ones `r`'s translation unit declares, when there are any.
pub fn within_cpp_translation_unit(
    ctx: &Ctx,
    candidates: &[SymId],
    r: &Ref,
    by_name: bool,
) -> Vec<SymId> {
    if candidates.len() < 2
        || !matches!(r.lang, Language::C | Language::Cpp)
        || r.kind == ReferenceKind::Imports
    {
        return candidates.to_vec();
    }
    if by_name && r.kind == ReferenceKind::Calls && cpp_implicit_receiver(ctx, r) != Some("bare") {
        return candidates.to_vec();
    }
    if r.kind == ReferenceKind::Calls && ctx.kind(r.from) == NodeKind::File {
        return candidates.to_vec();
    }
    let within: Vec<SymId> = candidates
        .iter()
        .copied()
        .filter(|&n| {
            is_in_cpp_translation_unit(ctx, n, r)
                && (!by_name || ctx.sym(n).file == r.file || is_cpp_name_alone_target(ctx, n, r))
        })
        .collect();
    if within.is_empty() || within.len() == candidates.len() {
        return candidates.to_vec();
    }
    if r.lang == Language::C
        && r.kind == ReferenceKind::Calls
        && within
            .iter()
            .all(|&n| features::is_c_feature_variant(ctx, n, &r.file, r.line))
        && !features::is_off_by_default(ctx, &r.file, r.line)
        && candidates.iter().any(|&n| {
            !within.contains(&n)
                && ctx.kind(n) == NodeKind::Function
                && !is_static_c_function(ctx, n)
                && !features::is_c_feature_variant(ctx, n, &r.file, r.line)
        })
    {
        return candidates.to_vec();
    }
    if r.kind == ReferenceKind::Calls {
        let first = ctx.sym(within[0]).name.clone();
        if let Some(args) = cpp_call_arguments(ctx, r, &first) {
            let fits = |n: SymId| cpp_argument_count_fits(ctx, n, &ctx.sym(n).name, &args);
            if within.iter().all(|&n| fits(n) == Some(false))
                && candidates
                    .iter()
                    .any(|&n| !within.contains(&n) && fits(n) == Some(true))
            {
                return candidates.to_vec();
            }
        }
    }
    within
}

/// Whether only another C or C++ translation unit can name `candidate`.
pub fn is_cpp_translation_unit_local(ctx: &Ctx, candidate: SymId, r: &Ref) -> bool {
    if !matches!(r.lang, Language::C | Language::Cpp) {
        return false;
    }
    let seen_from = |decl: SymId, file: &str| -> bool {
        let d = ctx.sym(decl);
        !is_c_source(ctx, &d.file)
            || d.file == file
            || includers::includers(ctx, &d.file).contains(file)
            || cpp_includes_after(ctx, &d.file, d.start_line, file)
    };
    let seen = |decl: SymId| seen_from(decl, &r.file);
    let c = ctx.sym(candidate);
    if CPP_TYPE_KINDS.contains(&c.kind) {
        return !seen(candidate);
    }
    if let Some(enclosing) = cpp_enclosing_class_decl(ctx, candidate) {
        return !seen(enclosing);
    }
    let decls: Vec<SymId> = cpp_type_decls(ctx, aliases::parent_scope(&c.qualified_name))
        .iter()
        .copied()
        .filter(|&d| seen_from(d, &c.file))
        .collect();
    if decls.iter().any(|&d| ctx.kind(d) == NodeKind::TypeAlias) {
        return false;
    }
    if !decls.is_empty() {
        return !decls.iter().any(|&d| seen(d));
    }
    !seen(candidate)
        && cpp_unnamed_namespace_spans(ctx, &c.file)
            .iter()
            .any(|&(start, end)| c.start_line >= start && c.start_line <= end)
}

/// Of the methods a C or C++ type's name gives, those `r`'s translation unit can see, when there are any.
pub fn cpp_visible_first(ctx: &Ctx, methods: &[SymId], r: &Ref) -> Vec<SymId> {
    if methods.len() < 2 || !matches!(r.lang, Language::C | Language::Cpp) {
        return methods.to_vec();
    }
    let visible: Vec<SymId> = methods
        .iter()
        .copied()
        .filter(|&m| !is_cpp_translation_unit_local(ctx, m, r))
        .collect();
    if visible.is_empty() {
        methods.to_vec()
    } else {
        visible
    }
}

/// The declarations of the C or C++ type (or alias) named `qualified`.
pub fn cpp_type_decls(ctx: &Ctx, qualified: &str) -> Rc<Vec<SymId>> {
    if qualified.is_empty() {
        return Rc::new(Vec::new());
    }
    ctx.memo(memo::NM, format!("td\0{qualified}"), || {
        Rc::new(
            ctx.by_qname(qualified)
                .iter()
                .copied()
                .filter(|&n| {
                    let s = ctx.sym(n);
                    CPP_TYPE_KINDS.contains(&s.kind)
                        && matches!(s.language, Language::C | Language::Cpp)
                })
                .collect(),
        )
    })
}

/// The innermost C or C++ class declaration of its file whose lines hold `member`.
pub fn cpp_enclosing_class_decl(ctx: &Ctx, member: SymId) -> Option<SymId> {
    let m = ctx.sym(member);
    let mut innermost: Option<SymId> = None;
    for &cls in cpp_classes_in(ctx, &m.file).iter() {
        let c = ctx.sym(cls);
        if cls == member
            || m.start_line < c.start_line
            || m.start_line > c.end_line.max(c.start_line)
        {
            continue;
        }
        if innermost.is_none_or(|i| c.start_line >= ctx.sym(i).start_line) {
            innermost = Some(cls);
        }
    }
    innermost
}

/// Whether the C or C++ file `source` `#include`s `header` below line `line`.
fn cpp_includes_after(ctx: &Ctx, source: &str, line: u32, header: &str) -> bool {
    let includes: Rc<Vec<(u32, Option<String>)>> =
        ctx.memo(memo::NM, format!("il\0{source}"), || {
            Rc::new(
                ctx.in_file(source)
                    .iter()
                    .copied()
                    .filter(|&n| ctx.kind(n) == NodeKind::Import)
                    .map(|n| (ctx.sym(n).start_line, includers::cpp_included_file(ctx, n)))
                    .collect(),
            )
        });
    includes
        .iter()
        .any(|(at, file)| *at > line && file.as_deref() == Some(header))
}

/// The line spans of the unnamed namespaces a C or C++ file writes, the outermost of nested ones.
pub fn cpp_unnamed_namespace_spans(ctx: &Ctx, file: &str) -> Rc<Vec<(u32, u32)>> {
    ctx.memo(memo::NM, format!("un\0{file}"), || {
        let mut spans = Vec::new();
        let text = ctx.source(file).map(|s| s.text.clone()).unwrap_or_default();
        if !js::test(&ctx.re(r"\bnamespace\s*\{"), &text) {
            return Rc::new(spans);
        }
        let raw_open = ctx.re(r"(?:^|[^\w$])(?:u8|[uUL])?R$");
        let unnamed_head = ctx.re(r"(?:^|[^\w$])namespace\s*$");
        let b = text.as_bytes();
        let mut open: Vec<(u32, bool)> = Vec::new();
        let mut line = 1u32;
        let mut line_start = true;
        let mut code: Vec<u8> = Vec::new();
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            if c == b'\n' {
                line += 1;
                line_start = true;
                code.push(b' ');
                i += 1;
                continue;
            }
            if line_start && c == b'#' {
                let mut end = i;
                while end < b.len()
                    && (b[end] != b'\n'
                        || (end >= 1 && b[end - 1] == b'\\')
                        || (end >= 2 && b[end - 1] == b'\r' && b[end - 2] == b'\\'))
                {
                    if b[end] == b'\n' {
                        line += 1;
                    }
                    end += 1;
                }
                i = end;
                continue;
            }
            if !js::is_space(c) {
                line_start = false;
            }
            if c == b'/' && b.get(i + 1) == Some(&b'/') {
                i = js::index_of(&text, "\n", i).unwrap_or(b.len());
                continue;
            }
            if c == b'/' && b.get(i + 1) == Some(&b'*') {
                let stop = js::index_of(&text, "*/", i + 2).map_or(b.len(), |e| e + 2);
                line += b[i..stop].iter().filter(|&&x| x == b'\n').count() as u32;
                i = stop;
                continue;
            }
            if c == b'"'
                && js::test(
                    &raw_open,
                    &String::from_utf8_lossy(&b[i.saturating_sub(4)..i]),
                )
            {
                let paren = js::index_of(&text, "(", i);
                let end = paren.and_then(|p| {
                    let delim = String::from_utf8_lossy(&b[i + 1..p]).into_owned();
                    js::index_of(&text, &format!("){delim}\""), p)
                });
                let stop = match (end, paren) {
                    (Some(e), Some(p)) => e + p - i + 1,
                    _ => b.len(),
                };
                let stop = stop.min(b.len());
                line += b[i..stop].iter().filter(|&&x| x == b'\n').count() as u32;
                i = stop;
                continue;
            }
            if c == b'"' || c == b'\'' {
                let mut j = i + 1;
                while j < b.len() && b[j] != c && b[j] != b'\n' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                i = j + 1;
                continue;
            }
            if c == b'{' {
                open.push((
                    line,
                    js::test(&unnamed_head, &String::from_utf8_lossy(&code)),
                ));
                code.clear();
            } else if c == b'}' {
                if let Some((start, unnamed)) = open.pop() {
                    if unnamed && !open.iter().any(|o| o.1) {
                        spans.push((start, line));
                    }
                }
                code.clear();
            } else if c == b';' {
                code.clear();
            } else {
                code.push(c);
                if code.len() > 200 {
                    code.drain(..code.len() - 100);
                }
            }
            i += 1;
        }
        Rc::new(spans)
    })
}

/// Whether a C/C++ function definition carries the `static` storage class.
pub fn is_static_c_function(ctx: &Ctx, candidate: SymId) -> bool {
    ctx.memo(memo::NM, format!("st\0{candidate}"), || {
        let c = ctx.sym(candidate);
        let Some(src) = ctx.source(&c.file) else {
            return false;
        };
        let at = |i: i64| -> &str {
            if i < 0 {
                return "";
            }
            src.lines.get(i as usize).map(String::as_str).unwrap_or("")
        };
        let head = format!(
            "{}\n{}",
            at(c.start_line as i64 - 2),
            at(c.start_line as i64 - 1)
        );
        js::test(&ctx.re(r"(^|[\s;}])static\s"), &head)
    })
}

/// Whether `n` is a C type or global that an Objective-C source file other than `r`'s defines.
pub fn is_objc_file_local_elsewhere(ctx: &Ctx, n: SymId, r: &Ref) -> bool {
    let s = ctx.sym(n);
    s.language == Language::ObjC
        && s.file != r.file
        && matches!(
            s.kind,
            NodeKind::Struct
                | NodeKind::Union
                | NodeKind::Enum
                | NodeKind::TypeAlias
                | NodeKind::Variable
                | NodeKind::Constant
        )
        && is_c_source(ctx, &s.file)
}

/// `isVisibleAcrossFiles`, for what a C or C++ reference can reach.
pub fn is_visible_across_files(ctx: &Ctx, candidate: SymId, r: &Ref) -> bool {
    let c = ctx.sym(candidate);
    if c.file == r.file {
        return true;
    }
    if is_test_suite_path(ctx, &c.file)
        && !codegraph_core::search::is_test_path(&r.file)
        && !is_included_cpp_test_suite(ctx, candidate, r)
    {
        return false;
    }
    if matches!(c.language, Language::C | Language::Cpp) {
        if c.kind == NodeKind::Function
            && is_c_source(ctx, &c.file)
            && is_static_c_function(ctx, candidate)
            && !includers::includers(ctx, &c.file).contains(&r.file)
        {
            return false;
        }
        return !is_cpp_translation_unit_local(ctx, candidate, r);
    }
    if is_objc_file_local_elsewhere(ctx, candidate, r) {
        return false;
    }
    if matches!(c.language, Language::Swift) {
        return c.visibility != Some(codegraph_core::Visibility::Private);
    }
    true
}

/// `preferCallSiteFile`.
pub fn prefer_call_site_file(ctx: &Ctx, nodes: &[SymId], file: &str) -> Vec<SymId> {
    if nodes.len() < 2 {
        return nodes.to_vec();
    }
    let (same, other): (Vec<SymId>, Vec<SymId>) =
        nodes.iter().partition(|&&n| ctx.sym(n).file == file);
    if same.is_empty() {
        nodes.to_vec()
    } else {
        same.into_iter().chain(other).collect()
    }
}
