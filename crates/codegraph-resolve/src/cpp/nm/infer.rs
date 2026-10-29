//! C++ receiver inference: the type a receiver's declaration gives it, read
//! back from the call through the calling function, its class and the header
//! beside the file; and the calls that read a type off a written expression —
//! constructions and chained calls.

use super::*;

/// `cppLastSegment`: the last non-empty `::` segment.
pub fn cpp_last_segment(name: &str) -> &str {
    name.split("::")
        .filter(|p| !p.is_empty())
        .last()
        .unwrap_or(name)
}

const CPP_LEADING_CONST: &str = r"\b(?:const|constexpr)\s+$";

/// Record the C++ declaration receiver inference settles on.
fn note_cpp_declaration(
    found: &mut ReceiverDeclaration,
    raw: &str,
    callers: bool,
    array: u32,
    constant: bool,
) {
    found.raw = Some(raw.to_string());
    found.callers = callers;
    found.pointer = aliases::is_pointer_type(raw);
    found.array = (array > 0).then_some(array);
    found.constant = constant;
}

/// `inferCppReceiverType`: the last name of the type `receiver` is declared with where `r` calls on it.
pub fn infer_cpp_receiver_type(
    ctx: &Ctx,
    receiver: &str,
    r: &Ref,
    depth: u32,
    found: Option<&mut ReceiverDeclaration>,
) -> Option<String> {
    let mut local = ReceiverDeclaration::default();
    let found = match found {
        Some(f) => f,
        None => &mut local,
    };
    let src = ctx.source(&r.file)?;
    let lines = &src.lines;
    if lines.is_empty() {
        return None;
    }
    let call_line = (r.line as usize).saturating_sub(1).min(lines.len() - 1);
    let escaped = js::escape(receiver);
    let pattern = ctx.re(&format!(r"\b{escaped}\b"));
    let declarator = build_declarator_regex(ctx, &escaped);
    let listed = build_listed_declarator_regex(ctx, &escaped);
    let raw_lines = raw_strings::non_code_lines(ctx, &r.file);
    let c = ctx.sym(r.from);
    let member_caller = (c.file == r.file
        && matches!(c.kind, NodeKind::Method | NodeKind::Function))
    .then_some(r.from);
    let mut member_looked = member_caller.is_none();
    let leading_const = ctx.re(CPP_LEADING_CONST);

    let mut i = call_line as i64;
    while i >= 0 {
        let iu = i as usize;
        i -= 1;
        if let Some(mc) = member_caller {
            if !member_looked && (iu as i64) < ctx.sym(mc).start_line as i64 - 1 {
                member_looked = true;
                match cpp_member_receiver_type(ctx, receiver, mc, r, found) {
                    Tri::Undefined => {}
                    Tri::Null => return None,
                    Tri::Found(t) => return Some(t),
                }
            }
        }
        let text = &lines[iu];
        if text.is_empty() || !js::test(&pattern, text) {
            continue;
        }
        let Some(line) = cpp_code_of(
            ctx,
            &raw_strings::literals_blanked(text, iu + 1, &raw_lines),
        ) else {
            continue;
        };
        if line.is_empty() || !js::test(&pattern, &line) {
            continue;
        }
        let mut decl = match_cpp_declarator(ctx, &line, &declarator)
            .or_else(|| match_cpp_listed_declarator(ctx, &line, receiver, &listed));
        if let (Some(d), Some(mc)) = (&decl, member_caller) {
            if ctx.sym(mc).name == receiver && js::at(&line, d.end) == b'(' {
                decl = match_cpp_declarator_from(ctx, &line, &declarator, d.end).or(decl);
            }
        }
        if let Some(d) = decl {
            let normalized = normalize_cpp_type_name(ctx, &d.ty);
            match normalized.as_deref() {
                Some("auto") => {
                    let init = cpp_auto_initializer(ctx, &line, receiver);
                    let statement =
                        cpp_auto_initializer_statement(ctx, &line, lines, iu, receiver, &raw_lines);
                    if let Some(init) = &init {
                        if !found.shadowed && found.initializer.is_none() {
                            found.initializer = Some((
                                iu as u32 + 1,
                                statement.clone().unwrap_or_else(|| init.clone()),
                            ));
                        }
                    }
                    let local = match &statement {
                        Some(s)
                            if !found.shadowed
                                && is_cpp_callers_declaration(ctx, &r.file, iu as u32 + 1, r) =>
                        {
                            cpp_auto_local_type(ctx, s, iu as u32 + 1, receiver, r)
                        }
                        _ => None,
                    };
                    if let Some(local) = local {
                        if let Some(lt) =
                            normalize_cpp_type_name(ctx, &local.text).filter(|t| t != "auto")
                        {
                            note_cpp_declaration(found, &local.text, true, 0, false);
                            let text = local.text.clone();
                            found.declared = Some(local);
                            return cpp_declared_type(ctx, &text, &lt, true, r, Some(found));
                        }
                    }
                    if let Some(init) = &init {
                        if let Some(t) = infer_cpp_auto_initializer_type(ctx, init, r, depth) {
                            return Some(t);
                        }
                    }
                    found.shadowed = true;
                }
                Some(n) => {
                    let in_scope = is_cpp_callers_declaration(ctx, &r.file, iu as u32 + 1, r);
                    let array = cpp_array_dimensions(ctx, js::slice_from(&line, d.end));
                    let constant = js::test(&leading_const, &line[..d.index]);
                    note_cpp_declaration(found, &d.ty, in_scope, array, constant);
                    return cpp_declared_type(ctx, &d.ty, n, in_scope, r, Some(found));
                }
                None => {
                    if cpp_type_began_above(&d.ty) {
                        found.shadowed = true;
                    }
                }
            }
        } else {
            if let Some(lv) = cpp_range_for_declaration(ctx, &line, receiver) {
                if cpp_for_body_encloses(lines, iu, lv.paren, call_line, r.column as usize) {
                    let normalized = normalize_cpp_type_name(ctx, &lv.ty);
                    if let Some(n) = normalized.as_deref().filter(|n| *n != "auto") {
                        let in_scope = is_cpp_callers_declaration(ctx, &r.file, iu as u32 + 1, r);
                        note_cpp_declaration(found, &lv.ty, in_scope, 0, false);
                        return cpp_declared_type(ctx, &lv.ty, n, in_scope, r, Some(found));
                    }
                    let element = if normalized.as_deref() == Some("auto") {
                        cpp_auto_loop_element(ctx, lines, iu, lv.colon, r)
                    } else {
                        None
                    };
                    if let Some(element) = element {
                        if let Some(et) =
                            normalize_cpp_type_name(ctx, &element.text).filter(|t| t != "auto")
                        {
                            note_cpp_declaration(found, &element.text, true, 0, false);
                            let text = element.text.clone();
                            found.declared = Some(element);
                            return cpp_declared_type(ctx, &text, &et, true, r, Some(found));
                        }
                    }
                }
            }
            if let Some(begun) = cpp_declaration_begun_above(
                ctx,
                lines,
                iu,
                &line,
                &pattern,
                &declarator,
                &raw_lines,
            ) {
                if let Some(bt) = normalize_cpp_type_name(ctx, &begun).filter(|t| t != "auto") {
                    let in_scope = is_cpp_callers_declaration(ctx, &r.file, iu as u32 + 1, r);
                    note_cpp_declaration(found, &begun, in_scope, 0, false);
                    return cpp_declared_type(ctx, &begun, &bt, in_scope, r, Some(found));
                }
            }
            if cpp_ends_declaration_begun_above(ctx, &line, &escaped) {
                found.shadowed = true;
            }
            let bound = if found.shadowed {
                None
            } else {
                cpp_structured_binding_type(
                    ctx, &line, lines, iu, receiver, call_line, r, &raw_lines,
                )
            };
            if let Some(bound) = bound {
                if let Some(bt) = normalize_cpp_type_name(ctx, &bound.text).filter(|t| t != "auto")
                {
                    note_cpp_declaration(found, &bound.text, true, 0, false);
                    let text = bound.text.clone();
                    found.declared = Some(bound);
                    return cpp_declared_type(ctx, &text, &bt, true, r, Some(found));
                }
            }
            if cpp_rebinds_receiver(ctx, &line, &escaped) {
                found.shadowed = true;
            }
        }
    }
    if let (false, Some(mc)) = (member_looked, member_caller) {
        match cpp_member_receiver_type(ctx, receiver, mc, r, found) {
            Tri::Undefined => {}
            Tri::Null => return None,
            Tri::Found(t) => return Some(t),
        }
    }

    let source_ext = ctx.re(r"(?i)\.(?:c|cc|cpp|cxx)$");
    let mut headers: Vec<String> = Vec::new();
    for ext in [".h", ".hpp", ".hxx"] {
        let h = js::replace_first(&source_ext, &r.file, |_| ext.to_string());
        if !headers.contains(&h) && h != r.file {
            headers.push(h);
        }
    }
    for header in headers {
        if !ctx.file_exists(&header) {
            continue;
        }
        let Some(hsrc) = ctx.source(&header) else {
            continue;
        };
        let hraw = raw_strings::non_code_lines(ctx, &header);
        for (i, text) in hsrc.lines.iter().enumerate() {
            if !js::test(&pattern, text) {
                continue;
            }
            let Some(line) = cpp_code_of(ctx, &raw_strings::literals_blanked(text, i + 1, &hraw))
            else {
                continue;
            };
            if line.is_empty() {
                continue;
            }
            let Some(d) = match_cpp_declarator(ctx, &line, &declarator)
                .or_else(|| match_cpp_listed_declarator(ctx, &line, receiver, &listed))
            else {
                continue;
            };
            if let Some(n) = normalize_cpp_type_name(ctx, &d.ty).filter(|n| n != "auto") {
                let in_scope = is_cpp_callers_declaration(ctx, &header, i as u32 + 1, r);
                let array = cpp_array_dimensions(ctx, js::slice_from(&line, d.end));
                let constant = js::test(&leading_const, &line[..d.index]);
                note_cpp_declaration(found, &d.ty, in_scope, array, constant);
                return cpp_declared_type(ctx, &d.ty, &n, in_scope, r, Some(found));
            }
        }
    }
    None
}

