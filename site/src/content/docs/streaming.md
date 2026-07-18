---
title: Streaming & hub
description: Bind vs attach, ring buffer, ingest API, entry shape, and normalization.
order: 3
---

The hub is process-local and in-memory. You get a fast UI for browsing streams; agents reuse that buffer instead of re-executing commands or stuffing full stdout into context.

## Bind vs attach

| Role | When | Behavior |
|------|------|----------|
| Hub | First process that binds `--host`/`--port` (default `127.0.0.1:3149`) | Serves SPA, REST, `/ws`, ring buffer, writes `hub-{port}.pid` |
| Attach | Port already taken | Forwards lines with `POST /api/ingest` (or batch) to the existing hub |

```bash
api-server | mzp --service api --project /path/to/repo
# second process on same host/port attaches automatically
worker | mzp --service worker
```

Useful flags: `--no-open`, `--max-bytes` (default `1073741824`), `--ttl-hours` (default `24`; `0` disables), `--project` / `MIZPAH_PROJECT` (cwd for Check with Claude / Cursor).

## HTTP surface (hub)

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/api/ingest` | Single line (`service`, `line`, optional `cmd`, `mzp`) |
| `POST` | `/api/ingest/batch` | Up to 128 lines per request |
| `GET` | `/api/logs` | Newest-first entries; `q` (CEL), `service`, `cursor`, `limit`, `from`, `to` |
| `GET` | `/api/properties` | Discovered JSON paths + samples |
| `GET` | `/api/services` | Active / blocked services |
| `POST` | `/api/services/disconnect` · `/reconnect` | Pause / resume a service tag |
| `GET` | `/api/stats` | Entry count, bytes, per-service counts |
| `GET` | `/api/activity` | Time buckets for the activity strip |
| `POST` | `/api/investigate` | Launch local Claude / Cursor seeded on an entry id |
| `GET` | `/ws` | Live push of new entries |

CORS is permissive for local tooling. Nothing leaves the machine unless you expose the bind address yourself.

## Entry shape

Each stored entry is roughly:

```json
{
  "id": 1842,
  "receivedAt": "2026-07-16T22:01:00.000Z",
  "service": "api",
  "data": {
    "level": "error",
    "msg": "timeout waiting for redis",
    "_mzp": { "cwd": "/Users/you/dev/api", "user": "you", "pid": 1234, "exe": "…" }
  }
}
```

- `service`: stream tag from `--service` or attach source defaults.
- `data`: parsed JSON object, or `{ "_raw": "…" }` for non-JSON.
- `_mzp`: receiver metadata (always present; client may supply it on ingest).

## Normalization

1. Prefer **NDJSON** (one JSON object per line).
2. Multi-line pretty dumps (Nest / Node `util.inspect` style) are buffered and reassembled when the parser can close the object.
3. Everything else is stored as `_raw`.

## UI behavior

- Virtualized list (newest at top when auto-scroll is on).
- CEL filter bar with autocomplete from discovered properties.
- Row click opens detail: JSON tree/raw, neighbor context, Check with Claude / Cursor.

See [CEL](../cel/) for query bindings and [MCP](../mcp/) for agent access to the same APIs.
