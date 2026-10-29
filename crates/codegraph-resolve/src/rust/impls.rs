//! `impl` headers: the trait and type a Rust method's `impl` names, what
//! `Self` is where a reference is written, and a method's `self` wrapper.

use super::ctx::{Ctx, Ref};
use super::uses::{is_ident, is_impl_or_trait_line, starts_with_word};
use super::{DECLARED_TYPE_KINDS, STD_TYPES};
use crate::symbols::SymId;
use codegraph_core::NodeKind;

#[derive(Debug, Clone, PartialEq)]
pub struct ImplHeader {
    /// The trait's name, the last segment of its path (`linked_list::Link` → `Link`).
    pub trait_: String,
    /// The type names its arguments write (`From<&'a Handle>` → `Handle`).
    pub trait_args: Vec<String>,
    /// The implementing type's name (`NonNull<Entry>` → `NonNull`).
    pub self_: String,
    /// The type names its arguments write (`Arc<Handle>` → `Handle`).
    pub self_args: Vec<String>,
    /// The implementing type as written: `pubgrub::Range<Version>`.
    pub self_text: String,
    /// The impl's own type parameters (`impl<'a, P: DerefMut>` → `P`).
    pub params: Vec<String>,
}

/// `&'a mut ` / `&` / `dyn ` prefixes, stripped.
pub fn strip_refs(t: &str) -> &str {
    let mut t = t.trim_start();
    loop {
        let before = t;
        if let Some(r) = t.strip_prefix('&') {
            t = r.trim_start();
            if t.starts_with('\'') {
                t = t.split_once(char::is_whitespace).map_or("", |(_, r)| r).trim_start();
            }
            if let Some(r) = t.strip_prefix("mut") {
                if r.starts_with(char::is_whitespace) {
                    t = r.trim_start();
                }
            }
        }
        if let Some(r) = t.strip_prefix("dyn") {
            if r.starts_with(char::is_whitespace) {
                t = r.trim_start();
            }
        }
        if t == before {
            return t;
        }
    }
}

/// Only `&'a mut ` prefixes stripped.
pub fn strip_amp(t: &str) -> &str {
    let mut t = t.trim_start();
    while let Some(r) = t.strip_prefix('&') {
        t = r.trim_start();
        if t.starts_with('\'') {
            t = t.split_once(char::is_whitespace).map_or("", |(_, r)| r).trim_start();
        }
        if let Some(r) = t.strip_prefix("mut") {
            if r.starts_with(char::is_whitespace) {
                t = r.trim_start();
            }
        }
    }
    t
}

/// The identifiers of a text that are not preceded by `'` or a word char.
pub fn idents_of(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if (c.is_ascii_alphabetic() || c == b'_') && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'\'')) {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(text[s..i].to_string());
            continue;
        }
        i += 1;
    }
    out
}

