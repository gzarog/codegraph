//! name-matcher.ts's shared strategies — qualified name, exact name, fuzzy,
//! function values — and the gates on their pick, as they run for a C or C++
//! reference.

use super::super::family::crosses_code_boundary;
use super::*;

/// `DEFAULT_AMBIGUOUS_NAME_CEILING`.
pub const AMBIGUOUS_NAME_CEILING: usize = 500;

fn dirs_of(path: &str) -> Vec<&str> {
    let mut d: Vec<&str> = path.split('/').collect();
    d.pop();
    d
}

/// `computePathProximity`.
pub fn path_proximity(a: &str, b: &str) -> i64 {
    includes::path_proximity(a, b)
}

/// `findBestMatch`.
pub fn find_best_match(ctx: &Ctx, r: &Ref, candidates: &[SymId]) -> Option<SymId> {
    let ref_dirs = dirs_of(&r.file);
    let has_same = candidates.iter().any(|&c| ctx.sym(c).language == r.lang);
    let mut best_score = -1f64;
    let mut best: Option<SymId> = None;
    let mut tied: Vec<SymId> = Vec::new();
    for &c in candidates {
        let s = ctx.sym(c);
        if has_same && s.language != r.lang {
            continue;
        }
        let mut score = 0f64;
        if s.file == r.file {
            score += 100.0;
        }
        let cd = dirs_of(&s.file);
        let shared = ref_dirs
            .iter()
            .zip(cd.iter())
            .take_while(|(a, b)| a == b)
            .count() as f64;
        score += (shared * 15.0).min(80.0);
        score += if s.language == r.lang { 50.0 } else { -80.0 };
        if r.kind == ReferenceKind::Calls && matches!(s.kind, NodeKind::Function | NodeKind::Method)
        {
            score += 25.0;
        }
        if r.kind == ReferenceKind::Instantiates
            && matches!(
                s.kind,
                NodeKind::Class | NodeKind::Struct | NodeKind::Union | NodeKind::Interface
            )
        {
            score += 25.0;
        }
        if r.kind == ReferenceKind::Decorates {
            if matches!(s.kind, NodeKind::Function | NodeKind::Method) {
                score += 25.0;
            } else if matches!(s.kind, NodeKind::Class | NodeKind::Interface) {
                score += 15.0;
            }
        }
        if s.exported {
            score += 10.0;
        }
        if s.file == r.file && s.start_line > 0 {
            let distance = (s.start_line as f64 - r.line as f64).abs();
            score += (20.0 - distance / 10.0).max(0.0);
        }
        if score > best_score {
            best_score = score;
            best = Some(c);
            tied = vec![c];
        } else if score == best_score {
            tied.push(c);
        }
    }
    if tied.len() > 1 {
        if let Some(c) = closest_copy(ctx, r, &tied) {
            return Some(c);
        }
    }
    best
}

fn shared_dir_depth(a: &str, b: &str) -> usize {
    let da = dirs_of(a);
    let db = dirs_of(b);
    da.iter().zip(db.iter()).take_while(|(x, y)| x == y).count()
}

fn basename(p: &str) -> &str {
    &p[p.rfind('/').map_or(0, |i| i + 1)..]
}

/// `closestCopy`.
pub fn closest_copy(ctx: &Ctx, r: &Ref, candidates: &[SymId]) -> Option<SymId> {
    if candidates.len() < 2 {
        return None;
    }
    let first = ctx.sym(candidates[0]);
    let base = basename(&first.file);
    let mut best = None;
    let mut best_depth: i64 = -1;
    let mut unique = false;
    for &c in candidates {
        let s = ctx.sym(c);
        if s.qualified_name != first.qualified_name
            || s.kind != first.kind
            || basename(&s.file) != base
        {
            return None;
        }
        let depth = shared_dir_depth(&r.file, &s.file) as i64;
        if depth > best_depth {
            best_depth = depth;
            best = Some(c);
            unique = true;
        } else if depth == best_depth {
            unique = false;
        }
    }
    if unique {
        best
    } else {
        None
    }
}

