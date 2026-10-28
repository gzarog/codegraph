//! Upstream `__tests__/python-attr-types.test.ts`: attribute types a class
//! body says only indirectly, through a factory or a base class.

use crate::harness::Project;
use codegraph_core::{EdgeKind, NodeKind};

const DISTRACTOR: &str = "class Other:\n    def send(self):\n        pass\n\n    def save(self):\n        pass\n\n    def go(self):\n        pass\n";

/// `qualified@file` of what the first method named `name` calls.
fn calls(p: &Project, name: &str) -> Vec<String> {
    let src = p
        .named(name)
        .into_iter()
        .find(|n| n.kind == NodeKind::Method)
        .unwrap_or_else(|| panic!("no method {name}"));
    p.targets(&src, EdgeKind::Calls)
        .into_iter()
        .map(|n| format!("{}@{}", n.qualified_name, n.file_path))
        .collect()
}

fn run(files: &[(&str, &str)]) -> Vec<String> {
    calls(&Project::new(files), "run")
}

const NONE: &[&str] = &[];

#[test]
fn factories() {
    let svc = "from client import Client\n\nclass Svc:\n    def __init__(self):\n        self.client = Client.from_env()\n\n    def run(self):\n        self.client.send()\n";
    assert_eq!(run(&[
        ("client.py", "class Client:\n    @classmethod\n    def from_env(cls) -> \"Client\":\n        return cls()\n\n    def send(self):\n        pass\n"),
        ("other.py", DISTRACTOR), ("svc.py", svc),
    ]), vec!["Client::send@client.py"], "classmethod annotation");
    assert_eq!(run(&[
        ("client.py", "class Client:\n    @classmethod\n    def from_env(cls):\n        return cls()\n\n    def send(self):\n        pass\n"),
        ("other.py", DISTRACTOR), ("svc.py", svc),
    ]), vec!["Client::send@client.py"], "classmethod returning cls()");
    assert_eq!(run(&[
        ("base.py", "class Base:\n    @classmethod\n    def create(cls):\n        return cls()\n\n    def send(self):\n        pass\n"),
        ("client.py", "from base import Base\n\nclass Client(Base):\n    def send(self):\n        pass\n"),
        ("svc.py", "from client import Client\n\nclass Svc:\n    def __init__(self):\n        self.client = Client.create()\n\n    def run(self):\n        self.client.send()\n"),
    ]), vec!["Client::send@client.py"], "cls() is the class called on");
    assert_eq!(run(&[
        ("client.py", "class Client:\n    def send(self):\n        pass\n\ndef make_client() -> Client:\n    return Client()\n"),
        ("other.py", DISTRACTOR),
        ("svc.py", "from client import make_client\n\nclass Svc:\n    def __init__(self):\n        self.client = make_client()\n\n    def run(self):\n        self.client.send()\n"),
    ]), vec!["Client::send@client.py"], "factory annotation");
    assert_eq!(run(&[
        ("client.py", "class Client:\n    def send(self):\n        pass\n\ndef make_client(url=None):\n    if url is None:\n        return None\n    return Client()\n"),
        ("other.py", DISTRACTOR),
        ("svc.py", "from client import make_client\n\nclass Svc:\n    def __init__(self):\n        self.client = make_client(\"x\")\n\n    def run(self):\n        self.client.send()\n"),
    ]), vec!["Client::send@client.py"], "unannotated factory");
    assert_eq!(run(&[
        ("app/__init__.py", ""),
        ("app/clients.py", "class Client:\n    def send(self):\n        pass\n\ndef build() -> \"Client\":\n    return Client()\n"),
        ("other.py", DISTRACTOR),
        ("svc.py", "from app import clients\n\nclass Svc:\n    def __init__(self):\n        self.client = clients.build()\n\n    def run(self):\n        self.client.send()\n"),
    ]), vec!["Client::send@app/clients.py"], "factory through its module");
    assert_eq!(run(&[
        ("client.py", "REGISTRY = {}\n\nclass Client:\n    def send(self):\n        pass\n\ndef make_client(kind):\n    return REGISTRY[kind]()\n"),
        ("other.py", DISTRACTOR),
        ("svc.py", "from client import make_client\n\nclass Svc:\n    def __init__(self):\n        self.conn = make_client(\"x\")\n\n    def run(self):\n        self.conn.send()\n"),
    ]), NONE, "unnameable return");
}

