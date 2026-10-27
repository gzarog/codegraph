//! Synthesized dispatch edges: task queues, event buses, stores and
//! registries joined after resolution.

use codegraph_core::EdgeKind;
use codegraph_index::{index_project, IndexOptions, ProjectConfig};
use codegraph_store::Store;
use std::fs;
use std::path::Path;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn index(files: &[(&str, &str)]) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    for (rel, text) in files {
        write(dir.path(), rel, text);
    }
    let mut store = Store::open_in_memory().unwrap();
    index_project(
        &mut store,
        dir.path(),
        &ProjectConfig::default(),
        &IndexOptions::default(),
    )
    .unwrap();
    (dir, store)
}

/// The `dispatch` family of a synthesized `from → to` call, if there is one.
fn synth(store: &Store, from: &str, to: &str) -> Option<String> {
    let tos = store.nodes_named(to).unwrap();
    for f in store.nodes_named(from).unwrap() {
        for e in store.outgoing_edges(&f.id, &[EdgeKind::Calls]).unwrap() {
            let Some(m) = &e.metadata else { continue };
            if m["synthesizedBy"] == "synth" && tos.iter().any(|t| t.id == e.target) {
                return m["dispatch"].as_str().map(str::to_string);
            }
        }
    }
    None
}

#[test]
fn celery_tasks() {
    let (_d, store) = index(&[
        (
            "app/tasks.py",
            "from celery import shared_task\n\n@shared_task\ndef process(ids):\n    return ids\n\ndef helper():\n    pass\n",
        ),
        (
            "app/views.py",
            "from app.tasks import process, helper\n\ndef submit(request):\n    process.delay([1])\n    helper.delay()\n",
        ),
    ]);
    assert_eq!(
        synth(&store, "submit", "process").as_deref(),
        Some("celery")
    );
    assert_eq!(synth(&store, "submit", "helper"), None, "not a task");
}

#[test]
fn spring_events() {
    let (_d, store) = index(&[
        (
            "src/main/java/app/AccountService.java",
            "package app;\npublic class AccountService {\n  private ApplicationEventPublisher publisher;\n  public void changePassword(String user) {\n    publisher.publishEvent(new PasswordChangedEvent(this, user));\n  }\n}\n",
        ),
        (
            "src/main/java/app/TokenRevoker.java",
            "package app;\npublic class TokenRevoker {\n  @EventListener\n  public void onPasswordChanged(PasswordChangedEvent event) {\n  }\n  @EventListener(UserDeletedEvent.class)\n  public void onDeleted() {\n  }\n}\n",
        ),
    ]);
    assert_eq!(
        synth(&store, "changePassword", "onPasswordChanged").as_deref(),
        Some("spring-event")
    );
    assert_eq!(synth(&store, "changePassword", "onDeleted"), None);
}

#[test]
fn mediatr_handlers() {
    let (_d, store) = index(&[
        (
            "Orders/CancelOrderHandler.cs",
            "namespace Orders;\npublic class CancelOrderHandler : IRequestHandler<CancelOrderCommand, bool>\n{\n    public async Task<bool> Handle(CancelOrderCommand request, CancellationToken ct)\n    {\n        return true;\n    }\n}\n",
        ),
        (
            "Orders/OrdersController.cs",
            "namespace Orders;\npublic class OrdersController\n{\n    private readonly IMediator _mediator;\n    public async Task Cancel(int id)\n    {\n        var command = new CancelOrderCommand(id);\n        await _mediator.Send(command);\n    }\n}\n",
        ),
    ]);
    assert_eq!(
        synth(&store, "Cancel", "Handle").as_deref(),
        Some("mediatr")
    );
}

#[test]
fn sidekiq_workers() {
    let (_d, store) = index(&[
        (
            "app/workers/destroy_user_worker.rb",
            "class DestroyUserWorker\n  include Sidekiq::Worker\n\n  def perform(user_id)\n    puts user_id\n  end\nend\n",
        ),
        (
            "app/services/accounts.rb",
            "class Accounts\n  def remove(user)\n    DestroyUserWorker.perform_async(user.id)\n  end\nend\n",
        ),
    ]);
    assert_eq!(
        synth(&store, "remove", "perform").as_deref(),
        Some("sidekiq")
    );
}

