# Mizpah Cursor plugin

Agent skill + MCP entry for the [Mizpah](https://ethira-dev.github.io/mizpah/) local JSON log hub.

## Install

**skills.sh / any agent**

```bash
npx skills add ethira-dev/mizpah
```

**Cursor Marketplace** — after the public repo includes this layout, submit at [cursor.com/marketplace/publish](https://cursor.com/marketplace/publish). Once approved, install from Customize / Marketplace search (“Mizpah”).

**Local (development)**

```bash
ln -sfn "$(pwd)" ~/.cursor/plugins/local/mizpah
# then: Developer: Reload Window
```

**skills.sh discovery** — listing grows from installs of `npx skills add ethira-dev/mizpah` (no separate publish form).

**MCP (all clients)** — still preferred for Claude Desktop / Codex / CLI:

```bash
mzp mcp install
```

Requires `mzp` on `PATH` and a running hub (`my-app 2>&1 | mzp --service <name>`). Plugin `mcp.json` matches that install shape (`command`: `mzp`, `args`: `["mcp"]`).

## Contents

| Path | Role |
|------|------|
| `skills/mizpah/` | Agent Skills workflow (pipe, JSON logs, MCP queries) |
| `mcp.json` | Stdio MCP: `mzp mcp` (same shape as `mzp mcp install`) |
| `assets/logo.svg` | Ethira mark |

Logo: [Ethira brand](https://www.ethira.dev/brand).
