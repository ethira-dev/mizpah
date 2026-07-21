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
| `heroku_router_log` | Heroku HTTP router (`heroku[router]: at=…`) — specialized (beats syslog/logfmt) |
| `f5_log` / `consul_log` / `nomad_log` | F5 BIG-IP / Consul / Nomad agent lines — specialized |

## Format-v1 packs

Mizpah vendors built-in format-v1 packs (see `crates/mizpah/formats/packs/PIN.txt` for the original pin, plus first-party additions) and loads them at startup via a PCRE2 engine. JSON packs may declare `match-keys` so detection requires distinctive fields (collision-safe). Binary captures are converted during file ingest, then classified by post-convert packs. The live count and full definitions are in the **Supported Formats** browser (and under `formats/packs/` in the repo).

**Examples** of pack ids: `nestjs_log`, `postgres_log`, `aws_eventbridge_log`, `cloudevents_log`, `ecs_log`, `cloudtrail_log`, `cef_log`, `suricata_eve_log`, `sysmon_log`, `cloudflare_firewall_log`, `wiz_issue_log`, …

When a pack overlaps a stable Mizpah id (`syslog_log` → `syslog`, `bunyan_log` → `bunyan`, …), ingest still emits the **stable** id. The upstream name is stored additively as `_pack_format` when the pack engine produced the row.

### Cloud

| Pack | Notes |
|------|--------|
| `cloudtrail_log`, `vpc_flow_log`, `cloudfront_log`, `nlb_log`, `waf_log`, `route53_query_log`, `lambda_cloudwatch_log` | AWS |
| `guardduty_log` | AWS GuardDuty findings |
| `alb_log`, `elb_log`, `s3_log` | AWS (longer-standing) |
| `gcp_cloud_logging_log`, `gcp_load_balancer_log`, `gcp_vpc_flow_log` | GCP |
| `chronicle_udm_log` | Google Chronicle / SecOps UDM |
| `azure_activity_log`, `azure_nsg_flow_log`, `azure_app_insights_log`, `azure_front_door_log` | Azure |

### Cloudflare

| Pack | Notes |
|------|--------|
| `cloudflare_json_log` | HTTP request / CDN access (existing) |
| `cloudflare_firewall_log`, `cloudflare_dns_log`, `cloudflare_spectrum_log`, `cloudflare_workers_trace_log` | Edge / compute Logpush |
| `cloudflare_audit_log`, `cloudflare_access_log`, `cloudflare_gateway_http_log`, `cloudflare_gateway_dns_log`, `cloudflare_gateway_network_log` | Account + Zero Trust |
| `cloudflare_magic_ids_log`, `cloudflare_page_shield_log`, `cloudflare_nel_log`, `cloudflare_email_security_log`, `cloudflare_casb_log`, `cloudflare_device_posture_log`, `cloudflare_zt_session_log` | Security / posture |

### SIEM / SOC

Recipe for pushing several of these into one hub: [SIEM → one hub](../siem-ingest/).

| Pack | Notes |
|------|--------|
| `cef_log`, `leef_log`, `ocsf_log`, `gelf_log`, `splunk_hec_log`, `sentinel_common_log` | Interchange |
| `suricata_eve_log`, `snort_log`, `falco_log`, `wazuh_log`, `crowdstrike_log`, `okta_system_log` | IDS / EDR / identity |
| `sysmon_log`, `windows_security_log`, `powershell_scriptblock_log`, `microsoft_defender_log`, `osquery_log`, `carbon_black_log`, `sentinelone_log`, `sophos_log` | Endpoint |
| `windows_evtx_log` | Windows `.evtx` via file-ingest converter |
| `duo_log`, `auth0_log`, `pingfederate_log`, `keycloak_log`, `vault_audit_log` | MFA / IdP / secrets |
| `zscaler_log`, `netskope_log`, `modsecurity_log`, `openvpn_log`, `wireguard_log`, `iptables_log`, `nftables_log` | Network / SWG / VPN |
| `akamai_log`, `fastly_log` | CDN / WAF JSON |
| `pcap_log`, `netflow_log` | pcap (tshark) / NetFlow·IPFIX (nfdump) via file ingest |
| `fail2ban_log`, `sshd_log`, `sudo_log`, `selinux_log`, `apparmor_log`, `guacamole_log`, `auditd_log` | Host auth / MAC |
| `dhcp_log`, `bind_dns_log`, `unbound_log`, `radius_log` | Infra attribution |
| `qualys_log`, `nessus_log` | Vulnerability scanners |
| `paloalto_log`, `cisco_asa_log`, `fortinet_log`, `checkpoint_log` | Network appliances |
| `ecs_log`, `bro_log`, `kubernetes_audit_log` | Existing SOC-adjacent |

### Database

