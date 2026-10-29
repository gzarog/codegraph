//! A C++ call's arguments and template arguments, and how they fit an
//! overload's parameters (name-matcher.ts, `cppSpecializationArguments` to
//! `isCppDependentBase`, with `splitCppTopLevel` … `cppOverloadFit` and
//! `retargetSelfOverload`).

use super::*;

/// The template arguments the C++ function `n` is an explicit specialization for.
pub fn cpp_specialization_arguments(ctx: &Ctx, n: SymId) -> Option<Vec<String>> {
    let s = ctx.sym(n);
    if s.language != Language::Cpp
        || !matches!(s.kind, NodeKind::Function | NodeKind::Method)
        || s.signature.as_deref().is_some_and(|g| g.starts_with('('))
    {
        return None;
    }
    ctx.memo(memo::NM, format!("sa\0{n}"), || {
        let head = match s.signature.as_deref().filter(|g| !g.is_empty()) {
            Some(sig) => sig.to_string(),
            None => {
                let src = ctx.source(&s.file);
                src.map(|src| {
                    let from = (s.start_line as usize)
                        .saturating_sub(1)
                        .min(src.lines.len());
                    let to = (s.start_line as usize + 11).min(src.lines.len());
                    js::slice_from(&src.lines[from..to].join("\n"), s.start_column as usize)
                        .to_string()
                })
                .unwrap_or_default()
            }
        };
        cpp_template_arguments_after(ctx, &head, &s.name, 0)
    })
}

/// The template arguments the C++ call at `r` writes after the name it calls.
pub fn cpp_call_template_arguments(ctx: &Ctx, r: &Ref) -> Option<Vec<String>> {
    let src = ctx.source(&r.file)?;
    let from = (r.line as usize).saturating_sub(1).min(src.lines.len());
    let to = (r.line as usize + 3).min(src.lines.len());
    let text = src.lines[from..to].join("\n");
    let name = js::split_last_member(&r.name);
    cpp_template_arguments_after(ctx, &text, name, r.column as usize)
}

/// The template arguments `text` writes right after `name`, the first time it is a word of its own from `from`.
pub fn cpp_template_arguments_after(
    ctx: &Ctx,
    text: &str,
    name: &str,
    from: usize,
) -> Option<Vec<String>> {
    if name.is_empty() {
        return None;
    }
    let word = |b: u8| js::is_word(b) || b == b'$';
    let mut at = js::index_of(text, name, from);
    while let Some(a) = at {
        let open = a + name.len();
        if word(js::before(text, a)) || word(js::at(text, open)) {
            at = js::index_of(text, name, a + 1);
            continue;
        }
        let end = if js::at(text, open) == b'<' {
            cpp_template_arguments_end(text, open)
        } else {
            None
        };
        let args: Vec<String> = match end {
            Some(e) => split_cpp_top_level(ctx, &text[open + 1..e])
                .iter()
                .map(|a| cpp_template_argument_key(a))
                .collect(),
            None => Vec::new(),
        };
        return (!args.is_empty()).then_some(args);
    }
    None
}

/// A C++ template argument as it compares: spaced only between words.
pub fn cpp_template_argument_key(arg: &str) -> String {
    // `arg.replace(/\s*([^\w\s])\s*/g, '$1').replace(/\s+/g, ' ')`
    let mut out = String::with_capacity(arg.len());
    let chars: Vec<char> = arg.chars().collect();
    let is_w = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            // Spaces before a punctuation character go.
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && !is_w(chars[j]) && !chars[j].is_whitespace() {
                i = j;
                continue;
            }
            out.push(c);
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
        if !is_w(c) {
            // Spaces after one go too.
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
        }
    }
    js::collapse_ws(&out)
}

/// Whether a call's template arguments are the ones a specialization writes.
pub fn cpp_template_arguments_match(written: &[String], call: &[String]) -> bool {
    written.len() == call.len()
        && written.iter().zip(call.iter()).all(|(w, c)| {
            w == c || w.ends_with(&format!("::{c}")) || c.ends_with(&format!("::{w}"))
        })
}

/// Whether the C++ function `n`, named `name`, is declared deleted.
pub fn is_cpp_deleted_function(ctx: &Ctx, n: SymId, name: &str) -> bool {
    if cpp_macro_method_head(ctx, n).is_some() {
        return false;
    }
    ctx.memo(memo::NM, format!("del\0{n}"), || {
        let s = ctx.sym(n);
        let Some(src) = ctx.source(&s.file) else {
            return false;
        };
        let from = (s.start_line as usize)
            .saturating_sub(1)
            .min(src.lines.len());
        let to = (s.start_line as usize + 11).min(src.lines.len());
        let text = src.lines[from..to].join("\n");
        let Some(at) = js::search(&ctx.re(&format!(r"\b{}\s*\(", js::escape(name))), &text) else {
            return false;
        };
        let declaration = text[at..].split([';', '{']).next().unwrap_or("");
        js::test(&ctx.re(r"=\s*delete\b"), declaration)
    })
}

