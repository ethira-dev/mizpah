# Mizpah build helpers
# https://github.com/casey/just

default:
    @just --list

# Install web dependencies
web-install:
    cd web && npm install

# Dev UI (proxies API/WS to hub on :1738)
web-dev:
    cd web && npm run dev

# Build SPA into crates/mizpah/static
ui:
    cd web && npm run build

# Debug Rust binary (rebuilds after UI if you run `just build`)
build: ui
    cargo build

# Release binary with embedded UI
release: ui
    cargo build --release

# Install `mizpah` and `mzp` onto PATH (~/.cargo/bin) and register MCP with AI clients
install: ui
    cargo install --path crates/mizpah --force
    # Prefer the cargo-installed binary so Homebrew's older `mizpah` does not shadow `mcp`
    "{{env_var_or_default('CARGO_HOME', home_directory() / '.cargo')}}/bin/mizpah" mcp install

# Same as install, then restart the hub only if it is already running
reinstall: install
    #!/usr/bin/env bash
    set -euo pipefail
    bin="{{env_var_or_default('CARGO_HOME', home_directory() / '.cargo')}}/bin/mzp"
    if curl -sf --max-time 1 "http://127.0.0.1:1738/api/stats" >/dev/null; then
        "$bin" hub restart
    else
        echo "mizpah hub not running; skip restart"
    fi

# Run hub (example): just run api
run service='demo' *args='':
    cargo run -q -- --service {{service}} --no-open {{args}}

test:
    cargo test -p mizpah

# Docs / marketing site (GitHub Pages)
site-install:
    cd site && npm install

site-dev:
    cd site && npm run dev

site-build:
    cd site && npm run build

# Frontend lint + typecheck
lint-web:
    cd web && npm run lint && npm run typecheck

# Rust format + clippy (matches CI curated pedantic denies)
lint-rust:
    cargo fmt --check
    cargo clippy -p mizpah --all-targets -- \
      -D warnings \
      -D clippy::manual_assert \
      -D clippy::uninlined_format_args \
      -D clippy::map_unwrap_or \
      -D clippy::redundant_clone

# Supply-chain / unused deps (requires cargo-deny + cargo-machete installed)
lint-deps:
    cargo deny check
    cargo machete

# Full local gate (same as PR CI core, minus npm ci / deny / miri)
check: lint-rust test lint-web
