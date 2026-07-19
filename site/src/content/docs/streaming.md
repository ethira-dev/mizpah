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
| `GET` | `/api/activity` | Time buckets for the activity strip (includes error/warn/other) |
| `GET`/`POST` | `/api/aggregate` | Group-by counts (+ optional sum/avg/min/max) |
| `GET` | `/api/nav/level` | Next/prev error or warn relative to an id |
| `GET` | `/api/trace/{opid}` · `/api/traces` | Trace correlation |
| `GET`/`POST` | `/api/bookmarks` | Annotations (mark/tags/comment) |
| `GET` | `/api/spectrogram` | Time × value heat-map for a field |
| `POST` | `/api/sql` | `SELECT` against a snapshot of `all_logs` |
| `POST` | `/api/investigate` | Launch local Claude / Cursor seeded on an entry id |
| `GET` | `/ws` | Live push of new entries |

CORS is permissive for local tooling. Nothing leaves the machine unless you expose the bind address yourself.

## Entry shape

Each stored entry is roughly:

```json
{
  "id": 1842,
  "receivedAt": "2026-07-16T22:01:00.000Z",
  "eventTime": "2026-07-16T22:00:58.100Z",
  "formatId": "json",
  "service": "api",
  "data": {
    "level": "error",
    "msg": "timeout waiting for redis",
    "timestamp": "2026-07-16T22:00:58.100Z",
    "_mzp": { "cwd": "/Users/you/dev/api", "user": "you", "pid": 1234, "exe": "…" }
  }
}
```

- `service`: stream tag from `--service` or attach source defaults.
- `receivedAt`: ingest time (used for ring TTL / max-bytes eviction).
- `eventTime`: parsed from common payload fields (`timestamp`, `@timestamp`, `time`, `ts`, …), falling back to `receivedAt`. Activity strip and `from`/`to` filters use this.
- `formatId`: detected format (`json`, `logfmt`, `raw`, …).
- `data`: parsed JSON object, or `{ "_raw": "…" }` for non-JSON.
- `_mzp`: receiver metadata (always present; client may supply it on ingest).

Config lives under the Mizpah config dir (`MIZPAH_CONFIG_DIR` or the platform project config path): `config.toml`, plus `formats/`, `themes/`, `scripts/`.

## Normalization

1. Prefer **NDJSON** (one JSON object per line).
2. Multi-line pretty dumps (Nest / Node `util.inspect` style) are buffered and reassembled when the parser can close the object.
3. Otherwise try built-in formats (**logfmt**, syslog, access_log, generic), then store as `_raw`.
4. `eventTime` is taken from common payload fields (`timestamp`, `@timestamp`, `time`, `ts`, …); TTL eviction still uses `receivedAt`.

## Formats & SQL

- Format id is stored on each entry (`formatId`) and as `_format` inside `data` when a non-JSON parser wins.
- `POST /api/sql` snapshots the ring into an in-memory SQLite table `all_logs` (`id`, `received_at`, `event_time`, `service`, `format_id`, `level`, `msg`, `data`) and runs a single `SELECT` (multi-statement / DDL rejected).

## UI behavior

- Virtualized list (newest at top when auto-scroll is on).
- CEL filter bar with autocomplete from discovered properties.
- Row click opens detail: JSON tree/raw, neighbor context, Check with Claude / Cursor.

See [CEL](../cel/) for query bindings, [Log formats](../formats/) for detection and packs, [SQL & aggregations](../sql/) for analytics, and [MCP](../mcp/) for agent access to the same APIs.
