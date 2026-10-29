//! Calls on C++ receivers whose types are known: held and iterated types,
//! declared types, member chains, unspelled receivers, implicit `this`, and
//! the overload a call reaches (name-matcher.ts, from
//! `isCppCallersDeclaration` to `cppQualifierSpecializationPool`).

use super::*;

/// Is line `line` of `file`, which declares a receiver, the calling
/// function's own code, or a member declaration of the caller's class?
pub fn is_cpp_callers_declaration(ctx: &Ctx, file: &str, line: u32, r: &Ref) -> bool {
    let c = ctx.sym(r.from);
    if !matches!(c.kind, NodeKind::Method | NodeKind::Function) {
        return false;
    }
    if file == r.file && line >= c.start_line && line <= r.line {
        return true;
    }
    let Some(cut) = c.qualified_name.rfind("::") else {
        return false;
    };
    let mut innermost: Option<SymId> = None;
    for &cls in cpp_classes_in(ctx, file).iter() {
        let s = ctx.sym(cls);
        if line < s.start_line || line > s.end_line.max(s.start_line) {
            continue;
        }
        if innermost.is_none_or(|i| s.start_line >= ctx.sym(i).start_line) {
            innermost = Some(cls);
        }
    }
    innermost.is_some_and(|i| ctx.sym(i).qualified_name == c.qualified_name[..cut])
}

/// The operator the member call at `r` is written with, or None.
pub fn cpp_member_operator(ctx: &Ctx, receiver: &str, r: &Ref) -> Option<&'static str> {
    let src = ctx.source(&r.file)?;
    let line = src.lines.get((r.line as usize).checked_sub(1)?)?;
    let at = js::slice_from(line, r.column as usize);
    if !at.starts_with(receiver) {
        return None;
    }
    let mut rest = at[receiver.len()..].to_string();
    let mut next = r.line as usize;
    while rest.trim().is_empty() && next < src.lines.len().min(r.line as usize + 3) {
        rest = src.lines[next].clone();
        next += 1;
    }
    let op = js::caps(&ctx.re(r"^\s*(\.|->)"), &rest)?;
    match js::group(&op, 1) {
        "." => Some("."),
        "->" => Some("->"),
        _ => None,
    }
}

pub fn is_cpp_dot_call(ctx: &Ctx, receiver: &str, r: &Ref) -> bool {
    cpp_member_operator(ctx, receiver, r) == Some(".")
}

fn count_char(s: &str, c: char) -> usize {
    s.chars().filter(|&x| x == c).count()
}

/// Is a C++ declared type one from outside the project with a lowercase name?
pub fn is_cpp_library_type(ctx: &Ctx, raw: &str) -> bool {
    if count_char(raw, '<') != count_char(raw, '>') {
        return false;
    }
    let Some(segments) = aliases::type_segments(ctx, raw) else {
        return false;
    };
    if segments.len() < 2 {
        return false;
    }
    let name = segments.last().unwrap();
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) || name.ends_with("_t") {
        return false;
    }
    if !js::test(&ctx.re(r"^(?:const_)?(?:reverse_|local_)?iterator$"), name)
        && is_cpp_specialization_member(raw)
    {
        return false;
    }
    if segments[..segments.len() - 1]
        .iter()
        .any(|s| !cpp_types_named(ctx, s).is_empty())
    {
        return false;
    }
    let declared = cpp_types_named(ctx, name);
    if segments[0] != "std" {
        return declared.is_empty() || cpp_finds_only_namesakes(ctx, raw, None);
    }
    let spelled = segments.join("::");
    !declared.iter().any(|&n| {
        let q = &ctx.sym(n).qualified_name;
        *q == spelled || q.ends_with(&format!("::{spelled}"))
    })
}

/// Is a C++ type written as a member of a template's specialization?
fn is_cpp_specialization_member(raw: &str) -> bool {
    let b = raw.as_bytes();
    let mut depth = 0i32;
    for i in 0..b.len() {
        if b[i] == b'<' {
            depth += 1;
        } else if b[i] == b'>' {
            depth -= 1;
            if depth == 0 && raw[i + 1..].trim_start().starts_with("::") {
                return true;
            }
        }
    }
    false
}

const CPP_HOLDERS: &[&str] = &[
    "std::unique_ptr",
    "std::shared_ptr",
    "std::optional",
    "absl::optional",
    "absl::StatusOr",
    "absl::NoDestructor",
];

/// The type a standard smart pointer, optional, StatusOr or NoDestructor holds.
pub fn cpp_held_type(ctx: &Ctx, raw: &str) -> Option<String> {
    let holder = aliases::type_segments(ctx, raw)?;
    if holder.len() != 2 || !CPP_HOLDERS.contains(&holder.join("::").as_str()) {
        return None;
    }
    aliases::template_arguments(raw)?.into_iter().next()
}

/// What `->` on a C++ value of the type `raw` reaches, as written.
fn cpp_arrow_target(
    ctx: &Ctx,
    raw: &str,
    site: &WrittenType,
    declared: Option<&WrittenType>,
    r: &Ref,
) -> Option<WrittenType> {
    if let Some(held) = cpp_held_type(ctx, raw) {
        return is_cpp_library_holder(ctx, raw).then(|| site.with_text(held));
    }
    cpp_iterator_target(ctx, &declared.unwrap_or(site).with_text(raw), r)
}

/// The element a container's iterator of the type `iterator` points at.
fn cpp_iterator_target(ctx: &Ctx, iterator: &WrittenType, at: &Ref) -> Option<WrittenType> {
    let bare = cpp_without_type_keywords(ctx, &iterator.text);
    let container = js::caps(
        &ctx.re(r"^(.*>)\s*::\s*(?:const_)?(?:reverse_)?iterator\s*$"),
        &bare,
    )
    .map(|c| js::group(&c, 1).to_string());
    let element = match container {
        Some(c) => cpp_range_element_hops(ctx, &iterator.with_text(c), at, 1),
        None => cpp_operator_target(ctx, iterator, at, 0),
    }?;
    (element.scope == iterator.scope || !is_cpp_template_parameter_type(ctx, &element))
        .then_some(element)
}

/// What `->` on a value of one of the project's classes reaches.
fn cpp_operator_target(ctx: &Ctx, ty: &WrittenType, at: &Ref, hops: u32) -> Option<WrittenType> {
    let written = cpp_without_type_keywords(ctx, &ty.text);
    if hops > 2
        || written.is_empty()
        || aliases::is_pointer_type(&written)
        || is_cpp_library_type(ctx, &written)
    {
        return None;
    }
    let found = match supertypes::class_written(ctx, &written, &ty.site(), at) {
        Tri::Found(f) if f.exact && CPP_CLASS_KINDS.contains(&ctx.kind(f.node)) => f.node,
        _ => return None,
    };
    let arrow = cpp_methods_of(ctx, found, "operator->", at);
    let returned = cpp_return_type_of(
        ctx,
        &if arrow.is_empty() {
            cpp_methods_of(ctx, found, "operator*", at)
        } else {
            arrow.clone()
        },
    )?;
    let bound = cpp_template_bound(ctx, &returned, found, &ty.with_text(written.clone()), at)?;
    if arrow.is_empty() {
        let t = js::replace_with(&ctx.re(r"&+\s*$"), &bound.text, "");
        return Some(bound.with_text(t.trim()));
    }
    cpp_pointee(ctx, &bound).or_else(|| cpp_operator_target(ctx, &bound, at, hops + 1))
}