/// What a C++ call's argument is: a literal's kind, or the type it has as written.
#[derive(Clone, Debug, PartialEq)]
pub enum ArgumentType {
    Char,
    Number,
    Zero,
    Bool,
    Null,
    Type {
        text: String,
        lvalue: Option<bool>,
        dependent: bool,
    },
}

/// What the argument `arg` of the call at `r` is, where that can be read.
pub fn cpp_argument_type(ctx: &Ctx, arg: &str, r: &Ref) -> Option<ArgumentType> {
    let text = arg.trim();
    if js::test(&ctx.re(r"^(?:u8|u|U|L)?'(?:[^'\\]|\\.)+'$"), text) {
        return Some(ArgumentType::Char);
    }
    if text == "true" || text == "false" {
        return Some(ArgumentType::Bool);
    }
    if text == "nullptr" || text == "NULL" {
        return Some(ArgumentType::Null);
    }
    if js::test(&ctx.re(r"^0+[uUlL]*$"), text) {
        return Some(ArgumentType::Zero);
    }
    if js::test(
        &ctx.re(r"^[-+]?(?:0[xX][\da-fA-F']+|\d[\d']*(?:\.\d*)?(?:[eE][-+]?\d+)?|\.\d+(?:[eE][-+]?\d+)?)[uUlLfFzZ]*$"),
        text,
    ) {
        return Some(ArgumentType::Number);
    }
    let address =
        js::caps(&ctx.re(r"^&\s*([A-Za-z_]\w*)$"), text).map(|c| js::group(&c, 1).to_string());
    let name = address
        .clone()
        .or_else(|| js::is_ident(text).then(|| text.to_string()));
    if let Some(name) = name {
        let mut decl = ReceiverDeclaration::default();
        infer_cpp_receiver_type(ctx, &name, r, 0, Some(&mut decl));
        let declared = decl
            .declared
            .as_ref()
            .map(|d| d.text.clone())
            .or(decl.raw.clone())?;
        if !decl.callers || decl.shadowed {
            return None;
        }
        let written = if decl.constant && !js::test(&ctx.re(r"^\s*const\b"), &declared) {
            format!("const {declared}")
        } else {
            declared
        };
        let typed = if address.is_some() || decl.array.is_some_and(|a| a > 0) {
            format!("{}*", js::replace_with(&ctx.re(r"\s*&+\s*$"), &written, ""))
        } else {
            written
        };
        let dependent = is_cpp_dependent_argument(ctx, &typed, r);
        return Some(ArgumentType::Type {
            text: typed,
            lvalue: Some(address.is_none()),
            dependent,
        });
    }
    if js::test(&ctx.re(r#"^(?:u8|u|U|L)?R?["']"#), text) {
        return None;
    }
    let typed = cpp_expression_type(ctx, text, r, 0)?;
    Some(ArgumentType::Type {
        lvalue: cpp_names_object(ctx, text, &typed.text),
        dependent: is_cpp_dependent_argument(ctx, &typed.text, r),
        text: typed.text,
    })
}

fn is_cpp_dependent_argument(ctx: &Ctx, ty: &str, r: &Ref) -> bool {
    normalize_cpp_type_name(ctx, ty)
        .is_some_and(|name| aliases::template_parameters(ctx, r.from).contains(&name))
}

/// Whether the argument `expr` of the written type `ty` names an object.
fn cpp_names_object(ctx: &Ctx, expr: &str, ty: &str) -> Option<bool> {
    let mut text = expr.trim().to_string();
    while text.starts_with('(')
        && text.ends_with(')')
        && cpp_opening_paren(&text, text.len() - 1) == Some(0)
    {
        text = text[1..text.len() - 1].trim().to_string();
    }
    if text.starts_with('*') || text.ends_with(']') {
        return Some(true);
    }
    if !text.ends_with(')') {
        return js::test(&ctx.re(r"^[A-Za-z_][\w\s.:>-]*$"), &text).then_some(true);
    }
    if js::test(&ctx.re(r"&&\s*$"), ty) {
        return None;
    }
    Some(js::test(&ctx.re(r"&\s*$"), ty))
}

/// What a C++ type is, for an argument to fit a parameter of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    String,
    CString,
    Char,
    Number,
    Bool,
    Pointer,
    Null,
    Class,
}

