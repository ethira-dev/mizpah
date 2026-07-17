---
title: MCP & agents
description: Stdio MCP tools, install targets, limits, and UI investigate hooks.
order: 6
---

mizpah exposes the live hub as a stdio MCP server (`mzp mcp`). Agents get structured tools against `/api/*` instead of pasted transcripts. That is both a clearer workflow (ask for errors, get rows) and a cheaper one (default 20 hits, max 50).

## Install

```bash
# hub must be reachable (pipe a process or mzp hub start)
mzp mcp install     # merges config into Cursor, Claude Desktop, Claude Code, Codex when present
# restart clients
mzp mcp uninstall   # remove those entries
```

First hub start also attempts registration. Homebrew / release installs: run `mzp mcp install` once after install if tools do not appear.

Override hub URL with `MIZPAH_URL` (default `http://127.0.0.1:1738`).

## Agent skill

Workflow skill (pipe into Mizpah, prefer JSON logs, query MCP with small limits) for Cursor and other agents that support [Agent Skills](https://agentskills.io):

```bash
npx skills add ethira-dev/mizpah
```

Also available as a Cursor plugin (repo-root `.cursor-plugin/` + `skills/mizpah/`). Install from the Cursor Marketplace / Customize when listed, or symlink locally — see the repo [PLUGIN.md](https://github.com/ethira-dev/mizpah/blob/main/PLUGIN.md). `mzp mcp install` remains the path for Claude Desktop, Codex, and other MCP clients.

## Tools

| Tool | Parameters | Notes |
|------|------------|--------|
| `list_services` | (none) | Service names in the buffer |
| `get_stats` | (none) | Entry count, approx bytes, max bytes, per-service counts |
| `list_properties` | `service?`, `q?` | Discovered paths + sample values (for writing CEL) |
| `search_logs` | `q?` (CEL), `service?`, `limit?`, `cursor?` | Newest-first; **default limit 20, max 50**; `hasMore` for pagination |
| `get_logs_around` | `id`, `before?` (default 5), `after?` (default 5), `service?`, `q?` | Window around an entry for stack/context |

Server instructions tell the model to keep limits small and never dump the full buffer. If the hub is down, start a stream: `my-app 2>&1 | mzp --service <name>`.

### Example agent flow

```text
1. list_properties (optional) → learn fields
2. search_logs q='level == "error"' service='api' limit=10
3. get_logs_around id=<id> before=5 after=5
```

## Investigate from the UI

Log detail → **Check with Claude** or **Check with Cursor** calls `POST /api/investigate`, which launches a local `claude` or `agent` CLI session seeded with that entry and instructions to use MCP for surrounding context.

Requirements:

- `claude` or `agent` on `PATH`
- `--project` / `MIZPAH_PROJECT` set if the hub was started outside the repo you care about

## Related

- [CEL](../cel/) for filter syntax
- [Attach](../attach/) to also ingest Cursor / Claude lifecycle events into the buffer
