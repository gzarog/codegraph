//! Data members, pointees, range elements and the types C++ expressions have
//! (name-matcher.ts, from `cppDataMemberIn` to `cppClassesIn`).

use super::*;

/// The type the C++ class `decl` declares its data member `name` with, as written.
pub fn cpp_data_member_in(ctx: &Ctx, decl: SymId, name: &str) -> Option<WrittenType> {
    let d = ctx.sym(decl);
    if !is_c_family(d.language) || !CPP_CLASS_KINDS.contains(&d.kind) {
        return None;
    }
    let key = format!("dm\0{decl}\0{name}");
    if let Some(hit) = ctx.memo_get::<Option<WrittenType>>(memo::NM, &key) {
        return hit;
    }
    let member = data_member_uncached(ctx, decl, name);
    ctx.memo_set(memo::NM, key, member.clone());
    member
}

fn data_member_uncached(ctx: &Ctx, decl: SymId, name: &str) -> Option<WrittenType> {
    let d = ctx.sym(decl);
    let src = ctx.source(&d.file)?;
    let lines = &src.lines;
    let escaped = js::escape(name);
    let word = ctx.re(&format!(r"\b{escaped}\b"));
    let declarator = build_declarator_regex(ctx, &escaped);
    let listed = build_listed_declarator_regex(ctx, &escaped);
    let declared = |text: &str| -> Option<DeclMatch> {
        match_cpp_declarator(ctx, text, &declarator)
            .or_else(|| match_cpp_listed_declarator(ctx, text, name, &listed))
    };
    let end = (d.end_line.max(d.start_line) as usize).min(lines.len());
    let nested = cpp_nested_spans(ctx, decl, end as u32);
    let raw_lines = raw_strings::non_code_lines(ctx, &d.file);
    let depths = cpp_class_paren_depths(ctx, decl, lines, end, &raw_lines);
    let type_line = CPP_TYPE_LINE.with(ctx);
    for l in d.start_line as usize..=end {
        if l == 0
            || !js::test(&word, &lines[l - 1])
            || nested.iter().any(|&(s, e)| l as u32 >= s && l as u32 <= e)
        {
            continue;
        }
        let mut code = cpp_code_of(
            ctx,
            &raw_strings::literals_blanked(&lines[l - 1], l, &raw_lines),
        );
        let mut from = l;
        let mut m = code.as_deref().and_then(&declared);
        if m.is_none() && code.is_some() && l > d.start_line as usize {
            if let Some(above) = cpp_code_of(
                ctx,
                &raw_strings::literals_blanked(&lines[l - 2], l - 1, &raw_lines),
            ) {
                if js::test(&type_line, &above) {
                    let joined = format!("{above} {}", code.as_deref().unwrap());
                    from = l - 1;
                    m = declared(&joined);
                    code = Some(joined);
                }
            }
        }
        let Some(m) = m else {
            continue;
        };
        let code = code.unwrap();
        let rest = &code[m.end..];
        if rest.starts_with('(') {
            continue;
        }
        let mut depth = depths[from - d.start_line as usize];
        let start = if from == d.start_line as usize {
            d.start_column as usize
        } else {
            0
        };
        for c in js::slice(&code, start, m.index).bytes() {
            if c == b'(' {
                depth += 1;
            } else if c == b')' {
                depth -= 1;
            }
        }
        let ty = if depth <= 0 {
            normalize_cpp_type_name(ctx, &m.ty)
        } else {
            None
        };
        if ty.is_some_and(|t| t != "auto") {
            let array = cpp_array_dimensions(ctx, rest);
            return Some(WrittenType {
                text: m.ty,
                scope: d.qualified_name.clone(),
                file: d.file.clone(),
                line: from as u32,
                parameters: aliases::template_parameters(ctx, decl),
                array: (array > 0).then_some(array),
                class_scope: None,
            });
        }
    }
    None
}

