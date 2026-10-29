//! A C++ function's own names: its parameters and locals, which a bare call
//! may call before any function or member, and the names a file makes types
//! or namespaces of itself.

use super::*;
use std::cell::RefCell;

/// `cppCodeOf`: a C++ source line as code, for reading declarations — None
/// for a `// …` line, else the line with its comments blanked, columns kept.
pub fn cpp_code_of(ctx: &Ctx, line: &str) -> Option<String> {
    if js::test(&ctx.re(r"^\s*//"), line) {
        return None;
    }
    let mut code = js::replace_all(&ctx.re(r"/\*.*?\*/"), line, |c| {
        " ".repeat(js::group(c, 0).len())
    });
    if let Some(open) = code.find("/*") {
        code.truncate(open);
    }
    if let Some(comment) = code.find("//") {
        code.truncate(comment);
    }
    Some(code)
}

/// Does `line` declare the receiver in a form the declarator pattern doesn't
/// read: a range-`for` variable or a structured binding?
pub fn cpp_rebinds_receiver(ctx: &Ctx, line: &str, escaped_receiver: &str) -> bool {
    js::test(
        &ctx.re(&format!(r"\bfor\s*\(.*\b{escaped_receiver}\s*:(?!:)")),
        line,
    ) || js::test(
        &ctx.re(&format!(
            r"\bauto\s*&{{0,2}}\s*\[[^\]]*\b{escaped_receiver}\b[^\]]*\]"
        )),
        line,
    )
}

/// Whether the C++ function `caller` declares `name` before the call at `r`.
pub fn is_cpp_local_name(ctx: &Ctx, name: &str, caller: SymId, r: &Ref) -> bool {
    let c = ctx.sym(caller);
    if c.file != r.file || CPP_CLASS_KINDS.contains(&c.kind) {
        return false;
    }
    let Some(src) = ctx.source(&r.file) else {
        return false;
    };
    let lines = &src.lines;
    let escaped = js::escape(name);
    let word = ctx.re(&format!(r"\b{escaped}\b"));
    let declarator = build_declarator_regex(ctx, &escaped);
    let listed = build_listed_declarator_regex(ctx, &escaped);
    let non_code = raw_strings::non_code_lines(ctx, &c.file);
    let from = (c.start_line as usize).saturating_sub(1);
    let to = lines.len().min(r.line as usize);
    for (i, raw) in lines.iter().enumerate().take(to).skip(from) {
        if !js::test(&word, raw) {
            continue;
        }
        let Some(code) = cpp_code_of(ctx, &raw_strings::literals_blanked(raw, i + 1, &non_code))
        else {
            continue;
        };
        if !js::test(&word, &code) {
            continue;
        }
        if cpp_rebinds_receiver(ctx, &code, &escaped) {
            return true;
        }
        for variant in [code.clone(), aliases::strip_template_arguments(&code)] {
            if let Some(m) = js::caps(&declarator, &variant) {
                let ty = js::group(&m, 1);
                if !js::test(&ctx.re(r"::\s*$"), ty) && normalize_cpp_type_name(ctx, ty).is_some() {
                    return true;
                }
            }
        }
        if let Some(m) = match_cpp_listed_declarator(ctx, &code, name, &listed) {
            if normalize_cpp_type_name(ctx, &m.ty).is_some() {
                return true;
            }
        }
    }
    false
}

const CPP_LOCAL_SPECIFIERS: &str =
    r"(?:(?:const|constexpr|static|volatile|thread_local|register)\s+)*";
const CPP_LOCAL_TEMPLATE_ARGUMENTS: &str = r"<(?:[^<>;={}]|<(?:[^<>;={}]|<[^<>;={}]*>)*>)*>";
const CPP_STATEMENT_START: &str = r"(?:^|[;{}]|\b(?:for|if)\s*\()\s*";

