//! JavaScript/TypeScript frameworks.

use crate::util::{is_ident_path, join_path, matching, split_args, Lines, Routes, HTTP_METHODS};
use codegraph_core::{Language, ReferenceKind};
use regex::Regex;
use std::sync::LazyLock;

static SERVER_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b([A-Za-z_$][\w$]*)\s*\.\s*(get|post|put|patch|delete|del|all|options|head|use)\s*\(\s*(['"`])([^'"`\n]*)['"`]"#).unwrap()
});
static ROUTE_CHAIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b([A-Za-z_$][\w$]*)\s*\.\s*route\s*\(\s*['"`]([^'"`\n]+)['"`]\s*\)"#).unwrap()
});
static CHAIN_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\.\s*(get|post|put|patch|delete|all)\s*\(").unwrap());
static CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"((?:[A-Za-z_$][\w$]*\s*\??\.\s*)*)([A-Za-z_$][\w$]*)\s*\(").unwrap()
});

/// Receivers that look like an HTTP app or router.
fn is_server_receiver(name: &str) -> bool {
    let l = name.to_lowercase();
    matches!(
        l.as_str(),
        "app"
            | "router"
            | "server"
            | "api"
            | "r"
            | "routes"
            | "fastify"
            | "hono"
            | "koa"
            | "instance"
            | "route"
            | "v1"
            | "v2"
    ) || l.ends_with("router")
        || l.ends_with("app")
        || l.ends_with("routes")
        || l.ends_with("api")
}

const RESERVED: &[&str] = &[
    "json",
    "jsonp",
    "send",
    "sendStatus",
    "sendFile",
    "status",
    "end",
    "redirect",
    "render",
    "set",
    "get",
    "header",
    "type",
    "format",
    "attachment",
    "download",
    "cookie",
    "clearCookie",
    "append",
    "location",
    "vary",
    "links",
    "accepts",
    "is",
    "next",
    "then",
    "catch",
    "finally",
    "resolve",
    "reject",
    "all",
    "race",
    "map",
    "filter",
    "forEach",
    "reduce",
    "find",
    "push",
    "pop",
    "slice",
    "splice",
    "includes",
    "keys",
    "values",
    "entries",
    "assign",
    "parse",
    "stringify",
    "log",
    "error",
    "warn",
    "info",
    "String",
    "Number",
    "Boolean",
    "Array",
    "Object",
    "Date",
    "Math",
    "JSON",
    "Promise",
    "require",
    "if",
    "for",
    "while",
    "switch",
    "function",
    "return",
    "typeof",
    "await",
    "new",
    "catch",
    "async",
];

