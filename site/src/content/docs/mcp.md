---
title: MCP & agents
description: Point Cursor, Claude, or Codex at the hub, and investigate from any log row.
order: 6
---

## Let agents query the hub

Keep a hub running. Point Cursor, Claude Desktop, Claude Code, or Codex at mizpah MCP. Agents use `search_logs`, `list_services`, `get_stats`, `list_properties`, and `get_logs_around` for small CEL slices, not a paste of the whole buffer.

```bash
my-app 2>&1 | mzp --service api
mzp mcp install     # or: first hub start auto-registers
# restart your IDE/client, then ask: "what errors did api emit in the last few minutes?"
mzp mcp uninstall   # opt out
```

Homebrew / release installs: run `mzp mcp install` once after install (or start a hub once).

## Investigate from the UI

Open a log → **Check with Claude** or **Check with Cursor**. mizpah launches a local `claude` or `agent` session seeded with that entry and instructions to pull surrounding context via MCP.

Requires the Claude Code (`claude`) or Cursor Agent (`agent`) CLI on `PATH`. If the hub was started elsewhere (or via `mzp attach`), set `--project` / `MIZPAH_PROJECT` so the agent lands in the right repo.

## Related

- [Attach Cursor / Claude hooks](../attach/) to ingest agent lifecycle events into the same buffer
- [CEL filters](../cel/) for the queries agents (and you) should prefer
