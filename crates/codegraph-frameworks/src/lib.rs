//! Framework awareness: HTTP routes, page routes and SPA route tables become
//! `route` nodes, each linked to the code that handles it by a reference the
//! normal resolver binds.

pub mod backend;
pub mod detect;
pub mod js;
pub mod strip;
pub mod util;

pub use detect::ProjectInfo;
pub use util::Routes;

use codegraph_core::Language;

/// A framework CodeGraph knows how to read routes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Framework {
    Express,
    NestJs,
    NextJs,
    SvelteKit,
    Nuxt,
    Astro,
    ExpoRouter,
    Remix,
    ReactRouter,
    VueRouter,
    AngularRouter,
    TanStackRouter,
    Flask,
    FastApi,
    Django,
    Spring,
    JaxRs,
    AspNet,
    GoHttp,
    Axum,
    Actix,
    Laravel,
    Symfony,
    Rails,
    Sinatra,
    Vapor,
    Play,
}

impl Framework {
    pub const ALL: &'static [Framework] = &[
        Framework::Express,
        Framework::NestJs,
        Framework::NextJs,
        Framework::SvelteKit,
        Framework::Nuxt,
        Framework::Astro,
        Framework::ExpoRouter,
        Framework::Remix,
        Framework::ReactRouter,
        Framework::VueRouter,
        Framework::AngularRouter,
        Framework::TanStackRouter,
        Framework::Flask,
        Framework::FastApi,
        Framework::Django,
        Framework::Spring,
        Framework::JaxRs,
        Framework::AspNet,
        Framework::GoHttp,
        Framework::Axum,
        Framework::Actix,
        Framework::Laravel,
        Framework::Symfony,
        Framework::Rails,
        Framework::Sinatra,
        Framework::Vapor,
        Framework::Play,
    ];

    pub fn name(self) -> &'static str {
        use Framework::*;
        match self {
            Express => "express",
            NestJs => "nestjs",
            NextJs => "nextjs",
            SvelteKit => "sveltekit",
            Nuxt => "nuxt",
            Astro => "astro",
            ExpoRouter => "expo-router",
            Remix => "remix",
            ReactRouter => "react-router",
            VueRouter => "vue-router",
            AngularRouter => "angular-router",
            TanStackRouter => "tanstack-router",
            Flask => "flask",
            FastApi => "fastapi",
            Django => "django",
            Spring => "spring",
            JaxRs => "jax-rs",
            AspNet => "aspnet",
            GoHttp => "go-http",
            Axum => "axum",
            Actix => "actix",
            Laravel => "laravel",
            Symfony => "symfony",
            Rails => "rails",
            Sinatra => "sinatra",
            Vapor => "vapor",
            Play => "play",
        }
    }

    /// Whether the project uses this framework.
    pub fn detect(self, p: &ProjectInfo) -> bool {
        use Framework::*;
        match self {
            Express => p.depends_on(&[
                "express",
                "koa",
                "@koa/router",
                "koa-router",
                "fastify",
                "hono",
                "elysia",
                "restify",
                "@hapi/hapi",
                "polka",
            ]),
            NestJs => p.depends_on(&["@nestjs/core", "@nestjs/common"]),
            NextJs => p.depends_on(&["next"]),
            SvelteKit => p.depends_on(&["@sveltejs/kit"]),
            Nuxt => p.depends_on(&["nuxt", "nuxt3"]),
            Astro => p.depends_on(&["astro"]),
            ExpoRouter => p.depends_on(&["expo-router"]),
            Remix => p.depends_on(&["@remix-run/react", "@remix-run/node", "@react-router/dev"]),
            ReactRouter => p.depends_on(&["react-router", "react-router-dom"]),
            VueRouter => p.depends_on(&["vue-router"]),
            AngularRouter => p.depends_on(&["@angular/router"]),
            TanStackRouter => p.depends_on(&[
                "@tanstack/react-router",
                "@tanstack/router",
                "@tanstack/solid-router",
            ]),
            Flask => p.mentions("flask") || p.mentions("quart"),
            FastApi => p.mentions("fastapi") || p.mentions("starlette"),
            Django => {
                p.mentions("django")
                    || p.mentions("djangorestframework")
                    || p.has_file(|f| f == "manage.py" || f.ends_with("/manage.py"))
            }
            Spring => {
                p.mentions("spring-boot-starter-web")
                    || p.mentions("spring-webmvc")
                    || p.mentions("spring-boot-starter-webflux")
                    || p.mentions("org.springframework.boot")
            }
            JaxRs => {
                p.mentions("jakarta.ws.rs-api")
                    || p.mentions("javax.ws.rs-api")
                    || p.mentions("quarkus-resteasy")
                    || p.mentions("micronaut-http")
                    || p.mentions("jersey-server")
            }
            AspNet => {
                p.mentions("microsoft.net.sdk.web")
                    || p.mentions("microsoft.aspnetcore.app")
                    || p.mentions("microsoft.aspnetcore.mvc")
            }
            GoHttp => p.has_file(|f| f.ends_with(".go")),
            Axum => p.mentions("axum"),
            Actix => p.mentions("actix-web") || p.mentions("rocket") || p.mentions("poem"),
            Laravel => {
                p.depends_on(&["laravel/framework", "laravel/lumen-framework"])
                    || p.has_file(|f| f == "routes/web.php" || f == "routes/api.php")
            }
            Symfony => p.depends_on(&["symfony/framework-bundle", "symfony/routing"]),
            Rails => p.mentions("rails") || p.has_file(|f| f == "config/routes.rb"),
            Sinatra => p.mentions("sinatra"),
            Vapor => p.mentions("vapor"),
            Play => p.has_file(|f| f == "conf/routes" || f.ends_with("/conf/routes")),
        }
    }

    fn applies_to(self, lang: Language) -> bool {
        use Framework::*;
        use Language as L;
        match self {
            Express | NestJs | ReactRouter | VueRouter | AngularRouter | TanStackRouter
            | NextJs | ExpoRouter | Remix => lang.is_js_family(),
            SvelteKit => lang.is_js_family() || lang == L::Svelte,
            Nuxt => lang == L::Vue,
            Astro => lang == L::Astro || lang.is_js_family(),
            Flask | FastApi | Django => lang == L::Python,
            Spring | JaxRs => matches!(lang, L::Java | L::Kotlin | L::Scala),
            AspNet => lang == L::CSharp,
            GoHttp => lang == L::Go,
            Axum | Actix => lang == L::Rust,
            Laravel | Symfony => lang == L::Php,
            Rails | Sinatra => lang == L::Ruby,
            Vapor => lang == L::Swift,
            Play => lang == L::Yaml,
        }
    }
}

