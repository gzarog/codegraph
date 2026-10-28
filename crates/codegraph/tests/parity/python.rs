//! Upstream `__tests__/python-*.test.ts`.

use crate::harness::{assert_has, assert_lacks, Project};
use codegraph_core::EdgeKind;

/// python-local-binding: a bare call to a name the function binds itself is
/// that value, not another file's `def` of the name.
#[test]
fn local_binding() {
    let p = Project::new(&[
        ("tests/test_testing.py", "def view(request):\n    return request\n"),
        (
            "tests/test_versioning.py",
            "class TestVersion:\n    def test_version(self, request):\n        view = MyView.as_view()\n        response = view(request)\n        return response\n",
        ),
        (
            "tests/conftest.py",
            "import pytest\n\n\n@pytest.fixture\ndef user_factory():\n    def make():\n        return 1\n    return make\n",
        ),
        (
            "tests/test_users.py",
            "def test_user(user_factory):\n    user = user_factory()\n    return user\n",
        ),
        ("app/utils.py", "def user_display(user):\n    return str(user)\n"),
        (
            "app/tags.py",
            "from app.utils import user_display\n\n\ndef user_display_tag(user):\n    \"\"\"\n    {% user_display user as user_display %}\n    \"\"\"\n    return user_display(user)\n",
        ),
    ]);
    assert_lacks(
        &p.calls_from("tests/test_versioning.py"),
        "tests/test_testing.py: view",
    );
    assert_has(
        &p.calls_from("tests/test_users.py"),
        "tests/conftest.py: user_factory",
    );
    assert_has(&p.calls_from("app/tags.py"), "app/utils.py: user_display");
}

/// python-quoted-annotation: `def f(o: "Alpha")` types `o` like `o: Alpha`.
#[test]
fn quoted_annotation() {
    let p = Project::new(&[
        ("pkg/__init__.py", ""),
        (
            "pkg/a.py",
            "def render(x):\n    return x\n\nclass Alpha:\n    def render(self):\n        return \"a\"\n\nclass Beta:\n    def render(self):\n        return \"b\"\n",
        ),
        (
            "pkg/b.py",
            "from __future__ import annotations\nfrom pkg.a import Alpha, Beta\n\ndef quoted(o: \"Alpha\"):\n    return o.render()\n\ndef single_quoted(o: 'Beta'):\n    return o.render()\n\ndef unquoted(o: Alpha):\n    return o.render()\n",
        ),
    ]);
    assert_eq!(p.callees(&p.function("unquoted")), vec!["Alpha::render"]);
    assert_eq!(p.callees(&p.function("quoted")), vec!["Alpha::render"]);
    assert_eq!(
        p.callees(&p.function("single_quoted")),
        vec!["Beta::render"]
    );
}

const PKG: &[(&str, &str)] = &[
    ("app/__init__.py", ""),
    ("app/email.py", "def send():\n    return 1\n"),
];

fn with_pkg<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut v: Vec<(&str, &str)> = PKG.to_vec();
    v.extend_from_slice(extra);
    v
}

/// python-from-import-module-name: `from app import email` never links a
/// variable named `app`.
#[test]
fn from_import_module_is_never_a_symbol() {
    for extra in [
        ("app/main.py", "app = object()\n"),
        ("server.py", "app = object()\n"),
    ] {
        let p = Project::new(&with_pkg(&[
            extra,
            (
                "tests/test_email.py",
                "from app import email\n\n\ndef test_send():\n    email.send()\n",
            ),
        ]));
        let out = p.imports_of_file("tests/test_email.py");
        assert!(out.iter().all(|e| !e.starts_with("variable:")), "{out:?}");
        assert_has(&out, "file:email.py@app/email.py");
    }
    for code in [
        "from app \\\n    import email\n",
        "from \\\n    app import email\n",
        "from typing import TYPE_CHECKING\n\nif TYPE_CHECKING:\n    from app import email\n",
    ] {
        let p = Project::new(&with_pkg(&[
            ("app/main.py", "app = object()\n"),
            ("tests/test_email.py", code),
        ]));
        let out = p.imports_of_file("tests/test_email.py");
        assert!(
            out.iter().all(|e| !e.starts_with("variable:")),
            "{code}: {out:?}"
        );
    }
    let p = Project::new(&with_pkg(&[
        ("app/main.py", "app = object()\n"),
        ("run.py", "from app.main import app\n"),
    ]));
    assert_has(&p.imports_of_file("run.py"), "variable:app@app/main.py");
}

