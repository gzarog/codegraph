//! Upstream `__tests__/python-attr-chain.test.ts`: a call through an
//! attribute chain is typed from evidence, guessed by its last link's
//! name, or left alone.

use crate::harness::Project;
use codegraph_core::NodeKind;

fn method(p: &Project, name: &str) -> Vec<String> {
    let mut v = p.calls_by_kind(NodeKind::Method, name);
    v.sort();
    v
}

fn function(p: &Project, name: &str) -> Vec<String> {
    let mut v = p.calls_by_kind(NodeKind::Function, name);
    v.sort();
    v
}

const NONE: &[&str] = &[];

#[test]
fn self_attribute_chain_is_no_guess() {
    let p = Project::new(&[
        ("sink.py", "class Sink:\n    def append(self, x):\n        return x\n\n    def get(self, k):\n        return k\n"),
        ("box.py", "class Box:\n    def __init__(self):\n        self.data = []\n        self.inner = {}\n\n    def build(self, k):\n        self.data.append(1)\n        rows = {}\n        rows[k] = []\n        rows[k].append(2)\n        return self.inner.get(k)\n"),
    ]);
    assert_eq!(method(&p, "build"), NONE);
}

#[test]
fn deep_chain_is_no_guess() {
    let p = Project::new(&[
        ("leaf.py", "class Leaf:\n    def go(self, x):\n        return x\n"),
        ("deep.py", "class Deep:\n    def __init__(self, a):\n        self.a = a\n\n    def run(self, x):\n        return self.a.b.go(x)\n"),
    ]);
    assert_eq!(method(&p, "run"), NONE);
}

#[test]
fn guess_prefers_the_class_spelled_whole() {
    let p = Project::new(&[
        ("cache.py", "class RedisCacheClient:\n    def get(self, k):\n        return k\n"),
        ("client.py", "class Client:\n    def get(self, k):\n        return k\n"),
        ("nodes.py", "class NodeList:\n    def render(self, c):\n        return c\n\n\nclass Other:\n    def render(self, c):\n        return c\n"),
        ("tests.py", "class Tests:\n    def setUp(self, client, nodelist):\n        self.client = client\n        self.nodelist = nodelist\n\n    def run(self):\n        self.client.get(1)\n        self.nodelist.render(2)\n"),
    ]);
    assert_eq!(
        method(&p, "run"),
        vec!["method:get@client.py", "method:render@nodes.py"]
    );
}

#[test]
fn guess_by_head_noun() {
    let p = Project::new(&[
        ("repos.py", "class GroupRepositoryAIProvider:\n    def create(self):\n        pass\n\n\nclass Other:\n    def create(self):\n        pass\n"),
        ("client.py", "class ClientMixin:\n    def force_login(self, u):\n        return u\n\n\nclass OtherMixin:\n    def force_login(self, u):\n        return u\n"),
        ("factory.py", "class RequestFactory:\n    def get(self, path):\n        return path\n"),
        ("use.py", "def lists(database):\n    database.group_shopping_lists.create()\n\n\ndef providers(database):\n    database.group_ai_providers.create()\n\n\ndef login(case):\n    case.client.force_login(1)\n\n\ndef meta(request):\n    return request.META.get(\"HTTP_HOST\")\n"),
    ]);
    assert_eq!(function(&p, "lists"), NONE);
    assert_eq!(function(&p, "providers"), vec!["method:create@repos.py"]);
    assert_eq!(function(&p, "login"), vec!["method:force_login@client.py"]);
    assert_eq!(function(&p, "meta"), NONE);
}

#[test]
fn chain_through_a_module_stays_the_module() {
    let p = Project::new(&[
        ("storage.py", "class PathNotImplementedStorage:\n    def exists(self, p):\n        return p\n\n\nclass Storage:\n    def exists(self, p):\n        return p\n"),
        ("use.py", "import os\n\n\ndef has(p):\n    return os.path.exists(p)\n"),
    ]);
    assert_eq!(function(&p, "has"), NONE);
}

