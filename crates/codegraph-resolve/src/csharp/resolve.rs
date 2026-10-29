//! A C# reference end to end, as upstream's ReferenceResolver runs it:
//! resolveOneIn's gates around resolveOneInner, and name-matcher.ts's
//! matchReference with the C# branches of its strategies.

use super::infer::{self, Bound};
use super::scope;
use crate::cpp::family::{crosses_code_boundary, same_language_family};
use crate::cpp::nm::{
    self, closest_copy, ends_at_scope_boundary, find_best_match, first_by_line,
    is_decisive_same_file_match, is_importable_kind, is_inheritance_ref, is_supertype_target,
    prefer_call_site_file, resolve_method_on_type, AMBIGUOUS_NAME_CEILING,
};
use crate::cpp::resolve as cpp_resolve;
use crate::cpp::{includes, js, Ctx, Ref};
use crate::matcher::Match;
use crate::symbols::SymId;
use codegraph_core::{Language, NodeKind, ReferenceKind, Visibility};
use std::collections::HashSet;

const CSHARP_STD_METHODS: &[&str] = &[
    "ToString",
    "Equals",
    "GetHashCode",
    "GetType",
    "CompareTo",
    "Add",
    "AddRange",
    "Remove",
    "RemoveAt",
    "RemoveAll",
    "Contains",
    "ContainsKey",
    "ContainsValue",
    "Clear",
    "Insert",
    "IndexOf",
    "CopyTo",
    "ToArray",
    "ToList",
    "ToDictionary",
    "GetEnumerator",
    "MoveNext",
    "Reset",
    "TryGetValue",
    "GetValueOrDefault",
    "TryAdd",
    "Write",
    "WriteLine",
    "WriteAsync",
    "WriteLineAsync",
    "Read",
    "ReadAsync",
    "ReadLine",
    "ReadToEnd",
    "Flush",
    "FlushAsync",
    "Close",
    "Dispose",
    "DisposeAsync",
    "Parse",
    "TryParse",
    "Format",
    "Join",
    "Split",
    "Replace",
    "Substring",
    "Trim",
    "TrimStart",
    "TrimEnd",
    "StartsWith",
    "EndsWith",
    "ToUpper",
    "ToLower",
    "ToUpperInvariant",
    "ToLowerInvariant",
    "Select",
    "Where",
    "First",
    "FirstOrDefault",
    "Single",
    "SingleOrDefault",
    "Last",
    "LastOrDefault",
    "Any",
    "All",
    "Count",
    "Sum",
    "Max",
    "Min",
    "OrderBy",
    "OrderByDescending",
    "GroupBy",
    "Skip",
    "Take",
    "Distinct",
    "Concat",
    "Cast",
    "OfType",
    "Aggregate",
    "Invoke",
    "DynamicInvoke",
    "GetMethod",
    "GetProperty",
    "GetField",
    "GetConstructor",
    "GetCustomAttributes",
    "GetGenericArguments",
    "MakeGenericType",
    "IsAssignableFrom",
    "ConfigureAwait",
    "Wait",
    "ContinueWith",
    "Append",
    "AppendLine",
    "Peek",
    "Push",
    "Pop",
    "Enqueue",
    "Dequeue",
    "HasFlag",
    "Find",
    "FindAll",
    "ForEach",
    "Sort",
    "Reverse",
    "Clone",
    "Seek",
    "SetLength",
];

fn is_std_method_name(name: &str) -> bool {
    CSHARP_STD_METHODS.contains(&name)
}

fn is_bare_name(ctx: &Ctx, name: &str) -> bool {
    js::test(&ctx.re(r"^[A-Za-z_]\w*$"), name)
}

/// `isMethodOwnerKind`.
fn is_method_owner_kind(ctx: &Ctx, n: SymId) -> bool {
    nm::is_method_owner_kind(ctx, n)
}

// ---------------------------------------------------------------- reachability

/// `localDeclarationScope`: the innermost function or method a variable's declaration is local to.
fn local_declaration_scope(ctx: &Ctx, n: SymId) -> Option<(u32, u32)> {
    let c = ctx.sym(n);
    if matches!(c.language, Language::C | Language::Cpp) {
        return None;
    }
    let mut scope: Option<(u32, u32)> = None;
    for &f in ctx.in_file(&c.file) {
        let s = ctx.sym(f);
        if !matches!(s.kind, NodeKind::Function | NodeKind::Method) || f == n {
            continue;
        }
        if s.start_line > c.start_line || s.end_line < c.start_line || s.start_line == s.end_line {
            continue;
        }
        if s.start_line == c.start_line && s.start_column >= c.start_column {
            continue;
        }
        if scope.is_none_or(|(a, b)| s.end_line - s.start_line < b - a) {
            scope = Some((s.start_line, s.end_line));
        }
    }
    scope
}