static CPP_LOCAL_TYPE: LazyLock<String> = LazyLock::new(|| {
    format!(
        r"(?:::\s*)?[A-Za-z_]\w*(?:\s*::\s*[A-Za-z_]\w*)*(?:\s*{CPP_LOCAL_TEMPLATE_ARGUMENTS}(?:\s*::\s*[A-Za-z_]\w*)*)?"
    )
});
static CPP_AUTO_LOCAL: Pattern = Pattern(LazyLock::new(|| {
    format!(
        r"{CPP_STATEMENT_START}{CPP_LOCAL_SPECIFIERS}auto(?:\s+|\s*[*&]+\s*)([A-Za-z_]\w*)\s*(?:=|\{{|\()"
    )
}));
static CPP_TYPED_LOCAL: Pattern = Pattern(LazyLock::new(|| {
    format!(
        r"{CPP_STATEMENT_START}{CPP_LOCAL_SPECIFIERS}({})(\s+|\s*[*&]+\s*)(?:const\s+)?([A-Za-z_]\w*)\s*([=({{;\[,])",
        *CPP_LOCAL_TYPE
    )
}));
static CPP_LOCAL_TYPE_DECLARATION: Pattern = Pattern(LazyLock::new(|| {
    format!(
        r"{CPP_STATEMENT_START}(?:using\s+([A-Za-z_]\w*)\s*=|typedef\b[^;]*?\b([A-Za-z_]\w*)\s*;|(?:struct|class|union|enum)(?:\s+class)?\s+([A-Za-z_]\w*)\b)"
    )
}));
const CPP_LAMBDA_HEAD: &str = r"\]\s*\(([^()]*)\)\s*(?:mutable\s*)?(?:->[^{;]*)?\{";