#[test]
fn untyped_chain_guessed_by_last_link() {
    let p = Project::new(&[
        ("client.py", "class Client:\n    def send(self, x):\n        return x\n"),
        ("other.py", "class Other:\n    def send(self, x):\n        return x\n"),
        ("use.py", "def run(cfg, x):\n    return cfg.client.send(x)\n\n\ndef run_conn(cfg, x):\n    return cfg.conn.send(x)\n"),
    ]);
    assert_eq!(function(&p, "run"), vec!["method:send@client.py"]);
    assert_eq!(function(&p, "run_conn"), NONE);
}

#[test]
fn single_segment_typed_receiver_still_resolves() {
    let p = Project::new(&[
        ("client.py", "class Client:\n    def send(self, payload):\n        return payload\n"),
        ("other.py", "class Other:\n    def send(self, payload):\n        return payload\n"),
        ("svc.py", "from client import Client\n\n\ndef run(payload):\n    c = Client()\n    return c.send(payload)\n"),
    ]);
    assert_eq!(function(&p, "run"), vec!["method:send@client.py"]);
}

#[test]
fn module_qualified_call_still_resolves() {
    let p = Project::new(&[
        ("ledger.py", "def record(row):\n    return row\n"),
        (
            "use.py",
            "from . import ledger\n\n\ndef save(row):\n    return ledger.record(row)\n",
        ),
    ]);
    assert!(function(&p, "save").contains(&"function:record@ledger.py".to_string()));
}

#[test]
fn dotted_module_path() {
    let p = Project::new(&[
        ("app/__init__.py", ""),
        ("app/utils/__init__.py", ""),
        ("app/utils/helpers.py", "def do_x():\n    return 1\n"),
        ("app/other.py", "def do_x():\n    return 2\n"),
        ("app/main.py", "import app.utils.helpers\nfrom app import utils\n\n\ndef full():\n    return app.utils.helpers.do_x()\n\n\ndef via_package():\n    return utils.helpers.do_x()\n"),
    ]);
    assert_eq!(
        function(&p, "full"),
        vec!["function:do_x@app/utils/helpers.py"]
    );
    assert_eq!(
        function(&p, "via_package"),
        vec!["function:do_x@app/utils/helpers.py"]
    );
}

#[test]
fn class_attribute_through_class_or_cls() {
    let p = Project::new(&[
        ("sink.py", "class Sink:\n    def lookup(self, k):\n        return k\n"),
        ("main.py", "class Registry:\n    def lookup(self, k):\n        return k\n\n\nclass Svc:\n    registry = Registry()\n\n    def run(self):\n        return Svc.registry.lookup(\"k\")\n\n    @classmethod\n    def c(cls):\n        return cls.registry.lookup(\"k\")\n"),
    ]);
    assert_eq!(method(&p, "run"), vec!["method:lookup@main.py"]);
    assert_eq!(method(&p, "c"), vec!["method:lookup@main.py"]);
}

#[test]
fn self_attribute_typed_by_class_body() {
    let p = Project::new(&[
        ("pkg/__init__.py", ""),
        ("pkg/a.py", "class Capture:\n    def stop(self):\n        pass\n"),
        ("pkg/b.py", "class Sink:\n    def stop(self):\n        pass\n"),
        ("pkg/models.py", "class User:\n    def save(self):\n        pass\n\n\nclass Other:\n    def save(self):\n        pass\n"),
        ("pkg/main.py", "from pkg.a import Capture\nfrom pkg.b import Sink\nfrom pkg import models\n\n\nclass Svc:\n    def __init__(self, cap: Capture):\n        self.cap = cap\n        self.sink = Sink()\n        self.user = models.User()\n\n    def by_param(self):\n        self.cap.stop()\n\n    def by_ctor(self):\n        self.sink.stop()\n\n    def by_module_type(self):\n        self.user.save()\n"),
    ]);
    assert_eq!(method(&p, "by_param"), vec!["method:stop@pkg/a.py"]);
    assert_eq!(method(&p, "by_ctor"), vec!["method:stop@pkg/b.py"]);
    assert_eq!(
        method(&p, "by_module_type"),
        vec!["method:save@pkg/models.py"]
    );
}