/// The type of a C++ receiver the calling function doesn't declare, as a data member of its class.
fn cpp_member_receiver_type(
    ctx: &Ctx,
    receiver: &str,
    caller: SymId,
    r: &Ref,
    found: &mut ReceiverDeclaration,
) -> Tri<String> {
    if found.shadowed {
        return Tri::Undefined;
    }
    let Some(member) = cpp_callers_member(ctx, receiver, caller, r) else {
        return Tri::Undefined;
    };
    let Some(normalized) = normalize_cpp_type_name(ctx, &member.text) else {
        return Tri::Undefined;
    };
    let followed = cpp_follow_written_aliases(ctx, &member, r);
    if is_cpp_template_dependent_type(ctx, followed.as_ref().unwrap_or(&member))
        || aliases::resolve_aliased_type(ctx, &member.text, r).is_null()
    {
        return Tri::Undefined;
    }
    note_cpp_declaration(found, &member.text, true, 0, false);
    let text = member.text.clone();
    found.declared = Some(member);
    match cpp_declared_type(ctx, &text, &normalized, true, r, Some(found)) {
        Some(t) => Tri::Found(t),
        None => Tri::Null,
    }
}

/// `lookupCalleeReturnType`: the return type recorded for `Class::method` or a free function.
pub fn lookup_callee_return_type(ctx: &Ctx, callee: &str, r: &Ref) -> Option<String> {
    let (method, cls) = if callee.contains("::") {
        let parts: Vec<&str> = callee.split("::").filter(|p| !p.is_empty()).collect();
        (
            parts.last().copied().unwrap_or(callee).to_string(),
            Some(parts[..parts.len().saturating_sub(1)].join("::")),
        )
    } else {
        (callee.to_string(), None)
    };
    let candidates: Vec<SymId> = ctx
        .named(&method)
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            matches!(s.kind, NodeKind::Method | NodeKind::Function)
                && s.language == r.lang
                && s.return_type.as_deref().is_some_and(|t| !t.is_empty())
        })
        .collect();
    if let Some(cls) = cls {
        let want = format!("{cls}::{method}");
        return candidates
            .iter()
            .map(|&n| ctx.sym(n))
            .find(|s| {
                s.qualified_name == want
                    || s.qualified_name.ends_with(&format!("::{want}"))
                    || want.ends_with(&format!("::{}", s.qualified_name))
            })
            .and_then(|s| s.return_type.clone());
    }
    candidates
        .iter()
        .map(|&n| ctx.sym(n))
        .find(|s| s.kind == NodeKind::Function)
        .and_then(|s| s.return_type.clone())
}

