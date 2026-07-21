---
title: Attach sources
description: Shell tee, Chromium CDP, and observe-only Cursor / Claude hooks into one hub.
order: 4
---

`mzp attach <target>` feeds sources into the same ring buffer as stdin pipes. Capture once; browse in the UI and let agents search without re-running the underlying work.

Bare `mzp attach` is **shell** (backward compatible).

| Target | Mechanism | Default service |
|--------|-----------|-----------------|
| `shell` | Shell rc hooks tee stdout/stderr from **new** interactive zsh/bash | absolute cwd / `--service` |
| `browser` | Foreground CDP session (console, exceptions, network) | page `location.host` |
| `cursor` | Merge observe-only hooks into `~/.cursor/hooks.json` | `cursor` |
| `claude` | Merge observe-only hooks into `~/.claude/settings.json` | `claude` / `--service` |

```bash
mzp attach
mzp attach shell --service my-project
mzp attach browser --launch
mzp attach cursor
mzp attach claude --service agents
mzp detach                 # shell
mzp detach cursor          # or: claude | all
mzp hub start|stop|restart
```

## Shell

Installs markers in `${ZDOTDIR:-$HOME}/.zshrc`, `~/.bashrc`, and a bash login file (`# >>> mizpah >>>` … `# <<< mizpah <<<`). Starts a background hub if needed. New interactive shells tee stdout/stderr to `/api/ingest/batch`.

Enriched fields:

- `cmd`: full command string from shell hooks (not child argv)
- `_mzp.cwd`: updates after `cd`
- `_mzp.user` / `pid` / `exe`: receiver process

<details>
<summary>Constraints</summary>

- Captures inherited stdout/stderr only (not `/dev/tty` TUIs, not typed input)
- Children see pipes instead of a TTY (colors, buffering, interactivity may change)
- If the hub is down, capture is best-effort; the shell stays responsive
- Shells already open at first attach need a new tab/window
- `mzp detach` leaves hooks installed but makes `__shell-init` a no-op; delete the marker blocks to remove entirely

</details>

## Browser

`mzp attach browser` (alias `mzp browser attach`) connects to Chromium DevTools Protocol and forwards:

- `console.*` and uncaught exceptions (`kind: "console"`)
- Network (Document / XHR / Fetch / WebSocket by default; bodies for Document/XHR/Fetch, truncated at 256 KiB)

```bash
mzp attach browser --launch          # separate Mizpah Chrome profile
mzp browser attach --cdp-port 9222   # existing debug port
```

Example CEL:

```cel
source == "browser" && kind == "console" && level == "error"
kind == "network" && status >= 400
service == "localhost:5173"
```

<details>
<summary>Constraints</summary>

- Needs a debugging port, or `--launch` (not your default profile cookies/extensions)
- Cannot inject into an already-running normal Chrome without restarting with `--remote-debugging-port`
- `--all-network` includes static asset metadata
- Bodies/headers may contain secrets; hub is local but treat the stream as sensitive ([storage security](../storage-security/))
- Foreground; Ctrl-C stops forwarding (launched browser stays open). No `detach browser`.

</details>

## Cursor / Claude hooks

Observe-only lifecycle hooks. Always exit 0 with empty stdout (never block or mutate the agent). Events are JSON with `source`, `kind`, `level`, `msg`, plus original hook fields. Strings larger than 64 KiB are truncated.

```bash
mzp attach cursor
mzp attach claude
# use the agent…
mzp detach cursor
mzp detach claude
```

```cel
source == "cursor" && kind == "afterShellExecution"
source == "claude" && kind == "PreToolUse"
source == "claude" && level == "error"
```

<details>
<summary>Constraints</summary>

- Prompts, file contents, shell output, and thoughts may be ingested
- Cursor cloud agents ignore `~/.cursor/hooks.json` (project `.cursor/hooks.json` only)
- Re-run attach after moving the `mzp` binary so absolute hook paths stay valid
- Intentionally skipped: Cursor Tab hooks; Claude `MessageDisplay`, `WorktreeCreate`, `FileChanged`

</details>