/// `isDecisiveSameFileMatch`.
fn is_decisive_same_file_match(ctx: &Ctx, best: SymId, r: &Ref, candidates: &[SymId]) -> bool {
    let b = ctx.sym(best);
    if b.file != r.file || best == r.from {
        return false;
    }
    if candidates
        .iter()
        .any(|&c| c != best && ctx.sym(c).file == r.file)
    {
        return false;
    }
    let Some(sep) = b.qualified_name.rfind("::").filter(|&s| s > 0) else {
        return b.kind != NodeKind::Method;
    };
    let owner = &b.qualified_name[..sep];
    if b.kind != NodeKind::Method {
        let owner_name = owner.rfind("::").map_or(owner, |c| &owner[c + 2..]);
        let declared: Vec<SymId> = ctx
            .in_file(&b.file)
            .iter()
            .copied()
            .filter(|&n| {
                let s = ctx.sym(n);
                s.name == owner_name && s.qualified_name == owner
            })
            .collect();
        if declared
            .iter()
            .all(|&n| matches!(ctx.kind(n), NodeKind::Function | NodeKind::Method))
        {
            return true;
        }
    }
    let from = &ctx.sym(r.from).qualified_name;
    from == owner || from.starts_with(&format!("{owner}::"))
}

/// `CPP_DEFINE_SIGNATURE`.
pub fn is_define_constant(ctx: &Ctx, n: SymId) -> bool {
    let s = ctx.sym(n);
    s.kind == NodeKind::Constant
        && js::test(
            &ctx.re(r"^\s*#\s*define\b"),
            s.signature.as_deref().unwrap_or(""),
        )
}

fn is_supertype_target(ctx: &Ctx, n: SymId) -> bool {
    let s = ctx.sym(n);
    matches!(
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
    ) && !(s.language == Language::Scala && s.kind == NodeKind::Module)
}

fn is_inheritance_ref(r: &Ref) -> bool {
    matches!(r.kind, ReferenceKind::Extends | ReferenceKind::Implements)
}

fn is_importable_kind(k: NodeKind) -> bool {
    !matches!(
        k,
        NodeKind::Property
            | NodeKind::Field
            | NodeKind::Method
            | NodeKind::EnumMember
            | NodeKind::Parameter
    )
}

/// `isLexicallyReachable`, for the candidates a C or C++ reference weighs:
/// C and C++ nesting is never a scope, so only other languages' locals are out.
fn is_lexically_reachable(ctx: &Ctx, n: SymId, _r: &Ref) -> bool {
    let _ = ctx.sym(n);
    true
}