/// How many parentheses are open where each line of the class declaration begins.
fn cpp_class_paren_depths(
    ctx: &Ctx,
    decl: SymId,
    lines: &[String],
    end: usize,
    raw_lines: &NonCode,
) -> Rc<Vec<i32>> {
    ctx.memo(memo::NM, format!("pd\0{decl}"), || {
        let d = ctx.sym(decl);
        let mut depths = Vec::new();
        let mut depth = 0i32;
        for l in d.start_line as usize..=end {
            depths.push(depth);
            let text = raw_strings::literals_blanked(
                lines.get(l.wrapping_sub(1)).map_or("", |s| s.as_str()),
                l,
                raw_lines,
            );
            let b = text.as_bytes();
            let mut i = if l == d.start_line as usize {
                d.start_column as usize
            } else {
                0
            };
            while i < b.len() {
                let c = b[i];
                if c == b'/' && b.get(i + 1) == Some(&b'/') {
                    break;
                } else if c == b'(' {
                    depth += 1;
                } else if c == b')' && depth > 0 {
                    depth -= 1;
                }
                i += 1;
            }
        }
        Rc::new(depths)
    })
}

/// Is `n` an anonymous union or struct whose members are its enclosing class's?
pub fn is_cpp_anonymous_members(ctx: &Ctx, n: SymId) -> bool {
    let s = ctx.sym(n);
    if s.name != "<anonymous>" || !matches!(s.kind, NodeKind::Union | NodeKind::Struct) {
        return false;
    }
    let Some(src) = ctx.source(&s.file) else {
        return false;
    };
    let head = src
        .lines
        .get((s.start_line as usize).wrapping_sub(1))
        .map(|l| js::slice_from(l, s.start_column as usize));
    let foot = src
        .lines
        .get((s.end_line.max(s.start_line) as usize).wrapping_sub(1));
    match (head, foot) {
        (Some(h), Some(f)) if !h.is_empty() && !f.is_empty() => {
            js::test(&ctx.re(r"^\s*(?:union|struct)\s*\{"), h)
                && js::test(&ctx.re(r"\}\s*;\s*(?://.*)?$"), f)
        }
        _ => false,
    }
}

/// The lines of the functions and classes nested in the class declaration `decl`.
fn cpp_nested_spans(ctx: &Ctx, decl: SymId, end: u32) -> Rc<Vec<(u32, u32)>> {
    ctx.memo(memo::NM, format!("ns\0{decl}"), || {
        let d = ctx.sym(decl);
        Rc::new(
            ctx.in_file(&d.file)
                .iter()
                .copied()
                .filter(|&n| {
                    let s = ctx.sym(n);
                    n != decl
                        && s.start_line >= d.start_line
                        && s.end_line.max(s.start_line) <= end
                        && (is_cpp_function_node(ctx, n)
                            || (CPP_CLASS_KINDS.contains(&s.kind)
                                && !is_cpp_anonymous_members(ctx, n)))
                })
                .map(|n| {
                    let s = ctx.sym(n);
                    (s.start_line, s.end_line.max(s.start_line))
                })
                .collect(),
        )
    })
}

/// The type a data member `name` of the calling function's class has, as written there.
pub fn cpp_callers_member(ctx: &Ctx, name: &str, caller: SymId, r: &Ref) -> Option<WrittenType> {
    let key = format!("cm\0{caller}\0{name}");
    if let Some(hit) = ctx.memo_get::<Option<WrittenType>>(memo::NM, &key) {
        return hit;
    }
    let owner = cpp_member_owner(ctx, caller).or_else(|| cpp_test_fixture(ctx, caller, r));
    let member = owner.and_then(|owner| {
        let decls =
            cpp_scope_declarations(ctx, &owner, &cpp_class_decls(ctx, &owner), caller).decls;
        let mut seen = HashSet::from([owner.clone()]);
        cpp_inherited_data_member(ctx, &decls, name, 0, &mut seen)
    });
    ctx.memo_set(memo::NM, key, member.clone());
    member
}

/// The data member `name` of a class, in one of its declarations or a class it derives from.
pub fn cpp_inherited_data_member(
    ctx: &Ctx,
    decls: &[SymId],
    name: &str,
    depth: u32,
    seen: &mut HashSet<String>,
) -> Option<WrittenType> {
    for &decl in decls {
        if let Some(own) = cpp_data_member_in(ctx, decl, name) {
            return Some(own);
        }
    }
    if depth >= 4 {
        return None;
    }
    for &decl in decls {
        for base in supertype_nodes(ctx, decl) {
            let b = ctx.sym(base);
            if !CPP_CLASS_KINDS.contains(&b.kind) || seen.contains(&b.qualified_name) {
                continue;
            }
            if is_cpp_dependent_base(ctx, decl, base) && !cpp_using_declares(ctx, decl, base, name)
            {
                continue;
            }
            seen.insert(b.qualified_name.clone());
            if let Some(inherited) = cpp_inherited_data_member(ctx, &[base], name, depth + 1, seen)
            {
                return Some(inherited);
            }
        }
    }
    None
}