const CPP_CHAR_TYPES: &[&str] = &["char", "wchar_t", "char8_t", "char16_t", "char32_t"];
const CPP_NUMBER_TYPES: &[&str] = &[
    "int",
    "unsigned",
    "signed",
    "short",
    "long",
    "float",
    "double",
    "size_t",
    "ssize_t",
    "ptrdiff_t",
    "int8_t",
    "int16_t",
    "int32_t",
    "int64_t",
    "uint8_t",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "intptr_t",
    "uintptr_t",
    "size_type",
    "difference_type",
];
const CPP_STRING_TYPES: &[&str] = &[
    "string",
    "wstring",
    "string_view",
    "wstring_view",
    "basic_string",
    "basic_string_view",
];

/// The category of the written C++ type `ty`.
pub fn cpp_type_category(ctx: &Ctx, ty: &str) -> Category {
    let flat = js::replace_with(
        &ctx.re(r"\b(?:const|volatile|struct|class|typename|enum)\b"),
        &aliases::strip_template_arguments(ty),
        " ",
    );
    let flat = js::replace_with(&ctx.re(r"&+"), &flat, " ")
        .trim()
        .to_string();
    let starless = flat.replace('*', " ");
    let words: Vec<&str> = starless.split_whitespace().collect();
    let last = js::last_segment(words.last().copied().unwrap_or(""));
    if flat.contains('*') {
        return if CPP_CHAR_TYPES.contains(&last) && flat.matches('*').count() == 1 {
            Category::CString
        } else {
            Category::Pointer
        };
    }
    if last == "nullptr_t" {
        return Category::Null;
    }
    if last == "bool" {
        return Category::Bool;
    }
    if CPP_CHAR_TYPES.contains(&last)
        && words
            .iter()
            .all(|w| CPP_CHAR_TYPES.contains(w) || *w == "signed" || *w == "unsigned")
    {
        return Category::Char;
    }
    if CPP_NUMBER_TYPES.contains(&last)
        || (!words.is_empty() && words.iter().all(|w| CPP_NUMBER_TYPES.contains(w)))
    {
        return Category::Number;
    }
    if CPP_STRING_TYPES.contains(&last) {
        Category::String
    } else {
        Category::Class
    }
}

/// How well the argument types `types` fit the parameters of the overload `n`.
pub fn cpp_argument_type_fit(
    ctx: &Ctx,
    n: SymId,
    name: &str,
    types: &[Option<ArgumentType>],
) -> i64 {
    let Some(list) = cpp_overload_parameters(ctx, n, name) else {
        return 0;
    };
    let params: Vec<String> = split_cpp_top_level(ctx, &cpp_without_comments(&list))
        .into_iter()
        .filter(|p| p != "void")
        .collect();
    let own = if types
        .iter()
        .any(|t| matches!(t, Some(ArgumentType::Type { .. })))
    {
        cpp_own_template_parameters(ctx, n)
    } else {
        Vec::new()
    };
    let mut score = 0;
    for (i, arg) in types.iter().enumerate().take(params.len()) {
        let Some(arg) = arg else {
            continue;
        };
        let param = cpp_parameter_type(ctx, &params[i]);
        if param.contains("...") {
            continue;
        }
        let category = cpp_type_category(ctx, &param);
        let deduced = !own.is_empty()
            && own.contains(&normalize_cpp_type_name(ctx, &param).unwrap_or_default());
        score += match arg {
            ArgumentType::Type {
                text,
                lvalue,
                dependent,
            } => cpp_written_type_fit(ctx, text, &param, category, *lvalue, *dependent, deduced),
            lit => literal_fit(lit, category),
        };
    }
    score
}

fn literal_fit(lit: &ArgumentType, c: Category) -> i64 {
    use Category::*;
    match lit {
        ArgumentType::Char => match c {
            Char => 3,
            Number => 1,
            Bool | Class => 0,
            _ => -3,
        },
        ArgumentType::Number => match c {
            Number => 2,
            Char => 1,
            Bool | Class => 0,
            _ => -3,
        },
        ArgumentType::Zero => match c {
            Number => 2,
            Char | Pointer | CString | Null => 1,
            Bool | Class => 0,
            String => -3,
        },
        ArgumentType::Bool => match c {
            Bool => 3,
            Number => 1,
            Char | Class => 0,
            _ => -3,
        },
        ArgumentType::Null => match c {
            Null => 4,
            Pointer | CString => 3,
            Class => 1,
            Bool => -1,
            _ => -3,
        },
        ArgumentType::Type { .. } => 0,
    }
}