/// `lexicalScopeOf`: the function a function, method or type is declared inside.
fn lexical_scope_of(ctx: &Ctx, n: SymId) -> Option<(u32, u32)> {
    let c = ctx.sym(n);
    let own = &c.qualified_name;
    let parent = own.rfind("::").map_or("", |i| &own[..i]);
    let mut qn = own.as_str();
    while let Some(i) = qn.rfind("::") {
        qn = &qn[..i];
        let container = ctx.by_qname(qn).iter().copied().find(|&p| {
            let s = ctx.sym(p);
            s.file == c.file
                && matches!(s.kind, NodeKind::Function | NodeKind::Method)
                && s.start_line <= c.start_line
                && s.end_line >= c.end_line
        });
        let Some(container) = container else { continue };
        if c.kind == NodeKind::Method && qn == parent {
            break;
        }
        let s = ctx.sym(container);
        return Some((s.start_line, s.end_line));
    }
    None
}

/// `isLexicallyReachable`, for the candidates a C# reference weighs (the
/// object-literal rules are JavaScript's).
fn is_lexically_reachable(ctx: &Ctx, n: SymId, r: &Ref) -> bool {
    let c = ctx.sym(n);
    let within = |(start, end): (u32, u32)| r.file == c.file && r.line >= start && r.line <= end;
    if matches!(c.kind, NodeKind::Variable | NodeKind::Constant)
        || (c.kind == NodeKind::Field && c.language == Language::Scala)
    {
        return local_declaration_scope(ctx, n).is_none_or(within);
    }
    if !matches!(
        c.kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Interface
            | NodeKind::Trait
            | NodeKind::TypeAlias
    ) {
        return true;
    }
    if matches!(c.language, Language::C | Language::Cpp) {
        return true;
    }
    lexical_scope_of(ctx, n).is_none_or(within)
}

/// `isCrossFileReachable`, for a C# reference: never a Markdown node (the
/// index holds none), nor an ES module binding, which a .NET reference never
/// reaches past the language gate anyway.
fn is_cross_file_reachable(_ctx: &Ctx, _n: SymId) -> bool {
    true
}

/// `isVisibleAcrossFiles`, for what a C# reference can reach.
pub fn is_visible_across_files(ctx: &Ctx, n: SymId, r: &Ref) -> bool {
    let c = ctx.sym(n);
    if c.file == r.file {
        return true;
    }
    if js::test(&ctx.re(r"(?i)[.-]min\.m?js$"), &c.file) {
        return false;
    }
    if nm::is_test_suite_path(ctx, &c.file) && !codegraph_core::search::is_test_path(&r.file) {
        return false;
    }
    if c.language == Language::CSharp
        && scope::is_type_kind(c.kind)
        && is_bare_name(ctx, &r.name)
        && (!scope::is_type_visible(ctx, n, r) || !scope::is_nested_type_in_scope(ctx, n, r))
    {
        return false;
    }
    match c.language {
        Language::C | Language::Cpp => nm::is_visible_across_files(ctx, n, r),
        Language::ObjC if nm::is_objc_file_local_elsewhere(ctx, n, r) => false,
        Language::Kotlin
        | Language::Java
        | Language::CSharp
        | Language::Swift
        | Language::Scala
        | Language::Php => c.visibility != Some(Visibility::Private),
        _ => true,
    }
}

/// `gateLanguageMatch`: a .NET reference's pick never crosses a code boundary
/// (bridge evidence is a JavaScript or native caller's).
fn gate_language(ctx: &Ctx, m: Option<Match>, r: &Ref) -> Option<Match> {
    let m = m?;
    (!crosses_code_boundary(r.lang, ctx.sym(m.target).language)).then_some(m)
}

// ---------------------------------------------------------------- strategies

