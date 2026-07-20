---
title: CEL filters
description: Query bindings, operators, REST usage, and how agents should filter.
order: 5
---

The UI filter bar and `GET /api/logs?q=` evaluate [CEL](https://cel.dev/) against each log entry. Tight filters keep the stream readable and keep MCP `search_logs` results small (and cheap) for agents.

Empty `q` matches everything.

**Natural language:** the UI and `POST /api/nl-cel` compile phrases like `errors from api last 15 minutes` into CEL. MCP `search_logs` accepts the same via `nl`. Prefer CEL when you already know the expression.

## Bindings

| Binding | Meaning |
|---------|---------|
| `service` | Stream service tag |
| `level` | First present of `level` / `severity` / `lvl` in `data` (string, lowercased for matching helpers) |
| `source` | Attach source when present (`browser`, `cursor`, `claude`, …) |
| `kind` | Event kind (browser: `console` / `network`; agent hooks: lifecycle name) |
| `cmd` | Full shell command when attach shell recorded it; also any JSON field named `cmd` |
| `_mzp.*` | Receiver metadata (`cwd`, `user`, `pid`, `exe`) |
| *fields* | Top-level keys from `data` (nested via `.`, e.g. `user.id`) |

Property discovery for autocomplete and MCP: `GET /api/properties` / tool `list_properties`.

## Examples

```cel
service == "api" && level == "error"
cmd.contains("npm test")
msg.contains("timeout") || error.contains("timeout")
has(user.id) && user.id == "42"
level in ["error", "warn"]
msg.matches("(?i)time.?out")
source == "browser" && kind == "network" && status >= 400
```

## REST

```bash
curl -sS -G "http://127.0.0.1:3149/api/logs" \
  --data-urlencode 'q=level == "error"' \
  --data-urlencode 'service=api' \
  --data-urlencode 'limit=20'

curl -sS -G "http://127.0.0.1:3149/api/properties" \
  --data-urlencode 'q=redis'
```

Logs are newest-first. Paginate with `cursor` (return entries with `id` strictly less than the cursor).

## For agents

Prefer specific CEL + small limits over broad dumps:

```text
search_logs: q='level == "error"', service='api', limit=20
get_logs_around: id=<from result>, before=5, after=5
```

See [MCP](../mcp/) for tool parameters and hard caps.
