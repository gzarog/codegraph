//! Formats that embed a host language: single-file components (Svelte, Vue,
//! Astro) whose `<script>` blocks are TypeScript/JavaScript, ArkTS (parsed
//! with the TypeScript grammar), and template languages whose structure is
//! their includes and component tags (Liquid, Razor).

use crate::ids::{file_node_id, node_id};
use codegraph_core::{
    now_ms, Edge, EdgeKind, ExtractionResult, Language, Node, NodeKind, ReferenceKind,
    UnresolvedReference,
};
use regex::Regex;
use std::sync::LazyLock;

static SCRIPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<script(\s[^>]*)?>(.*?)</script>").unwrap());
static TS_LANG_ATTR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\blang\s*=\s*["'](ts|typescript|tsx)["']"#).unwrap());
static COMPONENT_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([A-Z][\w]*(?:\.[A-Z][\w]*)*)[\s/>]").unwrap());
static SVELTE_EVENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bon:?[a-z]+\s*=\s*\{\s*([A-Za-z_$][\w$.]*)\s*\}").unwrap());
static VUE_EVENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:@|v-on:)[\w.:-]+\s*=\s*"\s*([A-Za-z_$][\w$.]*)\s*(?:\(|")"#).unwrap()
});
static KEBAB_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([a-z][a-z0-9]*(?:-[a-z0-9]+)+)[\s/>]").unwrap());

const SVELTE_RUNES: &[&str] = &[
    "$props",
    "$state",
    "$derived",
    "$effect",
    "$bindable",
    "$inspect",
    "$host",
    "$snippet",
];

/// `text` with every byte outside `keep` ranges blanked (newlines kept), so
/// a parse of the result reports the original line numbers.
fn mask(text: &str, keep: &[(usize, usize)]) -> String {
    let mut out: Vec<u8> = text
        .bytes()
        .map(|b| if b == b'\n' { b'\n' } else { b' ' })
        .collect();
    for &(s, e) in keep {
        out[s..e].copy_from_slice(&text.as_bytes()[s..e]);
    }
    // Blanking can split a multi-byte character at a range edge only if a
    // range boundary falls inside one, which regex match offsets never do.
    String::from_utf8(out).unwrap_or_default()
}