/// `matchByQualifiedName`, for a C# reference.
fn match_by_qualified_name(ctx: &Ctx, r: &Ref) -> Option<Match> {
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
    let mut candidates = keep(ctx.by_qname(&r.name));
    if r.kind == ReferenceKind::Imports {
        let namespaces: Vec<SymId> = candidates
            .iter()
            .copied()
            .filter(|&n| ctx.kind(n) == NodeKind::Namespace)
            .collect();
        candidates = if !namespaces.is_empty() {
            prefer_call_site_file(ctx, &namespaces, &r.file)
                .into_iter()
                .take(1)
                .collect()
        } else {
            candidates
                .into_iter()
                .filter(|&n| ctx.kind(n) != NodeKind::Import || ctx.sym(n).file == r.file)
                .collect()
        };
    }
    if candidates.len() == 1 {
        return Some(Match::new(candidates[0], "qualified-name", 0.95));
    }
    if candidates.len() > 1 {
        let ordered = prefer_call_site_file(ctx, &candidates, &r.file);
        if ctx.sym(ordered[0]).file == r.file {
            return Some(Match::new(ordered[0], "qualified-name", 0.95));
        }
    }
    let last = r.name.rsplit([':', '.']).next().unwrap_or("");
    if !last.is_empty() {
        let partial: Vec<SymId> = keep(ctx.named(last))
            .into_iter()
            .filter(|&c| ends_at_scope_boundary(&ctx.sym(c).qualified_name, &r.name))
            .collect();
        let own: Vec<SymId> = partial
            .iter()
            .copied()
            .filter(|&c| same_language_family(ctx.sym(c).language, r.lang))
            .collect();
        let pool = if own.is_empty() { partial } else { own };
        if let Some(&chosen) = prefer_call_site_file(ctx, &pool, &r.file).first() {
            return Some(Match::new(chosen, "qualified-name", 0.85));
        }
    }
    None
}

/// The candidate filters matchByExactName and matchFuzzy share for a C# reference.
fn passes_csharp_filters(ctx: &Ctx, n: SymId, r: &Ref, bare: bool, type_ref: bool) -> bool {
    !(bare && !scope::is_member_in_scope(ctx, n, r))
        && !nm::is_objc_file_local_elsewhere(ctx, n, r)
        && !(type_ref && !scope::can_name_in_type_position(ctx, n))
}

/// `matchByExactName`, for a C# reference.
fn match_by_exact_name(ctx: &Ctx, r: &Ref) -> Option<Match> {
    let value_ref = matches!(
        r.kind,
        ReferenceKind::References | ReferenceKind::FunctionRef
    );
    let import_ref = r.kind == ReferenceKind::Imports;
    let inheritance = is_inheritance_ref(r);
    let bare = matches!(r.kind, ReferenceKind::Calls | ReferenceKind::References)
        && is_bare_name(ctx, &r.name);
    let type_ref = scope::is_type_position_ref(ctx, r);
    let candidates: Vec<SymId> = ctx
        .named(&r.name)
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            passes_csharp_filters(ctx, n, r, bare, type_ref)
                && (!value_ref || same_language_family(s.language, r.lang))
                && s.kind != NodeKind::Import
                && (!inheritance || is_supertype_target(ctx, n))
                && (!import_ref || is_importable_kind(s.kind))
                && is_lexically_reachable(ctx, n, r)
                && !(s.language == Language::CSharp
                    && scope::is_type_kind(s.kind)
                    && is_bare_name(ctx, &r.name)
                    && (!scope::is_nested_type_in_scope(ctx, n, r)
                        || (s.file != r.file && !scope::is_type_visible(ctx, n, r))))
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    if candidates.len() == 1 {
        if !is_cross_file_reachable(ctx, candidates[0]) {
            return None;
        }
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
    let best = find_best_match(ctx, r, &candidates)?;
    if !is_cross_file_reachable(ctx, best) {
        return None;
    }
    let proximity = includes::path_proximity(&r.file, &ctx.sym(best).file);
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

/// `matchFuzzy`, for a C# reference (names are case-sensitive there).
fn match_fuzzy(ctx: &Ctx, r: &Ref) -> Option<Match> {
    let bare = matches!(r.kind, ReferenceKind::Calls | ReferenceKind::References)
        && is_bare_name(ctx, &r.name);
    let type_ref = scope::is_type_position_ref(ctx, r);
    let callable: Vec<SymId> = ctx
        .named(&r.name)
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            matches!(
                s.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Class
            ) && !(type_ref && !scope::can_name_in_type_position(ctx, n))
                && !(r.kind == ReferenceKind::Instantiates && s.kind == NodeKind::Method)
                && !nm::is_objc_file_local_elsewhere(ctx, n, r)
                && !(bare && !scope::is_member_in_scope(ctx, n, r))
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
    let pool = if same.is_empty() { callable } else { same };
    if pool.len() == 1
        && is_visible_across_files(ctx, pool[0], r)
        && is_cross_file_reachable(ctx, pool[0])
        && is_lexically_reachable(ctx, pool[0], r)
    {
        let cross = ctx.sym(pool[0]).language != r.lang;
        return Some(Match::new(pool[0], "fuzzy", if cross { 0.3 } else { 0.5 }));
    }
    None
}

/// `matchFunctionRef`, for a C# reference: a method group may name a method.
fn match_function_ref(ctx: &Ctx, r: &Ref) -> Option<Match> {
    if r.name.starts_with("this.") {
        return None;
    }
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
        return Some(Match::new(first_by_line(ctx, pool), "function-ref", 0.9));
    }
    let named: Vec<SymId> = ctx
        .named(&r.name)
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            matches!(s.kind, NodeKind::Function | NodeKind::Method)
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
        let conf = if same.len() == 1 { 0.95 } else { 0.9 };
        return Some(Match::new(first_by_line(ctx, &same), "function-ref", conf));
    }
    if candidates.len() == 1 && named.len() == 1 {
        return Some(Match::new(candidates[0], "function-ref", 0.8));
    }
    None
}

