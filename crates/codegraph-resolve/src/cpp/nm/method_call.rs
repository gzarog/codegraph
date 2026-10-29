//! `matchMethodCall`, as it runs for a C or C++ reference: the receiver's
//! declared type first, then a class the receiver names, then a method of the
//! name guessed by the receiver's words.

use super::*;

const TEST_DOUBLE_OWNER: &str = r"(?i)\b(?:fake|mock|mocked|stub|dummy|spy)\b";
const DISPATCHED_OWNER: &str = r"(?:View|ViewSet|APIView|Controller|Endpoint|ViewMixin)$";
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

/// `splitCamelCase`.
pub fn split_camel_case(ctx: &Ctx, s: &str) -> Vec<String> {
    let a = js::replace_all(&ctx.re(r"([a-z])([A-Z])"), s, |c| {
        format!("{} {}", js::group(c, 1), js::group(c, 2))
    });
    let b = js::replace_all(&ctx.re(r"([A-Z]+)([A-Z][a-z])"), &a, |c| {
        format!("{} {}", js::group(c, 1), js::group(c, 2))
    });
    b.split(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | ':' | '/' | '\\'))
        .filter(|w| w.chars().count() > 1)
        .map(str::to_string)
        .collect()
}

/// `receiverLink`.
fn receiver_link<'s>(ctx: &Ctx, receiver: &'s str) -> &'s str {
    let links: Vec<&str> = receiver.split('.').collect();
    let last = links[links.len() - 1];
    if links.len() > 1 && js::test(&ctx.re(r"^[A-Z][A-Z0-9_]+$"), last) {
        links[links.len() - 2]
    } else {
        last
    }
}

fn owner_segment(qn: &str) -> Option<&str> {
    let cut = qn.rfind("::").filter(|&c| c > 0)?;
    qn[..cut]
        .rsplit([':', '.'])
        .find(|s| !s.is_empty())
        .or(Some(""))
}

/// `isUnnamedTestDouble`.
fn is_unnamed_test_double(ctx: &Ctx, method: SymId, receiver: &str, r: &Ref) -> bool {
    let Some(owner) = owner_segment(&ctx.sym(method).qualified_name) else {
        return false;
    };
    let double = ctx.re(TEST_DOUBLE_OWNER);
    if !js::test(&double, &split_camel_case(ctx, owner).join(" ")) {
        return false;
    }
    if split_camel_case(ctx, receiver_link(ctx, receiver))
        .iter()
        .any(|w| js::test(&double, w))
    {
        return false;
    }
    !ctx.source(&r.file).is_some_and(|s| s.text.contains(owner))
}

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