/// `returned` with the class template `cls`'s own parameters bound to `ty`'s arguments.
fn cpp_template_bound(
    ctx: &Ctx,
    returned: &WrittenType,
    cls: SymId,
    ty: &WrittenType,
    at: &Ref,
) -> Option<WrittenType> {
    let params = cpp_own_template_parameters(ctx, cls);
    if params.is_empty() {
        return Some(returned.clone());
    }
    let args = aliases::template_arguments(&ty.text).unwrap_or_default();
    let followed =
        cpp_follow_written_aliases(ctx, returned, at).unwrap_or_else(|| returned.clone());
    let core = js::replace_with(
        &ctx.re(r"\b(?:const|volatile|typename)\b|[*&]"),
        &followed.text,
        " ",
    )
    .trim()
    .to_string();
    if let Some(index) = params.iter().position(|p| *p == core) {
        let arg = args
            .get(index)
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())?;
        let re = ctx.re(&format!(r"\b{}\b", js::escape(&core)));
        return Some(ty.with_text(js::replace_first(&re, &followed.text, |_| arg.clone())));
    }
    let mut text = followed.text.clone();
    for (i, param) in params.iter().enumerate() {
        let arg = args
            .get(i)
            .map(|a| a.trim().to_string())
            .unwrap_or_default();
        if !param.is_empty() && !arg.is_empty() {
            text = js::replace_with(&ctx.re(&format!(r"\b{}\b", js::escape(param))), &text, &arg);
        }
    }
    Some(followed.with_text(text))
}

/// What dereferencing a value of the type `ty` gives, for `*x` and `x->`.
pub fn cpp_dereferenced(ctx: &Ctx, ty: &WrittenType, at: &Ref) -> Option<WrittenType> {
    if ty.array.is_some_and(|a| a > 0) {
        return Some(cpp_array_element(ty));
    }
    cpp_pointee(ctx, ty).or_else(|| cpp_iterator_target(ctx, ty, at))
}

/// Whether the holder `raw` names is the library's.
fn is_cpp_library_holder(ctx: &Ctx, raw: &str) -> bool {
    let spelled = aliases::type_segments(ctx, raw)
        .map(|s| s.join("::"))
        .unwrap_or_default();
    !cpp_types_named(ctx, js::last_segment(&spelled))
        .iter()
        .any(|&n| {
            let q = &ctx.sym(n).qualified_name;
            *q == spelled || q.ends_with(&format!("::{spelled}"))
        })
}

/// A `->` call on a receiver declared as a holder or an iterator calls a
/// member of the type it holds or points at.
pub fn match_cpp_held_type_call(
    ctx: &Ctx,
    raw: &str,
    method_name: &str,
    r: &Ref,
    declared: Option<&WrittenType>,
) -> Tri<Match> {
    let site = cpp_call_site(ctx, r.from, r);
    let Some(target) = cpp_arrow_target(ctx, raw, &site, declared, r) else {
        return Tri::Undefined;
    };
    if aliases::is_pointer_type(&target.text) {
        return Tri::Undefined;
    }
    let held = target.text.clone();
    if is_cpp_library_type(ctx, &held) {
        return Tri::Null;
    }
    let found = match supertypes::class_written(ctx, &held, &target.site(), r) {
        Tri::Undefined => return Tri::Undefined,
        Tri::Null => return Tri::Null,
        Tri::Found(f) => f,
    };
    if !CPP_CLASS_KINDS.contains(&ctx.kind(found.node)) {
        return match aliases::resolve_aliased_type(ctx, &held, r) {
            Tri::Null => Tri::Null,
            Tri::Found(a) if is_cpp_library_type(ctx, &a.target.join("::")) => Tri::Null,
            _ => Tri::Undefined,
        };
    }
    if let Some(method) = cpp_method_of(ctx, found.node, method_name, r) {
        return Tri::Found(Match::new(
            method,
            "instance-method",
            if found.exact { 0.9 } else { 0.85 },
        ));
    }
    if cpp_lacks_method(ctx, found.node) {
        Tri::Null
    } else {
        Tri::Undefined
    }
}

/// Where a type the calling function writes is looked up from.
pub fn cpp_call_site(ctx: &Ctx, caller: SymId, r: &Ref) -> WrittenType {
    let c = ctx.sym(caller);
    WrittenType {
        text: String::new(),
        scope: if c.kind == NodeKind::File {
            String::new()
        } else {
            c.qualified_name.clone()
        },
        file: r.file.clone(),
        line: r.line,
        parameters: aliases::template_parameters(ctx, caller),
        array: None,
        class_scope: cpp_test_fixture(ctx, caller, r),
    }
}

/// A member call on a receiver the calling function or its class declares,
/// with the operator that declaration takes, calls a member of the class the
/// declared type names.
pub fn match_cpp_declared_type_call(
    ctx: &Ctx,
    receiver: &str,
    method_name: &str,
    decl: &ReceiverDeclaration,
    r: &Ref,
) -> Tri<Match> {
    let Some(raw) = decl
        .raw
        .as_deref()
        .filter(|_| decl.callers && !decl.shadowed)
    else {
        return Tri::Undefined;
    };
    if cpp_member_operator(ctx, receiver, r) != Some(if decl.pointer { "->" } else { "." }) {
        return Tri::Undefined;
    }
    if decl.aliased.is_none() && is_cpp_library_type(ctx, raw) {
        return Tri::Null;
    }
    let aliased_class = decl
        .alias
        .as_ref()
        .and_then(|a| aliases::aliased_class(ctx, a, None));
    if decl.aliased.is_some() && aliased_class.is_none() {
        return Tri::Undefined;
    }
    let site = match &decl.declared {
        Some(d) => d.site(),
        None => cpp_call_site(ctx, r.from, r).site(),
    };
    let written = match &aliased_class {
        Some(c) => format!("::{c}"),
        None => raw.to_string(),
    };
    let found = supertypes::class_written(ctx, &written, &site, r);
    if found.is_undefined()
        && aliased_class.is_none()
        && is_cpp_outside_qualified_type(ctx, raw, &site)
    {
        return Tri::Null;
    }
    let Tri::Found(found) = found else {
        return Tri::Undefined;
    };
    if !found.exact || !CPP_CLASS_KINDS.contains(&ctx.kind(found.node)) {
        return Tri::Undefined;
    }
    if let Some(method) = cpp_method_of(ctx, found.node, method_name, r) {
        return Tri::Found(Match::new(method, "instance-method", 0.9));
    }
    if cpp_lacks_method(ctx, found.node) {
        Tri::Null
    } else {
        Tri::Undefined
    }
}

