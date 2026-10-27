//! Surgical edits to JSON-with-comments files: set or remove one value by key
//! path while leaving every other byte (comments, formatting, key order)
//! untouched.

use serde_json::Value;

#[derive(Debug, Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
}

/// One member of an object: its key and the span of its value.
#[derive(Debug, Clone)]
struct Member {
    key: String,
    value: Span,
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        loop {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if self.s[self.i..].starts_with(b"//") {
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
            } else if self.s[self.i..].starts_with(b"/*") {
                self.i += 2;
                while self.i + 1 < self.s.len() && !self.s[self.i..].starts_with(b"*/") {
                    self.i += 1;
                }
                self.i = (self.i + 2).min(self.s.len());
            } else {
                return;
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        if self.s.get(self.i) != Some(&b'"') {
            return None;
        }
        let start = self.i;
        self.i += 1;
        while self.i < self.s.len() && self.s[self.i] != b'"' {
            if self.s[self.i] == b'\\' {
                self.i += 1;
            }
            self.i += 1;
        }
        self.i += 1;
        serde_json::from_slice::<String>(&self.s[start..self.i.min(self.s.len())]).ok()
    }

    /// Skip one value, returning its span.
    fn value(&mut self) -> Option<Span> {
        self.skip_ws();
        let start = self.i;
        match self.s.get(self.i)? {
            b'"' => {
                self.string()?;
            }
            b'{' | b'[' => {
                let (open, close) = if self.s[self.i] == b'{' {
                    (b'{', b'}')
                } else {
                    (b'[', b']')
                };
                let mut depth = 0;
                while self.i < self.s.len() {
                    match self.s[self.i] {
                        b'"' => {
                            self.string()?;
                            continue;
                        }
                        b'/' if self.s[self.i..].starts_with(b"//")
                            || self.s[self.i..].starts_with(b"/*") =>
                        {
                            self.skip_ws();
                            continue;
                        }
                        c if c == open => depth += 1,
                        c if c == close => {
                            depth -= 1;
                            if depth == 0 {
                                self.i += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    self.i += 1;
                }
            }
            _ => {
                while self.i < self.s.len()
                    && !matches!(self.s[self.i], b',' | b'}' | b']')
                    && !self.s[self.i].is_ascii_whitespace()
                {
                    self.i += 1;
                }
            }
        }
        Some(Span { start, end: self.i })
    }

    /// Members of the object starting at `self.i` (which must be `{`).
    fn members(&mut self) -> Option<Vec<Member>> {
        self.skip_ws();
        if self.s.get(self.i) != Some(&b'{') {
            return None;
        }
        self.i += 1;
        let mut out = Vec::new();
        loop {
            self.skip_ws();
            match self.s.get(self.i)? {
                b'}' => return Some(out),
                b',' => {
                    self.i += 1;
                    continue;
                }
                b'"' => {
                    let key = self.string()?;
                    self.skip_ws();
                    if self.s.get(self.i) != Some(&b':') {
                        return None;
                    }
                    self.i += 1;
                    let value = self.value()?;
                    out.push(Member { key, value });
                }
                _ => return None,
            }
        }
    }
}

/// Parse JSONC into a `Value` (comments and trailing commas allowed).
pub fn parse(text: &str) -> Option<Value> {
    if text.trim().is_empty() {
        return Some(Value::Object(Default::default()));
    }
    let mut out = String::with_capacity(text.len());
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
                out.push_str(&text[start..i.min(b.len())]);
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            _ => {
                let ch_len = text[i..].chars().next().map(char::len_utf8).unwrap_or(1);
                out.push_str(&text[i..i + ch_len]);
                i += ch_len;
            }
        }
    }
    let re = regex::Regex::new(r",(\s*[}\]])").unwrap();
    serde_json::from_str(&re.replace_all(&out, "$1")).ok()
}

/// Indentation of the line containing byte `at`.
fn indent_at(text: &str, at: usize) -> String {
    let line_start = text[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
    text[line_start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Pretty-print `value` so it sits at `indent` (continuation lines indented).
fn render(value: &Value, indent: &str) -> String {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_default();
    pretty
        .lines()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                l.to_string()
            } else {
                format!("{indent}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Set `path` to `value`, creating intermediate objects. Returns the new text,
/// or `None` when the file isn't an object we can edit.
pub fn set(text: &str, path: &[&str], value: &Value) -> Option<String> {
    if text.trim().is_empty() {
        let mut root = value.clone();
        for key in path.iter().rev() {
            let mut o = serde_json::Map::new();
            o.insert(key.to_string(), root);
            root = Value::Object(o);
        }
        return Some(format!("{}\n", serde_json::to_string_pretty(&root).ok()?));
    }
    let b = text.as_bytes();
    let mut p = Parser { s: b, i: 0 };
    p.skip_ws();
    let mut obj_start = p.i;
    for (depth, key) in path.iter().enumerate() {
        let mut parser = Parser { s: b, i: obj_start };
        let members = parser.members()?;
        let obj_end = parser.i; // at the closing `}`
        match members.iter().find(|m| m.key == *key) {
            Some(m) if depth == path.len() - 1 => {
                let indent = indent_at(text, m.value.start);
                return Some(format!(
                    "{}{}{}",
                    &text[..m.value.start],
                    render(value, &indent),
                    &text[m.value.end..]
                ));
            }
            Some(m) => {
                if b[m.value.start] != b'{' {
                    // Replace a non-object intermediate with the nested value.
                    let mut nested = value.clone();
                    for k in path[depth + 1..].iter().rev() {
                        let mut o = serde_json::Map::new();
                        o.insert(k.to_string(), nested);
                        nested = Value::Object(o);
                    }
                    let indent = indent_at(text, m.value.start);
                    return Some(format!(
                        "{}{}{}",
                        &text[..m.value.start],
                        render(&nested, &indent),
                        &text[m.value.end..]
                    ));
                }
                obj_start = m.value.start;
            }
            None => {
                // Insert `"key": <rest of path as nested objects>` before the closing brace.
                let mut nested = value.clone();
                for k in path[depth + 1..].iter().rev() {
                    let mut o = serde_json::Map::new();
                    o.insert(k.to_string(), nested);
                    nested = Value::Object(o);
                }
                let parent_indent = indent_at(text, obj_start);
                let member_indent = match members.last() {
                    Some(last) => {
                        // Key starts before the value; find the line of the last member.
                        let key_line_start = text[..last.value.start]
                            .rfind('\n')
                            .map(|i| i + 1)
                            .unwrap_or(0);
                        text[key_line_start..]
                            .chars()
                            .take_while(|c| *c == ' ' || *c == '\t')
                            .collect::<String>()
                    }
                    None => format!("{parent_indent}  "),
                };
                let entry = format!("\"{key}\": {}", render(&nested, &member_indent));
                return Some(match members.last() {
                    Some(last) => {
                        // After the last member's value (keep comments that follow it).
                        let after = last.value.end;
                        format!(
                            "{},\n{member_indent}{entry}{}",
                            &text[..after],
                            &text[after..]
                        )
                    }
                    None => {
                        format!(
                            "{}\n{member_indent}{entry}\n{parent_indent}{}",
                            &text[..obj_start + 1],
                            &text[obj_end..]
                        )
                    }
                });
            }
        }
    }
    None
}

/// Remove the member at `path`. Returns `None` when it isn't there.
pub fn remove(text: &str, path: &[&str]) -> Option<String> {
    let b = text.as_bytes();
    let mut p = Parser { s: b, i: 0 };
    p.skip_ws();
    let mut obj_start = p.i;
    for (depth, key) in path.iter().enumerate() {
        let mut parser = Parser { s: b, i: obj_start };
        let members = parser.members()?;
        let idx = members.iter().position(|m| m.key == *key)?;
        let m = &members[idx];
        if depth < path.len() - 1 {
            if b[m.value.start] != b'{' {
                return None;
            }
            obj_start = m.value.start;
            continue;
        }
        // The member runs from its key's opening quote to the end of its value.
        let key_start = text[..m.value.start]
            .rfind(&format!("\"{}\"", key.replace('"', "\\\"")))
            .unwrap_or(m.value.start);
        let (mut start, mut end) = (key_start, m.value.end);
        // Swallow one adjacent comma: the following one, else the preceding one.
        let rest = &text[end..];
        let next_non_ws = rest.find(|c: char| !c.is_whitespace()).map(|i| end + i);
        if let Some(i) = next_non_ws.filter(|&i| b[i] == b',') {
            end = i + 1;
        } else if idx > 0 {
            let before = &text[..start];
            if let Some(i) = before.rfind(',') {
                if before[i + 1..].trim().is_empty() {
                    start = i;
                }
            }
        }
        // And the whitespace up to the start of the member's line.
        let line_start = text[..start].rfind('\n').unwrap_or(start);
        if text[line_start..start].trim().is_empty() {
            start = line_start;
        }
        return Some(format!("{}{}", &text[..start], &text[end..]));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn set_preserves_comments_and_siblings() {
        let src = "{\n  // my servers\n  \"mcpServers\": {\n    \"other\": { \"command\": \"x\" } // keep\n  },\n  \"theme\": \"dark\"\n}\n";
        let out = set(
            src,
            &["mcpServers", "codegraph"],
            &json!({"command": "codegraph"}),
        )
        .unwrap();
        assert!(
            out.contains("// my servers")
                && out.contains("// keep")
                && out.contains("\"theme\": \"dark\"")
        );
        let v = parse(&out).unwrap();
        assert_eq!(v["mcpServers"]["codegraph"]["command"], "codegraph");
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        // Replacing is idempotent.
        let again = set(
            &out,
            &["mcpServers", "codegraph"],
            &json!({"command": "codegraph"}),
        )
        .unwrap();
        assert_eq!(again, out);
        // Removing restores the original structure.
        let removed = remove(&out, &["mcpServers", "codegraph"]).unwrap();
        let v = parse(&removed).unwrap();
        assert!(v["mcpServers"].get("codegraph").is_none());
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert!(removed.contains("// keep"));
    }

    #[test]
    fn creates_missing_paths() {
        let out = set(
            "",
            &["mcp", "servers", "codegraph"],
            &json!({"type": "local"}),
        )
        .unwrap();
        assert_eq!(
            parse(&out).unwrap()["mcp"]["servers"]["codegraph"]["type"],
            "local"
        );
        let out = set("{}", &["servers", "codegraph"], &json!(1)).unwrap();
        assert_eq!(parse(&out).unwrap()["servers"]["codegraph"], 1);
        let out = set("{ \"a\": 1 }", &["b", "c"], &json!(true)).unwrap();
        let v = parse(&out).unwrap();
        assert_eq!(
            (v["a"].clone(), v["b"]["c"].clone()),
            (json!(1), json!(true))
        );
    }
}