/// `matchMethodCall`, for a C or C++ reference.
pub fn match_method_call(ctx: &Ctx, r: &Ref) -> Option<Match> {
    let dot = js::caps(&ctx.re(r"^([\w.]+)\.(\w+:?(?:\w+:)*)$"), &r.name).or_else(|| {
        if r.is_cpp() {
            js::caps(&ctx.re(r"^([\w.]+)\.(operator[^\w\s.]+)$"), &r.name)
        } else {
            None
        }
    });
    let colon = if dot.is_none() {
        js::caps(&ctx.re(r"^(\w+)::(\w+)$"), &r.name)
    } else {
        None
    };
    let is_colon = dot.is_none() && colon.is_some();
    let Some(m) = dot.as_ref().or(colon.as_ref()) else {
        return if r.is_cpp() {
            match_cpp_implicit_this_call(ctx, r)
        } else {
            None
        };
    };
    let object = js::group(m, 1).to_string();
    let method = js::group(m, 2).to_string();

    if dot.is_some() {
        let mut decl = ReceiverDeclaration::default();
        // (A C receiver is read by upstream's shared inferrer, which isn't
        // ported: C has no methods, so its typed answers are dropped anyway.)
        let mut inferred = if r.is_cpp() {
            infer_cpp_receiver_type(ctx, &object, r, 0, Some(&mut decl))
        } else {
            None
        };
        if r.is_cpp() {
            match match_cpp_declared_type_call(ctx, &object, &method, &decl, r) {
                Tri::Found(found) => return Some(found),
                Tri::Null => return None,
                Tri::Undefined => {}
            }
            if inferred.is_some() && is_cpp_arrow_through_outside_type(ctx, &object, &decl, r) {
                inferred = None;
            }
            if let (Some(t), Some(raw)) = (&inferred, &decl.raw) {
                if decl.aliased != Some(true)
                    && cpp_finds_only_namesakes(ctx, raw, Some(&method))
                    && !is_cpp_template_member(ctx, raw, r)
                {
                    if t.starts_with(|c: char| c.is_ascii_uppercase())
                        && (decl.pointer || cpp_member_operator(ctx, &object, r) != Some("->"))
                    {
                        return None;
                    }
                    inferred = None;
                }
            }
        }
        if let Some(t) = &inferred {
            if let Some(typed) =
                resolve_method_on_type(ctx, t, &method, r, 0.9, "instance-method", 0)
            {
                return Some(typed);
            }
            if is_undeclared_type_name(ctx, t.rsplit('.').next().unwrap_or(t), r) {
                return None;
            }
        }
        if decl.aliased == Some(true) {
            if !decl.class_template && (decl.pointer || is_cpp_dot_call(ctx, &object, r)) {
                return None;
            }
            if is_undeclared_type_name(ctx, decl.written.as_deref().unwrap_or(""), r) {
                return None;
            }
        }
        if r.is_cpp()
            && decl.callers
            && !decl.shadowed
            && !decl.pointer
            && cpp_member_operator(ctx, &object, r) == Some("->")
        {
            if let Some(raw) = &decl.raw {
                match match_cpp_held_type_call(ctx, raw, &method, r, decl.declared.as_ref()) {
                    Tri::Found(found) => return Some(found),
                    Tri::Null => return None,
                    Tri::Undefined => {}
                }
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
    let classes = prefer_call_site_file(
        ctx,
        &within_cpp_translation_unit(ctx, &owners, r, false),
        &r.file,
    );
    for &cls in &classes {
        let c = ctx.sym(cls);
        if c.language != r.lang || (production && codegraph_core::search::is_test_path(&c.file)) {
            continue;
        }
        if c.language == Language::Cpp {
            let Some(found) = cpp_method_of(ctx, cls, &method, r) else {
                continue;
            };
            let own = ctx.sym(found).qualified_name == format!("{}::{method}", c.qualified_name);
            return Some(Match::new(
                found,
                "qualified-name",
                if own { 0.85 } else { 0.8 },
            ));
        }
        if let Some(found) = find_own_method(ctx, cls, &method) {
            return Some(Match::new(found, "qualified-name", 0.85));
        }
    }
    if r.is_cpp()
        && is_colon
        && cpp_qualifier_arguments(ctx, r).is_some()
        && ctx
            .named(&object)
            .iter()
            .any(|&n| ctx.sym(n).language == Language::Cpp && is_method_owner_kind(ctx, n))
    {
        return None;
    }
    let through_namespace =
        || r.is_cpp() && is_colon && is_cpp_namespace_qualifier(ctx, &object, r);

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
        let classes = prefer_call_site_file(
            ctx,
            &within_cpp_translation_unit(ctx, &owners, r, false),
            &r.file,
        );
        for &cls in &classes {
            let c = ctx.sym(cls);
            if c.language != r.lang || (production && codegraph_core::search::is_test_path(&c.file))
            {
                continue;
            }
            if let Some(found) = find_own_method(ctx, cls, &method) {
                if through_namespace() {
                    return None;
                }
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
    let dispatched = ctx.re(DISPATCHED_OWNER);
    let kept: Vec<SymId> = target
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            !(DISPATCHED_ACTIONS.contains(&s.name.as_str())
                && js::test(&dispatched, owner_segment(&s.qualified_name).unwrap_or("")))
        })
        .collect();
    let narrowed = kept.len() != target.len();
    target = kept;
    if production {
        target.retain(|&n| !codegraph_core::search::is_test_path(&ctx.sym(n).file));
    }
    let scoped_out = false;
    if target.len() == 1
        && !narrowed
        && !scoped_out
        && ctx.sym(target[0]).language == r.lang
        && !is_unnamed_test_double(ctx, target[0], &object, r)
    {
        if through_namespace() && !is_cpp_constructor_through(ctx, &object, target[0]) {
            return None;
        }
        if target[0] == r.from
            && !matches!(object.as_str(), "this" | "self" | "Self" | "super" | "cls")
        {
            return None;
        }
        return Some(Match::new(target[0], "instance-method", 0.7));
    }
    if target.len() > 1 {
        let link = receiver_link(ctx, &object);
        let words = split_camel_case(ctx, link);
        let head = words.last().map(|w| w.to_lowercase());
        let whole = link.replace('_', "").to_lowercase();
        let double = ctx.re(TEST_DOUBLE_OWNER);
        let mut best: Option<SymId> = None;
        let mut best_score = 0i64;
        for m in prefer_call_site_file(
            ctx,
            &within_cpp_translation_unit(ctx, &target, r, false),
            &r.file,
        ) {
            let s = ctx.sym(m);
            let owner = owner_segment(&s.qualified_name);
            let class_words = owner.map(|o| split_camel_case(ctx, o)).unwrap_or_default();
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
            if r.is_cpp() && is_colon && is_cpp_undeclared_qualifier(ctx, &object, r) {
                return None;
            }
            if through_namespace() && !is_cpp_constructor_through(ctx, &object, best) {
                return None;
            }
            return Some(Match::new(best, "instance-method", 0.65));
        }
    }
    None
}
