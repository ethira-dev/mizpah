---
title: MCP & agents
description: Stdio MCP tools, install targets, limits, and UI investigate hooks.
order: 6
---

mizpah exposes the live hub as a stdio MCP server (`mzp mcp`). Agents get structured tools against `/api/*` instead of pasted transcripts. That is both a clearer workflow (ask for errors, get rows) and a cheaper one (default 20 hits, max 50; results in TOON instead of pretty JSON).

## Install

```bash
mzp setup           # ensure hub + mcp install (+ optional --with-skill)
# or:
mzp mcp install     # merges config into Cursor, Claude Desktop, Claude Code, Codex when present
# restart clients
mzp mcp uninstall   # remove those entries
```

`mzp mcp` auto-starts a loopback hub if none is healthy. First hub start also attempts registration.

Override hub URL with `MIZPAH_URL` (default `http://127.0.0.1:3149`).

## Agent skill

Workflow skill aimed at **token and cost savings**: pipe into Mizpah, prefer JSON logs, query MCP with small limits — for Cursor and other agents that support [Agent Skills](https://agentskills.io).

**Without the skill**, agents often paste entire log dumps into chat (tens of thousands of tokens of noise). **With the skill**, they call `search_logs` with CEL (for example `level == "error"`) and keep only the rows that matter — same diagnosis, thinner context. See the side-by-side on the [home page](../../#teach-your-agent) (Teach your agent to save tokens).

### Install the skill

```bash
brew install ethira-dev/mizpah/mizpah
mzp setup --with-skill
# restart the agent client, then pipe or: mzp run -s api -- npm test
```

List skills in the package without installing:

```bash
npx skills add ethira-dev/mizpah --list
```

Install globally (all projects) or for one agent only:

```bash
npx skills add ethira-dev/mizpah -g
npx skills add ethira-dev/mizpah -a cursor -a claude-code
```

Also available as a Cursor plugin (repo-root `.cursor-plugin/` + `skills/mizpah/`). Install from the Cursor Marketplace / Customize when listed, or symlink locally — see the repo [PLUGIN.md](https://github.com/ethira-dev/mizpah/blob/main/PLUGIN.md).

## Tools

| Tool | Parameters | Notes |
|------|------------|--------|
| `list_services` | (none) | Service names in the buffer |
| `get_stats` | (none) | Entry count, approx bytes, max bytes, per-service counts |
| `list_properties` | `service?`, `q?` | Discovered paths + sample values (for writing CEL) |
| `search_logs` | `q?` (CEL), `nl?` (natural language → CEL), `service?`, `limit?`, `cursor?` | Newest-first; **default limit 20, max 50**; `hasMore` for pagination |
| `summarize_incident` | `minutes?` (default 15) | What broke? — levels, top services/messages, sample ids, traces |
| `get_logs_around` | `id`, `before?` (default 5), `after?` (default 5), `service?`, `q?` | Window around an entry for stack/context |
| `aggregate_logs` | `group_by?`, `q?`, `service?`, `limit?` | Top-N counts (GROUP BY); default `group_by=["service"]`; **default limit 20, max 50** |
| `get_trace` | `opid`, `limit?` | All buffered rows for a trace/request id (oldest-first); hard-capped |
| `list_traces` | `limit?` | Distinct traces in the buffer (counts + time range) |
| `query_sql` | `sql`, `limit?` | Single `SELECT` / `WITH … SELECT` over snapshot `all_logs`; **max 50 rows** via MCP |
| `list_bookmarks` | (none) | Bookmarks / tags / comments on buffered entries |
| `nav_level` | `from_id`, `direction?`, `levels?` | Next/prev error or warn (hub-wide) |
| `spectrogram` | `field?`, `time_buckets?` | Time × field heat-map (default `field=level`) |

Server instructions tell the model to keep limits small and never dump the full buffer. If tools fail, run `mzp setup` or `mzp run -- …`.

Bookmarks, spectrogram, SQL, and aggregates are also available in the web UI Tools sheet and via REST/CLI.

### Tool result format (TOON)

MCP tools return **TOON** ([Token-Oriented Object Notation](https://toonformat.dev/)) instead of pretty-printed JSON. TOON keeps the same data model (objects, arrays, primitives) but uses indentation and tabular arrays so agents spend fewer tokens on structure.

- Hub REST / WebSocket APIs stay JSON; only the MCP text payload is TOON.
- Log tools omit `_mzp` (cwd/user/pid/exe) from each row — still filterable via CEL when you need it.
- `list_properties` drops redundant `sampleValues` when `values` (with counts) is present.

Example `search_logs` result:

```text
entries[1]:
  - id: 42
    receivedAt: "2026-07-17T00:00:00Z"
    service: api
    data:
      level: error
      msg: timeout
hasMore: false
```

### Example agent flow

```text
1. list_properties (optional) → learn fields
2. search_logs q='level == "error"' service='api' limit=10
3. get_logs_around id=<id> before=5 after=5
4. aggregate_logs group_by=['level'] q='service == "api"' limit=10
5. get_trace opid=<trace-id>   # or query_sql for GROUP BY analytics
```

## Investigate from the UI

Log detail → **Check with Claude** or **Check with Cursor** calls `POST /api/investigate`, which launches a local `claude` or `agent` CLI session seeded with that entry and instructions to use MCP for surrounding context.

Requirements:

- `claude` or `agent` on `PATH`
- `--project` / `MIZPAH_PROJECT` set if the hub was started outside the repo you care about

## Related

- [CEL](../cel/) for filter syntax
- [Attach](../attach/) to also ingest Cursor / Claude lifecycle events into the buffer
