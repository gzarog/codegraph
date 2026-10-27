//! A small Markdown renderer for the docs: front matter, ATX headings,
//! paragraphs, fenced code (highlighted), block quotes (with `**Note:**`
//! style asides), nested lists, pipe tables, rules and raw HTML blocks;
//! inline code, links, images, emphasis and autolinks.

use codegraph_viewer::highlight::snippet_html;
use codegraph_viewer::vdom::escape_html;

/// A rendered page.
#[derive(Debug, Default)]
pub struct Doc {
    pub title: String,
    pub description: String,
    pub html: String,
    /// `(level, id, text)` of h2/h3 headings, for the table of contents.
    pub headings: Vec<(u8, String, String)>,
    /// Every link target, for checking.
    pub links: Vec<String>,
    /// Plain text, for search.
    pub text: String,
}

pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if (c == ' ' || c == '-' || c == '_') && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// Split `---` front matter (simple `key: value` lines) from the body.
fn front_matter(src: &str) -> (Vec<(String, String)>, &str) {
    let Some(rest) = src.strip_prefix("---\n") else {
        return (Vec::new(), src);
    };
    let Some(end) = rest.find("\n---\n") else {
        return (Vec::new(), src);
    };
    let fields = rest[..end]
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        })
        .collect();
    (fields, &rest[end + 5..])
}

// ------------------------------------------------------------------ inline

/// Render inline Markdown; `base` prefixes root-relative links.
pub fn inline(s: &str, base: &str, links: &mut Vec<String>) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    let mut text_start = 0;
    let flush = |out: &mut String, from: usize, to: usize| out.push_str(&escape_html(&s[from..to]));
    while i < b.len() {
        match b[i] {
            b'\\' if i + 1 < b.len() && b[i + 1].is_ascii_punctuation() => {
                flush(&mut out, text_start, i);
                out.push_str(&escape_html(&s[i + 1..i + 2]));
                i += 2;
                text_start = i;
            }
            b'`' => {
                let ticks = s[i..].bytes().take_while(|&c| c == b'`').count();
                let fence = &s[i..i + ticks];
                if let Some(end) = s[i + ticks..].find(fence) {
                    flush(&mut out, text_start, i);
                    let code = s[i + ticks..i + ticks + end].trim();
                    out.push_str(&format!("<code>{}</code>", escape_html(code)));
                    i += ticks + end + ticks;
                    text_start = i;
                } else {
                    i += ticks;
                }
            }
            b'!' | b'[' => {
                let image = b[i] == b'!';
                let open = if image { i + 1 } else { i };
                if image && b.get(open) != Some(&b'[') {
                    i += 1;
                    continue;
                }
                match parse_link(s, open) {
                    Some((label, url, end)) => {
                        flush(&mut out, text_start, i);
                        let href = rewrite(&url, base);
                        links.push(url.clone());
                        if image {
                            out.push_str(&format!(
                                "<img src=\"{}\" alt=\"{}\" loading=\"lazy\">",
                                escape_html(&href),
                                escape_html(&label)
                            ));
                        } else {
                            let external =
                                url.starts_with("http://") || url.starts_with("https://");
                            out.push_str(&format!(
                                "<a href=\"{}\"{}>{}</a>",
                                escape_html(&href),
                                if external { " rel=\"noopener\"" } else { "" },
                                inline(&label, base, links)
                            ));
                        }
                        i = end;
                        text_start = i;
                    }
                    None => i += 1,
                }
            }
            b'<' if s[i..].starts_with("<http") => match s[i..].find('>') {
                Some(end) => {
                    flush(&mut out, text_start, i);
                    let url = &s[i + 1..i + end];
                    out.push_str(&format!(
                        "<a href=\"{0}\" rel=\"noopener\">{0}</a>",
                        escape_html(url)
                    ));
                    i += end + 1;
                    text_start = i;
                }
                None => i += 1,
            },
            b'*' | b'_' => {
                let double = b.get(i + 1) == Some(&b[i]);
                let marker = if double { &s[i..i + 2] } else { &s[i..i + 1] };
                let left_ok = b
                    .get(i + marker.len())
                    .is_some_and(|c| !c.is_ascii_whitespace());
                // `_` inside words (snake_case) is not emphasis.
                let in_word = b[i] == b'_' && i > 0 && b[i - 1].is_ascii_alphanumeric();
                let close = s[i + marker.len()..].find(marker).filter(|&p| p > 0);
                match (left_ok && !in_word, close) {
                    (true, Some(p)) => {
                        flush(&mut out, text_start, i);
                        let inner = &s[i + marker.len()..i + marker.len() + p];
                        let tag = if double { "strong" } else { "em" };
                        out.push_str(&format!("<{tag}>{}</{tag}>", inline(inner, base, links)));
                        i += marker.len() * 2 + p;
                        text_start = i;
                    }
                    _ => i += marker.len(),
                }
            }
            _ => i += 1,
        }
    }
    flush(&mut out, text_start, b.len());
    out
}