#[test]
fn laravel_events() {
    let (_d, store) = index(&[
        (
            "app/Listeners/SendShipmentNotification.php",
            "<?php\nnamespace App\\Listeners;\nuse App\\Events\\OrderShipped;\nclass SendShipmentNotification\n{\n    public function handle(OrderShipped $event)\n    {\n    }\n}\n",
        ),
        (
            "app/Http/Controllers/OrderController.php",
            "<?php\nnamespace App\\Http\\Controllers;\nclass OrderController\n{\n    public function ship($id)\n    {\n        event(new \\App\\Events\\OrderShipped($id));\n    }\n}\n",
        ),
    ]);
    assert_eq!(
        synth(&store, "ship", "handle").as_deref(),
        Some("laravel-event")
    );
}

#[test]
fn js_stores_and_registries() {
    let (_d, store) = index(&[
        (
            "src/stores/cart.ts",
            "import { defineStore } from 'pinia';\nexport const useCartStore = defineStore('cart', {\n  actions: {\n    addItem(id: string) { return id; },\n  },\n});\n",
        ),
        (
            "src/components/Buy.ts",
            "import { useCartStore } from '../stores/cart';\nexport function buy(id: string) {\n  const cart = useCartStore();\n  cart.addItem(id);\n}\n",
        ),
        (
            "src/store/modules/user.js",
            "export default {\n  namespaced: true,\n  actions: {\n    login(ctx, creds) { return creds; },\n  },\n  mutations: {},\n};\n",
        ),
        (
            "src/views/login.js",
            "export function submit(store) {\n  store.dispatch('user/login', {});\n}\n",
        ),
        (
            "src/commands.ts",
            "class AddCommand { execute() {} }\nclass RemoveCommand { execute() {} }\nexport class Editor {\n  commands = { add: AddCommand, remove: RemoveCommand };\n  run(name: string) {\n    new this.commands[name]().execute();\n  }\n}\n",
        ),
    ]);
    assert_eq!(synth(&store, "buy", "addItem").as_deref(), Some("pinia"));
    assert_eq!(synth(&store, "submit", "login").as_deref(), Some("vuex"));
    assert_eq!(
        synth(&store, "run", "execute").as_deref(),
        Some("object-registry")
    );
}

#[test]
fn redux_thunks_and_rtk_hooks() {
    let (_d, store) = index(&[
        (
            "src/slice.ts",
            "import { createAsyncThunk } from '@reduxjs/toolkit';\nexport const loadUser = createAsyncThunk('user/load', async (id: string) => id);\nexport const refresh = createAsyncThunk('user/refresh', async (id: string, { dispatch }) => {\n  await dispatch(loadUser(id));\n});\n",
        ),
        (
            "src/api.ts",
            "import { createApi } from '@reduxjs/toolkit/query/react';\nexport const api = createApi({\n  endpoints: (build) => ({\n    getUser: build.query({ query: (id) => `users/${id}` }),\n  }),\n});\nexport const { useGetUserQuery } = api;\n",
        ),
    ]);
    assert_eq!(
        synth(&store, "refresh", "loadUser").as_deref(),
        Some("redux-thunk")
    );
    assert_eq!(
        synth(&store, "useGetUserQuery", "getUser").as_deref(),
        Some("rtk-query")
    );
}

#[test]
fn react_native_events() {
    let (_d, store) = index(&[
        (
            "android/src/main/java/geo/GeoModule.java",
            "package geo;\npublic class GeoModule {\n  private ReactContext reactContext;\n  public void onLocation(Location loc) {\n    reactContext.getJSModule(RCTDeviceEventEmitter.class).emit(\"locationUpdate\", body);\n  }\n}\n",
        ),
        (
            "src/geo.ts",
            "import { DeviceEventEmitter } from 'react-native';\nexport function handleLocation(e: unknown) { return e; }\nexport function watch() {\n  DeviceEventEmitter.addListener('locationUpdate', handleLocation);\n}\n",
        ),
    ]);
    assert_eq!(
        synth(&store, "onLocation", "handleLocation").as_deref(),
        Some("rn-event")
    );
}

