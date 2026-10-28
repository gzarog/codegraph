//! What a Python name holds, read from the function (or module) that binds
//! it: a parameter's annotation, or the one value it is assigned. A binding
//! that could hold something else (a branch's second value, a loop or
//! `with` target, a rebinding) leaves the name untyped: a wrong type links
//! a wrong method, a missing one only leaves the call to the name guess.

use super::{Match, Matcher};
use crate::symbols::SymId;
use codegraph_core::{Language, NodeKind, ReferenceKind};
use regex::Regex;

/// How a line binds a name.
#[derive(Debug, PartialEq)]
pub(super) enum Binding {
    /// `x = value` / `x: T = value`, with the annotation when present.
    Assign {
        annotation: Option<String>,
        value: String,
    },
    /// A loop, `with`, comprehension, tuple or walrus target: anything.
    Other,
}

/// The bindings of `name` on `lines`.
pub(super) fn bindings(lines: &[&str], name: &str) -> Vec<Binding> {
    let n = regex::escape(name);
    let assign = Regex::new(&format!(r"^\s*{n}\s*(?::\s*([^=]+?))?\s*=([^=].*)$")).expect("assign");
    let other = Regex::new(&format!(
        r"\bfor\s+[^:]*?\b{n}\b[^:]*?\s+in\b|\bas\s+{n}\b|\b{n}\s*:=|^\s*(?:[\w.\[\]]+\s*,\s*)+{n}\s*(?:,[^=]*)?=[^=]|^\s*{n}\s*,[^=]*=[^=]"
    ))
    .expect("other");
    let mut out = Vec::new();
    for l in lines {
        if let Some(c) = assign.captures(l) {
            out.push(Binding::Assign {
                annotation: c.get(1).map(|m| m.as_str().trim().to_string()),
                value: c[2].trim().to_string(),
            });
        } else if other.is_match(l) {
            out.push(Binding::Other);
        }
    }
    out
}

/// The single type an annotation names: `T`, `"T"`, `mod.T`, `Optional[T]`.
/// A union, a generic or anything else names none.
pub(super) fn annotation_type(a: &str) -> Option<String> {
    let a = a.trim().trim_matches(['"', '\'']).trim();
    let a = a
        .strip_prefix("Optional[")
        .or_else(|| a.strip_prefix("typing.Optional["))
        .and_then(|x| x.strip_suffix(']'))
        .unwrap_or(a)
        .trim()
        .trim_matches(['"', '\'']);
    let ok = !a.is_empty()
        && a.split('.').all(|s| {
            s.chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_')
        })
        && a.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.');
    ok.then(|| a.to_string())
}

/// The callee when `value` is one whole call (`f(…)`, `mod.Cls(…)`,
/// `await f(…)`); not a chain like `Foo().data()`.
pub(super) fn whole_call(value: &str) -> Option<&str> {
    let v = value.trim();
    let v = v.strip_prefix("await ").map_or(v, str::trim);
    let open = v.find('(')?;
    let name = v[..open].trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (i, c) in v[open..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '[' | '{') => depth += 1,
            (None, ')' | ']' | '}') => {
                depth -= 1;
                if depth == 0 {
                    return (open + i == v.len() - 1).then_some(name);
                }
            }
            _ => {}
        }
    }
    None
}

/// `Site.objects.create(…)`: a Django manager call returns the model.
fn django_manager(value: &str) -> Option<&str> {
    let v = value.trim();
    let (model, rest) = v.split_once(".objects.")?;
    let ok = model.chars().next().is_some_and(|c| c.is_uppercase())
        && model.chars().all(|c| c.is_alphanumeric() || c == '_');
    let method = rest.split('(').next()?;
    (ok && matches!(
        method,
        "create" | "get" | "first" | "last" | "latest" | "earliest"
    ))
    .then_some(model)
}

