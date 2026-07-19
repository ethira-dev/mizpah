---
title: SQL & aggregations
description: CEL-first filters plus aggregate_logs and SELECT over all_logs.
order: 7
---

CEL remains the default filter language in the UI and MCP `search_logs`. For analytics, Mizpah adds aggregations and a read-only SQL snapshot.

## Aggregations

```bash
mzp query 'level == "error"' --group-by service -n 10
```

REST: `GET /api/aggregate?q=…&groupBy=service,level&limit=20` (or `POST /api/aggregate` with a JSON body for longer CEL)  
MCP: `aggregate_logs`

The web UI Tools sheet exposes SQL, aggregations, bookmarks, and spectrogram against the same REST APIs.

## SQL

Snapshot table `all_logs` columns: `id`, `received_at`, `event_time`, `service`, `format_id`, `level`, `msg`, `data` (JSON text).

```bash
mzp sql "SELECT service, count(*) AS n FROM all_logs WHERE level = 'error' GROUP BY 1 ORDER BY n DESC"
```

REST: `POST /api/sql` with `{ "sql": "SELECT …", "limit": 100 }`  
MCP: `query_sql` (capped at 50 rows)

Only a single `SELECT` / `WITH … SELECT` is allowed. Dangerous keywords (`ATTACH`, `PRAGMA`, writes, …) are rejected.

## Scripts

```bash
mzp script ./report.mzp
```

Line-oriented commands: `query …`, `aggregate --group-by level …`, `#` comments.