/// `matchByExactName`, for a C or C++ reference.
pub fn match_by_exact_name(ctx: &Ctx, r: &Ref) -> Option<Match> {
    let value_ref = matches!(
        r.kind,
        ReferenceKind::References | ReferenceKind::FunctionRef
    );
    let import_ref = r.kind == ReferenceKind::Imports;
    let inheritance = is_inheritance_ref(r);
    let same_name = ctx.named(&r.name);
    let c_macro_call =
        r.kind == ReferenceKind::Calls && same_name.iter().any(|&n| is_define_constant(ctx, n));
    let filtered: Vec<SymId> = same_name
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            !is_objc_file_local_elsewhere(ctx, n, r)
                && !(c_macro_call && !matches!(s.kind, NodeKind::Function | NodeKind::Method))
                && (!value_ref || same_language_family(s.language, r.lang))
                && s.kind != NodeKind::Import
                && (!inheritance || is_supertype_target(ctx, n))
                && (!import_ref || is_importable_kind(s.kind))
                && is_lexically_reachable(ctx, n, r)
        })
        .collect();
    let ranked = cpp_specialization_pool(ctx, &filtered, r);
    let candidates = if ranked.len() <= AMBIGUOUS_NAME_CEILING {
        within_cpp_translation_unit(ctx, &ranked, r, true)
    } else {
        ranked
    };
    if candidates.is_empty() {
        return None;
    }
    if candidates.len() == 1 {
        let cross = ctx.sym(candidates[0]).language != r.lang;
        return Some(Match::new(
            candidates[0],
            "exact-match",
            if cross { 0.5 } else { 0.9 },
        ));
    }
    if candidates.len() > AMBIGUOUS_NAME_CEILING {
        return None;
    }
    let nearest = find_best_match(ctx, r, &candidates)?;
    let best = cpp_function_overload(ctx, nearest, &candidates, r);
    let proximity = path_proximity(&r.file, &ctx.sym(best).file);
    let weighed: Vec<SymId> = if candidates.iter().any(|&c| ctx.sym(c).language == r.lang) {
        candidates
            .iter()
            .copied()
            .filter(|&c| ctx.sym(c).language == r.lang)
            .collect()
    } else {
        candidates.clone()
    };
    let confidence = if is_decisive_same_file_match(ctx, best, r, &candidates) {
        0.9
    } else if best != r.from && closest_copy(ctx, r, &weighed) == Some(best) {
        0.8
    } else if proximity >= 30 {
        0.7
    } else {
        0.4
    };
    Some(Match::new(best, "exact-match", confidence))
}

fn ends_at_scope_boundary(qn: &str, name: &str) -> bool {
    if qn == name {
        return true;
    }
    let Some(before) = qn.strip_suffix(name) else {
        return false;
    };
    before.ends_with("::") || before.ends_with('.')
}

/// `matchByQualifiedName`, for a C or C++ reference.
pub fn match_by_qualified_name(ctx: &Ctx, r: &Ref) -> Option<Match> {
    if !r.name.contains("::") && !r.name.contains('.') {
        return None;
    }
    let keep = |nodes: &[SymId]| -> Vec<SymId> {
        if r.kind == ReferenceKind::Calls {
            nodes
                .iter()
                .copied()
                .filter(|&n| {
                    let s = ctx.sym(n);
                    !(s.kind == NodeKind::Constant
                        && matches!(s.language, Language::Yaml | Language::Properties))
                })
                .collect()
        } else {
            nodes.to_vec()
        }
    };
    if let Some(global_name) = r.name.strip_prefix("::") {
        let pool: Vec<SymId> = keep(ctx.by_qname(global_name))
            .into_iter()
            .filter(|&n| {
                let s = ctx.sym(n);
                is_c_family(s.language)
                    && !namespaces::macro_namespace_frames(ctx, &s.file)
                        .iter()
                        .any(|f| f.start <= s.start_line && f.end >= s.start_line)
            })
            .collect();
        let global = cpp_specialization_pool(ctx, &pool, r);
        let chosen = *prefer_call_site_file(ctx, &global, &r.file).first()?;
        let confidence = if global.len() == 1 || ctx.sym(chosen).file == r.file {
            0.95
        } else {
            0.85
        };
        return Some(Match::new(
            cpp_function_overload(ctx, chosen, &global, r),
            "qualified-name",
            confidence,
        ));
    }
    let candidates = cpp_qualifier_specialization_pool(
        ctx,
        &cpp_specialization_pool(ctx, &keep(ctx.by_qname(&r.name)), r),
        r,
    );
    if candidates.len() == 1 {
        return Some(Match::new(candidates[0], "qualified-name", 0.95));
    }
    if candidates.len() > 1 {
        let ordered = prefer_call_site_file(ctx, &candidates, &r.file);
        if ctx.sym(ordered[0]).file == r.file {
            return Some(Match::new(
                cpp_function_overload(ctx, ordered[0], &ordered, r),
                "qualified-name",
                0.95,
            ));
        }
    }
    let last = r.name.rsplit([':', '.']).next().unwrap_or("");
    if !last.is_empty() {
        let partial: Vec<SymId> = keep(ctx.named(last))
            .into_iter()
            .filter(|&c| ends_at_scope_boundary(&ctx.sym(c).qualified_name, &r.name))
            .collect();
        let partial =
            cpp_qualifier_specialization_pool(ctx, &cpp_specialization_pool(ctx, &partial, r), r);
        let own: Vec<SymId> = partial
            .iter()
            .copied()
            .filter(|&c| same_language_family(ctx.sym(c).language, r.lang))
            .collect();
        let pool = if own.is_empty() { partial } else { own };
        if let Some(&chosen) = prefer_call_site_file(ctx, &pool, &r.file).first() {
            return Some(Match::new(
                cpp_function_overload(ctx, chosen, &pool, r),
                "qualified-name",
                0.85,
            ));
        }
    }
    None
}