/// python-package-reexports: names imported from a package resolve through
/// its `__init__.py` re-exports to their definitions.
#[test]
fn package_reexports() {
    let p = Project::new(&[
        ("netbox/users/__init__.py", ""),
        (
            "netbox/users/models/__init__.py",
            "from .tokens import Token\nfrom .users import *\n",
        ),
        ("netbox/users/models/users.py", "class User:\n    pass\n"),
        ("netbox/users/models/tokens.py", "class Token:\n    pass\n"),
        ("netbox/extras/tests/test_templatetags.py", "class User:\n    pass\n"),
        ("netbox/alembic/versions/0001_tokens.py", "class Token:\n    pass\n"),
        ("netbox/netbox/__init__.py", ""),
        ("netbox/netbox/views/__init__.py", ""),
        (
            "netbox/netbox/views/generic/__init__.py",
            "from .object_views import *\n",
        ),
        (
            "netbox/netbox/views/generic/object_views.py",
            "class ObjectEditView:\n    pass\n",
        ),
        (
            "netbox/dcim/views.py",
            "from netbox.views import generic\n\n\nclass DeviceEditView(generic.ObjectEditView):\n    pass\n",
        ),
        ("netbox/extras/views.py", "class ObjectEditView:\n    pass\n"),
        (
            "netbox/core/views.py",
            "from users.models import Token, User\n\n\ndef make():\n    return User(), Token()\n",
        ),
    ]);
    let bases: Vec<String> = p
        .file_targets("netbox/dcim/views.py", EdgeKind::Extends)
        .into_iter()
        .map(|n| n.file_path)
        .collect();
    assert_eq!(bases, vec!["netbox/netbox/views/generic/object_views.py"]);
    let mut made: Vec<String> = p
        .file_targets("netbox/core/views.py", EdgeKind::Instantiates)
        .into_iter()
        .map(|n| n.file_path)
        .collect();
    made.sort();
    assert_eq!(
        made,
        vec![
            "netbox/users/models/tokens.py",
            "netbox/users/models/users.py"
        ]
    );
}

/// python-receiver-guessing: a call whose receiver is unknown is not a
/// project method picked by name.
#[test]
fn receiver_guessing() {
    let p = Project::new(&[
        (
            "app/tests.py",
            "class AuthTestCase:\n    def get(self, url):\n        return url\n\n    def login(self, name):\n        return name\n",
        ),
        ("app/curl.py", "def get(url):\n    return url\n"),
        ("app/helpers.py", "def slugify(text):\n    return text\n"),
        ("app/tags.py", "def render(context):\n    return context\n"),
        (
            "app/pages.py",
            "import app.helpers\nfrom django.shortcuts import render\n\ndef page(request):\n    kind = request.POST.get(\"kind\")\n    slug = app.helpers.slugify(kind)\n    return render(request, \"page.html\", {\"slug\": slug})\n",
        ),
        (
            "app/views.py",
            "from django.contrib.auth.models import User\n\nclass Backend:\n    def authenticate(self, username):\n        user = User.objects.get(email=username)\n        self.client.login(username=\"a\")\n        self.helper()\n        self.assertEqual(self.filterset(params, self.queryset).qs.get(), 2)\n        get(1)\n        return user\n\n    def helper(self):\n        return 1\n",
        ),
    ]);
    let targets: Vec<String> = p
        .targets(&p.callable("authenticate"), EdgeKind::Calls)
        .into_iter()
        .map(|n| n.qualified_name)
        .collect();
    assert_lacks(&targets, "AuthTestCase::get");
    assert_lacks(&targets, "AuthTestCase::login");
    assert_has(&targets, "Backend::helper");
    let page: Vec<String> = p
        .targets(&p.callable("page"), EdgeKind::Calls)
        .into_iter()
        .map(|n| format!("{}:{}", n.file_path, n.name))
        .collect();
    assert_lacks(&page, "app/curl.py:get");
    assert_lacks(&page, "app/tags.py:render");
    assert_has(&page, "app/helpers.py:slugify");
}

const CORPUS: &[(&str, &str)] = &[
    ("pkg/__init__.py", ""),
    ("pkg/corpus.py", "def corpus_pass(root):\n    return root\n"),
];

