#!/usr/bin/env node
/**
 * Runs upstream CodeGraph's own test files, unmodified, against the Rust port.
 *
 *   node run.mjs [filter ...]     e.g. `node run.mjs cpp- c-`
 *
 * Clones upstream at the pinned commit into `.upstream/`, copies its
 * `__tests__` into `.work/__tests__` and puts `shim/` at `.work/src`, so a
 * test's `import { CodeGraph } from '../src'` lands on the shim. A test file
 * that imports anything the shim doesn't export (the resolver's internals, a
 * module the port has no TypeScript twin of) is left out and listed. Filters keep the files whose name starts with one of
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

/** Node builtins and test helpers a test may import. */
const BUILTINS = /^(vitest|node:.*|fs|os|path|child_process|url|util|crypto|events|stream|readline|assert)$/;

/**
 * The value imports of a test file: `{ specifier, names }`, `names` holding
 * what it takes from the module (`default` for a default import, `*` for a
 * namespace one). Type-only imports are erased before the test runs.
 */
function imports(source) {
  const out = [];
  const statement = /^\s*import\s+(type\s+)?([^'";]*?)\s*from\s*['"]([^'"]+)['"]|^\s*import\s*['"]([^'"]+)['"]/gm;
  for (const m of source.matchAll(statement)) {
    if (m[4]) {
      out.push({ specifier: m[4], names: [] });
      continue;
    }
    if (m[1]) continue;
    const clause = m[2];
    const names = [];
    const braces = /\{([^}]*)\}/.exec(clause);
    if (braces) {
      for (const part of braces[1].split(',')) {
        const item = part.trim();
        if (item === '' || item.startsWith('type ')) continue;
        names.push(item.split(/\s+as\s+/)[0].trim());
      }
    }
    const head = clause.replace(/\{[^}]*\}/, '').replace(/,\s*$/, '').trim();
    if (/^\*\s+as\s+/.test(head)) names.push('*');
    else if (head !== '' && head !== ',') names.push('default');
    out.push({ specifier: m[3], names });
  }
  for (const m of source.matchAll(/\bimport\(\s*['"]([^'"]+)['"]\s*\)/g)) out.push({ specifier: m[1], names: ['*'] });
  return out;
}

/** The shim file standing in for upstream's `../src/<rest>` module, or null. */
function shimFile(specifier) {
  const rest = specifier === '../src' ? 'index' : specifier.replace(/^\.\.\/src\//, '');
  for (const candidate of [`${rest}.ts`, `${rest}/index.ts`]) {
    const file = path.join(here, 'shim', candidate);
    if (fs.existsSync(file)) return file;
  }
  return null;
}

/** The names a shim file exports (`default` included). */
function exportsOf(file) {
  const text = fs.readFileSync(file, 'utf8');
  const names = new Set();
  for (const m of text.matchAll(/^export\s+(?:async\s+)?(?:function\*?|const|let|class|interface|type|enum)\s+([A-Za-z_$][\w$]*)/gm)) names.add(m[1]);
  for (const m of text.matchAll(/^export\s+(?:type\s+)?\{([^}]*)\}/gm)) {
    for (const part of m[1].split(',')) {
      const name = part.trim().split(/\s+as\s+/).pop().trim();
      if (name) names.add(name.replace(/^type\s+/, ''));
    }
  }
  if (/^export\s+default\b/m.test(text)) names.add('default');
  return names;
}

/** Why a test file can't run against the shim (what it imports that the shim lacks), or null. */
function unsupported(source) {
  const missing = [];
  for (const { specifier, names } of imports(source)) {
    if (BUILTINS.test(specifier)) continue;
    if (!specifier.startsWith('../src')) {
      missing.push(specifier);
      continue;
    }
    const file = shimFile(specifier);
    if (!file) {
      missing.push(specifier);
      continue;
    }
    const have = exportsOf(file);
    const lacking = names.filter((n) => n !== '*' && !have.has(n));
    if (lacking.length) missing.push(`${specifier} {${lacking.join(', ')}}`);
  }
  return missing.length ? [...new Set(missing)].join(', ') : null;
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
    const why = unsupported(source);
    if (why) {
      skipped.push(`${name}  (${why})`);
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
