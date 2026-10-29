//! What a C# name can reach: the namespaces a file runs in and imports, the
//! types around a reference and what they derive from, `using static` types
//! and `using` aliases (name-matcher.ts `isCsharpMemberInScope` …
//! `csharpAncestorNames`, `csharpUsingAlias`, `isDotNetTypeRef`).

use crate::cpp::ctx::memo;
use crate::cpp::{js, nm, Ctx, Ref};
use crate::symbols::SymId;
use codegraph_core::{Language, NodeKind, ReferenceKind};
use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;

/// Memo keys of this module, under the context's name-matcher family.
const NM: u16 = memo::NM;

/// `CSHARP_TYPE_KINDS` (the index holds no `record` kind).
pub fn is_type_kind(k: NodeKind) -> bool {
    matches!(
        k,
        NodeKind::Class | NodeKind::Interface | NodeKind::Enum | NodeKind::Struct
    )
}

/// `CSHARP_MEMBER_KINDS` (the index holds no `event` kind).
fn is_member_kind(k: NodeKind) -> bool {
    matches!(
        k,
        NodeKind::Method
            | NodeKind::Property
            | NodeKind::Field
            | NodeKind::EnumMember
            | NodeKind::Constant
    )
}

/// `path.posix.dirname`.
pub fn dirname(p: &str) -> &str {
    match p.trim_end_matches('/').rfind('/') {
        Some(0) => "/",
        Some(i) => &p[..i],
        None => ".",
    }
}

fn last_dotted(s: &str) -> String {
    s.rsplit('.').next().unwrap_or(s).to_string()
}

/// `hasNoReceiverOnLine`: the bare name has no `.` before it on its line.
pub fn has_no_receiver_on_line(ctx: &Ctx, r: &Ref) -> bool {
    let Some(src) = ctx.source(&r.file) else {
        return true;
    };
    // (`split('\n')`: a CRLF line keeps its `\r`, which nothing here reads.)
    let Some(line) = (r.line as usize)
        .checked_sub(1)
        .and_then(|i| src.lines.get(i))
    else {
        return true;
    };
    let lower = line.to_lowercase();
    let name = r.name.to_lowercase();
    let column = r.column as usize;
    let start = if js::starts_at(&lower, column, &name) {
        Some(column)
    } else if column >= name.len() && js::starts_at(&lower, column - name.len(), &name) {
        Some(column - name.len())
    } else {
        js::search(
            &ctx.re(&format!(r"(?<![\w$]){}\s*\(", js::escape(&name))),
            &lower,
        )
    };
    let Some(start) = start else {
        return false;
    };
    !js::test(&ctx.re(r"\.\s*$"), js::slice(line, 0, start))
}

/// `csharpSupertypesOf`: the simple names a C# type's declarations derive from.
pub fn supertypes_of(ctx: &Ctx, type_name: &str) -> Rc<Vec<String>> {
    ctx.memo(NM, format!("cs-sup\0{type_name}"), || {
        let mut names = Vec::new();
        for &d in ctx.named(type_name) {
            let s = ctx.sym(d);
            if s.language != Language::CSharp || !is_type_kind(s.kind) {
                continue;
            }
            let Some(src) = ctx.source(&s.file) else {
                continue;
            };
            let from = (s.start_line as usize)
                .saturating_sub(1)
                .min(src.lines.len());
            let to = (s.start_line as usize + 8).min(src.lines.len());
            let mut head = src.lines[from..to].join(" ");
            if let Some(brace) = head.find('{') {
                head.truncate(brace);
            }
            let mut depth = 0i32;
            let mut flat = String::new();
            for ch in head.chars() {
                match ch {
                    '<' | '(' => depth += 1,
                    '>' | ')' => depth = (depth - 1).max(0),
                    _ if depth == 0 => flat.push(ch),
                    _ => {}
                }
            }
            let bases = js::caps(
                &ctx.re(&format!(
                    r"\b{}\s*:\s*(.*?)(?:\bwhere\b|$)",
                    js::escape(type_name)
                )),
                &flat,
            )
            .map(|c| js::group(&c, 1).to_string())
            .unwrap_or_default();
            for m in js::all(&ctx.re(r"([A-Za-z_][\w.]*)"), &bases) {
                names.push(last_dotted(js::group(&m, 1)));
            }
        }
        Rc::new(names)
    })
}