/// Calls made inside an inline handler body.
fn body_calls(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in CALL.captures_iter(body) {
        let method = &c[2];
        let receiver = c[1].replace([' ', '?'], "");
        let receiver = receiver.trim_end_matches('.');
        if RESERVED.contains(&method)
            || matches!(
                receiver,
                "res" | "req" | "ctx" | "reply" | "response" | "request" | "console"
            )
        {
            continue;
        }
        let name = if receiver.is_empty() {
            method.to_string()
        } else {
            format!("{receiver}.{method}")
        };
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// The handlers among a route call's arguments (after the path).
fn add_handlers(routes: &mut Routes, route: &str, args: &[&str], line: u32) {
    for a in args {
        let a = a.trim();
        if is_ident_path(a) {
            routes.handler(route, a, ReferenceKind::References, line);
        } else if a.contains("=>") || a.starts_with("function") || a.starts_with("async") {
            for call in body_calls(a) {
                routes.handler(route, &call, ReferenceKind::Calls, line);
            }
        } else if let Some(inner) = a.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            add_handlers(routes, route, &split_args(inner), line);
        }
    }
}

/// Express, Koa, Fastify, Hono, Elysia: `app.get('/x', handler)`.
pub fn server_routes(file: &str, lang: Language, safe: &str, routes: &mut Routes, framework: &str) {
    let lines = Lines::new(safe);
    let b = safe.as_bytes();
    for c in SERVER_CALL.captures_iter(safe) {
        let receiver = &c[1];
        let method = &c[2];
        let path = &c[4];
        if !is_server_receiver(receiver) || method == "use" && !path.starts_with('/') {
            continue;
        }
        if method == "get" && !path.starts_with('/') && path != "*" {
            // `config.get('key')`, `map.get('x')`
            continue;
        }
        let m = c.get(0).unwrap();
        let open = safe[m.start()..].find('(').map(|p| m.start() + p).unwrap();
        let Some(close) = matching(b, open) else {
            continue;
        };
        let args = split_args(&safe[open + 1..close]);
        let verb = match method {
            "use" => "USE",
            "del" => "DELETE",
            other => &other.to_uppercase(),
        };
        let line = lines.line(m.start());
        let id = routes.route(file, lang, framework, &format!("{verb} {path}"), line);
        add_handlers(routes, &id, args.get(1..).unwrap_or(&[]), line);
    }
    for c in ROUTE_CHAIN.captures_iter(safe) {
        if !is_server_receiver(&c[1]) {
            continue;
        }
        let path = c[2].to_string();
        let mut at = c.get(0).unwrap().end();
        while let Some(link) = CHAIN_LINK.captures(&safe[at..]) {
            let open = at + link.get(0).unwrap().end() - 1;
            let Some(close) = matching(b, open) else {
                break;
            };
            let line = lines.line(at);
            let id = routes.route(
                file,
                lang,
                framework,
                &format!("{} {path}", link[1].to_uppercase()),
                line,
            );
            add_handlers(routes, &id, &split_args(&safe[open + 1..close]), line);
            at = close + 1;
        }
    }
}

static NEST_CONTROLLER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"@Controller\s*\(\s*(?:['"`]([^'"`]*)['"`]|\{[^}]*path\s*:\s*['"`]([^'"`]*)['"`][^}]*\})?\s*\)"#).unwrap()
});
static NEST_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"@(Get|Post|Put|Patch|Delete|All|Options|Head)\s*\(\s*(?:['"`]([^'"`]*)['"`])?\s*\)"#,
    )
    .unwrap()
});
static CLASS_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bclass\s+([A-Za-z_$][\w$]*)").unwrap());
static METHOD_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|\n)\s*(?:@[\w.]+(?:\([^)]*\))?\s*)*(?:public\s+|private\s+|protected\s+|static\s+|async\s+)*([A-Za-z_$][\w$]*)\s*\(").unwrap()
});