#[test]
fn factory_returns_must_be_whole_calls() {
    let lib = "class Client:\n    def send(self):\n        pass\n\n    def close(self):\n        pass\n\nclass Fake:\n    def send(self):\n        pass\n\nclass Pool:\n    def acquire(self):\n        return Client()\n\n    def close(self):\n        pass\n";
    let f = "from lib import Client, Fake, Pool\n\ndef either(t):\n    return Client() if t else Fake()\n\ndef wrapped(t):\n    return Client() \\\n        if t else Fake()\n\ndef chained():\n    return Pool().acquire()\n\ndef closed():\n    return Client().close()\n\ndef same(t):\n    return Client(1) if t else Client(\n        2,\n    )\n\ndef make_pool():\n    return Pool()\n";
    let svc = "from f import either, wrapped, chained, closed, same, make_pool\n\nclass Svc:\n    def __init__(self, t):\n        self.a = either(t)\n        self.b = wrapped(t)\n        self.c = chained()\n        self.d = closed()\n        self.e = same(t)\n        self.g = make_pool().acquire()\n\n    def a_(self):\n        self.a.send()\n\n    def b_(self):\n        self.b.send()\n\n    def c_(self):\n        self.c.close()\n\n    def d_(self):\n        self.d.send()\n\n    def e_(self):\n        self.e.send()\n\n    def g_(self):\n        self.g.close()\n";
    let p = Project::new(&[("lib.py", lib), ("f.py", f), ("svc.py", svc)]);
    for m in ["a_", "b_", "c_", "d_", "g_"] {
        assert_eq!(calls(&p, m), NONE, "{m}");
    }
    assert_eq!(calls(&p, "e_"), vec!["Client::send@lib.py"]);
}

#[test]
fn factory_negatives() {
    assert_eq!(run(&[
        ("client.py", "class Client:\n    def send(self):\n        pass\n\nclass Fake:\n    def send(self):\n        pass\n\ndef make_client(test):\n    if test:\n        return Fake()\n    return Client()\n"),
        ("svc.py", "from client import make_client\n\nclass Svc:\n    def __init__(self):\n        self.conn = make_client(False)\n\n    def run(self):\n        self.conn.send()\n"),
    ]), NONE, "two classes");
    assert_eq!(run(&[
        ("client.py", "class Client:\n    def send(self):\n        pass\n\ndef clients():\n    yield Client()\n\nasync def connect():\n    return Client()\n"),
        ("other.py", DISTRACTOR),
        ("svc.py", "from client import clients, connect\n\nclass Svc:\n    def __init__(self):\n        self.a = clients()\n        self.b = connect()\n\n    def run(self):\n        self.a.send()\n        self.b.send()\n"),
    ]), NONE, "generator / un-awaited coroutine");
    assert_eq!(run(&[
        ("client.py", "class Client:\n    def send(self):\n        pass\n\nasync def connect() -> Client:\n    return Client()\n"),
        ("other.py", DISTRACTOR),
        ("svc.py", "from client import connect\n\nclass Svc:\n    async def start(self):\n        self.client = await connect()\n\n    def run(self):\n        self.client.send()\n"),
    ]), vec!["Client::send@client.py"], "awaited coroutine");
    assert_eq!(run(&[
        ("signing.py", "from loader import import_string\n\nclass Signer:\n    def unsign(self, v):\n        return v\n\ndef get_signer(backend):\n    Signer = import_string(backend)\n    return Signer()\n"),
        ("loader.py", "def import_string(p):\n    return None\n"),
        ("other.py", "class Other:\n    def unsign(self, v):\n        return v\n"),
        ("svc.py", "from signing import Signer, get_signer\nfrom loader import import_string\n\nclass Svc:\n    def __init__(self, path):\n        self.a = get_signer(path)\n        Backend = import_string(path)\n        self.b = Backend()\n\n    def run(self):\n        self.a.unsign(1)\n        self.b.unsign(2)\n"),
    ]), NONE, "rebound class name");
    assert_eq!(run(&[
        ("shapes/__init__.py", ""),
        ("shapes/box.py", "class Box:\n    def substitute(self):\n        pass\n"),
        ("other.py", "class Other:\n    def substitute(self):\n        pass\n"),
        ("shapes/table.py", "from typing import Optional\nfrom . import box\n\nclass Table:\n    def __init__(self, box: Optional[box.Box] = None):\n        self.box = box\n\n    def run(self):\n        self.box.substitute()\n"),
    ]), vec!["Box::substitute@shapes/box.py"], "parameter shadowing a module");
    assert_eq!(run(&[
        ("client.py", "class Session:\n    def send(self):\n        pass\n\ndef session() -> Session:\n    return Session()\n"),
        ("svc.py", "from requests import session\n\nclass Svc:\n    def __init__(self):\n        self.http = session()\n\n    def run(self):\n        self.http.send()\n"),
    ]), NONE, "outside factory");
    assert_eq!(run(&[
        ("p/__init__.py", ""),
        ("p/a.py", "class Client:\n    def send(self):\n        pass\n\ndef make() -> Client:\n    return Client()\n"),
        ("q/__init__.py", ""),
        ("q/a.py", "class Fake:\n    def send(self):\n        pass\n\ndef make() -> Fake:\n    return Fake()\n"),
        ("svc.py", "from q.a import make\n\nclass Svc:\n    def __init__(self):\n        self.client = make()\n\n    def run(self):\n        self.client.send()\n"),
    ]), vec!["Fake::send@q/a.py"], "same-named factory elsewhere");
}