/// Is a `->` call on `receiver` made on a value of a type from outside the project?
pub fn is_cpp_arrow_through_outside_type(
    ctx: &Ctx,
    receiver: &str,
    decl: &ReceiverDeclaration,
    r: &Ref,
) -> bool {
    let Some(raw) = decl.raw.as_deref() else {
        return false;
    };
    if !decl.callers || decl.shadowed || decl.pointer || decl.aliased.is_some() {
        return false;
    }
    if cpp_member_operator(ctx, receiver, r) != Some("->") || is_cpp_library_type(ctx, raw) {
        return false;
    }
    let site = match &decl.declared {
        Some(d) => d.site(),
        None => cpp_call_site(ctx, r.from, r).site(),
    };
    supertypes::class_written(ctx, raw, &site, r).is_undefined()
        && is_cpp_outside_qualified_type(ctx, raw, &site)
}

/// Is the C++ type `raw` written under a namespace the project declares no type in?
pub fn is_cpp_outside_qualified_type(ctx: &Ctx, raw: &str, site: &supertypes::NameSite) -> bool {
    let Some(segments) = aliases::type_segments(ctx, raw) else {
        return false;
    };
    if segments.len() < 2 {
        return false;
    }
    let head = &segments[0];
    if head == "std"
        || !segments
            .last()
            .unwrap()
            .starts_with(|c: char| c.is_ascii_uppercase())
    {
        return false;
    }
    if site.parameters.contains(head) || namespaces::namespace_aliases(ctx).contains_key(head) {
        return false;
    }
    if namespaces::macro_namespace_frames(ctx, &site.file)
        .iter()
        .any(|f| f.path.contains(head))
    {
        return false;
    }
    !cpp_project_scope_names(ctx).contains(head)
}

/// Is every class the last name of a qualified C++ type finds a namesake?
pub fn cpp_finds_only_namesakes(ctx: &Ctx, raw: &str, method: Option<&str>) -> bool {
    if count_char(raw, '<') != count_char(raw, '>') {
        return false;
    }
    let Some(segments) = aliases::type_segments(ctx, raw) else {
        return false;
    };
    if segments.len() < 2 {
        return false;
    }
    if segments[..segments.len() - 1]
        .iter()
        .any(|s| !cpp_types_named(ctx, s).is_empty())
    {
        return false;
    }
    let name = segments.last().unwrap().clone();
    let alias = namespaces::namespace_aliases(ctx)
        .get(&segments[0])
        .cloned();
    let mut spellings: Vec<Vec<String>> = vec![segments.clone()];
    if let Some(a) = alias {
        let mut s: Vec<String> = a.split("::").map(str::to_string).collect();
        s.extend(segments[1..].iter().cloned());
        spellings.push(s);
    }
    let written: Vec<String> = spellings
        .iter()
        .flat_map(|s| cpp_namespace_spellings(ctx, &s.join("::")))
        .collect();
    let std = segments[0] == "std";
    let fits = |owner: &str| -> bool {
        written.iter().any(|w| {
            owner == w
                || owner.ends_with(&format!("::{w}"))
                || (!std && w.ends_with(&format!("::{owner}")))
        })
    };
    let mut owners: Vec<String> = cpp_types_named(ctx, &name)
        .iter()
        .map(|&n| ctx.sym(n).qualified_name.clone())
        .collect();
    if let Some(method) = method {
        for m in method_matches(ctx, &name, method, Language::Cpp) {
            let q = &ctx.sym(m).qualified_name;
            owners.push(q[..q.len() - method.len() - 2].to_string());
        }
    }
    !owners.is_empty()
        && !owners
            .iter()
            .any(|o| cpp_namespace_spellings(ctx, o).iter().any(|s| fits(s)))
}

/// `context.getMethodMatches(type, method, language)`.
pub fn method_matches(ctx: &Ctx, type_name: &str, method: &str, lang: Language) -> Vec<SymId> {
    let want = format!("{type_name}::{method}");
    let suffix = format!("::{want}");
    ctx.named(method)
        .iter()
        .copied()
        .filter(|&m| {
            let s = ctx.sym(m);
            s.kind == NodeKind::Method
                && same_language_family(s.language, lang)
                && (s.qualified_name == want || s.qualified_name.ends_with(&suffix))
        })
        .collect()
}

/// A C++ qualified name as spelled, and with a macro that names a namespace read as that namespace.
pub fn cpp_namespace_spellings(ctx: &Ctx, qualified: &str) -> Vec<String> {
    let macros = namespaces::name_macros(ctx);
    if macros.aliases.is_empty() {
        return vec![qualified.to_string()];
    }
    let named = qualified
        .split("::")
        .map(|s| {
            macros
                .aliases
                .get(s)
                .cloned()
                .unwrap_or_else(|| s.to_string())
        })
        .collect::<Vec<_>>()
        .join("::");
    if named == qualified {
        vec![qualified.to_string()]
    } else {
        vec![qualified.to_string(), named]
    }
}

/// Is a C++ type written as a member of a template parameter where `r` is made?
pub fn is_cpp_template_member(ctx: &Ctx, raw: &str, r: &Ref) -> bool {
    aliases::type_segments(ctx, raw)
        .and_then(|s| s.into_iter().next())
        .is_some_and(|h| aliases::template_parameters(ctx, r.from).contains(&h))
}

/// Every name in the qualified names of the project's C and C++ types.
pub fn cpp_project_scope_names(ctx: &Ctx) -> Rc<HashSet<String>> {
    ctx.memo(memo::NM, "scope-names".into(), || {
        let mut names: HashSet<String> =
            namespaces::macro_namespace_heads(ctx).into_iter().collect();
        for s in &ctx.syms.syms {
            if !is_c_family(s.language)
                || !matches!(
                    s.kind,
                    NodeKind::Class
                        | NodeKind::Struct
                        | NodeKind::Union
                        | NodeKind::Enum
                        | NodeKind::TypeAlias
                )
            {
                continue;
            }
            for seg in s.qualified_name.split("::") {
                if !seg.is_empty() {
                    names.insert(seg.to_string());
                }
            }
        }
        let macros = namespaces::name_macros(ctx);
        for (m, n) in &macros.aliases {
            if names.contains(m) {
                names.insert(n.clone());
            } else if names.contains(n) {
                names.insert(m.clone());
            }
        }
        Rc::new(names)
    })
}

/// Whether a C++ class with no method of a name surely has none.
pub fn cpp_lacks_method(ctx: &Ctx, cls: SymId) -> bool {
    aliases::template_parameters(ctx, cls).is_empty()
        && cpp_bases_known(ctx, cls, 0, &mut HashSet::new())
}

