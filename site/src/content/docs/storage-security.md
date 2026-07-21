---
title: Storage security
description: Zero-config encryption at rest, ring retention, redaction, and what Mizpah does (and does not) protect.
order: 3
---

Mizpah is built for local log browsing. By default logs live only in an **in-memory ring**. When something must hit disk, encryption and private file modes turn on automatically — no passphrases, no `persistEncrypt` flags, no key files to manage.

## What lives where

| Location | Default | Contents |
|----------|---------|----------|
| In-memory ring | Always on | Queryable log entries (plaintext in process for fast CEL/SQL) |
| Persist segments | Off until `persistDir` is set | Encrypted append-only segments under the config tree |
| Update spill | Temporary, during self-update | Encrypted blob so the buffer survives a binary replace |
| Config dir | Always | `config.toml`, PID files, optional sealed DEK fallback |

Typical config dir paths (override with `MIZPAH_CONFIG_DIR`):

| Platform | Path |
|----------|------|
| macOS | `~/Library/Application Support/dev.ethira.mizpah/` |
| Linux | `~/.config/mizpah/` |
| Windows | `%APPDATA%\ethira\mizpah\` |

## Enabling durable persist

Optional. Add to `config.toml` (camelCase):

```toml
persistDir = "persist"   # relative → under config dir; or an absolute path
maxBytes = 1073741824    # ring + disk prune budget (default 1 GiB)
ttlHours = 24            # age eviction; 0 disables (default 24)
```

CLI flags `--max-bytes` / `--ttl-hours` still apply to the hub ring. With persist enabled, disk segments are pruned to the same TTL / byte policy so sensitive rows do not linger forever on disk after they leave memory.

`mzp hub restart` clears the **in-memory** buffer; hydrated persist (if configured) reloads encrypted segments on the next hub start.

## Encryption at rest (automatic)

Whenever Mizpah writes log payloads to disk (persist **or** update spill):

1. A per-install **data encryption key (DEK)** is created on first use.
2. The DEK is stored in the **OS credential store** when available:
   - macOS Keychain
   - Windows Credential Manager
   - Linux Secret Service / `libsecret`
3. Each record (or spill blob) is sealed with **AES-256-GCM** (versioned framing + random nonce). Persist lines look like `mzp1:` + base64 — not readable JSON.
4. On hydrate / spill restore, ciphertext is decrypted into the ring only.

macOS may show a **one-time Keychain allow** dialog the first time the hub needs the DEK. That is the only expected prompt; there is nothing to put in `config.toml` for encryption.

### Keychain unavailable (silent fallback)

Headless Linux without Secret Service, locked-down environments, or CI:

- Mizpah writes a **sealed** key file under the config dir (`log-store.dek` + salt), mode `0o600`.
- The wrap key is bound to local user/machine material (HKDF), so copying ciphertext alone to another host does not unlock it.
- A single warning is logged: file-backed key in use. Still zero config for you.

Same-user backups of the **entire** config directory remain a residual risk in fallback mode; prefer the OS keychain when possible.

### Legacy plaintext segments

Older installs may have plaintext `segment-*.ndjson` files. On hydrate, Mizpah loads them, **rewrites encrypted segments**, and drops the plaintext. No manual migration step.

## Filesystem hardening

Applied automatically when writing persist / spill / key material:

- Config and persist directories: `0o700` (Unix)
- Segment files, spill blobs, sealed key files: `0o600`
- Symlink paths are refused (`O_NOFOLLOW` where available)

## In-memory protections

The working ring stays **plaintext in process** so search stays fast. Mizpah still reduces accidental leakage:

| Control | Behavior |
|---------|----------|
| TTL + `maxBytes` | Default 24h / 1 GiB; shorter dwell = smaller blast radius |
| Core dumps | Disabled for the hub (`RLIMIT_CORE=0`) |
| Linux `ptrace` bar | Hub sets non-dumpable (`PR_SET_DUMPABLE=0`) — blocks casual same-user debugger attach (not root) |
| Best-effort redaction | Common patterns redacted at ingest (`Authorization`, `Bearer …`, `api_key=`, PEM blocks, etc.). Not a DLP product |
| SQL snapshot | Ephemeral in-memory SQLite only — not written to disk |

## Threat model

**In scope (what storage security targets)**

- Other local OS users reading persist/spill files
- Backup / sync tools picking up segment or spill files
- Leftover disk images after a crash or uninstall
- Accidental core dumps of the hub process

**Out of scope**

- A compromised account that already runs as your user (binary replace, `LD_PRELOAD`, root `ptrace`, unlocked keychain scrape)
- Anyone who can reach an exposed hub HTTP API (default bind is loopback; see [Hub trust model](../development/#hub-trust-model))
- Encrypting the in-memory ring or claiming debugger immunity

For shared or remote hubs, combine loopback / SSH tunnels with optional [Custom auth (OIDC)](../auth/).

## Related

- [Streaming & hub](../streaming/) — ring buffer, ingest, entry shape
- [CLI reference](../cli/) — `--max-bytes`, `--ttl-hours`, hub lifecycle
- [Attach sources](../attach/) — treat shell/browser/agent streams as sensitive
- Vulnerability reporting: repository [SECURITY.md](https://github.com/ethira-dev/mizpah/blob/main/.github/SECURITY.md)
