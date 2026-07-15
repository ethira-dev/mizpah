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

# Install `mizpah` onto PATH (~/.cargo/bin) and register MCP with AI clients
install: ui
    cargo install --path crates/mizpah --force
    # Prefer the cargo-installed binary so Homebrew's older `mizpah` does not shadow `mcp`
    "{{env_var_or_default('CARGO_HOME', home_directory() / '.cargo')}}/bin/mizpah" mcp install

# Run hub (example): just run api
run service='demo' *args='':
    cargo run -q -- --service {{service}} --no-open {{args}}

test:
    cargo test -p mizpah

# Frontend lint + typecheck
lint-web:
    cd web && npm run lint && npm run typecheck

# Rust format + clippy
lint-rust:
    cargo fmt --check
    cargo clippy -p mizpah -- -D warnings

# Full local gate (same as PR CI, minus npm ci)
check: lint-rust test lint-web
