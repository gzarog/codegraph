---
title: Quickstart
description: Install CodeGraph, index a project and connect your coding agent in a few minutes.
---

## 1. Install

```bash
curl -fsSL https://raw.githubusercontent.com/gzarog/codegraph/main/install.sh | sh
```

On Windows, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/gzarog/codegraph/main/install.ps1 | iex
```

Other options, including building from source, are on the [Installation](/getting-started/installation/) page.

## 2. Index your project

```bash
cd your-project
codegraph init
```

This creates `.codegraph/` and indexes every source file. Add `.codegraph/` to your `.gitignore`.

## 3. Connect your agent

```bash
codegraph install
```

The installer detects which agents you have (Claude Code, Cursor, Codex, Gemini CLI and others), asks whether to configure them globally or just for this project, and registers the MCP server. Restart your agent afterwards.

## 4. Ask

In your agent, ask a structural question: *"How does a request reach the database in this project?"* The agent calls `codegraph_explore` and answers from the returned source instead of searching file by file.

You can ask CodeGraph directly too:

```bash
codegraph explore "how does login reach the session store"
codegraph ui
```
