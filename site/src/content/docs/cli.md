---
title: CLI reference
description: Flags and subcommands for mzp / mizpah.
order: 7
---

`mizpah` and `mzp` are the same binary.

## Global / stream flags

| Flag | Description |
|------|-------------|
| `--service` / `-s` / `MIZPAH_SERVICE` | Service name for this stdin stream (default: `OTEL_SERVICE_NAME` / `SERVICE_NAME` / project manifests such as package.json, Cargo.toml, pyproject.toml, go.mod, … / git root / directory) |
| `--host` | Bind/connect host (default `127.0.0.1`) |
| `--port` / `-p` | Bind/connect port (default `3149`) |
| `--max-bytes` | Ring buffer cap in bytes (default `1073741824`, hub only) |
| `--ttl-hours` | Drop logs older than this many hours (default `24`, hub only; `0` disables) |
| `--no-open` | Do not open a browser when starting as hub |
| `--project` / `MIZPAH_PROJECT` | Project directory for Check with Claude/Cursor (default: hub cwd) |

Durable on-disk buffer (optional) is configured in `config.toml` as `persistDir` — not a CLI flag. Segments are encrypted at rest automatically; see [Storage security](../storage-security/).

## Subcommands

| Command | Description |
|---------|-------------|
| `mzp attach` / `attach shell` | Enable shell stdout/stderr capture for new interactive shells |
| `mzp attach browser` | CDP console + network (alias: `mzp browser attach`) |
| `mzp attach cursor` / `attach claude` | Install observe-only agent hooks |
| `mzp detach` / `detach shell` / `cursor` / `claude` / `all` | Disable shell and/or remove agent hooks (hub left running) |
| `mzp hub start` | Start a detached hub if one is not already healthy |
| `mzp hub stop` | Stop the hub for this port (via PID file) |
| `mzp hub restart` | Stop then start (clears the in-memory buffer) |
| `mzp open` | Open the web UI (hub must already be reachable) |
| `mzp ingest` / `mzp files` | Ingest local files/globs (gzip/bzip2; optional `--follow`; SSH `user@host:path` unless secure) |
| `mzp query` | CEL query (or `--group-by` aggregate) against the hub |
| `mzp sql` | Run a `SELECT` against the hub snapshot |
| `mzp script` | Run a line-oriented script (`query` / `aggregate`) |
| `mzp tui` | Minimal terminal UI against a running hub |
| `mzp setup` | Ensure hub, install MCP configs, print skill next steps (`--with-skill`, `--skip-mcp-install`) |
| `mzp doctor` | Readiness checks (binary, hub, MCP configs, npx) |
| `mzp why` | Incident summary for the last N minutes (`--minutes`, default 15) |
| `mzp run -- <cmd…>` | Ensure hub, run command, forward stdout/stderr, emit `process.exit` |
| `mzp mcp` | Stdio MCP server (auto-starts loopback hub; or `MIZPAH_URL`) |
| `mzp mcp install` | Merge MCP config into Cursor / Claude / Codex |
| `mzp mcp uninstall` | Remove those MCP entries |

See [attach](../attach/), [streaming](../streaming/), [storage security](../storage-security/), and [MCP](../mcp/) for behavior details.