fn cpp_bases_known(ctx: &Ctx, cls: SymId, depth: u32, seen: &mut HashSet<String>) -> bool {
    let q = ctx.sym(cls).qualified_name.clone();
    if !seen.insert(q.clone()) {
        return true;
    }
    for &decl in ctx.by_qname(&q) {
        let d = ctx.sym(decl);
        if !is_c_family(d.language) || !CPP_CLASS_KINDS.contains(&d.kind) {
            continue;
        }
        let bases = supertype_nodes(ctx, decl);
        let written: Vec<String> = cpp_base_specifiers(ctx, decl)
            .into_iter()
            .filter(|s| !is_cpp_library_type(ctx, s))
            .collect();
        let unknown = if depth >= 4 {
            bases.len() + written.len() > 0
        } else {
            written.len() > bases.len()
        };
        if unknown {
            return false;
        }
        for base in bases {
            if !CPP_CLASS_KINDS.contains(&ctx.kind(base))
                || !cpp_bases_known(ctx, base, depth + 1, seen)
            {
                return false;
            }
        }
    }
    true
}

/// C++ class `cls`'s method `name`: its own (the overload the call fits),
/// else the nearest one of a class it derives from.
pub fn cpp_method_of(ctx: &Ctx, cls: SymId, name: &str, r: &Ref) -> Option<SymId> {
    cpp_method_of_depth(ctx, cls, name, r, 0, &mut HashSet::new())
}

pub fn cpp_method_of_depth(
    ctx: &Ctx,
    cls: SymId,
    name: &str,
    r: &Ref,
    depth: u32,
    seen: &mut HashSet<String>,
) -> Option<SymId> {
    let c = ctx.sym(cls);
    let want = format!("{}::{name}", c.qualified_name);
    let family: Vec<SymId> = method_matches(ctx, &c.name, name, r.lang)
        .into_iter()
        .filter(|&n| ctx.sym(n).qualified_name == want)
        .collect();
    let own = cpp_qualifier_specialization_pool(ctx, &cpp_specialization_pool(ctx, &family, r), r);
    if own.is_empty() && !family.is_empty() {
        return None;
    }
    if !own.is_empty() {
        let ordered = prefer_call_site_file(ctx, &cpp_visible_first(ctx, &own, r), &r.file);
        if r.name.contains("().") {
            return Some(ordered[0]);
        }
        let namesakes = cpp_class_decls(ctx, &c.qualified_name).len() > 1;
        let in_body: Vec<SymId> = if namesakes {
            ordered
                .iter()
                .copied()
                .filter(|&n| {
                    let s = ctx.sym(n);
                    s.file == c.file && s.start_line >= c.start_line && s.end_line <= c.end_line
                })
                .collect()
        } else {
            Vec::new()
        };
        let defined: Vec<SymId> = if !in_body.is_empty() {
            in_body
        } else if namesakes {
            let includers = includers::includers(ctx, &c.file);
            ordered
                .iter()
                .copied()
                .filter(|&n| {
                    let f = &ctx.sym(n).file;
                    *f == c.file || includers.contains(f)
                })
                .collect()
        } else {
            ordered.clone()
        };
        return Some(cpp_overload_for(
            ctx,
            if defined.is_empty() {
                &ordered
            } else {
                &defined
            },
            name,
            r,
        ));
    }
    seen.insert(c.qualified_name.clone());
    if depth >= 4 {
        return None;
    }
    for &decl in ctx.by_qname(&c.qualified_name) {
        let d = ctx.sym(decl);
        if !is_c_family(d.language) || !CPP_CLASS_KINDS.contains(&d.kind) {
            continue;
        }
        for base in supertype_nodes(ctx, decl) {
            let b = ctx.sym(base);
            if !CPP_CLASS_KINDS.contains(&b.kind) || seen.contains(&b.qualified_name) {
                continue;
            }
            if let Some(inherited) = cpp_method_of_depth(ctx, base, name, r, depth + 1, seen) {
                return Some(inherited);
            }
        }
    }
    None
}

/// A call written with no receiver, or on `this`, in a member function calls
/// what C++ name lookup finds there.
pub fn match_cpp_implicit_this_call(ctx: &Ctx, r: &Ref) -> Option<Match> {
    if r.kind != ReferenceKind::Calls || !js::is_ident(&r.name) {
        return None;
    }
    let receiver = cpp_implicit_receiver(ctx, r)?;
    let caller = r.from;
    let found = cpp_implicit_this_method(ctx, &r.name, receiver, caller, r)?;
    if receiver == "bare"
        && r.name != ctx.sym(caller).name
        && is_cpp_local_name(ctx, &r.name, caller, r)
    {
        return None;
    }
    Some(Match::new(found, "instance-method", 0.9))
}

/// The method a call of `name` written with no receiver, or on `this`, reaches from `caller`.
pub fn cpp_implicit_this_method(
    ctx: &Ctx,
    name: &str,
    receiver: &str,
    caller: SymId,
    r: &Ref,
) -> Option<SymId> {
    let owner = cpp_member_owner(ctx, caller).or_else(|| cpp_test_fixture(ctx, caller, r))?;
    let mut found: Tri<SymId> = Tri::Null;
    for scope in cpp_enclosing_classes(ctx, &owner, caller) {
        found = cpp_scope_method(ctx, &scope, name, receiver, r);
        if !matches!(found, Tri::Null) || receiver == "this" {
            break;
        }
    }
    found.found()
}

const CPP_THIS_RECEIVER: &str = r"^(?:this\s*->|\(\s*\*\s*this\s*\)\s*\.)\s*(?:template\s+)?";

/// How a C++ call recorded by its bare name is written: `bare`, `this`, or None.
pub fn cpp_implicit_receiver(ctx: &Ctx, r: &Ref) -> Option<&'static str> {
    let line = ctx
        .source(&r.file)?
        .lines
        .get((r.line as usize).checked_sub(1)?)?
        .clone();
    let this_re = ctx.re(CPP_THIS_RECEIVER);
    let on_this_before = ctx.re(r"(?:^|[^\w$])(?:this\s*->|\(\s*\*\s*this\s*\)\s*\.)$");
    let member_before = ctx.re(r"(?:\.|->|::)$");
    let at = |column: usize| -> Option<&'static str> {
        let text = js::slice_from(&line, column);
        let this = js::caps(&this_re, text)
            .map(|c| c.get(0).unwrap().as_str().len())
            .unwrap_or(0);
        let before = js::slice(&line, 0, column).trim_end();
        let on_this = this > 0 || js::test(&on_this_before, before);
        if !on_this && js::test(&member_before, before) {
            return None;
        }
        let rest = &text[this..];
        if !rest.starts_with(r.name.as_str())
            || js::is_word(js::at(rest, r.name.len()))
            || js::at(rest, r.name.len()) == b'$'
        {
            return None;
        }
        cpp_argument_list_follows(rest, r.name.len()).then_some(if on_this {
            "this"
        } else {
            "bare"
        })
    };
    let verdict = at(r.column as usize);
    if verdict.is_some() || line.is_ascii() {
        return verdict;
    }
    other_column(&line, r.column as usize).and_then(at)
}