/// The frameworks a project uses.
pub fn detect(project: &ProjectInfo) -> Vec<Framework> {
    Framework::ALL
        .iter()
        .copied()
        .filter(|f| f.detect(project))
        .collect()
}

/// Routes declared in one file by the detected frameworks.
pub fn extract_routes(
    frameworks: &[Framework],
    file: &str,
    lang: Language,
    content: &str,
) -> Routes {
    use Framework::*;
    let mut routes = Routes::default();
    let active: Vec<Framework> = frameworks
        .iter()
        .copied()
        .filter(|f| f.applies_to(lang))
        .collect();
    if active.is_empty() {
        return routes;
    }
    let safe = strip::strip_comments(content, lang);
    let names: Vec<&str> = active.iter().map(|f| f.name()).collect();
    // Several frameworks share one extractor; run each extractor once.
    let mut ran: Vec<&'static str> = Vec::new();
    let mut once = |key: &'static str| {
        if ran.contains(&key) {
            false
        } else {
            ran.push(key);
            true
        }
    };
    for fw in &active {
        match fw {
            Express => js::server_routes(file, lang, &safe, &mut routes, "express"),
            NestJs => js::nest_routes(file, lang, &safe, &mut routes),
            NextJs => js::nextjs_routes(file, lang, content, &mut routes),
            SvelteKit => js::sveltekit_routes(file, lang, content, &mut routes),
            Nuxt | Astro | ExpoRouter | Remix => {
                js::file_routes(file, lang, content, &mut routes, fw.name())
            }
            ReactRouter | VueRouter | AngularRouter => {
                if once("spa") {
                    js::object_routes(file, lang, &safe, &mut routes, fw.name());
                }
            }
            TanStackRouter => js::tanstack_routes(file, lang, &safe, &mut routes),
            Flask | FastApi | Django => {
                if once("python") {
                    backend::python_routes(file, lang, &safe, &mut routes, &names);
                }
            }
            Spring | JaxRs => {
                if once("jvm") {
                    backend::spring_routes(file, lang, &safe, &mut routes);
                }
            }
            AspNet => backend::aspnet_routes(file, lang, &safe, &mut routes),
            GoHttp => backend::go_routes(file, lang, &safe, &mut routes),
            Axum | Actix => {
                if once("rust") {
                    backend::rust_routes(file, lang, &safe, &mut routes);
                }
            }
            Laravel | Symfony => {
                if once("php") {
                    backend::php_routes(file, lang, &safe, &mut routes);
                }
            }
            Rails | Sinatra => {
                if once("ruby") {
                    backend::ruby_routes(file, lang, &safe, &mut routes);
                }
            }
            Vapor => backend::vapor_routes(file, lang, &safe, &mut routes),
            Play => {
                if file.ends_with("conf/routes") || file.ends_with(".routes") {
                    backend::play_routes(file, lang, content, &mut routes);
                }
            }
        }
    }
    routes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_from_manifests() {
        let mut p = ProjectInfo::default();
        p.add_manifest(
            "package.json",
            r#"{"dependencies": {"express": "^4", "react-router-dom": "6"}}"#,
        );
        p.add_manifest("requirements.txt", "Flask==3.0\nrequests\n");
        let found: Vec<&str> = detect(&p).iter().map(|f| f.name()).collect();
        assert_eq!(found, vec!["express", "react-router", "flask"]);
    }

    #[test]
    fn dispatches_by_language() {
        let r = extract_routes(
            &[Framework::Express, Framework::Flask],
            "a.py",
            Language::Python,
            "@app.route('/x')\ndef x():\n  pass\n",
        );
        assert_eq!(r.nodes.len(), 1);
        let r = extract_routes(
            &[Framework::Express],
            "a.py",
            Language::Python,
            "app.get('/x', h)",
        );
        assert!(r.is_empty());
        // Commented-out routes don't count.
        let r = extract_routes(
            &[Framework::Express],
            "a.js",
            Language::JavaScript,
            "// app.get('/x', h)\n",
        );
        assert!(r.is_empty());
    }
}
