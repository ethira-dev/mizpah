---
title: Quick start
description: Pipe a service or attach a source and open the live UI in under a second.
order: 1
---

**Pipe a service:**

```bash
api-server 2>&1 | mzp --service api
# UI opens at http://127.0.0.1:1738. More streams join the same hub automatically.
worker | mzp --service worker
```

**Or attach a source:**

```bash
mzp attach              # shell (default): enable + ensure hub
mzp attach browser --launch
mzp attach cursor       # Cursor agent hooks → hub
mzp attach claude       # Claude Code hooks → hub
mzp open
mzp detach              # shell only
mzp detach cursor       # or: claude | all
```

(`mizpah` is the same binary if you prefer the long name.)

## What you get

- **Live JSON UI, zero SaaS.** Local hub on `:1738`, multi-service, virtualized, pause/resume. No account. No Docker compose novel.
- **Attach anything.** `mzp attach shell|browser|cursor|claude` pipes terminal output, Chromium DevTools, or agent lifecycle hooks into the same hub.
- **Filter like you mean it.** [CEL](https://cel.dev/) in the search bar, with autocomplete for every property you've actually logged.
- **Agents that can see.** MCP tools so Cursor / Claude / Codex search the live buffer instead of eating a 10k-line dump.
- **One-click investigate.** Open a log → **Check with Claude** or **Check with Cursor** and drop into a local agent session already seeded with that entry.

Next: [Install](../install/) · [Attach sources](../attach/) · [CEL filters](../cel/)