impl Matcher<'_> {
    /// The body lines of the callable `scope` (not its `def` line).
    fn body_lines(&mut self, scope: SymId) -> Vec<String> {
        let s = self.syms.get(scope);
        let (file, start, end) = (s.file.clone(), s.start_line as usize, s.end_line as usize);
        let lines = self.file_lines(&file);
        lines
            .iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .cloned()
            .collect()
    }

    /// The function `scope` is nested in.
    fn enclosing_callable(&self, scope: SymId) -> Option<SymId> {
        let s = self.syms.get(scope);
        let owner = s.owner()?;
        let name = owner.rsplit("::").next()?;
        self.syms.named(name).iter().copied().find(|&i| {
            let o = self.syms.get(i);
            o.qualified_name == owner && o.file == s.file && o.kind.is_callable()
        })
    }

    /// The annotation of the parameter `name` of the callable `scope`, when
    /// it is one (`Some(None)` for an unannotated parameter).
    fn param_annotation(&mut self, scope: SymId, name: &str) -> Option<Option<String>> {
        if !self.python_param(scope, name) {
            return None;
        }
        let s = self.syms.get(scope);
        let (file, start) = (s.file.clone(), s.start_line as usize);
        let lines = self.file_lines(&file);
        let mut header = String::new();
        for l in lines.iter().skip(start.saturating_sub(1)).take(12) {
            header.push_str(l);
            header.push('\n');
            if l.trim_end().ends_with(':') {
                break;
            }
        }
        let re = Regex::new(&format!(
            r#"(?:^|[(,\s]){}\s*:\s*((?:"[^"]*"|'[^']*'|[\w.]+(?:\[[^\]]*\])?)(?:\s*\|\s*[\w.]+)*)"#,
            regex::escape(name)
        ))
        .ok()?;
        Some(re.captures(&header).map(|c| c[1].to_string()))
    }

    /// The class a Python name holds where `from` uses it: the innermost
    /// function binding it, else the module.
    pub(super) fn python_var_type(&mut self, from: SymId, file: &str, var: &str) -> Option<SymId> {
        let mut scope = Some(from);
        for _ in 0..4 {
            let Some(sc) = scope else { break };
            if !self.syms.get(sc).kind.is_callable() {
                break;
            }
            if let Some(ann) = self.param_annotation(sc, var) {
                let t = annotation_type(&ann?)?;
                return self.python_class_named(sc, file, &t);
            }
            let body = self.body_lines(sc);
            let refs: Vec<&str> = body.iter().map(String::as_str).collect();
            let found = bindings(&refs, var);
            if !found.is_empty() {
                return self.python_binding_type(sc, file, found);
            }
            scope = self.enclosing_callable(sc);
        }
        // Module level: top-level lines only.
        let lines = self.file_lines(file);
        let top: Vec<&str> = lines
            .iter()
            .map(String::as_str)
            .filter(|l| !l.starts_with([' ', '\t']))
            .collect();
        let found = bindings(&top, var);
        if found.is_empty() {
            return None;
        }
        let file_sym = self.syms.file_node(file)?;
        self.python_binding_type(file_sym, file, found)
    }

    /// The type a set of bindings agrees on: one annotation, or one value
    /// besides `None` placeholders, and no other kind of binding.
    fn python_binding_type(
        &mut self,
        scope: SymId,
        file: &str,
        found: Vec<Binding>,
    ) -> Option<SymId> {
        let mut values = Vec::new();
        for b in found {
            match b {
                Binding::Other => return None,
                Binding::Assign {
                    annotation: Some(a),
                    ..
                } => {
                    let t = annotation_type(&a)?;
                    return self.python_class_named(scope, file, &t);
                }
                Binding::Assign { value, .. } if value == "None" => {}
                Binding::Assign { value, .. } => values.push(value),
            }
        }
        let [value] = values.as_slice() else {
            return None;
        };
        if let Some(model) = django_manager(value) {
            return self.python_class_named(scope, file, model);
        }
        let callee = whole_call(value)?.to_string();
        let target = self.python_resolve_name(scope, file, &callee)?;
        match self.syms.get(target).kind {
            NodeKind::Class => Some(target),
            NodeKind::Function => self.python_return_type(target),
            _ => None,
        }
    }

    /// Whether `value` constructs a project class (`Cls(…)`), not one
    /// imported from outside (`StringIO()` from io).
    pub(super) fn python_project_class_call(
        &mut self,
        scope: SymId,
        file: &str,
        value: &str,
    ) -> bool {
        let Some(callee) = whole_call(value) else {
            return false;
        };
        let last = callee.rsplit('.').next().unwrap_or(callee);
        if !last.chars().next().is_some_and(char::is_uppercase) {
            return false;
        }
        let callee = callee.to_string();
        self.python_class_named(scope, file, &callee).is_some()
    }

    /// `name` (`Cls`, `mod.Cls`) as the project class `scope` sees.
    fn python_class_named(&mut self, scope: SymId, file: &str, name: &str) -> Option<SymId> {
        let t = self.python_resolve_name(scope, file, name)?;
        (self.syms.get(t).kind == NodeKind::Class).then_some(t)
    }

    /// Resolve a name as written in `scope`, outside the reference being
    /// resolved (its position belongs to that reference).
    fn python_resolve_name(&mut self, scope: SymId, file: &str, name: &str) -> Option<SymId> {
        if self.typing_depth > 3 {
            return None;
        }
        self.typing_depth += 1;
        let at = self.at.take();
        let m: Option<Match> =
            self.resolve(scope, file, Language::Python, ReferenceKind::Calls, name);
        self.at = at;
        self.typing_depth -= 1;
        m.map(|m| m.target)
    }

    /// The class a Python function returns: its `-> T` annotation, else the
    /// one class every `return Cls(…)` constructs.
    fn python_return_type(&mut self, f: SymId) -> Option<SymId> {
        let s = self.syms.get(f);
        let (file, start) = (s.file.clone(), s.start_line as usize);
        let lines = self.file_lines(&file);
        let mut header = String::new();
        for l in lines.iter().skip(start.saturating_sub(1)).take(12) {
            header.push_str(l);
            if l.trim_end().ends_with(':') {
                break;
            }
            header.push(' ');
        }
        static ARROW: std::sync::LazyLock<Regex> =
            std::sync::LazyLock::new(|| Regex::new(r"->\s*(.+?)\s*:\s*$").unwrap());
        if let Some(c) = ARROW.captures(&header) {
            let t = annotation_type(&c[1])?;
            return self.python_class_named(f, &file, &t);
        }
        let body = self.body_lines(f);
        let mut class: Option<String> = None;
        for l in &body {
            let Some(v) = l.trim_start().strip_prefix("return ") else {
                continue;
            };
            let callee = whole_call(v)?;
            let last = callee.rsplit('.').next().unwrap_or(callee);
            if !last.chars().next().is_some_and(char::is_uppercase) {
                return None;
            }
            if class.as_deref().is_some_and(|c| c != callee) {
                return None;
            }
            class = Some(callee.to_string());
        }
        self.python_class_named(f, &file, &class?)
    }
}

