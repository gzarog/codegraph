//! The CodeGraph documentation website: Markdown pages under `site/content`,
//! ordered by `site/nav.json`, rendered into a static site with a sidebar,
//! per-page table of contents, client-side search and light/dark themes.
//! The build fails on a link to a page that doesn't exist.

pub mod markdown;

use codegraph_viewer::vdom::escape_html;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub const REPO_URL: &str = "https://github.com/gzarog/codegraph";

#[derive(Debug, Clone)]
pub struct NavItem {
    pub slug: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct NavGroup {
    /// None for a top-level item without a heading.
    pub label: Option<String>,
    pub items: Vec<NavItem>,
}

pub fn read_nav(site: &Path) -> Result<Vec<NavGroup>, String> {
    let text =
        std::fs::read_to_string(site.join("nav.json")).map_err(|e| format!("nav.json: {e}"))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("nav.json: {e}"))?;
    let item = |x: &Value| -> Option<NavItem> {
        Some(NavItem {
            slug: x.get("slug")?.as_str()?.to_string(),
            label: x.get("label")?.as_str()?.to_string(),
        })
    };
    let mut groups = Vec::new();
    for g in v.as_array().ok_or("nav.json must be an array")? {
        match g.get("items").and_then(Value::as_array) {
            Some(items) => groups.push(NavGroup {
                label: g.get("label").and_then(Value::as_str).map(str::to_string),
                items: items.iter().filter_map(item).collect(),
            }),
            None => groups.push(NavGroup {
                label: None,
                items: item(g).into_iter().collect(),
            }),
        }
    }
    Ok(groups)
}

pub struct Report {
    pub pages: usize,
    pub out: PathBuf,
}

fn head(title: &str, description: &str, base: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<meta name="description" content="{description}">
<link rel="icon" href="{base}/favicon.svg" type="image/svg+xml">
<link rel="stylesheet" href="{base}/site.css">
<script>try{{var t=localStorage.getItem('cg-theme');if(t)document.documentElement.dataset.theme=t}}catch(e){{}}</script>
<script src="{base}/site.js" defer></script>
</head>
"#,
        title = escape_html(title),
        description = escape_html(description),
    )
}

fn header(base: &str, with_menu: bool) -> String {
    format!(
        r#"<header class="top">
  {menu}<a class="brand" href="{base}/"><span class="logo" aria-hidden="true">◆</span>codegraph</a>
  <div class="search">
    <input type="search" id="search" placeholder="Search the docs" aria-label="Search the docs" autocomplete="off" data-base="{base}">
    <div class="results" id="search-results" role="listbox" hidden></div>
  </div>
  <nav class="links" aria-label="Site">
    <a href="{base}/getting-started/introduction/">Docs</a>
    <a href="{base}/reference/cli/">CLI</a>
    <a href="{REPO_URL}" rel="noopener">GitHub</a>
  </nav>
  <button class="theme" type="button" id="theme" aria-label="Toggle dark mode"><svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true"><path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" fill="currentColor"/></svg></button>
</header>
"#,
        menu = if with_menu {
            r#"<button class="menu" type="button" id="menu" aria-label="Open navigation" aria-expanded="false">☰</button>"#
        } else {
            ""
        }
    )
}

fn sidebar(nav: &[NavGroup], current: &str, base: &str) -> String {
    let mut out = String::from("<nav class=\"side\" id=\"side\" aria-label=\"Documentation\">");
    for g in nav {
        if let Some(l) = &g.label {
            let _ = write!(out, "<p class=\"group\">{}</p>", escape_html(l));
        }
        out.push_str("<ul>");
        for it in &g.items {
            let _ = write!(
                out,
                "<li><a href=\"{base}/{}/\"{}>{}</a></li>",
                it.slug,
                if it.slug == current {
                    " aria-current=\"page\""
                } else {
                    ""
                },
                escape_html(&it.label)
            );
        }
        out.push_str("</ul>");
    }
    out.push_str("</nav>");
    out
}