/// `matchDottedCallChain`, for a C# reference: `Receiver.factory(…).method()`.
fn match_dotted_call_chain(ctx: &Ctx, r: &Ref) -> Option<Match> {
    let m = js::caps(&ctx.re(r"^(.+)\(\)\.(\w+)$"), &r.name)?;
    let inner = js::group(&m, 1);
    let method = js::group(&m, 2);
    let last_dot = inner.rfind('.').filter(|&d| d > 0)?;
    let factory_class = inner[..last_dot].rsplit('.').next().unwrap_or("");
    let factory_method = &inner[last_dot + 1..];
    if factory_class.is_empty() || factory_method.is_empty() {
        return None;
    }
    let ret = nm::lookup_callee_return_type(ctx, &format!("{factory_class}::{factory_method}"), r)?;
    resolve_method_on_type(ctx, &ret, method, r, 0.85, "instance-method", 0)
}

// ---------------------------------------------------------------- method calls

/// `findOwnMethod`.
fn find_own_method(ctx: &Ctx, cls: SymId, method: &str) -> Option<SymId> {
    let c = ctx.sym(cls);
    let methods: Vec<SymId> = ctx
        .in_file(&c.file)
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            s.kind == NodeKind::Method && s.name == method
        })
        .collect();
    let exact = format!("{}::{method}", c.qualified_name);
    let by_name = format!("{}::{method}", c.name);
    let suffix = format!("::{by_name}");
    methods
        .iter()
        .copied()
        .find(|&n| ctx.sym(n).qualified_name == exact)
        .or_else(|| {
            methods.iter().copied().find(|&n| {
                let q = &ctx.sym(n).qualified_name;
                *q == by_name || q.ends_with(&suffix)
            })
        })
}

fn owner_segment(qn: &str) -> &str {
    let cut = qn.rfind("::").unwrap_or(0);
    qn[..cut]
        .rsplit([':', '.'])
        .find(|s| !s.is_empty())
        .unwrap_or("")
}

/// `inheritedClassMethod`: a method named `name` on a supertype of the given classes, nearest first.
fn inherited_class_method(ctx: &Ctx, classes: &[SymId], name: &str) -> Option<SymId> {
    let mut seen: HashSet<SymId> = classes.iter().copied().collect();
    let mut frontier: Vec<SymId> = classes.to_vec();
    for _ in 0..5 {
        if frontier.is_empty() {
            break;
        }
        let mut next = Vec::new();
        for &cls in &frontier {
            let lang = ctx.sym(cls).language;
            let supers = if lang == Language::CSharp {
                infer::class_head_supertypes(ctx, cls).to_vec()
            } else {
                Vec::new()
            };
            for sup in supers {
                for &d in ctx.named(&sup) {
                    let s = ctx.sym(d);
                    if s.language != lang || !is_method_owner_kind(ctx, d) || seen.contains(&d) {
                        continue;
                    }
                    seen.insert(d);
                    let method = ctx.in_file(&s.file).iter().copied().find(|&n| {
                        let m = ctx.sym(n);
                        m.kind == NodeKind::Method
                            && m.name == name
                            && owner_segment(&m.qualified_name) == s.name
                    });
                    if let Some(m) = method {
                        return Some(m);
                    }
                    next.push(d);
                }
            }
        }
        frontier = next;
    }
    None
}

/// `classStrategyMethod`: a class's own method, else the nearest one a declared supertype has.
fn class_strategy_method(ctx: &Ctx, cls: SymId, method: &str) -> Option<SymId> {
    find_own_method(ctx, cls, method).or_else(|| inherited_class_method(ctx, &[cls], method))
}

/// `flatWord`.
fn flat_word(ctx: &Ctx, w: &str) -> String {
    let letters: String = w.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    js::replace_with(&ctx.re(r"\d+$"), &letters, "").to_lowercase()
}