fn cpp_class_exists(ctx: &Ctx, name: &str, r: &Ref) -> bool {
    ctx.named(cpp_last_segment(name)).iter().any(|&n| {
        let s = ctx.sym(n);
        CPP_CLASS_KINDS.contains(&s.kind) && s.language == r.lang
    })
}

/// `resolveCppCallResultType`: the class a C++ call or construction expression produces.
pub fn resolve_cpp_call_result_type(ctx: &Ctx, inner: &str, r: &Ref, depth: u32) -> Option<String> {
    if depth > 3 {
        return None;
    }
    let expr = inner.trim();
    if let Some(m) = js::caps(
        &ctx.re(r"(?:^|::)(?:make_unique|make_shared)\s*<\s*([A-Za-z_]\w*)"),
        expr,
    ) {
        return Some(js::group(&m, 1).to_string());
    }
    if let Some(dot) = expr.rfind('.').filter(|&d| d > 0) {
        let recv = &expr[..dot];
        let method = &expr[dot + 1..];
        if recv.contains('.') || recv.contains('(') || recv.contains("::") {
            return None;
        }
        let recv_type = infer_cpp_receiver_type(ctx, recv, r, depth + 1, None)?;
        return lookup_callee_return_type(ctx, &format!("{recv_type}::{method}"), r);
    }
    if let Some(ret) = lookup_callee_return_type(ctx, expr, r) {
        return Some(ret);
    }
    if cpp_class_exists(ctx, expr, r) {
        return Some(cpp_last_segment(expr).to_string());
    }
    None
}