/// python-local-import-alias: `import a.b as X` inside a function binds X there.
#[test]
fn local_import_alias() {
    for code in [
        "def check():\n    import pkg.corpus as CP\n    return CP.corpus_pass(\"x\")\n",
        "class T:\n    def check(self):\n        import pkg.corpus as CP\n        return CP.corpus_pass(\"x\")\n",
        "def check():\n    if True:\n        import pkg.corpus as CP\n    return CP.corpus_pass(\"x\")\n",
        "def check():\n\timport pkg.corpus as CP\n\treturn CP.corpus_pass(\"x\")\n",
        "import pkg.corpus as CP\n\n\ndef check():\n    return CP.corpus_pass(\"x\")\n",
    ] {
        let mut files = CORPUS.to_vec();
        files.push(("test_local.py", code));
        let p = Project::new(&files);
        assert_eq!(p.calls_of("check"), vec!["corpus_pass@pkg/corpus.py"], "{code}");
    }
    let p = Project::new(&[
        ("x/__init__.py", ""),
        ("x/y.py", "def corpus_pass(r):\n    return r\n"),
        ("z/__init__.py", ""),
        ("z/w.py", "def corpus_pass(r):\n    return r\n"),
        (
            "m.py",
            "def a():\n    import x.y as m\n    return m.corpus_pass(1)\n\n\ndef b():\n    import z.w as m\n    return m.corpus_pass(2)\n",
        ),
    ]);
    assert_eq!(p.calls_of("a"), vec!["corpus_pass@x/y.py"]);
    assert_eq!(p.calls_of("b"), vec!["corpus_pass@z/w.py"]);
    let mut files = CORPUS.to_vec();
    files.push((
        "m.py",
        "def a():\n    import pkg.corpus as CP\n    return CP.corpus_pass(1)\n\n\ndef b(CP):\n    return CP.corpus_pass(2)\n",
    ));
    assert!(Project::new(&files).calls_of("b").is_empty());
    let mut files = CORPUS.to_vec();
    files.push((
        "cp.py",
        "class C:\n    def corpus_pass(self, r):\n        return r\n\n\nCP = C()\n",
    ));
    files.push((
        "m.py",
        "from cp import CP\n\n\ndef b():\n    return CP.corpus_pass(2)\n\n\ndef a():\n    import pkg.corpus as CP\n    return CP.corpus_pass(1)\n",
    ));
    let p = Project::new(&files);
    assert_eq!(p.calls_of("a"), vec!["corpus_pass@pkg/corpus.py"]);
    assert_eq!(p.calls_of("b"), vec!["corpus_pass@cp.py"]);
    let p = Project::new(&[
        ("backends/__init__.py", ""),
        ("backends/jinja2.py", "def from_string(x):\n    return x\n"),
        (
            "m.py",
            "def f():\n    try:\n        import jinja2 as j\n    except ImportError:\n        return None\n    return j.from_string(\"x\")\n",
        ),
    ]);
    assert!(p.calls_of("f").is_empty());
}

/// python-self-call-not-import: `self.get_ip()` is the method, not the
/// imported function of the name.
#[test]
fn self_call_not_import() {
    let httpkit = "def get_ip(request):\n    return \"1.2.3.4\"\n";
    let p = Project::new(&[
        ("pkg/__init__.py", ""),
        ("pkg/httpkit.py", httpkit),
        (
            "pkg/adapter.py",
            "from pkg.httpkit import get_ip\n\n\nclass Adapter:\n    def get_ip(self, request):\n        return get_ip(request)\n\n    def notify(self, request):\n        return self.get_ip(request)\n",
        ),
    ]);
    assert_eq!(
        p.all_calls(),
        vec![
            "Adapter::get_ip -> get_ip (pkg/httpkit.py)",
            "Adapter::notify -> Adapter::get_ip (pkg/adapter.py)",
        ]
    );
    let p = Project::new(&[
        ("pkg/__init__.py", ""),
        ("pkg/httpkit.py", httpkit),
        (
            "pkg/base.py",
            "class Base:\n    def get_ip(self, request):\n        return \"base\"\n",
        ),
        (
            "pkg/adapter.py",
            "from pkg.base import Base\nfrom pkg.httpkit import get_ip\n\n\nclass Adapter(Base):\n    def notify(self, request):\n        return self.get_ip(request)\n\n    def raw(self, request):\n        return get_ip(request)\n",
        ),
    ]);
    assert_eq!(
        p.all_calls(),
        vec![
            "Adapter::notify -> Base::get_ip (pkg/base.py)",
            "Adapter::raw -> get_ip (pkg/httpkit.py)",
        ]
    );
}