#[test]
fn chained_factory_calls() {
    let p = Project::new(&[
        ("client.py", "class Client:\n    @classmethod\n    def from_env(cls):\n        return cls()\n\n    def send(self):\n        pass\n\ndef make_client() -> Client:\n    return Client()\n"),
        ("other.py", DISTRACTOR),
        ("svc.py", "from client import Client, make_client\n\nclass Svc:\n    def run(self):\n        Client.from_env().send()\n        make_client().send()\n        Client().send()\n"),
    ]);
    let sends: Vec<String> = calls(&p, "run").into_iter().filter(|c| c.contains("send")).collect();
    assert_eq!(sends, vec!["Client::send@client.py"; 3]);
    let p = Project::new(&[
        ("client.py", "class Client:\n    def send(self):\n        pass\n\ndef make_client(kind):\n    return kind()\n"),
        ("other.py", DISTRACTOR),
        ("svc.py", "from client import make_client\n\nclass Svc:\n    def run(self):\n        make_client(1).send()\n"),
    ]);
    assert!(calls(&p, "run").iter().all(|c| !c.contains("send")));
}

#[test]
fn base_classes() {
    assert_eq!(run(&[
        ("repo.py", "class Repo:\n    def save(self):\n        pass\n"),
        ("base.py", "from repo import Repo\n\nclass Base:\n    def __init__(self):\n        self.repo = Repo()\n"),
        ("other.py", DISTRACTOR),
        ("child.py", "from base import Base\n\nclass Child(Base):\n    def run(self):\n        self.repo.save()\n"),
    ]), vec!["Repo::save@repo.py"], "base __init__");
    assert_eq!(run(&[
        ("repo.py", "class Repo:\n    def save(self):\n        pass\n"),
        ("base.py", "from repo import Repo\n\nclass Base:\n    def __init__(self, repo: Repo):\n        self.repo = repo\n"),
        ("mid.py", "from base import Base\n\nclass Mid(Base):\n    pass\n"),
        ("other.py", DISTRACTOR),
        ("child.py", "from mid import Mid\n\nclass Child(Mid):\n    def run(self):\n        self.repo.save()\n"),
    ]), vec!["Repo::save@repo.py"], "chain of bases");
    assert_eq!(run(&[
        ("repo.py", "class Repo:\n    def save(self):\n        pass\n\nclass Cache:\n    def save(self):\n        pass\n"),
        ("base.py", "from repo import Repo\n\nclass Base:\n    def __init__(self):\n        self.repo = Repo()\n"),
        ("child.py", "from base import Base\nfrom repo import Cache\n\nclass Child(Base):\n    def __init__(self):\n        super().__init__()\n        self.repo = Cache()\n\n    def run(self):\n        self.repo.save()\n"),
    ]), vec!["Cache::save@repo.py"], "subclass shadows");
    assert_eq!(run(&[
        ("repo.py", "class Repo:\n    def save(self):\n        pass\n"),
        ("base.py", "from repo import Repo\n\nclass Base:\n    def __init__(self):\n        self.store = Repo()\n"),
        ("other.py", DISTRACTOR),
        ("child.py", "from base import Base\n\nclass Child(Base):\n    def __init__(self, repo):\n        super().__init__()\n        self.store = repo\n\n    def run(self):\n        self.store.save()\n"),
    ]), NONE, "untyped subclass assignment");
    assert_eq!(run(&[
        ("repo.py", "class Repo:\n    def save(self):\n        pass\n\nclass Cache:\n    def save(self):\n        pass\n"),
        ("bases.py", "from repo import Repo, Cache\n\nclass A:\n    def __init__(self):\n        self.store = Repo()\n\nclass B:\n    def __init__(self):\n        self.store = Cache()\n"),
        ("child.py", "from bases import A, B\n\nclass Child(A, B):\n    def run(self):\n        self.store.save()\n"),
    ]), NONE, "bases disagree");
    assert_eq!(run(&[
        ("repo.py", "class Repo:\n    def save(self):\n        pass\n"),
        ("other.py", DISTRACTOR),
        ("child.py", "from django.views import View\n\nclass Child(View):\n    def run(self):\n        self.repo.save()\n"),
    ]), NONE, "outside base");
    assert_eq!(run(&[
        ("fields/__init__.py", "from .impl import *\n"),
        ("fields/impl.py", "class MultiValueField:\n    def clean(self, v):\n        return v\n"),
        ("forms.py", "from fields import MultiValueField\n\nclass ComplexField(MultiValueField):\n    pass\n"),
        ("other.py", "class Other:\n    def clean(self, v):\n        return v\n"),
        ("tests.py", "from forms import ComplexField\n\nclass T:\n    def setUp(self):\n        self.field = ComplexField()\n\n    def run(self):\n        self.field.clean(1)\n"),
    ]), vec!["MultiValueField::clean@fields/impl.py"], "base through a star import");
    assert_eq!(run(&[
        ("repo.py", "class Repo:\n    def save(self):\n        pass\n"),
        ("other.py", DISTRACTOR),
        ("child.py", "from abc import ABC\n\nclass Child(ABC):\n    def run(self):\n        self.repo.save()\n"),
    ]), vec!["Repo::save@repo.py"], "ABC gives nothing");
    assert_eq!(run(&[
        ("models.py", "from django.db import models\n\nclass Author(models.Model):\n    def save(self):\n        pass\n\nclass Book(models.Model):\n    author = models.ForeignKey(Author, on_delete=models.CASCADE)\n\n    def run(self):\n        self.author.save()\n"),
        ("other.py", DISTRACTOR),
    ]), vec!["Author::save@models.py"], "descriptor field");
    assert_eq!(run(&[
        ("session.py", "class Session:\n    def get(self):\n        pass\n"),
        ("api.py", "import requests\n\nclass Api:\n    def __init__(self):\n        self.session = requests.Session()\n\n    def run(self):\n        self.session.get()\n"),
    ]), NONE, "outside class built in a method");
}