fn toc(headings: &[(u8, String, String)]) -> String {
    if headings.len() < 2 {
        return String::new();
    }
    let mut out = String::from(
        "<aside class=\"toc\" aria-label=\"On this page\"><p class=\"group\">On this page</p><ul>",
    );
    for (level, id, text) in headings {
        let _ = write!(
            out,
            "<li class=\"l{level}\"><a href=\"#{id}\">{}</a></li>",
            escape_html(text)
        );
    }
    out.push_str("</ul></aside>");
    out
}

fn footer() -> String {
    format!(
        "<footer class=\"foot\"><p>CodeGraph is MIT licensed. A Rust port of <a href=\"https://github.com/colbymchenry/codegraph\" rel=\"noopener\">colbymchenry/codegraph</a>. <a href=\"{REPO_URL}\" rel=\"noopener\">Source</a>.</p></footer>"
    )
}

/// Build the site from `site` into `out`, with every URL under `base`
/// (`""` at a domain root, `/codegraph` for a project page).
pub fn build(site: &Path, out: &Path, base: &str) -> Result<Report, String> {
    let base = base.trim_end_matches('/');
    let nav = read_nav(site)?;
    let order: Vec<&NavItem> = nav.iter().flat_map(|g| &g.items).collect();
    let known: HashSet<String> = order.iter().map(|i| format!("/{}/", i.slug)).collect();
    let section_of = |slug: &str| {
        nav.iter()
            .find(|g| g.items.iter().any(|i| i.slug == slug))
            .and_then(|g| g.label.clone())
            .unwrap_or_default()
    };
    prepare_out(out)?;
    let mut broken = Vec::new();
    let mut search = Vec::new();
    for (idx, item) in order.iter().enumerate() {
        let path = site.join("content").join(format!("{}.md", item.slug));
        let src = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let doc = markdown::render(&src, base);
        for l in &doc.links {
            if let Some(p) = l.strip_prefix('/') {
                let page = format!("/{}", p.split('#').next().unwrap_or(""));
                let asset = site.join("static").join(p);
                if page != "/" && !known.contains(&page) && !asset.is_file() {
                    broken.push(format!("{}: {l}", item.slug));
                }
            }
        }
        let title = if doc.title.is_empty() {
            item.label.clone()
        } else {
            doc.title.clone()
        };
        let prev = idx.checked_sub(1).and_then(|i| order.get(i));
        let next = order.get(idx + 1);
        let pager = format!(
            "<nav class=\"pager\" aria-label=\"Pages\">{}{}</nav>",
            prev.map(|p| format!(
                "<a class=\"prev\" href=\"{base}/{}/\"><span>Previous</span>{}</a>",
                p.slug,
                escape_html(&p.label)
            ))
            .unwrap_or_default(),
            next.map(|n| format!(
                "<a class=\"next\" href=\"{base}/{}/\"><span>Next</span>{}</a>",
                n.slug,
                escape_html(&n.label)
            ))
            .unwrap_or_default(),
        );
        let section = section_of(&item.slug);
        let html = format!(
            "{head}<body class=\"doc\">\n{header}<div class=\"layout\">{side}<main id=\"main\"><article>{crumb}<h1>{title}</h1>{desc}{body}<p class=\"edit\"><a href=\"{REPO_URL}/edit/main/site/content/{slug}.md\" rel=\"noopener\">Edit this page</a></p></article>{pager}</main>{toc}</div>{foot}</body></html>\n",
            head = head(&format!("{title} · CodeGraph"), &doc.description, base),
            header = header(base, true),
            side = sidebar(&nav, &item.slug, base),
            crumb = if section.is_empty() { String::new() } else { format!("<p class=\"crumb\">{}</p>", escape_html(&section)) },
            title = escape_html(&title),
            desc = if doc.description.is_empty() { String::new() } else { format!("<p class=\"lede\">{}</p>", escape_html(&doc.description)) },
            body = doc.html,
            slug = item.slug,
            toc = toc(&doc.headings),
            foot = footer(),
        );
        let dir = out.join(&item.slug);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("index.html"), html).map_err(|e| e.to_string())?;
        let text: String = doc.text.chars().take(4000).collect();
        search.push(json!({
            "title": title,
            "url": format!("{base}/{}/", item.slug),
            "section": section,
            "headings": doc.headings.iter().map(|(_, id, t)| json!({ "id": id, "text": t })).collect::<Vec<_>>(),
            "text": text,
        }));
    }
    if !broken.is_empty() {
        return Err(format!("broken links:\n  {}", broken.join("\n  ")));
    }
    // Landing page.
    let landing = std::fs::read_to_string(site.join("landing.html"))
        .map_err(|e| format!("landing.html: {e}"))?;
    let landing = landing
        .replace("{{base}}", base)
        .replace("{{repo}}", REPO_URL);
    std::fs::write(
        out.join("index.html"),
        format!(
            "{}<body class=\"landing\">\n{}{landing}{}</body></html>\n",
            head(
                "CodeGraph: a code knowledge graph for AI agents",
                "A local-first knowledge graph of your codebase for AI coding agents: one binary that indexes, serves MCP, and shows the graph in your browser.",
                base
            ),
            header(base, false),
            footer()
        ),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(
        out.join("404.html"),
        format!(
            "{}<body class=\"doc\">\n{}<main class=\"notfound\"><h1>Page not found</h1><p>That page doesn't exist. Try the search above or <a href=\"{base}/getting-started/introduction/\">start at the introduction</a>.</p></main>{}</body></html>\n",
            head("Not found · CodeGraph", "", base),
            header(base, false),
            footer()
        ),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(out.join("search.json"), Value::Array(search).to_string())
        .map_err(|e| e.to_string())?;
    copy_dir(&site.join("static"), out).map_err(|e| e.to_string())?;
    Ok(Report {
        pages: order.len(),
        out: out.to_path_buf(),
    })
}

const MARKER: &str = ".codegraph-site";

/// Empty `out` for a fresh build, but only if it's missing, empty, or a
/// previous build's output (marked); never someone's unrelated directory.
fn prepare_out(out: &Path) -> Result<(), String> {
    if out.exists() {
        let empty = std::fs::read_dir(out)
            .map_err(|e| e.to_string())?
            .next()
            .is_none();
        if !empty && !out.join(MARKER).is_file() {
            return Err(format!(
                "{} is not empty and wasn't made by this generator; pick another --out",
                out.display()
            ));
        }
        std::fs::remove_dir_all(out).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    std::fs::write(out.join(MARKER), "").map_err(|e| e.to_string())
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let dest = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            std::fs::create_dir_all(&dest)?;
            copy_dir(&e.path(), &dest)?;
        } else {
            std::fs::copy(e.path(), dest)?;
        }
    }
    Ok(())
}