/// `[label](url)` starting at `open`: `(label, url, end)`.
fn parse_link(s: &str, open: usize) -> Option<(String, String, usize)> {
    let b = s.as_bytes();
    let mut depth = 0;
    let mut close = None;
    for (j, &c) in b.iter().enumerate().skip(open) {
        match c {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(j);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    if b.get(close + 1) != Some(&b'(') {
        return None;
    }
    let end = s[close + 2..].find(')')? + close + 2;
    let url = s[close + 2..end]
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    Some((s[open + 1..close].to_string(), url, end + 1))
}

/// Root-relative links (`/guides/x/`) live under the site's base path.
pub fn rewrite(url: &str, base: &str) -> String {
    if url.starts_with('/') && !url.starts_with("//") {
        format!("{}{url}", base.trim_end_matches('/'))
    } else {
        url.to_string()
    }
}

fn strip_inline(s: &str) -> String {
    let mut ignored = Vec::new();
    let html = inline(s, "", &mut ignored);
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

// ------------------------------------------------------------------ blocks

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// `(ordered, content)` when `line` starts a list item.
fn list_item(line: &str) -> Option<(bool, &str)> {
    let t = line.trim_start();
    if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
        return Some((false, rest));
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && t[digits..].starts_with(". ") {
        return Some((true, &t[digits + 2..]));
    }
    None
}

fn table_cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|c| c.trim().to_string())
        .collect()
}

fn is_table_rule(line: &str) -> bool {
    let t = line.trim();
    t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

struct Renderer<'a> {
    base: &'a str,
    doc: Doc,
}

impl Renderer<'_> {
    fn inline(&mut self, s: &str) -> String {
        inline(s, self.base, &mut self.doc.links)
    }

    fn text(&mut self, s: &str) {
        self.doc.text.push_str(&strip_inline(s));
        self.doc.text.push(' ');
    }

    /// Render a list starting at `lines[i]`; returns the next line index.
    fn list(&mut self, lines: &[&str], mut i: usize, out: &mut String) -> usize {
        let indent = indent_of(lines[i]);
        let ordered = list_item(lines[i]).map(|(o, _)| o).unwrap_or(false);
        out.push_str(if ordered { "<ol>" } else { "<ul>" });
        while i < lines.len() {
            let line = lines[i];
            let Some((_, first)) =
                list_item(line).filter(|(o, _)| indent_of(line) == indent && *o == ordered)
            else {
                break;
            };
            let mut body = vec![first.to_string()];
            let mut nested = String::new();
            i += 1;
            while i < lines.len() {
                let l = lines[i];
                if l.trim().is_empty() {
                    // A blank line ends the item unless indented content follows.
                    if lines
                        .get(i + 1)
                        .is_some_and(|n| indent_of(n) > indent && !n.trim().is_empty())
                    {
                        i += 1;
                        continue;
                    }
                    break;
                }
                if indent_of(l) > indent && list_item(l).is_some() {
                    i = self.list(lines, i, &mut nested);
                    continue;
                }
                if indent_of(l) > indent && list_item(l).is_none() {
                    body.push(l.trim().to_string());
                    i += 1;
                    continue;
                }
                break;
            }
            let joined = body.join(" ");
            self.text(&joined);
            out.push_str(&format!("<li>{}{nested}</li>", self.inline(&joined)));
            while i < lines.len() && lines[i].trim().is_empty() {
                // Items separated by blank lines still belong to this list.
                if lines
                    .get(i + 1)
                    .and_then(|n| list_item(n).map(|(o, _)| (o, indent_of(n))))
                    == Some((ordered, indent))
                {
                    i += 1;
                } else {
                    break;
                }
            }
        }
        out.push_str(if ordered { "</ol>" } else { "</ul>" });
        i
    }

    fn blocks(&mut self, src: &str) -> String {
        let lines: Vec<&str> = src.lines().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            let t = line.trim();
            if t.is_empty() {
                i += 1;
            } else if let Some(fence) = ["```", "~~~"].iter().find(|f| t.starts_with(**f)) {
                let lang = t[fence.len()..].trim().to_string();
                let mut code = Vec::new();
                i += 1;
                while i < lines.len() && !lines[i].trim_start().starts_with(fence) {
                    code.push(lines[i]);
                    i += 1;
                }
                i += 1;
                let code = code.join("\n");
                self.doc.text.push_str(&code);
                self.doc.text.push(' ');
                let body = snippet_html(&code, &lang).join("\n");
                out.push_str(&format!(
                    "<div class=\"code\"><button class=\"copy\" type=\"button\" aria-label=\"Copy code\">Copy</button><pre data-lang=\"{}\"><code>{body}</code></pre></div>",
                    escape_html(&lang)
                ));
            } else if t.starts_with('#') && t.trim_start_matches('#').starts_with(' ') {
                let level = t.bytes().take_while(|&c| c == b'#').count().min(6) as u8;
                let text = t[level as usize..].trim();
                let id = slug(&strip_inline(text));
                self.text(text);
                if level == 2 || level == 3 {
                    self.doc
                        .headings
                        .push((level, id.clone(), strip_inline(text)));
                }
                let html = self.inline(text);
                out.push_str(&format!(
                    "<h{level} id=\"{id}\"><a class=\"anchor\" href=\"#{id}\" aria-hidden=\"true\">#</a>{html}</h{level}>"
                ));
                i += 1;
            } else if t == "---" || t == "***" {
                out.push_str("<hr>");
                i += 1;
            } else if t.starts_with('>') {
                let mut quote = Vec::new();
                while i < lines.len() && lines[i].trim_start().starts_with('>') {
                    let q = lines[i].trim_start()[1..]
                        .strip_prefix(' ')
                        .unwrap_or(&lines[i].trim_start()[1..]);
                    quote.push(q);
                    i += 1;
                }
                let inner = quote.join("\n");
                let kind = ["Note", "Tip", "Warning", "Caution"]
                    .iter()
                    .find(|k| inner.starts_with(&format!("**{k}:**")))
                    .map(|k| k.to_lowercase());
                let body = self.blocks(&inner);
                match kind {
                    Some(k) => {
                        out.push_str(&format!("<aside class=\"callout {k}\">{body}</aside>"))
                    }
                    None => out.push_str(&format!("<blockquote>{body}</blockquote>")),
                }
            } else if list_item(line).is_some() {
                i = self.list(&lines, i, &mut out);
            } else if t.starts_with('|') && lines.get(i + 1).is_some_and(|n| is_table_rule(n)) {
                let head = table_cells(line);
                let aligns: Vec<&str> = table_cells(lines[i + 1])
                    .iter()
                    .map(|c| match (c.starts_with(':'), c.ends_with(':')) {
                        (true, true) => " style=\"text-align:center\"",
                        (false, true) => " style=\"text-align:right\"",
                        _ => "",
                    })
                    .collect();
                i += 2;
                out.push_str("<div class=\"table\"><table><thead><tr>");
                for (j, h) in head.iter().enumerate() {
                    self.text(h);
                    let cell = self.inline(h);
                    out.push_str(&format!(
                        "<th{}>{cell}</th>",
                        aligns.get(j).copied().unwrap_or("")
                    ));
                }
                out.push_str("</tr></thead><tbody>");
                while i < lines.len() && lines[i].trim().starts_with('|') {
                    out.push_str("<tr>");
                    for (j, c) in table_cells(lines[i]).iter().enumerate() {
                        self.text(c);
                        let cell = self.inline(c);
                        out.push_str(&format!(
                            "<td{}>{cell}</td>",
                            aligns.get(j).copied().unwrap_or("")
                        ));
                    }
                    out.push_str("</tr>");
                    i += 1;
                }
                out.push_str("</tbody></table></div>");
            } else if t.starts_with('<') && !t.starts_with("<http") {
                // Raw HTML runs to the next blank line.
                while i < lines.len() && !lines[i].trim().is_empty() {
                    out.push_str(lines[i]);
                    out.push('\n');
                    i += 1;
                }
            } else {
                let mut para = Vec::new();
                while i < lines.len() {
                    let l = lines[i].trim();
                    if l.is_empty()
                        || l.starts_with("```")
                        || l.starts_with('>')
                        || (l.starts_with('#') && l.trim_start_matches('#').starts_with(' '))
                        || list_item(lines[i]).is_some()
                        || (l.starts_with('|')
                            && lines.get(i + 1).is_some_and(|n| is_table_rule(n)))
                    {
                        break;
                    }
                    para.push(l);
                    i += 1;
                }
                let joined = para.join(" ");
                self.text(&joined);
                let html = self.inline(&joined);
                out.push_str(&format!("<p>{html}</p>"));
            }
        }
        out
    }
}