/// `context.getSupertypeNodes(id)`: the targets of a node's resolved `extends`/`implements` edges.
pub fn supertype_nodes(ctx: &Ctx, id: SymId) -> Vec<SymId> {
    ctx.syms.supers.get(&id).cloned().unwrap_or_default()
}

/// The fixture class of a gtest `TEST_F(Fixture, Name)` or `TEST_P` body.
pub fn cpp_test_fixture(ctx: &Ctx, caller: SymId, r: &Ref) -> Option<String> {
    let c = ctx.sym(caller);
    if c.kind != NodeKind::Function || (c.name != "TEST_F" && c.name != "TEST_P") {
        return None;
    }
    let src = ctx.source(&c.file)?;
    let from = (c.start_line as usize)
        .saturating_sub(1)
        .min(src.lines.len());
    let to = (c.start_line as usize + 1).min(src.lines.len());
    let head = src.lines[from..to].join(" ");
    let m = js::caps(
        &ctx.re(r"\bTEST_[FP]\s*\(\s*((?:::\s*)?[A-Za-z_]\w*(?:\s*::\s*[A-Za-z_]\w*)*)\s*,"),
        &head,
    )?;
    let fixture = js::squash(js::group(&m, 1));
    let site = supertypes::NameSite {
        scope: aliases::parent_scope(&c.qualified_name).to_string(),
        file: c.file.clone(),
        line: c.start_line,
        parameters: Rc::new(HashSet::new()),
        self_id: None,
        class_scope: None,
    };
    match supertypes::class_written(ctx, &fixture, &site, r) {
        Tri::Found(f) if f.exact && CPP_CLASS_KINDS.contains(&ctx.kind(f.node)) => {
            Some(ctx.sym(f.node).qualified_name.clone())
        }
        _ => None,
    }
}

/// The type `*x` has when `x` has the type `ty`.
pub fn cpp_pointee(ctx: &Ctx, ty: &WrittenType) -> Option<WrittenType> {
    let text = js::replace_with(&ctx.re(r"&+\s*$"), &ty.text, "");
    let text = text.trim();
    let b = text.as_bytes();
    let mut depth = 0i32;
    for i in (0..b.len()).rev() {
        match b[i] {
            b'>' => depth += 1,
            b'<' => depth -= 1,
            b'*' if depth == 0 => return Some(ty.with_text(text[..i].trim())),
            _ => {}
        }
    }
    cpp_held_type(ctx, text).map(|held| ty.with_text(held))
}

/// The element type of a range of the type `ty`, as written.
pub fn cpp_range_element_of(ctx: &Ctx, ty: &WrittenType, at: &Ref) -> Option<WrittenType> {
    cpp_range_element_hops(ctx, ty, at, 0)
}

pub fn cpp_range_element_hops(
    ctx: &Ctx,
    ty: &WrittenType,
    at: &Ref,
    hops: u32,
) -> Option<WrittenType> {
    if ty.array.is_some_and(|a| a > 0) {
        return Some(cpp_array_element(ty));
    }
    let range = cpp_follow_written_aliases(ctx, ty, at)?;
    if aliases::is_pointer_type(&range.text) || hops > 2 {
        return None;
    }
    let segments = aliases::type_segments(ctx, &range.text)?;
    let last = segments.last()?.as_str();
    let library = segments[0] == "std" || segments[0] == "absl";
    if CPP_LIBRARY_SEQUENCES.contains(&last) || CPP_PROJECT_SEQUENCES.contains(&last) {
        if CPP_LIBRARY_SEQUENCES.contains(&last)
            && !library
            && !cpp_types_named(ctx, last).is_empty()
        {
            return None;
        }
        let element = aliases::template_arguments(&range.text)?
            .into_iter()
            .next()?;
        return Some(range.with_text(cpp_without_nullability(ctx, &element)));
    }
    if CPP_LIBRARY_MAPS.contains(&last) && library {
        let args = aliases::template_arguments(&range.text).unwrap_or_default();
        let (Some(key), Some(value)) = (args.first(), args.get(1)) else {
            return None;
        };
        if key.is_empty() || value.is_empty() {
            return None;
        }
        return Some(range.with_text(format!(
            "std::pair<const {}, {}>",
            cpp_without_nullability(ctx, key),
            cpp_without_nullability(ctx, value)
        )));
    }
    let cls = cpp_written_class(ctx, &range, at)?;
    let begin = cpp_return_type_of(ctx, &cpp_methods_of(ctx, cls, "begin", at))?;
    cpp_iterator_element(ctx, &begin, at, hops)
}

