---
title: Install
description: Homebrew, Cargo from source, or prebuilt release archives.
order: 2
---

## Homebrew

```bash
brew install ethira-dev/mizpah/mizpah
mzp --help
```

After install, start a hub once (or run `mzp mcp install`) so Cursor / Claude Desktop / Claude Code / Codex pick up the MCP server. Restart those clients afterward.

## From source

Requirements: [Rust](https://rustup.rs/) (stable) and Node.js 20+.

```bash
just install
# Builds web/ → crates/mizpah/static, cargo install --path crates/mizpah --force,
# then mzp mcp install when clients are present.
```

Without just:

```bash
cd web && npm ci && npm run build
cargo install --path crates/mizpah --force
mzp mcp install
```

Smoke test:

```bash
echo '{"msg":"hi","level":"info"}' | mzp --no-open
curl -sS "http://127.0.0.1:1738/api/stats"
```

### PATH notes

Cargo installs to `~/.cargo/bin`. If `mzp` is not found:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

If Homebrew and Cargo both provide `mzp`, put `~/.cargo/bin` first, or invoke `~/.cargo/bin/mzp` explicitly. Older brew builds may not include `mcp` until the tap is updated.

## Prebuilt binaries

Archives from [Releases](https://github.com/ethira-dev/mizpah/releases) contain `mizpah` and `mzp` (same binary, two names):

| Platform | Archive |
|----------|---------|
| macOS Apple Silicon | `mizpah-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `mizpah-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 | `mizpah-x86_64-unknown-linux-gnu.tar.gz` |

```bash
curl -L https://github.com/ethira-dev/mizpah/releases/latest/download/mizpah-aarch64-apple-darwin.tar.gz \
  | tar -xz
mv mizpah mzp ~/.local/bin/
mzp mcp install
```
