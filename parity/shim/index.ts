/**
 * Upstream CodeGraph's `CodeGraph` class, as far as its test suite uses it,
 * backed by this repository's Rust binary and the SQLite index it writes.
 *
 * Upstream's `__tests__/*.test.ts` import `CodeGraph` from `../src`; the
 * parity runner (`parity/run.mjs`) puts this file there, so the tests run
 * unmodified against the Rust port. Indexing and syncing shell out to the
 * `codegraph` binary; every query reads the index directly. Both schemas
 * share their tables and columns, so rows map one to one.
 */
import { execFileSync } from 'node:child_process';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { binary, binaryEnv } from './binary';

export type NodeKind = string;
export type EdgeKind = string;

export interface Node {
  id: string;
  kind: NodeKind;
  name: string;
  qualifiedName: string;
  filePath: string;
  language: string;
  startLine: number;
  endLine: number;
  startColumn: number;
  endColumn: number;
  docstring?: string;
  signature?: string;
  visibility?: string;
  isExported: boolean;
  isAsync: boolean;
  isStatic: boolean;
  isAbstract: boolean;
  decorators?: string[];
  typeParameters?: string[];
  returnType?: string;
  updatedAt: number;
}

export interface Edge {
  source: string;
  target: string;
  kind: EdgeKind;
  metadata?: Record<string, unknown>;
  line?: number;
  column?: number;
  provenance?: string;
}

export interface UnresolvedReference {
  fromNodeId: string;
  referenceName: string;
  referenceKind: string;
  line: number;
  column: number;
  filePath?: string;
  language?: string;
}

export interface Subgraph {
  nodes: Map<string, Node>;
  edges: Edge[];
  roots: string[];
}

type Row = Record<string, unknown>;

const json = (text: unknown): any => {
  if (typeof text !== 'string' || text === '') return undefined;
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
};

function toNode(r: Row): Node {
  return {
    id: r.id as string,
    kind: r.kind as string,
    name: r.name as string,
    qualifiedName: r.qualified_name as string,
    filePath: r.file_path as string,
    language: r.language as string,
    startLine: r.start_line as number,
    endLine: r.end_line as number,
    startColumn: r.start_column as number,
    endColumn: r.end_column as number,
    docstring: (r.docstring as string | null) ?? undefined,
    signature: (r.signature as string | null) ?? undefined,
    visibility: (r.visibility as string | null) ?? undefined,
    isExported: r.is_exported === 1,
    isAsync: r.is_async === 1,
    isStatic: r.is_static === 1,
    isAbstract: r.is_abstract === 1,
    decorators: json(r.decorators),
    typeParameters: json(r.type_parameters),
    returnType: (r.return_type as string | null) ?? undefined,
    updatedAt: r.updated_at as number,
  };
}

function toEdge(r: Row): Edge {
  return {
    source: r.source as string,
    target: r.target as string,
    kind: r.kind as string,
    metadata: json(r.metadata),
    line: (r.line as number | null) ?? undefined,
    column: (r.col as number | null) ?? undefined,
    provenance: (r.provenance as string | null) ?? undefined,
  };
}


let captured = 0;

/**
 * With `$CODEGRAPH_PARITY_CAPTURE` set, keep a copy of every project a test
 * indexes there, named after the test, so it can be indexed again by upstream
 * and the two graphs compared.
 */
function capture(root: string): void {
  const into = process.env.CODEGRAPH_PARITY_CAPTURE;
  if (!into) return;
  const test = (globalThis as { expect?: { getState(): { testPath?: string; currentTestName?: string } } }).expect?.getState();
  const file = path.basename(test?.testPath ?? 'unknown', '.test.ts');
  const name = `${file}--${test?.currentTestName ?? ''}--${captured++}`.replace(/[^A-Za-z0-9._-]+/g, '_').slice(0, 200);
  fs.cpSync(root, path.join(into, name), {
    recursive: true,
    filter: (src) => path.basename(src) !== '.codegraph',
  });
}

