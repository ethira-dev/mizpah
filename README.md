# Mizpah

Stop drowning in `tail -f`. Pipe JSON into **`mzp`**, get a live web UI in under a second, hook your whole shell, and hand the same hub to Cursor, Claude, or Codex — so agents *query* your logs instead of guessing from a paste.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/ethira-dev/mizpah)](https://github.com/ethira-dev/mizpah/releases/latest)

![Mizpah UI — live JSON log stream](docs/images/ui-stream.png)

![Properties sidebar for services, levels, and fields](docs/images/ui-properties.png)

![CEL filter modal with recipes and field inserts](docs/images/ui-filter.png)

![Log detail with tree view and Check with Claude / Cursor](docs/images/ui-detail.png)

```bash
my-app 2>&1 | mzp --service api
```

## Why Mizpah

- **Live JSON UI, zero SaaS** — local hub on `:1738`, multi-service, virtualized, pause/resume. No account. No Docker compose novel.
- **Capture your whole terminal** — `mzp attach` forwards stdout/stderr from new zsh/bash shells, tagged with cwd and the command you ran.
- **Filter like you mean it** — [CEL](https://cel.dev/) in the search bar, with autocomplete for every property you’ve actually logged.
- **Agents that can see** — MCP tools so Cursor / Claude / Codex search the live buffer instead of eating a 10k-line dump.
- **One-click investigate** — open a log → **Check with Claude** or **Check with Cursor** and drop into a local agent session already seeded with that entry.

## Get going in 30 seconds

**Pipe a service:**

```bash
api-server 2>&1 | mzp --service api
# UI opens at http://127.0.0.1:1738 — more streams join the same hub automatically
worker | mzp --service worker
```

**Or capture everything you type in new shells:**

```bash
mzp attach    # enable + ensure hub
mzp open      # open the UI
# …open a new terminal, run your stack…
mzp detach    # stop forwarding (hub stays up)
```

(`mizpah` is the same binary if you prefer the long name.)

## What you can do

### Watch live JSON logs

First process binds `:1738` and serves the UI. Everything else attaches. Tag streams with `--service` (defaults to absolute cwd), switch services in the UI, and keep a 1 GiB in-memory ring buffer hot without shipping logs to the cloud.

Pretty-printed Nest / `util.inspect` dumps? Mizpah reassembles them into structured JSON when it can. Non-JSON lines land as `{ "_raw": "…" }`. Prefer NDJSON when you control the logger.

```bash
api-server | mzp --service api --project /path/to/repo
```

### Capture your terminal

`mzp attach` installs shell hooks, starts a background hub if needed, and tees stdout/stderr from **new** interactive shells into Mizpah. Every line gets the command’s absolute cwd (updates after `cd`) and a `cmd` property with the full command string.

```bash
mzp attach --service my-project   # optional: one shared service name
mzp open
mzp hub stop                      # also: start / restart
```

<details>
<summary>Know before you attach</summary>

- Captures stdout/stderr that inherit the shell redirect — not typed input, and not TUI apps that write only to `/dev/tty`
- `cmd` / per-command cwd come from shell hooks, not child process argv
- Programs see pipes instead of a TTY — colors, buffering, and interactivity may change
- Capture is best-effort if the hub is down; your terminal stays responsive
- Shells already open when you first attach need a new window/tab

Hooks live in `${ZDOTDIR:-$HOME}/.zshrc`, `~/.bashrc`, and a bash login file. Remove the `# >>> mizpah >>>` … `# <<< mizpah <<<` blocks anytime — or leave them; `detach` makes `__shell-init` a no-op.

</details>

### Filter with CEL

The filter bar is a real query language — syntax highlighting, property autocomplete, nested paths like `user.id`.

| Binding | Meaning |
|---------|---------|
| `service` | Stream service tag |
| `level` | First of `level` / `severity` / `lvl` in the JSON |
| `cmd` | Full shell command (attach mode); also a normal JSON field when present |
| *fields* | Every top-level key from the log JSON (nested via `.`) |

```cel
service == "api" && level == "error"
cmd.contains("npm test")
msg.contains("timeout") || error.contains("timeout")
has(user.id) && user.id == "42"
level in ["error", "warn"]
msg.matches("(?i)time.?out")
```

Empty query = everything. REST: `GET /api/logs?q=<cel>` · `GET /api/properties?q=<search>`.

### Let agents query the hub

Keep a hub running. Point Cursor, Claude Desktop, Claude Code, or Codex at Mizpah MCP. Agents use `search_logs`, `list_services`, `get_stats`, `list_properties`, and `get_logs_around` — small CEL slices, not a paste of the whole buffer.

```bash
my-app 2>&1 | mzp --service api
mzp mcp install     # or: first hub start auto-registers
# restart your IDE/client, then ask: "what errors did api emit in the last few minutes?"
mzp mcp uninstall   # opt out
```

Homebrew / release installs: run `mzp mcp install` once after install (or start a hub once).

### Investigate from the UI

Open a log → **Check with Claude** or **Check with Cursor**. Mizpah launches a local `claude` or `agent` session seeded with that entry and instructions to pull surrounding context via MCP.

Requires the Claude Code (`claude`) or Cursor Agent (`agent`) CLI on `PATH`. If the hub was started elsewhere (or via `mzp attach`), set `--project` / `MIZPAH_PROJECT` so the agent lands in the right repo.

## Install

### Homebrew

```bash
brew install ethira-dev/mizpah/mizpah
mzp --help
```

### From source

Requirements: [Rust](https://rustup.rs/) (stable) and Node.js 20+.

```bash
# Puts `mzp` and `mizpah` on PATH (~/.cargo/bin)
just install

# Without just:
cd web && npm ci && npm run build
cargo install --path crates/mizpah --force
mzp mcp install
```

`just install` (and the first hub start) register Mizpah as an MCP server in Cursor, Claude Desktop, Claude Code, and Codex when those apps are present. Restart the client after install so tools appear.

```bash
echo '{"msg":"hi"}' | mzp
```

If you get `command not found`, put Cargo’s bin dir on `PATH`:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

If you also have a Homebrew install, ensure `~/.cargo/bin` is before `/opt/homebrew/bin`, or run `~/.cargo/bin/mzp mcp install` — an older brew binary will not understand `mcp` until the tap is updated.

### Prebuilt binaries (GitHub Releases)

Download the archive for your platform from [Releases](https://github.com/ethira-dev/mizpah/releases):

```bash
# Apple Silicon example
curl -L https://github.com/ethira-dev/mizpah/releases/latest/download/mizpah-aarch64-apple-darwin.tar.gz \
  | tar -xz
mv mizpah mzp ~/.local/bin/   # or: sudo mv mizpah mzp /usr/local/bin/
```

| Platform | Archive |
|----------|---------|
| macOS Apple Silicon | `mizpah-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `mizpah-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 | `mizpah-x86_64-unknown-linux-gnu.tar.gz` |

## CLI cheat sheet

| Flag / command | Description |
|----------------|-------------|
| `--service` / `-s` | Service name for this stdin stream (default: absolute cwd) |
| `--host` | Bind/connect host (default `127.0.0.1`) |
| `--port` / `-p` | Bind/connect port (default `1738`) |
| `--max-bytes` | Ring buffer cap in bytes (default `1073741824`, hub only) |
| `--no-open` | Do not open a browser when starting as hub |
| `--project` / `MIZPAH_PROJECT` | Project directory for Check with Claude/Cursor (default: hub cwd) |
| `mzp attach` | Enable shell stdout/stderr capture for new interactive shells |
| `mzp detach` | Disable shell capture (hub left running) |
| `mzp hub start` | Start a detached hub if one is not already healthy |
| `mzp hub stop` | Stop the hub for this port (via PID file) |
| `mzp hub restart` | Stop then start (clears the in-memory buffer) |
| `mzp open` | Open the web UI (hub must already be reachable) |
| `mzp mcp` | Stdio MCP server (hub at `:1738`, or `MIZPAH_URL`) |
| `mzp mcp install` | Merge MCP config into Cursor / Claude / Codex |
| `mzp mcp uninstall` | Remove those MCP entries |

## Development

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
```

Pull requests run the same Rust and web checks via GitHub Actions (`.github/workflows/ci.yml`).

### Architecture

```
stdin ──► try bind :1738
                          ├─ success → hub (Axum + ring buffer + UI + hub-{port}.pid)
                          └─ AddrInUse → attach (POST /api/ingest)

mzp attach ──► shell hooks ──► tee stdout/stderr ──► POST /api/ingest/batch
mzp hub    ──► start | stop | restart detached hub on :1738
mzp open   ──► browser → http://127.0.0.1:1738
```

## License

MIT