/// NestJS: `@Controller('users')` + `@Get(':id') findOne()`.
pub fn nest_routes(file: &str, lang: Language, safe: &str, routes: &mut Routes) {
    let lines = Lines::new(safe);
    let controllers: Vec<(usize, String, String)> = NEST_CONTROLLER
        .captures_iter(safe)
        .filter_map(|c| {
            let start = c.get(0).unwrap().end();
            let prefix = c
                .get(1)
                .or(c.get(2))
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let class = CLASS_NAME.captures(&safe[start..])?[1].to_string();
            Some((start, prefix, class))
        })
        .collect();
    for c in NEST_METHOD.captures_iter(safe) {
        let m = c.get(0).unwrap();
        let Some((_, prefix, class)) = controllers.iter().rev().find(|(s, _, _)| *s < m.start())
        else {
            continue;
        };
        let after = &safe[m.end()..];
        let Some(method) = METHOD_NAME.captures(after).map(|x| x[1].to_string()) else {
            continue;
        };
        let path = join_path(prefix, c.get(2).map(|p| p.as_str()).unwrap_or(""));
        let line = lines.line(m.start());
        let id = routes.route(
            file,
            lang,
            "nestjs",
            &format!("{} {path}", c[1].to_uppercase()),
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

static OBJ_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bpath\s*:\s*['"`]([^'"`\n]*)['"`]"#).unwrap());
static OBJ_COMPONENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:component|Component|element|loadComponent|lazy|page)\s*:\s*(?:<\s*)?(?:\(\s*\)\s*=>\s*import\([^)]*\)\.then\([^)]*=>\s*\w+\.)?([A-Z][\w$]*)").unwrap()
});
static JSX_ROUTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<Route\b([^>]*?)/?>"#).unwrap());
static JSX_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bpath\s*=\s*(?:\{\s*)?['"`]([^'"`]*)['"`]"#).unwrap());
static JSX_TARGET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:element|component|Component)\s*=\s*\{\s*<?\s*([A-Z][\w$.]*)").unwrap()
});

/// Start offsets of the object literals enclosing `offset`, innermost first.
fn enclosing_objects(b: &[u8], offset: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut i = offset;
    while i > 0 && out.len() < 12 {
        i -= 1;
        match b[i] {
            b'}' | b']' | b')' => depth += 1,
            b'{' | b'[' | b'(' => {
                if depth == 0 {
                    if b[i] == b'{' {
                        out.push(i);
                    }
                } else {
                    depth -= 1;
                }
            }
            _ => {}
        }
    }
    out
}

/// Route tables written as object literals (React Router data routers, Vue
/// Router, Angular, TanStack): `{ path: 'users', component: Users, children: [...] }`.
pub fn object_routes(file: &str, lang: Language, safe: &str, routes: &mut Routes, framework: &str) {
    let lines = Lines::new(safe);
    let b = safe.as_bytes();
    for c in OBJ_PATH.captures_iter(safe) {
        let m = c.get(0).unwrap();
        let objects = enclosing_objects(b, m.start());
        let Some(&own) = objects.first() else {
            continue;
        };
        let Some(own_end) = matching(b, own) else {
            continue;
        };
        // Parent route objects contribute their own `path:` prefixes.
        let mut segments = vec![c[1].to_string()];
        for &parent in &objects[1..] {
            let Some(end) = matching(b, parent) else {
                continue;
            };
            let body = &safe[parent..end];
            // The parent's own `path:` sits outside its `children` array.
            let own_part = match body.find("children") {
                Some(ch) => &body[..ch],
                None => continue,
            };
            if let Some(p) = OBJ_PATH.captures(own_part) {
                segments.push(p[1].to_string());
            }
        }
        segments.reverse();
        let full = segments.iter().fold(String::new(), |acc, s| {
            if s.starts_with('/') {
                s.clone()
            } else {
                join_path(&acc, s)
            }
        });
        let body = &safe[own..own_end];
        // The component belongs to this object, not a nested child.
        let own_body = match body.find("children") {
            Some(ch) => &body[..ch],
            None => body,
        };
        let line = lines.line(m.start());
        let target = OBJ_COMPONENT.captures(own_body).map(|t| t[1].to_string());
        if target.is_none() && !own_body.contains("redirect") && !own_body.contains("children") {
            continue;
        }
        let id = routes.route(file, lang, framework, &full, line);
        if let Some(t) = target {
            routes.handler(&id, &t, ReferenceKind::References, line);
        }
    }
    for c in JSX_ROUTE.captures_iter(safe) {
        let attrs = &c[1];
        let Some(path) = JSX_PATH.captures(attrs) else {
            continue;
        };
        let line = lines.line(c.get(0).unwrap().start());
        let id = routes.route(file, lang, framework, &join_path("", &path[1]), line);
        if let Some(t) = JSX_TARGET.captures(attrs) {
            routes.handler(&id, &t[1], ReferenceKind::References, line);
        }
    }
}

static DEFAULT_EXPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"export\s+default\s+(?:async\s+)?(?:function\s*\*?\s*|class\s+)?([A-Za-z_$][\w$]*)")
        .unwrap()
});
static EXPORTED_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"export\s+(?:async\s+)?(?:function\s+|const\s+)(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\b",
    )
    .unwrap()
});
static TANSTACK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"create(?:File)?Route\s*\(\s*['"`]([^'"`]+)['"`]\s*\)"#).unwrap()
});

/// A file-based router's URL for `rel` (the path below the routes dir, no extension).
fn file_route_path(rel: &str, index_names: &[&str]) -> String {
    let mut segs: Vec<String> = Vec::new();
    for seg in rel.split('/') {
        // Route groups `(auth)` and parallel slots `@modal` don't appear in URLs.
        if seg.starts_with('(') && seg.ends_with(')') || seg.starts_with('@') {
            continue;
        }
        if index_names.contains(&seg) {
            continue;
        }
        segs.push(seg.to_string());
    }
    join_path("", &segs.join("/"))
}

/// `a/b.page.tsx` → `a/b.page`: only the last extension of the file name.
fn strip_ext(path: &str) -> &str {
    let name_start = path.rfind('/').map(|i| i + 1).unwrap_or(0);
    match path[name_start..].rfind('.') {
        Some(i) if i > 0 => &path[..name_start + i],
        _ => path,
    }
}

/// Where a file sits below one of `dirs` (`src/pages/a/b.tsx` → `a/b.tsx`).
fn below<'a>(file: &'a str, dirs: &[&str]) -> Option<&'a str> {
    for d in dirs {
        if let Some(rest) = file.strip_prefix(d) {
            return Some(rest);
        }
        let marker = format!("/{d}");
        if let Some(i) = file.find(&marker) {
            return Some(&file[i + marker.len()..]);
        }
    }
    None
}

