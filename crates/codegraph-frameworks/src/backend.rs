//! Server frameworks outside the JavaScript ecosystem.

use crate::util::{
    is_ident_path, join_path, matching, next_declared_name, split_args, Lines, Routes,
};
use codegraph_core::{Language, ReferenceKind};
use regex::Regex;
use std::sync::LazyLock;

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap()
}

// ------------------------------------------------------------------ Python

static FLASK_ROUTE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"@(\w+)\.route\s*\(\s*['"]([^'"]*)['"](?:[^)]*?methods\s*=\s*[\[(]([^\])]+)[\])])?[^)]*\)"#,
    )
});
static PY_VERB_DECORATOR: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"@(\w+)\.(get|post|put|patch|delete|options|head|websocket|api_route)\s*\(\s*['"]([^'"]*)['"]"#,
    )
});
static DJANGO_PATH: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"\b(path|re_path|url)\s*\(\s*r?['"]([^'"]*)['"]\s*,\s*([\w.]+(?:\s*\([^)]*\))?)"#)
});
static DRF_REGISTER: LazyLock<Regex> =
    LazyLock::new(|| re(r#"\.register\s*\(\s*r?['"]([^'"]+)['"]\s*,\s*([\w.]+)"#));
static ROUTER_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| re(r#"\b(\w+)\s*=\s*(?:APIRouter|Blueprint)\s*\(([^)]*)\)"#));
static PREFIX_KW: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?:prefix|url_prefix)\s*=\s*['"]([^'"]*)['"]"#));

/// The `prefix=` / `url_prefix=` each APIRouter/Blueprint variable declares in this file.
fn router_prefixes(safe: &str) -> Vec<(String, String)> {
    ROUTER_PREFIX
        .captures_iter(safe)
        .filter_map(|c| Some((c[1].to_string(), PREFIX_KW.captures(&c[2])?[1].to_string())))
        .collect()
}

pub fn python_routes(
    file: &str,
    lang: Language,
    safe: &str,
    routes: &mut Routes,
    frameworks: &[&str],
) {
    let lines = Lines::new(safe);
    let prefixes = router_prefixes(safe);
    let prefix_of = |var: &str| {
        prefixes
            .iter()
            .find(|(v, _)| v == var)
            .map(|(_, p)| p.as_str())
            .unwrap_or("")
    };
    if frameworks.contains(&"flask") {
        for c in FLASK_ROUTE.captures_iter(safe) {
            let m = c.get(0).unwrap();
            let methods: Vec<String> = c
                .get(3)
                .map(|ms| {
                    ms.as_str()
                        .split(',')
                        .map(|s| {
                            s.trim()
                                .trim_matches(|q| q == '\'' || q == '"')
                                .to_uppercase()
                        })
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_else(|| vec!["GET".into()]);
            let path = join_path(prefix_of(&c[1]), &c[2]);
            let handler = next_declared_name(safe, m.end(), &["def"]);
            for method in methods {
                let line = lines.line(m.start());
                let id = routes.route(file, lang, "flask", &format!("{method} {path}"), line);
                if let Some((h, _)) = &handler {
                    routes.handler(&id, h, ReferenceKind::References, line);
                }
            }
        }
    }
    if frameworks.contains(&"fastapi") || frameworks.contains(&"flask") {
        for c in PY_VERB_DECORATOR.captures_iter(safe) {
            let m = c.get(0).unwrap();
            let verb = match &c[2] {
                "api_route" => "ANY".to_string(),
                "websocket" => "WS".to_string(),
                v => v.to_uppercase(),
            };
            let path = join_path(prefix_of(&c[1]), &c[3]);
            let line = lines.line(m.start());
            let fw = if frameworks.contains(&"fastapi") {
                "fastapi"
            } else {
                "flask"
            };
            let id = routes.route(file, lang, fw, &format!("{verb} {path}"), line);
            if let Some((h, _)) = next_declared_name(safe, m.end(), &["def"]) {
                routes.handler(&id, &h, ReferenceKind::References, line);
            }
        }
    }
    if frameworks.contains(&"django") {
        for c in DJANGO_PATH.captures_iter(safe) {
            let m = c.get(0).unwrap();
            let line = lines.line(m.start());
            let path = join_path("", c[2].trim_start_matches('^').trim_end_matches('$'));
            let id = routes.route(file, lang, "django", &path, line);
            let expr = c[3].trim();
            if let Some(inc) = expr.strip_prefix("include") {
                let module = inc.trim_matches(|ch: char| {
                    ch == '(' || ch == ')' || ch == '\'' || ch == '"' || ch.is_whitespace()
                });
                if !module.is_empty() {
                    routes.handler(&id, module, ReferenceKind::Imports, line);
                }
                continue;
            }
            let head = expr
                .split(".as_view")
                .next()
                .unwrap_or(expr)
                .split('(')
                .next()
                .unwrap_or(expr);
            let last = head.rsplit('.').next().unwrap_or(head);
            if is_ident_path(last) {
                routes.handler(&id, last, ReferenceKind::References, line);
            }
        }
        for c in DRF_REGISTER.captures_iter(safe) {
            let viewset = c[2].rsplit('.').next().unwrap_or(&c[2]).to_string();
            if !viewset.ends_with("ViewSet") && !viewset.ends_with("View") {
                continue;
            }
            let line = lines.line(c.get(0).unwrap().start());
            let id = routes.route(
                file,
                lang,
                "django",
                &format!("VIEWSET {}", join_path("", &c[1])),
                line,
            );
            routes.handler(&id, &viewset, ReferenceKind::References, line);
        }
    }
}

// --------------------------------------------------------------- JVM / C#

static SPRING_CLASS_MAPPING: LazyLock<Regex> =
    LazyLock::new(|| re(r#"@RequestMapping\s*\(\s*(?:value\s*=\s*|path\s*=\s*)?\{?\s*"([^"]*)""#));
static SPRING_METHOD: LazyLock<Regex> =
    LazyLock::new(|| re(r#"@(Get|Post|Put|Patch|Delete|Request)Mapping\b(?:\s*\(([^)]*)\))?"#));
static ANNOTATION_PATH: LazyLock<Regex> = LazyLock::new(|| re(r#""([^"]*)""#));
static REQUEST_METHOD: LazyLock<Regex> = LazyLock::new(|| re(r"RequestMethod\.(\w+)"));
static JAXRS_PATH: LazyLock<Regex> = LazyLock::new(|| re(r#"@Path\s*\(\s*"([^"]*)"\s*\)"#));
static JAXRS_VERB: LazyLock<Regex> =
    LazyLock::new(|| re(r"@(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\b"));
static JVM_CLASS: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(?:class|object|interface)\s+([A-Za-z_]\w*)"));
static JVM_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?:fun\s+(?:<[^>]*>\s*)?([A-Za-z_]\w*)\s*\(|([A-Za-z_]\w*)\s*\([^;{]*\)\s*(?:throws\s+[\w.,\s]+)?\{)",
    )
});

/// The class declared after the last class-level annotation before `offset`.
fn enclosing_class(safe: &str, offset: usize) -> Option<(usize, String)> {
    JVM_CLASS
        .captures_iter(&safe[..offset])
        .last()
        .map(|c| (c.get(0).unwrap().start(), c[1].to_string()))
}

fn jvm_method_after(safe: &str, from: usize) -> Option<String> {
    let window = &safe[from..(from + 600).min(safe.len())];
    JVM_METHOD
        .captures(window)
        .and_then(|c| c.get(1).or(c.get(2)).map(|m| m.as_str().to_string()))
}

pub fn spring_routes(file: &str, lang: Language, safe: &str, routes: &mut Routes) {
    let lines = Lines::new(safe);
    // Class-level @RequestMapping: the one right before each class declaration.
    let class_prefix = |class_start: usize| -> String {
        let head_start = safe[..class_start]
            .rfind(['}', ';'])
            .map(|i| i + 1)
            .unwrap_or(0);
        SPRING_CLASS_MAPPING
            .captures(&safe[head_start..class_start])
            .map(|c| c[1].to_string())
            .unwrap_or_default()
    };
    for c in SPRING_METHOD.captures_iter(safe) {
        let m = c.get(0).unwrap();
        let Some((class_start, class)) = enclosing_class(safe, m.start()) else {
            continue;
        };
        let args = c.get(2).map(|a| a.as_str()).unwrap_or("");
        // A class-level @RequestMapping has no method after it before the class.
        let Some(method) = jvm_method_after(safe, m.end()) else {
            continue;
        };
        if &c[1] == "Request" && safe[m.end()..].trim_start().starts_with("public class") {
            continue;
        }
        let verbs: Vec<String> = match &c[1] {
            "Request" => {
                let v: Vec<String> = REQUEST_METHOD
                    .captures_iter(args)
                    .map(|x| x[1].to_uppercase())
                    .collect();
                if v.is_empty() {
                    vec!["ANY".into()]
                } else {
                    v
                }
            }
            v => vec![v.to_uppercase()],
        };
        let paths: Vec<String> = {
            let p: Vec<String> = ANNOTATION_PATH
                .captures_iter(args)
                .map(|x| x[1].to_string())
                .collect();
            if p.is_empty() {
                vec![String::new()]
            } else {
                p
            }
        };
        let prefix = class_prefix(class_start);
        let line = lines.line(m.start());
        for verb in &verbs {
            for p in &paths {
                let id = routes.route(
                    file,
                    lang,
                    "spring",
                    &format!("{verb} {}", join_path(&prefix, p)),
                    line,
                );
                routes.handler(
                    &id,
                    &format!("{class}::{method}"),
                    ReferenceKind::References,
                    line,
                );
            }
        }
    }
    // JAX-RS / Micronaut-style @Path + @GET.
    for c in JAXRS_VERB.captures_iter(safe) {
        let m = c.get(0).unwrap();
        let Some((class_start, class)) = enclosing_class(safe, m.start()) else {
            continue;
        };
        let head_start = safe[..class_start]
            .rfind(['}', ';'])
            .map(|i| i + 1)
            .unwrap_or(0);
        let prefix = JAXRS_PATH
            .captures(&safe[head_start..class_start])
            .map(|p| p[1].to_string())
            .unwrap_or_default();
        let window_end = safe[m.end()..]
            .find('{')
            .map(|i| m.end() + i)
            .unwrap_or(safe.len());
        let window_start = safe[..m.start()]
            .rfind(['}', ';', '{'])
            .map(|i| i + 1)
            .unwrap_or(0);
        let sub = JAXRS_PATH
            .captures(&safe[window_start..window_end])
            .map(|p| p[1].to_string())
            .unwrap_or_default();
        let Some(method) = jvm_method_after(safe, m.end()) else {
            continue;
        };
        let line = lines.line(m.start());
        let id = routes.route(
            file,
            lang,
            "jax-rs",
            &format!("{} {}", &c[1], join_path(&prefix, &sub)),
            line,
        );
        routes.handler(
            &id,
            &format!("{class}::{method}"),
            ReferenceKind::References,
            line,
        );
    }
}

static CS_CLASS_ROUTE: LazyLock<Regex> = LazyLock::new(|| re(r#"\[Route\s*\(\s*"([^"]*)"\s*\)\]"#));
static CS_VERB: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"\[Http(Get|Post|Put|Patch|Delete|Head|Options)(?:\s*\(\s*"([^"]*)"[^)]*\))?\]"#)
});
static CS_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?:public|private|protected|internal)[\w\s<>,\[\]?]*?\s([A-Za-z_]\w*)\s*\(")
});
static CS_MINIMAL: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"\.Map(Get|Post|Put|Patch|Delete|Methods|Fallback|Group)\s*\(\s*"([^"]*)""#)
});

/// ASP.NET Core: attribute routing and minimal APIs.
pub fn aspnet_routes(file: &str, lang: Language, safe: &str, routes: &mut Routes) {
    let lines = Lines::new(safe);
    let b = safe.as_bytes();
    for c in CS_VERB.captures_iter(safe) {
        let m = c.get(0).unwrap();
        let Some((class_start, class)) = enclosing_class(safe, m.start()) else {
            continue;
        };
        let head_start = safe[..class_start]
            .rfind(['}', ';', '{'])
            .map(|i| i + 1)
            .unwrap_or(0);
        let controller = class
            .strip_suffix("Controller")
            .unwrap_or(&class)
            .to_string();
        let prefix = CS_CLASS_ROUTE
            .captures(&safe[head_start..class_start])
            .map(|p| p[1].replace("[controller]", &controller.to_lowercase()))
            .unwrap_or_default();
        let window = &safe[m.end()..(m.end() + 400).min(safe.len())];
        let Some(method) = CS_METHOD.captures(window).map(|x| x[1].to_string()) else {
            continue;
        };
        let sub = c
            .get(2)
            .map(|s| s.as_str().replace("[action]", &method.to_lowercase()))
            .unwrap_or_default();
        let line = lines.line(m.start());
        let id = routes.route(
            file,
            lang,
            "aspnet",
            &format!("{} {}", c[1].to_uppercase(), join_path(&prefix, &sub)),
            line,
        );
        routes.handler(
            &id,
            &format!("{class}::{method}"),
            ReferenceKind::References,
            line,
        );
    }
    for c in CS_MINIMAL.captures_iter(safe) {
        if &c[1] == "Group" {
            continue;
        }
        let m = c.get(0).unwrap();
        let open = m.start() + safe[m.start()..].find('(').unwrap();
        let Some(close) = matching(b, open) else {
            continue;
        };
        let args = split_args(&safe[open + 1..close]);
        let line = lines.line(m.start());
        let verb = if &c[1] == "Methods" || &c[1] == "Fallback" {
            "ANY".to_string()
        } else {
            c[1].to_uppercase()
        };
        let id = routes.route(
            file,
            lang,
            "aspnet",
            &format!("{verb} {}", join_path("", &c[2])),
            line,
        );
        if let Some(h) = args.last().filter(|a| is_ident_path(a)) {
            routes.handler(&id, h, ReferenceKind::References, line);
        }
    }
}

// --------------------------------------------------------------------- Go

static GO_ROUTE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"\b(\w+)\s*\.\s*(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS|Any|Get|Post|Put|Patch|Delete|Head|Options|HandleFunc|Handle|Group|Route)\s*\(\s*"([^"]*)""#,
    )
});

pub fn go_routes(file: &str, lang: Language, safe: &str, routes: &mut Routes) {
    let lines = Lines::new(safe);
    let b = safe.as_bytes();
    // `v1 := r.Group("/v1")` prefixes.
    let groups: Vec<(String, String)> =
        Regex::new(r#"\b(\w+)\s*:?=\s*(\w+)\s*\.\s*(?:Group|Route|PathPrefix)\s*\(\s*"([^"]*)""#)
            .unwrap()
            .captures_iter(safe)
            .map(|c| (c[1].to_string(), c[3].to_string()))
            .collect();
    for c in GO_ROUTE.captures_iter(safe) {
        let method = &c[2];
        if matches!(method, "Group" | "Route") {
            continue;
        }
        let m = c.get(0).unwrap();
        let open = m.start() + safe[m.start()..].find('(').unwrap();
        let Some(close) = matching(b, open) else {
            continue;
        };
        let args = split_args(&safe[open + 1..close]);
        let prefix = groups
            .iter()
            .find(|(v, _)| *v == c[1])
            .map(|(_, p)| p.as_str())
            .unwrap_or("");
        let verb = match method {
            "HandleFunc" | "Handle" | "Any" => {
                // gorilla/mux `.Methods("GET")` after the call.
                let tail = &safe[close..(close + 80).min(safe.len())];
                Regex::new(r#"^\)\s*\.\s*Methods\s*\(\s*"(\w+)""#)
                    .unwrap()
                    .captures(tail)
                    .map(|x| x[1].to_string())
                    .unwrap_or_else(|| "ANY".into())
            }
            v => v.to_uppercase(),
        };
        let raw = &c[3];
        // Go 1.22 ServeMux patterns: "GET /items/{id}".
        let (verb, raw) = match raw.split_once(' ') {
            Some((v, p)) if v.chars().all(|ch| ch.is_ascii_uppercase()) => (v.to_string(), p),
            _ => (verb, raw),
        };
        let line = lines.line(m.start());
        let id = routes.route(
            file,
            lang,
            "go-http",
            &format!("{verb} {}", join_path(prefix, raw)),
            line,
        );
        for a in args.iter().skip(1) {
            let a = a.trim().trim_start_matches('&');
            let a = a
                .strip_prefix("http.HandlerFunc(")
                .and_then(|x| x.strip_suffix(')'))
                .unwrap_or(a);
            if is_ident_path(a) {
                routes.handler(&id, a, ReferenceKind::References, line);
            }
        }
    }
}

// ------------------------------------------------------------------- Rust

static AXUM_ROUTE: LazyLock<Regex> = LazyLock::new(|| re(r#"\.route\s*\(\s*"([^"]*)"\s*,"#));
static AXUM_METHOD: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(get|post|put|patch|delete|head|options|any)\s*\(\s*([\w:]+)\s*\)"));
static RUST_ATTR_ROUTE: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"#\[(?:\w+::)?(get|post|put|patch|delete|head|options)\s*\(\s*"([^"]*)""#)
});
static ACTIX_RESOURCE: LazyLock<Regex> =
    LazyLock::new(|| re(r#"web::resource\s*\(\s*"([^"]*)"\s*\)"#));
static ACTIX_TO: LazyLock<Regex> =
    LazyLock::new(|| re(r"web::(get|post|put|patch|delete)\s*\(\s*\)\s*\.to\s*\(\s*([\w:]+)\s*\)"));

pub fn rust_routes(file: &str, lang: Language, safe: &str, routes: &mut Routes) {
    let lines = Lines::new(safe);
    let b = safe.as_bytes();
    for c in AXUM_ROUTE.captures_iter(safe) {
        let m = c.get(0).unwrap();
        let open = m.start() + safe[m.start()..].find('(').unwrap();
        let Some(close) = matching(b, open) else {
            continue;
        };
        let body = &safe[open + 1..close];
        let line = lines.line(m.start());
        for h in AXUM_METHOD.captures_iter(body) {
            let id = routes.route(
                file,
                lang,
                "axum",
                &format!("{} {}", h[1].to_uppercase(), &c[1]),
                line,
            );
            routes.handler(&id, &h[2], ReferenceKind::References, line);
        }
    }
    for c in RUST_ATTR_ROUTE.captures_iter(safe) {
        let m = c.get(0).unwrap();
        let line = lines.line(m.start());
        let id = routes.route(
            file,
            lang,
            "actix",
            &format!("{} {}", c[1].to_uppercase(), &c[2]),
            line,
        );
        if let Some((h, _)) = next_declared_name(safe, m.end(), &["fn"]) {
            routes.handler(&id, &h, ReferenceKind::References, line);
        }
    }
    for c in ACTIX_RESOURCE.captures_iter(safe) {
        let m = c.get(0).unwrap();
        let tail = &safe[m.end()..(m.end() + 400).min(safe.len())];
        let end = tail.find(";").unwrap_or(tail.len());
        let line = lines.line(m.start());
        for t in ACTIX_TO.captures_iter(&tail[..end]) {
            let id = routes.route(
                file,
                lang,
                "actix",
                &format!("{} {}", t[1].to_uppercase(), &c[1]),
                line,
            );
            routes.handler(&id, &t[2], ReferenceKind::References, line);
        }
    }
}

// -------------------------------------------------------------------- PHP

static LARAVEL_ROUTE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"Route::(get|post|put|patch|delete|options|any|match|resource|apiResource)\s*\(\s*['"]([^'"]*)['"]"#,
    )
});
static LARAVEL_ARRAY_HANDLER: LazyLock<Regex> =
    LazyLock::new(|| re(r#"\[\s*([\w\\]+)::class\s*,\s*['"](\w+)['"]\s*\]"#));
static LARAVEL_STRING_HANDLER: LazyLock<Regex> = LazyLock::new(|| re(r#"['"]([\w\\]+)@(\w+)['"]"#));
static LARAVEL_CLASS_HANDLER: LazyLock<Regex> = LazyLock::new(|| re(r"([\w\\]+)::class"));
static SYMFONY_ROUTE: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"#\[Route\s*\(\s*(?:path:\s*)?['"]([^'"]*)['"](?:[^\]]*methods:\s*\[([^\]]*)\])?"#)
});

fn short_class(c: &str) -> &str {
    c.rsplit('\\').next().unwrap_or(c)
}

const RESOURCE_ACTIONS: &[(&str, &str, &str)] = &[
    ("GET", "", "index"),
    ("GET", "/create", "create"),
    ("POST", "", "store"),
    ("GET", "/{id}", "show"),
    ("GET", "/{id}/edit", "edit"),
    ("PUT", "/{id}", "update"),
    ("DELETE", "/{id}", "destroy"),
];

pub fn php_routes(file: &str, lang: Language, safe: &str, routes: &mut Routes) {
    let lines = Lines::new(safe);
    let b = safe.as_bytes();
    for c in LARAVEL_ROUTE.captures_iter(safe) {
        let m = c.get(0).unwrap();
        let open = m.start() + safe[m.start()..].find('(').unwrap();
        let Some(close) = matching(b, open) else {
            continue;
        };
        let args = &safe[open + 1..close];
        let line = lines.line(m.start());
        let path = join_path("", &c[2]);
        match &c[1] {
            "resource" | "apiResource" => {
                let Some(cls) = LARAVEL_CLASS_HANDLER.captures(args) else {
                    continue;
                };
                let class = short_class(&cls[1]).to_string();
                for (verb, suffix, action) in RESOURCE_ACTIONS {
                    if &c[1] == "apiResource" && (*action == "create" || *action == "edit") {
                        continue;
                    }
                    let id = routes.route(
                        file,
                        lang,
                        "laravel",
                        &format!("{verb} {path}{suffix}"),
                        line,
                    );
                    routes.handler(
                        &id,
                        &format!("{class}::{action}"),
                        ReferenceKind::References,
                        line,
                    );
                }
            }
            verb => {
                let verb = if verb == "any" || verb == "match" {
                    "ANY".to_string()
                } else {
                    verb.to_uppercase()
                };
                let id = routes.route(file, lang, "laravel", &format!("{verb} {path}"), line);
                if let Some(h) = LARAVEL_ARRAY_HANDLER.captures(args) {
                    routes.handler(
                        &id,
                        &format!("{}::{}", short_class(&h[1]), &h[2]),
                        ReferenceKind::References,
                        line,
                    );
                } else if let Some(h) = LARAVEL_STRING_HANDLER.captures(args) {
                    routes.handler(
                        &id,
                        &format!("{}::{}", short_class(&h[1]), &h[2]),
                        ReferenceKind::References,
                        line,
                    );
                } else if let Some(h) = LARAVEL_CLASS_HANDLER.captures(args) {
                    // Invokable controller.
                    routes.handler(
                        &id,
                        &format!("{}::__invoke", short_class(&h[1])),
                        ReferenceKind::References,
                        line,
                    );
                }
            }
        }
    }
    for c in SYMFONY_ROUTE.captures_iter(safe) {
        let m = c.get(0).unwrap();
        let verbs: Vec<String> = c
            .get(2)
            .map(|v| {
                v.as_str()
                    .split(',')
                    .map(|s| {
                        s.trim()
                            .trim_matches(|q| q == '\'' || q == '"')
                            .to_uppercase()
                    })
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .filter(|v: &Vec<String>| !v.is_empty())
            .unwrap_or_else(|| vec!["ANY".into()]);
        let line = lines.line(m.start());
        let Some((method, _)) = next_declared_name(safe, m.end(), &["function"]) else {
            continue;
        };
        let class = enclosing_class(safe, m.start()).map(|(_, c)| c);
        for verb in verbs {
            let id = routes.route(
                file,
                lang,
                "symfony",
                &format!("{verb} {}", join_path("", &c[1])),
                line,
            );
            let target = match &class {
                Some(cl) => format!("{cl}::{method}"),
                None => method.clone(),
            };
            routes.handler(&id, &target, ReferenceKind::References, line);
        }
    }
}

// ------------------------------------------------------------------- Ruby

static RAILS_VERB: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"(?m)^\s*(get|post|put|patch|delete|match)\s+['"]([^'"]*)['"](?:\s*(?:=>|,)\s*['"]([\w/]+)#(\w+)['"])?(?:[^\n]*?\bto:\s*['"]([\w/]+)#(\w+)['"])?"#,
    )
});
static RAILS_RESOURCES: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"(?m)^\s*(resources|resource)\s+:(\w+)(?:[^\n]*?only:\s*\[([^\]]*)\])?"#)
});
static RAILS_ROOT: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?m)^\s*root\s+(?:to:\s*)?['"]([\w/]+)#(\w+)['"]"#));
static SINATRA: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?m)^\s*(get|post|put|patch|delete)\s+['"](/[^'"]*)['"]\s*(?:do|\{)"#));

fn camel(s: &str) -> String {
    s.split(['_', '/'])
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

fn controller_ref(controller: &str, action: &str) -> String {
    format!(
        "{}Controller::{action}",
        camel(controller.rsplit('/').next().unwrap_or(controller))
    )
}

pub fn ruby_routes(file: &str, lang: Language, safe: &str, routes: &mut Routes) {
    let lines = Lines::new(safe);
    if file.ends_with("config/routes.rb") || file.contains("config/routes/") {
        for c in RAILS_VERB.captures_iter(safe) {
            let line = lines.line(c.get(0).unwrap().start());
            let verb = if &c[1] == "match" {
                "ANY".to_string()
            } else {
                c[1].to_uppercase()
            };
            let id = routes.route(
                file,
                lang,
                "rails",
                &format!("{verb} {}", join_path("", &c[2])),
                line,
            );
            let target = c.get(3).zip(c.get(4)).or(c.get(5).zip(c.get(6)));
            if let Some((ctrl, action)) = target {
                routes.handler(
                    &id,
                    &controller_ref(ctrl.as_str(), action.as_str()),
                    ReferenceKind::References,
                    line,
                );
            }
        }
        for c in RAILS_RESOURCES.captures_iter(safe) {
            let line = lines.line(c.get(0).unwrap().start());
            let name = &c[2];
            let singular = &c[1] == "resource";
            let only: Option<Vec<String>> = c.get(3).map(|o| {
                o.as_str()
                    .split(',')
                    .map(|s| s.trim().trim_start_matches(':').to_string())
                    .collect()
            });
            let base = format!("/{name}");
            let actions: &[(&str, &str, &str)] = if singular {
                &[
                    ("GET", "", "show"),
                    ("GET", "/new", "new"),
                    ("POST", "", "create"),
                    ("GET", "/edit", "edit"),
                    ("PATCH", "", "update"),
                    ("DELETE", "", "destroy"),
                ]
            } else {
                &[
                    ("GET", "", "index"),
                    ("GET", "/new", "new"),
                    ("POST", "", "create"),
                    ("GET", "/:id", "show"),
                    ("GET", "/:id/edit", "edit"),
                    ("PATCH", "/:id", "update"),
                    ("DELETE", "/:id", "destroy"),
                ]
            };
            let controller = if singular {
                format!("{name}s")
            } else {
                name.to_string()
            };
            for (verb, suffix, action) in actions {
                if only
                    .as_ref()
                    .is_some_and(|o| !o.iter().any(|a| a == action))
                {
                    continue;
                }
                let id = routes.route(file, lang, "rails", &format!("{verb} {base}{suffix}"), line);
                routes.handler(
                    &id,
                    &controller_ref(&controller, action),
                    ReferenceKind::References,
                    line,
                );
            }
        }
        for c in RAILS_ROOT.captures_iter(safe) {
            let line = lines.line(c.get(0).unwrap().start());
            let id = routes.route(file, lang, "rails", "GET /", line);
            routes.handler(
                &id,
                &controller_ref(&c[1], &c[2]),
                ReferenceKind::References,
                line,
            );
        }
        return;
    }
    for c in SINATRA.captures_iter(safe) {
        let line = lines.line(c.get(0).unwrap().start());
        routes.route(
            file,
            lang,
            "sinatra",
            &format!("{} {}", c[1].to_uppercase(), &c[2]),
            line,
        );
    }
}

// ------------------------------------------------------------------ Swift

static VAPOR: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"\b(\w+)\s*\.\s*(get|post|put|patch|delete|on)\s*\(((?:\s*"[^"]*"\s*,?|\s*":\w+"\s*,?)*)\s*(?:use:\s*([\w.]+))?"#,
    )
});

pub fn vapor_routes(file: &str, lang: Language, safe: &str, routes: &mut Routes) {
    let lines = Lines::new(safe);
    for c in VAPOR.captures_iter(safe) {
        if !matches!(
            &c[1],
            "app" | "routes" | "router" | "group" | "protected" | "api"
        ) && !c[1].ends_with("routes")
            && !c[1].ends_with("Routes")
        {
            continue;
        }
        let segs: Vec<String> = ANNOTATION_PATH
            .captures_iter(&c[3])
            .map(|s| s[1].to_string())
            .collect();
        let line = lines.line(c.get(0).unwrap().start());
        let id = routes.route(
            file,
            lang,
            "vapor",
            &format!("{} {}", c[2].to_uppercase(), join_path("", &segs.join("/"))),
            line,
        );
        if let Some(h) = c.get(4) {
            routes.handler(
                &id,
                h.as_str().trim_start_matches("self."),
                ReferenceKind::References,
                line,
            );
        }
    }
}

// ------------------------------------------------------------------- Play

static PLAY_LINE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?m)^\s*(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\s+(\S+)\s+([\w.@]+)"));

/// Play Framework `conf/routes`: `GET /users controllers.Users.list(page: Int)`.
pub fn play_routes(file: &str, lang: Language, content: &str, routes: &mut Routes) {
    let lines = Lines::new(content);
    for c in PLAY_LINE.captures_iter(content) {
        let line = lines.line(c.get(0).unwrap().start());
        let id = routes.route(file, lang, "play", &format!("{} {}", &c[1], &c[2]), line);
        let target = c[3].trim_start_matches('@');
        let mut parts: Vec<&str> = target.split('.').collect();
        if parts.len() >= 2 {
            let method = parts.pop().unwrap();
            let class = parts.pop().unwrap();
            routes.handler(
                &id,
                &format!("{class}::{method}"),
                ReferenceKind::References,
                line,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(r: &Routes) -> Vec<String> {
        r.nodes.iter().map(|n| n.name.clone()).collect()
    }

    fn refs(r: &Routes) -> Vec<String> {
        r.refs.iter().map(|n| n.reference_name.clone()).collect()
    }

    #[test]
    fn python() {
        let src = "bp = Blueprint('a', __name__, url_prefix='/api')\n@bp.route('/items', methods=['GET', 'POST'])\ndef items():\n    pass\n@router.get('/u/{id}')\nasync def get_user(id):\n    pass\nurlpatterns = [path('home/', views.HomeView.as_view()), path('api/', include('api.urls'))]\n";
        let mut r = Routes::default();
        python_routes(
            "a.py",
            Language::Python,
            src,
            &mut r,
            &["flask", "fastapi", "django"],
        );
        assert_eq!(
            names(&r),
            vec![
                "GET /api/items",
                "POST /api/items",
                "GET /u/{id}",
                "/home",
                "/api"
            ]
        );
        assert_eq!(
            refs(&r),
            vec!["items", "items", "get_user", "HomeView", "api.urls"]
        );
    }

    #[test]
    fn spring_and_aspnet() {
        let java = "@RestController\n@RequestMapping(\"/api/users\")\npublic class UserController {\n  @GetMapping(\"/{id}\")\n  public User get(@PathVariable Long id) { return null; }\n  @PostMapping\n  public User create(@RequestBody User u) { return u; }\n}\n";
        let mut r = Routes::default();
        spring_routes("U.java", Language::Java, java, &mut r);
        assert_eq!(names(&r), vec!["GET /api/users/{id}", "POST /api/users"]);
        assert_eq!(
            refs(&r),
            vec!["UserController::get", "UserController::create"]
        );
        let cs = "[ApiController]\n[Route(\"api/[controller]\")]\npublic class OrdersController : ControllerBase {\n  [HttpGet(\"{id}\")]\n  public IActionResult Get(int id) => Ok();\n}\napp.MapPost(\"/hook\", HandleHook);\n";
        let mut r = Routes::default();
        aspnet_routes("O.cs", Language::CSharp, cs, &mut r);
        assert_eq!(names(&r), vec!["GET /api/orders/{id}", "POST /hook"]);
        assert_eq!(refs(&r), vec!["OrdersController::Get", "HandleHook"]);
    }

    #[test]
    fn go_rust_php_ruby_swift_play() {
        let mut r = Routes::default();
        go_routes("m.go", Language::Go, "v1 := r.Group(\"/v1\")\nv1.GET(\"/ping\", handlers.Ping)\nmux.HandleFunc(\"GET /items/{id}\", getItem)\n", &mut r);
        assert_eq!(names(&r), vec!["GET /v1/ping", "GET /items/{id}"]);
        let mut r = Routes::default();
        rust_routes("m.rs", Language::Rust, "Router::new().route(\"/users\", get(list_users).post(create_user));\n#[get(\"/health\")]\nasync fn health() -> &'static str { \"ok\" }\n", &mut r);
        assert_eq!(names(&r), vec!["GET /users", "POST /users", "GET /health"]);
        assert_eq!(refs(&r), vec!["list_users", "create_user", "health"]);
        let mut r = Routes::default();
        php_routes("web.php", Language::Php, "<?php\nRoute::get('/posts', [PostController::class, 'index']);\nRoute::post('/x', 'App\\Http\\XController@store');\nRoute::apiResource('photos', PhotoController::class);\n", &mut r);
        assert_eq!(&names(&r)[..2], &["GET /posts", "POST /x"]);
        assert_eq!(r.nodes.len(), 2 + 5);
        assert_eq!(
            &refs(&r)[..2],
            &["PostController::index", "XController::store"]
        );
        let mut r = Routes::default();
        ruby_routes("config/routes.rb", Language::Ruby, "Rails.application.routes.draw do\n  root 'home#index'\n  get '/about', to: 'pages#about'\n  resources :articles, only: [:index, :show]\nend\n", &mut r);
        assert_eq!(
            names(&r),
            vec!["GET /about", "GET /articles", "GET /articles/:id", "GET /"]
        );
        assert_eq!(
            refs(&r),
            vec![
                "PagesController::about",
                "ArticlesController::index",
                "ArticlesController::show",
                "HomeController::index"
            ]
        );
        let mut r = Routes::default();
        vapor_routes(
            "routes.swift",
            Language::Swift,
            "app.get(\"hello\", \":name\", use: hello)\n",
            &mut r,
        );
        assert_eq!(names(&r), vec!["GET /hello/:name"]);
        let mut r = Routes::default();
        play_routes(
            "conf/routes",
            Language::Yaml,
            "GET  /clients/:id   controllers.Clients.show(id: Long)\n",
            &mut r,
        );
        assert_eq!(refs(&r), vec!["Clients::show"]);
    }
}
