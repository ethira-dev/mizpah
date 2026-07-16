---
title: Install
description: Homebrew, build from source, or grab a prebuilt binary from GitHub Releases.
order: 2
---

## Homebrew

```bash
brew install ethira-dev/mizpah/mizpah
mzp --help
```

## From source

Requirements: [Rust](https://rustup.rs/) (stable) and Node.js 20+.

```bash
# Puts `mzp` and `mizpah` on PATH (~/.cargo/bin)
just install

# Without just:
cd web && npm ci && npm run build
cargo install --path crates/mizpah --force
mzp mcp install
```

`just install` (and the first hub start) register mizpah as an MCP server in Cursor, Claude Desktop, Claude Code, and Codex when those apps are present. Restart the client after install so tools appear.

```bash
echo '{"msg":"hi"}' | mzp
```

If you get `command not found`, put Cargo’s bin dir on `PATH`:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

If you also have a Homebrew install, ensure `~/.cargo/bin` is before `/opt/homebrew/bin`, or run `~/.cargo/bin/mzp mcp install`. An older brew binary will not understand `mcp` until the tap is updated.

## Prebuilt binaries (GitHub Releases)

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