/// The byte offset in `line` of a column that counts UTF-16 units, when it
/// differs from the byte offset `column` itself (cppUtf8Column's converse:
/// offsets here are bytes).
pub fn other_column(line: &str, column: usize) -> Option<usize> {
    let mut units = 0usize;
    for (i, c) in line.char_indices() {
        if units >= column {
            return (units == column && i != column).then_some(i);
        }
        units += c.len_utf16();
    }
    (units == column && line.len() != column).then_some(line.len())
}

/// A call's receiver the extractor drops, at the start of `text`.
fn cpp_unspelled_receiver_text(ctx: &Ctx, text: &str) -> Option<(String, String, usize)> {
    let head = js::caps(
        &ctx.re(r"^(?:\(\s*\*\s*([A-Za-z_]\w*)\s*\)|[A-Za-z_]\w*)"),
        text,
    )?;
    let mut end = head.get(0).unwrap().end();
    let mut unspelled = head.get(1).is_some_and(|g| g.as_str() != "this");
    let open_re = ctx.re(r"^\s*\[");
    while let Some(open) = js::caps(&open_re, &text[end..]) {
        let close = cpp_closing_bracket(text, end + open.get(0).unwrap().end() - 1)?;
        end = close + 1;
        unspelled = true;
    }
    if let Some(member) = js::caps(&ctx.re(r"^\s*(?:->|\.)\s*(?:first|second)\b"), &text[end..]) {
        end += member.get(0).unwrap().end();
        unspelled = true;
    }
    if !unspelled {
        return None;
    }
    let op = js::caps(&ctx.re(r"^\s*(->|\.)\s*(?:template\s+)?"), &text[end..])?;
    Some((
        text[..end].trim().to_string(),
        js::group(&op, 1).to_string(),
        end + op.get(0).unwrap().end(),
    ))
}

fn code_line_at(ctx: &Ctx, r: &Ref) -> Option<String> {
    let src = ctx.source(&r.file)?;
    let raw = src.lines.get((r.line as usize).checked_sub(1)?)?;
    let raw_lines = raw_strings::non_code_lines(ctx, &r.file);
    cpp_code_of(
        ctx,
        &raw_strings::literals_blanked(raw, r.line as usize, &raw_lines),
    )
}

/// The receiver a C++ call recorded by its bare name is made on, when one is unspelled.
pub fn cpp_unspelled_receiver(ctx: &Ctx, r: &Ref) -> Option<(String, String)> {
    let line = code_line_at(ctx, r)?;
    let at = |column: usize| -> Option<(String, String)> {
        let text = js::slice_from(&line, column);
        let (expression, op, length) = cpp_unspelled_receiver_text(ctx, text)?;
        let rest = &text[length..];
        if !rest.starts_with(r.name.as_str())
            || js::is_word(js::at(rest, r.name.len()))
            || js::at(rest, r.name.len()) == b'$'
        {
            return None;
        }
        cpp_argument_list_follows(rest, r.name.len()).then_some((expression, op))
    };
    let found = at(r.column as usize);
    if found.is_some() || line.is_ascii() {
        return found;
    }
    other_column(&line, r.column as usize).and_then(at)
}

/// The member chain a C++ call recorded by its bare name is made on.
pub fn cpp_member_chain_receiver(ctx: &Ctx, r: &Ref) -> Option<(String, String)> {
    let line = code_line_at(ctx, r)?;
    let chain_re =
        ctx.re(r"^([A-Za-z_]\w*(?:\s*(?:\.|->)\s*[A-Za-z_]\w*)*)\s*(\.|->)\s*(?:template\s+)?");
    let at = |column: usize| -> Option<(String, String)> {
        let text = js::slice_from(&line, column);
        let chain = js::caps(&chain_re, text)?;
        let expr = js::group(&chain, 1);
        if !expr.contains('.') && !expr.contains("->") {
            return None;
        }
        let rest = &text[chain.get(0).unwrap().end()..];
        if !rest.starts_with(r.name.as_str())
            || js::is_word(js::at(rest, r.name.len()))
            || js::at(rest, r.name.len()) == b'$'
        {
            return None;
        }
        cpp_argument_list_follows(rest, r.name.len())
            .then(|| (expr.to_string(), js::group(&chain, 2).to_string()))
    };
    let found = at(r.column as usize);
    if found.is_some() || line.is_ascii() {
        return found;
    }
    other_column(&line, r.column as usize).and_then(at)
}

/// A call on a data member reached through a member chain calls a member of
/// the class that member's type names.
pub fn match_cpp_member_chain_call(ctx: &Ctx, r: &Ref) -> Tri<Match> {
    let Some((expression, op)) = cpp_member_chain_receiver(ctx, r) else {
        return Tri::Undefined;
    };
    match_on_object(ctx, cpp_member_object(ctx, &expression, &op, r, 0), r, true)
}

/// A call on a receiver the extractor drops calls a member of the class that receiver has.
pub fn match_cpp_unspelled_receiver_call(ctx: &Ctx, r: &Ref) -> Tri<Match> {
    let Some((expression, op)) = cpp_unspelled_receiver(ctx, r) else {
        return Tri::Undefined;
    };
    match_on_object(
        ctx,
        cpp_member_object(ctx, &expression, &op, r, 0),
        r,
        false,
    )
}

fn match_on_object(
    ctx: &Ctx,
    object: Option<WrittenType>,
    r: &Ref,
    outside_counts: bool,
) -> Tri<Match> {
    let Some(object) = object else {
        return Tri::Undefined;
    };
    if aliases::is_pointer_type(&object.text) {
        return Tri::Undefined;
    }
    if is_cpp_library_type(ctx, &object.text) {
        return Tri::Null;
    }
    let found = match supertypes::class_written(ctx, &object.text, &object.site(), r) {
        Tri::Undefined => {
            return if outside_counts
                && is_cpp_outside_qualified_type(ctx, &object.text, &object.site())
            {
                Tri::Null
            } else {
                Tri::Undefined
            };
        }
        Tri::Null => {
            return if is_cpp_template_parameter_type(ctx, &object)
                && object.scope != ctx.sym(r.from).qualified_name
            {
                Tri::Undefined
            } else {
                Tri::Null
            };
        }
        Tri::Found(f) => f,
    };
    if !found.exact || !CPP_CLASS_KINDS.contains(&ctx.kind(found.node)) {
        return Tri::Undefined;
    }
    if let Some(method) = cpp_method_of(ctx, found.node, &r.name, r) {
        return Tri::Found(Match::new(method, "instance-method", 0.9));
    }
    if cpp_lacks_method(ctx, found.node) {
        Tri::Null
    } else {
        Tri::Undefined
    }
}