/// A written type without `typename`, `const` and `&`.
pub fn cpp_without_type_keywords(ctx: &Ctx, text: &str) -> String {
    js::replace_with(&ctx.re(r"\b(?:typename|const)\b|&"), text, " ")
        .trim()
        .to_string()
}

fn iterator_member_kind(member: &str) -> Option<&'static str> {
    Some(match member {
        "begin" | "end" | "find" | "lower_bound" | "upper_bound" => "iterator",
        "cbegin" | "cend" => "const_iterator",
        "rbegin" | "rend" => "reverse_iterator",
        "crbegin" | "crend" => "const_reverse_iterator",
        _ => return None,
    })
}

pub const CPP_ITERATOR_ALGORITHMS: &str = r"^(?:::\s*)?std\s*::\s*(?:find|find_if|find_if_not|lower_bound|upper_bound|min_element|max_element|partition_point|adjacent_find|next|prev)$";

/// The iterator a library container's `member` returns, where its type is written.
pub fn cpp_library_iterator_of(
    ctx: &Ctx,
    object: &WrittenType,
    member: &str,
) -> Option<WrittenType> {
    let kind = iterator_member_kind(member)?;
    if aliases::is_pointer_type(&object.text) {
        return None;
    }
    let container = cpp_without_type_keywords(ctx, &object.text);
    let segments = aliases::type_segments(ctx, &container)?;
    let last = segments.last()?.as_str();
    if segments[0] != "std" && segments[0] != "absl" {
        return None;
    }
    (CPP_LIBRARY_SEQUENCES.contains(&last) || CPP_LIBRARY_MAPS.contains(&last))
        .then(|| object.with_text(format!("{container}::{kind}")))
}

const CPP_LIBRARY_ADAPTORS: &[&str] = &["stack", "queue", "priority_queue"];

/// What a library container's or holder's `member` hands out.
pub fn cpp_library_element_of(
    ctx: &Ctx,
    object: &WrittenType,
    member: &str,
    at: &Ref,
) -> Option<WrittenType> {
    if aliases::is_pointer_type(&object.text) {
        return None;
    }
    let segments = aliases::type_segments(ctx, &cpp_without_type_keywords(ctx, &object.text))?;
    let last = segments.last()?.clone();
    let last = last.as_str();
    let text = js::replace_first(
        &ctx.re(r"^\s*(?:typename\s+)?const\s+"),
        &object.text,
        |_| String::new(),
    );
    let text = js::replace_first(&ctx.re(r"(?:\s*\bconst\b)?\s*&+\s*$"), &text, |_| {
        String::new()
    });
    let text = text.trim();
    let library = segments[0] == "std" || segments[0] == "absl";
    if matches!(member, "front" | "back" | "at") {
        if (library && CPP_LIBRARY_SEQUENCES.contains(&last))
            || CPP_PROJECT_SEQUENCES.contains(&last)
        {
            return cpp_range_element_of(ctx, object, at);
        }
        if member == "at" && library && CPP_LIBRARY_MAPS.contains(&last) {
            let value = aliases::template_arguments(text)?.get(1).cloned()?;
            return Some(object.with_text(cpp_without_nullability(ctx, &value)));
        }
    }
    if segments[0] == "std"
        && CPP_LIBRARY_ADAPTORS.contains(&last)
        && (member == "top" || ((member == "front" || member == "back") && last == "queue"))
    {
        let element = aliases::template_arguments(text)?.into_iter().next()?;
        return Some(object.with_text(cpp_without_nullability(ctx, &element)));
    }
    let held = cpp_held_type(ctx, text)?;
    if held.contains('[') {
        return None;
    }
    let holder = segments.join("::");
    if member == "get"
        && matches!(
            holder.as_str(),
            "std::unique_ptr" | "std::shared_ptr" | "absl::NoDestructor"
        )
    {
        return Some(object.with_text(format!("{held}*")));
    }
    if (member == "value" || member == "value_or")
        && matches!(
            holder.as_str(),
            "std::optional" | "absl::optional" | "absl::StatusOr"
        )
    {
        return Some(object.with_text(held));
    }
    None
}

