#!/usr/bin/env node
/**
 * Runs upstream CodeGraph's own test files, unmodified, against the Rust port.
 *
 *   node run.mjs [filter ...]     e.g. `node run.mjs cpp- c-`
 *
 * Clones upstream at the pinned commit into `.upstream/`, copies its
 * `__tests__` into `.work/__tests__` and puts `shim/` at `.work/src`, so a
 * test's `import { CodeGraph } from '../src'` lands on the shim. A test file
 * that imports anything else from upstream's `src/` (the extractor, the
 * resolver's internals) tests code the port has no TypeScript twin of, so it
 * is left out and listed. Filters keep the files whose name starts with one of
 * them; with none, every supported file runs.
 *
 * The binary is `$CODEGRAPH_BIN`, else `../target/release/codegraph`.
 */
import { execFileSync, spawnSync } from 'node:child_process';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';

const UPSTREAM = 'https://github.com/colbymchenry/codegraph.git';
const COMMIT = process.env.CODEGRAPH_UPSTREAM_COMMIT ?? '01e0ac3';

const here = path.dirname(fileURLToPath(import.meta.url));
const upstream = path.join(here, '.upstream');
const work = path.join(here, '.work');

const git = (...args) => execFileSync('git', args, { cwd: upstream, stdio: 'inherit' });

function checkout() {
  if (!fs.existsSync(path.join(upstream, '.git'))) {
    fs.mkdirSync(upstream, { recursive: true });
    git('init', '-q');
    git('remote', 'add', 'origin', UPSTREAM);
  }
  const head = spawnSync('git', ['rev-parse', 'HEAD'], { cwd: upstream, encoding: 'utf8' }).stdout.trim();
  if (head.startsWith(COMMIT)) return;
  const fetched = spawnSync('git', ['fetch', '-q', '--depth', '1', 'origin', COMMIT], { cwd: upstream, stdio: 'inherit' });
  // A short SHA can't be fetched by itself; fall back to the full history.
  if (fetched.status !== 0) git('fetch', '-q', 'origin');
  git('checkout', '-q', '--detach', COMMIT);
}

/** Module specifiers a test may import: vitest, node builtins, and the shim's two modules. */
const SUPPORTED = /^(vitest|node:.*|fs|os|path|child_process|url|util|crypto|\.\.\/src|\.\.\/src\/index|\.\.\/src\/types)$/;

function imports(source) {
  const statements = /^\s*(?:import|export)\b[^;'"]*?(?:\bfrom\s*)?['"]([^'"]+)['"]/gm;
  const dynamic = /\bimport\(\s*['"]([^'"]+)['"]\s*\)/g;
  return [...source.matchAll(statements), ...source.matchAll(dynamic)].map((m) => m[1]);
}

function stage(filters) {
  fs.rmSync(work, { recursive: true, force: true });
  fs.mkdirSync(path.join(work, '__tests__'), { recursive: true });
  fs.cpSync(path.join(here, 'shim'), path.join(work, 'src'), { recursive: true });

  const tests = path.join(upstream, '__tests__');
  const picked = [];
  const skipped = [];
  for (const name of fs.readdirSync(tests).sort()) {
    if (!name.endsWith('.test.ts')) continue;
    if (filters.length && !filters.some((f) => name.startsWith(f))) continue;
    const source = fs.readFileSync(path.join(tests, name), 'utf8');
    const foreign = imports(source).filter((s) => !SUPPORTED.test(s));
    if (foreign.length) {
      skipped.push(`${name}  (${[...new Set(foreign)].join(', ')})`);
      continue;
    }
    fs.writeFileSync(path.join(work, '__tests__', name), source);
    picked.push(name);
  }
  return { picked, skipped };
}

const args = process.argv.slice(2);
const passthrough = args.indexOf('--');
const filters = passthrough < 0 ? args : args.slice(0, passthrough);
const vitestArgs = passthrough < 0 ? [] : args.slice(passthrough + 1);

const exe = process.platform === 'win32' ? 'codegraph.exe' : 'codegraph';
const binary = path.resolve(process.env.CODEGRAPH_BIN ?? path.join(here, '..', 'target', 'release', exe));
if (!fs.existsSync(binary)) {
  console.error(`no binary at ${binary}: run \`cargo build --release\` or set CODEGRAPH_BIN`);
  process.exit(1);
}

checkout();
const { picked, skipped } = stage(filters);
if (skipped.length) {
  console.log(`skipped ${skipped.length} file(s) that import upstream internals:`);
  for (const s of skipped) console.log(`  ${s}`);
}
if (!picked.length) {
  console.log('no test files to run');
  process.exit(0);
}
console.log(`running ${picked.length} upstream test file(s)`);

const vitest = path.join(here, 'node_modules', 'vitest', 'vitest.mjs');
const run = spawnSync(process.execPath, [vitest, 'run', '--config', path.join(here, 'vitest.config.mts'), ...vitestArgs], {
  cwd: here,
  stdio: 'inherit',
  env: { ...process.env, CODEGRAPH_BIN: binary },
});
process.exit(run.status ?? 1);