/// Does `text` hold `name` as a whole word?
pub fn cpp_has_word(text: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(at) = js::index_of(text, name, from) {
        if !js::is_word(js::before(text, at)) && !js::is_word(js::at(text, at + name.len())) {
            return true;
        }
        from = at + 1;
    }
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BindingKind {
    Parameter,
    Variable,
    Type,
    Function,
}

/// A declaration of a name in a C++ function's own code, at a 0-based line.
#[derive(Clone, Debug)]
struct Binding {
    line: usize,
    kind: BindingKind,
    column: usize,
    loop_paren: Option<usize>,
}

/// A C++ function's head: its parameter names and the 0-based line its name is on.
#[derive(Clone, Debug)]
struct CallerHead {
    parameters: HashSet<String>,
    body: usize,
}

/// A file's lines as code (cppCodeOf of each, literals blanked), read lazily.
struct CodeLines<'c, 'a> {
    ctx: &'c Ctx<'a>,
    raw: Rc<super::super::ctx::Source>,
    non_code: NonCode,
    cache: RefCell<HashMap<usize, Rc<str>>>,
}

impl<'c, 'a> CodeLines<'c, 'a> {
    fn new(ctx: &'c Ctx<'a>, file: &str) -> Option<Self> {
        Some(CodeLines {
            ctx,
            raw: ctx.source(file)?,
            non_code: raw_strings::non_code_lines(ctx, file),
            cache: RefCell::default(),
        })
    }

    fn get(&self, l: usize) -> Rc<str> {
        if let Some(c) = self.cache.borrow().get(&l) {
            return c.clone();
        }
        let raw = self.raw.lines.get(l).map(String::as_str).unwrap_or("");
        let code: Rc<str> = cpp_code_of(
            self.ctx,
            &raw_strings::literals_blanked(raw, l + 1, &self.non_code),
        )
        .unwrap_or_default()
        .into();
        self.cache.borrow_mut().insert(l, code.clone());
        code
    }
}

/// The names a C++ parameter list declares.
fn cpp_parameter_names(ctx: &Ctx, list: &str) -> Vec<String> {
    let mut names = Vec::new();
    for parameter in split_cpp_top_level(ctx, list) {
        let declarator = js::replace_with(&ctx.re(r"(?s)=.*$"), &parameter, "");
        let declarator = declarator.trim();
        let Some(m) = js::caps(&ctx.re(r"([A-Za-z_]\w*)\s*(?:\[[^\]]*\])*$"), declarator) else {
            continue;
        };
        let start = m.get(0).unwrap().start();
        let before = js::replace_with(
            &ctx.re(r"\b(?:const|volatile|struct|class|enum|union|typename|signed|unsigned)\b"),
            &declarator[..start],
            "",
        );
        let before = before.trim();
        if !before.is_empty() && !js::test(&ctx.re(r"::\s*$"), before) {
            names.push(js::group(&m, 1).to_string());
        }
    }
    names
}

/// The text inside the balanced `(…)` opening at (`line`, `column`), within a dozen lines.
fn cpp_paren_text_from(code: &CodeLines, line: usize, column: usize) -> Option<String> {
    let mut depth = 0i32;
    let mut text = String::new();
    for l in line..line + 12 {
        let c = code.get(l);
        let from = if l == line {
            js::floor(&c, column.min(c.len()))
        } else {
            0
        };
        for ch in c[from..].chars() {
            if ch == '(' {
                depth += 1;
            } else if ch == ')' {
                depth -= 1;
                if depth == 0 {
                    return Some(text.get(1..).unwrap_or("").to_string());
                }
            }
            text.push(ch);
        }
        text.push(' ');
    }
    None
}

/// Does a `T name(args)` local's argument list declare parameters, so it declares a function?
fn cpp_declares_local_function(ctx: &Ctx, ty: &str, args: &str) -> bool {
    if ty == "void" || args.trim().is_empty() {
        return true;
    }
    let builtin =
        ctx.re(r"^(?:int|char|bool|float|double|long|short|unsigned|signed|void|size_t)$");
    let typed = ctx.re(r"^(?:(?:const|volatile|unsigned|signed)\s+)*[A-Za-z_][\w:]*(?:\s*<[^>]*>)?\s*[*&]*\s*(?:[A-Za-z_]\w*)?$");
    let spaced = ctx.re(r"\s|[*&]");
    split_cpp_top_level(ctx, args)
        .iter()
        .any(|a| js::test(&builtin, a) || (js::test(&typed, a) && js::test(&spaced, a)))
}

/// The C++ function `caller`'s parameter names and the line its head's name is on.
fn cpp_caller_head(
    ctx: &Ctx,
    caller: SymId,
    code: &CodeLines,
    line_count: usize,
) -> Rc<CallerHead> {
    let key = format!("ch\0{caller}");
    if let Some(hit) = ctx.memo_get::<Rc<CallerHead>>(memo::NM, &key) {
        return hit;
    }
    let c = ctx.sym(caller);
    let first = (c.start_line as usize).saturating_sub(1);
    let mut head = CallerHead {
        parameters: HashSet::new(),
        body: first,
    };
    let named = ctx.re(&format!(r"\b{}\s*\(", js::escape(&c.name)));
    for l in first..line_count.min(first + 13) {
        let text = code.get(l);
        let Some(at) = js::caps(&named, &text).map(|m| m.get(0).unwrap().end()) else {
            continue;
        };
        let list = cpp_paren_text_from(code, l, at - 1);
        head = CallerHead {
            parameters: list
                .map(|l| cpp_parameter_names(ctx, &l))
                .unwrap_or_default()
                .into_iter()
                .collect(),
            body: l,
        };
        break;
    }
    let head = Rc::new(head);
    ctx.memo_set(memo::NM, key, head.clone());
    head
}

/// The declarations of `name` in the C++ function `caller`'s own code.
fn cpp_local_bindings(
    ctx: &Ctx,
    name: &str,
    caller: SymId,
    head: &CallerHead,
    code: &CodeLines,
) -> Vec<Binding> {
    let c = ctx.sym(caller);
    let raw_lines = &code.raw.lines;
    let first = (c.start_line as usize).saturating_sub(1);
    let last = (raw_lines.len() as i64 - 1).min(c.end_line.max(c.start_line) as i64 - 1);
    let mut bindings = Vec::new();
    if head.parameters.contains(name) {
        bindings.push(Binding {
            line: head.body,
            column: 0,
            kind: BindingKind::Parameter,
            loop_paren: None,
        });
    }
    let ends = ctx.re(r"[;{}:]\s*$");
    let directive = ctx.re(r"^\s*#");
    let starts_statement = |l: usize| -> bool {
        let mut k = l;
        while k > first {
            k -= 1;
            let above = code.get(k);
            if !above.trim().is_empty() {
                return js::test(&ends, &above) || js::test(&directive, &above) || k < head.body;
            }
        }
        true
    };
    let excluded = ctx.re(r"^(?:using|typedef|struct|class|union|enum|auto)$");
    let mut l = head.body as i64;
    while l <= last {
        let li = l as usize;
        l += 1;
        if !cpp_has_word(raw_lines.get(li).map(String::as_str).unwrap_or(""), name) {
            continue;
        }
        let text = code.get(li);
        if !cpp_has_word(&text, name) {
            continue;
        }
        let starts = |index: usize| index > 0 || li == head.body || starts_statement(li);
        for m in js::all(&CPP_LOCAL_TYPE_DECLARATION.with(ctx), &text) {
            let declared = js::group_opt(&m, 1)
                .or(js::group_opt(&m, 2))
                .or(js::group_opt(&m, 3));
            let index = m.get(0).unwrap().start();
            if declared == Some(name) && starts(index) {
                bindings.push(Binding {
                    line: li,
                    column: index,
                    kind: BindingKind::Type,
                    loop_paren: None,
                });
            }
        }
        for m in js::all(&CPP_AUTO_LOCAL.with(ctx), &text) {
            let index = m.get(0).unwrap().start();
            if js::group(&m, 1) == name && starts(index) {
                bindings.push(Binding {
                    line: li,
                    column: index,
                    kind: BindingKind::Variable,
                    loop_paren: None,
                });
            }
        }
        for m in js::all(&CPP_TYPED_LOCAL.with(ctx), &text) {
            let whole = m.get(0).unwrap();
            let ty = js::group(&m, 1);
            let head_word = ty
                .split(|ch: char| ch.is_whitespace() || ch == '<' || ch == ':')
                .next()
                .unwrap_or("");
            if js::group(&m, 3) != name
                || !starts(whole.start())
                || is_not_a_declared_type(head_word)
                || js::test(&excluded, ty)
            {
                continue;
            }
            let mut kind = BindingKind::Variable;
            if js::group(&m, 4) == "(" {
                if js::group(&m, 2).contains(['*', '&']) {
                    continue;
                }
                let args = cpp_paren_text_from(code, li, whole.end() - 1).unwrap_or_default();
                if cpp_declares_local_function(ctx, ty, &args) {
                    kind = BindingKind::Function;
                }
            }
            bindings.push(Binding {
                line: li,
                column: whole.start(),
                kind,
                loop_paren: None,
            });
        }
        if text.contains(',') {
            if let Some(m) = cpp_listed_declarator_on(ctx, &text, name) {
                if starts(m.index) {
                    bindings.push(Binding {
                        line: li,
                        column: m.index,
                        kind: BindingKind::Variable,
                        loop_paren: None,
                    });
                }
            }
        }
        if let Some(lp) = cpp_range_for_declaration(ctx, &text, name) {
            bindings.push(Binding {
                line: li,
                column: lp.paren,
                kind: BindingKind::Variable,
                loop_paren: Some(lp.paren),
            });
        }
        for m in js::all(&ctx.re(CPP_LAMBDA_HEAD), &text) {
            if cpp_parameter_names(ctx, js::group(&m, 1))
                .iter()
                .any(|p| p == name)
            {
                bindings.push(Binding {
                    line: li,
                    column: m.get(0).unwrap().end(),
                    kind: BindingKind::Variable,
                    loop_paren: None,
                });
            }
        }
    }
    bindings
}

/// Is the code at (`to_line`, `to_column`) inside the brace block open at (`line`, `column`)?
fn cpp_block_reaches(
    code: &CodeLines,
    line: usize,
    column: usize,
    to_line: usize,
    to_column: usize,
) -> bool {
    let mut depth = 0i32;
    for l in line..=to_line {
        let text = code.get(l);
        let b = text.as_bytes();
        let end = if l == to_line {
            to_column.min(b.len())
        } else {
            b.len()
        };
        let from = if l == line { column } else { 0 };
        for &ch in b.iter().take(end).skip(from) {
            if ch == b'{' {
                depth += 1;
            } else if ch == b'}' {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
        }
    }
    true
}

/// Whether a C++ call written by its bare name calls a local of the calling function.
pub fn is_cpp_local_callable_call(ctx: &Ctx, r: &Ref) -> bool {
    let name = &r.name;
    if !js::is_ident(name) || cpp_implicit_receiver(ctx, r) != Some("bare") {
        return false;
    }
    let caller = r.from;
    let c = ctx.sym(caller);
    if !matches!(c.kind, NodeKind::Function | NodeKind::Method)
        || c.file != r.file
        || c.name == *name
        || r.line < c.start_line
        || r.line > c.end_line.max(c.start_line)
    {
        return false;
    }
    let Some(code) = CodeLines::new(ctx, &r.file) else {
        return false;
    };
    let key = format!("lb\0{caller}\0{name}");
    let bindings = match ctx.memo_get::<Rc<Vec<Binding>>>(memo::NM, &key) {
        Some(b) => b,
        None => {
            let head = cpp_caller_head(ctx, caller, &code, code.raw.lines.len());
            let b = Rc::new(cpp_local_bindings(ctx, name, caller, &head, &code));
            ctx.memo_set(memo::NM, key, b.clone());
            b
        }
    };
    let call_line = (r.line as usize).saturating_sub(1);
    let column = r.column as usize;
    let mut decided: Option<&Binding> = None;
    for b in bindings.iter() {
        if b.line > call_line || (b.line == call_line && b.column >= column) {
            continue;
        }
        let in_scope = b.kind == BindingKind::Parameter
            || match b.loop_paren {
                Some(paren) => {
                    cpp_for_body_encloses(&code.raw.lines, b.line, paren, call_line, column)
                }
                None => cpp_block_reaches(&code, b.line, b.column, call_line, column),
            };
        if in_scope
            && decided.is_none_or(|d| b.line > d.line || (b.line == d.line && b.column > d.column))
        {
            decided = Some(b);
        }
    }
    decided.is_some_and(|d| matches!(d.kind, BindingKind::Parameter | BindingKind::Variable))
}

/// `isMethodOwnerKind`.
pub fn is_method_owner_kind(ctx: &Ctx, n: SymId) -> bool {
    let s = ctx.sym(n);
    matches!(
        s.kind,
        NodeKind::Class | NodeKind::Struct | NodeKind::Union | NodeKind::Interface
    ) || (s.language == Language::Scala && s.kind == NodeKind::Module)
}

/// A capitalized type name that names no class the project declares.
pub fn is_undeclared_type_name(ctx: &Ctx, type_name: &str, r: &Ref) -> bool {
    type_name.starts_with(|c: char| c.is_ascii_uppercase())
        && !ctx.named(type_name).iter().any(|&n| {
            is_method_owner_kind(ctx, n) && same_language_family(ctx.sym(n).language, r.lang)
        })
}

/// Is the qualifier of a C++ `X::m()` call a class the index doesn't hold?
pub fn is_cpp_undeclared_qualifier(ctx: &Ctx, qualifier: &str, r: &Ref) -> bool {
    if !is_undeclared_type_name(ctx, qualifier, r)
        || !cpp_types_named(ctx, qualifier).is_empty()
        || ctx.named(qualifier).iter().any(|&n| {
            let s = ctx.sym(n);
            is_c_family(s.language) && matches!(s.kind, NodeKind::Namespace | NodeKind::Module)
        })
    {
        return false;
    }
    let names = cpp_names_brought_in(ctx, &r.file);
    !names.types.contains(qualifier) && !names.namespaces.contains(qualifier)
}

/// Is the qualifier of a C++ `x::m()` call a namespace, and not a type?
pub fn is_cpp_namespace_qualifier(ctx: &Ctx, qualifier: &str, r: &Ref) -> bool {
    qualifier.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && cpp_types_named(ctx, qualifier).is_empty()
        && !cpp_names_brought_in(ctx, &r.file).types.contains(qualifier)
}

/// Is `method` the constructor of the class a C++ call `ns::m(…)` names through its namespace?
pub fn is_cpp_constructor_through(ctx: &Ctx, namespace: &str, method: SymId) -> bool {
    let m = ctx.sym(method);
    let parts: Vec<&str> = m.qualified_name.split("::").collect();
    if parts.len() < 3 || parts[parts.len() - 3] != namespace {
        return false;
    }
    let owner = parts[parts.len() - 2];
    if owner == m.name {
        return true;
    }
    if owner != "<anonymous>" {
        return false;
    }
    let holder = ctx
        .by_qname(&parts[..parts.len() - 1].join("::"))
        .iter()
        .copied()
        .find(|&n| {
            let s = ctx.sym(n);
            s.file == m.file
                && s.start_line <= m.start_line
                && s.end_line.max(s.start_line) >= m.start_line
        });
    let head = holder
        .and_then(|h| {
            let s = ctx.sym(h);
            let src = ctx.source(&s.file)?;
            let from = (s.start_line as usize)
                .saturating_sub(1)
                .min(src.lines.len());
            let to = (s.start_line as usize + 1).min(src.lines.len());
            Some(src.lines[from..to].join(" "))
        })
        .unwrap_or_default();
    js::test(
        &ctx.re(&format!(
            r"\b(?:class|struct|union)\s+(?:[A-Za-z_]\w*\s+)*{}\s*<",
            js::escape(&m.name)
        )),
        &head,
    )
}

/// The names a C++ file makes types or namespaces of itself.
#[derive(Debug, Default)]
pub struct CppFileNames {
    pub types: HashSet<String>,
    pub namespaces: HashSet<String>,
}

pub fn cpp_names_brought_in(ctx: &Ctx, file: &str) -> Rc<CppFileNames> {
    let key = format!("nb\0{file}");
    if let Some(hit) = ctx.memo_get::<Rc<CppFileNames>>(memo::NM, &key) {
        return hit;
    }
    let raw = ctx.source(file).map(|s| s.text.clone()).unwrap_or_default();
    let text = js::strip_comments_cpp(&js::replace_with(&ctx.re(r"\\\r?\n"), &raw, " "));
    let mut types = aliases::type_names_declared(ctx, &text);
    let mut namespaces = HashSet::new();
    let ident = ctx.re(r"[A-Za-z_]\w*");
    for m in js::all(&ctx.re(r"\bnamespace\s+([^;{}()]{1,200}?)\s*[;{]"), &text) {
        for name in js::all(&ident, js::group(&m, 1)) {
            namespaces.insert(name.get(0).unwrap().as_str().to_string());
        }
    }
    for (mac, declared) in cpp_macro_type_names(ctx).iter() {
        if cpp_has_word(&text, mac) {
            types.extend(declared.iter().cloned());
        }
    }
    if js::test(&ctx.re(r"\bTYPED_TEST(?:_P)?\s*\("), &text) {
        types.insert("TypeParam".into());
        types.insert("TestFixture".into());
    }
    let names = Rc::new(CppFileNames { types, namespaces });
    ctx.memo_set(memo::NM, key, names.clone());
    names
}

/// The type names each of the project's macros declares where it is expanded, in definition order.
pub fn cpp_macro_type_names(ctx: &Ctx) -> Rc<Vec<(String, HashSet<String>)>> {
    ctx.memo(memo::NM, "mt".into(), || {
        let header = ctx.re(r"(?i)\.(?:h|hh|hpp|hxx|h\+\+|inl|inc|ipp|tcc)$");
        let define = ctx.re(r"(?m)^[ \t]*#[ \t]*define[ \t]+([A-Za-z_]\w*)(?:\([^)\n]*\))?((?:[^\n\\]|\\\r?\n|\\(?!\r?\n))*)");
        let declares = ctx.re(r"\b(?:using|typedef|template)\b");
        let splice = ctx.re(r"\\\r?\n");
        let mut seen = HashSet::new();
        let mut macros = Vec::new();
        for file in ctx.all_files() {
            if !js::test(&header, file) {
                continue;
            }
            let Some(src) = ctx.source(file) else { continue };
            if !src.text.contains("define") {
                continue;
            }
            for m in js::all(&define, &src.text) {
                let name = js::group(&m, 1);
                let body = js::group(&m, 2);
                if seen.contains(name) || !js::test(&declares, body) {
                    continue;
                }
                let names = aliases::type_names_declared(ctx, &js::strip_comments_cpp(&js::replace_with(&splice, body, " ")));
                if !names.is_empty() {
                    seen.insert(name.to_string());
                    macros.push((name.to_string(), names));
                }
            }
        }
        Rc::new(macros)
    })
}
