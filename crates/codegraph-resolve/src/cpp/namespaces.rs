//! C and C++ namespaces the source spells in ways the index cannot see: a
//! namespace alias (`namespace py = pybind11;`) and a macro that opens a
//! namespace (`FMT_BEGIN_NAMESPACE`) — upstream `cpp-namespaces.ts`.

use super::ctx::{memo, Ctx};
use super::js;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// The project's namespace aliases: `namespace py = pybind11;`.
pub fn namespace_aliases(ctx: &Ctx) -> Rc<HashMap<String, String>> {
    ctx.memo(memo::NS_ALIASES, String::new(), || {
        let src_re = ctx.re(r"(?i)\.(?:h|hh|hpp|hxx|inl|c|cc|cpp|cxx)$");
        let alias_re = ctx.re(
            r"(?m)^[ \t]*namespace[ \t]+([A-Za-z_]\w*)[ \t]*=[ \t]*(?:::)?([A-Za-z_][\w:]*)[ \t]*;",
        );
        let mut aliases = HashMap::new();
        for file in ctx.all_files() {
            if !js::test(&src_re, file) {
                continue;
            }
            let Some(src) = ctx.source(file) else {
                continue;
            };
            if !src.text.contains("namespace") {
                continue;
            }
            for m in js::all(&alias_re, &src.text) {
                aliases
                    .entry(js::group(&m, 1).to_string())
                    .or_insert_with(|| js::group(&m, 2).to_string());
            }
        }
        Rc::new(aliases)
    })
}

#[derive(Default)]
pub struct NsMacros {
    pub openers: HashMap<String, Vec<String>>,
    /// The `(name)`-taking openers: `PYBIND11_NAMESPACE_BEGIN(detail)`.
    pub opener_fns: HashSet<String>,
    pub closers: HashMap<String, usize>,
    pub aliases: HashMap<String, String>,
    /// Opener names in the order their definitions were read.
    opener_order: Vec<String>,
}

const CPP_CLOSER_BODY: &str = r"^(?:[A-Za-z_]\w*\s+)*\}(?:\s*\})*\s*;?$";

/// The project's namespace-opening macros as the namespace path each opens,
/// and the closing macros as how many scopes each closes.
pub fn namespace_macros(ctx: &Ctx) -> Rc<NsMacros> {
    ctx.memo(memo::NS_MACROS, String::new(), || {
        let header_re = ctx.re(r"(?i)\.(?:h|hh|hpp|hxx|h\+\+|inl|ipp|tcc)$");
        let define_re = ctx.re(
            r"(?m)^[ \t]*#[ \t]*define[ \t]+([A-Za-z_]\w*)(\(\s*([A-Za-z_]\w*)?\s*\))?[ \t]+([^\n]*)$",
        );
        let closer = ctx.re(CPP_CLOSER_BODY);
        let splice = ctx.re(r"\\\r?\n");
        let inline_body = ctx.re(
            r"^(?:inline\s+namespace\s+[^{}]*\{\s*|namespace\s+[A-Za-z_]\w*\s*\{\s*)+[\w\s]*$",
        );
        let mut m = NsMacros::default();
        let mut bodies: Vec<(String, String)> = Vec::new();
        for file in ctx.all_files() {
            if !js::test(&header_re, file) {
                continue;
            }
            let Some(raw) = ctx.source(file) else {
                continue;
            };
            if !raw.text.contains('#') || !raw.text.contains("define") {
                continue;
            }
            let source = js::strip_comments_cpp(&js::replace_with(&splice, &raw.text, " "));
            for d in js::all(&define_re, &source) {
                let name = js::group(&d, 1).to_string();
                let body = js::group(&d, 4).trim().to_string();
                if d.get(2).is_some() {
                    let param = js::group(&d, 3);
                    let opens = !param.is_empty()
                        && js::test(
                            &ctx.re(&format!(r"^namespace\s+{}\s*\{{[\w\s]*$", js::escape(param))),
                            &body,
                        );
                    if opens {
                        m.opener_fns.insert(name);
                    } else if js::test(&closer, &body) {
                        m.closers.insert(name, body.matches('}').count());
                    }
                    continue;
                }
                if js::is_ident(&body) {
                    m.aliases.insert(name, body);
                } else if js::test(&closer, &body) {
                    m.closers.insert(name, body.matches('}').count());
                } else if js::test(&inline_body, &body) {
                    bodies.push((name, body));
                }
            }
        }
        let strip_inline = ctx.re(r"inline\s+namespace\s+[^{}]*\{");
        let ns_word = ctx.re(r"namespace\s+([A-Za-z_]\w*)");
        for (name, body) in bodies {
            if m.openers.contains_key(&name) {
                continue;
            }
            let stripped = js::replace_with(&strip_inline, &body, "");
            let path: Vec<String> = js::all(&ns_word, &stripped)
                .iter()
                .map(|c| {
                    let n = js::group(c, 1);
                    m.aliases.get(n).cloned().unwrap_or_else(|| n.to_string())
                })
                .collect();
            if !path.is_empty() {
                m.opener_order.push(name.clone());
                m.openers.insert(name, path);
            }
        }
        Rc::new(m)
    })
}