function run(args: string[], cwd: string): void {
  if (args[0] !== 'init' || !args.includes('--no-index')) capture(cwd);
  execFileSync(binary(), args, {
    cwd,
    stdio: ['ignore', 'pipe', 'pipe'],
    env: binaryEnv(),
  });
}

const CHUNK = 500;

export class CodeGraph {
  private db: DatabaseSync;

  private constructor(private readonly root: string) {
    this.db = new DatabaseSync(path.join(root, '.codegraph', 'codegraph.db'));
  }

  static async init(projectRoot: string, options: { index?: boolean } = {}): Promise<CodeGraph> {
    const root = path.resolve(projectRoot);
    run(options.index === false ? ['init', root, '--no-index'] : ['init', root], root);
    return new CodeGraph(root);
  }

  static async open(projectRoot: string): Promise<CodeGraph> {
    const root = path.resolve(projectRoot);
    if (!fs.existsSync(path.join(root, '.codegraph', 'codegraph.db'))) run(['init', root], root);
    return new CodeGraph(root);
  }

  static initSync(projectRoot: string, options: { index?: boolean } = {}): CodeGraph {
    const root = path.resolve(projectRoot);
    run(options.index === false ? ['init', root, '--no-index'] : ['init', root], root);
    return new CodeGraph(root);
  }

  close(): void {
    try {
      this.db.close();
    } catch {
      // already closed
    }
  }

  destroy(): void {
    this.close();
    fs.rmSync(path.join(this.root, '.codegraph'), { recursive: true, force: true });
  }

  getProjectRoot(): string {
    return this.root;
  }

  private reopen(): void {
    this.close();
    this.db = new DatabaseSync(path.join(this.root, '.codegraph', 'codegraph.db'));
  }

  async indexAll(): Promise<{ success: boolean }> {
    this.close();
    run(['index', this.root], this.root);
    this.reopen();
    return { success: true };
  }

  async sync(): Promise<{ success: boolean }> {
    this.close();
    run(['sync', this.root], this.root);
    this.reopen();
    return { success: true };
  }

  private nodes(sql: string, ...params: unknown[]): Node[] {
    return (this.db.prepare(sql).all(...(params as never[])) as Row[]).map(toNode);
  }

  private edges(sql: string, ...params: unknown[]): Edge[] {
    return (this.db.prepare(sql).all(...(params as never[])) as Row[]).map(toEdge);
  }

  getNode(id: string): Node | null {
    return this.nodes('SELECT * FROM nodes WHERE id = ?', id)[0] ?? null;
  }

  getNodesByIds(ids: readonly string[]): Map<string, Node> {
    const out = new Map<string, Node>();
    const unique = [...new Set(ids)];
    for (let i = 0; i < unique.length; i += CHUNK) {
      const chunk = unique.slice(i, i + CHUNK);
      for (const n of this.nodes(`SELECT * FROM nodes WHERE id IN (${chunk.map(() => '?').join(',')})`, ...chunk)) out.set(n.id, n);
    }
    return out;
  }

  getNodesByQualifiedName(qualifiedName: string): Node[] {
    return this.nodes('SELECT * FROM nodes WHERE qualified_name = ? ORDER BY file_path, start_line, id', qualifiedName);
  }

  getNodesInFile(filePath: string): Node[] {
    return this.nodes('SELECT * FROM nodes WHERE file_path = ? ORDER BY start_line, id', filePath);
  }

  getNodesInFiles(filePaths: readonly string[]): Node[] {
    return filePaths.flatMap((f) => this.getNodesInFile(f));
  }

  getNodesByKind(kind: NodeKind): Node[] {
    return this.nodes('SELECT * FROM nodes WHERE kind = ? ORDER BY file_path, start_line, id', kind);
  }

  getNodesByName(name: string): Node[] {
    return this.nodes('SELECT * FROM nodes WHERE name = ? ORDER BY file_path, start_line, id', name);
  }

  getAllNodes(): Node[] {
    return this.nodes('SELECT * FROM nodes ORDER BY file_path, start_line, id');
  }