| Pack | Notes |
|------|--------|
| `postgres_log`, `vpostgres_log` | PostgreSQL (incl. duration / statement lines) |
| `pgaudit_log` | pgAudit `AUDIT:` session/object lines |
| `mysql_error_log`, `mysql_gen_log`, `mysql_slow_log`, `mysql_audit_log` | MySQL / MariaDB error, general, slow (per-line stats), audit |
| `mssql_error_log` | Microsoft SQL Server ERRORLOG |
| `oracle_alert_log` | Oracle alert / `ORA-` lines |
| `mongodb_json_log`, `mongodb_audit_log` | MongoDB 4.4+ server JSON + audit JSON |
| `redis_log`, `redis_slowlog` | Redis process logs + SLOWLOG entries |
| `cassandra_log` | Cassandra / ScyllaDB system.log |
| `cockroachdb_log`, `cockroachdb_json_log` | CockroachDB crdb text + JSON sinks |
| `clickhouse_log` | ClickHouse server |
| `elasticsearch_log`, `elasticsearch_slow_log`, `opensearch_log` | ES / OpenSearch server + slowlog |

### Queues / events

| Pack | Notes |
|------|--------|
| `aws_sqs_log`, `aws_sqs_camel_log`, `aws_sns_log`, `aws_eventbridge_log`, `aws_kinesis_log`, `aws_step_functions_log` | AWS message / event envelopes |
| `cloudevents_log` | CloudEvents 1.0 JSON |
| `gcp_pubsub_log` | GCP Pub/Sub push envelope |
| `azure_event_grid_log`, `azure_service_bus_log`, `azure_event_hubs_log` | Azure messaging schemas |
| `kafka_log`, `rabbitmq_log`, `rabbitmq_json_log`, `nats_log`, `pulsar_log`, `activemq_log` | Broker process / JSON logs |
| `celery_log`, `sidekiq_log`, `bullmq_log`, `bullmq_json_log` | Job workers |
| `temporal_log` | Temporal / Cadence history events |
| `prisma_log`, `zookeeper_log` | ORM query lines / coordination (adjacent) |

### App frameworks

| Pack | Notes |
|------|--------|
| `nestjs_log` | Default NestJS `ConsoleLogger` (`[Nest] pid - timestamp LEVEL [Context] …`) |
| `winston_log`, `pino_log`, `bunyan_log`, `morgan_log` | Node loggers / Express morgan `dev`+`tiny` |
| `rails_log`, `laravel_log`, `monolog_log` | Ruby / PHP |
| `slog_text_log`, `slog_json_log`, `zerolog_log`, `zerolog_json_log`, `logrus_log`, `logrus_json_log`, `zap_console_log` | Go |
| `python_logging_log`, `uvicorn_log`, `gunicorn_log`, `werkzeug_log`, `structlog_log`, `structlog_json_log`, `loguru_log`, `django_log` | Python |
| `java_log` | Java / Log4j text (includes Spring Boot `--- [thread]` console) |
| `env_logger_log`, `rust_tracing_log`, `simple_rs_log` | Rust |
| `dotnet_console_log`, `serilog_text_log`, `serilog_compact_log`, `nlog_log` | .NET |
| `celery_log`, `sidekiq_log`, `compose_log`, `airflow_log`, `temporal_log` | Workers / orchestration |
| `jenkins_log`, `gitlab_ci_log`, `github_actions_log`, `terraform_log`, `ansible_log` | CI / IaC |
| `etcd_log`, `datadog_log`, `otel_collector_log` | Infra / agents / OTel Collector file exporter (JSON) |
| `nextjs_log`, `vite_log`, `prisma_log`, `elixir_logger_log`, `deno_log`, `bun_log` | Frontend toolchains / niche runtimes |

Pretty Nest / `util.inspect` object dumps are still reassembled separately (not this pack). JSON field packs also recognize stable ids `slog`, `zerolog`, `logrus`, and `structlog`.

### Wiz

| Pack | Notes |
|------|--------|
| `wiz_audit_log`, `wiz_issue_log`, `wiz_vulnerability_log`, `wiz_configuration_finding_log`, `wiz_detection_log`, `wiz_inventory_log` | Flattened GraphQL / webhook JSON nodes |

### Detection order (pipes / stdin)

1. JSON object → Mizpah JSON packs, then vendored JSON packs (honoring `match-keys`), else `json`
2. Specialized: `heroku_router_log`, `f5_log`, `consul_log`, `nomad_log`, then `logfmt`, `syslog`, `access_log`, `bro_log`, `w3c_log`
3. Other vendored text packs (specificity-ordered)
4. `generic`
5. `raw`

File ingest (`mzp ingest` / `mzp files`) may lock a format after sampling the first lines and pass a `formatHint` to the hub. Stdin/pipe stays per-line so mixed streams keep working.

Binary file ingest converts before classify:

| Input | Tool / crate | Pack id |
|-------|----------------|---------|
| `.evtx` | Rust `evtx` | `windows_evtx_log` |
| `.pcap` / `.pcapng` | `tshark` on `PATH` | `pcap_log` |
| `nfcapd.*` | `nfdump` on `PATH` | `netflow_log` |

`--follow` is refused for those binary paths (ingest completed files only).

### Still deferred

- **sFlow** (not covered by nfdump path)
- **OTel Collector file exporter `format: proto`** (length-prefixed binary; JSON lines are supported as `otel_collector_log`)

## File ingest

```bash
mzp ingest ./app.log.gz --service api
mzp ingest ./Security.evtx --service windows
mzp files './logs/*.log' --follow
mzp ingest user@host:/var/log/app.log   # SSH; refused when MIZPAH_SECURE=1
```

Gzip and bzip2 are decompressed on the fly (including before pcap/EVTX conversion when named `*.gz`). Remote paths stream into the **local** hub.

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