#[cfg(test)]
mod tests {
    use super::{annotation_type, bindings, whole_call, Binding};

    #[test]
    fn reads_bindings() {
        let b = bindings(&["    reg: Store = make()", "    for reg in rows:"], "reg");
        assert_eq!(
            b,
            vec![
                Binding::Assign {
                    annotation: Some("Store".into()),
                    value: "make()".into()
                },
                Binding::Other
            ]
        );
        assert_eq!(
            bindings(&["    with open(p) as reg:"], "reg"),
            vec![Binding::Other]
        );
        assert_eq!(
            bindings(&["    a, reg = 1, 2"], "reg"),
            vec![Binding::Other]
        );
        assert!(bindings(&["    reg == 1"], "reg").is_empty());
    }

    #[test]
    fn reads_values() {
        assert_eq!(whole_call("await open_registry()"), Some("open_registry"));
        assert_eq!(whole_call("loader.Cls(1, (2))"), Some("loader.Cls"));
        assert_eq!(whole_call("Foo().data()"), None);
        assert_eq!(whole_call("Store() if flag else {}"), None);
        assert_eq!(annotation_type("Optional[Store]").as_deref(), Some("Store"));
        assert_eq!(annotation_type("\"Store\"").as_deref(), Some("Store"));
        assert_eq!(annotation_type("\"Store | dict\""), None);
    }
}

/// One logical Python statement: its first line (1-based), indentation and
/// text with continuation lines joined.
#[derive(Debug, Clone)]
pub(super) struct Stmt {
    pub line: usize,
    pub indent: usize,
    pub text: String,
}