fn cpp_written_type_fit(
    ctx: &Ctx,
    arg: &str,
    param: &str,
    category: Category,
    lvalue: Option<bool>,
    dependent: bool,
    deduced: bool,
) -> i64 {
    let fit = cpp_unqualified_type_fit(ctx, arg, param, category);
    if fit < 0 || dependent {
        return fit;
    }
    let arg_category = cpp_type_category(ctx, arg);
    if category == Category::Pointer && arg_category == Category::Class {
        return -3;
    }
    if !deduced
        && category == Category::Pointer
        && arg_category == Category::CString
        && !js::test(&ctx.re(r"\bvoid\b"), param)
    {
        return -3;
    }
    if category == Category::CString && arg_category == Category::Pointer {
        return -3;
    }
    if cpp_refers_to_const(ctx, param) != Some(false) {
        return fit;
    }
    if !deduced && cpp_argument_const(ctx, arg, param) == Some(true) {
        return -3;
    }
    if lvalue == Some(false) && !aliases::strip_template_arguments(param).contains('*') {
        -3
    } else {
        fit
    }
}

/// Whether the object an argument of type `arg` gives a pointer or reference parameter `param` is const.
pub fn cpp_argument_const(ctx: &Ctx, arg: &str, param: &str) -> Option<bool> {
    let arg_pointer = aliases::strip_template_arguments(arg).contains('*');
    if aliases::strip_template_arguments(param).contains('*') {
        return if arg_pointer {
            cpp_refers_to_const(ctx, arg)
        } else {
            None
        };
    }
    if arg_pointer {
        None
    } else {
        Some(js::test(
            &ctx.re(r"\bconst\b"),
            &aliases::strip_template_arguments(arg),
        ))
    }
}

/// Whether the C++ type `ty`, a pointer or an lvalue reference of one level, refers to a const object.
pub fn cpp_refers_to_const(ctx: &Ctx, ty: &str) -> Option<bool> {
    let text = aliases::strip_template_arguments(ty);
    let text = text.trim();
    let stars = text.matches('*').count();
    if stars > 1 || (stars == 0 && !js::test(&ctx.re(r"[^&]&\s*$"), text)) {
        return None;
    }
    let scope = if stars == 1 {
        &text[..text.find('*').unwrap()]
    } else {
        text
    };
    Some(js::test(&ctx.re(r"\bconst\b"), scope))
}

fn cpp_unqualified_type_fit(ctx: &Ctx, arg: &str, param: &str, category: Category) -> i64 {
    use Category::*;
    let arg_category = cpp_type_category(ctx, arg);
    let pointer = |c: Category| c == Pointer || c == CString;
    if normalize_cpp_type_name(ctx, arg) == normalize_cpp_type_name(ctx, param)
        && pointer(arg_category) == pointer(category)
    {
        return 3;
    }
    if arg_category == category && arg_category != Class {
        return 2;
    }
    let textual = |c: Category| c == String || c == CString;
    if matches!(arg_category, Char | Number | Pointer) && category == String {
        return -3;
    }
    if (textual(arg_category) || arg_category == Pointer) && matches!(category, Char | Number) {
        return -3;
    }
    if matches!(arg_category, Char | Number) && matches!(category, Pointer | CString) {
        return -3;
    }
    0
}

/// A C++ parameter declaration's type: without its default argument and its name.
pub fn cpp_parameter_type(ctx: &Ctx, param: &str) -> String {
    let b = param.as_bytes();
    let mut text = param;
    let mut depth = 0i32;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'<' | b'(' | b'[' => depth += 1,
            b'>' | b')' | b']' => depth -= 1,
            b'=' if depth == 0 => {
                text = &param[..i];
                break;
            }
            _ => {}
        }
    }
    let text = text.trim();
    if let Some(named) = js::caps(
        &ctx.re(r"^(.*[\w>*&\s])\b[A-Za-z_]\w*\s*(?:\[[^\]]*\])?$"),
        text,
    ) {
        let head = js::group(&named, 1);
        if js::test(&ctx.re(r"[A-Za-z_]"), head)
            && !js::test(
                &ctx.re(r"^(?:const|volatile|unsigned|signed|struct|class|enum)\s*$"),
                head.trim(),
            )
        {
            return head.trim().to_string();
        }
    }
    text.to_string()
}