/// `impl<'a, T> From<&'a Handle> for NonNull<Entry> where … {` → its trait and type; none for an inherent impl.
pub fn parse_impl_header(text: &str) -> Option<ImplHeader> {
    let mut rest = text.trim_start();
    if let Some(r) = rest.strip_prefix("pub") {
        let r2 = r.trim_start();
        rest = if r2.starts_with('(') { r2.find(')').map_or(r2, |e| &r2[e + 1..]) } else { r2 };
        rest = rest.trim_start();
    }
    if let Some(r) = rest.strip_prefix("unsafe") {
        rest = r.trim_start();
    }
    let mut rest = rest.strip_prefix("impl")?.to_string();
    if let Some(b) = rest.find('{') {
        rest.truncate(b);
    }
    if let Some(w) = super::uses::find_word(&rest, "where") {
        rest.truncate(w);
    }
    let mut rest = rest.trim().to_string();
    let mut params = Vec::new();
    if rest.starts_with('<') {
        let b = rest.as_bytes();
        let mut depth = 0;
        let mut i = 0;
        let mut from = 1;
        while i < b.len() {
            match b[i] {
                b'<' => depth += 1,
                b'>' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                b',' if depth == 1 => {
                    params.push(rest[from..i].to_string());
                    from = i + 1;
                }
                _ => {}
            }
            i += 1;
        }
        params.push(rest[from..i.min(rest.len())].to_string());
        rest = rest[(i + 1).min(rest.len())..].trim().to_string();
    }
    // ` for ` outside the angle brackets parts the trait from the type.
    let b = rest.as_bytes();
    let mut depth = 0i32;
    let mut split = None;
    for i in 0..b.len() {
        match b[i] {
            b'<' => depth += 1,
            b'>' => depth -= 1,
            c if depth == 0 && c.is_ascii_whitespace() && rest[i + 1..].starts_with("for") && rest[i + 4..].starts_with(char::is_whitespace) => {
                split = Some(i);
                break;
            }
            _ => {}
        }
    }
    let split = split?;
    let trait_part = rest[..split].trim().trim_start_matches('!');
    let head = |part: &str| -> String {
        let p = strip_refs(part).trim();
        let p = p.split('<').next().unwrap_or(p).trim();
        let last = p.rsplit("::").next().unwrap_or(p).trim();
        if is_ident(last) { last.to_string() } else { String::new() }
    };
    let args = |part: &str| -> Vec<String> {
        match part.find('<') {
            None => Vec::new(),
            Some(o) => idents_of(&part[o + 1..]).into_iter().filter(|i| i != "mut" && i != "dyn").collect(),
        }
    };
    let self_part = rest[split + 5..].trim();
    Some(ImplHeader {
        trait_: head(trait_part.split('<').next().unwrap_or(trait_part)),
        trait_args: args(trait_part),
        self_: head(self_part),
        self_args: args(strip_refs(self_part)),
        self_text: self_part.to_string(),
        params: params
            .iter()
            .filter_map(|p| {
                let p = p.trim_start();
                let p = p.strip_prefix("const ").unwrap_or(p).trim_start();
                let id: String = p.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                (!id.is_empty() && id.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')).then_some(id)
            })
            .collect(),
    })
}

fn is_item_line(line: &str) -> bool {
    let mut t = line;
    if let Some(r) = t.strip_prefix("pub") {
        let r2 = r.trim_start();
        t = if r2.starts_with('(') { r2.find(')').map_or(r2, |e| r2[e + 1..].trim_start()) } else { r2 };
    }
    ["fn", "struct", "enum", "mod", "trait", "const", "static", "type"]
        .iter()
        .any(|w| starts_with_word(t, w))
}

fn without_comment(l: &str) -> &str {
    l.find("//").map_or(l, |i| &l[..i])
}

impl<'a> Ctx<'a> {
    /// The `impl … {` header text at or above a line (lines joined until the `{`).
    fn header_text(&self, file: &str, header_line: u32) -> String {
        let Some(src) = self.source(file) else { return String::new() };
        let lines: Vec<&str> = src.lines().collect();
        let i = header_line as usize - 1;
        let mut text = without_comment(lines.get(i).copied().unwrap_or("")).to_string();
        let mut j = i + 1;
        while !text.contains('{') && j < lines.len().min(i + 8) {
            text.push(' ');
            text.push_str(without_comment(lines[j]));
            j += 1;
        }
        text
    }

    /// The `impl Trait<…> for Type<…>` a Rust method is declared in: none for an inherent impl, or none at all.
    pub fn trait_impl_header(&self, method: SymId) -> Option<ImplHeader> {
        self.memo(10, method.to_string(), || {
            let m = self.syms.get(method);
            let src = self.source(&m.file)?;
            let lines: Vec<&str> = src.lines().collect();
            let mut i = (m.start_line as usize).saturating_sub(1);
            while i > 0 {
                i -= 1;
                let line = lines.get(i).copied().unwrap_or("");
                if is_impl_line(line) {
                    return parse_impl_header(&self.header_text(&m.file, i as u32 + 1));
                }
                if is_item_line(line) {
                    break;
                }
            }
            None
        })
    }

    /// Whether a Rust method sits in an `impl Trait for Type` block.
    pub fn is_trait_impl_method(&self, cand: SymId) -> bool {
        let m = self.syms.get(cand);
        if m.kind != NodeKind::Method {
            return false;
        }
        self.memo(11, cand.to_string(), || {
            let Some(src) = self.source(&m.file) else { return false };
            let lines: Vec<&str> = src.lines().collect();
            let mut i = (m.start_line as usize).saturating_sub(1);
            while i > 0 {
                i -= 1;
                let line = lines.get(i).copied().unwrap_or("");
                if is_impl_line(line) {
                    let l = without_comment(line);
                    return l.contains(" for ") || l.contains("\tfor ") || l.contains(" for\t");
                }
                if is_item_line(line) {
                    break;
                }
            }
            false
        })
    }

    /// The type `Self` is where `r` is written: its `impl`'s implementing type (and trait), or the trait it sits in.
    pub fn self_type(&self, file: &str, line: u32) -> Option<SelfType> {
        let header = self.impl_line_at(file, line);
        if header == 0 {
            return None;
        }
        let text = self.header_text(file, header);
        let t = text.trim_start();
        let mut t2 = t;
        if let Some(r) = t2.strip_prefix("pub") {
            let r2 = r.trim_start();
            t2 = if r2.starts_with('(') { r2.find(')').map_or(r2, |e| r2[e + 1..].trim_start()) } else { r2 };
        }
        if let Some(r) = t2.strip_prefix("unsafe") {
            t2 = r.trim_start();
        }
        if let Some(r) = t2.strip_prefix("trait") {
            if r.starts_with(char::is_whitespace) {
                let name = super::scopes::ident_at(r.trim_start(), 0)?;
                return Some(SelfType {
                    ty: name.to_string(),
                    root: None,
                    trait_: None,
                    is_trait: true,
                });
            }
        }
        let imp = parse_impl_header(&text);
        let written: Vec<String> = match &imp {
            Some(_) => {
                let at = super::uses::find_word(&text, "for")?;
                let after = strip_amp(&text[at + 3..]);
                path_segments(after)
            }
            None => {
                let at = super::uses::find_word(&text, "impl")?;
                let mut after = text[at + 4..].trim_start();
                if after.starts_with('<') {
                    let end = angle_end(after, 0)?;
                    after = after[end + 1..].trim_start();
                }
                path_segments(after)
            }
        };
        let ty = match &imp {
            Some(h) => h.self_.clone(),
            None => written.last()?.clone(),
        };
        if ty.is_empty() {
            return None;
        }
        Some(SelfType {
            ty,
            root: (written.len() > 1).then(|| written[0].clone()),
            trait_: imp.map(|h| h.trait_).filter(|t| !t.is_empty()),
            is_trait: false,
        })
    }

    /// Whether `Self`'s type comes from outside the project.
    pub fn is_outside_self(&self, s: &SelfType, file: &str) -> bool {
        if let Some(root) = &s.root {
            return matches!(root.as_str(), "std" | "core" | "alloc") || self.graph().dependencies.contains(root);
        }
        let uses = self.uses(file);
        if uses.external.contains(&s.ty) && !uses.bound.contains(&s.ty) {
            return true;
        }
        STD_TYPES.contains(&s.ty.as_str()) && !self.is_project_named(&s.ty, DECLARED_TYPE_KINDS)
    }

    /// The std wrapper a Rust method's `self` is declared through — `self: Pin<&mut Self>` → `Pin`.
    pub fn self_wrapper(&self, method: SymId) -> Option<&'static str> {
        let sig = self.syms.get(method).signature.as_deref()?;
        let t = sig.trim_start().strip_prefix('(')?.trim_start();
        let t = t.strip_prefix("mut ").map_or(t, str::trim_start);
        let t = t.strip_prefix("self")?.trim_start().strip_prefix(':')?;
        let mut t = strip_amp(t);
        for p in ["std", "core", "alloc"] {
            if let Some(r) = t.strip_prefix(p) {
                if let Some(r) = r.trim_start().strip_prefix("::") {
                    // `std::pin::Pin`
                    let r = r.trim_start();
                    let e = r.find("::").unwrap_or(0);
                    t = r[e..].trim_start_matches(':').trim_start();
                }
            }
        }
        ["Pin", "Box", "Rc", "Arc"]
            .into_iter()
            .find(|w| t.strip_prefix(w).is_some_and(|r| r.trim_start().starts_with('<')))
    }

    /// The trait whose body declares a Rust method, if one does.
    pub fn trait_of(&self, method: SymId) -> Option<SymId> {
        self.memo(12, method.to_string(), || {
            let m = self.syms.get(method);
            let owner = m.owner()?.rsplit("::").next()?.to_string();
            self.syms.in_file(&m.file).iter().copied().find(|&i| {
                let t = self.syms.get(i);
                t.kind == NodeKind::Trait && t.name == owner && t.start_line <= m.start_line && t.end_line >= m.start_line
            })
        })
    }

    /// Whether `method`'s impl on a wrapper applies to one of `inner`.
    pub fn impl_for(&self, method: SymId, inner: Option<&str>) -> bool {
        self.trait_impl_header(method)
            .is_some_and(|h| h.self_args.iter().any(|a| h.params.contains(a) || Some(a.as_str()) == inner))
    }
}