/// `matchFuzzy`, for a C or C++ reference (names are case-sensitive there).
pub fn match_fuzzy(ctx: &Ctx, r: &Ref) -> Option<Match> {
    let callable: Vec<SymId> = ctx
        .named(&r.name)
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            matches!(
                s.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Class
            ) && !(r.kind == ReferenceKind::Instantiates && s.kind == NodeKind::Method)
                && !is_objc_file_local_elsewhere(ctx, n, r)
                && (!matches!(
                    r.kind,
                    ReferenceKind::References | ReferenceKind::FunctionRef
                ) || same_language_family(s.language, r.lang))
        })
        .collect();
    let same: Vec<SymId> = callable
        .iter()
        .copied()
        .filter(|&n| ctx.sym(n).language == r.lang)
        .collect();
    let pool = cpp_specialization_pool(ctx, if same.is_empty() { &callable } else { &same }, r);
    if pool.len() == 1
        && is_visible_across_files(ctx, pool[0], r)
        && is_lexically_reachable(ctx, pool[0], r)
    {
        let cross = ctx.sym(pool[0]).language != r.lang;
        return Some(Match::new(pool[0], "fuzzy", if cross { 0.3 } else { 0.5 }));
    }
    None
}

/// `matchFunctionRef`, for a C or C++ reference.
pub fn match_function_ref(ctx: &Ctx, r: &Ref) -> Option<Match> {
    if r.name.starts_with("this.") {
        return None;
    }
    let bare_fn_only = r.lang == Language::Cpp;
    if let Some(sep) = r.name.rfind("::") {
        let member = &r.name[sep + 2..];
        let suffix = format!("::{}", r.name);
        let scoped: Vec<SymId> = ctx
            .named(member)
            .iter()
            .copied()
            .filter(|&n| {
                let s = ctx.sym(n);
                matches!(s.kind, NodeKind::Function | NodeKind::Method)
                    && same_language_family(s.language, r.lang)
                    && n != r.from
                    && (s.qualified_name == r.name || s.qualified_name.ends_with(&suffix))
            })
            .collect();
        if scoped.is_empty() {
            return None;
        }
        let same: Vec<SymId> = scoped
            .iter()
            .copied()
            .filter(|&n| ctx.sym(n).file == r.file)
            .collect();
        if same.is_empty() && scoped.len() > 1 {
            return None;
        }
        let pool = if same.is_empty() { &scoped } else { &same };
        let target = first_by_line(ctx, pool);
        return Some(Match::new(target, "function-ref", 0.9));
    }
    let named: Vec<SymId> = ctx
        .named(&r.name)
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            (s.kind == NodeKind::Function || (!bare_fn_only && s.kind == NodeKind::Method))
                && same_language_family(s.language, r.lang)
                && n != r.from
        })
        .collect();
    let candidates: Vec<SymId> = named
        .iter()
        .copied()
        .filter(|&n| is_lexically_reachable(ctx, n, r))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let same: Vec<SymId> = candidates
        .iter()
        .copied()
        .filter(|&n| ctx.sym(n).file == r.file)
        .collect();
    if !same.is_empty() {
        let target = first_by_line(ctx, &same);
        return Some(Match::new(
            target,
            "function-ref",
            if same.len() == 1 { 0.95 } else { 0.9 },
        ));
    }
    if candidates.len() == 1 && named.len() == 1 {
        return Some(Match::new(candidates[0], "function-ref", 0.8));
    }
    None
}

