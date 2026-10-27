//! Dynamic dispatch the call graph can't see: event listeners and emits, C
//! function-pointer slots, and HTTP requests to the project's own routes.
//!
//! Extraction records each side as a reference with a reserved name prefix;
//! resolution joins them:
//! - `bind:<channel>:<key>` (candidate = the handler) registers a handler,
//! - `fire:<channel>:<key>` invokes whatever is bound to that key,
//! - `http:<METHOD> <path>` requests a route,
//! - `nav:<path>` navigates to a page (`router.push`, `<Link href>`, `redirect`).

use crate::util::Lines;
use codegraph_core::{Language, ReferenceKind};
use regex::Regex;
use std::sync::LazyLock;

pub const BIND_PREFIX: &str = "bind:";
pub const FIRE_PREFIX: &str = "fire:";
pub const HTTP_PREFIX: &str = "http:";
pub const NAV_PREFIX: &str = "nav:";

/// One dispatch site, at a 1-based line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub line: u32,
    pub name: String,
    pub kind: ReferenceKind,
    /// For binds: the handler expression to resolve.
    pub handler: Option<String>,
}

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap()
}

static JS_LISTEN: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"\.\s*(?:on|once|addListener|addEventListener|prependListener|subscribe)\s*\(\s*['"`]([\w:./-]+)['"`]\s*,\s*([A-Za-z_$][\w$.]*)\s*[,)]"#,
    )
});
static JS_EMIT: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"\.\s*(?:emit|dispatch|publish|trigger|fire)\s*\(\s*['"`]([\w:./-]+)['"`]"#)
});
static DOM_DISPATCH: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"dispatchEvent\s*\(\s*new\s+(?:Custom)?Event\s*\(\s*['"`]([\w:./-]+)['"`]"#)
});

static C_INIT_SLOT: LazyLock<Regex> =
    LazyLock::new(|| re(r"\.\s*([A-Za-z_]\w*)\s*=\s*&?\s*([A-Za-z_]\w*)\s*[,}\n]"));
static C_ASSIGN_SLOT: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?:->|\.)\s*([A-Za-z_]\w*)\s*=\s*&?\s*([A-Za-z_]\w*)\s*;"));
static C_SLOT_CALL: LazyLock<Regex> = LazyLock::new(|| re(r"(?:->|\.)\s*([A-Za-z_]\w*)\s*\("));
static C_PAREN_SLOT_CALL: LazyLock<Regex> =
    LazyLock::new(|| re(r"\(\s*\*\s*\w+\s*(?:->|\.)\s*([A-Za-z_]\w*)\s*\)\s*\("));

static JS_HTTP: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"\b(fetch|\$fetch|useFetch|ofetch|axios|ky|superagent|request|api|http|client)(?:\s*\.\s*(get|post|put|patch|delete|head))?\s*(?:<[^>()]*>)?\s*\(\s*(['"`])((?:https?://[^/'"`]*|\$\{[^}]*\})?/[^'"`]*)['"`]"#,
    )
});
static FETCH_METHOD: LazyLock<Regex> = LazyLock::new(|| re(r#"method\s*:\s*['"`](\w+)['"`]"#));
static JS_NAV: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"(?:\b(?:router|navigation|history|navigator|nav)\s*\.\s*(?:push|replace|navigate|redirect)|\b(?:navigate|redirect|permanentRedirect|goto|navigateTo))\s*\(\s*(?:\{\s*pathname\s*:\s*)?(['"`])((?:\$\{[^}]*\})?/[^'"`]*)['"`]"#,
    )
});
static JSX_LINK: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"<(?:Link|NavLink|RouterLink|router-link|NuxtLink|A|a)\b[^>]*?\b(?:href|to)\s*=\s*\{?\s*(['"`])(/[^'"`]*)['"`]"#,
    )
});
static SERVER_NAV: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"\b(?:redirect_to|redirect|RedirectResponse|HttpResponseRedirect)\s*\(?\s*f?['"](/[^'"]*)['"]"#,
    )
});
static PY_HTTP: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"\b(?:requests|httpx|client|session|self\.client)\s*\.\s*(get|post|put|patch|delete|head)\s*\(\s*f?['"]((?:https?://[^/'"]*)?/[^'"]*)['"]"#,
    )
});

const C_KEYWORDS: &[&str] = &[
    "if", "while", "for", "switch", "return", "sizeof", "NULL", "true", "false",
];

