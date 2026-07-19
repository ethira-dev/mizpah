---
title: Log formats
description: JSON, logfmt, syslog, access logs, format packs, SIEM formats, and custom format detection.
order: 6
---

Mizpah prefers NDJSON, but non-JSON lines are detected and normalized into structured `data` plus a `formatId`.

Browse every vendored pack on the [Supported Formats](../../formats/) page: search by id or title, then inspect the pack JSON definition. Packs are synced from `crates/mizpah/formats/packs/` at site build time.

## Built-ins (stable Mizpah IDs)

| Format | Notes |
|--------|--------|
| `json` | One JSON object per line (when no pack matches) |
| `bunyan` / `pino` / `otel` / `journald` / `slog` / `zerolog` / `logrus` / `structlog` | Mizpah JSON field packs → normalized `level` / `msg` / `@timestamp` |
| `logfmt` | `level=error msg="…"` key/value pairs |
| `syslog` | Syslog / RFC5424-ish lines (hand parser; also backed by `syslog_log` pack samples) |
| `access_log` | Combined / common HTTP access logs |
| `generic` | Level-token plaintext (`ERROR …`) |
| `raw` | Fallback `{ "_raw": "…" }` |
| `bro_log` | Bro / Zeek TSV (header-driven `#fields`) |
| `w3c_log` | W3C Extended Log File Format (`#Fields:`) |

## Format-v1 packs

Mizpah vendors built-in format-v1 packs (see `crates/mizpah/formats/packs/PIN.txt` for the original pin, plus first-party additions) and loads them at startup via a PCRE2 engine. Converter-only packs are omitted until a converter exists. The live count and full definitions are in the **Supported Formats** browser (and under `formats/packs/` in the repo).

**Examples** of pack ids: `nestjs_log`, `postgres_log`, `ecs_log`, `cloudtrail_log`, `cef_log`, `suricata_eve_log`, `sysmon_log`, `cloudflare_firewall_log`, `wiz_issue_log`, …

When a pack overlaps a stable Mizpah id (`syslog_log` → `syslog`, `bunyan_log` → `bunyan`, …), ingest still emits the **stable** id. The upstream name is stored additively as `_pack_format` when the pack engine produced the row.

### Cloud

| Pack | Notes |
|------|--------|
| `cloudtrail_log`, `vpc_flow_log`, `cloudfront_log`, `nlb_log`, `waf_log`, `route53_query_log`, `lambda_cloudwatch_log` | AWS |
| `alb_log`, `elb_log`, `s3_log` | AWS (longer-standing) |
| `gcp_cloud_logging_log`, `gcp_load_balancer_log`, `gcp_vpc_flow_log` | GCP |
| `azure_activity_log`, `azure_nsg_flow_log`, `azure_app_insights_log`, `azure_front_door_log` | Azure |

### Cloudflare

| Pack | Notes |
|------|--------|
| `cloudflare_json_log` | HTTP request / CDN access (existing) |
| `cloudflare_firewall_log`, `cloudflare_dns_log`, `cloudflare_spectrum_log`, `cloudflare_workers_trace_log` | Edge / compute Logpush |
| `cloudflare_audit_log`, `cloudflare_access_log`, `cloudflare_gateway_http_log`, `cloudflare_gateway_dns_log`, `cloudflare_gateway_network_log` | Account + Zero Trust |
| `cloudflare_magic_ids_log`, `cloudflare_page_shield_log`, `cloudflare_nel_log`, `cloudflare_email_security_log`, `cloudflare_casb_log`, `cloudflare_device_posture_log`, `cloudflare_zt_session_log` | Security / posture |

### SIEM / SOC

