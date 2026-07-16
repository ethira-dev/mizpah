---
title: Development
description: Build SPA + Rust binary, local gates, site, and architecture map.
order: 8
---

## Build & run

```bash
just release
# or
cd web && npm install && npm run build
cargo build --release
./target/release/mzp --no-open
```

| Target | Purpose |
|--------|---------|
| `just install` | UI + `cargo install` to `~/.cargo/bin` + `mcp install` |
| `just ui` | Rebuild SPA into `crates/mizpah/static` |
| `just build` | UI + debug binary |
| `just test` | Rust unit tests |
| `just web-dev` | Vite (proxies API/WS to `:1738`) |
| `just lint-rust` | `cargo fmt --check` + clippy |
| `just lint-web` | eslint + tsc |
| `just check` | lint-rust + test + lint-web (matches PR CI) |
| `just site-dev` / `site-build` | Docs site (`site/`, base `/mizpah/`) |

CI: `.github/workflows/ci.yml`. Pages: `.github/workflows/pages.yml` → [ethira-dev.github.io/mizpah](https://ethira-dev.github.io/mizpah/).

## Architecture

```
stdin ──► try bind :1738
            ├─ success → hub (Axum + ring buffer + UI + hub-{port}.pid)
            └─ AddrInUse → attach (POST /api/ingest)

mzp attach shell   ──► shell hooks ──► tee ──► POST /api/ingest/batch
mzp attach browser ──► CDP ──► console/network ──► POST /api/ingest/batch
mzp attach cursor  ──► ~/.cursor/hooks.json ──► __hook-forward ──► POST /api/ingest
mzp attach claude  ──► ~/.claude/settings.json ──► __hook-forward ──► POST /api/ingest
mzp mcp            ──► stdio MCP ──► HubClient ──► GET /api/logs|properties|stats|…
mzp open           ──► browser → http://127.0.0.1:1738
```

Crates live under `crates/mizpah`. Web UI under `web/`. Marketing/docs under `site/`.

## License

MIT. See the [repository](https://github.com/ethira-dev/mizpah).
