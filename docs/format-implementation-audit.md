# Format / power-tools Implementation Audit

**Date:** 2026-07-19  
**Scope:** Phases 0–L from the feature adoption plan  
**Method:** Gap matrix vs acceptance criteria, REST/MCP/CLI/docs contract walk, `cargo test` + live hub scenarios, security review of SQL/SSH/spill/persist  
**Tests:** `cargo test -p mizpah --bin mizpah` → **181 passed**; `web` `npm run build` → **ok**

---

## 1. Executive summary

**Usable today for the core product story:** live JSON hub, CEL search, event-time-aware filters/activity, logfmt/syslog/access ingest, file ingest (incl. gzip), aggregates, SQL SELECT snapshot, trace lookup, bookmarks (+ update-spill), MCP `aggregate_logs` / `get_trace` / `query_sql`, basic TUI, opt-in persist of log rows.

**Not ready / thin:** web UI for most power surfaces (SQL, bookmarks, spectrogram, real trace API, aggregate UI); themes; Gantt timeline; session/highlight/filter-stack; persist of annotations; pcap/otel file-exporter converters; true file follow still coarser than ideal file-follow.

**No P0** (crash / data-loss / remote RCE) confirmed. Highest-impact issues are **P1 contract/docs drift** and **feature incompleteness** that overstates “full parity.”

---

## 2. Gap matrix (plan vs reality)

| Phase | Backend | CLI | API | MCP | Web UI | Tests | Verdict |
|-------|---------|-----|-----|-----|--------|-------|---------|
| **0** event_time + config | Yes | config defaults | via entries | via SQL | eventTime display | Yes | **partial** — config dirs scaffolded; formats/themes/scripts not loaded as plugins |
| **A** nav + level activity | Yes | — | `/api/nav/level`, activity | — (client `nav_level` unused) | local `j/k` `e/E`; stacked activity | nav yes; activity weak | **partial** — web does not call nav API |
| **B** aggregate + scripts | Yes | `query --group-by`, `script` | GET only | `aggregate_logs` (no metrics args) | dead `fetchAggregate` | Yes | **partial** — `aggregate_post` unwired |
| **C** trace | Yes | — | `/api/traces`, `/api/trace/{opid}` | `get_trace` | “Show related” = CEL only | Yes | **partial** — no Gantt; web ignores trace API |
| **D** formats + file ingest | Yes | `ingest` / `files` | via ingest | — | — | formats + path detect | **partial** — no `formats_dir` loader; follow re-reads whole file |
| **E** bookmarks / sessions | annotate + spill | — | bookmarks + tag | — | **missing** | annotate + spill | **partial** — no highlights/filter-stack/sessions UI |
| **F** SQL | snapshot SELECT | `sql` | POST `/api/sql` | `query_sql` | **missing** | Yes | **partial** |
| **G** format pack | logfmt/syslog/access/generic + format packs + bro/w3c | — | formatHint on batch | — | — | per-pack samples | **done** — 71 vendored packs (converters deferred); stable ids preserved |
| **H** spectrogram | Yes | — | GET | — | **missing** | smoke | **backend-only** |
| **I** themes/keymaps | keymap file + themes stub | TUI ensures keymap | — | — | hardcoded keys | defaults | **stub** |
| **J** TUI | minimal list | `tui` | via search_logs | — | n/a | **none** | **minimal** |
| **K** persist | NDJSON segments | config `persistDir` | — | — | — | roundtrip | **partial** — annotations not persisted; no segment rotation |
| **L** remote SSH | ssh/scp argv | `ingest` remote | — | — | — | path detect only | **partial** — secure refuse works; no follow remote; no SSH e2e test |

---

## 3. Contract issues

| Severity | Issue | Evidence |
|----------|--------|----------|
| **P1** | MCP docs omit new tools | [`site/src/content/docs/mcp.md`](../site/src/content/docs/mcp.md) lists only original 5 tools; code has `aggregate_logs`, `get_trace`, `query_sql` |
| **P1** | Dead web helpers | [`web/src/lib/api.ts`](../web/src/lib/api.ts) `fetchAggregate` / `fetchTrace` never imported |
| **P1** | Unwired handler | `aggregate_post` in [`api/routes.rs`](../crates/mizpah/src/api/routes.rs) (~470) not registered in [`api/mod.rs`](../crates/mizpah/src/api/mod.rs) |
| **P2** | Web “Show related” ≠ hub trace | [`log-detail-dialog.tsx`](../web/src/components/log-detail-dialog.tsx) applies CEL; does not call `/api/trace/{opid}` (misses buffer-wide / unloaded rows) |
| **P2** | Web `e`/`E` ≠ `/api/nav/level` | Client scans loaded `entries` only |
| **P2** | MCP thinner than store | No MCP for bookmarks, spectrogram, list_traces, nav, activity; aggregate MCP lacks sum/avg/min/max |