/// Whether `text` from `from` on is an argument list, after any template arguments.
pub fn cpp_argument_list_follows(text: &str, from: usize) -> bool {
    let b = text.as_bytes();
    let mut i = from;
    while i < b.len() && js::is_space(b[i]) {
        i += 1;
    }
    if js::at(text, i) == b'<' {
        let mut depth = 0i32;
        while i < b.len() {
            let c = b[i];
            if c == b'<' {
                depth += 1;
            } else if c == b'>' {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            } else if matches!(c, b';' | b'{' | b'}') {
                return false;
            }
            i += 1;
        }
        if depth != 0 {
            return false;
        }
        i += 1;
        while i < b.len() && js::is_space(b[i]) {
            i += 1;
        }
    }
    js::at(text, i) == b'('
}

/// The C and C++ class, struct and union declarations of a qualified name.
pub fn cpp_class_decls(ctx: &Ctx, qualified: &str) -> Vec<SymId> {
    ctx.by_qname(qualified)
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            CPP_CLASS_KINDS.contains(&s.kind) && is_c_family(s.language)
        })
        .collect()
}

/// The qualified name of the class whose scope a C++ caller's body is in.
pub fn cpp_member_owner(ctx: &Ctx, caller: SymId) -> Option<String> {
    let c = ctx.sym(caller);
    if CPP_CLASS_KINDS.contains(&c.kind) {
        return Some(c.qualified_name.clone());
    }
    if !matches!(c.kind, NodeKind::Method | NodeKind::Function) {
        return None;
    }
    let cut = c.qualified_name.rfind("::").filter(|&i| i > 0)?;
    let owner = &c.qualified_name[..cut];
    if c.kind == NodeKind::Function {
        let src = ctx.source(&c.file)?;
        let from = (c.start_line as usize)
            .saturating_sub(1)
            .min(src.lines.len());
        let to = (c.start_line as usize + 2).min(src.lines.len());
        let head = src.lines[from..to].join(" ");
        let cls = js::escape(js::last_segment(owner));
        let f = js::escape(&c.name);
        if !js::test(
            &ctx.re(&format!(r"\b{cls}\s*(?:<[^<>;{{}}]*>\s*)?::\s*~?{f}\s*\(")),
            &head,
        ) {
            return None;
        }
    }
    if !cpp_class_decls(ctx, owner).is_empty() {
        return Some(owner.to_string());
    }
    let outer = owner.rfind("::").filter(|&i| i > 0)?;
    (!cpp_class_decls(ctx, &owner[..outer]).is_empty()).then(|| owner.to_string())
}

/// One class C++ lookup passes through, with the declarations that are the class there.
#[derive(Clone, Debug)]
pub struct ClassScope {
    pub qualified_name: String,
    pub decls: Vec<SymId>,
    pub holds: bool,
    pub narrowed: bool,
}

/// `rangeWithin(inner, outer)`: `inner`'s lines lie within `outer`'s.
pub fn range_within(ctx: &Ctx, inner: SymId, outer: SymId) -> bool {
    let (a, b) = (ctx.sym(inner), ctx.sym(outer));
    a.start_line >= b.start_line && a.end_line <= b.end_line
}

/// `sameRange(a, b)`.
pub fn same_range(ctx: &Ctx, a: SymId, b: SymId) -> bool {
    let (x, y) = (ctx.sym(a), ctx.sym(b));
    x.start_line == y.start_line && x.end_line == y.end_line
}

/// The classes a C++ caller's name lookup passes through, innermost first.
pub fn cpp_enclosing_classes(ctx: &Ctx, owner: &str, caller: SymId) -> Vec<ClassScope> {
    let mut scopes = Vec::new();
    let mut qualified = owner.to_string();
    let mut inner = caller;
    let mut hops = 0;
    while hops < 8 && !qualified.is_empty() {
        let decls = cpp_class_decls(ctx, &qualified);
        if decls.is_empty() && hops > 0 {
            let last = js::last_segment(&qualified).to_string();
            let inner_file = ctx.sym(inner).file.clone();
            let f = ctx.in_file(&inner_file).iter().copied().find(|&n| {
                let s = ctx.sym(n);
                s.name == last
                    && matches!(s.kind, NodeKind::Method | NodeKind::Function)
                    && range_within(ctx, inner, n)
                    && !same_range(ctx, inner, n)
            });
            let Some(f) = f else {
                break;
            };
            let Some(fn_owner) = cpp_member_owner(ctx, f) else {
                break;
            };
            qualified = fn_owner;
            inner = f;
            hops += 1;
            continue;
        }
        let scope = cpp_scope_declarations(ctx, &qualified, &decls, inner);
        if let Some(&first) = scope.decls.first() {
            inner = first;
        }
        scopes.push(scope);
        qualified = match qualified.rfind("::") {
            Some(cut) if cut > 0 => qualified[..cut].to_string(),
            _ => String::new(),
        };
        hops += 1;
    }
    scopes
}

/// A class scope's declarations as cppEnclosingClasses keeps them.
pub fn cpp_scope_declarations(
    ctx: &Ctx,
    qualified: &str,
    decls: &[SymId],
    inner: SymId,
) -> ClassScope {
    let scope = |kept: Vec<SymId>, holds: bool| ClassScope {
        qualified_name: qualified.to_string(),
        narrowed: kept.len() < decls.len(),
        decls: kept,
        holds,
    };
    let inner_file = ctx.sym(inner).file.clone();
    let holding: Vec<SymId> = decls
        .iter()
        .copied()
        .filter(|&d| ctx.sym(d).file == inner_file && range_within(ctx, inner, d))
        .collect();
    if !holding.is_empty() {
        return scope(holding, true);
    }
    let local: Vec<SymId> = decls
        .iter()
        .copied()
        .filter(|&d| ctx.sym(d).file == inner_file)
        .collect();
    if !local.is_empty() || decls.len() < 2 {
        return scope(
            if local.is_empty() {
                decls.to_vec()
            } else {
                local
            },
            false,
        );
    }
    let included: Vec<SymId> = decls
        .iter()
        .copied()
        .filter(|&d| includers::includers(ctx, &ctx.sym(d).file).contains(&inner_file))
        .collect();
    scope(
        if included.is_empty() {
            decls.to_vec()
        } else {
            included
        },
        false,
    )
}