fn page_route(
    file: &str,
    lang: Language,
    content: &str,
    routes: &mut Routes,
    framework: &str,
    path: &str,
) {
    let id = routes.route(file, lang, framework, path, 1);
    if let Some(c) = DEFAULT_EXPORT.captures(content) {
        routes.handler(&id, &c[1], ReferenceKind::References, 1);
    }
}

fn api_methods(
    file: &str,
    lang: Language,
    content: &str,
    routes: &mut Routes,
    framework: &str,
    path: &str,
) {
    let lines = Lines::new(content);
    for c in EXPORTED_METHOD.captures_iter(content) {
        let line = lines.line(c.get(0).unwrap().start());
        let id = routes.route(file, lang, framework, &format!("{} {path}", &c[1]), line);
        routes.handler(&id, &c[1], ReferenceKind::References, line);
    }
}

/// Next.js `pages/` and `app/` routers.
pub fn nextjs_routes(file: &str, lang: Language, content: &str, routes: &mut Routes) {
    let is_script = |f: &str| {
        [".tsx", ".ts", ".jsx", ".js", ".mdx"]
            .iter()
            .any(|e| f.ends_with(e))
    };
    if !is_script(file) {
        return;
    }
    if let Some(rel) = below(file, &["src/app/", "app/"]) {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let dir = rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        if name.starts_with("page.") {
            page_route(
                file,
                lang,
                content,
                routes,
                "nextjs",
                &file_route_path(dir, &[]),
            );
        } else if name.starts_with("route.") {
            api_methods(
                file,
                lang,
                content,
                routes,
                "nextjs",
                &file_route_path(dir, &[]),
            );
        }
        return;
    }
    if let Some(rel) = below(file, &["src/pages/", "pages/"]) {
        let stem = strip_ext(rel);
        let base = stem.rsplit('/').next().unwrap_or(stem);
        if base.starts_with('_') {
            return;
        }
        let path = file_route_path(stem, &["index"]);
        if stem.starts_with("api/") {
            let id = routes.route(file, lang, "nextjs", &format!("API {path}"), 1);
            if let Some(c) = DEFAULT_EXPORT.captures(content) {
                routes.handler(&id, &c[1], ReferenceKind::References, 1);
            }
        } else {
            page_route(file, lang, content, routes, "nextjs", &path);
        }
    }
}

/// SvelteKit `src/routes/**/+page.svelte`, `+server.ts`.
pub fn sveltekit_routes(file: &str, lang: Language, content: &str, routes: &mut Routes) {
    let Some(rel) = below(file, &["src/routes/"]) else {
        return;
    };
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let dir = rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let path = file_route_path(dir, &[]);
    if name.starts_with("+server.") {
        api_methods(file, lang, content, routes, "sveltekit", &path);
    } else if name.starts_with("+page.") {
        let id = routes.route(file, lang, "sveltekit", &path, 1);
        if name.contains(".server.") || name.ends_with(".ts") || name.ends_with(".js") {
            for f in ["load", "actions"] {
                if content.contains(&format!("export const {f}"))
                    || content.contains(&format!("export async function {f}"))
                    || content.contains(&format!("export function {f}"))
                {
                    routes.handler(&id, f, ReferenceKind::References, 1);
                }
            }
        }
    }
}

/// Simple file-based routers: Nuxt `pages/`, Astro `src/pages/`, Expo Router
/// `app/`, Remix flat routes `app/routes/`.
pub fn file_routes(
    file: &str,
    lang: Language,
    content: &str,
    routes: &mut Routes,
    framework: &str,
) {
    let (dirs, exts): (&[&str], &[&str]) = match framework {
        "nuxt" => (&["pages/"], &[".vue"]),
        "astro" => (
            &["src/pages/"],
            &[".astro", ".md", ".mdx", ".ts", ".js", ".html"],
        ),
        "expo-router" => (&["src/app/", "app/"], &[".tsx", ".ts", ".jsx", ".js"]),
        "remix" => (&["app/routes/"], &[".tsx", ".ts", ".jsx", ".js"]),
        _ => return,
    };
    if !exts.iter().any(|e| file.ends_with(e)) {
        return;
    }
    let Some(rel) = below(file, dirs) else { return };
    let stem = strip_ext(rel);
    let base = stem.rsplit('/').next().unwrap_or(stem);
    if base.starts_with('_') || base.starts_with('+') {
        return;
    }
    let stem = if framework == "remix" {
        stem.replace('.', "/").replace('$', ":")
    } else {
        stem.to_string()
    };
    let path = file_route_path(&stem, &["index", "_index"]);
    if framework == "astro" && (file.ends_with(".ts") || file.ends_with(".js")) {
        api_methods(file, lang, content, routes, framework, &path);
        return;
    }
    page_route(file, lang, content, routes, framework, &path);
}