CLI docs ([`cli.md`](../site/src/content/docs/cli.md)) match `ingest`/`files`/`query`/`sql`/`script`/`tui`. Formats/SQL doc pages exist and largely match backend.

---

## 4. Correctness (behavioral)

Live hub on `:31993` after rebuild:

| Scenario | Result |
|----------|--------|
| eventTime from `@timestamp` | Entry `old` → `eventTime=2020-01-01T00:00:00Z` |
| Time filter `from`/`to` 2019–2021 | Returns only `old` |
| Activity level split | Current hour `{count:3, error:1, warn:1, other:1}` (2020 event outside 24h window — expected with event_time) |
| logfmt ingest | `formatId=logfmt`, msg/level parsed |
| Aggregate by level | error:2, info:1, warn:1 |
| SQL GROUP BY level | Matches aggregate counts |
| Trace `t1` | Oldest-first: `old` then `now` |
| Bookmarks | POST + GET ok |
| Nav next error | Returns wrapped `{entry:…}` (HubClient unwraps `.entry`) |
| Spectrogram `field=level` | Labels `error/info/warn` |
| SQL DELETE | HTTP 400 rejected |
| `MIZPAH_SECURE=1` remote | Refused |
| gzip ingest | `fromgz` present |

Unit/integration suite: **181 passed**.

---

## 5. Security review

| Area | Assessment | Severity |
|------|------------|----------|
| SQL sandbox | SELECT/WITH only; multi-statement blocked; coarse keyword ban | **P2** false positive: `LIKE '% drop %'` rejected; not a bypass of writes (DELETE still blocked by leading keyword) |
| SSH ingest | `Command::new("ssh").arg(host).arg("cat").arg(path)` — argv, not shell | **P2** path/`-` option edge cases; no ProxyCommand hardening; acceptable for local-dev tool |
| Spill HMAC | Body lines (entries + annotation sidecar) covered; symlink refused; size caps | OK |
| Persist | Append-only NDJSON; no auth change; disk can grow without rotation | **P2** operational risk if `persistDir` set without ops care |
| Unauthenticated hub | Pre-existing model; remote bind still warned | unchanged |

**No P0** found.

---

## 6. Test gaps (worth adding next)

1. Activity histogram with level breakdown + event_time outside window  
2. TUI smoke (or at least keymap load)  
3. API integration: `/api/aggregate`, `/api/nav/level`, `/api/bookmarks`, `/api/spectrogram`  
4. Persist does **not** restore annotations (document + test)  
5. Follow-mode duplication (append line → count grows by 1, not full re-ingest)  
6. Generic format never selected (detect 0.45 &lt; 0.5) — either raise detect or drop from builtins list  

---

## 7. Recommended fix backlog (by user impact)

1. **Docs/MCP honesty** — Document `aggregate_logs`, `get_trace`, `query_sql` in `mcp.md`; note web limitations  
2. **Wire or delete dead code** — Register `aggregate_post` or remove it; use or remove `fetchAggregate`/`fetchTrace`  
3. **Web: call real APIs** — Show related → `/api/trace`; optional `e`/`E` → `/api/nav/level` when scrolling beyond loaded page  
4. **Web power surfaces** — Bookmarks panel; SQL panel; aggregate top-N; spectrogram view (backend ready)  
5. **Follow = true tail** — Track byte offset per file; stop whole-file re-ingest  
6. **Persist annotations** — Mirror spill annotation records into persist dir  
7. **Format pack** — journald JSON, Bunyan/Pino/OTel field maps; fix generic detect threshold  
8. **Themes / shared keymaps** — Consume `keymaps.toml` in web; ship 1–2 themes  
9. **TUI** — Tests + nav/trace shortcuts via hub APIs  
10. **SQL validator** — Tokenize / allowlist instead of substring bans  

---

## 8. Readiness labels

| Surface | Label |
|---------|--------|
| Pipe NDJSON + CEL + MCP search | **Production-usable** |
| event_time / activity levels / file+logfmt ingest | **Production-usable** (with known follow caveat) |
| Aggregate / SQL / trace APIs | **Production-usable** for CLI/agents |
| Bookmarks | **API-ready**; UI missing |
| Spectrogram / TUI / themes / format zoo | **Demo / early** |
| “Full parity” marketing | **Do not claim yet** |
