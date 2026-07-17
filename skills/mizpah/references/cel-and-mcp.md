# CEL and MCP reference

## MCP tools

| Tool | Parameters | Notes |
|------|------------|--------|
| `list_services` | — | Service names in the buffer |
| `get_stats` | — | Entry count, bytes, per-service counts |
| `list_properties` | `service?`, `q?` | Discovered JSON paths + samples |
| `search_logs` | `q?` (CEL), `service?`, `limit?`, `cursor?` | Newest-first; default limit **20**, max **50** |
| `get_logs_around` | `id`, `before?` (5), `after?` (5), `service?`, `q?` | Window around an entry |

Hub URL: `MIZPAH_URL` or `http://127.0.0.1:3149`. Register clients with `mzp mcp install`.

## CEL examples

```cel
level == "error"
msg.contains("timeout")
service == "api" && level == "warn"
_raw.contains("ECONNREFUSED")
```

GitHub Actions logs from `gh run view --log-failed` are plain text and land in `_raw`:

```cel
_raw.contains("FAIL")
_raw.contains("error:")
service == "gha" && _raw.contains("panic")
```

Use `list_properties` to discover real paths before inventing field names. Nested paths and `_mzp` metadata (`cwd`, `user`, `pid`, `exe`) are available when present.

## JSON logging reminders

- Prefer NDJSON over pretty-printed multi-line dumps when the process allows.
- Non-JSON lines land in `_raw`; pretty Nest / `util.inspect` blocks may be reassembled.
- Enabling JSON at the source improves autocomplete and CEL precision.

Full CEL docs: https://ethira-dev.github.io/mizpah/docs/cel/