/// The initializer of `auto receiver = …;` on `line`.
fn cpp_auto_initializer(ctx: &Ctx, line: &str, receiver: &str) -> Option<String> {
    let m = js::caps(
        &ctx.re(&format!(r"\b{}\b\s*=\s*([^;]+)", js::escape(receiver))),
        line,
    )?;
    let init = js::group(&m, 1).trim();
    (!init.is_empty()).then(|| init.to_string())
}

/// The whole initializer of `auto receiver = …;` that `line` begins, read on to its `;`.
fn cpp_auto_initializer_statement(
    ctx: &Ctx,
    line: &str,
    lines: &[String],
    index: usize,
    receiver: &str,
    raw_lines: &NonCode,
) -> Option<String> {
    let m = js::caps(
        &ctx.re(&format!(r"\b{}\b\s*=(?!=)", js::escape(receiver))),
        line,
    )?;
    let end = m.get(0).unwrap().end();
    cpp_statement_from(ctx, &line[end..], lines, index, raw_lines)
}

/// The C++ statement `text`, the rest of `lines[index]`, begins, read on to the `;` that ends it.
pub fn cpp_statement_from(
    ctx: &Ctx,
    text: &str,
    lines: &[String],
    index: usize,
    raw_lines: &NonCode,
) -> Option<String> {
    let splice = ctx.re(r"\\\s*$");
    let spliced = |code: &str| js::replace_with(&splice, code, " ");
    let mut text = spliced(text);
    let mut next = index + 1;
    loop {
        if let Some(end) = cpp_statement_end(&text) {
            let s = text[..end].trim();
            return (!s.is_empty()).then(|| s.to_string());
        }
        if next >= lines.len() || next > index + 4 {
            return None;
        }
        let code = cpp_code_of(
            ctx,
            &raw_strings::literals_blanked(&lines[next], next + 1, raw_lines),
        )
        .unwrap_or_default();
        text.push(' ');
        text.push_str(&spliced(&code));
        next += 1;
    }
}

const CPP_STRUCTURED_BINDING: &str = r"\bauto\s*&{0,2}\s*\[([^\]]*)\]";