/// The words in the arguments of the macros a C++ class declaration calls among its members.
pub fn cpp_member_macro_arguments(ctx: &Ctx, decl: SymId) -> Rc<Vec<String>> {
    ctx.memo(memo::NM, format!("mma\0{decl}"), || {
        let d = ctx.sym(decl);
        let mut words = Vec::new();
        if let Some(src) = ctx.source(&d.file) {
            let non_code = raw_strings::non_code_lines(ctx, &d.file);
            let call = ctx.re(r"^\s*[A-Z][A-Z0-9_]*\s*\(([^;{}]*)\)\s*;?\s*$");
            let split = ctx.re(r"\W+");
            let mut depth = 0i32;
            let first = (d.start_line as usize).saturating_sub(1);
            for i in first..src.lines.len().min(d.end_line as usize) {
                let blanked = raw_strings::literals_blanked(&src.lines[i], i + 1, &non_code);
                let code_src = if i == first {
                    js::slice_from(&blanked, d.start_column as usize).to_string()
                } else {
                    blanked
                };
                let code = cpp_code_of(ctx, &code_src).unwrap_or_default();
                if depth == 1 {
                    if let Some(c) = js::caps(&call, &code) {
                        for w in split.split(js::group(&c, 1)).filter_map(|w| w.ok()) {
                            if w.len() >= 3 {
                                words.push(w.to_string());
                            }
                        }
                    }
                }
                for c in code.bytes() {
                    if c == b'{' {
                        depth += 1;
                    } else if c == b'}' {
                        depth -= 1;
                    }
                }
            }
        }
        Rc::new(words)
    })
}