/// The method `name` a class scope gives C++ lookup.
fn cpp_scope_method(
    ctx: &Ctx,
    scope: &ClassScope,
    name: &str,
    receiver: &str,
    r: &Ref,
) -> Tri<SymId> {
    let candidates: Vec<SymId> = ctx
        .by_qname(&format!("{}::{name}", scope.qualified_name))
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            s.kind == NodeKind::Method
                && is_c_family(s.language)
                && (!scope.narrowed
                    || scope.decls.iter().any(|&d| {
                        let df = &ctx.sym(d).file;
                        *df == s.file || includers::includers(ctx, df).contains(&s.file)
                    }))
        })
        .collect();
    let own = cpp_specialization_pool(ctx, &candidates, r);
    let holder = if scope.holds {
        scope.decls.first().copied()
    } else {
        None
    };
    let inline: Vec<SymId> = match holder {
        Some(h) => own
            .iter()
            .copied()
            .filter(|&n| ctx.sym(n).file == ctx.sym(h).file && range_within(ctx, n, h))
            .collect(),
        None => Vec::new(),
    };
    let out_of_line: Vec<SymId> = if js::last_segment(&scope.qualified_name) == "<anonymous>" {
        Vec::new()
    } else {
        own.iter()
            .copied()
            .filter(|n| !inline.contains(n))
            .collect()
    };
    if !inline.is_empty() || !out_of_line.is_empty() {
        let mut ordered = inline.clone();
        ordered.extend(prefer_call_site_file(ctx, &out_of_line, &r.file));
        return Tri::Found(cpp_overload_for(ctx, &ordered, name, r));
    }
    if scope.decls.iter().any(|&d| {
        cpp_member_macro_arguments(ctx, d)
            .iter()
            .any(|a| name.starts_with(a.as_str()) || name.ends_with(a.as_str()))
    }) {
        return Tri::Undefined;
    }
    for &decl in &scope.decls {
        for base in supertype_nodes(ctx, decl) {
            if !CPP_CLASS_KINDS.contains(&ctx.kind(base)) {
                continue;
            }
            if receiver == "bare"
                && is_cpp_dependent_base(ctx, decl, base)
                && !cpp_using_declares(ctx, decl, base, name)
            {
                continue;
            }
            let mut seen = HashSet::from([scope.qualified_name.clone()]);
            let Some(inherited) = cpp_method_of_depth(ctx, base, name, r, 1, &mut seen) else {
                continue;
            };
            let iq = ctx.sym(inherited).qualified_name.clone();
            let siblings: Vec<SymId> = ctx
                .by_qname(&iq)
                .iter()
                .copied()
                .filter(|&n| {
                    let s = ctx.sym(n);
                    n != inherited && s.kind == NodeKind::Method && is_c_family(s.language)
                })
                .collect();
            let mut pool = vec![inherited];
            pool.extend(prefer_call_site_file(ctx, &siblings, &r.file));
            return Tri::Found(cpp_overload_for(
                ctx,
                &cpp_specialization_pool(ctx, &pool, r),
                name,
                r,
            ));
        }
    }
    Tri::Null
}

/// Of one class's overloads of `name`, the one the call's arguments fit best.
pub fn cpp_overload_for(ctx: &Ctx, candidates: &[SymId], name: &str, r: &Ref) -> SymId {
    if candidates.len() == 1 {
        return candidates[0];
    }
    let callable: Vec<SymId> = candidates
        .iter()
        .copied()
        .filter(|&n| !is_cpp_deleted_function(ctx, n, name))
        .collect();
    let overloads = if callable.is_empty() {
        candidates.to_vec()
    } else {
        callable
    };
    if overloads.len() == 1 {
        return overloads[0];
    }
    let Some(args) = cpp_call_arguments(ctx, r, name) else {
        return overloads[0];
    };
    let fits: Vec<i64> = overloads
        .iter()
        .map(|&n| cpp_overload_fit(ctx, n, name, &args))
        .collect();
    let top = *fits.iter().max().unwrap();
    let tied: Vec<SymId> = overloads
        .iter()
        .zip(fits.iter())
        .filter(|(_, &f)| f == top)
        .map(|(&n, _)| n)
        .collect();
    cpp_argument_type_choice(ctx, &tied, name, &args, r)
}

/// Of overloads the arguments fit alike by count, the one their types fit best.
pub fn cpp_argument_type_choice(
    ctx: &Ctx,
    tied: &[SymId],
    name: &str,
    args: &[String],
    r: &Ref,
) -> SymId {
    if tied.len() == 1 || args.is_empty() {
        return tied[0];
    }
    let types: Vec<Option<ArgumentType>> =
        args.iter().map(|a| cpp_argument_type(ctx, a, r)).collect();
    if types.iter().all(Option::is_none) {
        return tied[0];
    }
    let fits: Vec<i64> = tied
        .iter()
        .map(|&n| cpp_argument_type_fit(ctx, n, name, &types))
        .collect();
    let top = *fits.iter().max().unwrap();
    let best_index = fits.iter().position(|&f| f == top).unwrap();
    let best = tied[best_index];
    let others: Vec<SymId> = tied
        .iter()
        .zip(fits.iter())
        .filter(|(&n, &f)| f == top && n != best)
        .map(|(&n, _)| n)
        .collect();
    cpp_const_twin_choice(ctx, best, &others, name, &types)
}

fn cpp_const_twin_choice(
    ctx: &Ctx,
    best: SymId,
    others: &[SymId],
    name: &str,
    types: &[Option<ArgumentType>],
) -> SymId {
    if others.is_empty() {
        return best;
    }
    let Some(shape) = cpp_constless_parameters(ctx, best, name).map(|p| p.join(",")) else {
        return best;
    };
    let mut choice = best;
    let mut added = cpp_const_added(ctx, best, name, types);
    for &n in others {
        if cpp_constless_parameters(ctx, n, name)
            .map(|p| p.join(","))
            .as_deref()
            != Some(shape.as_str())
        {
            continue;
        }
        let count = cpp_const_added(ctx, n, name, types);
        if count < added {
            choice = n;
            added = count;
        }
    }
    choice
}

fn cpp_constless_parameters(ctx: &Ctx, n: SymId, name: &str) -> Option<Vec<String>> {
    let list = cpp_overload_parameters(ctx, n, name)?;
    let konst = ctx.re(r"\bconst\b");
    Some(
        split_cpp_top_level(ctx, &cpp_without_comments(&list))
            .into_iter()
            .filter(|p| p != "void")
            .map(|p| js::squash(&js::replace_with(&konst, &cpp_parameter_type(ctx, &p), " ")))
            .collect(),
    )
}

fn cpp_const_added(ctx: &Ctx, n: SymId, name: &str, types: &[Option<ArgumentType>]) -> usize {
    let Some(list) = cpp_overload_parameters(ctx, n, name) else {
        return 0;
    };
    let params: Vec<String> = split_cpp_top_level(ctx, &cpp_without_comments(&list))
        .into_iter()
        .filter(|p| p != "void")
        .collect();
    let mut count = 0;
    for (i, arg) in types.iter().enumerate().take(params.len()) {
        let param = cpp_parameter_type(ctx, &params[i]);
        if let Some(ArgumentType::Type { text, .. }) = arg {
            if cpp_refers_to_const(ctx, &param) == Some(true)
                && cpp_argument_const(ctx, text, &param) == Some(false)
            {
                count += 1;
            }
        }
    }
    count
}