| Pack | Notes |
|------|--------|
| `cef_log`, `leef_log`, `ocsf_log`, `gelf_log`, `splunk_hec_log`, `sentinel_common_log` | Interchange |
| `suricata_eve_log`, `snort_log`, `falco_log`, `wazuh_log`, `crowdstrike_log`, `okta_system_log` | IDS / EDR / identity |
| `sysmon_log`, `windows_security_log`, `microsoft_defender_log`, `osquery_log`, `carbon_black_log`, `sentinelone_log`, `sophos_log` | Endpoint |
| `duo_log`, `auth0_log`, `pingfederate_log`, `keycloak_log` | MFA / IdP |
| `zscaler_log`, `netskope_log`, `modsecurity_log`, `openvpn_log`, `wireguard_log`, `iptables_log`, `nftables_log` | Network / SWG / VPN |
| `fail2ban_log`, `sshd_log`, `sudo_log`, `selinux_log`, `apparmor_log`, `guacamole_log`, `auditd_log` | Host auth / MAC |
| `dhcp_log`, `bind_dns_log`, `unbound_log`, `radius_log` | Infra attribution |
| `qualys_log`, `nessus_log` | Vulnerability scanners |
| `paloalto_log`, `cisco_asa_log`, `fortinet_log`, `checkpoint_log` | Network appliances |
| `ecs_log`, `bro_log`, `kubernetes_audit_log` | Existing SOC-adjacent |

### App frameworks

| Pack | Notes |
|------|--------|
| `nestjs_log` | Default NestJS `ConsoleLogger` (`[Nest] pid - timestamp LEVEL [Context] …`) |
| `winston_log`, `pino_log`, `bunyan_log`, `morgan_log` | Node loggers / Express morgan `dev`+`tiny` |
| `rails_log`, `laravel_log`, `monolog_log` | Ruby / PHP |
| `slog_text_log`, `slog_json_log`, `zerolog_log`, `zerolog_json_log`, `logrus_log`, `logrus_json_log`, `zap_console_log` | Go |
| `python_logging_log`, `uvicorn_log`, `gunicorn_log`, `werkzeug_log`, `structlog_log`, `structlog_json_log`, `loguru_log` | Python |
| `env_logger_log`, `rust_tracing_log`, `simple_rs_log` | Rust |
| `dotnet_console_log`, `serilog_text_log`, `serilog_compact_log`, `nlog_log` | .NET |
| `celery_log`, `sidekiq_log`, `compose_log` | Workers / Docker Compose prefixes |
| `nextjs_log`, `vite_log`, `prisma_log`, `elixir_logger_log`, `deno_log`, `bun_log` | Frontend toolchains / niche runtimes |

Pretty Nest / `util.inspect` object dumps are still reassembled separately (not this pack). JSON field packs also recognize stable ids `slog`, `zerolog`, `logrus`, and `structlog`.

### Wiz

| Pack | Notes |
|------|--------|
| `wiz_audit_log`, `wiz_issue_log`, `wiz_vulnerability_log`, `wiz_configuration_finding_log`, `wiz_detection_log`, `wiz_inventory_log` | Flattened GraphQL / webhook JSON nodes |

### Detection order (pipes / stdin)

1. JSON object → Mizpah JSON packs, then vendored JSON packs, else `json`
2. Specialized: `logfmt`, `syslog`, `access_log`, `bro_log`, `w3c_log`
3. Other vendored text packs (specificity-ordered)
4. `generic`
5. `raw`

File ingest (`mzp ingest` / `mzp files`) may lock a format after sampling the first lines and pass a `formatHint` to the hub. Stdin/pipe stays per-line so mixed streams keep working.

### Deferred (not registered)

Converter-only packs are **not** loaded until a converter exists:

- `pcap_log` (needs tshark converter)
- `otel_collector_log` (file-exporter converter)
- Windows Event Log / EVTX and NetFlow/IPFIX (binary converters)

## File ingest

```bash
mzp ingest ./app.log.gz --service api
mzp files './logs/*.log' --follow
mzp ingest user@host:/var/log/app.log   # SSH; refused when MIZPAH_SECURE=1
```

Gzip and bzip2 are decompressed on the fly. Remote paths stream into the **local** hub.

## Custom formats

Place TOML field packs under the Mizpah config dir `formats/` (see `MIZPAH_CONFIG_DIR`), e.g.:

```toml
id = "myapp"
matchKeys = ["event", "severity"]
levelField = "severity"
msgField = "event"
timeField = "ts"
```

Built-in Mizpah packs (bunyan/pino/otel/journald) ship in-process; user TOML packs are loaded at parse time. Vendored JSON packs live under the binary’s embedded `formats/packs/` tree.