const FAKE: &str = "class Fake:\n    def run(self, name):\n        return name\n\n    def join(self, *parts):\n        return parts\n";

/// python-external-module-receiver: a call on an outside module is never a
/// project method of the name.
#[test]
fn external_module_receiver() {
    let app = |code: &str| Project::new(&[("app.py", &format!("{code}\n\n{FAKE}"))]);
    for code in [
        "import subprocess\n\n\ndef use():\n    return subprocess.run([\"ls\"])\n",
        "import subprocess as sp\n\n\ndef use():\n    return sp.run([\"ls\"])\n",
        "def use():\n    import subprocess\n    return subprocess.run([\"ls\"])\n",
    ] {
        let out = app(code).call_kinds("use");
        assert!(
            !out.iter().any(|e| e.starts_with("method:Fake::")),
            "{code}: {out:?}"
        );
    }
    let p = app("import subprocess\n\n\ndef use():\n    f = Fake()\n    subprocess.run([\"ls\"])\n    return f.run(\"x\")\n\n");
    assert_eq!(p.call_kinds("use"), vec!["method:Fake::run"]);
    let p = app("import subprocess\n\nsubprocess = Fake()\n\n\ndef use():\n    return subprocess.run(\"x\")\n\n");
    assert_eq!(p.call_kinds("use"), vec!["method:Fake::run"]);
    for prelude in [
        "import subprocess\n\nsubprocess, other = Fake(), 1\n",
        "import subprocess\n\n\ndef init():\n    global subprocess\n    subprocess = Fake()\n",
        "import subprocess\n\nfor subprocess in [Fake()]:\n    pass\n",
        "try:\n    from fastlib import subprocess\nexcept ImportError:\n    class subprocess:\n        @staticmethod\n        def run(name):\n            return name\n",
    ] {
        let out = app(&format!("{prelude}\n\ndef use():\n    return subprocess.run(\"x\")\n\n")).call_kinds("use");
        assert_eq!(out.len(), 1, "{prelude}: {out:?}");
    }
    let p = Project::new(&[
        ("__init__.py", ""),
        (
            "tasks.py",
            "class Runner:\n    def go(self, x):\n        return x\n\n\nrunner = Runner()\n",
        ),
        (
            "app.py",
            "from mypkg.tasks import runner\n\n\ndef use():\n    return runner.go(\"x\")\n",
        ),
        (
            "other.py",
            "class Other:\n    def go(self, x):\n        return None\n",
        ),
    ]);
    assert_eq!(p.call_kinds("use"), vec!["method:Runner::go"]);
    let p = Project::new(&[
        ("tasks.py", "def run(name):\n    return name\n"),
        (
            "app.py",
            "import tasks\n\n\ndef use():\n    return tasks.run(\"x\")\n",
        ),
    ]);
    assert_eq!(p.call_kinds("use"), vec!["function:run"]);
}

/// python-receiver-guess: a guess by method name is never a dispatched view
/// handler or an unnamed test double; a Django manager call types its result.
#[test]
fn receiver_guess() {
    let p = Project::new(&[
        (
            "app/views.py",
            "from django.views import View\n\n\nclass ClientRegistrationView(View):\n    def post(self, request):\n        return None\n\n\nclass SessionView(View):\n    def post(self, request):\n        return None\n",
        ),
        (
            "app/models.py",
            "from django.db import models\n\n\nclass Site(models.Model):\n    def save(self, *args, **kwargs):\n        super().save(*args, **kwargs)\n\n\nclass Rack(models.Model):\n    def save(self, *args, **kwargs):\n        super().save(*args, **kwargs)\n",
        ),
        (
            "tests/fakes.py",
            "class _FakeHTTPResponse:\n    def json(self):\n        return {}\n\n\nclass Payload:\n    def json(self):\n        return {}\n",
        ),
        (
            "tests/test_views.py",
            "from app.models import Site\n\n\ndef test_login(client):\n    resp = client.post(\"/login\")\n    assert resp.status_code == 200\n\n\ndef test_api(api_client):\n    response = api_client.get(\"/recipes\")\n    data = response.json()\n    assert data\n\n\ndef test_site():\n    site = Site.objects.create(name=\"a\")\n    site.save()\n",
        ),
    ]);
    let calls: Vec<String> = p
        .file_targets("tests/test_views.py", EdgeKind::Calls)
        .into_iter()
        .map(|n| n.qualified_name)
        .collect();
    assert!(!calls.iter().any(|q| q.ends_with("::post")), "{calls:?}");
    assert_lacks(&calls, "_FakeHTTPResponse::json");
    assert_has(&calls, "Site::save");
    assert_lacks(&calls, "Rack::save");
}

