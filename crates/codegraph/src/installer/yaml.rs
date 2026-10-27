//! Add or remove the `codegraph` entry under a top-level block-style
//! `mcp_servers:` mapping in a YAML file (Hermes), leaving everything else as written.

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn is_content(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && !t.starts_with('#')
}

pub fn server_block(indent_by: usize) -> Vec<String> {
    let a = " ".repeat(indent_by);
    let b = " ".repeat(indent_by * 2);
    let c = " ".repeat(indent_by * 3);
    vec![
        format!("{a}codegraph:"),
        format!("{b}command: codegraph"),
        format!("{b}args:"),
        format!("{c}- serve"),
        format!("{c}- --mcp"),
        format!("{b}timeout: 120"),
        format!("{b}connect_timeout: 60"),
        format!("{b}enabled: true"),
    ]
}

/// Line range of the `codegraph:` child of the top-level `mcp_servers:` block.
fn find(lines: &[&str]) -> (Option<usize>, Option<(usize, usize)>) {
    let Some(top) = lines
        .iter()
        .position(|l| indent(l) == 0 && l.trim_end() == "mcp_servers:")
    else {
        return (None, None);
    };
    let mut i = top + 1;
    let mut child_indent = None;
    while i < lines.len() {
        let l = lines[i];
        if is_content(l) {
            let ind = indent(l);
            if ind == 0 {
                break;
            }
            let ci = *child_indent.get_or_insert(ind);
            if ind == ci && l.trim_start().starts_with("codegraph:") {
                let mut end = i + 1;
                while end < lines.len() && (!is_content(lines[end]) || indent(lines[end]) > ci) {
                    end += 1;
                }
                while end > i + 1 && lines[end - 1].trim().is_empty() {
                    end -= 1;
                }
                return (Some(top), Some((i, end)));
            }
        }
        i += 1;
    }
    (Some(top), None)
}

/// Whether `mcp_servers` exists but is written inline (`mcp_servers: {…}`), which we don't edit.
pub fn has_inline_mapping(text: &str) -> bool {
    text.lines().any(|l| {
        indent(l) == 0 && l.starts_with("mcp_servers:") && is_content(&l["mcp_servers:".len()..])
    })
}

/// Insert or replace the codegraph server. `None` when the file uses an inline mapping.
pub fn upsert(text: &str) -> Option<String> {
    if has_inline_mapping(text) {
        return None;
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    match find(&lines) {
        (Some(_), Some((s, e))) => {
            let ind = indent(lines[s]).max(1);
            out.splice(s..e, server_block(ind));
        }
        (Some(top), None) => {
            let child = lines[top + 1..]
                .iter()
                .find(|l| is_content(l))
                .map(|l| indent(l))
                .filter(|&i| i > 0)
                .unwrap_or(2);
            out.splice(top + 1..top + 1, server_block(child));
        }
        (None, _) => {
            if !out.is_empty() && !out.last().is_some_and(|l| l.trim().is_empty()) {
                out.push(String::new());
            }
            out.push("mcp_servers:".to_string());
            out.extend(server_block(2));
        }
    }
    Some(out.join("\n") + "\n")
}

/// Remove the codegraph server (and `mcp_servers:` if it is left empty).
pub fn remove(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let (Some(top), Some((s, e))) = find(&lines) else {
        return None;
    };
    let mut out: Vec<&str> = lines.clone();
    out.drain(s..e);
    let has_children = out[top + 1..]
        .iter()
        .take_while(|l| !is_content(l) || indent(l) > 0)
        .any(|l| is_content(l));
    if !has_children {
        out.remove(top);
    }
    Some(out.join("\n").trim_end().to_string() + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermes_config() {
        let src = "model: x\nmcp_servers:\n  other:\n    command: y\n";
        let out = upsert(src).unwrap();
        assert!(out.contains("  codegraph:\n    command: codegraph") && out.contains("  other:"));
        assert_eq!(upsert(&out).unwrap(), out);
        let removed = remove(&out).unwrap();
        assert_eq!(removed, src);
        let fresh = upsert("model: x\n").unwrap();
        assert!(
            fresh.ends_with("enabled: true\n") && fresh.contains("\nmcp_servers:\n  codegraph:")
        );
        assert_eq!(remove(&fresh).unwrap(), "model: x\n");
        assert!(upsert("mcp_servers: {a: 1}\n").is_none());
    }
}
