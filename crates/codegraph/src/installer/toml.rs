//! Replace or remove one `[table]` in a TOML file, keeping the rest verbatim.

/// The header name of a table line (`[a.b]` → `a.b`); `None` for other lines
/// and for array-of-tables headers.
fn table_header(line: &str) -> Option<&str> {
    let t = line.trim();
    if t.starts_with("[[") {
        return None;
    }
    let inner = t.strip_prefix('[')?.split(']').next()?;
    Some(inner.trim())
}

fn is_any_header(line: &str) -> bool {
    line.trim_start().starts_with('[')
}

/// Line range `[start, end)` of `header`'s table, plus its sub-tables
/// (`header.x`), which belong to the same definition.
fn table_range(lines: &[&str], header: &str) -> Option<(usize, usize)> {
    let start = lines.iter().position(|l| table_header(l) == Some(header))?;
    let mut end = start + 1;
    while end < lines.len() {
        if is_any_header(lines[end])
            && !table_header(lines[end]).is_some_and(|h| h.starts_with(&format!("{header}.")))
        {
            break;
        }
        end += 1;
    }
    // Leave blank lines between this table and the next one.
    while end > start + 1 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    Some((start, end))
}

/// Set `header`'s table to `block` (a full `[header]\n...` text).
pub fn upsert_table(text: &str, header: &str, block: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let block = block.trim_end();
    match table_range(&lines, header) {
        Some((s, e)) => {
            let mut out: Vec<String> = lines[..s].iter().map(|l| l.to_string()).collect();
            out.extend(block.lines().map(str::to_string));
            out.extend(lines[e..].iter().map(|l| l.to_string()));
            out.join("\n") + "\n"
        }
        None if text.trim().is_empty() => format!("{block}\n"),
        None => format!("{}\n\n{block}\n", text.trim_end()),
    }
}

/// Remove `header`'s table. `None` when absent.
pub fn remove_table(text: &str, header: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let (s, mut e) = table_range(&lines, header)?;
    while e < lines.len() && lines[e].trim().is_empty() {
        e += 1;
    }
    let mut out: Vec<&str> = lines[..s].to_vec();
    out.extend(&lines[e..]);
    let joined = out.join("\n");
    Some(if joined.trim().is_empty() {
        String::new()
    } else {
        joined.trim_end().to_string() + "\n"
    })
}

fn toml_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// `[header]` with string / string-array values.
pub fn render_table(header: &str, command: &str, args: &[&str]) -> String {
    format!(
        "[{header}]\ncommand = {}\nargs = [{}]\n",
        toml_string(command),
        args.iter()
            .map(|a| toml_string(a))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_and_remove() {
        let src = "model = \"o3\"\n\n[mcp_servers.other]\ncommand = \"x\"\n\n[mcp_servers.codegraph]\ncommand = \"old\"\n\n[mcp_servers.codegraph.env]\nA = \"1\"\n\n[[profiles]]\nname = \"p\"\n";
        let block = render_table("mcp_servers.codegraph", "codegraph", &["serve", "--mcp"]);
        let out = upsert_table(src, "mcp_servers.codegraph", &block);
        assert!(
            out.contains("command = \"codegraph\"")
                && !out.contains("old")
                && !out.contains("A = \"1\"")
        );
        assert!(
            out.contains("[mcp_servers.other]")
                && out.contains("[[profiles]]")
                && out.starts_with("model")
        );
        assert_eq!(upsert_table(&out, "mcp_servers.codegraph", &block), out);
        let removed = remove_table(&out, "mcp_servers.codegraph").unwrap();
        assert!(!removed.contains("codegraph") && removed.contains("[[profiles]]"));
        assert_eq!(upsert_table("", "t", "[t]\na = 1\n"), "[t]\na = 1\n");
    }
}