/// The logical statements of `lines` (the file's), from line `from` to
/// `to` (1-based, inclusive). Brackets and a trailing backslash continue a
/// statement; a string never spans lines unless triple-quoted, so a stray
/// quote or bracket inside one never swallows the statements after it.
pub(super) fn statements(lines: &[String], from: usize, to: usize) -> Vec<Stmt> {
    let mut out: Vec<Stmt> = Vec::new();
    let mut open: Option<Stmt> = None;
    let mut depth = 0i32;
    let mut triple: Option<&str> = None;
    for (i, raw) in lines
        .iter()
        .enumerate()
        .take(to.min(lines.len()))
        .skip(from.saturating_sub(1))
    {
        let line = raw.trim_end_matches('\r');
        if open.is_none() {
            let t = line.trim_start();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            open = Some(Stmt {
                line: i + 1,
                indent: line.len() - t.len(),
                text: String::new(),
            });
        }
        let stmt = open.as_mut().expect("open statement");
        if !stmt.text.is_empty() {
            stmt.text.push(' ');
        }
        stmt.text.push_str(line.trim());
        // Scan for brackets outside strings and comments.
        let b = line.as_bytes();
        let mut j = 0;
        let mut quote: Option<u8> = None;
        while j < b.len() {
            if let Some(t) = triple {
                if line[j..].starts_with(t) {
                    triple = None;
                    j += 3;
                } else {
                    j += 1;
                }
                continue;
            }
            let c = b[j];
            match quote {
                Some(q) => {
                    if c == b'\\' {
                        j += 1;
                    } else if c == q {
                        quote = None;
                    }
                }
                None => match c {
                    b'#' => break,
                    b'"' | b'\'' => {
                        let t = if c == b'"' { "\"\"\"" } else { "'''" };
                        if line[j..].starts_with(t) {
                            triple = Some(t);
                            j += 3;
                            continue;
                        }
                        quote = Some(c);
                    }
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => depth = (depth - 1).max(0),
                    _ => {}
                },
            }
            j += 1;
        }
        let continued = triple.is_some() || depth > 0 || line.trim_end().ends_with('\\');
        if !continued {
            out.push(open.take().expect("open statement"));
            depth = 0;
        }
    }
    out.extend(open);
    out
}

/// Statements of a body outside any `def`/`class` nested in it.
fn outside_nested(stmts: &[Stmt]) -> Vec<&Stmt> {
    let mut out = Vec::new();
    let mut nested_at: Option<usize> = None;
    for s in stmts {
        if let Some(ind) = nested_at {
            if s.indent > ind {
                continue;
            }
            nested_at = None;
        }
        let t = s.text.trim_start_matches("async ");
        if t.starts_with("def ") || t.starts_with("class ") {
            nested_at = Some(s.indent);
        }
        out.push(s);
    }
    out
}

/// Whether a statement binds `name` other than by an import: assignment
/// (also augmented, annotated, tuple), a loop/`with`/`except` target, a
/// walrus, a one-line compound's assignment, a `def` or `class`.
pub(super) fn stmt_rebinds(text: &str, name: &str) -> bool {
    let n = regex::escape(name);
    let re = Regex::new(&format!(
        r"^(?:(?:if|elif|else|while|try|except|finally|with)\b[^:]*:\s*)?(?:{n}\s*(?::[^=]+)?(?:[-+*/%&|^@]|//|>>|<<|\*\*)?=[^=]|(?:[\w.\[\]]+\s*,\s*)+{n}\s*(?:,[^=]*)?=[^=]|{n}\s*,[^=]*=[^=])|^for\s+[^:]*?\b{n}\b[^:]*?\s+in\b|^(?:with|except)\b.*\bas\s+{n}\b|\b{n}\s*:=|^(?:async\s+)?(?:def|class)\s+{n}\b"
    ))
    .expect("rebinds");
    re.is_match(text)
}

/// A statement importing `name` (`from m import name`, `import m as name`).
fn stmt_imports(text: &str, name: &str) -> bool {
    let n = regex::escape(name);
    Regex::new(&format!(
        r"^from\s+\S+\s+import\b.*(?:[\s(,]{n}\s*(?:[,)]|$)|\bas\s+{n}\b)|^import\s+.*\bas\s+{n}\b"
    ))
    .expect("imports")
    .is_match(text)
}

/// What a class body says an attribute holds.
#[derive(Debug, PartialEq)]
pub(super) enum AttrType {
    /// Every assignment agrees on this project class.
    Typed(SymId),
    /// Assigned something from outside the project.
    External,
    /// Assignments that disagree or say nothing usable.
    Contested,
    /// Never assigned.
    Unknown,
}

/// What one value evidences.
enum Evidence {
    Class(SymId),
    External,
    Unknown,
}

/// `A(…) if c else B(…)`: the two arms of a conditional expression.
fn conditional_arms(value: &str) -> Option<(&str, &str)> {
    let (a, rest) = value.split_once(" if ")?;
    let (_, b) = rest.rsplit_once(" else ")?;
    Some((a.trim(), b.trim()))
}

/// `GroupRepositoryAIProvider` → group, repository, ai, provider.
fn camel_words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.trim_start_matches('_').chars().collect();
    let mut words = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let boundary = c.is_uppercase()
            && i > 0
            && (chars[i - 1].is_lowercase()
                || chars[i - 1].is_ascii_digit()
                || chars[i - 1].is_uppercase()
                    && chars.get(i + 1).is_some_and(|n| n.is_lowercase()));
        if boundary && !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