/// The type the structured binding on line `index` gives `receiver`, as written.
#[allow(clippy::too_many_arguments)]
fn cpp_structured_binding_type(
    ctx: &Ctx,
    line: &str,
    lines: &[String],
    index: usize,
    receiver: &str,
    call_line: usize,
    r: &Ref,
    raw_lines: &NonCode,
) -> Option<WrittenType> {
    if !line.contains('[') {
        return None;
    }
    for m in js::all(&ctx.re(CPP_STRUCTURED_BINDING), line) {
        let Some(place) = js::group(&m, 1)
            .split(',')
            .map(str::trim)
            .position(|n| n == receiver)
        else {
            continue;
        };
        let whole = m.get(0).unwrap();
        let end = whole.end();
        let lp = js::caps(
            &ctx.re(r"\bfor\s*\(\s*(?:(?:const|volatile)\s+)*$"),
            &line[..whole.start()],
        )
        .map(|c| c.get(0).unwrap().start());
        let colon = js::caps(&ctx.re(r"^\s*:(?!:)"), &line[end..]).map(|c| c.get(0).unwrap().end());
        let value = if let (Some(lp), Some(colon)) = (lp, colon) {
            let paren = js::index_of(line, "(", lp).unwrap_or(0);
            if !cpp_for_body_encloses(lines, index, paren, call_line, r.column as usize) {
                return None;
            }
            cpp_auto_loop_element(ctx, lines, index, end + colon, r)
        } else {
            let assign =
                js::caps(&ctx.re(r"^\s*=(?!=)"), &line[end..]).map(|c| c.get(0).unwrap().end());
            let assign =
                assign.filter(|_| is_cpp_callers_declaration(ctx, &r.file, index as u32 + 1, r))?;
            cpp_statement_from(ctx, &line[end + assign..], lines, index, raw_lines).and_then(
                |init| cpp_auto_local_type(ctx, &init, index as u32 + 1, whole.as_str(), r),
            )
        };
        return value
            .and_then(|v| cpp_binding_elements(ctx, &v, r))
            .and_then(|e| e.into_iter().nth(place));
    }
    None
}

/// What calling a function wrapper of the written type `ty` returns, as written.
pub fn cpp_wrapper_return_type(_ctx: &Ctx, ty: &WrittenType) -> Option<WrittenType> {
    let signature = aliases::template_arguments(&ty.text)
        .and_then(|a| a.into_iter().next())
        .unwrap_or_default();
    let mut depth = 0i32;
    for (i, c) in signature.bytes().enumerate() {
        match c {
            b'<' => depth += 1,
            b'>' => depth -= 1,
            b'(' if depth == 0 => {
                let returned = signature[..i].trim();
                return (!returned.is_empty()).then(|| ty.with_text(returned));
            }
            _ => {}
        }
    }
    None
}

/// What a structured binding of a value of the type `ty` names, in order.
fn cpp_binding_elements(ctx: &Ctx, ty: &WrittenType, at: &Ref) -> Option<Vec<WrittenType>> {
    let followed = cpp_follow_written_aliases(ctx, ty, at)?;
    let segments = aliases::type_segments(ctx, &followed.text)?;
    if segments.len() != 2
        || segments[0] != "std"
        || (segments[1] != "pair" && segments[1] != "tuple")
    {
        return None;
    }
    Some(
        aliases::template_arguments(&followed.text)?
            .iter()
            .map(|t| followed.with_text(cpp_without_nullability(ctx, t)))
            .collect(),
    )
}

/// The member `first` or `second` of a `std::pair` of the type `pair`.
pub fn cpp_pair_member(
    ctx: &Ctx,
    pair: &WrittenType,
    member: &str,
    at: &Ref,
) -> Option<WrittenType> {
    let index = match member {
        "first" => 0,
        "second" => 1,
        _ => return None,
    };
    let followed = cpp_follow_written_aliases(ctx, pair, at)?;
    if aliases::type_segments(ctx, &followed.text)?.join("::") != "std::pair" {
        return None;
    }
    let element = aliases::template_arguments(&followed.text)?
        .into_iter()
        .nth(index)?;
    if element.is_empty() {
        return None;
    }
    Some(followed.with_text(cpp_without_nullability(ctx, &element)))
}