/// Normalize a requested URL to a route-comparable path: drop the origin, the
/// query, and turn template holes (`${id}`, `{id}`) into `:param`.
pub fn normalize_url(raw: &str) -> String {
    let mut s = raw.to_string();
    // A leading `${BASE}` is the origin.
    if s.starts_with("${") {
        if let Some(i) = s.find('}') {
            s = s[i + 1..].to_string();
        }
    }
    if let Some(i) = s.find("://") {
        s = s[i + 3..]
            .find('/')
            .map(|j| s[i + 3 + j..].to_string())
            .unwrap_or_else(|| "/".into());
    }
    if let Some(i) = s.find(['?', '#']) {
        s.truncate(i);
    }
    let holes = Regex::new(r"\$\{[^}]*\}|\{[^}]*\}").unwrap();
    let s = holes.replace_all(&s, ":param").into_owned();
    if s.len() > 1 {
        s.trim_end_matches('/').to_string()
    } else {
        s
    }
}

/// Dispatch sites in one file (comments already stripped from `safe`).
pub fn sites(lang: Language, safe: &str) -> Vec<Site> {
    let lines = Lines::new(safe);
    let mut out = Vec::new();
    let line = |off: usize| lines.line(off);
    if lang.is_js_family() || matches!(lang, Language::Svelte | Language::Vue | Language::Astro) {
        for c in JS_LISTEN.captures_iter(safe) {
            out.push(Site {
                line: line(c.get(0).unwrap().start()),
                name: format!("{BIND_PREFIX}event:{}", &c[1]),
                kind: ReferenceKind::References,
                handler: Some(c[2].to_string()),
            });
        }
        for c in JS_EMIT
            .captures_iter(safe)
            .chain(DOM_DISPATCH.captures_iter(safe))
        {
            out.push(Site {
                line: line(c.get(0).unwrap().start()),
                name: format!("{FIRE_PREFIX}event:{}", &c[1]),
                kind: ReferenceKind::Calls,
                handler: None,
            });
        }
        for c in JS_HTTP.captures_iter(safe) {
            let m = c.get(0).unwrap();
            let method = match c.get(2) {
                Some(v) => v.as_str().to_uppercase(),
                None => {
                    // fetch(url, { method: 'POST' })
                    let tail = &safe[m.end()..(m.end() + 200).min(safe.len())];
                    let call_end = tail.find(')').unwrap_or(tail.len());
                    FETCH_METHOD
                        .captures(&tail[..call_end])
                        .map(|x| x[1].to_uppercase())
                        .unwrap_or_else(|| "GET".into())
                }
            };
            // `api(...)`/`client(...)` without a verb are too generic to trust.
            if c.get(2).is_none() && !matches!(&c[1], "fetch" | "$fetch" | "useFetch" | "ofetch") {
                continue;
            }
            out.push(Site {
                line: line(m.start()),
                name: format!("{HTTP_PREFIX}{method} {}", normalize_url(&c[4])),
                kind: ReferenceKind::Calls,
                handler: None,
            });
        }
    }
    let nav = |line: u32, path: &str| Site {
        line,
        name: format!("{NAV_PREFIX}{}", normalize_url(path)),
        kind: ReferenceKind::Navigates,
        handler: None,
    };
    if lang.is_js_family() || matches!(lang, Language::Svelte | Language::Vue | Language::Astro) {
        for c in JS_NAV.captures_iter(safe) {
            out.push(nav(line(c.get(0).unwrap().start()), &c[2]));
        }
    }
    // Link targets live in markup, which the stripper may have blanked as
    // strings; read them from the comment-stripped text as given.
    if lang.is_js_family() || matches!(lang, Language::Svelte | Language::Vue | Language::Astro) {
        for c in JSX_LINK.captures_iter(safe) {
            out.push(nav(line(c.get(0).unwrap().start()), &c[2]));
        }
    }
    if matches!(lang, Language::Python | Language::Ruby | Language::Php) {
        for c in SERVER_NAV.captures_iter(safe) {
            out.push(nav(line(c.get(0).unwrap().start()), &c[1]));
        }
    }
    if lang == Language::Python {
        for c in PY_HTTP.captures_iter(safe) {
            out.push(Site {
                line: line(c.get(0).unwrap().start()),
                name: format!(
                    "{HTTP_PREFIX}{} {}",
                    c[1].to_uppercase(),
                    normalize_url(&c[2])
                ),
                kind: ReferenceKind::Calls,
                handler: None,
            });
        }
    }
    if matches!(lang, Language::C | Language::Cpp | Language::ObjC) {
        for c in C_INIT_SLOT
            .captures_iter(safe)
            .chain(C_ASSIGN_SLOT.captures_iter(safe))
        {
            let (slot, func) = (&c[1], &c[2]);
            if C_KEYWORDS.contains(&func) || func.chars().all(|ch| ch.is_ascii_digit()) {
                continue;
            }
            out.push(Site {
                line: line(c.get(0).unwrap().start()),
                name: format!("{BIND_PREFIX}fnptr:{slot}"),
                kind: ReferenceKind::References,
                handler: Some(func.to_string()),
            });
        }
        // In C++ (and Objective-C) `a->f(` is usually an ordinary method
        // call; only the explicit `(*a->f)(` form is a pointer call there.
        let plain = (lang == Language::C).then(|| C_SLOT_CALL.captures_iter(safe));
        for c in plain
            .into_iter()
            .flatten()
            .chain(C_PAREN_SLOT_CALL.captures_iter(safe))
        {
            out.push(Site {
                line: line(c.get(0).unwrap().start()),
                name: format!("{FIRE_PREFIX}fnptr:{}", &c[1]),
                kind: ReferenceKind::Calls,
                handler: None,
            });
        }
    }
    out
}

