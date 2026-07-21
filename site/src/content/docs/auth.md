---
title: Custom auth (OIDC)
description: Point Mizpah at your identity provider so only allowed users can use a shared hub.
order: 4
---

By default Mizpah is a **local, unauthenticated** hub on `127.0.0.1`. When you run it on a server and want specific people to open the UI, enable opt-in **OIDC** and point it at your IdP (Okta, Microsoft Entra ID, Keycloak, Google Workspace, Auth0, or any standards-compliant issuer).

Local loopback use stays unchanged until you set `auth.enabled = true`.

## What you get

| Surface | When auth is on |
|---------|-----------------|
| Web UI, query APIs, `/ws` | Session cookie after OIDC login, **or** `Authorization: Bearer <apiToken>` |
| Machine ingest (`POST /api/ingest*`) | Loopback exempt, **or** `Authorization: Bearer <ingestToken>` |
| Hub discovery | Public `GET /api/health` (no login) |
| Self-update apply | Still **loopback-only** — OIDC does not grant it |

Authorization v1 is an **email / domain allowlist** only (no roles or per-project ACLs).

## Prerequisites

1. A **confidential** OIDC application at your IdP (client id + client secret). Public / secretless PKCE-only apps are not supported in v1.
2. HTTPS in front of the hub (reverse proxy). The browser redirect URI must be HTTPS in production.
3. Mizpah config directory writable by the hub process (session signing secret is stored there unless you set `MIZPAH_SESSION_SECRET`).

Config file: `config.toml` under the Mizpah config dir (`MIZPAH_CONFIG_DIR`, or the platform path from `directories` — typically `~/Library/Application Support/dev.ethira.mizpah/` on macOS, `~/.config/mizpah/` on Linux).

## 1. Register the app at your IdP

Create a confidential / web application and set:

| Setting | Value |
|---------|--------|
| Redirect / callback URI | `https://<your-host>/api/auth/callback` |
| Grant type | Authorization code |
| Scopes | at least `openid`, `email` (and usually `profile`) |
| Client authentication | Client secret (confidential) |

Copy the **issuer URL**, **client id**, and **client secret**. Issuer examples:

| Provider | Typical `issuerUrl` |
|----------|---------------------|
| Keycloak | `https://idp.example.com/realms/<realm>` |
| Okta | `https://<org>.okta.com` (or the custom auth server issuer) |
| Microsoft Entra ID | `https://login.microsoftonline.com/<tenant-id>/v2.0` |
| Google | `https://accounts.google.com` |
| Auth0 | `https://<tenant>.auth0.com/` |

Mizpah fetches `{issuerUrl}/.well-known/openid-configuration` at hub start and fails fast if discovery fails.

## 2. Configure Mizpah

Add an `[auth]` section to `config.toml`:

```toml
[auth]
enabled = true
issuerUrl = "https://login.example.com/realms/prod"
clientId = "mizpah"
# Prefer env: MIZPAH_OIDC_CLIENT_SECRET — leave empty here
clientSecret = ""
redirectUri = "https://logs.example.com/api/auth/callback"
scopes = ["openid", "profile", "email"]
allowedEmails = ["alice@example.com"]
allowedDomains = ["example.com"]
ingestToken = ""   # or set MIZPAH_INGEST_TOKEN
apiToken = ""      # or set MIZPAH_API_TOKEN
sessionTtlHours = 12
```

### Allowlists

- If **both** `allowedEmails` and `allowedDomains` are empty → any user who can complete OIDC and has an **email** claim may use the hub.
- If either list is non-empty → the user’s email must match an entry in `allowedEmails` **or** the domain after `@` must match `allowedDomains`.

### Secrets (prefer environment)

| Variable | Purpose |
|----------|---------|
| `MIZPAH_OIDC_CLIENT_SECRET` | OIDC client secret (required when auth is enabled) |
| `MIZPAH_INGEST_TOKEN` | Bearer for remote ingest / forwarders |
| `MIZPAH_API_TOKEN` | Bearer for MCP and non-browser API clients |
| `MIZPAH_SESSION_SECRET` | Optional; otherwise Mizpah generates `session.secret` under the config dir |