/// Where the statement `text` begins ends: its first `;` outside brackets.
fn cpp_statement_end(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in text.bytes().enumerate() {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b';' if depth <= 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// The type of an `auto` local from its initializer `init`.
fn infer_cpp_auto_initializer_type(ctx: &Ctx, init: &str, r: &Ref, depth: u32) -> Option<String> {
    if let Some(m) = js::caps(&ctx.re(r"^new\s+([A-Za-z_][\w:]*)"), init) {
        return Some(cpp_last_segment(js::group(&m, 1)).to_string());
    }
    if let Some(m) = js::caps(&ctx.re(r"^([A-Za-z_][\w:]*(?:\s*<[^>;]*>)?)\s*\("), init) {
        let callee: String = js::group(&m, 1)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        return resolve_cpp_call_result_type(ctx, &callee, r, depth + 1);
    }
    None
}

/// `matchCppConstruction`: a C++ construction makes the class C++ lookup finds for the type as written.
pub fn match_cpp_construction(ctx: &Ctx, r: &Ref) -> Tri<Match> {
    let line = ctx.line(&r.file, r.line);
    let text = js::slice_from(&line, r.column as usize);
    if text.is_empty() {
        return Tri::Undefined;
    }
    let specifiers = js::caps(
        &ctx.re(r"^(?:(?:new|const|volatile|static|thread_local|constexpr|constinit|mutable|struct|class|union|typename)\s+)*"),
        text,
    )
    .map_or(0, |c| c.get(0).unwrap().end());
    let Some(written) = cpp_type_written_at(&text[specifiers..]) else {
        return Tri::Undefined;
    };
    let made = |node: SymId, exact: bool| {
        if exact {
            Match::new(node, "qualified-name", 0.9)
        } else {
            Match::new(node, "exact-match", 0.7)
        }
    };
    if aliases::declares_local_alias(ctx, &written, r) {
        let named = match aliases::resolve_aliased_type(ctx, &written, r) {
            Tri::Found(a) if !a.pointer => aliases::aliased_class(ctx, &a, Some(r)),
            _ => None,
        };
        let cls = named.and_then(|n| {
            ctx.by_qname(&n)
                .iter()
                .copied()
                .find(|&c| CPP_CLASS_KINDS.contains(&ctx.kind(c)))
        });
        return match cls {
            Some(c) => Tri::Found(made(c, true)),
            None => Tri::Null,
        };
    }
    let site = cpp_call_site(ctx, r.from, r).site();
    let found = match supertypes::class_written(ctx, &written, &site, r) {
        Tri::Null => {
            return match cpp_enum_written(ctx, &written, &site) {
                Some(e) => Tri::Found(made(e, true)),
                None => Tri::Null,
            };
        }
        Tri::Undefined => return Tri::Undefined,
        Tri::Found(f) => f,
    };
    let namesakes: Vec<SymId> = ctx
        .by_qname(&ctx.sym(found.node).qualified_name)
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            s.file == r.file && (CPP_CLASS_KINDS.contains(&s.kind) || s.kind == NodeKind::TypeAlias)
        })
        .collect();
    let ty = if namesakes.len() > 1 {
        cpp_namesake_around(ctx, &namesakes, r)
    } else {
        Some(found.node)
    };
    match ty {
        Some(t) => Tri::Found(made(t, found.exact)),
        None => Tri::Undefined,
    }
}

/// The enum a C++ type written at `site` names.
fn cpp_enum_written(ctx: &Ctx, written: &str, site: &supertypes::NameSite) -> Option<SymId> {
    let global = written.trim_start().starts_with("::");
    let path: String = aliases::strip_template_arguments(written)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let path = path.strip_prefix("::").unwrap_or(&path).to_string();
    let scopes = if global {
        vec![String::new()]
    } else {
        aliases::scopes_within(&site.scope)
    };
    for scope in scopes {
        let q = if scope.is_empty() {
            path.clone()
        } else {
            format!("{scope}::{path}")
        };
        let enums: Vec<SymId> = ctx
            .by_qname(&q)
            .iter()
            .copied()
            .filter(|&n| {
                let s = ctx.sym(n);
                s.kind == NodeKind::Enum && (s.file == site.file || !is_c_source(ctx, &s.file))
            })
            .collect();
        if !enums.is_empty() {
            return enums
                .iter()
                .copied()
                .find(|&n| ctx.sym(n).file == site.file)
                .or(Some(enums[0]));
        }
    }
    None
}

