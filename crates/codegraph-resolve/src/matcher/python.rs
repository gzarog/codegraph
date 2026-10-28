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