/// `namesOwnerType`.
fn names_owner_type(ctx: &Ctx, receiver: &str, method: SymId) -> bool {
    let q = &ctx.sym(method).qualified_name;
    let Some(cut) = q.rfind("::") else {
        return false;
    };
    let owner = q[..cut]
        .rsplit([':', '.'])
        .find(|s| !s.is_empty())
        .unwrap_or("");
    flat_word(ctx, receiver.rsplit('.').next().unwrap_or(receiver)) == flat_word(ctx, owner)
}

/// `sharesReceiverWord`.
fn shares_receiver_word(ctx: &Ctx, receiver: &str, method: SymId) -> bool {
    if names_owner_type(ctx, receiver, method) {
        return true;
    }
    let q = &ctx.sym(method).qualified_name;
    let Some(cut) = q.rfind("::") else {
        return false;
    };
    let owner: HashSet<String> = nm::split_camel_case(ctx, &q[..cut])
        .iter()
        .map(|w| flat_word(ctx, w))
        .filter(|w| w.chars().count() > 2)
        .collect();
    nm::split_camel_case(ctx, receiver)
        .iter()
        .any(|w| owner.contains(&flat_word(ctx, w)))
}

fn receiver_link<'s>(ctx: &Ctx, receiver: &'s str) -> &'s str {
    let links: Vec<&str> = receiver.split('.').collect();
    let last = links[links.len() - 1];
    if links.len() > 1 && js::test(&ctx.re(r"^[A-Z][A-Z0-9_]+$"), last) {
        links[links.len() - 2]
    } else {
        last
    }
}

const TEST_DOUBLE_OWNER: &str = r"(?i)\b(?:fake|mock|mocked|stub|dummy|spy)\b";

fn is_unnamed_test_double(ctx: &Ctx, method: SymId, receiver: &str, r: &Ref) -> bool {
    let q = &ctx.sym(method).qualified_name;
    if q.rfind("::").is_none_or(|c| c == 0) {
        return false;
    }
    let owner = owner_segment(q);
    let double = ctx.re(TEST_DOUBLE_OWNER);
    if !js::test(&double, &nm::split_camel_case(ctx, owner).join(" ")) {
        return false;
    }
    if nm::split_camel_case(ctx, receiver_link(ctx, receiver))
        .iter()
        .any(|w| js::test(&double, w))
    {
        return false;
    }
    !ctx.source(&r.file).is_some_and(|s| s.text.contains(owner))
}

const DISPATCHED_ACTIONS: &[&str] = &[
    "get",
    "post",
    "put",
    "patch",
    "delete",
    "head",
    "options",
    "index",
    "show",
    "store",
    "update",
    "destroy",
    "create",
    "edit",
    "list",
    "retrieve",
    "partial_update",
];