fn singular(w: &str) -> String {
    if let Some(s) = w.strip_suffix("ies") {
        format!("{s}y")
    } else if w.ends_with("ss") {
        w.to_string()
    } else {
        w.strip_suffix('s').unwrap_or(w).to_string()
    }
}

impl Matcher<'_> {
    /// Logical statements of the symbol `s`'s body (after its header).
    fn body_statements(&mut self, s: SymId) -> Vec<Stmt> {
        let sym = self.syms.get(s);
        let (file, start, end) = (
            sym.file.clone(),
            sym.start_line as usize,
            sym.end_line as usize,
        );
        let lines = self.file_lines(&file);
        let all = statements(&lines, start, end);
        // Drop the header statement (`def f(…):` / `class C(…):`).
        all.into_iter().skip(1).collect()
    }

    /// Whether the body of the callable `scope` rebinds `name` (outside
    /// nested functions), before `line` when given.
    pub(super) fn python_rebinds(
        &mut self,
        scope: SymId,
        name: &str,
        before: Option<usize>,
    ) -> bool {
        if !self.syms.get(scope).kind.is_callable() {
            return false;
        }
        let stmts = self.body_statements(scope);
        outside_nested(&stmts)
            .iter()
            .filter(|s| before.is_none_or(|b| s.line < b))
            .any(|s| stmt_rebinds(&s.text, name))
    }

    /// The import statement in the callable `scope` (or one around it)
    /// that binds `name`, resolved.
    pub(super) fn python_local_import(
        &mut self,
        scope: SymId,
        file: &str,
        name: &str,
    ) -> Option<SymId> {
        let mut sc = Some(scope);
        for _ in 0..4 {
            let s = sc?;
            if !self.syms.get(s).kind.is_callable() {
                return None;
            }
            let stmts = self.body_statements(s);
            let hit = outside_nested(&stmts)
                .iter()
                .find(|st| stmt_imports(&st.text, name))
                .map(|st| st.text.clone());
            if let Some(text) = hit {
                let b = crate::imports::parse_import(Language::Python, &text, "")
                    .into_iter()
                    .find(|b| b.local == name)?;
                return self.resolve_binding(file, Language::Python, &b, 0);
            }
            sc = self.enclosing_callable(s);
        }
        None
    }

    /// Whether `name` is bound in `file` by importing it from outside the
    /// project (at module level or in `scope`).
    fn python_external_name(&mut self, scope: SymId, file: &str, name: &str) -> bool {
        let root = name.split('.').next().unwrap_or(name);
        let mut bindings: Vec<crate::imports::Binding> = self
            .bindings(file)
            .iter()
            .filter(|b| b.local == root || b.local.split('.').next() == Some(root))
            .cloned()
            .collect();
        if self.syms.get(scope).kind.is_callable() {
            let stmts = self.body_statements(scope);
            for st in outside_nested(&stmts) {
                if stmt_imports(&st.text, root) || st.text.starts_with("import ") {
                    bindings.extend(
                        crate::imports::parse_import(Language::Python, &st.text, "")
                            .into_iter()
                            .filter(|b| b.local == root),
                    );
                }
            }
        }
        !bindings.is_empty()
            && bindings.iter().all(|b| {
                !self
                    .modules
                    .python_project_module_any_root(self.syms, &b.module)
            })
    }

    /// What the value assigned in the method `scope` at `line` evidences.
    fn python_value_evidence(
        &mut self,
        scope: SymId,
        file: &str,
        value: &str,
        line: usize,
    ) -> Evidence {
        let value = value.trim();
        if let Some((a, b)) = conditional_arms(value) {
            let (ca, cb) = (whole_call(a), whole_call(b));
            return match (ca, cb) {
                (Some(x), Some(y)) if x == y => {
                    let x = x.to_string();
                    self.python_callee_evidence(scope, file, &x, line)
                }
                _ => Evidence::Unknown,
            };
        }
        if let Some(callee) = whole_call(value) {
            let callee = callee.to_string();
            return self.python_callee_evidence(scope, file, &callee, line);
        }
        let is_name = !value.is_empty()
            && value.chars().all(|c| c.is_alphanumeric() || c == '_')
            && !value.starts_with(|c: char| c.is_ascii_digit());
        if is_name && self.syms.get(scope).kind.is_callable() {
            if let Some(ann) = self.param_annotation(scope, value) {
                if self.python_rebinds(scope, value, Some(line)) {
                    return Evidence::Unknown;
                }
                return match ann.and_then(|a| annotation_type(&a)) {
                    Some(t) => match self.python_class_named(scope, file, &t) {
                        Some(c) => Evidence::Class(c),
                        None if self.python_external_name(scope, file, &t) => Evidence::External,
                        None => Evidence::Unknown,
                    },
                    None => Evidence::Unknown,
                };
            }
        }
        Evidence::Unknown
    }

    /// What constructing `callee` at `line` in `scope` evidences.
    fn python_callee_evidence(
        &mut self,
        scope: SymId,
        file: &str,
        callee: &str,
        line: usize,
    ) -> Evidence {
        let root = callee.split('.').next().unwrap_or(callee);
        if self.python_rebinds(scope, root, Some(line)) {
            return Evidence::Unknown;
        }
        let target = if callee == root {
            self.python_local_import(scope, file, root)
                .or_else(|| self.python_resolve_name(scope, file, callee))
        } else {
            self.python_resolve_name(scope, file, callee)
        };
        match target {
            Some(t) if self.syms.get(t).kind == NodeKind::Class => Evidence::Class(t),
            Some(_) => Evidence::Unknown,
            None if self.python_external_name(scope, file, callee) => Evidence::External,
            None => Evidence::Unknown,
        }
    }

    /// What the class `class` says its attribute `attr` holds: its class
    /// body's annotation or value, and every `self.attr = …` in its methods.
    pub(super) fn python_attr_type(&mut self, class: SymId, attr: &str) -> AttrType {
        let file = self.syms.get(class).file.clone();
        let stmts = self.body_statements(class);
        let Some(class_indent) = stmts.first().map(|s| s.indent) else {
            return AttrType::Unknown;
        };
        let a = regex::escape(attr);
        let class_ann = Regex::new(&format!(r"^{a}\s*:\s*([^=]+?)\s*(?:=.*)?$")).expect("ann");
        let class_assign = Regex::new(&format!(r"^{a}\s*=([^=].*)$")).expect("assign");
        let self_assign = Regex::new(&format!(
            r"^(?:self|cls)\.{a}\s*(?:(:[^=]+)?=([^=].*)|((?:[-+*/%&|^@]|//|>>|<<|\*\*)=.*))$"
        ))
        .expect("self assign");
        let mut evidence: Vec<Evidence> = Vec::new();
        // The method each statement is in, and whether a def nests it there.
        let mut method: Option<(SymId, usize)> = None;
        let mut nested_at: Option<usize> = None;
        for s in &stmts {
            if s.indent <= class_indent {
                method = None;
                nested_at = None;
            }
            let t = s.text.trim_start_matches("async ");
            if s.indent == class_indent {
                if let Some(def) = t.strip_prefix("def ") {
                    let name = def.split('(').next().unwrap_or("").trim().to_string();
                    let qn = format!("{}::{name}", self.syms.get(class).qualified_name);
                    let m = self.syms.named(&name).iter().copied().find(|&i| {
                        self.syms.get(i).qualified_name == qn && self.syms.get(i).file == file
                    });
                    method = m.map(|m| (m, s.indent));
                    continue;
                }
                if let Some(c) = class_ann.captures(t) {
                    match annotation_type(&c[1])
                        .and_then(|ty| self.python_class_named(class, &file, &ty))
                    {
                        Some(cls) => evidence.push(Evidence::Class(cls)),
                        None => evidence.push(Evidence::Unknown),
                    }
                } else if let Some(c) = class_assign.captures(t) {
                    let v = c[1].to_string();
                    let e = self.python_value_evidence(class, &file, &v, s.line);
                    evidence.push(e);
                }
                continue;
            }
            let Some((m, m_indent)) = method else {
                continue;
            };
            if let Some(ind) = nested_at {
                if s.indent > ind {
                    if self_assign.is_match(t) {
                        evidence.push(Evidence::Unknown);
                    }
                    continue;
                }
                nested_at = None;
            }
            if s.indent > m_indent && (t.starts_with("def ") || t.starts_with("class ")) {
                nested_at = Some(s.indent);
                continue;
            }
            if let Some(c) = self_assign.captures(t) {
                if c.get(3).is_some() {
                    evidence.push(Evidence::Unknown);
                    continue;
                }
                if let Some(ann) = c.get(1) {
                    let ann = ann.as_str().trim_start_matches(':').to_string();
                    match annotation_type(&ann)
                        .and_then(|ty| self.python_class_named(m, &file, &ty))
                    {
                        Some(cls) => evidence.push(Evidence::Class(cls)),
                        None => evidence.push(Evidence::Unknown),
                    }
                    continue;
                }
                let v = c[2].to_string();
                let e = self.python_value_evidence(m, &file, &v, s.line);
                evidence.push(e);
            }
        }
        if evidence.is_empty() {
            return AttrType::Unknown;
        }
        let mut class_seen: Option<SymId> = None;
        let mut external = false;
        for e in evidence {
            match e {
                Evidence::Unknown => return AttrType::Contested,
                Evidence::External => external = true,
                Evidence::Class(c) => {
                    if class_seen.is_some_and(|x| x != c) {
                        return AttrType::Contested;
                    }
                    class_seen = Some(c);
                }
            }
        }
        match (class_seen, external) {
            (Some(_), true) => AttrType::Contested,
            (Some(c), false) => AttrType::Typed(c),
            (None, _) => AttrType::External,
        }
    }

    /// A project method `tail` guessed from the receiver's name `link`:
    /// the class spelled like it whole (`nodelist` → NodeList), else the
    /// one whose name holds its head noun (`group_ai_providers` → …Provider).
    pub(super) fn python_name_guess(
        &mut self,
        link: &str,
        tail: &str,
        file: &str,
    ) -> Option<SymId> {
        let text = self.file_lines(file).join("\n");
        let cands: Vec<(SymId, String)> = self
            .syms
            .named_for(tail, Language::Python)
            .iter()
            .copied()
            .filter_map(|i| {
                let s = self.syms.get(i);
                if s.language != Language::Python
                    || !matches!(s.kind, NodeKind::Method | NodeKind::Property)
                {
                    return None;
                }
                let owner = s.owner()?;
                let owner = owner.rsplit("::").next().unwrap_or(owner).to_string();
                let excluded = super::python_dispatched_handler(&owner, &s.name)
                    || super::python_test_double(&owner) && !text.contains(&owner);
                (!excluded).then_some((i, owner))
            })
            .collect();
        let pick = |hits: Vec<&(SymId, String)>| -> Option<SymId> {
            let owners: std::collections::HashSet<(String, &str)> = hits
                .iter()
                .map(|(i, o)| (o.clone(), self.syms.get(*i).file.as_str()))
                .collect();
            (owners.len() == 1).then(|| hits[0].0)
        };
        let whole = link.replace('_', "").to_lowercase();
        let spelled: Vec<&(SymId, String)> = cands
            .iter()
            .filter(|(_, o)| o.trim_start_matches('_').replace('_', "").to_lowercase() == whole)
            .collect();
        if !spelled.is_empty() {
            return pick(spelled);
        }
        let noun = singular(&camel_words(link).pop()?);
        let holding: Vec<&(SymId, String)> = cands
            .iter()
            .filter(|(_, o)| camel_words(o).iter().any(|w| singular(w) == noun))
            .collect();
        if holding.is_empty() {
            return None;
        }
        pick(holding)
    }

    /// Python member calls on a dotted receiver: a module path, a typed
    /// attribute, or a guess by the last link's name. `None` leaves the
    /// reference to the generic steps.
    pub(super) fn python_member(
        &mut self,
        from: SymId,
        file: &str,
        head: &str,
        tail: &str,
    ) -> Option<Option<Match>> {
        if !head
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        {
            return None;
        }
        let segs: Vec<&str> = head.split('.').collect();
        let root = segs[0];
        let enclosing = self.enclosing_type(from);
        let class_root = matches!(root, "self" | "cls")
            || enclosing.is_some_and(|c| self.syms.get(c).name == root);
        if class_root && segs.len() >= 2 {
            let class = enclosing?;
            if segs.len() == 2 {
                match self.python_attr_type(class, segs[1]) {
                    AttrType::Typed(t) => {
                        return Some(
                            self.syms
                                .member_named(t, tail, Language::Python)
                                .map(|m| Match::new(m, "attr-type", 0.85)),
                        )
                    }
                    AttrType::External => return Some(None),
                    AttrType::Contested | AttrType::Unknown => {}
                }
            }
            let link = segs[segs.len() - 1];
            return Some(
                self.python_name_guess(link, tail, file)
                    .map(|m| Match::new(m, "receiver-name", 0.6)),
            );
        }
        if segs.len() < 2 {
            return None;
        }
        // A module path: `os.path.exists`, `utils.helpers.do_x`.
        let bindings = self.bindings(file);
        let local = self.python_local_binding(from, root);
        let binding = local.or_else(|| {
            bindings
                .iter()
                .filter(|b| {
                    head == b.local || head.starts_with(&format!("{}.", b.local)) || b.local == root
                })
                .max_by_key(|b| b.local.len())
                .cloned()
        });
        if let Some(b) = binding {
            let rest: Vec<&str> = if head == b.local || head.starts_with(&format!("{}.", b.local)) {
                head[b.local.len()..]
                    .trim_start_matches('.')
                    .split('.')
                    .filter(|s| !s.is_empty())
                    .collect()
            } else {
                segs[1..].to_vec()
            };
            let mut module = match &b.imported {
                None => b.module.clone(),
                Some(i) if b.module.ends_with('.') => format!("{}{i}", b.module),
                Some(i) => format!("{}.{i}", b.module),
            };
            for r in &rest {
                module = format!("{module}.{r}");
            }
            let files = self
                .modules
                .resolve(self.syms, file, Language::Python, &module);
            if let Some(t) = files
                .iter()
                .find_map(|f| self.find_export(f, tail, Language::Python, 1))
            {
                return Some(Some(Match::new(t, "module-path", 0.9)));
            }
            if !self
                .modules
                .python_project_module_any_root(self.syms, &b.module)
            {
                return Some(None);
            }
            return None;
        }
        // A typed local or parameter, one attribute deep.
        if let Some(t) = self.python_var_type(from, file, root) {
            if segs.len() == 2 {
                if let AttrType::Typed(c) = self.python_attr_type(t, segs[1]) {
                    return Some(
                        self.syms
                            .member_named(c, tail, Language::Python)
                            .map(|m| Match::new(m, "attr-type", 0.85)),
                    );
                }
            }
        }
        let link = segs[segs.len() - 1];
        Some(
            self.python_name_guess(link, tail, file)
                .map(|m| Match::new(m, "receiver-name", 0.6)),
        )
    }

    /// An import inside the callable `scope` binding `name`, as a binding.
    fn python_local_binding(
        &mut self,
        scope: SymId,
        name: &str,
    ) -> Option<crate::imports::Binding> {
        let mut sc = Some(scope);
        for _ in 0..4 {
            let s = sc?;
            if !self.syms.get(s).kind.is_callable() {
                return None;
            }
            let stmts = self.body_statements(s);
            for st in outside_nested(&stmts) {
                if st.text.starts_with("import ") || st.text.starts_with("from ") {
                    if let Some(b) = crate::imports::parse_import(Language::Python, &st.text, "")
                        .into_iter()
                        .find(|b| b.local == name || b.local.split('.').next() == Some(name))
                    {
                        return Some(b);
                    }
                }
            }
            sc = self.enclosing_callable(s);
        }
        None
    }
}