#[test]
fn attribute_type_reaches_base_method() {
    let p = Project::new(&[
        ("app/__init__.py", ""),
        ("app/capture.py", "class Base:\n    def stop(self):\n        pass\n\n\nclass Capture(Base):\n    def start(self):\n        pass\n"),
        ("app/sink.py", "class Sink:\n    def stop(self):\n        pass\n\n    def start(self):\n        pass\n"),
        ("app/main.py", "from app.capture import Capture\n\n\nclass Svc:\n    def __init__(self):\n        self.cap = Capture()\n\n    def run(self):\n        self.cap.start()\n        self.cap.stop()\n"),
    ]);
    assert_eq!(
        method(&p, "run"),
        vec!["method:start@app/capture.py", "method:stop@app/capture.py"]
    );
}

#[test]
fn attribute_of_typed_local_or_parameter() {
    let p = Project::new(&[
        ("models.py", "class Llm:\n    def rebuild(self):\n        pass\n\n\nclass Other:\n    def rebuild(self):\n        pass\n\n\nclass Config:\n    llm: Llm\n\n\nclass App:\n    def __init__(self):\n        self.config = Config()\n"),
        ("use.py", "from models import App, Config\n\n\ndef by_param(config: Config):\n    config.llm.rebuild()\n\n\ndef by_local():\n    app = App()\n    app.config.llm.rebuild()\n    cfg = app.config\n"),
    ]);
    assert_eq!(function(&p, "by_param"), vec!["method:rebuild@models.py"]);
    assert_eq!(function(&p, "by_local"), vec!["method:rebuild@models.py"]);
}

#[test]
fn external_attribute_type_unresolved() {
    let p = Project::new(&[
        ("session.py", "class Session:\n    def get(self, url):\n        return url\n"),
        ("svc.py", "import requests\n\n\nclass Svc:\n    def __init__(self):\n        self.http = requests.Session()\n\n    def fetch(self, url):\n        return self.http.get(url)\n"),
    ]);
    assert_eq!(method(&p, "fetch"), NONE);
}

#[test]
fn attribute_typed_only_by_whole_call() {
    let lib = "class Client:\n    def send(self):\n        pass\n\n    def close(self):\n        pass\n\n\nclass Fake:\n    def send(self):\n        pass\n\n\nclass Pool:\n    def acquire(self):\n        return Client()\n\n    def close(self):\n        pass\n";
    let p = Project::new(&[
        ("lib.py", lib),
        ("svc.py", "from lib import Client, Fake, Pool\n\n\nclass Svc:\n    def __init__(self, t):\n        self.pooled = Pool().acquire()\n        self.either = Client() if t else Fake()\n        self.wrapped = Client() \\\n            if t else Fake()\n        self.closed = Client().close()\n        self.same = Client(1) if t else Client(2)\n        self.multi = Client(\n            t,\n        )\n\n    def pooled_(self):\n        self.pooled.close()\n\n    def either_(self):\n        self.either.send()\n\n    def wrapped_(self):\n        self.wrapped.send()\n\n    def closed_(self):\n        self.closed.send()\n\n    def multi_(self):\n        self.multi.send()\n\n    def same_(self):\n        self.same.send()\n"),
    ]);
    for m in ["pooled_", "either_", "wrapped_", "closed_"] {
        assert_eq!(method(&p, m), NONE, "{m}");
    }
    assert_eq!(method(&p, "multi_"), vec!["method:send@lib.py"]);
    assert_eq!(method(&p, "same_"), vec!["method:send@lib.py"]);
}