fn kebab_to_pascal(s: &str) -> String {
    s.split('-')
        .map(|p| {
            let mut c = p.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

fn file_stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.split('.').next().unwrap_or(name)
}

fn line_of(text: &str, offset: usize) -> u32 {
    text[..offset].matches('\n').count() as u32 + 1
}

fn reference(from: &str, name: &str, kind: ReferenceKind, line: u32) -> UnresolvedReference {
    UnresolvedReference {
        from_node_id: from.to_string(),
        reference_name: name.to_string(),
        reference_kind: kind,
        line,
        column: 0,
        file_path: None,
        language: None,
        candidates: Vec::new(),
        row_id: None,
    }
}

/// Parse the script parts of a component file and add a `component` node
/// that references the components and handlers its template uses.
pub fn extract_component(path: &str, content: &str, lang: Language) -> ExtractionResult {
    let mut keep = Vec::new();
    let mut typescript = false;
    if lang == Language::Astro {
        // Frontmatter between the leading `---` fences is TypeScript.
        if let Some(rest) = content.strip_prefix("---") {
            if let Some(end) = rest.find("\n---") {
                keep.push((3, 3 + end));
                typescript = true;
            }
        }
    }
    for c in SCRIPT.captures_iter(content) {
        let attrs = c.get(1).map(|m| m.as_str()).unwrap_or("");
        if attrs.contains("application/json") || attrs.contains("text/template") {
            continue;
        }
        typescript |= TS_LANG_ATTR.is_match(attrs) || lang == Language::Astro;
        let body = c.get(2).unwrap();
        keep.push((body.start(), body.end()));
    }
    let script_lang = if typescript {
        "typescript"
    } else {
        "javascript"
    };
    let masked = mask(content, &keep);
    let mut result = if keep.is_empty() {
        ExtractionResult::default()
    } else {
        crate::stack::run_guarded(|| crate::tsjs::extract(path, &masked, script_lang))
            .and_then(|o| crate::decode(&o, path, lang))
            .unwrap_or_default()
    };
    for n in &mut result.nodes {
        n.language = lang;
    }
    result
        .unresolved_references
        .retain(|r| !SVELTE_RUNES.contains(&r.reference_name.as_str()));

    let lines = content.lines().count().max(1) as u32;
    let file_id = file_node_id(path);
    if !result.nodes.iter().any(|n| n.kind == NodeKind::File) {
        let mut f = Node::new(
            &file_id,
            NodeKind::File,
            path.rsplit('/').next().unwrap_or(path),
            path,
            lang,
            1,
            lines,
        );
        f.qualified_name = path.to_string();
        f.updated_at = now_ms();
        result.nodes.insert(0, f);
    }
    let name = file_stem(path).to_string();
    let comp_id = node_id(path, "component", &name, 1);
    let mut comp = Node::new(&comp_id, NodeKind::Component, &name, path, lang, 1, lines);
    comp.qualified_name = format!("{path}::{name}");
    comp.is_exported = true;
    comp.updated_at = now_ms();
    result.nodes.push(comp);
    result
        .edges
        .push(Edge::new(&file_id, &comp_id, EdgeKind::Contains));

    // Template: everything outside the scripts.
    let template_ranges = {
        let mut ranges = Vec::new();
        let mut at = 0;
        let mut sorted = keep.clone();
        sorted.sort();
        for (s, e) in sorted {
            ranges.push((at, s));
            at = e;
        }
        ranges.push((at, content.len()));
        ranges
    };
    let mut seen = std::collections::HashSet::new();
    for (s, e) in template_ranges {
        let tpl = &content[s..e];
        for c in COMPONENT_TAG.captures_iter(tpl) {
            let tag = c[1].to_string();
            if seen.insert(("tag", tag.clone())) {
                result.unresolved_references.push(reference(
                    &comp_id,
                    &tag,
                    ReferenceKind::References,
                    line_of(content, s + c.get(0).unwrap().start()),
                ));
            }
        }
        if lang == Language::Vue {
            for c in KEBAB_TAG.captures_iter(tpl) {
                let tag = kebab_to_pascal(&c[1]);
                if seen.insert(("tag", tag.clone())) {
                    result.unresolved_references.push(reference(
                        &comp_id,
                        &tag,
                        ReferenceKind::References,
                        line_of(content, s + c.get(0).unwrap().start()),
                    ));
                }
            }
        }
        let events: &Regex = if lang == Language::Vue {
            &VUE_EVENT
        } else {
            &SVELTE_EVENT
        };
        for c in events.captures_iter(tpl) {
            let handler = c[1].to_string();
            if seen.insert(("call", handler.clone())) {
                result.unresolved_references.push(reference(
                    &comp_id,
                    &handler,
                    ReferenceKind::Calls,
                    line_of(content, s + c.get(0).unwrap().start()),
                ));
            }
        }
    }
    result
}

/// ArkTS is a TypeScript superset: parse it with the TypeScript grammar.
pub fn extract_arkts(path: &str, content: &str) -> ExtractionResult {
    let mut r = crate::stack::run_guarded(|| crate::tsjs::extract(path, content, "typescript"))
        .and_then(|o| crate::decode(&o, path, Language::ArkTs))
        .unwrap_or_default();
    for n in &mut r.nodes {
        n.language = Language::ArkTs;
    }
    r
}

static LIQUID_INCLUDE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\{%-?\s*(render|include|section|sections|layout)\s+['"]([\w./-]+)['"]"#).unwrap()
});
static RAZOR_USING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*@using\s+([\w.]+)").unwrap());
static RAZOR_TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*@(?:model|inject|inherits)\s+([\w.<>]+)").unwrap());

fn base_result(path: &str, content: &str, lang: Language) -> (ExtractionResult, String) {
    let lines = content.lines().count().max(1) as u32;
    let file_id = file_node_id(path);
    let mut f = Node::new(
        &file_id,
        NodeKind::File,
        path.rsplit('/').next().unwrap_or(path),
        path,
        lang,
        1,
        lines,
    );
    f.qualified_name = path.to_string();
    f.updated_at = now_ms();
    (
        ExtractionResult {
            nodes: vec![f],
            ..Default::default()
        },
        file_id,
    )
}

fn import_node(
    r: &mut ExtractionResult,
    file_id: &str,
    path: &str,
    lang: Language,
    module: &str,
    statement: &str,
    line: u32,
) {
    let id = node_id(path, "import", module, line);
    let mut n = Node::new(&id, NodeKind::Import, module, path, lang, line, line);
    n.signature = Some(statement.to_string());
    n.updated_at = now_ms();
    r.nodes.push(n);
    r.edges.push(Edge::new(file_id, &id, EdgeKind::Contains));
    r.unresolved_references
        .push(reference(file_id, module, ReferenceKind::Imports, line));
}

/// Liquid templates: `render`/`include`/`section` tags import snippets and sections.
pub fn extract_liquid(path: &str, content: &str) -> ExtractionResult {
    let (mut r, file_id) = base_result(path, content, Language::Liquid);
    for c in LIQUID_INCLUDE.captures_iter(content) {
        let dir = match &c[1] {
            "section" | "sections" => "sections",
            "layout" => "layout",
            _ => "snippets",
        };
        let name = &c[2];
        let module = if name.ends_with(".liquid") || name.ends_with(".json") {
            format!("{dir}/{name}")
        } else {
            format!("{dir}/{name}.liquid")
        };
        let line = line_of(content, c.get(0).unwrap().start());
        import_node(
            &mut r,
            &file_id,
            path,
            Language::Liquid,
            &module,
            &format!("\"{module}\""),
            line,
        );
    }
    r
}

/// Razor views/components: `@using` namespaces, `@model`/`@inject` types and component tags.
pub fn extract_razor(path: &str, content: &str) -> ExtractionResult {
    let (mut r, file_id) = base_result(path, content, Language::Razor);
    for c in RAZOR_USING.captures_iter(content) {
        let line = line_of(content, c.get(0).unwrap().start());
        import_node(
            &mut r,
            &file_id,
            path,
            Language::Razor,
            &c[1],
            &format!("using {};", &c[1]),
            line,
        );
    }
    let name = file_stem(path).to_string();
    let comp_id = node_id(path, "component", &name, 1);
    let mut comp = Node::new(
        &comp_id,
        NodeKind::Component,
        &name,
        path,
        Language::Razor,
        1,
        content.lines().count().max(1) as u32,
    );
    comp.qualified_name = format!("{path}::{name}");
    comp.is_exported = true;
    comp.updated_at = now_ms();
    r.nodes.push(comp);
    r.edges
        .push(Edge::new(&file_id, &comp_id, EdgeKind::Contains));
    for c in RAZOR_TYPE.captures_iter(content) {
        let ty = c[1].split('<').next().unwrap_or(&c[1]);
        let ty = ty.rsplit('.').next().unwrap_or(ty);
        r.unresolved_references.push(reference(
            &comp_id,
            ty,
            ReferenceKind::TypeOf,
            line_of(content, c.get(0).unwrap().start()),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for c in COMPONENT_TAG.captures_iter(content) {
        if seen.insert(c[1].to_string()) {
            r.unresolved_references.push(reference(
                &comp_id,
                &c[1],
                ReferenceKind::References,
                line_of(content, c.get(0).unwrap().start()),
            ));
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svelte_component() {
        let src = "<script lang=\"ts\">\n  import Button from './Button.svelte';\n  let count = $state(0);\n  function inc() { count += 1; }\n</script>\n\n<Button on:click={inc}>{count}</Button>\n";
        let r = extract_component("src/Counter.svelte", src, Language::Svelte);
        let inc = r
            .nodes
            .iter()
            .find(|n| n.name == "inc")
            .expect("function in script");
        assert_eq!(inc.start_line, 4, "line numbers match the .svelte file");
        assert_eq!(inc.language, Language::Svelte);
        assert!(r
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Component && n.name == "Counter"));
        let names: Vec<&str> = r
            .unresolved_references
            .iter()
            .map(|u| u.reference_name.as_str())
            .collect();
        assert!(
            names.contains(&"Button") && names.contains(&"inc") && !names.contains(&"$state"),
            "{names:?}"
        );
    }

    #[test]
    fn vue_and_astro() {
        let vue = "<template>\n  <user-card @click=\"select(u)\" />\n</template>\n<script setup>\nfunction select(u) {}\n</script>\n";
        let r = extract_component("Users.vue", vue, Language::Vue);
        let names: Vec<&str> = r
            .unresolved_references
            .iter()
            .map(|u| u.reference_name.as_str())
            .collect();
        assert!(
            names.contains(&"UserCard") && names.contains(&"select"),
            "{names:?}"
        );
        assert!(r
            .nodes
            .iter()
            .any(|n| n.name == "select" && n.start_line == 5));
        let astro = "---\nimport Layout from '../layouts/Layout.astro';\nconst title = 'Hi';\n---\n<Layout title={title} />\n";
        let r = extract_component("src/pages/index.astro", astro, Language::Astro);
        assert!(r.nodes.iter().any(|n| n.name == "title"));
        assert!(
            r.unresolved_references
                .iter()
                .any(|u| u.reference_name == "Layout"
                    && u.reference_kind == ReferenceKind::References)
        );
    }

    #[test]
    fn liquid_and_razor() {
        let r = extract_liquid(
            "templates/index.liquid",
            "{% render 'card', p: p %}\n{% section 'header' %}",
        );
        let imports: Vec<&str> = r
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Import)
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(
            imports,
            vec!["snippets/card.liquid", "sections/header.liquid"]
        );
        let r = extract_razor(
            "Pages/Index.razor",
            "@using App.Services\n@inject IWeather Weather\n<Counter />\n",
        );
        assert!(r
            .unresolved_references
            .iter()
            .any(|u| u.reference_name == "IWeather"));
        assert!(r
            .unresolved_references
            .iter()
            .any(|u| u.reference_name == "Counter"));
    }
}
