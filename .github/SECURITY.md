# Security Policy

## Reporting a vulnerability

Please report security issues privately via [GitHub Security Advisories](https://github.com/ethira-dev/mizpah/security/advisories/new) for this repository.

Do **not** open a public issue for an unfixed vulnerability.

We aim to acknowledge reports within a few days.

## Local log storage

When Mizpah writes logs to disk (`persistDir` or self-update spill), payloads are encrypted at rest with AES-256-GCM using a per-install data encryption key (DEK):

- Prefer the OS credential store (macOS Keychain, Windows Credential Manager, Linux Secret Service).
- If the OS store is unavailable, a machine/user-bound sealed key file is used under the config directory (`0o600`).
- Persist segment files and spill blobs use private file modes (`0o600`); the config/persist directory is `0o700` when the platform supports it.
- Legacy plaintext persist segments are migrated to ciphertext on hydrate.

The in-memory ring stays plaintext so search stays fast. The hub disables core dumps (and on Linux sets non-dumpable) to reduce accidental leakage. Ingest applies best-effort redaction of common secret patterns; this is not a DLP product.

**Threat model:** encryption-at-rest protects other local users, backups of the persist/spill files, and leftover disk images. A compromised local account (same user with debugger/root, or anyone who can call the loopback hub API) is out of scope.

User-facing detail: [Storage security](https://ethira-dev.github.io/mizpah/docs/storage-security/) (site docs).

## Dependencies

Known Rust dependency advisories are tracked via the [RustSec Advisory Database](https://rustsec.org/) and checked in CI with `cargo audit`.