/// The base specifiers a C or C++ class declaration writes, read from its own head.
pub fn cpp_base_specifiers(ctx: &Ctx, decl: SymId) -> Vec<String> {
    let v: Rc<Vec<String>> = ctx.memo(memo::NM, format!("bs\0{decl}"), || {
        let d = ctx.sym(decl);
        let mut specs = Vec::new();
        if let Some(src) = ctx.source(&d.file) {
            let non_code = raw_strings::non_code_lines(ctx, &d.file);
            let from = (d.start_line as usize)
                .saturating_sub(1)
                .min(src.lines.len());
            let to = (d.start_line as usize + 11).min(src.lines.len());
            let mut head: Vec<String> = src.lines[from..to]
                .iter()
                .enumerate()
                .map(|(k, l)| {
                    raw_strings::literals_blanked(l, d.start_line as usize + k, &non_code)
                })
                .collect();
            if !head.is_empty() {
                head[0] = js::slice_from(&head[0], d.start_column as usize).to_string();
                let text = head
                    .iter()
                    .map(|l| cpp_code_of(ctx, l).unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Some(brace) = text.find('{') {
                    let colon = js::search(&ctx.re(r"(?<!:):(?!:)"), &text[..brace]);
                    if let Some(colon) = colon {
                        if !text[..brace].contains(';') {
                            let access = ctx.re(r"\b(?:public|protected|private|virtual)\b");
                            for s in split_cpp_top_level(ctx, &text[colon + 1..brace]) {
                                specs.push(js::replace_with(&access, &s, " ").trim().to_string());
                            }
                        }
                    }
                }
            }
        }
        Rc::new(specs)
    });
    (*v).clone()
}

/// Whether the class declaration `decl` brings `name` in from its base `base` with a using-declaration.
pub fn cpp_using_declares(ctx: &Ctx, decl: SymId, base: SymId, name: &str) -> bool {
    let usings: Rc<Vec<(String, String)>> = ctx.memo(memo::NM, format!("ud\0{decl}"), || {
        let d = ctx.sym(decl);
        let mut usings = Vec::new();
        if let Some(src) = ctx.source(&d.file) {
            let non_code = raw_strings::non_code_lines(ctx, &d.file);
            let from = (d.start_line as usize).saturating_sub(1).min(src.lines.len());
            let to = (d.end_line.max(d.start_line) as usize).min(src.lines.len());
            let body = src.lines[from..to.max(from)]
                .iter()
                .enumerate()
                .map(|(k, l)| cpp_code_of(ctx, &raw_strings::literals_blanked(l, d.start_line as usize + k, &non_code)).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n");
            let re = ctx.re(r"\busing\s+(?!namespace\b)((?:::\s*)?[A-Za-z_][\w:]*(?:\s*<[^;{}]*>)?)\s*::\s*([A-Za-z_]\w*)\s*;");
            for m in js::all(&re, &body) {
                let of = aliases::strip_template_arguments(js::group(&m, 1));
                usings.push((js::last_segment(of.trim()).to_string(), js::group(&m, 2).to_string()));
            }
        }
        Rc::new(usings)
    });
    let bname = &ctx.sym(base).name;
    usings
        .iter()
        .any(|(of, member)| of == bname && member == name)
}

/// Whether `base`, a base of the class declaration `decl`, depends on template parameters.
pub fn is_cpp_dependent_base(ctx: &Ctx, decl: SymId, base: SymId) -> bool {
    let parameters = aliases::template_parameters(ctx, decl);
    if parameters.is_empty() {
        return false;
    }
    let bname = &ctx.sym(base).name;
    let spec = cpp_base_specifiers(ctx, decl)
        .into_iter()
        .find(|s| js::last_segment(aliases::strip_template_arguments(s).trim()) == bname);
    let Some(spec) = spec else {
        return false;
    };
    parameters.iter().any(|p| {
        js::test(
            &ctx.re(&format!(r"(?<![\w$]){}(?![\w$])", js::escape(p))),
            &spec,
        )
    })
}

// --- paren lists, argument lists and overloads (name-matcher.ts 29241–29447) ---

/// Split `a, f(b, c), d<e, f>` at its top-level commas.
pub fn split_cpp_top_level(ctx: &Ctx, text: &str) -> Vec<String> {
    let _ = ctx;
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut closes: Vec<usize> = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < b.len() {
        let ch = b[i];
        if ch == b'"' || ch == b'\'' {
            let mut j = i + 1;
            while j < b.len() && b[j] != ch {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            cur.push_str(js::slice(text, i, j + 1));
            i = j + 1;
            continue;
        }
        if matches!(ch, b'(' | b'[' | b'{') {
            depth += 1;
        } else if matches!(ch, b')' | b']' | b'}') {
            depth = (depth - 1).max(0);
        } else if ch == b'<' {
            if let Some(end) = cpp_template_arguments_end(text, i) {
                if end > i {
                    closes.push(end);
                }
            }
        } else if ch == b'>' && closes.last() == Some(&i) {
            closes.pop();
        }
        if ch == b',' && depth == 0 && closes.is_empty() {
            out.push(cur.trim().to_string());
            cur.clear();
            i += 1;
            continue;
        }
        let c = text[i..].chars().next().unwrap();
        cur.push(c);
        i += c.len_utf8();
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// Where the template argument list the `<` at `at` opens closes, or None when it compares or shifts.
pub fn cpp_template_arguments_end(text: &str, at: usize) -> Option<usize> {
    let b = text.as_bytes();
    if !js::is_word(js::before(text, at)) || matches!(js::at(text, at + 1), b'<' | b'=') {
        return None;
    }
    let mut angles = 0i32;
    let mut others = 0i32;
    let mut i = at;
    while i < b.len() {
        let ch = b[i];
        if ch == b'"' || ch == b'\'' {
            let mut j = i + 1;
            while j < b.len() && b[j] != ch {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            i = j + 1;
            continue;
        }
        if matches!(ch, b'(' | b'[' | b'{') {
            others += 1;
        } else if matches!(ch, b')' | b']' | b'}') {
            others -= 1;
            if others < 0 {
                return None;
            }
        } else if others > 0 {
        } else if ch == b'<' {
            angles += 1;
        } else if ch == b'>' && js::before(text, i) != b'-' {
            angles -= 1;
            if angles == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// The balanced `( … )` after `name` from `line`/`column` of `file` (up to a dozen lines).
pub fn cpp_paren_list_after(
    ctx: &Ctx,
    file: &str,
    line: u32,
    column: usize,
    name: &str,
) -> Option<String> {
    let src = ctx.source(file)?;
    let from = (line as usize).saturating_sub(1).min(src.lines.len());
    let to = (line as usize + 11).min(src.lines.len());
    cpp_paren_list_in(ctx, &src.lines[from..to].join("\n"), column, name)
}

/// The `( … )` that follows `name` in `text`, from `column` on, without its parentheses.
pub fn cpp_paren_list_in(ctx: &Ctx, text: &str, column: usize, name: &str) -> Option<String> {
    let re = ctx.re(&format!(r"\b{}\s*(?:<[^<>()]*>)?\s*\(", js::escape(name)));
    let column = js::floor(text, column);
    let m = re.find(&text[column..]).ok().flatten()?;
    let open = column + m.end() - 1;
    let b = text.as_bytes();
    let mut depth = 0i32;
    for i in open..b.len() {
        if b[i] == b'(' {
            depth += 1;
        } else if b[i] == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(text[open + 1..i].to_string());
            }
        }
    }
    None
}

/// C++ text with each comment blanked to a space; literals kept.
pub fn cpp_without_comments(text: &str) -> String {
    if !text.contains('/') {
        return text.to_string();
    }
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'"' || c == b'\'' {
            let mut j = i + 1;
            while j < b.len() && b[j] != c && b[j] != b'\n' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            out.push_str(js::slice(text, i, j + 1));
            i = j + 1;
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            let end = js::index_of(text, "\n", i);
            out.push(' ');
            i = end.unwrap_or(b.len());
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let end = js::index_of(text, "*/", i + 2);
            out.push(' ');
            i = end.map_or(b.len(), |e| e + 2);
        } else {
            let ch = text[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

pub fn cpp_call_arguments(ctx: &Ctx, r: &Ref, name: &str) -> Option<Vec<String>> {
    let list = cpp_paren_list_after(ctx, &r.file, r.line, r.column as usize, name)?;
    Some(split_cpp_top_level(ctx, &cpp_without_comments(&list)))
}

/// The parameter list of the C++ overload `n`, as it gives the parameters' defaults.
pub fn cpp_overload_parameters(ctx: &Ctx, n: SymId, name: &str) -> Option<String> {
    let s = ctx.sym(n);
    let own = match cpp_macro_method_head(ctx, n) {
        Some(head) => cpp_paren_list_in(ctx, &head, 0, name),
        None => cpp_paren_list_after(ctx, &s.file, s.start_line, 0, name),
    }?;
    if s.kind != NodeKind::Method {
        return Some(own);
    }
    let key = format!("op\0{n}");
    if let Some(hit) = ctx.memo_get::<Option<String>>(memo::NM, &key) {
        return hit;
    }
    let mut found = own.clone();
    let classes = cpp_class_decls(ctx, aliases::parent_scope(&s.qualified_name));
    if !classes
        .iter()
        .any(|&c| ctx.sym(c).file == s.file && range_within(ctx, n, c))
    {
        let types = |list: &str| -> Vec<String> {
            split_cpp_top_level(ctx, &cpp_without_comments(list))
                .into_iter()
                .filter(|p| p != "void")
                .map(|p| {
                    let t = cpp_parameter_type(ctx, &p);
                    normalize_cpp_type_name(ctx, &t).unwrap_or(p)
                })
                .collect()
        };
        let want = types(&own);
        let word = ctx.re(&format!(r"\b{}\s*\(", js::escape(name)));
        'search: for &cls in &classes {
            let c = ctx.sym(cls);
            let Some(src) = ctx.source(&c.file) else {
                continue;
            };
            let last = (c.end_line.max(c.start_line) as usize).min(src.lines.len());
            for line in c.start_line as usize..=last {
                if line == 0 || !js::test(&word, &src.lines[line - 1]) {
                    continue;
                }
                let Some(list) = cpp_paren_list_after(ctx, &c.file, line as u32, 0, name) else {
                    continue;
                };
                if !list.contains('=') {
                    continue;
                }
                if types(&list) == want {
                    found = list;
                    break 'search;
                }
            }
        }
    }
    ctx.memo_set(memo::NM, key, Some(found.clone()));
    Some(found)
}

/// A C++ parameter that is a pack: `T&&... args`.
pub fn is_cpp_parameter_pack(p: &str) -> bool {
    let re = fancy_regex::Regex::new(r"<[^<>]*>").unwrap();
    let mut flat = p.to_string();
    loop {
        let next = js::replace_with(&re, &flat, "");
        if next == flat {
            break;
        }
        flat = next;
    }
    flat.contains("...")
}

/// Whether as many arguments as `args` can fill the parameters of `n`.
pub fn cpp_argument_count_fits(ctx: &Ctx, n: SymId, name: &str, args: &[String]) -> Option<bool> {
    if !matches!(ctx.kind(n), NodeKind::Function | NodeKind::Method) {
        return None;
    }
    let list = cpp_overload_parameters(ctx, n, name)?;
    let params: Vec<String> = split_cpp_top_level(ctx, &cpp_without_comments(&list))
        .into_iter()
        .filter(|p| p != "void")
        .collect();
    let required = params
        .iter()
        .filter(|p| !is_cpp_parameter_pack(p) && !p.contains('='))
        .count();
    Some(
        args.len() >= required
            && (params.iter().any(|p| is_cpp_parameter_pack(p)) || args.len() <= params.len()),
    )
}

/// How well a call's arguments fit an overload's parameters: arity, and string literals.
pub fn cpp_overload_fit(ctx: &Ctx, n: SymId, name: &str, args: &[String]) -> i64 {
    if !matches!(ctx.kind(n), NodeKind::Function | NodeKind::Method) {
        return -1;
    }
    let Some(list) = cpp_overload_parameters(ctx, n, name) else {
        return 0;
    };
    let params: Vec<String> = split_cpp_top_level(ctx, &cpp_without_comments(&list))
        .into_iter()
        .filter(|p| p != "void")
        .collect();
    let variadic = params.iter().any(|p| is_cpp_parameter_pack(p));
    let required = params
        .iter()
        .filter(|p| !is_cpp_parameter_pack(p) && !p.contains('='))
        .count();
    let mut score = 0;
    if args.len() < required || (!variadic && args.len() > params.len()) {
        score -= 3;
    }
    let literal = ctx.re(r#"^(?:u8|u|U|L)?R?"|^FMT_STRING\s*\("#);
    let wide_arg_re = ctx.re(r#"^LR?""#);
    let wide_param_re = ctx.re(r"\bw(?:string|char_t|format|string_view)|wchar_t");
    let stringish = ctx.re(r"string|char|Char|format");
    let stringish2 = ctx.re(r"string|char|Char|\bstr\b");
    let short_template = ctx.re(r"^(?:const\s+)?[A-Z]\w{0,2}\s*[&*]{0,2}\s*\w*$");
    for i in 0..args.len().min(params.len()) {
        let arg = &args[i];
        let param = &params[i];
        if is_cpp_parameter_pack(param) {
            break;
        }
        if !js::test(&literal, arg) {
            continue;
        }
        let wide_arg = js::test(&wide_arg_re, arg);
        let wide_param = js::test(&wide_param_re, param);
        let category = cpp_type_category(ctx, &cpp_parameter_type(ctx, param));
        if wide_arg != wide_param && js::test(&stringish, param) {
            score -= 2;
        } else if matches!(
            category,
            Category::Pointer | Category::Number | Category::Bool | Category::Char
        ) {
            score -= 3;
        } else {
            score += if js::test(&stringish2, param) {
                3
            } else if js::test(&short_template, param) {
                1
            } else {
                -2
            };
        }
    }
    score
}

/// A method's same-owner overloads and every one's arity (overloadSetOf).
type Arity = Option<(usize, usize)>;

struct OverloadSet {
    name: String,
    own: Arity,
    siblings: Vec<(SymId, Arity)>,
}

fn overload_set_of(ctx: &Ctx, id: SymId) -> Option<OverloadSet> {
    let s = ctx.sym(id);
    if !matches!(s.kind, NodeKind::Method | NodeKind::Function) {
        return None;
    }
    let name = s.name.clone();
    let owner = aliases::parent_scope(&s.qualified_name).to_string();
    let siblings: Vec<SymId> = ctx
        .in_file(&s.file)
        .iter()
        .copied()
        .filter(|&n| {
            let x = ctx.sym(n);
            x.name == name
                && n != id
                && matches!(x.kind, NodeKind::Method | NodeKind::Function)
                && aliases::parent_scope(&x.qualified_name) == owner
        })
        .collect();
    if siblings.is_empty() {
        return None;
    }
    let pack = ctx.re(r"\.\.\.|\bparams\s|\bvararg\s");
    let arity = |n: SymId| -> Arity {
        let x = ctx.sym(n);
        let list = cpp_paren_list_after(ctx, &x.file, x.start_line, 0, &name)?;
        let params: Vec<String> = split_cpp_top_level(ctx, &list)
            .into_iter()
            .filter(|p| !p.is_empty() && p != "void")
            .collect();
        let is_pack = |p: &str| js::test(&pack, &js::replace_with(&ctx.re(r"<[^<>]*>"), p, ""));
        let min = params
            .iter()
            .filter(|p| !p.contains('=') && !is_pack(p))
            .count();
        let max = if params.iter().any(|p| is_pack(p)) {
            usize::MAX
        } else {
            params.len()
        };
        Some((min, max))
    };
    Some(OverloadSet {
        own: arity(id),
        siblings: siblings.iter().map(|&n| (n, arity(n))).collect(),
        name,
    })
}

/// A call a method makes to its own name, with arguments its parameters
/// cannot take, is to another overload of it (retargetSelfOverload, for C++).
pub fn retarget_self_overload(ctx: &Ctx, m: Match, r: &Ref) -> Match {
    if r.kind != ReferenceKind::Calls || r.lang != Language::Cpp || m.target != r.from {
        return m;
    }
    let Some(set) = overload_set_of(ctx, m.target) else {
        return m;
    };
    let Some((min, max)) = set.own else {
        return m;
    };
    let Some(args) = cpp_paren_list_after(ctx, &r.file, r.line, r.column as usize, &set.name)
    else {
        return m;
    };
    let argc = if args.trim().is_empty() {
        0
    } else {
        split_cpp_top_level(ctx, &args).len()
    };
    if argc >= min && argc <= max {
        return m;
    }
    let fits: Vec<SymId> = set
        .siblings
        .iter()
        .filter(|(_, a)| a.is_some_and(|(lo, hi)| argc >= lo && argc <= hi))
        .map(|(n, _)| *n)
        .collect();
    if fits.len() == 1 {
        Match {
            target: fits[0],
            ..m
        }
    } else {
        m
    }
}