/// TanStack Router `createFileRoute('/posts/$id')`.
pub fn tanstack_routes(file: &str, lang: Language, safe: &str, routes: &mut Routes) {
    let lines = Lines::new(safe);
    for c in TANSTACK.captures_iter(safe) {
        let line = lines.line(c.get(0).unwrap().start());
        let id = routes.route(file, lang, "tanstack-router", &c[1], line);
        if let Some(comp) = OBJ_COMPONENT.captures(&safe[c.get(0).unwrap().end()..]) {
            routes.handler(&id, &comp[1], ReferenceKind::References, line);
        }
    }
}

pub fn is_http_method(s: &str) -> bool {
    HTTP_METHODS.contains(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(r: &Routes) -> Vec<String> {
        r.nodes.iter().map(|n| n.name.clone()).collect()
    }

    #[test]
    fn express() {
        let src = "const router = express.Router();\nrouter.get('/users/:id', auth, users.show);\napp.post('/login', async (req, res) => { const u = await signIn(req.body); res.json(u); });\nconfig.get('port');\napp.route('/book').get(listBooks).post(addBook);\n";
        let mut r = Routes::default();
        server_routes("a.js", Language::JavaScript, src, &mut r, "express");
        assert_eq!(
            names(&r),
            vec!["GET /users/:id", "POST /login", "GET /book", "POST /book"]
        );
        let refs: Vec<&str> = r.refs.iter().map(|x| x.reference_name.as_str()).collect();
        assert!(
            refs.contains(&"auth")
                && refs.contains(&"users.show")
                && refs.contains(&"signIn")
                && refs.contains(&"listBooks")
        );
    }

    #[test]
    fn nest() {
        let src = "@Controller('users')\nexport class UsersController {\n  @Get(':id')\n  async findOne(@Param('id') id: string) {}\n  @Post()\n  create() {}\n}\n";
        let mut r = Routes::default();
        nest_routes("u.ts", Language::TypeScript, src, &mut r);
        assert_eq!(names(&r), vec!["GET /users/:id", "POST /users"]);
        assert_eq!(r.refs[0].reference_name, "UsersController::findOne");
    }

    #[test]
    fn object_and_jsx_routes() {
        let src = "const routes = [{ path: '/admin', component: AdminLayout, children: [ { path: 'users', component: UserList } ] }];\n<Route path=\"/about\" element={<About />} />";
        let mut r = Routes::default();
        object_routes("r.tsx", Language::Tsx, src, &mut r, "react-router");
        assert_eq!(names(&r), vec!["/admin", "/admin/users", "/about"]);
        let refs: Vec<&str> = r.refs.iter().map(|x| x.reference_name.as_str()).collect();
        assert_eq!(refs, vec!["AdminLayout", "UserList", "About"]);
    }

    #[test]
    fn file_based() {
        let mut r = Routes::default();
        nextjs_routes(
            "app/(shop)/products/[id]/page.tsx",
            Language::Tsx,
            "export default function ProductPage() {}",
            &mut r,
        );
        nextjs_routes(
            "app/api/items/route.ts",
            Language::TypeScript,
            "export async function GET() {}\nexport const POST = async () => {}",
            &mut r,
        );
        nextjs_routes(
            "pages/index.tsx",
            Language::Tsx,
            "export default Home",
            &mut r,
        );
        nextjs_routes("pages/_app.tsx", Language::Tsx, "", &mut r);
        sveltekit_routes(
            "src/routes/blog/[slug]/+page.svelte",
            Language::Svelte,
            "",
            &mut r,
        );
        file_routes(
            "app/routes/posts.$id.tsx",
            Language::Tsx,
            "export default function Post() {}",
            &mut r,
            "remix",
        );
        assert_eq!(
            names(&r),
            vec![
                "/products/[id]",
                "GET /api/items",
                "POST /api/items",
                "/",
                "/blog/[slug]",
                "/posts/:id"
            ]
        );
        assert_eq!(r.refs[0].reference_name, "ProductPage");
    }
}
