---
title: Quick start
description: Bind a hub, stream NDJSON, filter in the UI, and optionally wire MCP.
order: 1
---

mizpah is a local in-memory JSON log hub. The first process that can bind `127.0.0.1:1738` becomes the hub (Axum API, WebSocket fan-out, SPA, ring buffer). Later processes attach by POSTing to `/api/ingest`.

**UX:** virtualized live stream, CEL search with field autocomplete, click-through JSON detail.

**Cost:** agents should query that buffer over MCP (`search_logs`, default limit 20) instead of pasting stdout or re-running tests and lint for every question.

## Pipe a service

```bash
api-server 2>&1 | mzp --service api
# opens http://127.0.0.1:1738
worker | mzp --service worker   # joins the same hub
```

`--service` tags the stream (default: absolute cwd). Prefer NDJSON. Non-JSON lines become `{ "_raw": "…" }`; pretty Nest / `util.inspect` blocks are reassembled when possible.

## Or attach a source

```bash
mzp attach                 # shell: tee new interactive zsh/bash shells
mzp attach browser --launch
mzp attach cursor          # observe-only Cursor hooks → hub
mzp attach claude          # observe-only Claude Code hooks → hub
mzp open
mzp mcp install            # register MCP in Cursor / Claude / Codex
```

`mizpah` is an alias for `mzp`.

## Agent skill (token savings)

Teach the agent to pipe into Mizpah, prefer JSON logs, and query MCP with small limits instead of pasting stdout:

```bash
npx skills add ethira-dev/mizpah
```

Works with Cursor, Claude Code, Codex, and other [Agent Skills](https://agentskills.io) clients. Pair with `mzp mcp install` so tools are available. Full detail: [MCP & agents](../mcp/#agent-skill).

## Next

- [Install](../install/)
- [Streaming & hub protocol](../streaming/)
- [Attach sources](../attach/)
- [CEL](../cel/)
- [MCP tools](../mcp/)
- [CLI reference](../cli/)
