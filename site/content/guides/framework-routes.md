---
title: Framework Routes
description: Which web frameworks CodeGraph understands, and how routes appear in the graph.
---

When a project's manifest (`package.json`, `requirements.txt`, `pyproject.toml`, `go.mod`, `Cargo.toml`, `composer.json`, `Gemfile`, `pom.xml`/`build.gradle`, `*.csproj`, `Package.swift`, `build.sbt`) names a supported framework, CodeGraph turns its routes into `route` nodes linked to their handlers.

| Ecosystem | Frameworks |
|---|---|
| JavaScript / TypeScript | Express, Koa, Fastify, Hono, NestJS, Next.js (pages and app router), SvelteKit, Nuxt, Astro, Remix, Expo Router, React Router, Vue Router, Angular router, TanStack Router |
| Python | Flask (including blueprints), FastAPI (including routers), Django |
| Java / Kotlin | Spring, JAX-RS |
| C# | ASP.NET (attribute routing and minimal APIs) |
| Go | `net/http` and router libraries that register routes as `r.GET(...)`, `HandleFunc(...)` (Gin, Echo, Chi, Fiber, gorilla/mux) |
| Rust | Axum, Actix, Rocket |
| PHP | Laravel, Symfony |
| Ruby | Rails, Sinatra |
| Swift | Vapor |
| Scala | Play |

## What a route looks like

A route node is named by its method and path, such as `GET /users/:id`; file-based page routes are named by path alone, such as `/items/[id]`. It has a `references` edge to its handler: the controller method, view function or page component. So `codegraph callers listUsers` shows the route that serves it, and `codegraph impact listUsers` includes the route.

Prefixes are applied where the framework declares them next to the routes: Flask blueprints, FastAPI routers, NestJS controllers, Go route groups and nested router configurations.

## Requests and navigations

Calls to the project's own routes, such as `fetch('/api/users')` or `router.push('/login')`, are linked to the route they reach. See [dynamic dispatch](/core-concepts/resolution/). The [viewer](/guides/viewer/) uses these links to draw the screen graph and steps.