/// `nodes.reduce((a, b) => a.startLine <= b.startLine ? a : b)`.
fn first_by_line(ctx: &Ctx, nodes: &[SymId]) -> SymId {
    let mut best = nodes[0];
    for &n in &nodes[1..] {
        if ctx.sym(best).start_line > ctx.sym(n).start_line {
            best = n;
        }
    }
    best
}

/// `hasBridgeEvidence`, for a native (C/C++) reference.
fn has_bridge_evidence(ctx: &Ctx, candidate: SymId, r: &Ref) -> bool {
    if r.kind != ReferenceKind::Calls {
        return false;
    }
    let c = ctx.sym(candidate);
    if c.kind != NodeKind::Function {
        return false;
    }
    let name = js::escape(&c.name);
    let Some(src) = ctx.source(&c.file) else {
        return false;
    };
    // (`split('\n')`: the lines keep a CRLF's `\r`.)
    let raw: Vec<&str> = src.text.split('\n').collect();
    let end = (c.end_line as usize).min(raw.len());
    if c.language == Language::Go {
        let from = (c.start_line as usize).saturating_sub(2).min(end);
        let declaration = raw[from..end].join("\n");
        let stripped = crate::cpp::js::strip_comments_cpp(&src.text);
        return js::test(&ctx.re(r#"\bimport\s+(?:\(\s*)?"C""#), &stripped)
            && js::test(
                &ctx.re(&format!(r"(?m)^//export {name}\r?\nfunc {name}\s*\(")),
                &declaration,
            );
    }
    if c.language == Language::Rust {
        let from = (c.start_line as usize).saturating_sub(1).min(end);
        let declaration = raw[from..end].join("\n");
        return js::test(
            &ctx.re(&format!(r#"\bpub\s+extern\s+"C"\s+fn\s+{name}\b"#)),
            &crate::cpp::js::strip_comments_cpp(&declaration),
        );
    }
    false
}

/// `gateLanguageMatch`: reject a pick across a code boundary without bridge evidence.
pub fn gate_language_match(ctx: &Ctx, m: Option<Match>, r: &Ref) -> Option<Match> {
    let m = m?;
    let lang = ctx.sym(m.target).language;
    if !crosses_code_boundary(r.lang, lang) || has_bridge_evidence(ctx, m.target, r) {
        Some(m)
    } else {
        None
    }
}

/// `bareCallReceiver`: the receiver a call recorded by its bare name is written on with `.`.
pub fn bare_call_receiver(ctx: &Ctx, r: &Ref) -> Option<String> {
    if r.kind != ReferenceKind::Calls || !js::test(&ctx.re(r"^[A-Za-z_$][\w$]*$"), &r.name) {
        return None;
    }
    let src = ctx.source(&r.file)?;
    let name = r.name.replace('$', r"\$");
    let call = ctx.re(&format!(
        r"(?<![\w$]){name}\s*(?:<[^<>()]*>|\[(?:[^\[\]]|\[[^\[\]]*\])*\])?\s*[({{]"
    ));
    let from_line = (r.line as usize).saturating_sub(1).min(src.lines.len());
    let to_line = (r.line as usize + 7).min(src.lines.len());
    let span = &src.lines[from_line..to_line];
    let from = r.column as usize;
    let mut text = span
        .first()
        .map(|l| js::slice_from(l, from).to_string())
        .unwrap_or_default();
    let mut at = js::search(&call, &text);
    if at.is_none_or(|a| text.find(r.name.as_str()).is_some_and(|i| i < a)) {
        text = js::slice_from(&span.join("\n"), from).to_string();
        at = js::search(&call, &text);
    }
    let at = at?;
    let before = text[..at].trim_end();
    if !js::test(&ctx.re(r"\??\.$"), before) {
        return None;
    }
    let head = js::replace_with(&ctx.re(r"\??\.$"), before, "");
    let head = head.trim_end();
    if js::test(&ctx.re(r"(?:^|[^\w$.])(?:this|self|super|Self)$"), head)
        || js::test(&ctx.re(r"(?:^|[^\w$.])super\s*\([^()]*\)$"), head)
    {
        return Some("self".into());
    }
    let start = head.char_indices().rev().nth(39).map_or(0, |(i, _)| i);
    Some(head[start..].to_string())
}

/// `isCollapsedNonRecursion`, for a C or C++ reference.
pub fn is_collapsed_non_recursion(ctx: &Ctx, r: &Ref) -> bool {
    bare_call_receiver(ctx, r).is_some_and(|w| w != "self")
}

/// `matchCppMacroNamespaced`: a name qualified by a namespace the project opens with a macro.
pub fn match_cpp_macro_namespaced(ctx: &Ctx, r: &Ref) -> Option<Match> {
    let mut target = r.name.strip_prefix("::").unwrap_or(&r.name).to_string();
    let head_end = target.find("::")?;
    let head = target[..head_end].to_string();
    let ns_aliases = namespaces::namespace_aliases(ctx);
    let alias = ns_aliases.get(&head).cloned();
    if let Some(a) = &alias {
        target = format!("{a}{}", &target[head.len()..]);
    }
    let name = target[target.rfind("::").map_or(0, |i| i + 2)..].to_string();
    if !js::test(&ctx.re(r"^[A-Za-z_~]\w*$"), &name) {
        return None;
    }
    let mut matches = Vec::new();
    for &n in ctx.named(&name) {
        let s = ctx.sym(n);
        if !is_c_family(s.language)
            || !matches!(
                s.kind,
                NodeKind::Function
                    | NodeKind::Method
                    | NodeKind::Class
                    | NodeKind::Struct
                    | NodeKind::Enum
                    | NodeKind::TypeAlias
                    | NodeKind::Union
                    | NodeKind::Variable
                    | NodeKind::Constant
            )
        {
            continue;
        }
        let frames = namespaces::macro_namespace_frames(ctx, &s.file);
        let mut around: Vec<&namespaces::Frame> = frames
            .iter()
            .filter(|f| f.start <= s.start_line && f.end >= s.start_line)
            .collect();
        around.sort_by_key(|f| f.start);
        let prefix: Vec<String> = around.iter().flat_map(|f| f.path.iter().cloned()).collect();
        let effective = if !prefix.is_empty() {
            format!("{}::{}", prefix.join("::"), s.qualified_name)
        } else if alias.is_some() {
            s.qualified_name.clone()
        } else {
            String::new()
        };
        if effective == target {
            matches.push(n);
        }
    }
    let pool = cpp_specialization_pool(ctx, &matches, r);
    if pool.is_empty() {
        return None;
    }
    let args = if pool.len() > 1 && r.kind == ReferenceKind::Calls {
        cpp_call_arguments(ctx, r, &name)
    } else {
        None
    };
    let scores: Vec<i64> = pool
        .iter()
        .map(|&n| {
            (if codegraph_core::search::is_test_path(&ctx.sym(n).file) {
                -10
            } else {
                0
            }) + args
                .as_ref()
                .map_or(0, |a| cpp_overload_fit(ctx, n, &name, a))
        })
        .collect();
    let top = *scores.iter().max().unwrap();
    let tied: Vec<SymId> = pool
        .iter()
        .zip(&scores)
        .filter(|(_, &s)| s == top)
        .map(|(&n, _)| n)
        .collect();
    let best = match &args {
        Some(a) => cpp_argument_type_choice(ctx, &tied, &name, a, r),
        None => tied[0],
    };
    Some(Match::new(best, "qualified-name", 0.8))
}