  searchNodes(query: string, options: { limit?: number; kinds?: string[] } = {}): Array<{ node: Node; score: number }> {
    const like = `%${query}%`;
    let rows = this.nodes("SELECT * FROM nodes WHERE kind != 'file' AND (name LIKE ? OR qualified_name LIKE ?) ORDER BY length(name), file_path, start_line", like, like);
    if (options.kinds?.length) rows = rows.filter((n) => options.kinds!.includes(n.kind));
    return rows.slice(0, options.limit ?? 20).map((node) => ({ node, score: node.name === query ? 1 : 0.5 }));
  }

  private kindFilter(kinds?: readonly string[]): { sql: string; params: string[] } {
    return kinds && kinds.length > 0 ? { sql: ` AND kind IN (${kinds.map(() => '?').join(',')})`, params: [...kinds] } : { sql: '', params: [] };
  }

  getOutgoingEdges(nodeId: string, kinds?: EdgeKind[]): Edge[] {
    const k = this.kindFilter(kinds);
    return this.edges(`SELECT * FROM edges WHERE source = ?${k.sql} ORDER BY kind, target, line, col`, nodeId, ...k.params);
  }

  getIncomingEdges(nodeId: string, kinds?: EdgeKind[]): Edge[] {
    const k = this.kindFilter(kinds);
    return this.edges(`SELECT * FROM edges WHERE target = ?${k.sql} ORDER BY kind, source, line, col`, nodeId, ...k.params);
  }

  getOutgoingEdgesFrom(nodeIds: readonly string[], kinds?: EdgeKind[]): Edge[] {
    const unique = [...new Set(nodeIds)];
    const k = this.kindFilter(kinds);
    const out: Edge[] = [];
    for (let i = 0; i < unique.length; i += CHUNK) {
      const chunk = unique.slice(i, i + CHUNK);
      out.push(...this.edges(`SELECT * FROM edges WHERE source IN (${chunk.map(() => '?').join(',')})${k.sql}`, ...chunk, ...k.params));
    }
    return out;
  }

  getIncomingEdgesTo(nodeIds: readonly string[], kinds?: EdgeKind[]): Edge[] {
    const unique = [...new Set(nodeIds)];
    const k = this.kindFilter(kinds);
    const out: Edge[] = [];
    for (let i = 0; i < unique.length; i += CHUNK) {
      const chunk = unique.slice(i, i + CHUNK);
      out.push(...this.edges(`SELECT * FROM edges WHERE target IN (${chunk.map(() => '?').join(',')})${k.sql}`, ...chunk, ...k.params));
    }
    return out;
  }

  getUnresolvedReferencesInFile(filePath: string, limit?: number): UnresolvedReference[] {
    const rows = this.db
      .prepare(`SELECT * FROM refs WHERE file_path = ? AND status != 'resolved' ORDER BY id${limit ? ' LIMIT ?' : ''}`)
      .all(...((limit ? [filePath, limit] : [filePath]) as never[])) as Row[];
    return rows.map((r) => ({
      fromNodeId: r.from_node_id as string,
      referenceName: r.reference_name as string,
      referenceKind: r.reference_kind as string,
      line: r.line as number,
      column: r.col as number,
      filePath: (r.file_path as string | null) ?? undefined,
      language: (r.language as string | null) ?? undefined,
    }));
  }

  private walk(nodeId: string, maxDepth: number, outgoing: boolean): Array<{ node: Node; edge: Edge }> {
    const kinds = ['calls', 'references', 'imports', 'instantiates', 'navigates'];
    const result: Array<{ node: Node; edge: Edge }> = [];
    const visited = new Map<string, number>();
    const reported = new Set([nodeId]);
    const nearer = (id: string, depth: number) => {
      const seen = visited.get(id);
      return seen === undefined || depth < seen;
    };
    const recurse = (id: string, depth: number) => {
      if (!nearer(id, depth)) return;
      visited.set(id, depth);
      if (depth >= maxDepth) return;
      const edges = outgoing ? this.getOutgoingEdges(id, kinds) : this.getIncomingEdges(id, kinds);
      const others = this.getNodesByIds(edges.map((e) => (outgoing ? e.target : e.source)));
      for (const edge of edges) {
        const other = others.get(outgoing ? edge.target : edge.source);
        if (!other || !nearer(other.id, depth + 1)) continue;
        if (!reported.has(other.id)) {
          reported.add(other.id);
          result.push({ node: other, edge });
        }
        recurse(other.id, depth + 1);
      }
    };
    recurse(nodeId, 0);
    return result;
  }