/// Whether a route pattern (`/users/:id`, `/users/{id}`, `/users/[id]`,
/// `/users/<int:id>`) serves a requested path (holes as `:param`).
pub fn route_matches(pattern: &str, path: &str) -> bool {
    let is_param = |s: &str| {
        s.starts_with(':')
            || (s.starts_with('{') && s.ends_with('}'))
            || (s.starts_with('[') && s.ends_with(']'))
            || (s.starts_with('<') && s.ends_with('>'))
            || s.starts_with('$')
            || s == "*"
    };
    let p: Vec<&str> = pattern
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let q: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    // A trailing catch-all (`[...slug]`, `*`) absorbs the rest.
    let catch_all = p.last().is_some_and(|l| {
        l.starts_with("[...") || l.starts_with("[[...") || *l == "*" || l.starts_with('*')
    });
    if !(p.len() == q.len() || catch_all && q.len() + 1 >= p.len()) {
        return false;
    }
    p.iter()
        .zip(q.iter())
        .all(|(a, b)| is_param(a) || *b == ":param" || a == b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_events_slots_and_requests() {
        let js = "bus.on('saved', onSaved);\nfunction save() { bus.emit('saved', x); }\nconst r = await fetch(`${API}/users/${id}?full=1`, { method: 'PUT' });\naxios.post('/api/login', body);\n";
        let s = sites(Language::TypeScript, js);
        let names: Vec<&str> = s.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "bind:event:saved",
                "fire:event:saved",
                "http:PUT /users/:param",
                "http:POST /api/login"
            ]
        );
        assert_eq!(s[0].handler.as_deref(), Some("onSaved"));
        let c = "static struct file_ops ops = { .read = disk_read, .write = disk_write };\nint f(struct file *f) { return f->ops->read(f); }\n";
        let names: Vec<String> = sites(Language::C, c).into_iter().map(|x| x.name).collect();
        assert_eq!(
            names,
            vec!["bind:fnptr:read", "bind:fnptr:write", "fire:fnptr:read"]
        );
        let nav = "function go() { router.push(`/items/${id}`); }\nconst L = () => <Link href=\"/about\">About</Link>;\n";
        let names: Vec<String> = sites(Language::Tsx, nav)
            .into_iter()
            .map(|x| x.name)
            .collect();
        assert_eq!(names, vec!["nav:/items/:param", "nav:/about"]);
        assert_eq!(
            sites(Language::Ruby, "redirect_to '/login'\n")[0].name,
            "nav:/login"
        );
        let py = "resp = requests.get('http://localhost:8000/items/5')\n";
        assert_eq!(sites(Language::Python, py)[0].name, "http:GET /items/5");
    }

    #[test]
    fn route_patterns() {
        assert!(route_matches("/users/:id", "/users/:param"));
        assert!(route_matches("/users/{id}", "/users/42"));
        assert!(route_matches("/blog/[...slug]", "/blog/a/b"));
        assert!(!route_matches("/users/:id", "/users"));
        assert!(!route_matches("/users", "/accounts"));
        assert_eq!(normalize_url("https://x.io/api/v1/?q=1"), "/api/v1");
    }
}