/// What dereferencing an iterator of the type `ty` gives, as written.
fn cpp_iterator_element(ctx: &Ctx, ty: &WrittenType, at: &Ref, hops: u32) -> Option<WrittenType> {
    let iterator = cpp_follow_written_aliases(ctx, ty, at)?;
    if aliases::is_pointer_type(&iterator.text) {
        return cpp_pointee(ctx, &iterator);
    }
    let bare = cpp_without_type_keywords(ctx, &iterator.text);
    if let Some(n) = js::caps(
        &ctx.re(r"^(.*>)\s*::\s*(?:const_)?(?:reverse_)?iterator\s*$"),
        &bare,
    ) {
        return cpp_range_element_hops(ctx, &iterator.with_text(js::group(&n, 1)), at, hops + 1);
    }
    let cls = cpp_written_class(ctx, &iterator, at)?;
    let star = cpp_return_type_of(ctx, &cpp_methods_of(ctx, cls, "operator*", at))?;
    let t = js::replace_with(&ctx.re(r"&+\s*$"), &star.text, "");
    Some(star.with_text(t.trim()))
}

/// The type of the object `x.m` or `x->m` names a member of, given `x`'s text.
pub fn cpp_member_object(
    ctx: &Ctx,
    object: &str,
    op: &str,
    at: &Ref,
    depth: u32,
) -> Option<WrittenType> {
    let ty = cpp_expression_type(ctx, object, at, depth + 1)?;
    let followed = cpp_follow_written_aliases(ctx, &ty, at)?;
    if op == "->" {
        return cpp_dereferenced(ctx, &followed, at);
    }
    if aliases::is_pointer_type(&followed.text) || followed.array.is_some_and(|a| a > 0) {
        None
    } else {
        Some(followed)
    }
}

