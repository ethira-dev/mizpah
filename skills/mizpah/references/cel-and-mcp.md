# CEL and MCP reference

## MCP tools

| Tool | Parameters | Notes |
|------|------------|--------|
| `list_services` | — | Service names in the buffer |
| `get_stats` | — | Entry count, bytes, per-service counts |
| `list_properties` | `service?`, `q?` | Discovered JSON paths + samples |
| `search_logs` | `q?` (CEL), `nl?` (natural language), `service?`, `limit?`, `cursor?` | Newest-first; default limit **20**, max **50** |
| `get_logs_around` | `id`, `before?` (5), `after?` (5), `service?`, `q?` | Window around an entry |
| `aggregate_logs` | `group_by?`, `q?`, `service?`, `limit?` | Top-N counts |
| `get_trace` / `list_traces` | `opid` / `limit?` | Request correlation |
| `query_sql` | `sql`, `limit?` | Snapshot `SELECT`; max 50 via MCP |
| `summarize_incident` | `minutes?` | What broke? brief |
| `list_bookmarks` / `nav_level` / `spectrogram` | — | Annotations, error nav, heat-map |

Tool results are **TOON** (Token-Oriented Object Notation), not pretty JSON — same fields, fewer tokens. Log rows omit `_mzp`. Hub REST stays JSON. Details: https://ethira-dev.github.io/mizpah/docs/mcp/#tool-result-format-toon

Hub URL: `MIZPAH_URL` or `http://127.0.0.1:3149`. Prefer `mzp setup` (or `mzp mcp install`) to register clients. MCP auto-starts a loopback hub when needed.

## CEL examples

```cel
level == "error"
msg.contains("timeout")
service == "api" && level == "warn"
_raw.contains("ECONNREFUSED")
```

Natural language (compiled server-side):

```text
search_logs nl='errors from api'
search_logs nl='contains timeout'
```

GitHub Actions / plain text often have `msg` and `_raw`:

```cel
msg.contains("FAIL") || _raw.contains("FAIL")
service == "gha" && (level == "error" || _raw.contains("panic"))
```

Use `list_properties` to discover real paths before inventing field names. Nested paths and `_mzp` metadata (`cwd`, `user`, `pid`, `exe`) are available when present.

## JSON logging reminders

- Prefer NDJSON over pretty-printed multi-line dumps when the process allows.
- Non-JSON lines are promoted to `msg` / `_raw` (and level when heuristics match).
- Enabling JSON at the source improves autocomplete and CEL precision.

Full CEL docs: https://ethira-dev.github.io/mizpah/docs/cel/
