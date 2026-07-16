---
title: CLI reference
description: Flags and subcommands for mzp / mizpah.
order: 7
---

| Flag / command | Description |
|----------------|-------------|
| `--service` / `-s` | Service name for this stdin stream (default: absolute cwd) |
| `--host` | Bind/connect host (default `127.0.0.1`) |
| `--port` / `-p` | Bind/connect port (default `1738`) |
| `--max-bytes` | Ring buffer cap in bytes (default `1073741824`, hub only) |
| `--no-open` | Do not open a browser when starting as hub |
| `--project` / `MIZPAH_PROJECT` | Project directory for Check with Claude/Cursor (default: hub cwd) |
| `mzp attach` / `attach shell` | Enable shell stdout/stderr capture for new interactive shells |
| `mzp attach browser` | CDP console + network (alias: `mzp browser attach`) |
| `mzp attach cursor` / `attach claude` | Install observe-only agent hooks into the hub |
| `mzp detach` / `detach shell` / `cursor` / `claude` / `all` | Disable shell and/or remove agent hooks (hub left running) |
| `mzp hub start` | Start a detached hub if one is not already healthy |
| `mzp hub stop` | Stop the hub for this port (via PID file) |
| `mzp hub restart` | Stop then start (clears the in-memory buffer) |
| `mzp open` | Open the web UI (hub must already be reachable) |
| `mzp mcp` | Stdio MCP server (hub at `:1738`, or `MIZPAH_URL`) |
| `mzp mcp install` | Merge MCP config into Cursor / Claude / Codex |
| `mzp mcp uninstall` | Remove those MCP entries |

`mizpah` is an alias for the same binary as `mzp`.
