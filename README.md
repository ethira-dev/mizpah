# Mizpah

JSON log viewer with a modern web UI. Pipe NDJSON into Mizpah and filter logs by discovered properties.

```bash
my-app 2>&1 | mizpah --service api
```

## Install

### Homebrew

```bash
brew install ethira-dev/mizpah/mizpah
```

### From source

Requirements: [Rust](https://rustup.rs/) (stable) and Node.js 20+.

```bash
# From this repo — puts mizpah on PATH (~/.cargo/bin)
just install

# Without just:
cd web && npm ci && npm run build
cargo install --path crates/mizpah --force
```

Then run from anywhere:

```bash
mizpah --help
echo '{"msg":"hi"}' | mizpah --service demo
```

If you get `command not found`, ensure Cargo’s bin dir is on your PATH:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

### Prebuilt binaries (GitHub Releases)

After a `v*` tag is published, download the archive for your platform from the [Releases](https://github.com/ethira-dev/mizpah/releases) page:

```bash
# Apple Silicon example
curl -L https://github.com/ethira-dev/mizpah/releases/latest/download/mizpah-aarch64-apple-darwin.tar.gz \
  | tar -xz
mv mizpah ~/.local/bin/   # or: sudo mv mizpah /usr/local/bin/
```

Asset names:

| Platform | Archive |
|----------|---------|
| macOS Apple Silicon | `mizpah-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `mizpah-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 | `mizpah-x86_64-unknown-linux-gnu.tar.gz` |

## Features

- **Required `--service`** — tag each stream; run multiple processes and switch services in the UI
- **Hub + attach** — first process binds `:1738` and serves the UI; later processes forward to it
- **In-memory ring buffer** — default 1 GiB; oldest logs are evicted when full
- **Property discovery** — nested paths like `user.id` with filter chips (`=`, `!=`, `contains`, `>`, `<`, `exists`)
- **Live WebSocket stream** — virtualized log list, pause/resume tail
- **Pretty-object reassembly** — Nest-style multiline `{` … `}` dumps become structured JSON when ingest can convert them

## Usage

```bash
# Hub (starts UI at http://127.0.0.1:1738)
api-server | mizpah --service api

# Attach more services to the same hub
worker | mizpah --service worker
cron   | mizpah --service cron
```

### CLI

| Flag | Description |
|------|-------------|
| `--service` / `-s` | **Required.** Service name for this stdin stream |
| `--host` | Bind/connect host (default `127.0.0.1`) |
| `--port` / `-p` | Bind/connect port (default `1738`) |
| `--max-bytes` | Ring buffer cap in bytes (default `1073741824`, hub only) |
| `--no-open` | Do not open a browser when starting as hub |

Lines that are not JSON objects are stored as `{ "_raw": "…" }`.

NestJS / `util.inspect`-style multiline object dumps (e.g. `{` then `key: 'value',` lines then `}`) are reassembled into a single JSON object when possible. Prefer NDJSON from your logger when you control the format.

## Development

```bash
# Build UI into crates/mizpah/static, then compile
just release
# or
cd web && npm install && npm run build
cargo build --release

./target/release/mizpah --service demo --no-open
```

Useful targets:

```bash
just install   # UI + install binary to ~/.cargo/bin
just ui        # rebuild SPA only
just build     # UI + debug binary
just test      # Rust unit tests
just web-dev   # Vite dev server (proxies to :1738)
```

### Architecture

```
stdin --service api ──► try bind :1738
                          ├─ success → hub (Axum + ring buffer + UI)
                          └─ AddrInUse → attach (POST /api/ingest)
```

## License

MIT