/// The type of a C++ expression as written where it is declared.
pub fn cpp_expression_type(ctx: &Ctx, expr: &str, at: &Ref, depth: u32) -> Option<WrittenType> {
    if depth > 4 {
        return None;
    }
    let mut text = expr.trim().to_string();
    while text.starts_with('(')
        && text.ends_with(')')
        && cpp_opening_paren(&text, text.len() - 1) == Some(0)
    {
        text = text[1..text.len() - 1].trim().to_string();
    }
    if let Some(rest) = text.strip_prefix('*') {
        let pointer = cpp_expression_type(ctx, rest, at, depth + 1)?;
        let followed = cpp_follow_written_aliases(ctx, &pointer, at)?;
        return cpp_dereferenced(ctx, &followed, at);
    }
    let caller = at.from;
    let c = ctx.sym(caller);
    let callers = WrittenType {
        text: String::new(),
        scope: c.qualified_name.clone(),
        file: at.file.clone(),
        line: at.line,
        parameters: aliases::template_parameters(ctx, caller),
        array: None,
        class_scope: None,
    };
    if text == "this" {
        let owner = cpp_member_owner(ctx, caller).or_else(|| cpp_test_fixture(ctx, caller, at))?;
        return Some(callers.with_text(format!("{owner}*")));
    }
    if let Some(name) = js::caps(&ctx.re(r"^(?:this\s*->\s*)?([A-Za-z_]\w*)$"), &text) {
        let name = js::group(&name, 1).to_string();
        let mut decl = ReceiverDeclaration::default();
        infer_cpp_receiver_type(ctx, &name, at, depth + 1, Some(&mut decl));
        if let Some(d) = decl.declared {
            return Some(d);
        }
        if let Some((line, init)) = &decl.initializer {
            if is_cpp_callers_declaration(ctx, &at.file, *line, at) {
                let mut at2 = at.clone();
                at2.line = *line;
                at2.column = 0;
                return cpp_expression_type(ctx, init, &at2, depth + 1);
            }
        }
        let raw = decl.raw.as_ref()?;
        if !decl.callers || decl.shadowed {
            return None;
        }
        let mut t = callers.with_text(raw.clone());
        t.array = decl.array;
        return Some(t);
    }
    if let Some(created) = js::caps(
        &ctx.re(r"^new\s+((?:::\s*)?[A-Za-z_][\w:]*(?:\s*<[^;{}()]*>)?)\s*(?:[({\[][\s\S]*)?$"),
        &text,
    ) {
        return Some(callers.with_text(format!("{}*", js::group(&created, 1).trim())));
    }
    if text.ends_with(']') {
        let open = cpp_opening_bracket(&text, text.len() - 1)?;
        if open == 0 {
            return None;
        }
        let container = cpp_expression_type(ctx, &text[..open], at, depth + 1)?;
        let followed = cpp_follow_written_aliases(ctx, &container, at)?;
        return cpp_subscript_element(ctx, &followed, at);
    }
    if !text.ends_with(')') {
        let field = js::caps(&ctx.re(r"^(.+?)\s*(\.|->)\s*([A-Za-z_]\w*)$"), &text)?;
        let member_name = js::group(&field, 3).to_string();
        let object = cpp_member_object(ctx, js::group(&field, 1), js::group(&field, 2), at, depth)?;
        if let Some(element) = cpp_pair_member(ctx, &object, &member_name, at) {
            return Some(element);
        }
        let cls = cpp_written_class(ctx, &object, at)?;
        let mut seen = HashSet::from([ctx.sym(cls).qualified_name.clone()]);
        return cpp_inherited_data_member(ctx, &[cls], &member_name, 0, &mut seen);
    }
    let open = cpp_opening_paren(&text, text.len() - 1)?;
    let callee = if open > 0 {
        text[..open].trim().to_string()
    } else {
        String::new()
    };
    if let Some(cast) = js::caps(
        &ctx.re(r"^(?:static|dynamic|const|reinterpret)_cast\s*<([\s\S]+)>$"),
        &callee,
    ) {
        return Some(callers.with_text(js::group(&cast, 1).trim()));
    }
    if let Some(made) = js::caps(
        &ctx.re(r"^(?:::\s*)?(?:std|absl)\s*::\s*make_(unique|shared)\s*<([\s\S]+)>$"),
        &callee,
    ) {
        return Some(callers.with_text(format!(
            "std::{}_ptr<{}>",
            js::group(&made, 1),
            js::group(&made, 2).trim()
        )));
    }
    if cpp_held_type(ctx, &callee).is_some() {
        return Some(callers.with_text(callee));
    }
    if js::test(&ctx.re(CPP_ITERATOR_ALGORITHMS), &callee) {
        let args = &text[open + 1..text.len() - 1];
        let first = split_cpp_top_level(ctx, args).into_iter().next()?;
        if first.is_empty() {
            return None;
        }
        return cpp_expression_type(ctx, &first, at, depth + 1);
    }
    if js::is_ident(&callee) {
        if let Some(wrapper) = cpp_expression_type(ctx, &callee, at, depth + 1) {
            if let Some(returned) = cpp_wrapper_return_type(ctx, &wrapper) {
                return Some(returned);
            }
        }
    }
    if let Some(member) = js::caps(
        &ctx.re(r"^(.+?)\s*(\.|->)\s*([A-Za-z_]\w*)\s*(<[\s\S]*>)?$"),
        &callee,
    ) {
        let method = js::group(&member, 3).to_string();
        let targs = js::group_opt(&member, 4).map(str::to_string);
        let object =
            cpp_member_object(ctx, js::group(&member, 1), js::group(&member, 2), at, depth);
        if let (Some(object), None) = (&object, &targs) {
            if let Some(iterator) = cpp_library_iterator_of(ctx, object, &method) {
                return Some(iterator);
            }
            if let Some(element) = cpp_library_element_of(ctx, object, &method, at) {
                return Some(element);
            }
        }
        let cls = cpp_written_class(ctx, &object?, at)?;
        let methods = cpp_methods_of(ctx, cls, &method, at);
        let Some(targs) = targs else {
            return cpp_return_type_of(ctx, &methods);
        };
        let templates: Vec<SymId> = methods
            .into_iter()
            .filter(|&f| !cpp_own_template_parameters(ctx, f).is_empty())
            .collect();
        return cpp_member_template_return(ctx, &templates, &targs, &callers)
            .or_else(|| cpp_return_type_of(ctx, &templates));
    }
    if !js::test(
        &ctx.re(r"^(?:[A-Za-z_]\w*\s*::\s*)*[A-Za-z_]\w*(?:\s*<[\s\S]*>)?$"),
        &callee,
    ) {
        return None;
    }
    let spelled = js::squash(&aliases::strip_template_arguments(&callee));
    if !spelled.contains("::") {
        if let Some(own) = cpp_implicit_this_method(ctx, &spelled, "this", caller, at) {
            return cpp_return_type_of(ctx, &[own]);
        }
    }
    for scope in aliases::scopes_within(aliases::parent_scope(&c.qualified_name)) {
        let q = if scope.is_empty() {
            spelled.clone()
        } else {
            format!("{scope}::{spelled}")
        };
        let fns: Vec<SymId> = ctx
            .by_qname(&q)
            .iter()
            .copied()
            .filter(|&n| is_cpp_function_node(ctx, n))
            .collect();
        if !fns.is_empty() {
            return cpp_return_type_of(ctx, &fns);
        }
    }
    match supertypes::class_written(ctx, &callee, &callers.site(), at) {
        Tri::Found(f) if f.exact && CPP_CLASS_KINDS.contains(&ctx.kind(f.node)) => {
            Some(callers.with_text(callee))
        }
        _ => None,
    }
}