/// `matchMethodCall`, for a C# reference.
fn match_method_call(ctx: &Ctx, r: &Ref) -> Option<Match> {
    let dot = js::caps(&ctx.re(r"^([\w.]+)\.(\w+:?(?:\w+:)*)$"), &r.name);
    let colon = if dot.is_none() {
        js::caps(&ctx.re(r"^(\w+)::(\w+)$"), &r.name)
    } else {
        None
    };
    let m = dot.as_ref().or(colon.as_ref())?;
    let object = js::group(m, 1).to_string();
    let method = js::group(m, 2).to_string();

    if dot.is_some() {
        let mut inferred = infer::infer_local_receiver_type(ctx, &object, r);
        if inferred.is_none() {
            inferred = infer::infer_member_receiver_type(ctx, &object, r);
            if inferred
                .as_deref()
                .is_some_and(|t| t.starts_with(|c: char| c.is_ascii_lowercase()))
            {
                return None;
            }
        }
        if let Some(t) = inferred.clone() {
            if !ctx.named(&t).iter().any(|&n| is_method_owner_kind(ctx, n)) {
                match infer::type_parameter_bound(ctx, &t, r) {
                    Bound::Unbounded => return None,
                    Bound::Bound(b) => inferred = Some(b),
                    Bound::NotAParameter => {}
                }
            }
        }
        if let Some(t) = &inferred {
            if let Some(typed) =
                resolve_method_on_type(ctx, t, &method, r, 0.9, "instance-method", 0)
            {
                return Some(typed);
            }
            if nm::is_undeclared_type_name(ctx, t.rsplit('.').next().unwrap_or(t), r) {
                return None;
            }
        }
    }

    // Strategy 1: a class the receiver names.
    let production = !codegraph_core::search::is_test_path(&r.file);
    let owners: Vec<SymId> = ctx
        .named(&object)
        .iter()
        .copied()
        .filter(|&n| is_method_owner_kind(ctx, n))
        .collect();
    let mut classes = prefer_call_site_file(ctx, &owners, &r.file);
    if classes.len() > 1 {
        let type_ref = r.with_name(object.clone());
        let (visible, rest): (Vec<SymId>, Vec<SymId>) = classes.iter().partition(|&&c| {
            ctx.sym(c).language != Language::CSharp || scope::is_type_visible(ctx, c, &type_ref)
        });
        classes = visible.into_iter().chain(rest).collect();
    }
    for &cls in &classes {
        let c = ctx.sym(cls);
        if c.language != r.lang
            || !is_lexically_reachable(ctx, cls, r)
            || (production && codegraph_core::search::is_test_path(&c.file))
        {
            continue;
        }
        if let Some(found) = class_strategy_method(ctx, cls, &method) {
            return Some(Match::new(found, "qualified-name", 0.85));
        }
    }
    let same_lang: Vec<SymId> = classes
        .iter()
        .copied()
        .filter(|&c| ctx.sym(c).language == r.lang)
        .collect();
    if let Some(found) = inherited_class_method(ctx, &same_lang, &method) {
        return Some(Match::new(found, "qualified-name", 0.8));
    }

    if let Some(aliased) = scope::using_alias(ctx, &object, r) {
        return resolve_method_on_type(ctx, &aliased, &method, r, 0.9, "instance-method", 0);
    }
    // A capitalized receiver the project declares no type or namespace of is a type from outside it.
    if js::test(&ctx.re(r"^[A-Z][A-Za-z0-9_]*$"), &object)
        && !ctx.named(&object).iter().any(|&n| {
            let s = ctx.sym(n);
            same_language_family(s.language, r.lang)
                && (is_method_owner_kind(ctx, n)
                    || matches!(
                        s.kind,
                        NodeKind::Enum
                            | NodeKind::Namespace
                            | NodeKind::Module
                            | NodeKind::Trait
                            | NodeKind::TypeAlias
                    ))
        })
    {
        return None;
    }
    if js::test(&ctx.re(r"^(?:string|object|int|long|short|byte|bool|char|double|float|decimal|uint|ulong|ushort|sbyte)$"), &object) {
        return None;
    }

    // Strategy 2: a class named like the receiver, capitalized.
    let mut chars = object.chars();
    let capitalized: String = chars
        .next()
        .map(|f| f.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    if capitalized != object {
        let owners: Vec<SymId> = ctx
            .named(&capitalized)
            .iter()
            .copied()
            .filter(|&n| is_method_owner_kind(ctx, n))
            .collect();
        for &cls in &prefer_call_site_file(ctx, &owners, &r.file) {
            let c = ctx.sym(cls);
            if c.language != r.lang
                || !is_lexically_reachable(ctx, cls, r)
                || (production && codegraph_core::search::is_test_path(&c.file))
            {
                continue;
            }
            if let Some(found) = class_strategy_method(ctx, cls, &method) {
                return Some(Match::new(found, "instance-method", 0.8));
            }
        }
    }

    // Strategy 3: a method of the name, by the receiver's words.
    let candidates = ctx.named(&method);
    if candidates.len() > AMBIGUOUS_NAME_CEILING {
        return None;
    }
    let methods: Vec<SymId> = candidates
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            s.kind == NodeKind::Method && s.name == method
        })
        .collect();
    let same: Vec<SymId> = methods
        .iter()
        .copied()
        .filter(|&n| ctx.sym(n).language == r.lang)
        .collect();
    let mut target = if same.is_empty() { methods } else { same };
    let dispatched = ctx.re(r"(?:View|ViewSet|APIView|Controller|Endpoint|ViewMixin)$");
    let kept: Vec<SymId> = target
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            !(DISPATCHED_ACTIONS.contains(&s.name.as_str())
                && js::test(&dispatched, owner_segment(&s.qualified_name)))
        })
        .collect();
    let narrowed = kept.len() != target.len();
    target = kept;
    if production {
        target.retain(|&n| !codegraph_core::search::is_test_path(&ctx.sym(n).file));
    }
    let reachable: Vec<SymId> = target
        .iter()
        .copied()
        .filter(|&n| is_lexically_reachable(ctx, n, r))
        .collect();
    let scoped_out = reachable.len() != target.len();
    target = reachable;
    let selfish = js::test(&ctx.re(r"^(?:self|Self|this|base)$"), &object);
    let std = is_std_method_name(&method) && !selfish;
    if target.len() == 1
        && !narrowed
        && !scoped_out
        && ctx.sym(target[0]).language == r.lang
        && !is_unnamed_test_double(ctx, target[0], &object, r)
        && !(std && !shares_receiver_word(ctx, receiver_link(ctx, &object), target[0]))
    {
        if target[0] == r.from
            && !matches!(object.as_str(), "this" | "self" | "Self" | "super" | "cls")
        {
            return None;
        }
        return Some(Match::new(target[0], "instance-method", 0.7));
    }
    if target.len() > 1 || (target.len() == 1 && scoped_out && !narrowed) {
        let link = receiver_link(ctx, &object);
        let words = nm::split_camel_case(ctx, link);
        let head = words.last().map(|w| w.to_lowercase());
        let whole = link.replace('_', "").to_lowercase();
        let double = ctx.re(TEST_DOUBLE_OWNER);
        let mut best: Option<SymId> = None;
        let mut best_score = 0i64;
        for m in prefer_call_site_file(ctx, &target, &r.file) {
            if std && !shares_receiver_word(ctx, link, m) {
                continue;
            }
            let s = ctx.sym(m);
            let cut = s.qualified_name.rfind("::").filter(|&c| c > 0);
            let owner = cut.map(|_| owner_segment(&s.qualified_name));
            let class_words = owner
                .map(|o| nm::split_camel_case(ctx, o))
                .unwrap_or_default();
            let mut score = words
                .iter()
                .filter(|w| {
                    class_words
                        .iter()
                        .any(|cw| cw.to_lowercase() == w.to_lowercase())
                })
                .count() as i64;
            if js::test(&double, &class_words.join(" "))
                && !words.iter().any(|w| js::test(&double, w))
                && !ctx
                    .source(&r.file)
                    .is_some_and(|src| src.text.contains(owner.unwrap_or("")))
            {
                continue;
            }
            if head.is_some() && head == class_words.last().map(|w| w.to_lowercase()) {
                score += 1;
            }
            if !class_words.is_empty() && whole == class_words.join("").to_lowercase() {
                score += 2;
            }
            if s.language == r.lang {
                score += 1;
            }
            if score > best_score {
                best_score = score;
                best = Some(m);
            }
        }
        if let Some(best) = best.filter(|&b| best_score >= 2 && b != r.from) {
            return Some(Match::new(best, "instance-method", 0.65));
        }
    }
    None
}