#[test]
fn pinned_forms() {
    assert_eq!(run(&[
        ("app/__init__.py", ""),
        ("app/models.py", "class User:\n    def save(self):\n        pass\n"),
        ("other.py", DISTRACTOR),
        ("svc.py", "import app.models\nfrom app import models\n\nclass Svc:\n    def __init__(self, u):\n        self.user = models.User()\n        self.other: app.models.User = u\n\n    def run(self):\n        self.user.save()\n        self.other.save()\n"),
    ]), vec!["User::save@app/models.py"; 2]);
    assert_eq!(run(&[
        ("a.py", "class A:\n    def go(self):\n        pass\n\nclass B:\n    def go(self):\n        pass\n"),
        ("other.py", DISTRACTOR),
        ("svc.py", "from a import A, B\n\nclass Svc:\n    def __init__(self, f):\n        if f:\n            self.x = A(1)\n        else:\n            self.x = A(2)\n        try:\n            self.y = A()\n        except Exception:\n            self.y = B()\n\n    def run(self):\n        self.x.go()\n        self.y.go()\n"),
    ]), vec!["A::go@a.py"]);
}

/// Edges out of every method named `run`, sorted.
fn run_edges(p: &Project) -> Vec<String> {
    let mut out: Vec<String> = p
        .named("run")
        .into_iter()
        .filter(|n| n.kind == NodeKind::Method)
        .flat_map(|n| p.targets(&n, EdgeKind::Calls))
        .map(|t| format!("{}@{}", t.qualified_name, t.file_path))
        .collect();
    out.sort();
    out
}