/// The element type of the range the `auto` range-for on line `line` iterates.
pub fn cpp_auto_loop_element(
    ctx: &Ctx,
    lines: &[String],
    line: usize,
    colon: usize,
    r: &Ref,
) -> Option<WrittenType> {
    let key = format!("le\0{}\0{}\0{line}:{colon}", r.from, r.file);
    if let Some(hit) = ctx.memo_get::<Option<WrittenType>>(memo::NM, &key) {
        return hit;
    }
    ctx.memo_set(memo::NM, key.clone(), None::<WrittenType>);
    let mut at = r.clone();
    at.line = line as u32 + 1;
    at.column = colon as u32;
    let result = cpp_range_expression(ctx, lines, line, colon)
        .and_then(|range| cpp_expression_type(ctx, &range, &at, 0))
        .and_then(|ty| cpp_range_element_of(ctx, &ty, &at))
        .and_then(|element| cpp_follow_written_aliases(ctx, &element, &at))
        .filter(|f| !is_cpp_template_parameter_type(ctx, f));
    ctx.memo_set(memo::NM, key, result.clone());
    result
}

/// The type of the initializer `init` of the `auto` local `name` declared on line `line`.
pub fn cpp_auto_local_type(
    ctx: &Ctx,
    init: &str,
    line: u32,
    name: &str,
    r: &Ref,
) -> Option<WrittenType> {
    let key = format!("al\0{}\0{}\0{line}\0{name}", r.from, r.file);
    if let Some(hit) = ctx.memo_get::<Option<WrittenType>>(memo::NM, &key) {
        return hit;
    }
    ctx.memo_set(memo::NM, key.clone(), None::<WrittenType>);
    let mut at = r.clone();
    at.line = line;
    at.column = 0;
    let result = cpp_expression_type(ctx, init, &at, 0)
        .and_then(|ty| cpp_follow_written_aliases(ctx, &ty, &at))
        .filter(|f| !is_cpp_template_dependent_type(ctx, f));
    ctx.memo_set(memo::NM, key, result.clone());
    result
}

/// Is `ty` a template parameter's where it is written, or a library template of one?
pub fn is_cpp_template_dependent_type(ctx: &Ctx, ty: &WrittenType) -> bool {
    if ty.parameters.is_empty() {
        return false;
    }
    if is_cpp_template_parameter_type(ctx, ty) {
        return true;
    }
    if let Some(held) = cpp_held_type(ctx, &ty.text) {
        return is_cpp_template_dependent_type(ctx, &ty.with_text(held));
    }
    let library = is_cpp_library_type(ctx, &ty.text)
        || aliases::type_segments(ctx, &ty.text).is_some_and(|s| s[0] == "std");
    library
        && js::all(&ctx.re(r"[A-Za-z_]\w*"), &ty.text)
            .iter()
            .any(|w| ty.parameters.contains(w.get(0).unwrap().as_str()))
}

/// Where the string or character literal opening at `text[start]` closes.
pub fn cpp_literal_end(text: &str, start: usize) -> usize {
    let b = text.as_bytes();
    let quote = b[start];
    let mut i = start + 1;
    while i < b.len() {
        if b[i] == b'\\' {
            i += 1;
        } else if b[i] == quote {
            return i;
        }
        i += 1;
    }
    b.len()
}

