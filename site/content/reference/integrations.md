---
title: Agent Integrations
description: The coding agents codegraph install supports, and exactly what it changes for each.
---

```bash
codegraph install                       # interactive: pick a location and agents
codegraph install --yes                 # global config, every detected agent
codegraph install --target claude,cursor --location local
codegraph install --print-config codex  # print the snippet, change nothing
codegraph uninstall --yes
```

| Agent | id | Global config | Project config |
|---|---|---|---|
| Claude Code | `claude` | `~/.claude.json` | `.mcp.json` |
| Cursor | `cursor` | `~/.cursor/mcp.json` | `.cursor/mcp.json` |
| Codex CLI | `codex` | `~/.codex/config.toml` | `.codex/config.toml` |
| opencode | `opencode` | `~/.config/opencode/opencode.json(c)` | `opencode.json(c)` |
| Gemini CLI | `gemini` | `~/.gemini/settings.json` | `.gemini/settings.json` |
| Kiro | `kiro` | `~/.kiro/settings/mcp.json` | `.kiro/settings/mcp.json` |
| Antigravity | `antigravity` | `~/.gemini/config/mcp_config.json` | none |
| Hermes Agent | `hermes` | `~/.hermes/config.yaml` | none |
| GitHub Copilot (VS Code) | `copilot-vscode` | VS Code user `mcp.json` | `.vscode/mcp.json` |
| GitHub Copilot CLI | `copilot-cli` | `~/.copilot/mcp-config.json` | none |
| GitHub Copilot (JetBrains) | `copilot-jetbrains` | JetBrains `mcp.json` | none |

## What changes

- **Only CodeGraph's own entry** in each config file is added or updated. Comments, formatting and every other setting are preserved, in JSON, JSONC, TOML and YAML alike.
- **An instructions block**, between marker comments, in `CLAUDE.md`, `AGENTS.md` or `GEMINI.md` for agents that read one.
- **Claude Code permissions**: the `mcp__codegraph__*` tools are pre-allowed so the agent isn't prompted on every call. Pass `--no-permissions` to skip this.
- **Optional prompt hook** (`--prompt-hook`, Claude Code): structural questions get `explore` output attached before the agent starts. Prompts that aren't about code structure are left alone.

Re-running `install` changes nothing that is already right. `uninstall` removes exactly what `install` added, and deletes a config file only if CodeGraph's entry was the only thing in it.