  getCallers(nodeId: string, maxDepth = 1): Array<{ node: Node; edge: Edge }> {
    return this.walk(nodeId, maxDepth, false);
  }

  getCallees(nodeId: string, maxDepth = 1): Array<{ node: Node; edge: Edge }> {
    return this.walk(nodeId, maxDepth, true);
  }

  getImpactRadius(nodeId: string, maxDepth = 3): Subgraph {
    const focal = this.getNode(nodeId);
    if (!focal) return { nodes: new Map(), edges: [], roots: [] };
    const nodes = new Map<string, Node>([[focal.id, focal]]);
    const edges: Edge[] = [];
    const visited = new Map<string, number>();
    const expanded = new Set<string>();
    const containers = new Set(['class', 'interface', 'struct', 'union', 'trait', 'protocol', 'module', 'enum']);
    const nearer = (id: string, depth: number) => {
      const seen = visited.get(id);
      return seen === undefined || depth < seen;
    };
    const recurse = (id: string, depth: number) => {
      if (!nearer(id, depth)) return;
      visited.set(id, depth);
      if (depth >= maxDepth) return;
      const first = !expanded.has(id);
      expanded.add(id);
      const node = this.getNode(id);
      if (node && containers.has(node.kind)) {
        const contains = this.getOutgoingEdges(id, ['contains']);
        const children = this.getNodesByIds(contains.map((e) => e.target));
        for (const edge of contains) {
          const child = children.get(edge.target);
          if (child && nearer(child.id, depth)) {
            if (!nodes.has(child.id)) {
              nodes.set(child.id, child);
              edges.push(edge);
            }
            recurse(child.id, depth);
          }
        }
      }
      const incoming = this.getIncomingEdges(id).filter((e) => e.kind !== 'contains');
      const sources = this.getNodesByIds(incoming.map((e) => e.source));
      for (const edge of incoming) {
        const source = sources.get(edge.source);
        if (!source) continue;
        if (first) edges.push(edge);
        if (nearer(source.id, depth + 1)) {
          nodes.set(source.id, source);
          recurse(source.id, depth + 1);
        }
      }
    };
    recurse(nodeId, 0);
    return { nodes, edges, roots: [nodeId] };
  }

  getChildren(nodeId: string): Node[] {
    const contains = this.getOutgoingEdges(nodeId, ['contains']);
    const nodes = this.getNodesByIds(contains.map((e) => e.target));
    return contains.map((e) => nodes.get(e.target)).filter((n): n is Node => n !== undefined);
  }

  getAncestors(nodeId: string): Node[] {
    const out: Node[] = [];
    const seen = new Set([nodeId]);
    let current = nodeId;
    for (;;) {
      const parent = this.getIncomingEdges(current, ['contains'])[0];
      if (!parent || seen.has(parent.source)) break;
      const node = this.getNode(parent.source);
      if (!node) break;
      out.push(node);
      seen.add(node.id);
      current = node.id;
    }
    return out;
  }

  /** The files `filePath`'s symbols depend on, through edges other than `contains`. */
  getFileDependencies(filePath: string): string[] {
    const rows = this.db
      .prepare(`SELECT DISTINCT t.file_path AS f FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
        WHERE s.file_path = ? AND t.file_path != ? AND e.kind != 'contains' ORDER BY f`)
      .all(filePath, filePath) as Row[];
    return rows.map((r) => r.f as string);
  }

  /** The files whose symbols depend on `filePath`'s. */
  getFileDependents(filePath: string): string[] {
    const rows = this.db
      .prepare(`SELECT DISTINCT s.file_path AS f FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
        WHERE t.file_path = ? AND s.file_path != ? AND e.kind != 'contains' ORDER BY f`)
      .all(filePath, filePath) as Row[];
    return rows.map((r) => r.f as string);
  }
}

export default CodeGraph;