/// Sync, then check the synced graph against a fresh index.
fn converge(p: &mut Project) -> Vec<String> {
    p.sync();
    let synced = run_edges(p);
    p.reindex();
    assert_eq!(synced, run_edges(p), "sync vs fresh");
    synced
}

#[test]
fn sync_converges() {
    let mut p = Project::new(&[
        ("client.py", "class Client:\n    def send(self):\n        pass\n\nclass Fake:\n    def send(self):\n        pass\n"),
        ("factory.py", "from client import Client, Fake\n\ndef make():\n    return Client()\n"),
        ("svc.py", "from factory import make\n\nclass Svc:\n    def __init__(self):\n        self.client = make()\n\n    def run(self):\n        self.client.send()\n"),
    ]);
    assert_eq!(run_edges(&p), vec!["Client::send@client.py"]);
    p.write(&[("factory.py", "from client import Client, Fake\n\ndef make():\n    return Fake()\n")]);
    assert_eq!(converge(&mut p), vec!["Fake::send@client.py"], "factory changes");

    let mut p = Project::new(&[
        ("client.py", "class Client:\n    def send(self):\n        pass\n"),
        ("other.py", DISTRACTOR),
        ("factory.py", "from client import Client\n\ndef make(kind):\n    return kind()\n"),
        ("svc.py", "from factory import make\n\nclass Svc:\n    def __init__(self):\n        self.conn = make(1)\n\n    def run(self):\n        self.conn.send()\n"),
    ]);
    assert_eq!(run_edges(&p), NONE);
    p.write(&[("factory.py", "from client import Client\n\ndef make(kind) -> Client:\n    return kind()\n")]);
    assert_eq!(converge(&mut p), vec!["Client::send@client.py"], "gains annotation");

    let mut p = Project::new(&[
        ("client.py", "class Client:\n    def send(self):\n        pass\n\nclass Fake:\n    def send(self):\n        pass\n"),
        ("factory.py", "from client import Client, Fake\n\ndef make(kind):\n    return kind()\n"),
        ("svc.py", "from factory import make\n\nclass Svc:\n    def __init__(self):\n        self.client = make(1)\n\n    def run(self):\n        self.client.send()\n"),
    ]);
    assert_eq!(run_edges(&p), vec!["Client::send@client.py"]);
    p.write(&[("factory.py", "from client import Client, Fake\n\ndef make(kind) -> Fake:\n    return kind()\n")]);
    assert_eq!(converge(&mut p), vec!["Fake::send@client.py"], "guess re-typed");

    let mut p = Project::new(&[
        ("repo.py", "class Repo:\n    def save(self):\n        pass\n\nclass Cache:\n    def save(self):\n        pass\n"),
        ("base.py", "from repo import Repo, Cache\n\nclass Base:\n    def __init__(self):\n        self.repo = Repo()\n"),
        ("child.py", "from base import Base\n\nclass Child(Base):\n    def run(self):\n        self.repo.save()\n"),
    ]);
    assert_eq!(run_edges(&p), vec!["Repo::save@repo.py"]);
    p.write(&[("base.py", "from repo import Repo, Cache\n\nclass Base:\n    def __init__(self):\n        self.repo = Cache()\n")]);
    assert_eq!(converge(&mut p), vec!["Cache::save@repo.py"], "base __init__ changes");

    let mut p = Project::new(&[
        ("repo.py", "class Repo:\n    def save(self):\n        pass\n"),
        ("other.py", DISTRACTOR),
        ("base.py", "from repo import Repo\n\nclass Base:\n    def __init__(self):\n        self.repo = Repo()\n"),
        ("child.py", "from base import Base\n\nclass Child(Base):\n    def run(self):\n        self.repo.save()\n"),
    ]);
    assert_eq!(run_edges(&p), vec!["Repo::save@repo.py"]);
    std::fs::remove_file(p.root().join("base.py")).unwrap();
    assert_eq!(converge(&mut p), NONE, "base deleted");
}