fn is_impl_line(line: &str) -> bool {
    is_impl_or_trait_line(line) && {
        let t = line.trim_start();
        let t = t.strip_prefix("pub").map_or(t, |r| {
            let r = r.trim_start();
            if r.starts_with('(') { r.find(')').map_or(r, |e| r[e + 1..].trim_start()) } else { r }
        });
        let t = t.strip_prefix("unsafe").map_or(t, str::trim_start);
        starts_with_word(t, "impl")
    }
}

/// `a::b::C` segments at the start of `text`.
fn path_segments(text: &str) -> Vec<String> {
    let mut segs = Vec::new();
    let mut t = text.trim_start();
    loop {
        let Some(id) = super::scopes::ident_at(t, 0) else { break };
        segs.push(id.to_string());
        t = t[id.len()..].trim_start();
        match t.strip_prefix("::") {
            Some(r) => t = r.trim_start(),
            None => break,
        }
    }
    segs
}

/// The offset of the `>` closing the `<` at `open`.
pub fn angle_end(text: &str, open: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut depth = 0;
    for i in open..b.len() {
        match b[i] {
            b'<' => depth += 1,
            b'>' if i == 0 || b[i - 1] != b'-' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct SelfType {
    /// Its name, or the trait's in a trait's own body.
    pub ty: String,
    /// The first segment of the path it is written with, if any.
    pub root: Option<String>,
    /// The trait a trait impl implements.
    pub trait_: Option<String>,
    pub is_trait: bool,
}

impl<'a> Ctx<'a> {
    pub fn self_type_at(&self, r: &Ref<'_>) -> Option<SelfType> {
        self.self_type(r.file, r.line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_impl_headers() {
        let h = parse_impl_header("impl<'a, T> From<&'a Handle> for NonNull<Entry> where T: X {").unwrap();
        assert_eq!(h.trait_, "From");
        assert_eq!(h.trait_args, vec!["Handle"]);
        assert_eq!(h.self_, "NonNull");
        assert_eq!(h.self_args, vec!["Entry"]);
        assert_eq!(h.params, vec!["T"]);
        let h = parse_impl_header("impl Lowest for semverlib::Range {").unwrap();
        assert_eq!(h.self_, "Range");
        assert_eq!(h.self_text, "semverlib::Range");
        assert!(parse_impl_header("impl<T> Pool<T> {").is_none());
        let h = parse_impl_header("impl<P: DerefMut> AsyncWrite for Pin<P> {").unwrap();
        assert_eq!((h.self_.as_str(), h.self_args.clone(), h.params.clone()), ("Pin", vec!["P".to_string()], vec!["P".to_string()]));
    }
}
