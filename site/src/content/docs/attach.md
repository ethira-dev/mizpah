---
title: Attach sources
description: Capture shell, browser DevTools, Cursor, and Claude into the same hub.
order: 4
---

`mzp attach` takes a target. Bare `mzp attach` is **shell** (backward compatible).

| Target | What it does |
|--------|----------------|
| `shell` (default) | Tee stdout/stderr from **new** interactive zsh/bash shells |
| `browser` | Chrome/Edge DevTools: console + network (foreground CDP session) |
| `cursor` | Install observe-only Cursor agent hooks → hub |
| `claude` | Install observe-only Claude Code hooks → hub |

```bash
mzp attach                         # shell
mzp attach shell --service my-project
mzp attach browser --launch
mzp attach cursor                  # service default: cursor
mzp attach claude --service agents
mzp detach                         # shell
mzp detach cursor                  # or: claude | all
mzp hub stop                       # also: start / restart
```

## Shell

Installs shell hooks, starts a background hub if needed, and tees stdout/stderr from **new** interactive shells. Every line gets the command’s absolute cwd (updates after `cd` via `_mzp.cwd`), a `cmd` property with the full command string, and `_mzp` for the shell-forward receiver.

<details>
<summary>Know before you attach shell</summary>

- Captures stdout/stderr that inherit the shell redirect (not typed input, and not TUI apps that write only to `/dev/tty`)
- `cmd` / per-command cwd come from shell hooks, not child process argv
- Programs see pipes instead of a TTY, so colors, buffering, and interactivity may change
- Capture is best-effort if the hub is down; your terminal stays responsive
- Shells already open when you first attach need a new window/tab

Hooks live in `${ZDOTDIR:-$HOME}/.zshrc`, `~/.bashrc`, and a bash login file. Remove the `# >>> mizpah >>>` … `# <<< mizpah <<<` blocks anytime, or leave them; `detach` makes `__shell-init` a no-op.

</details>

## Browser

`mzp attach browser` (alias: `mzp browser attach`) connects to Chromium via CDP and forwards `console.*`, uncaught exceptions, and network calls (with request/response bodies for Document/XHR/Fetch) into the hub as JSON.

Each event’s hub **service** is the page’s `location.host` (e.g. `localhost:5173`). Optional `--service` overrides that for the whole session. There is no `detach browser`; stop with Ctrl-C.

```bash
mzp attach browser --launch
# Or: mzp browser attach --cdp-port 9222
```

```
source == "browser" && kind == "console" && level == "error"
kind == "network" && status >= 400
service == "localhost:5173"
```

<details>
<summary>Know before you attach browser</summary>

- Requires Chrome/Edge with a DevTools debugging port, or `--launch` (opens a **separate** mizpah profile, not your default cookies/extensions)
- You cannot inject debugging into an already-running normal Chrome; use `--launch` or restart Chrome with `--remote-debugging-port`
- Default network ingest: Document, XHR, Fetch, WebSocket. Bodies for Document/XHR/Fetch (truncated at 256 KiB). Use `--all-network` for static asset metadata
- Bodies and headers may contain secrets. The hub is local only, but still treat the stream as sensitive
- Foreground process; Ctrl-C stops forwarding (launched browser stays open)

</details>

## Cursor / Claude agent hooks

`mzp attach cursor` and `mzp attach claude` merge **observe-only** lifecycle hooks into user-global config (`~/.cursor/hooks.json`, `~/.claude/settings.json`). Each event is forwarded as JSON with `source`, `kind`, `level`, and `msg` (plus the original hook fields). String values larger than 64 KiB are truncated.

```bash
mzp attach cursor
mzp attach claude
mzp open
# …use Cursor Agent or Claude Code…
mzp detach cursor
mzp detach claude
```

```
source == "cursor" && kind == "afterShellExecution"
source == "claude" && kind == "PreToolUse"
source == "claude" && level == "error"
```

<details>
<summary>Know before you attach cursor / claude</summary>

- Hooks never block or modify agent actions (always exit 0, empty stdout)
- Prompts, file contents, shell output, and thoughts may be ingested; treat the hub as sensitive
- Cursor cloud agents ignore `~/.cursor/hooks.json` (project `.cursor/hooks.json` only)
- Re-run attach after moving the `mzp` binary so absolute hook command paths stay valid
- Skipped on purpose: Cursor Tab hooks; Claude `MessageDisplay`, `WorktreeCreate`, `FileChanged`

</details>