#[test]
fn sync_converges_more() {
    let mut p = Project::new(&[
        ("repo.py", "class Repo:\n    def save(self):\n        pass\n"),
        ("base.py", "from repo import Repo\n\nclass Base:\n    def __init__(self):\n        self.repo = Repo()\n"),
        ("child.py", "from base import Base\n\nclass Child(Base):\n    def run(self):\n        self.repo.save()\n"),
        ("unrelated.py", "def f():\n    return 1\n"),
    ]);
    p.write(&[("unrelated.py", "def f():\n    return 2\n")]);
    p.sync();
    assert_eq!(run_edges(&p), vec!["Repo::save@repo.py"], "unrelated edit");

    let mut p = Project::new(&[
        ("client.py", "class Client:\n    def send(self):\n        pass\n\nclass Fake:\n    def send(self):\n        pass\n"),
        ("f.py", "from client import Client, Fake\n\ndef make():\n    return Client()\n\ndef make2(k):\n    return k()\n"),
        ("svc.py", "from f import make, make2\n\nclass A:\n    def __init__(self):\n        self.a = make()\n\n    def run(self):\n        self.a.send()\n\nclass B:\n    def __init__(self):\n        self.b = make2(1)\n\n    def run(self):\n        self.b.send()\n"),
    ]);
    assert_eq!(run_edges(&p), vec!["Client::send@client.py"]);
    p.write(&[("f.py", "from client import Client, Fake\n\ndef make():\n    return Fake()\n\ndef make2(k) -> Client:\n    return k()\n")]);
    assert_eq!(converge(&mut p), vec!["Client::send@client.py", "Fake::send@client.py"], "retry");

    let mut p = Project::new(&[
        ("client.py", "class Client:\n    def send(self):\n        pass\n"),
        ("other.py", DISTRACTOR),
        ("f.py", "from client import Client\n\ndef make_v2():\n    return Client()\n"),
        ("svc.py", "from f import make\n\nclass Svc:\n    def __init__(self):\n        self.c = make()\n\n    def run(self):\n        self.c.send()\n"),
    ]);
    assert_eq!(run_edges(&p), NONE);
    p.write(&[("f.py", "from client import Client\n\ndef make():\n    return Client()\n")]);
    assert_eq!(converge(&mut p), vec!["Client::send@client.py"], "factory appears");

    let svc = "from client import make, helper\n\nclass Svc:\n    def __init__(self, x, y):\n        self.c = make(1)\n        self.d = x\n        self.e = y\n        helper()\n\n    def run(self):\n        self.c.send()\n        self.d.send()\n        self.e.send()\n";
    let mut p = Project::new(&[
        ("client.py", "class Client:\n    def send(self):\n        pass\n\ndef make(k):\n    return k()\n\ndef helper():\n    pass\n"),
        ("other.py", DISTRACTOR),
        ("svc.py", svc),
    ]);
    assert_eq!(run_edges(&p), NONE);
    p.write(&[("client.py", "class Client:\n    def send(self):\n        pass\n\ndef make(k) -> Client:\n    return k()\n\ndef helper():\n    pass\n")]);
    assert_eq!(converge(&mut p), vec!["Client::send@client.py"], "narrowed retry");

    let mut p = Project::new(&[
        ("repo.py", "class Repo:\n    def save(self):\n        pass\n"),
        ("other.py", DISTRACTOR),
        ("base.py", "from repo import Repo\n\nclass Base:\n    def __init__(self):\n        self.store = Repo()\n"),
        ("child.py", "from base import Base\n\nclass Child(Base):\n    def run(self):\n        self.store.save()\n"),
    ]);
    assert_eq!(run_edges(&p), vec!["Repo::save@repo.py"]);
    p.write(&[("base.py", "from repo import Repo\n\nclass Base:\n    def __init__(self, repo):\n        self.store = repo\n")]);
    assert_eq!(converge(&mut p), NONE, "base stops typing");
}