/// python-module-scope-collection-methods: a module-level dict, list, set
/// or tuple keeps its builtin methods off project classes.
#[test]
fn module_scope_collection_methods() {
    let collections = [
        ("dict_literal", "{\"answer\": \"42\"}", "get"),
        ("empty_dict", "{}", "get"),
        ("dict_constructor", "dict()", "get"),
        ("list_literal", "[1]", "append"),
        ("empty_list", "[]", "append"),
        ("list_constructor", "list()", "append"),
        ("set_literal", "{1}", "add"),
        ("set_constructor", "set()", "add"),
        ("tuple_literal", "(1,)", "index"),
        ("empty_tuple", "()", "index"),
        ("tuple_constructor", "tuple()", "index"),
        ("frozenset_constructor", "frozenset()", "union"),
    ];
    let mut files: Vec<(String, String)> = vec![
        (
            "settings.py".into(),
            "DEFAULTS = {\"answer\": \"42\"}\n\ndef read_setting(name):\n    return DEFAULTS.get(name, None)\n".into(),
        ),
        (
            "cache.py".into(),
            "class LRUCache:\n    def __init__(self):\n        self._store = {}\n\n    def get(self, key):\n        return self._store.get(key)\n\nclass ProjectCollection:\n    def append(self, item):\n        pass\n    def add(self, item):\n        pass\n    def index(self, item):\n        return 0\n    def union(self, item):\n        return self\n".into(),
        ),
        (
            "unknown.py".into(),
            "UNKNOWN = load_defaults()\n\ndef read_unknown(name):\n    return UNKNOWN.get(name)\n".into(),
        ),
        (
            "client.py".into(),
            "from cache import LRUCache\n\ndef read_cache(lRUCache: LRUCache, name):\n    return lRUCache.get(name)\n".into(),
        ),
    ];
    for (name, value, method) in collections {
        files.push((
            format!("{name}.py"),
            format!(
                "lRUCache = {value}\n\ndef use_{name}(item):\n    return lRUCache.{method}(item)\n"
            ),
        ));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let p = Project::new(&refs);
    let method = |name: &str| {
        p.named(name)
            .into_iter()
            .find(|n| n.kind == codegraph_core::NodeKind::Method && n.file_path == "cache.py")
            .unwrap()
    };
    let caller = |name: &str, file: &str| {
        p.named(name)
            .into_iter()
            .find(|n| n.kind == codegraph_core::NodeKind::Function && n.file_path == file)
            .unwrap()
    };
    let lacks = |c: &str, file: &str, m: &str| {
        let ids = p.callee_ids(&caller(c, file));
        assert!(!ids.contains(&method(m).id), "{c} -> {m}");
    };
    lacks("read_setting", "settings.py", "get");
    for (name, _, m) in collections {
        lacks(&format!("use_{name}"), &format!("{name}.py"), m);
    }
    lacks("read_unknown", "unknown.py", "get");
    let ids = p.callee_ids(&caller("read_cache", "client.py"));
    assert!(ids.contains(&method("get").id));
}

/// python-module-global-receiver-call: a call through a module-level
/// instance (`_svc = SceneService()`) reaches that class's method.
#[test]
fn module_global_receiver_call() {
    let base = "class GenericCRUDService:\n    def delete(self, obj):\n        return obj\n";
    let other = "class Unrelated:\n    def delete(self, obj):\n        return None\n\n    def purge(self):\n        return None\n";
    let svc = |body: &str| {
        format!("from base import GenericCRUDService\n\n\nclass SceneService(GenericCRUDService):\n    def purge(self):\n        return 1\n\n\n_svc = SceneService()\n\n\n{body}\n")
    };
    for (body, expected) in [
        (
            "def use(obj):\n    _svc.delete(obj)",
            "GenericCRUDService::delete@base.py",
        ),
        ("def use():\n    _svc.purge()", "SceneService::purge@svc.py"),
    ] {
        let s = svc(body);
        let p = Project::new(&[("base.py", base), ("other.py", other), ("svc.py", &s)]);
        assert_eq!(p.calls_and_news("use"), vec![expected], "{body}");
    }
    let s = svc("def use(_svc):\n    _svc.purge()");
    let p = Project::new(&[("base.py", base), ("other.py", other), ("svc.py", &s)]);
    assert_lacks(&p.calls_and_news("use"), "SceneService::purge@svc.py");

    let unrelated = "class Unrelated:\n    def handler(self):\n        return None\n";
    for (code, expected) in [
        ("class SceneService:\n    def __init__(self, h):\n        self.handler = h\n\n\n_svc = SceneService(print)\n\n\ndef use():\n    _svc.handler()\n", vec!["Unrelated::handler@other.py"]),
        ("class SceneService:\n    handler = None\n\n\n_svc = SceneService()\n\n\ndef use():\n    _svc.handler()\n", vec!["Unrelated::handler@other.py"]),
        ("class SceneService:\n    @property\n    def handler(self):\n        return len\n\n\n_svc = SceneService()\n\n\ndef use():\n    _svc.handler()\n", vec![]),
        ("from thirdparty import Session\n\n\nclass SceneService(Session):\n    pass\n\n\n_svc = SceneService()\n\n\ndef use():\n    _svc.handler()\n", vec!["Unrelated::handler@other.py"]),
        ("class A:\n    def handler(self):\n        return 1\n\n\nclass B:\n    pass\n\n\nif X:\n    _svc = A()\nelse:\n    _svc = B()\n\n\ndef use():\n    _svc.handler()\n", vec![]),
    ] {
        let p = Project::new(&[("other.py", unrelated), ("svc.py", code)]);
        assert_eq!(p.calls_and_news("use"), expected, "{code}");
    }

    let z = "class Z:\n    def run(self):\n        return 3\n";
    let p = Project::new(&[
        ("other.py", z),
        ("svc.py", "class A:\n    def run(self):\n        return 1\n\n\n_svc = A()\n\n\ndef use():\n    class _svc:\n        def run(self):\n            return 2\n\n    _svc.run()\n"),
    ]);
    assert_eq!(p.calls_and_news("use"), vec!["use::_svc::run@svc.py"]);

    for (classes, expected) in [
        ("class Base:\n    def run(self):\n        return 1\n\n\nclass A(\n    Base,\n):\n    pass\n", "Base::run@svc.py"),
        ("from typing import Generic, TypeVar\n\nT = TypeVar(\"T\")\n\n\nclass Base(Generic[T]):\n    def run(self):\n        return 1\n\n\nclass A(Base[int]):\n    pass\n", "Base::run@svc.py"),
        ("class A:\n    def run(self):\n        return 1\n\n    def other(self):\n        return dict(\n            run=1,\n        )\n", "A::run@svc.py"),
    ] {
        let code = format!("{classes}\n\n_svc = A()\n\n\ndef use():\n    _svc.run()\n");
        let p = Project::new(&[("svc.py", &code)]);
        assert_eq!(p.calls_and_news("use"), vec![expected], "{classes}");
    }

    let p = Project::new(&[
        ("svc.py", "class A:\n    def run(self, n):\n        if n:\n            _svc.run(n - 1)\n\n\n_svc = A()\n"),
        ("other.py", "class Z:\n    def run(self, n):\n        return n\n"),
    ]);
    let run = p
        .named("run")
        .into_iter()
        .find(|n| n.qualified_name == "A::run")
        .unwrap();
    assert_eq!(p.callees(&run), vec!["A::run"]);

    let p = Project::new(&[
        ("other.py", z),
        ("svc.py", "class A:\n    def run(self):\n        return 1\n\n\n_svc = A()\n\n\ndef use():\n    def helper():\n        class _svc:\n            def run(self):\n                return 2\n\n        return _svc\n\n    _svc.run()\n"),
    ]);
    assert_eq!(p.calls_and_news("use"), vec!["A::run@svc.py"]);
}

/// python-function-value-scope: a name passed as a value is the local or
/// module symbol in scope, never a function nested elsewhere.
#[test]
fn function_value_scope() {
    let p = Project::new(&[
        ("httpx/_client.py", "class Client:\n    @property\n    def auth(self):\n        return self._auth\n\n    def _build_auth(self, auth):\n        return auth\n\n    def set_auth(self, auth):\n        self._auth = self._build_auth(auth)\n"),
        ("httpx/_api.py", "def request(method, url):\n    return method, url\n"),
        ("httpx/_auth.py", "class Auth:\n    def auth_flow(self, request):\n        yield request\n\n    def sync_auth_flow(self, request):\n        flow = self.auth_flow(request)\n        return flow\n\n\nclass Recorder:\n    def request(self):\n        return None\n"),
        ("tests/test_auth.py", "def test_custom_auth():\n    def auth(request):\n        return request\n\n    return register(auth)\n\n\ndef test_basic_auth():\n    auth = (\"user\", \"pass\")\n    return register(auth)\n"),
        ("docs/example/test_order.py", "import pytest\n\n\n@pytest.fixture\ndef func():\n    return 1\n"),
        ("tests/test_coverage.py", "import pytest\n\n\ndef _run_both(func):\n    return func()\n\n\n@pytest.fixture(\n    scope=\"module\",\n)\ndef recipe():\n    return {}\n\n\ndef test_recipe(recipe):\n    return use(recipe)\n"),
    ]);
    let nested = "tests/test_auth.py:test_custom_auth::auth";
    assert_lacks(&p.refs_from("httpx/_client.py", None), nested);
    assert_lacks(
        &p.refs_from("tests/test_auth.py", Some("test_basic_auth")),
        nested,
    );
    assert_has(
        &p.refs_from("tests/test_auth.py", Some("test_custom_auth")),
        nested,
    );
    assert_lacks(
        &p.refs_from("httpx/_auth.py", None),
        "httpx/_api.py:request",
    );
    assert_lacks(
        &p.refs_from("tests/test_coverage.py", Some("_run_both")),
        "docs/example/test_order.py:func",
    );
    assert_has(
        &p.refs_from("tests/test_coverage.py", Some("test_recipe")),
        "tests/test_coverage.py:recipe",
    );
}

const LOADER: &str = "class Registry:\n    def get(self, k):\n        return k\n\n\ndef load_registry():\n    return Registry()\n";

/// python-from-import-module-file: the module of a from-import links its
/// file, as `import x` does.
#[test]
fn from_import_module_file() {
    let p = Project::new(&[
        ("loader.py", LOADER),
        ("packs.py", "from loader import Registry, load_registry\n\n\ndef use():\n    return load_registry()\n"),
        ("plain.py", "import loader\n\n\ndef use():\n    return loader.load_registry()\n"),
    ]);
    assert_eq!(
        p.imports_of_file("packs.py"),
        vec![
            "class:Registry@loader.py",
            "file:loader.py@loader.py",
            "function:load_registry@loader.py"
        ]
    );
    assert_eq!(
        p.imports_of_file("plain.py"),
        vec!["file:loader.py@loader.py"]
    );

    let p = Project::new(&[
        ("pkg/__init__.py", "def helper():\n    return 1\n"),
        ("pkg/sub/__init__.py", "def deep():\n    return 2\n"),
        (
            "app.py",
            "from pkg import helper\nfrom pkg.sub import deep\n",
        ),
    ]);
    assert_eq!(
        p.imports_of_file("app.py"),
        vec![
            "file:__init__.py@pkg/__init__.py",
            "file:__init__.py@pkg/sub/__init__.py",
            "function:deep@pkg/sub/__init__.py",
            "function:helper@pkg/__init__.py",
        ]
    );

    let p = Project::new(&[
        ("pyproject.toml", "[project]\nname = \"app\"\n"),
        ("src/app/__init__.py", "def helper():\n    return 1\n"),
        (
            "tests/test_app.py",
            "from app import helper\n\n\ndef test_helper():\n    assert helper() == 1\n",
        ),
    ]);
    assert_eq!(
        p.imports_of_file("tests/test_app.py"),
        vec![
            "file:__init__.py@src/app/__init__.py",
            "function:helper@src/app/__init__.py"
        ]
    );

    let p = Project::new(&[
        ("loader.py", LOADER),
        ("packs.py", "from \\\n    loader import Registry\n"),
    ]);
    assert_eq!(
        p.imports_of_file("packs.py"),
        vec!["class:Registry@loader.py", "file:loader.py@loader.py"]
    );

    let p = Project::new(&[
        ("tests/fixtures/requests.py", "class Session:\n    pass\n"),
        (
            "client.py",
            "from requests import Session\nimport requests\n",
        ),
    ]);
    assert!(p.imports_of_file("client.py").is_empty());

    let p = Project::new(&[
        ("pkg/__init__.py", ""),
        ("pkg/loader.py", LOADER),
        ("app.py", "from pkg import loader\n"),
    ]);
    assert_eq!(
        p.imports_of_file("app.py"),
        vec![
            "file:__init__.py@pkg/__init__.py",
            "file:loader.py@pkg/loader.py"
        ]
    );

    let mut p = Project::new(&[("packs.py", "from loader import Registry\n")]);
    assert!(p.imports_of_file("packs.py").is_empty());
    p.write(&[("loader.py", LOADER)]);
    p.sync();
    let synced = p.imports_of_file("packs.py");
    p.reindex();
    assert_eq!(synced, p.imports_of_file("packs.py"));
    assert_eq!(
        synced,
        vec!["class:Registry@loader.py", "file:loader.py@loader.py"]
    );
}

/// python-from-import-module-name: the module of a from-import is never a
/// variable named like the package.
#[test]
fn from_import_module_name() {
    let pkg = [
        ("app/__init__.py", ""),
        ("app/email.py", "def send():\n    return 1\n"),
    ];
    for extra in [
        ("app/main.py", "app = object()\n"),
        ("server.py", "app = object()\n"),
    ] {
        let mut files = pkg.to_vec();
        files.push(extra);
        files.push((
            "tests/test_email.py",
            "from app import email\n\n\ndef test_send():\n    email.send()\n",
        ));
        let out = Project::new(&files).imports_of_file("tests/test_email.py");
        assert!(!out.iter().any(|e| e.starts_with("variable:")), "{out:?}");
        assert_has(&out, "file:email.py@app/email.py");
    }
    for code in [
        "from app \\\n    import email\n",
        "from \\\n    app import email\n",
        "from typing import TYPE_CHECKING\n\nif TYPE_CHECKING:\n    from app import email\n",
    ] {
        let mut files = pkg.to_vec();
        files.push(("app/main.py", "app = object()\n"));
        files.push(("tests/test_email.py", code));
        let out = Project::new(&files).imports_of_file("tests/test_email.py");
        assert!(
            !out.iter().any(|e| e.starts_with("variable:")),
            "{code}: {out:?}"
        );
    }
    let mut files = pkg.to_vec();
    files.push(("app/main.py", "app = object()\n"));
    files.push(("run.py", "from app.main import app\n"));
    assert_has(
        &Project::new(&files).imports_of_file("run.py"),
        "variable:app@app/main.py",
    );
}

/// python-resolution-work (behaviour; upstream also counts file reads): a
/// method value on a global typed by another module's writes, and bare
/// calls a function shadows.
#[test]
fn resolution_work() {
    let conns: Vec<String> = (0..16).map(|i| format!("conn{i}")).collect();
    let settings = format!(
        "from store import Store\n{}\ndef init():\n    global {}\n{}",
        conns
            .iter()
            .map(|c| format!("{c} = None\n"))
            .collect::<String>(),
        conns.join(", "),
        conns
            .iter()
            .map(|c| format!("    {c} = Store()\n"))
            .collect::<String>()
    );
    let consumer = format!(
        "import settings\n{}",
        conns
            .iter()
            .enumerate()
            .map(|(i, c)| format!("\ndef cb{i}(pool):\n    pool.submit(settings.{c}.fetch)\n"))
            .collect::<String>()
    );
    let callers = (0..16)
        .map(|i| format!("def f{i}():\n    return helper()\n\n"))
        .collect::<String>()
        + "def shadowed(make):\n    helper = make()\n    return helper()\n";
    let p = Project::new(&[
        (
            "store.py",
            "class Store:\n    def fetch(self, ids):\n        return ids\n",
        ),
        ("settings.py", &settings),
        ("consumer.py", &consumer),
        ("helpers.py", "def helper():\n    return 1\n"),
        ("callers.py", &callers),
    ]);
    let fetch = p
        .named("fetch")
        .into_iter()
        .find(|n| n.qualified_name == "Store::fetch")
        .unwrap();
    let mut refs: Vec<String> =
        p.cg.store()
            .incoming_edges(&fetch.id, &[EdgeKind::References, EdgeKind::Calls])
            .unwrap()
            .iter()
            .filter_map(|e| p.node(&e.source))
            .map(|n| n.name)
            .collect();
    refs.sort();
    let mut want: Vec<String> = (0..16).map(|i| format!("cb{i}")).collect();
    want.sort();
    assert_eq!(refs, want);
    let helper = p.function("helper");
    let mut callers: Vec<String> =
        p.cg.store()
            .incoming_edges(&helper.id, &[EdgeKind::Calls])
            .unwrap()
            .iter()
            .filter_map(|e| p.node(&e.source))
            .map(|n| n.name)
            .collect();
    callers.sort();
    let mut want: Vec<String> = (0..16).map(|i| format!("f{i}")).collect();
    want.sort();
    assert_eq!(callers, want);
}