Do not commit real secrets into `config.toml` if the file is shared or checked in.

## 3. Put TLS in front of the hub

Example pattern:

1. Bind Mizpah on loopback: `--host 127.0.0.1 --port 3149`
2. Reverse proxy (Caddy, nginx, Traefik, …) terminates HTTPS and proxies to `127.0.0.1:3149`
3. Forward `X-Forwarded-Proto: https` so the session cookie is marked `Secure`
4. Set `redirectUri` to the **public** HTTPS callback URL

If you must bind a non-loopback interface, pass `--allow-remote`. With auth enabled, Mizpah still warns that ingest needs a token or loopback; with auth disabled, the warning is stronger.

## 4. Start the hub and sign in

```bash
export MIZPAH_OIDC_CLIENT_SECRET='…'
export MIZPAH_INGEST_TOKEN='…'   # for remote pipes / Vector / Fluent Bit
export MIZPAH_API_TOKEN='…'      # for mzp mcp / scripts

mzp hub start --host 127.0.0.1 --port 3149
# or, if binding on a public interface behind your own controls:
# mzp hub start --host 0.0.0.0 --port 3149 --allow-remote
```

Open `https://logs.example.com/`. The SPA loads; the first API call that returns **401** redirects the browser to `/api/auth/login`, then to your IdP, then back to `/api/auth/callback`, which sets the `mizpah_session` cookie and redirects to `/`.

Useful endpoints:

| Path | Role |
|------|------|
| `GET /api/auth/login` | Start OIDC (PKCE + state) |
| `GET /api/auth/callback` | Code exchange, allowlist, set cookie |
| `POST /api/auth/logout` | Clear cookie |
| `GET /api/auth/me` | Current user (or 401) |
| `GET /api/health` | Always public liveness |

## 5. Wire machine clients

### Ingest (pipes, attach, file forwarders, Vector)

On non-loopback ingest, send the ingest bearer:

```bash
export MIZPAH_INGEST_TOKEN='…'
my-app | mzp --service api --host logs.example.com --port 443
# or HTTP:
curl -sS -X POST "https://logs.example.com/api/ingest/batch" \
  -H "Authorization: Bearer $MIZPAH_INGEST_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"service":"vector","lines":["{\"msg\":\"hi\"}"]}'
```

Loopback ingest to a hub on `127.0.0.1` does not need the token.

### MCP / API query

```bash
export MIZPAH_URL='https://logs.example.com'
export MIZPAH_API_TOKEN='…'
mzp mcp
```

## Checklist

1. IdP confidential app with redirect `https://<host>/api/auth/callback`
2. `[auth] enabled = true` with `issuerUrl`, `clientId`, `redirectUri`
3. `MIZPAH_OIDC_CLIENT_SECRET` set in the hub environment
4. Allowlist emails/domains (or leave both empty only if every IdP user may access logs)
5. TLS reverse proxy + `X-Forwarded-Proto`
6. `MIZPAH_INGEST_TOKEN` for any remote ingest path
7. `MIZPAH_API_TOKEN` for MCP / automation

## Troubleshooting

| Symptom | Likely cause |
|---------|----------------|
| Hub exits on start: `auth config: …` | Missing issuer/client/secret/redirect, or OIDC discovery failed |
| Login works but “email … is not allowed” | Allowlist mismatch; check email claim and domains |
| Cookie not sticking / immediate re-login | Missing HTTPS or `X-Forwarded-Proto` (cookie needs `Secure`) |
| Remote ingest 401 | Set `MIZPAH_INGEST_TOKEN` on the client to match hub `ingestToken` |
| MCP 401 | Set `MIZPAH_API_TOKEN` to match hub `apiToken` |
| Callback 404 / HTML shell | Proxy must forward `/api/auth/*` to the hub (not only `/`) |

## Related

- [Hub trust model](../development/#hub-trust-model) — bind policy and architecture
- [Storage security](../storage-security/) — encryption at rest (separate from HTTP auth)
- [SIEM → one hub](../siem-ingest/) — multi-source ingest into a shared hub
- [Streaming & hub](../streaming/) — HTTP surface and attach model