#[test]
fn review_rounds() {
    assert_eq!(run(&[
        ("a.py", "class A:\n    def send(self):\n        pass\n"),
        ("b.py", "class B:\n    def send(self):\n        pass\n"),
        ("make.py", "from a import A\nfrom b import B\n\ndef make(k):\n    if k: return A()\n    return B()\n"),
        ("svc.py", "from make import make\n\nclass Svc:\n    def __init__(self):\n        self.c = make(1)\n\n    def run(self):\n        self.c.send()\n"),
    ]), NONE, "one-line return");
    assert_eq!(run(&[
        ("a.py", "class A:\n    def send(self):\n        pass\n"),
        ("other.py", DISTRACTOR),
        ("make.py", "from a import A\n\ndef make(k):\n    if k: return A()\n    else: return A()\n"),
        ("svc.py", "from make import make\n\nclass Svc:\n    def __init__(self):\n        self.c = make(1)\n\n    def run(self):\n        self.c.send()\n"),
    ]), vec!["A::send@a.py"], "one-line returns agree");
    let p = Project::new(&[
        ("backend.py", "class Backend:\n    def run(self):\n        pass\n"),
        ("svc.py", "from backend import Backend\n\ndef load():\n    return object\n\nclass Svc:\n    def go(self):\n        Backend = load()\n        Backend().run()\n"),
    ]);
    assert!(calls(&p, "go").iter().all(|c| !c.starts_with("Backend::")), "rebound class");
    let p = Project::new(&[
        ("client.py", "class Client:\n    def send(self):\n        pass\n"),
        ("make.py", "from client import Client\n\ndef make():\n    return Client()\n"),
        ("svc.py", "from make import make\n\nclass Svc:\n    def go(self, make):\n        make().send()\n"),
    ]);
    assert!(!calls(&p, "go").contains(&"Client::send@client.py".to_string()), "parameter shadows factory");
    assert_eq!(run(&[
        ("a.py", "class A:\n    def send(self):\n        pass\n"),
        ("other.py", DISTRACTOR),
        ("fac.py", "from a import A\n\nK = 1\n\ndef make():\n    if not K:\n        raise ValueError(\"bad config: return value missing\")\n    print(\"will yield\")\n    return A()\n"),
        ("svc.py", "from fac import make\n\nclass Svc:\n    def __init__(self):\n        self.c = make()\n\n    def run(self):\n        self.c.send()\n"),
    ]), vec!["A::send@a.py"], "return in a string");
    let p = Project::new(&[
        ("a.py", "class A:\n    def send(self):\n        pass\n"),
        ("other.py", DISTRACTOR),
        ("fac.py", "from a import A\n\ndef make() -> A:\n    return A()\n"),
        ("svc.py", "from fac import make\n\nclass Svc:\n    def run(self):\n        self.c = make()\n        self.c.send()\n"),
    ]);
    let sends: Vec<String> = calls(&p, "run").into_iter().filter(|c| c.contains("::send@")).collect();
    assert_eq!(sends, vec!["A::send@a.py"], "factory in calling method");
}
