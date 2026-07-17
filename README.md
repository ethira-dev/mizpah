# mizpah

Local JSON log hub: pipe any process into **`mzp`**, get a searchable UI on `:3149`, and let agents query the same in-memory buffer over MCP.

**UX:** filterable live stream, property autocomplete, row detail with JSON tree. Better than `tail -f` when you need to find something.

**Cost:** agents call `search_logs` / `get_logs_around` for small CEL slices instead of pasting dumps or re-running tests and lint. Same answers, thinner context.

**[Docs & demos](https://ethira-dev.github.io/mizpah/)** · [Quick start](https://ethira-dev.github.io/mizpah/docs/quick-start/) · [Install](https://ethira-dev.github.io/mizpah/docs/install/) · [MCP](https://ethira-dev.github.io/mizpah/docs/mcp/)

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/ethira-dev/mizpah)](https://github.com/ethira-dev/mizpah/releases/latest)
[![dependency status](https://deps.rs/repo/github/ethira-dev/mizpah/status.svg)](https://deps.rs/repo/github/ethira-dev/mizpah)

```bash
brew install ethira-dev/mizpah/mizpah
my-app 2>&1 | mzp --service api
# UI: http://127.0.0.1:3149
```

## Install

**Homebrew**

```bash
brew install ethira-dev/mizpah/mizpah
mzp --help
```

**From source** (Rust stable, Node 20+)

```bash
just install   # builds web UI, cargo install → ~/.cargo/bin, mcp install
# or:
cd web && npm ci && npm run build
cargo install --path crates/mizpah --force
mzp mcp install
```

**Prebuilt:** [GitHub Releases](https://github.com/ethira-dev/mizpah/releases) (`mizpah` + `mzp` binaries).

If `mzp` is missing after a Cargo install: `export PATH="$HOME/.cargo/bin:$PATH"`. Prefer `~/.cargo/bin` ahead of Homebrew when both are installed.

## How it works

```
stdin ──► try bind :3149
            ├─ success → hub (Axum, ring buffer, SPA, hub-{port}.pid)
            └─ AddrInUse → attach (POST /api/ingest)

attach shell|browser|cursor|claude ──► ingest → same buffer
MCP (stdio) ──► GET /api/logs|properties|stats|… against the hub
```

- Default bind: `127.0.0.1:3149`. Ring buffer default: 1 GiB (`--max-bytes`).
- Prefer NDJSON. Pretty Nest / `util.inspect` blocks are reassembled when possible; other non-JSON becomes `{ "_raw": "…" }`.
- Every entry gets `_mzp` (`cwd`, `user`, `pid`, `exe`).

Full reference: [streaming](https://ethira-dev.github.io/mizpah/docs/streaming/), [attach](https://ethira-dev.github.io/mizpah/docs/attach/), [CEL](https://ethira-dev.github.io/mizpah/docs/cel/), [MCP](https://ethira-dev.github.io/mizpah/docs/mcp/), [CLI](https://ethira-dev.github.io/mizpah/docs/cli/).

## Quick commands

```bash
api-server 2>&1 | mzp --service api
worker | mzp --service worker

mzp attach                 # shell tee for new interactive shells
mzp attach browser --launch
mzp attach cursor          # observe-only Cursor hooks
mzp attach claude
mzp open
mzp mcp install            # Cursor / Claude / Codex
mzp hub stop               # also: start | restart
```

MCP tools (keep `search_logs` limits small: default 20, max 50): `list_services`, `get_stats`, `list_properties`, `search_logs`, `get_logs_around`.

## Agent skill

Installable Agent Skill for Cursor and other agents — optimized for **token / cost savings** (pipe → JSON logs → small MCP queries, not pasted dumps):

```bash
npx skills add ethira-dev/mizpah
```

Cursor plugin layout lives at the repo root (`.cursor-plugin/`, `skills/mizpah/`, `mcp.json`). See [PLUGIN.md](PLUGIN.md). Marketplace: submit / install from Customize once listed. Cross-client MCP remains `mzp mcp install`.

## Development

```bash
just check      # fmt/clippy + tests + web lint (matches CI)
just web-dev    # Vite → proxies API/WS to :3149
just site-dev   # docs site at /mizpah/
just ui         # rebuild SPA into crates/mizpah/static
just release
```

PRs run `.github/workflows/ci.yml`. The docs site deploys via `.github/workflows/pages.yml`.

## License

MIT
