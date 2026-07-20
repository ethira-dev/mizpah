---
name: mizpah
description: >-
  Cut agent token and cost when debugging logs: pipe commands into Mizpah and
  query the live JSON hub over MCP instead of pasting stdout or re-running
  tests. Use when the user mentions token savings, cost, context size, mizpah,
  mzp, CEL filters, attaching shell/browser/agent log sources, PR CI, GitHub
  Actions, gh run, or failed checks.
---

# Mizpah

Local JSON log hub: pipe processes into `mzp`, browse `:3149`, query the same buffer via MCP. Prefer hub queries over pasting dumps or re-running tests/lint for every log question.

## Capture

```bash
mzp setup                 # ensure hub + MCP (once per machine)
my-app 2>&1 | mzp --service <name>
# or: mzp run -s <name> -- <cmd…>
# or: mzp attach / mzp attach browser --launch / mzp attach cursor
```

- Tag streams with `--service` / `MIZPAH_SERVICE` (default: `OTEL_SERVICE_NAME` / `SERVICE_NAME`, else project manifests such as package.json, Cargo.toml, pyproject.toml, go.mod, … / git / dir).
- Hub: `http://127.0.0.1:3149` (`MIZPAH_URL` to override). MCP auto-starts a loopback hub; if tools still fail, run `mzp setup`.

## Prefer JSON output

Whenever a tool or runtime can emit structured logs, enable it so Mizpah parses fields for CEL and property discovery:

- Flags: `--json`, `--log-format=json`, `-o json`
- Env: `LOG_FORMAT=json`, `RUST_LOG_FORMAT=json`, framework JSON loggers

Prefer **NDJSON** (one JSON object per line). Plain text is promoted to `msg` / `_raw` (and often `level`); still prefer JSON when available.

## Query (MCP) — keep context small

Tools: `list_services`, `get_stats`, `list_properties`, `search_logs` (CEL `q` or natural-language `nl`), `get_logs_around`, `aggregate_logs`, `get_trace`, `list_traces`, `query_sql`, `list_bookmarks`, `nav_level`, `spectrogram`, `summarize_incident`.

Results are **TOON** (not pretty JSON) for fewer tokens; log rows omit `_mzp`. See `references/cel-and-mcp.md`.

1. Optional: `summarize_incident` for a quick “what broke?” brief
2. `list_properties` → field paths / samples
3. `search_logs` with CEL or `nl`, `limit` ≤ 20 (max 50); paginate with `cursor` / `hasMore`
4. `get_logs_around` for stack/context around an `id`

Never dump the full buffer into chat. Do not paste large stdout when MCP can answer.

```text
summarize_incident minutes=15
search_logs q='level == "error"' service='api' limit=10
search_logs nl='errors from api last 15 minutes' limit=10
get_logs_around id=<id> before=5 after=5
```

CEL examples and tool notes: [references/cel-and-mcp.md](references/cel-and-mcp.md).

## GitHub Actions / PR CI

Triage failed PR checks without stuffing `gh` logs into chat. Metadata via `gh` JSON; full logs via pipe → Mizpah → MCP.

1. **Metadata only (into chat):** find the failed run with small JSON — e.g. `gh pr checks`, or `gh run list --branch <branch> --status failure -L 5 --json databaseId,name,conclusion,url`, then `gh run view <id> --json jobs,conclusion` (not `--log`).
2. **Ensure hub first:** `mzp setup` or `mzp hub start` (idempotent if already up). Use `--no-open` on ingest pipes.
3. **Pipe failed logs (not into chat):**

```bash
mzp hub start
gh run view <run-id> --log-failed 2>&1 | mzp --service gha --no-open
```

Prefer `--log-failed` over `--log`. Add `--job <job-id>` when only one job failed. Tag as `gha` or `gha-<pr>`. Escalate to full `--log` only if failed-step logs are insufficient — still piped to `mzp`.

4. **Query MCP (small limits):** `list_services` / `get_stats` → `summarize_incident` or `search_logs` with `service='gha'`, `q='level == "error" || msg.contains("error") || _raw.contains("error")'`, `limit` ≤ 20 → `get_logs_around` on hits.
5. **Hard rules:** never run `gh … --log` / `--log-failed` without piping to `mzp`; never paste full GHA logs into chat; once ingested, do not re-fetch the whole run — paginate with `cursor` / `hasMore`.

## Setup

```bash
mzp setup --with-skill   # hub + MCP + npx skills add ethira-dev/mizpah
mzp doctor               # readiness checks
```

`mizpah` is an alias for `mzp`. Docs: https://ethira-dev.github.io/mizpah/docs/mcp/