/// The outermost namespaces the project's namespace-opening macros open.
pub fn macro_namespace_heads(ctx: &Ctx) -> Vec<String> {
    let m = namespace_macros(ctx);
    m.opener_order
        .iter()
        .filter_map(|n| m.openers.get(n).and_then(|p| p.first().cloned()))
        .collect()
}

/// The project's macros that stand for a single name (`#define ROCKSDB_NAMESPACE rocksdb`).
pub fn name_macros(ctx: &Ctx) -> Rc<NsMacros> {
    namespace_macros(ctx)
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub start: u32,
    pub end: u32,
    pub path: Vec<String>,
}

/// The line ranges of a file each namespace macro opens, with the path it opens.
pub fn macro_namespace_frames(ctx: &Ctx, file: &str) -> Rc<Vec<Frame>> {
    ctx.memo(memo::NS_FRAMES, file.to_string(), || {
        let mut frames: Vec<Frame> = Vec::new();
        let m = namespace_macros(ctx);
        if !m.openers.is_empty() || !m.opener_fns.is_empty() {
            let token_re = ctx.re(
                r"^[ \t]*([A-Z_][A-Z0-9_]*)(?:\(\s*([A-Za-z_]\w*)?\s*\))?[ \t]*;?[ \t]*(?://.*|/\*.*\*/[ \t]*)?\r?$",
            );
            let lines = ctx.source(file).map(|s| s.lines.clone()).unwrap_or_default();
            let mut open: Vec<(u32, Vec<String>)> = Vec::new();
            for (i, text) in lines.iter().enumerate() {
                let Some(c) = js::caps(&token_re, text) else {
                    continue;
                };
                let token = js::group(&c, 1);
                if token.is_empty() {
                    continue;
                }
                let path = match c.get(2).map(|g| g.as_str()) {
                    Some(a) if m.opener_fns.contains(token) => {
                        Some(vec![m.aliases.get(a).cloned().unwrap_or_else(|| a.to_string())])
                    }
                    None => m.openers.get(token).cloned(),
                    _ => None,
                };
                if let Some(path) = path {
                    open.push((i as u32 + 1, path));
                } else if m.closers.contains_key(token) && !open.is_empty() {
                    let (start, path) = open.pop().unwrap();
                    frames.push(Frame {
                        start,
                        end: i as u32 + 1,
                        path,
                    });
                }
            }
            for (start, path) in open {
                frames.push(Frame {
                    start,
                    end: lines.len() as u32,
                    path,
                });
            }
        }
        Rc::new(frames)
    })
}

/// A declaration's qualified name with the namespaces macros open around it.
pub fn macro_qualified_name(ctx: &Ctx, file: &str, start_line: u32, qualified: &str) -> String {
    let prefix = macro_prefix(ctx, file, start_line);
    if prefix.is_empty() {
        qualified.to_string()
    } else {
        format!("{}::{qualified}", prefix.join("::"))
    }
}

/// The namespace path the macro frames around `line` open, outermost first.
pub fn macro_prefix(ctx: &Ctx, file: &str, line: u32) -> Vec<String> {
    let mut frames: Vec<Frame> = macro_namespace_frames(ctx, file)
        .iter()
        .filter(|f| f.start <= line && f.end >= line)
        .cloned()
        .collect();
    frames.sort_by_key(|f| f.start);
    frames.into_iter().flat_map(|f| f.path).collect()
}