#[test]
fn unclosed_values_never_skip_statements() {
    let lib = "class Client:\n    def send(self):\n        pass\n\n\nclass Fake:\n    def send(self):\n        pass\n";
    let run = "    def run(self):\n        self.c.send()\n";
    let a = format!("from lib import Client, Fake\n\n\nclass A:\n    def __init__(self):\n        self.c = Client()\n\n    def check(self):\n        raise ValueError(\"self.c = must be set (\")\n\n    def reset(self):\n        self.c = Fake()\n\n{run}");
    let b = format!("from lib import Client, Fake\n\n\nclass B:\n    def __init__(self):\n        self.c = Client()\n\n    opts = dict(\n    c=1)\n\n    def reset(self):\n        self.c = Fake()\n\n{run}");
    let c = format!("from lib import Client, Fake\n\n\nclass C:\n    def __init__(self):\n        self.c = Client()\n\n    def check(self):\n        self.c = f\"{{'\"'}}\"\n        self.c = Fake()\n\n{run}");
    let p = Project::new(&[("lib.py", lib), ("a.py", &a), ("b.py", &b), ("c.py", &c)]);
    let runs: Vec<_> = p
        .named("run")
        .into_iter()
        .filter(|n| n.kind == NodeKind::Method)
        .collect();
    assert_eq!(runs.len(), 3);
    for r in runs {
        assert!(p.callees(&r).is_empty(), "{}", r.file_path);
    }
}

fn write_svc(files: &[(&str, &str)]) -> Project {
    Project::new(files)
}

#[test]
fn review_round_0930() {
    let p = write_svc(&[
        ("pool.py", "class Pool:\n    def acquire(self):\n        return object()\n\n    def send(self, x):\n        return x\n"),
        ("svc.py", "from pool import Pool\n\nclass Svc:\n    def run(self):\n        self.conn = Pool().acquire()\n        return self.conn.send(1)\n"),
    ]);
    assert!(!method(&p, "run").contains(&"method:send@pool.py".to_string()));
    let p = write_svc(&[
        ("backend.py", "class Backend:\n    def run(self):\n        return 1\n"),
        ("svc.py", "from backend import Backend\n\ndef load():\n    return object\n\nclass Svc:\n    def go(self):\n        Backend = load()\n        self.b = Backend()\n        return self.b.run()\n"),
    ]);
    assert!(!method(&p, "go").contains(&"method:run@backend.py".to_string()));
    let p = write_svc(&[
        ("app/local.py", "class Session:\n    def get(self, url):\n        return url\n"),
        ("app/api.py", "from requests import Session\n\nclass Api:\n    def fetch(self):\n        self.session = Session()\n        return self.session.get(\"x\")\n"),
    ]);
    assert!(!method(&p, "fetch").contains(&"method:get@app/local.py".to_string()));
    let p = write_svc(&[
        ("box.py", "class Box:\n    def run(self):\n        return 1\n"),
        ("svc.py", "from box import Box\n\ndef wrap(b):\n    return b\n\nclass Svc:\n    def __init__(self, box: Box):\n        box = wrap(box)\n        self.held = box\n\n    def go(self):\n        return self.held.run()\n"),
    ]);
    assert!(!method(&p, "go").contains(&"method:run@box.py".to_string()));
    let p = write_svc(&[
        ("app/__init__.py", ""),
        ("app/rel/__init__.py", ""),
        ("app/rel/utils/__init__.py", ""),
        ("app/rel/utils/helpers.py", "def do_x():\n    return 1\n"),
        (
            "app/rel/use.py",
            "from . import utils\n\ndef rel_caller():\n    return utils.helpers.do_x()\n",
        ),
    ]);
    assert!(
        function(&p, "rel_caller").contains(&"function:do_x@app/rel/utils/helpers.py".to_string())
    );
}

const FOO: &str = "class Foo:\n    def m(self):\n        return 1\n";
const OTHER: &str = "class Other:\n    def m(self):\n        return 2\n";

fn m_calls(svc: &str, f: &str, kind: NodeKind) -> Vec<String> {
    let p = Project::new(&[("foo.py", FOO), ("other.py", OTHER), ("svc.py", svc)]);
    let mut v: Vec<String> = p
        .calls_by_kind(kind, f)
        .into_iter()
        .filter(|c| c.starts_with("method:m@"))
        .collect();
    v.sort();
    v
}