/// Of several classes or aliases `r`'s file declares under one qualified name, the one whose holder holds `r`.
fn cpp_namesake_around(ctx: &Ctx, namesakes: &[SymId], r: &Ref) -> Option<SymId> {
    let holders: Vec<SymId> = ctx
        .in_file(&r.file)
        .iter()
        .copied()
        .filter(|&n| {
            matches!(
                ctx.kind(n),
                NodeKind::Function
                    | NodeKind::Method
                    | NodeKind::Class
                    | NodeKind::Struct
                    | NodeKind::Union
            )
        })
        .collect();
    let around: Vec<SymId> = namesakes
        .iter()
        .copied()
        .filter(|&cls| {
            let c = ctx.sym(cls);
            let mut holder: Option<SymId> = None;
            for &n in &holders {
                let s = ctx.sym(n);
                if n == cls || s.start_line > c.start_line || s.end_line < c.end_line {
                    continue;
                }
                if holder.is_none_or(|h| {
                    let hs = ctx.sym(h);
                    (s.end_line as i64 - s.start_line as i64)
                        < (hs.end_line as i64 - hs.start_line as i64)
                }) {
                    holder = Some(n);
                }
            }
            holder.is_none_or(|h| {
                let hs = ctx.sym(h);
                hs.start_line <= r.line && r.line <= hs.end_line
            })
        })
        .collect();
    (around.len() == 1).then(|| around[0])
}

/// The type name `text` starts with, template arguments and all.
pub fn cpp_type_written_at(text: &str) -> Option<String> {
    let b = text.as_bytes();
    let mut end = 0usize;
    let mut first = true;
    loop {
        // `^(?:::\s*)?[A-Za-z_]\w*` first, then `^\s*::\s*[A-Za-z_]\w*`.
        let mut j = end;
        let mut ok = false;
        if !first {
            while j < b.len() && js::is_space(b[j]) {
                j += 1;
            }
        }
        if j + 1 < b.len() && b[j] == b':' && b[j + 1] == b':' {
            j += 2;
            while j < b.len() && js::is_space(b[j]) {
                j += 1;
            }
        } else if !first {
            j = usize::MAX;
        }
        if j != usize::MAX && j < b.len() && (b[j].is_ascii_alphabetic() || b[j] == b'_') {
            j += 1;
            while j < b.len() && js::is_word(b[j]) {
                j += 1;
            }
            ok = true;
        }
        if !ok {
            return (end > 0).then(|| text[..end].to_string());
        }
        first = false;
        end = j;
        let mut k = end;
        while k < b.len() && js::is_space(b[k]) {
            k += 1;
        }
        if k >= b.len() || b[k] != b'<' {
            continue;
        }
        end = k + 1;
        let mut angles = 1i32;
        let mut parens = 0i32;
        while angles > 0 {
            let c = *b.get(end)?;
            end += 1;
            if c == b'(' {
                parens += 1;
            } else if c == b')' {
                parens -= 1;
            } else if parens > 0 {
                continue;
            } else if c == b'<' {
                angles += 1;
            } else if c == b'>' {
                angles -= 1;
            }
        }
    }
}

/// `matchCppCallChain`: a C++ chained call whose receiver is itself a call, `inner().method`.
pub fn match_cpp_call_chain(ctx: &Ctx, r: &Ref) -> Option<Match> {
    let m = js::caps(&ctx.re(r"^(.+)\(\)\.(\w+)$"), &r.name)?;
    let inner = js::group(&m, 1);
    let method = js::group(&m, 2);
    match match_cpp_chain_on_written_type(ctx, inner, method, r) {
        Tri::Undefined => {}
        Tri::Null => return None,
        Tri::Found(found) => return Some(found),
    }
    let cls = resolve_cpp_call_result_type(ctx, inner, r, 0)?;
    resolve_method_on_type(ctx, &cls, method, r, 0.85, "instance-method", 0)
}