/// Render a page; `base` is the site's path prefix (`/codegraph`).
pub fn render(src: &str, base: &str) -> Doc {
    let (fields, body) = front_matter(src);
    let mut r = Renderer {
        base,
        doc: Doc::default(),
    };
    for (k, v) in fields {
        match k.as_str() {
            "title" => r.doc.title = v,
            "description" => r.doc.description = v,
            _ => {}
        }
    }
    r.doc.html = r.blocks(body);
    r.doc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_common_blocks() {
        let src = "---\ntitle: Hello\ndescription: A page\n---\n\n## Get *started*\n\nRun `codegraph init` in [your project](/guides/indexing/) or see <https://x.dev>.\n\n- one\n- two\n  - nested\n\n1. first\n2. second\n\n> **Note:** careful\n\n| A | B |\n|---|--:|\n| `x` | 2 |\n\n```bash\ncodegraph init # go\n```\n";
        let d = render(src, "/codegraph");
        assert_eq!(d.title, "Hello");
        assert!(d.html.contains("<h2 id=\"get-started\">"), "{}", d.html);
        assert!(d.html.contains("<em>started</em>"));
        assert!(
            d.html
                .contains("<a href=\"/codegraph/guides/indexing/\">your project</a>"),
            "{}",
            d.html
        );
        assert!(d
            .html
            .contains("<a href=\"https://x.dev\" rel=\"noopener\">https://x.dev</a>"));
        assert!(
            d.html
                .contains("<ul><li>one</li><li>two<ul><li>nested</li></ul></li></ul>"),
            "{}",
            d.html
        );
        assert!(d.html.contains("<ol><li>first</li><li>second</li></ol>"));
        assert!(d.html.contains("<aside class=\"callout note\">"));
        assert!(d.html.contains("<td style=\"text-align:right\">2</td>"));
        assert!(
            d.html.contains("<span class=\"c\"># go</span>"),
            "{}",
            d.html
        );
        assert_eq!(d.links, vec!["/guides/indexing/".to_string()]);
        assert_eq!(
            d.headings,
            vec![(2, "get-started".into(), "Get started".into())]
        );
        assert!(d.text.contains("Run codegraph init in your project"));
    }

    #[test]
    fn snake_case_is_not_emphasis_and_html_is_escaped() {
        let mut l = Vec::new();
        assert_eq!(
            inline("call codegraph_explore <b> a_b_c *x*", "", &mut l),
            "call codegraph_explore &lt;b&gt; a_b_c <em>x</em>"
        );
    }
}
