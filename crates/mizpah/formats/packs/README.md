# Vendored format packs

JSON format-v1 definitions embedded at build time. Original upstream packs are licensed under the BSD license (see `LICENSE`); first-party additions (cloud, SIEM, proxy, database, queues/events, etc.) ship alongside them.

Pinned revision for the original set: see `PIN.txt`.

## JSON `match-keys`

Optional `match-keys` lists required JSON paths. If any path is missing, pack confidence is `0` (prevents false matches on a shared timestamp field alone).

## Binary converters

Mizpah does **not** use lnav-style `converter` JSON (packs with a `converter` key are skipped). File ingest preprocesses binaries in Rust (`file_convert.rs`):

- `.evtx` → in-process `evtx` crate → `windows_evtx_log`
- `.pcap` / `.pcapng` → `tshark -T ek` → `pcap_log`
- `nfcapd.*` → `nfdump -o json` → `netflow_log`

Post-convert packs are normal JSON packs (no `converter` field).