/// The C++ function a call of `chosen`'s name reaches among `candidates`.
pub fn cpp_function_overload(ctx: &Ctx, chosen: SymId, candidates: &[SymId], r: &Ref) -> SymId {
    if !is_c_family(r.lang) || r.kind != ReferenceKind::Calls {
        return chosen;
    }
    let cq = ctx.sym(chosen).qualified_name.clone();
    let overloads: Vec<SymId> = candidates
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            s.kind == NodeKind::Function
                && s.qualified_name == cq
                && is_visible_across_files(ctx, n, r)
        })
        .collect();
    if overloads.is_empty() {
        return chosen;
    }
    if r.lang == Language::C {
        let own: Vec<SymId> = overloads
            .iter()
            .copied()
            .filter(|&n| {
                let f = &ctx.sym(n).file;
                *f == r.file || includers::includers(ctx, f).contains(&r.file)
            })
            .collect();
        let external: Vec<SymId> = if own.is_empty() {
            Vec::new()
        } else {
            overloads
                .iter()
                .copied()
                .filter(|n| !own.contains(n) && !is_static_c_function(ctx, *n))
                .collect()
        };
        let outside: Vec<SymId> = if !external.is_empty()
            && own
                .iter()
                .all(|&n| super::super::features::is_c_feature_variant(ctx, n, &r.file, r.line))
            && !super::super::features::is_off_by_default(ctx, &r.file, r.line)
        {
            external
                .into_iter()
                .filter(|&n| !super::super::features::is_c_feature_variant(ctx, n, &r.file, r.line))
                .collect()
        } else {
            Vec::new()
        };
        let pool = if !outside.is_empty() {
            outside
        } else if !own.is_empty() {
            own
        } else {
            overloads
        };
        return find_best_match(ctx, r, &pool).unwrap_or(chosen);
    }
    let ordered: Vec<SymId> = if overloads.contains(&chosen) {
        let mut v = vec![chosen];
        v.extend(overloads.iter().copied().filter(|&n| n != chosen));
        v
    } else {
        overloads
    };
    let name = ctx.sym(chosen).name.clone();
    cpp_overload_for(ctx, &ordered, &name, r)
}

/// Of the C++ functions `candidates` a call may reach, the ones its template arguments allow.
pub fn cpp_specialization_pool(ctx: &Ctx, candidates: &[SymId], r: &Ref) -> Vec<SymId> {
    if r.lang != Language::Cpp {
        return candidates.to_vec();
    }
    let written: Vec<Option<Vec<String>>> = candidates
        .iter()
        .map(|&n| cpp_specialization_arguments(ctx, n))
        .collect();
    if written.iter().all(Option::is_none) {
        return candidates.to_vec();
    }
    let others: Vec<SymId> = candidates
        .iter()
        .zip(written.iter())
        .filter(|(_, w)| w.is_none())
        .map(|(&n, _)| n)
        .collect();
    let Some(args) = cpp_call_template_arguments(ctx, r) else {
        return if others.is_empty() {
            candidates.to_vec()
        } else {
            others
        };
    };
    let named: Vec<SymId> = candidates
        .iter()
        .zip(written.iter())
        .filter(|(_, w)| {
            w.as_ref()
                .is_some_and(|w| cpp_template_arguments_match(w, &args))
        })
        .map(|(&n, _)| n)
        .collect();
    if named.is_empty() {
        others
    } else {
        named
    }
}

/// The candidates a call through a qualifier written with template arguments can mean.
pub fn cpp_qualifier_specialization_pool(ctx: &Ctx, candidates: &[SymId], r: &Ref) -> Vec<SymId> {
    if r.lang != Language::Cpp || candidates.is_empty() {
        return candidates.to_vec();
    }
    let Some(args) = cpp_qualifier_arguments(ctx, r) else {
        return candidates.to_vec();
    };
    candidates
        .iter()
        .copied()
        .filter(|&n| {
            cpp_specialization_owner_arguments(ctx, n)
                .is_none_or(|owner| cpp_template_arguments_match(&owner, &args))
        })
        .collect()
}

/// The template arguments a call writes on the qualifier before its name.
pub fn cpp_qualifier_arguments(ctx: &Ctx, r: &Ref) -> Option<Vec<String>> {
    let segments: Vec<&str> = r.name.split("::").collect();
    if segments.len() < 2 {
        return None;
    }
    let src = ctx.source(&r.file)?;
    let from = (r.line as usize).saturating_sub(1).min(src.lines.len());
    let to = (r.line as usize + 3).min(src.lines.len());
    cpp_template_arguments_after(
        ctx,
        &src.lines[from..to].join("\n"),
        segments[segments.len() - 2],
        r.column as usize,
    )
}

/// The arguments of the full specialization of its class a member is defined out of line for.
fn cpp_specialization_owner_arguments(ctx: &Ctx, n: SymId) -> Option<Vec<String>> {
    let s = ctx.sym(n);
    if s.language != Language::Cpp || !matches!(s.kind, NodeKind::Method | NodeKind::Function) {
        return None;
    }
    let segments: Vec<&str> = s.qualified_name.split("::").collect();
    if segments.len() < 2 {
        return None;
    }
    ctx.memo(memo::NM, format!("soa\0{n}"), || {
        let owner = js::escape(segments[segments.len() - 2]);
        let name = js::escape(&s.name);
        let src = ctx.source(&s.file)?;
        let from = (s.start_line as usize)
            .saturating_sub(1)
            .min(src.lines.len());
        let to = (s.start_line as usize + 11).min(src.lines.len());
        let head_all = src.lines[from..to].join("\n");
        let head = js::slice_from(&head_all, s.start_column as usize);
        let after_re = ctx.re(&format!(r"^\s*::\s*~?{name}(?![\w$])"));
        for m in js::all(&ctx.re(&format!(r"(?<![\w$]){owner}<")), head) {
            let whole = m.get(0).unwrap();
            let open = whole.end() - 1;
            let Some(end) = cpp_template_arguments_end(head, open) else {
                continue;
            };
            if !js::test(&after_re, &head[end + 1..]) {
                continue;
            }
            let args: Vec<String> = split_cpp_top_level(ctx, &head[open + 1..end])
                .iter()
                .map(|a| cpp_template_argument_key(a))
                .collect();
            let bfrom = (s.start_line as usize)
                .saturating_sub(3)
                .min(src.lines.len());
            let bto = (s.start_line as usize)
                .saturating_sub(1)
                .min(src.lines.len());
            let before = format!(
                "{}\n{}",
                src.lines[bfrom..bto.max(bfrom)].join("\n"),
                &head[..whole.start()]
            );
            let header = js::caps(
                &ctx.re(r"\btemplate\s*<\s*(>)?(?![\s\S]*\btemplate\s*<)"),
                &before,
            );
            let parameters = aliases::template_parameters(ctx, n);
            let dependent = header.as_ref().is_some_and(|h| h.get(1).is_none())
                || args.iter().any(|a| {
                    parameters.iter().any(|p| {
                        js::test(
                            &ctx.re(&format!(r"(?<![\w$]){}(?![\w$])", js::escape(p))),
                            a,
                        )
                    })
                });
            return if dependent || args.is_empty() {
                None
            } else {
                Some(args)
            };
        }
        None
    })
}