fn match_cpp_chain_on_written_type(ctx: &Ctx, inner: &str, method: &str, r: &Ref) -> Tri<Match> {
    let Some(followed) =
        cpp_chain_inner_type(ctx, inner, r).and_then(|t| cpp_follow_written_aliases(ctx, &t, r))
    else {
        return Tri::Undefined;
    };
    if is_cpp_template_dependent_type(ctx, &followed) {
        return Tri::Undefined;
    }
    let object = cpp_pointee(ctx, &followed).unwrap_or(followed);
    if is_cpp_library_type(ctx, &object.text) {
        return Tri::Null;
    }
    let site = object.site();
    let found = supertypes::class_written(ctx, &object.text, &site, r);
    if found.is_undefined() && is_cpp_outside_qualified_type(ctx, &object.text, &site) {
        return Tri::Null;
    }
    let Tri::Found(found) = found else {
        return Tri::Undefined;
    };
    if !found.exact || !CPP_CLASS_KINDS.contains(&ctx.kind(found.node)) {
        return Tri::Undefined;
    }
    if let Some(target) = cpp_method_of(ctx, found.node, method, r) {
        return Tri::Found(Match::new(target, "instance-method", 0.85));
    }
    if cpp_lacks_method(ctx, found.node) {
        Tri::Null
    } else {
        Tri::Undefined
    }
}

/// What the inner call of a C++ chain returns, as written where it is declared.
fn cpp_chain_inner_type(ctx: &Ctx, inner: &str, r: &Ref) -> Option<WrittenType> {
    let Some(dot) = inner.rfind('.') else {
        return cpp_expression_type(ctx, &format!("{inner}()"), r, 1);
    };
    let recv = &inner[..dot];
    if !js::is_ident(recv) {
        return None;
    }
    let op = cpp_member_operator(ctx, recv, r)?;
    let recv_type = cpp_expression_type(ctx, recv, r, 1)?;
    let followed = cpp_follow_written_aliases(ctx, &recv_type, r)?;
    let object = if op == "->" {
        cpp_dereferenced(ctx, &followed, r)
    } else if aliases::is_pointer_type(&followed.text) {
        None
    } else {
        Some(followed)
    }?;
    let name = &inner[dot + 1..];
    if let Some(element) = cpp_library_element_of(ctx, &object, name, r) {
        return Some(element);
    }
    let cls = cpp_written_class(ctx, &object, r)?;
    cpp_return_type_of(ctx, &cpp_methods_of(ctx, cls, name, r))
}

/// `getSupertypes(typeName, language)`: the names of what the same-named types extend or implement.
pub fn get_supertypes(ctx: &Ctx, type_name: &str, lang: Language) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for &tn in ctx.named(type_name) {
        let s = ctx.sym(tn);
        if s.language != lang
            || !matches!(
                s.kind,
                NodeKind::Class
                    | NodeKind::Struct
                    | NodeKind::Interface
                    | NodeKind::Trait
                    | NodeKind::Protocol
                    | NodeKind::Enum
            )
        {
            continue;
        }
        for &sup in ctx.syms.supers.get(&tn).map(Vec::as_slice).unwrap_or(&[]) {
            let name = &ctx.sym(sup).name;
            if !name.is_empty() && name != type_name && !out.contains(name) {
                out.push(name.clone());
            }
        }
    }
    out
}

/// `resolveMethodOnType`, for a C or C++ reference.
pub fn resolve_method_on_type(
    ctx: &Ctx,
    type_name: &str,
    method: &str,
    r: &Ref,
    confidence: f32,
    by: &'static str,
    depth: u32,
) -> Option<Match> {
    let matches = method_matches(ctx, type_name, method, r.lang);
    if matches.is_empty() {
        if depth < 4 {
            for sup in get_supertypes(ctx, type_name, r.lang) {
                if let Some(via) =
                    resolve_method_on_type(ctx, &sup, method, r, confidence, by, depth + 1)
                {
                    return Some(via);
                }
            }
        }
        return None;
    }
    if r.kind == ReferenceKind::FunctionRef && matches.len() != 1 {
        return None;
    }
    let ordered = prefer_call_site_file(
        ctx,
        &within_cpp_translation_unit(ctx, &cpp_visible_first(ctx, &matches, r), r, false),
        &r.file,
    );
    Some(Match::new(ordered[0], by, confidence))
}