/// The repository's `site/` directory.
pub fn default_site_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../site")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real docs build, and every internal link resolves.
    #[test]
    fn the_docs_build_without_broken_links() {
        let out = tempfile::tempdir().unwrap();
        let report = build(&default_site_dir(), out.path(), "/codegraph").unwrap();
        assert!(report.pages >= 15);
        let quick =
            std::fs::read_to_string(out.path().join("getting-started/quickstart/index.html"))
                .unwrap();
        assert!(quick.contains("aria-current=\"page\""));
        assert!(out.path().join("search.json").is_file());
        assert!(out.path().join("site.css").is_file());
    }

    #[test]
    fn reports_broken_links() {
        let site = tempfile::tempdir().unwrap();
        let p = site.path();
        std::fs::create_dir_all(p.join("content")).unwrap();
        std::fs::create_dir_all(p.join("static")).unwrap();
        std::fs::write(p.join("nav.json"), r#"[{"slug": "a", "label": "A"}]"#).unwrap();
        std::fs::write(p.join("content/a.md"), "See [b](/b/).\n").unwrap();
        std::fs::write(p.join("landing.html"), "<main></main>").unwrap();
        let out = tempfile::tempdir().unwrap();
        let err = build(p, out.path(), "").err().unwrap();
        assert!(err.contains("a: /b/"), "{err}");
        // An unrelated, non-empty directory is never wiped.
        let mine = tempfile::tempdir().unwrap();
        std::fs::write(mine.path().join("keep.txt"), "x").unwrap();
        assert!(build(p, mine.path(), "")
            .err()
            .unwrap()
            .contains("not empty"));
        assert!(mine.path().join("keep.txt").is_file());
    }
}