/// The entries of a project directory (empty when it can't be read).
fn dir_entries(ctx: &Ctx, dir: &str) -> Vec<String> {
    let path = if matches!(dir, "." | "" | "/") {
        ctx.root.to_path_buf()
    } else {
        ctx.root.join(dir)
    };
    let mut entries: Vec<String> = std::fs::read_dir(&path)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    // (readdir's order, which is the file system's: sorted here.)
    entries.sort();
    entries
}

fn read_project_file(ctx: &Ctx, dir: &str, entry: &str) -> Option<String> {
    let base = if matches!(dir, "." | "" | "/") {
        ctx.root.to_path_buf()
    } else {
        ctx.root.join(dir)
    };
    std::fs::read_to_string(Path::new(&base).join(entry)).ok()
}

/// `csharpStaticUsings`: the types a C# file sees through static usings.
pub fn static_usings(ctx: &Ctx, file: &str) -> Rc<HashSet<String>> {
    ctx.memo(NM, format!("cs-su\0{file}"), || {
        let mut owners: HashSet<String> = (*project_static_usings(ctx, dirname(file))).clone();
        let text = ctx.source(file).map(|s| s.text.clone()).unwrap_or_default();
        for m in js::all(
            &ctx.re(r"(?m)^\s*(?:global\s+)?using\s+static\s+([\w.]+)\s*;"),
            &text,
        ) {
            owners.insert(last_dotted(js::group(&m, 1)));
        }
        Rc::new(owners)
    })
}