#[test]
fn review_round_1001() {
    let go = |svc: &str| m_calls(svc, "go", NodeKind::Method);
    let foo = vec!["method:m@foo.py".to_string()];
    assert_eq!(go("from foo import Foo\n\nclass Base:\n    def __init__(self, client=None):\n        pass\n\nclass Svc(Base):\n    def __init__(self, client: Foo):\n        super().__init__(\n            client=client,\n        )\n        validate(client=client)\n        self.client = client\n\n    def go(self):\n        return self.client.m()\n\ndef validate(client=None):\n    return client\n"), foo, "keyword argument");
    assert_eq!(go("from foo import Foo\n\ndef reg(**kw):\n    return kw\n\nclass Svc:\n    def __init__(self):\n        reg(Foo=1)\n        reg(\n            Foo=2,\n        )\n        self.b = Foo()\n\n    def go(self):\n        return self.b.m()\n"), foo, "keyword named like the class");
    assert_eq!(go("class Svc:\n    def __init__(self):\n        from foo import Foo\n        self.b = Foo()\n\n    def go(self):\n        return self.b.m()\n"), foo, "local import");
    assert_eq!(go("from foo import Foo\n\nclass Svc:\n    def __init__(self):\n        def cb(Foo=None):\n            Foo = 1\n            return Foo\n        self.b = Foo()\n\n    def go(self):\n        return self.b.m()\n"), foo, "nested def");
    assert_eq!(go("from foo import Foo\n\nclass Svc:\n    def __init__(self):\n        print(\"Foo = %s\" % 1)\n        self.b = Foo()\n\n    def go(self):\n        return self.b.m()\n"), foo, "string");
    assert_eq!(go("from foo import Foo\n\ndef load():\n    return object\n\nclass Svc:\n    def __init__(self, k):\n        if (Foo := load()):\n            pass\n        self.b = Foo()\n\n    def go(self):\n        return self.b.m()\n"), NONE, "walrus");
    assert_eq!(go("from foo import Foo\n\ndef load():\n    return object\n\nclass Svc:\n    def __init__(self, k):\n        if k: Foo = load()\n        self.b = Foo()\n\n    def go(self):\n        return self.b.m()\n"), NONE, "one-line compound");
    assert_eq!(go("from foo import Foo\n\nclass Svc:\n    def go(self):\n        self.a.b = Foo()\n        return self.a.b.m()\n"), NONE, "deeper chain");
    assert_eq!(
        m_calls(
            "from foo import Foo\n\ndef go(c):\n    c.client = Foo()\n    return c.client.m()\n",
            "go",
            NodeKind::Function
        ),
        NONE,
        "untyped parameter"
    );
    assert_eq!(go("from foo import Foo\nfrom other import Other\n\nclass Svc:\n    def reset(self):\n        self.x = Other()\n\n    def go(self):\n        self.x = Foo()\n        return self.x.m()\n"), NONE, "disagree");
}

#[test]
fn review_round_1002() {
    let go = |svc: &str| {
        m_calls(
            &format!("from foo import Foo\nfrom other import Other\n\n{svc}"),
            "go",
            NodeKind::Method,
        )
    };
    assert_eq!(go("class Svc:\n    def __init__(self, d, c):\n        d[Foo] = 1\n        self._h[Foo] = c\n        Foo.attr = 1\n        self.b = Foo()\n\n    def go(self):\n        return self.b.m()\n"), vec!["method:m@foo.py"]);
    assert_eq!(go("class Svc:\n    def go(self, a):\n        if a:\n            self.x = Foo()\n        else:\n            self.x = Other()\n        return self.x.m()\n"), NONE);
    assert_eq!(go("class Svc:\n    def __init__(self):\n        self.x = Other()\n\n    def go(self, a):\n        if a:\n            self.x = Foo()\n        return self.x.m()\n"), NONE);
    assert_eq!(go("class Svc:\n    def __init__(self, c: Foo):\n        def on_ready(c):\n            self.x = c\n        self.cb = on_ready\n\n    def go(self):\n        return self.x.m()\n"), NONE);
    assert_eq!(go("def load():\n    return object\n\nclass Svc:\n    def __init__(self):\n        def setup():\n            Foo = load()\n            self.x = Foo()\n        setup()\n\n    def go(self):\n        return self.x.m()\n"), NONE);
    assert_eq!(
        go("class Svc:\n    def go(self):\n        self.x += Foo()\n        return self.x.m()\n"),
        NONE
    );
}