#[test]
fn interface_dispatch_reaches_overrides() {
    let (_d, store) = index(&[
        ("go.mod", "module example.com/app\n\ngo 1.22\n"),
        (
            "storage/interface.go",
            "package storage\n\ntype Series interface{ Labels() string }\n\ntype SeriesSet interface {\n\tNext() bool\n\tAt() Series\n\tErr() error\n}\n\ntype Closer interface {\n\tClose(force bool) error\n}\n",
        ),
        (
            "tsdb/querier.go",
            "package tsdb\n\nimport \"example.com/app/storage\"\n\ntype base struct {\n\terr error\n}\n\nfunc (b *base) Next() bool { return false }\nfunc (b *base) Err() error { return b.err }\n\ntype blockSeriesSet struct {\n\tbase\n}\n\nfunc use(s storage.SeriesSet) { s.Next() }\n",
        ),
        (
            "tsdb/at.go",
            "package tsdb\n\nimport \"example.com/app/storage\"\n\nfunc (b *blockSeriesSet) At() storage.Series { return nil }\nfunc (b *blockSeriesSet) Close() error { return nil }\n",
        ),
        (
            "src/main/java/app/Repo.java",
            "package app;\npublic interface Repo {\n  void save(String x);\n}\n",
        ),
        (
            "src/main/java/app/SqlRepo.java",
            "package app;\npublic class SqlRepo implements Repo {\n  public void save(String x) {}\n}\n",
        ),
    ]);
    let edge_kind = |from: &str, to: &str, kind: EdgeKind| {
        let tos = store.nodes_named(to).unwrap();
        store.nodes_named(from).unwrap().iter().any(|f| {
            store
                .outgoing_edges(&f.id, &[kind])
                .unwrap()
                .iter()
                .any(|e| tos.iter().any(|t| t.id == e.target))
        })
    };
    assert!(edge_kind(
        "blockSeriesSet",
        "SeriesSet",
        EdgeKind::Implements
    ));
    assert!(
        !edge_kind("blockSeriesSet", "Closer", EdgeKind::Implements),
        "Close arity differs"
    );
    assert!(
        edge_kind("blockSeriesSet", "At", EdgeKind::Contains),
        "cross-file method"
    );
    // SeriesSet.Next → base.Next, promoted through the embedding.
    let next_edges: Vec<_> = store
        .nodes_named("Next")
        .unwrap()
        .into_iter()
        .filter(|n| n.qualified_name == "SeriesSet::Next")
        .flat_map(|n| store.outgoing_edges(&n.id, &[EdgeKind::Calls]).unwrap())
        .collect();
    let base_next = store
        .nodes_named("Next")
        .unwrap()
        .into_iter()
        .find(|n| n.qualified_name == "base::Next")
        .unwrap();
    assert!(
        next_edges.iter().any(|e| e.target == base_next.id),
        "{next_edges:?}"
    );
    assert_eq!(synth(&store, "save", "save").as_deref(), Some("override"));
}

#[test]
fn callbacks_and_rerenders() {
    let (_d, store) = index(&[
        (
            "src/scene.ts",
            "export class Scene {\n  private listeners = new Set<() => void>();\n  onUpdate(fn: () => void) {\n    this.listeners.add(fn);\n  }\n  notify() {\n    this.listeners.forEach((f) => f());\n  }\n}\n",
        ),
        (
            "src/app.ts",
            "import { Scene } from './scene';\nexport class App {\n  scene = new Scene();\n  start() {\n    this.scene.onUpdate(this.repaint);\n  }\n  repaint() {}\n}\n",
        ),
        (
            "src/Counter.tsx",
            "import React from 'react';\nexport class Counter extends React.Component {\n  bump() {\n    this.setState({ n: 1 });\n  }\n  render() {\n    return null;\n  }\n}\n",
        ),
    ]);
    assert_eq!(
        synth(&store, "bump", "render").as_deref(),
        Some("react-render")
    );
    assert_eq!(
        synth(&store, "notify", "repaint").as_deref(),
        Some("callback")
    );
}