// ---------------------------------------------------------------- pipeline

/// `matchReferenceInner`, for a C# reference.
fn match_reference_inner(ctx: &Ctx, r: &Ref) -> Option<Match> {
    if r.kind == ReferenceKind::FunctionRef {
        return match_function_ref(ctx, r);
    }
    includes::match_by_file_path(ctx, r)
        .or_else(|| match_by_qualified_name(ctx, r))
        .or_else(|| match_dotted_call_chain(ctx, r))
        .or_else(|| match_method_call(ctx, r))
        .or_else(|| match_by_exact_name(ctx, r))
        .or_else(|| match_fuzzy(ctx, r))
}

/// `matchReference`, for a C# reference.
fn match_reference(ctx: &Ctx, r: &Ref) -> Option<Match> {
    let result = gate_language(ctx, match_reference_inner(ctx, r), r)?;
    if result.target == r.from && nm::is_collapsed_non_recursion(ctx, r) {
        return None;
    }
    if result.target == r.from && is_inheritance_ref(r) {
        return cpp_resolve::other_supertype_named(ctx, r);
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
    Some(nm::retarget_self_overload(ctx, result, r))
}

/// `resolveOneInner`, for a C# reference (framework resolvers aren't ported;
/// C# has no import mappings, so the import strategy never answers).
fn resolve_inner(ctx: &Ctx, r: &Ref) -> Option<Match> {
    if !cpp_resolve::has_any_possible_match(ctx, &r.name) {
        return None;
    }
    if r.kind == ReferenceKind::FunctionRef {
        return gate_language(ctx, match_function_ref(ctx, r), r);
    }
    gate_language(ctx, match_reference(ctx, r), r)
        .filter(|m| is_visible_across_files(ctx, m.target, r))
}

/// type-parameters.ts `declaredTypeParameters`, for a declaration of a
/// language other than C++: the `<…>` after its name, and one before it
/// detached from any type name.
fn declared_type_parameters(ctx: &Ctx, n: SymId) -> std::rc::Rc<Vec<String>> {
    ctx.memo(crate::cpp::ctx::memo::NM, format!("cs-tp\0{n}"), || {
        let s = ctx.sym(n);
        let mut names: Vec<String> = Vec::new();
        let Some(src) = ctx.source(&s.file) else {
            return std::rc::Rc::new(names);
        };
        let from = (s.start_line as usize)
            .saturating_sub(1)
            .min(src.lines.len());
        let to = (s.end_line as usize)
            .min(from + 40)
            .min(src.lines.len())
            .max(from);
        let head = src.lines[from..to].join("\n");
        let Some(at) = name_index(&head, &s.name) else {
            return std::rc::Rc::new(names);
        };
        let b = head.as_bytes();
        let mut open = at + s.name.len();
        while open < b.len() && matches!(b[open], b' ' | b'\t') {
            open += 1;
        }
        if open < b.len() && matches!(b[open], b'<' | b'[') {
            collect(ctx, &head, open, &mut names);
        }
        let before = &head[..at];
        let bb = before.as_bytes();
        for i in 0..bb.len() {
            if bb[i] == b'<'
                && (i == 0 || matches!(bb[i - 1], b' ' | b'\t' | b'\n' | b'\r' | b'(' | b','))
            {
                collect(ctx, before, i, &mut names);
            }
        }
        std::rc::Rc::new(names)
    })
}

fn name_index(head: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let word = |c: u8| js::is_word(c) || c == b'$';
    let mut from = 0;
    while let Some(at) = js::index_of(head, name, from) {
        if !word(js::before(head, at)) && !word(js::at(head, at + name.len())) {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// type-parameters.ts `collect` (not C++): the parameter names of the bracket group at `open`.
fn collect(ctx: &Ctx, text: &str, open: usize, names: &mut Vec<String>) {
    let b = text.as_bytes();
    let angle = b[open] == b'<';
    let closer = if angle {
        b'>'
    } else if b[open] == b'[' {
        b']'
    } else {
        b')'
    };
    let opener = |c: u8| c == b'[' || c == b'(' || (angle && c == b'<');
    let is_closer = |c: u8, prev: u8| {
        c == b']' || c == b')' || (angle && c == b'>' && prev != b'-' && prev != b'=')
    };
    let mut depth = 0i32;
    let mut close = None;
    for i in open..b.len() {
        let c = b[i];
        if c == b'{' || c == b';' {
            return;
        }
        if opener(c) {
            depth += 1;
        } else if is_closer(c, if i > 0 { b[i - 1] } else { 0 }) {
            depth -= 1;
            if depth == 0 {
                if c == closer {
                    close = Some(i);
                }
                break;
            }
        }
    }
    let Some(close) = close else { return };
    let inner = &text[open + 1..close];
    let ib = inner.as_bytes();
    let mut items = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for i in 0..ib.len() {
        let c = ib[i];
        if opener(c) {
            depth += 1;
        } else if is_closer(c, if i > 0 { ib[i - 1] } else { 0 }) {
            depth -= 1;
        } else if c == b',' && depth == 0 {
            items.push(&inner[start..i]);
            start = i + 1;
        }
    }
    items.push(&inner[start..]);
    let annotations = ctx.re(r"^\s*(?:@[\w.]+(?:\([^()]*\))?\s*)+");
    let param = ctx
        .re(r"^\s*(?:(?:in|out|reified|const|typename|class|struct)\s+|[+\-']\s*)*([A-Za-z_]\w*)");
    for item in items {
        let bare = js::replace_with(&annotations, item, "");
        if let Some(m) = js::caps(&param, &bare) {
            let p = js::group(&m, 1);
            if p != "typename" && p != "class" && !names.iter().any(|n| n == p) {
                names.push(p.to_string());
            }
        }
    }
}

/// type-parameters.ts `gateTypeParameter`, for a C# reference: a type
/// position naming a type parameter of a declaration around it names that parameter.
fn gate_type_parameter(ctx: &Ctx, m: Option<Match>, r: &Ref) -> Option<Match> {
    let m = m?;
    if !matches!(
        r.kind,
        ReferenceKind::References
            | ReferenceKind::TypeOf
            | ReferenceKind::Returns
            | ReferenceKind::Instantiates
    ) || !js::test(&ctx.re(r"^[A-Z][A-Za-z0-9_]*$"), &r.name)
    {
        return Some(m);
    }
    for d in cpp_resolve::enclosing_declarations(ctx, &r.file, r.line) {
        if declared_type_parameters(ctx, d)
            .iter()
            .any(|p| *p == r.name)
        {
            return None;
        }
    }
    Some(m)
}

/// `resolveOneIn`, for a C# reference.
pub fn resolve(ctx: &Ctx, r: &Ref) -> Option<Match> {
    let m = gate_type_parameter(
        ctx,
        cpp_resolve::gate_target_kind(ctx, resolve_inner(ctx, r), r),
        r,
    );
    let call = cpp_resolve::gate_super_self_call(ctx, gate_language(ctx, m, r), r)?;
    if r.kind != ReferenceKind::Calls {
        return Some(call);
    }
    let member = r.name.rfind('.').map(|d| &r.name[d + 1..]);
    match cpp_resolve::resolve_alias_binding(ctx, call.target, member) {
        Some(forwarded) if forwarded != call.target => gate_language(
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
