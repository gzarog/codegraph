---
title: Affected Tests in CI
description: Use the graph to run only the tests a change can affect.
---

`codegraph affected` lists the test files that depend, directly or through other code, on the files you changed.

```bash
codegraph affected src/auth/session.ts src/db/pool.ts
git diff --name-only origin/main | codegraph affected --stdin
```

| Option | Meaning |
|---|---|
| `--stdin` | Read changed files from standard input, one per line. |
| `--depth N` | How many dependency hops to follow (default 5). |
| `--filter GLOB` | Report files matching this glob instead of test files, such as `"e2e/**"`. |
| `--json` | Machine-readable output. |
| `--quiet` | Print only the paths. |

Test files are recognised by their names and directories (`*.test.ts`, `*_test.go`, `test_*.py`, `tests/`, `__tests__/`, `spec/` and similar).

## In CI

```yaml
- run: curl -fsSL https://raw.githubusercontent.com/gzarog/codegraph/main/install.sh | sh
- run: ~/.local/bin/codegraph init --force .
- run: |
    git diff --name-only origin/${{ github.base_ref }} \
      | ~/.local/bin/codegraph affected --stdin --quiet > affected.txt
- run: npx vitest run $(cat affected.txt)
```

Run the full suite when `affected.txt` is empty and the change touched non-source files (configuration, dependencies), since the graph only knows about code.