#[test]
fn queues_buses_and_sockets() {
    let (_d, store) = index(&[
        (
            "src/mail/mail.service.ts",
            "import { Queue } from 'bullmq';\nimport { InjectQueue } from '@nestjs/bullmq';\nexport class MailService {\n  constructor(@InjectQueue('email') private readonly emails: Queue) {}\n  async signup(user: string) {\n    await this.emails.add('welcome', { user });\n  }\n}\n",
        ),
        (
            "src/mail/mail.processor.ts",
            "import { Processor, Process } from '@nestjs/bull';\n@Processor('email')\nexport class MailProcessor {\n  @Process('welcome')\n  async sendWelcome(job: unknown) {\n    return job;\n  }\n  @Process('reset')\n  async sendReset(job: unknown) {\n    return job;\n  }\n}\n",
        ),
        (
            "src/users/users.service.ts",
            "export class UsersService {\n  constructor(private eventEmitter: EventEmitter2) {}\n  create() {\n    this.eventEmitter.emit('user.created', {});\n  }\n}\n",
        ),
        (
            "src/users/audit.listener.ts",
            "export class AuditListener {\n  @OnEvent('user.*')\n  handleUserEvent(payload: unknown) {\n    return payload;\n  }\n}\n",
        ),
        (
            "src/chat/chat.gateway.ts",
            "@WebSocketGateway()\nexport class ChatGateway {\n  @SubscribeMessage('message')\n  handleMessage(body: string) {\n    return body;\n  }\n}\n",
        ),
        (
            "web/chat.ts",
            "export function send(socket: any, text: string) {\n  socket.emit('message', text);\n}\n",
        ),
    ]);
    assert_eq!(
        synth(&store, "signup", "sendWelcome").as_deref(),
        Some("queue")
    );
    assert_eq!(synth(&store, "signup", "sendReset"), None);
    assert_eq!(
        synth(&store, "create", "handleUserEvent").as_deref(),
        Some("bus")
    );
    assert_eq!(
        synth(&store, "send", "handleMessage").as_deref(),
        Some("bus")
    );
}

#[test]
fn spring_tests_reach_their_routes() {
    let (_d, store) = index(&[
        (
            "pom.xml",
            "<project><dependencies><dependency><artifactId>spring-boot-starter-web</artifactId></dependency></dependencies></project>\n",
        ),
        (
            "src/main/java/app/OwnerController.java",
            "package app;\n@RestController\n@RequestMapping(\"/owners\")\npublic class OwnerController {\n  @PostMapping(\"/new\")\n  public String create() { return \"ok\"; }\n}\n",
        ),
        (
            "src/test/java/app/OwnerControllerTests.java",
            "package app;\nimport static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.*;\nclass OwnerControllerTests {\n  void createsOwner() throws Exception {\n    mockMvc.perform(post(\"/owners/new\"));\n  }\n}\n",
        ),
    ]);
    let test = &store.nodes_named("createsOwner").unwrap()[0];
    let targets: Vec<String> = store
        .outgoing_edges(&test.id, &[EdgeKind::Calls])
        .unwrap()
        .iter()
        .filter_map(|e| store.node(&e.target).unwrap())
        .map(|n| n.name)
        .collect();
    assert!(
        targets.iter().any(|n| n.contains("/owners/new")),
        "{targets:?}"
    );
}

#[test]
fn go_function_fields() {
    let (_d, store) = index(&[
        ("go.mod", "module example.com/app\n\ngo 1.22\n"),
        (
            "discovery/refresh/refresh.go",
            "package refresh\n\nimport \"context\"\n\ntype Options struct {\n\tInterval int\n\tRefreshF func(ctx context.Context) error\n}\n\ntype Discovery struct {\n\trefreshf func(ctx context.Context) error\n}\n\nfunc NewDiscovery(opts Options) *Discovery {\n\treturn &Discovery{refreshf: opts.RefreshF}\n}\n\nfunc (d *Discovery) refresh(ctx context.Context) error {\n\treturn d.refreshf(ctx)\n}\n",
        ),
        (
            "discovery/aws/ec2.go",
            "package aws\n\nimport (\n\t\"context\"\n\t\"example.com/app/discovery/refresh\"\n)\n\ntype EC2Discovery struct{}\n\nfunc (d *EC2Discovery) fetch(ctx context.Context) error { return nil }\n\nfunc New() *refresh.Discovery {\n\td := &EC2Discovery{}\n\treturn refresh.NewDiscovery(refresh.Options{Interval: 5, RefreshF: d.fetch})\n}\n",
        ),
    ]);
    assert_eq!(
        synth(&store, "refresh", "fetch").as_deref(),
        Some("go-func-field")
    );
}