fn project_static_usings(ctx: &Ctx, dir: &str) -> Rc<HashSet<String>> {
    ctx.memo(NM, format!("cs-psu\0{dir}"), || {
        let mut owners = HashSet::new();
        if matches!(dir, "." | "" | "/") {
            for f in ctx.all_files() {
                if !f.ends_with(".cs") {
                    continue;
                }
                let Some(src) = ctx.source(f) else { continue };
                if !src.text.contains("global using static") {
                    continue;
                }
                for m in js::all(
                    &ctx.re(r"(?m)^\s*global\s+using\s+static\s+([\w.]+)\s*;"),
                    &src.text,
                ) {
                    owners.insert(last_dotted(js::group(&m, 1)));
                }
            }
        } else {
            owners.extend(project_static_usings(ctx, dirname(dir)).iter().cloned());
        }
        let include = ctx.re(r#"(?i)<Using\s+Include\s*=\s*"([\w.]+)"[^>]*\bStatic\s*=\s*"true""#);
        for entry in dir_entries(ctx, dir) {
            if !js::test(&ctx.re(r"(?i)\.(?:csproj|props)$"), &entry) {
                continue;
            }
            let Some(text) = read_project_file(ctx, dir, &entry) else {
                continue;
            };
            for m in js::all(&include, &text) {
                owners.insert(last_dotted(js::group(&m, 1)));
            }
        }
        Rc::new(owners)
    })
}

/// What `csharpNamespaceScope` reads of a file.
#[derive(Debug, Default)]
pub struct NamespaceScope {
    pub namespaces: Vec<String>,
    pub usings: HashSet<String>,
    pub aliases: Vec<(String, String)>,
}

impl NamespaceScope {
    fn alias(&self, name: &str) -> Option<&str> {
        // (A Map keeps the last `set` of a key.)
        self.aliases
            .iter()
            .rev()
            .find(|(a, _)| a == name)
            .map(|(_, t)| t.as_str())
    }
}

const IMPLICIT_USINGS: &[&str] = &[
    "System",
    "System.Collections.Generic",
    "System.IO",
    "System.Linq",
    "System.Net.Http",
    "System.Threading",
    "System.Threading.Tasks",
];

/// `csharpNamespaceScope`: the namespaces a C# file's code runs in and the ones it imports.
pub fn namespace_scope(ctx: &Ctx, file: &str) -> Rc<NamespaceScope> {
    ctx.memo(NM, format!("cs-ns\0{file}"), || {
        let raw = ctx.source(file).map(|s| s.text.clone()).unwrap_or_default();
        let text = js::strip_comments_cpp(&raw);
        let namespaces = js::all(&ctx.re(r"(?m)^\s*namespace\s+([\w.]+)"), &text)
            .iter()
            .map(|m| js::group(m, 1).to_string())
            .collect();
        let (project_usings, project) = project_usings(ctx, dirname(file));
        let mut usings: HashSet<String> = (*project_usings).clone();
        if !project {
            usings.extend(global_usings(ctx, ".").iter().cloned());
        }
        let mut aliases = Vec::new();
        for m in js::all(
            &ctx.re(
                r"(?m)^\s*(?:global\s+)?using\s+(?!static\b)(?:([A-Za-z_]\w*)\s*=\s*)?([\w.]+)\s*;",
            ),
            &text,
        ) {
            match js::group_opt(&m, 1) {
                Some(a) => aliases.push((a.to_string(), js::group(&m, 2).to_string())),
                None => {
                    usings.insert(js::group(&m, 2).to_string());
                }
            }
        }
        Rc::new(NamespaceScope {
            namespaces,
            usings,
            aliases,
        })
    })
}

/// `csharpProjectUsings`: the project-wide usings from `dir` up, and whether a `.csproj` sits there or above.
fn project_usings(ctx: &Ctx, dir: &str) -> (Rc<HashSet<String>>, bool) {
    ctx.memo(NM, format!("cs-pu\0{dir}"), || {
        let root = matches!(dir, "." | "" | "/");
        let (mut usings, mut project) = if root {
            (HashSet::new(), false)
        } else {
            let (u, p) = project_usings(ctx, dirname(dir));
            ((*u).clone(), p)
        };
        let include =
            ctx.re(r#"(?i)<Using\s+Include\s*=\s*"([\w.]+)"(?![^>]*\bStatic\s*=\s*"true")[^>]*>"#);
        let implicit = ctx.re(r"(?i)<ImplicitUsings>\s*(?:enable|true)\s*</ImplicitUsings>");
        for entry in dir_entries(ctx, dir) {
            if !js::test(&ctx.re(r"(?i)\.(?:csproj|props)$"), &entry) {
                continue;
            }
            let Some(text) = read_project_file(ctx, dir, &entry) else {
                continue;
            };
            if js::test(&ctx.re(r"(?i)\.csproj$"), &entry) && !project {
                project = true;
                usings.extend(
                    global_usings(ctx, if root { "." } else { dir })
                        .iter()
                        .cloned(),
                );
            }
            for m in js::all(&include, &text) {
                usings.insert(js::group(&m, 1).to_string());
            }
            if js::test(&implicit, &text) {
                usings.extend(IMPLICIT_USINGS.iter().map(|s| s.to_string()));
            }
        }
        (Rc::new(usings), project)
    })
}

/// `csharpGlobalUsings`: the `global using X;` of the `.cs` files under `dir`.
fn global_usings(ctx: &Ctx, dir: &str) -> Rc<HashSet<String>> {
    ctx.memo(NM, format!("cs-gu\0{dir}"), || {
        let prefix = if dir == "." {
            String::new()
        } else {
            format!("{dir}/")
        };
        let mut usings = HashSet::new();
        for f in ctx.all_files() {
            if !f.ends_with(".cs") || !f.starts_with(&prefix) {
                continue;
            }
            let Some(src) = ctx.source(f) else { continue };
            if !src.text.contains("global using") {
                continue;
            }
            for m in js::all(
                &ctx.re(r"(?m)^\s*global\s+using\s+(?!static\b)([\w.]+)\s*;"),
                &src.text,
            ) {
                usings.insert(js::group(&m, 1).to_string());
            }
        }
        Rc::new(usings)
    })
}

/// `isCsharpTypeVisible`: whether a bare C# type name can mean `n`.
pub fn is_type_visible(ctx: &Ctx, n: SymId, r: &Ref) -> bool {
    let q = &ctx.sym(n).qualified_name;
    let Some(cut) = q.find("::") else {
        return true;
    };
    let ns = &q[..cut];
    let scope = namespace_scope(ctx, &r.file);
    if let Some(aliased) = scope.alias(&r.name) {
        return aliased == format!("{ns}.{}", q[cut + 2..].replace("::", "."));
    }
    if scope
        .namespaces
        .iter()
        .any(|own| own == ns || own.starts_with(&format!("{ns}.")))
    {
        return true;
    }
    scope.usings.contains(ns)
}

/// The C# types of `file` whose lines hold `line`.
fn types_around(ctx: &Ctx, file: &str, line: u32) -> Vec<SymId> {
    ctx.in_file(file)
        .iter()
        .copied()
        .filter(|&t| {
            let s = ctx.sym(t);
            is_type_kind(s.kind) && s.start_line <= line && s.end_line >= line
        })
        .collect()
}

/// `isCsharpNestedTypeInScope`: whether a bare C# name can reach `n` as a nested type.
pub fn is_nested_type_in_scope(ctx: &Ctx, n: SymId, r: &Ref) -> bool {
    let s = ctx.sym(n);
    let Some(cut) = s.qualified_name.rfind("::") else {
        return true;
    };
    let owner = &s.qualified_name[..cut];
    let Some(owner_type) = ctx
        .in_file(&s.file)
        .iter()
        .copied()
        .find(|&p| is_type_kind(ctx.kind(p)) && ctx.sym(p).qualified_name == owner)
    else {
        return true;
    };
    let enclosing = types_around(ctx, &r.file, r.line);
    let nested = format!("{owner}::");
    if enclosing.iter().any(|&p| {
        let q = &ctx.sym(p).qualified_name;
        q == owner || q.starts_with(&nested)
    }) {
        return true;
    }
    let owner_name = &ctx.sym(owner_type).name;
    enclosing
        .iter()
        .any(|&p| ancestor_names(ctx, &ctx.sym(p).qualified_name, 0).contains(owner_name))
}

/// `csharpAncestorNames`: the simple names of the C# types `qn` derives from, a few levels up.
pub fn ancestor_names(ctx: &Ctx, qn: &str, depth: u32) -> Rc<HashSet<String>> {
    let key = format!("cs-anc\0{qn}");
    if let Some(hit) = ctx.memo_get::<Rc<std::cell::RefCell<HashSet<String>>>>(NM, &key) {
        return Rc::new(hit.borrow().clone());
    }
    // A cycle reads what is gathered so far.
    let cell = Rc::new(std::cell::RefCell::new(HashSet::new()));
    ctx.memo_set(NM, key, cell.clone());
    let mut names: HashSet<String> = HashSet::new();
    let base_name = ctx.re(r"([A-Za-z_]\w*)\s*(?:<.*)?$");
    for &d in ctx.by_qname(qn) {
        let s = ctx.sym(d);
        if s.language != Language::CSharp || !is_type_kind(s.kind) {
            continue;
        }
        let Some(src) = ctx.source(&s.file) else {
            continue;
        };
        let mut header = String::new();
        let mut i = (s.start_line as usize).saturating_sub(1);
        while i < src.lines.len().min(s.start_line as usize + 6) && !header.contains('{') {
            header.push_str(src.lines.get(i).map(String::as_str).unwrap_or(""));
            header.push(' ');
            i += 1;
        }
        let head = header.split('{').next().unwrap_or("");
        let head = js::replace_with(&ctx.re(r"\bwhere\b[\s\S]*$"), head, "");
        let list = js::caps(&ctx.re(r":\s*([^{;]*)"), &head)
            .map(|c| js::group(&c, 1).to_string())
            .unwrap_or_default();
        for base in nm::split_cpp_top_level(ctx, &list) {
            if let Some(m) = js::caps(&base_name, base.trim()) {
                names.insert(js::group(&m, 1).to_string());
            }
        }
        cell.borrow_mut().extend(names.iter().cloned());
    }
    if depth < 4 {
        let bases: Vec<String> = names.iter().cloned().collect();
        for base in bases {
            for &t in ctx.named(&base) {
                let s = ctx.sym(t);
                if s.language != Language::CSharp || !is_type_kind(s.kind) || s.qualified_name == qn
                {
                    continue;
                }
                for up in ancestor_names(ctx, &s.qualified_name, depth + 1).iter() {
                    names.insert(up.clone());
                }
            }
            cell.borrow_mut().extend(names.iter().cloned());
        }
    }
    *cell.borrow_mut() = names.clone();
    Rc::new(names)
}

/// `isCsharpMemberInScope`: whether a bare C# name can mean the member `n`.
pub fn is_member_in_scope(ctx: &Ctx, n: SymId, r: &Ref) -> bool {
    let s = ctx.sym(n);
    if !is_member_kind(s.kind) {
        return true;
    }
    let Some(cut) = s.qualified_name.rfind("::") else {
        return true;
    };
    if !has_no_receiver_on_line(ctx, r) {
        return true;
    }
    let owner = s.qualified_name[..cut].rsplit("::").next().unwrap_or("");
    let owner = owner.rsplit('.').next().unwrap_or(owner);
    if static_usings(ctx, &r.file).contains(owner) {
        return true;
    }
    let around = types_around(ctx, &r.file, r.line);
    if around.is_empty() {
        return true;
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: std::collections::VecDeque<String> =
        around.iter().map(|&t| ctx.sym(t).name.clone()).collect();
    while let Some(name) = queue.pop_front() {
        if seen.len() >= 40 {
            break;
        }
        if seen.contains(&name) {
            continue;
        }
        seen.insert(name.clone());
        if name == owner {
            return true;
        }
        queue.extend(supertypes_of(ctx, &name).iter().cloned());
    }
    false
}

/// `csharpUsingAlias`: the simple name of the type a file's `using Name = …;` aliases `name` to.
pub fn using_alias(ctx: &Ctx, name: &str, r: &Ref) -> Option<String> {
    if !js::is_ident(name) {
        return None;
    }
    let aliases: Rc<Vec<(String, String)>> = ctx.memo(NM, format!("cs-al\0{}", r.file), || {
        let text = ctx.source(&r.file).map(|s| s.text.clone()).unwrap_or_default();
        Rc::new(
            js::all(
                &ctx.re(r"(?m)^\s*(?:global\s+)?using\s+([A-Za-z_]\w*)\s*=\s*(?:global::)?([\w.]+)\s*(?:<[^;>]*>)?\s*;"),
                &text,
            )
            .iter()
            .map(|m| (js::group(m, 1).to_string(), last_dotted(js::group(m, 2))))
            .collect(),
        )
    });
    aliases
        .iter()
        .rev()
        .find(|(a, _)| a == name)
        .map(|(_, t)| t.clone())
}

/// `isDotNetTypeRef`, for C#: a reference whose site is a type position.
pub fn is_type_position_ref(ctx: &Ctx, r: &Ref) -> bool {
    if r.lang != Language::CSharp
        || !matches!(
            r.kind,
            ReferenceKind::References
                | ReferenceKind::Instantiates
                | ReferenceKind::TypeOf
                | ReferenceKind::Returns
        )
    {
        return false;
    }
    let name = &r.name;
    if !js::is_ident(name) {
        return false;
    }
    if r.kind == ReferenceKind::Instantiates {
        return true;
    }
    let Some(src) = ctx.source(&r.file) else {
        return false;
    };
    let Some(line) = (r.line as usize)
        .checked_sub(1)
        .and_then(|i| src.lines.get(i))
    else {
        return false;
    };
    let column = r.column as usize;
    if !js::starts_at(line, column, name) {
        return false;
    }
    let before = &line[..column];
    let after = &line[column + name.len()..];
    if js::test(&ctx.re(r"\bnew\s+$"), before) {
        return true;
    }
    if js::test(&ctx.re(r"^\s+@?[A-Za-z_]"), after) {
        return !js::test(
            &ctx.re(r"^\s+(?:is|as|and|or|not|when|in|switch|with)\b"),
            after,
        );
    }
    if js::test(&ctx.re(r"^<|^\?(?![.?\[])|^\[\s*[,\]]"), after) {
        return true;
    }
    js::test(&ctx.re(r"^\s*[,>]"), after) && js::test(&ctx.re(r"<[^<>()]*$"), before)
}

/// `canNameInTypePosition`.
pub fn can_name_in_type_position(ctx: &Ctx, n: SymId) -> bool {
    !matches!(
        ctx.kind(n),
        NodeKind::Property
            | NodeKind::Method
            | NodeKind::EnumMember
            | NodeKind::Field
            | NodeKind::Constant
    )
}