#[cfg(test)]
mod stmt_tests {
    use super::{camel_words, statements, stmt_rebinds};

    #[test]
    fn statements_join_and_never_swallow() {
        let lines: Vec<String> = [
            "def f():",
            "    raise ValueError(\"x = (\")",
            "    a = g(",
            "        1,",
            "    )",
            "    b = 1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let s = statements(&lines, 1, 6);
        let texts: Vec<&str> = s.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "def f():",
                "raise ValueError(\"x = (\")",
                "a = g( 1, )",
                "b = 1"
            ]
        );
    }

    #[test]
    fn rebinding_forms() {
        assert!(stmt_rebinds("Foo = load()", "Foo"));
        assert!(stmt_rebinds("if k: Foo = load()", "Foo"));
        assert!(stmt_rebinds("if (Foo := load()):", "Foo"));
        assert!(stmt_rebinds("x += 1", "x"));
        assert!(!stmt_rebinds("reg(Foo=1)", "Foo"));
        assert!(!stmt_rebinds("d[Foo] = 1", "Foo"));
        assert!(!stmt_rebinds("Foo.attr = 1", "Foo"));
        assert!(!stmt_rebinds("print(\"Foo = %s\" % 1)", "Foo"));
        assert!(!stmt_rebinds("x == 1", "x"));
    }

    #[test]
    fn words() {
        assert_eq!(
            camel_words("GroupRepositoryAIProvider"),
            ["group", "repository", "ai", "provider"]
        );
        assert_eq!(
            camel_words("group_ai_providers"),
            ["group", "ai", "providers"]
        );
        assert_eq!(
            camel_words("_FakeHTTPResponse"),
            ["fake", "http", "response"]
        );
    }
}
