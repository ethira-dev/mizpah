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
| `just lint-rust` | `cargo fmt --check` + clippy (incl. curated pedantic) |
| `just lint-deps` | `cargo deny check` + `cargo machete` |
| `just lint-web` | eslint + tsc |
| `just check` | lint-rust + test + lint-web (matches PR CI core) |
| `just site-dev` / `site-build` | Docs site (`site/`, base `/mizpah/`) |

CI: `.github/workflows/ci.yml` (fmt, clippy, test, cargo-deny, machete, miri, audit). Pages: `.github/workflows/pages.yml` → [ethira-dev.github.io/mizpah](https://ethira-dev.github.io/mizpah/).

## Hub trust model

The hub exposes unauthenticated ingest, query, investigate, and update APIs. Binding defaults to `127.0.0.1`.

- Loopback binds (`127.0.0.1`, `::1`, `localhost`) are always allowed.
- Non-loopback binds require `--allow-remote` and print a warning. Prefer an SSH tunnel or an authenticating reverse proxy if you expose Mizpah beyond the machine.

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

Rust modules under `crates/mizpah/src/`:

| Area | Modules |
|------|---------|
| Hub lifecycle | `hub/` (probe, spawn, start/stop, PID file), defaults |
| Shared helpers | `util/` (config dir, atomic write, PATH/`which`, shell quote), `ingest_forward` |
| Store / API | `store/{ingest,query,activity}`, `api/{routes,ws,static_files}`, `models`, `error` |
| Attach | `shell_attach/`, `browser_attach/`, `agent_hooks/`, `shell_forward` |
| Agents / MCP | `mcp/`, `investigate`, `filter`, `properties` |
| CLI | `cli` (clap + dispatch), `main` (pipe mode + `run_hub`) |

Web UI under `web/` (`hooks/use-mizpah` + `mizpah-connection`, `lib/{api,types,log-format}`). Marketing/docs under `site/`. Wire-shape fixtures: `crates/mizpah/tests/fixtures/` (Rust) and `web/src/lib/api-contract.ts` (TS).

## License

MIT. See the [repository](https://github.com/ethira-dev/mizpah).
