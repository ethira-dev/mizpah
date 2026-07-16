---
title: Development
description: Build the SPA and Rust binary, run the local gate, and how the hub attaches.
order: 8
---

```bash
just release
# or
cd web && npm install && npm run build
cargo build --release

./target/release/mzp --no-open
```

Useful targets:

```bash
just install    # UI + install binary to ~/.cargo/bin
just ui         # rebuild SPA only
just build      # UI + debug binary
just test       # Rust unit tests
just web-dev    # Vite dev server (proxies to :1738)
just lint-rust  # cargo fmt --check + clippy
just lint-web   # eslint + tsc
just check      # lint-rust + test + lint-web (matches PR CI)
just site-dev   # this documentation site
```

Pull requests run the same Rust and web checks via GitHub Actions (`.github/workflows/ci.yml`).

## Architecture

```
stdin ──► try bind :1738
                          ├─ success → hub (Axum + ring buffer + UI + hub-{port}.pid)
                          └─ AddrInUse → attach (POST /api/ingest)

mzp attach shell   ──► shell hooks ──► tee stdout/stderr ──► POST /api/ingest/batch
mzp attach browser ──► CDP ──► console/network ──► POST /api/ingest/batch
mzp attach cursor  ──► ~/.cursor/hooks.json ──► __hook-forward ──► POST /api/ingest
mzp attach claude  ──► ~/.claude/settings.json ──► __hook-forward ──► POST /api/ingest
mzp hub            ──► start | stop | restart detached hub on :1738
mzp open           ──► browser → http://127.0.0.1:1738
```

## License

MIT. See the [repository](https://github.com/ethira-dev/mizpah).