/// Is column `use_column` of line `use_line` inside the body of the `for` whose header opens at `lines[line][paren]`?
pub fn cpp_for_body_encloses(
    lines: &[String],
    line: usize,
    paren: usize,
    use_line: usize,
    use_column: usize,
) -> bool {
    #[derive(PartialEq)]
    enum Phase {
        Before,
        Header,
        Gap,
        Block,
        Statement,
    }
    let mut phase = Phase::Before;
    let mut depth = 0i32;
    let mut comment = false;
    let mut l = line;
    while l <= use_line && l < lines.len() {
        let text = &lines[l];
        let b = text.as_bytes();
        let end = if l == use_line {
            use_column.min(b.len())
        } else {
            b.len()
        };
        let mut c = 0;
        while c < end {
            let ch = b[c];
            if comment {
                if ch == b'*' && b.get(c + 1) == Some(&b'/') {
                    comment = false;
                    c += 1;
                }
                c += 1;
                continue;
            }
            if ch == b'/' && b.get(c + 1) == Some(&b'/') {
                break;
            }
            if ch == b'/' && b.get(c + 1) == Some(&b'*') {
                comment = true;
                c += 2;
                continue;
            }
            if ch == b'"' || (ch == b'\'' && !(c > 0 && b[c - 1].is_ascii_digit())) {
                c = cpp_literal_end(text, c) + 1;
                continue;
            }
            if phase == Phase::Before {
                if l == line && c == paren {
                    phase = Phase::Header;
                    depth = 1;
                }
                c += 1;
                continue;
            }
            if phase == Phase::Header {
                if ch == b'(' {
                    depth += 1;
                } else if ch == b')' {
                    depth -= 1;
                    if depth == 0 {
                        phase = Phase::Gap;
                    }
                }
                c += 1;
                continue;
            }
            if phase == Phase::Gap {
                if js::is_space(ch) {
                    c += 1;
                    continue;
                }
                phase = if ch == b'{' {
                    Phase::Block
                } else {
                    Phase::Statement
                };
            }
            match ch {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' => depth -= 1,
                b'}' => {
                    depth -= 1;
                    if depth <= 0 {
                        return false;
                    }
                }
                b';' if depth <= 0 && phase == Phase::Statement => return false,
                _ => {}
            }
            c += 1;
        }
        l += 1;
    }
    phase != Phase::Before && phase != Phase::Header
}

/// What C++ receiver inference made of a receiver's declared type.
#[derive(Clone, Debug, Default)]
pub struct ReceiverDeclaration {
    /// `Some(true)`: an alias followed; `Some(false)`: an alias that can't be read.
    pub aliased: Option<bool>,
    pub alias: Option<aliases::AliasedType>,
    pub written: Option<String>,
    pub pointer: bool,
    pub class_template: bool,
    pub raw: Option<String>,
    pub constant: bool,
    pub array: Option<u32>,
    pub callers: bool,
    pub shadowed: bool,
    pub declared: Option<WrittenType>,
    pub initializer: Option<(u32, String)>,
}

/// A declared C++ type as receiver inference reads it.
pub fn cpp_declared_type(
    ctx: &Ctx,
    raw: &str,
    normalized: &str,
    in_caller_scope: bool,
    r: &Ref,
    found: Option<&mut ReceiverDeclaration>,
) -> Option<String> {
    if !in_caller_scope {
        return Some(normalized.to_string());
    }
    let aliased = aliases::resolve_aliased_type(ctx, raw, r);
    if aliased.is_undefined() {
        return Some(normalized.to_string());
    }
    let aliased = aliased.found();
    if let Some(found) = found {
        found.aliased = Some(aliased.is_some());
        found.alias = aliased.clone();
        found.written = Some(normalized.to_string());
        found.pointer = match &aliased {
            Some(a) => a.pointer,
            None => aliases::is_pointer_type(raw),
        };
        found.class_template = aliased.as_ref().is_some_and(|a| a.class_template);
    }
    aliased.map(|a| aliases::aliased_type_name(&a))
}

/// The classes, structs and unions a C or C++ file declares.
pub fn cpp_classes_in(ctx: &Ctx, file: &str) -> Rc<Vec<SymId>> {
    ctx.memo(memo::NM, format!("ci\0{file}"), || {
        Rc::new(
            ctx.in_file(file)
                .iter()
                .copied()
                .filter(|&n| CPP_CLASS_KINDS.contains(&ctx.kind(n)))
                .collect(),
        )
    })
}

/// The C and C++ types the project declares under `name`.
pub fn cpp_types_named(ctx: &Ctx, name: &str) -> Vec<SymId> {
    ctx.named(name)
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            is_c_family(s.language) && CPP_TYPE_KINDS.contains(&s.kind)
        })
        .collect()
}
