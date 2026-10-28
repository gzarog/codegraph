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
