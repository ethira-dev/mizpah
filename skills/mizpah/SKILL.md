---
name: mizpah
description: >-
  Run commands through Mizpah and query the live JSON log hub over MCP to save
  tokens. Use when debugging with logs, piping process output, attaching shell
  or browser sources, writing CEL filters, or when the user mentions mizpah, mzp,
  or avoiding pasted stdout / re-runs for log questions.
---

# Mizpah

Local JSON log hub: pipe processes into `mzp`, browse `:1738`, query the same buffer via MCP. Prefer hub queries over pasting dumps or re-running tests/lint for every log question.

## Capture

```bash
my-app 2>&1 | mzp --service <name>
# or: mzp attach / mzp attach browser --launch / mzp attach cursor
```

- Tag streams with `--service` (default: absolute cwd).
- Hub: `http://127.0.0.1:1738` (`MIZPAH_URL` to override). If MCP fails because the hub is down, start a stream first.

## Prefer JSON output

Whenever a tool or runtime can emit structured logs, enable it so Mizpah parses fields for CEL and property discovery:

- Flags: `--json`, `--log-format=json`, `-o json`
- Env: `LOG_FORMAT=json`, `RUST_LOG_FORMAT=json`, framework JSON loggers

Prefer **NDJSON** (one JSON object per line). Plain text becomes `{ "_raw": "…" }` and is harder to filter.

## Query (MCP) — keep context small

Tools: `list_services`, `get_stats`, `list_properties`, `search_logs`, `get_logs_around`.

1. `list_properties` (optional) → field paths / samples
2. `search_logs` with CEL, `limit` ≤ 20 (max 50); paginate with `cursor` / `hasMore`
3. `get_logs_around` for stack/context around an `id`

Never dump the full buffer into chat. Do not paste large stdout when MCP can answer.

```text
search_logs q='level == "error"' service='api' limit=10
get_logs_around id=<id> before=5 after=5
```

CEL examples and tool notes: [references/cel-and-mcp.md](references/cel-and-mcp.md).

## Setup

```bash
mzp mcp install   # Cursor, Claude Desktop/Code, Codex when present
```

`mizpah` is an alias for `mzp`. Docs: https://ethira-dev.github.io/mizpah/docs/mcp/
