---
title: SIEM → one hub
description: Push exports from common SIEM/SOC sources into a single Mizpah hub.
order: 7
---

Mizpah is a **push-based** in-memory hub: you bring log lines to it (pipe, file ingest, or HTTP). Format packs detect and normalize SIEM-shaped payloads. It is **not** a SIEM API collector and does **not** listen on syslog UDP/TCP.

For shared servers, enable [Custom auth (OIDC)](../auth/) and terminate TLS in front of the hub.

## Recipe

1. **Start one hub** — `mzp hub start` (or the first `… | mzp` bind on `:3149`).
2. **Export or forward** lines from each tool into Mizpah (file, pipe, or HTTP). Tag each source with a distinct `--service`.
3. **Explore** in the UI, CEL, SQL, or MCP across services — one buffer, many sources.

```bash
mzp hub start
# then, in other terminals / jobs:
mzp ingest ./exports/splunk-hec/*.json --service splunk-export --follow
cat ./exports/sentinel-cef.log | mzp --service sentinel
mzp ingest ./exports/crowdstrike/*.json --service crowdstrike
```

## What maps to which pack

Use pack **ids** (not filenames). Full catalog: [Log formats](../formats/) and the [Supported Formats](../../formats/) browser.

| Source shape | Pack id |
|--------------|---------|
| Splunk HEC event envelopes (`time` / `host` / `source` / `event`) | `splunk_hec_log` |
| Google Chronicle / SecOps UDM JSON | `chronicle_udm_log` |
| CrowdStrike FDR-ish JSON | `crowdstrike_log` |
| Microsoft Sentinel CommonSecurityLog-style CEF-in-syslog text | `sentinel_common_log` |
| Datadog agent / intake JSON (`ddsource` + `message`) | `datadog_log` |
| Elastic Common Schema JSON payloads | `ecs_log` |
| CEF / LEEF / OCSF / GELF interchange | `cef_log`, `leef_log`, `ocsf_log`, `gelf_log` |

Also useful: GuardDuty, Microsoft Defender, Wazuh, Falco, Suricata EVE, Sysmon, Okta / Duo / Auth0 packs (see [SIEM / SOC](../formats/#siem--soc)).

**Note:** `elasticsearch_log` / `opensearch_log` are **server process** logs, not Elastic Security event stores.

## Copy-paste examples

### Splunk HEC JSON (file follow)

```bash
mzp ingest /var/exports/hec/*.ndjson --service splunk-hec --follow
```

### Sentinel / ArcSight-style CEF over a text stream

```bash
# Lines already look like syslog+CEF; Mizpah parses — it does not bind :514
tail -F /var/log/sentinel-forwarder.log | mzp --service sentinel-cef
```

### CrowdStrike JSON glob

```bash
mzp ingest ~/Downloads/fdr-export/**/*.json --service crowdstrike
```

### Vector / Fluent Bit → HTTP ingest

Emit NDJSON (or CEF text) to the hub. When `[auth]` is enabled on a non-loopback hub, send the ingest bearer:

```bash
curl -sS -X POST "https://logs.example.com/api/ingest/batch" \
  -H "Authorization: Bearer $MIZPAH_INGEST_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"service":"vector","lines":["{\"msg\":\"hi\",\"level\":\"info\"}"]}'
```

Configure your forwarder’s HTTP sink similarly (batch ≤ 128 lines per request).

### Remote one-shot files over SSH

```bash
mzp ingest 'user@siem-host:/var/exports/udm/*.json' --service chronicle
```

Remote `--follow` is not supported. Remote SSH helpers are refused when `MIZPAH_SECURE` / `secure = true`.

## Caveats

- **No native SIEM pull** — no Splunk/Elastic/Chronicle/Sentinel/Datadog API collectors.
- **No syslog listener** — syslog is a line parser for text you already ingest.
- **Ring buffer** — process-local memory (`maxBytes` / `ttlHours`); not durable SIEM retention. Restart clears the buffer unless you set `persistDir`. Persist and update-spill are encrypted at rest automatically — see [Storage security](../storage-security/).
- **Do not expose ingest to the internet** without `ingestToken` + TLS.

See also: [Streaming & hub](../streaming/), [Storage security](../storage-security/), [File ingest](../formats/#file-ingest), [CLI](../cli/).
