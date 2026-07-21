//! Opt-in OIDC auth: sessions, allowlists, ingest/API tokens, login routes.

use axum::extract::{Query, Request, State};
use axum::http::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet,
    EndpointSet, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    TokenResponse,
};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

use crate::config::AuthConfig;
use crate::error::ApiError;
use crate::util::config_dir;

type HmacSha256 = Hmac<Sha256>;

/// OIDC client after discovery + redirect URI (auth + optional token/userinfo endpoints).
type OidcClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

pub const SESSION_COOKIE: &str = "mizpah_session";
const SESSION_SECRET_FILE: &str = "session.secret";
const PENDING_TTL: Duration = Duration::from_secs(600);

#[derive(Clone)]
pub struct AuthState {
    pub session_secret: Vec<u8>,
    pub session_ttl: Duration,
    pub allowed_emails: Vec<String>,
    pub allowed_domains: Vec<String>,
    pub ingest_token: Option<String>,
    pub api_token: Option<String>,
    /// `None` only in unit tests that exercise token/session middleware without IdP discovery.
    pub oidc: Option<Arc<OidcRuntime>>,
}

pub struct OidcRuntime {
    client: OidcClient,
    http: openidconnect::reqwest::Client,
    scopes: Vec<String>,
    pending: Mutex<HashMap<String, PendingLogin>>,
}

struct PendingLogin {
    pkce_verifier: PkceCodeVerifier,
    nonce: Nonce,
    created: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionPayload {
    email: String,
    name: Option<String>,
    exp: u64,
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// Resolve env overrides onto a cloned [`AuthConfig`].
pub fn resolve_auth_config(cfg: &AuthConfig) -> AuthConfig {
    let mut out = cfg.clone();
    if let Ok(v) = std::env::var("MIZPAH_OIDC_CLIENT_SECRET") {
        let t = v.trim();
        if !t.is_empty() {
            out.client_secret = t.to_string();
        }
    }
    if let Ok(v) = std::env::var("MIZPAH_INGEST_TOKEN") {
        let t = v.trim();
        if !t.is_empty() {
            out.ingest_token = t.to_string();
        }
    }
    if let Ok(v) = std::env::var("MIZPAH_API_TOKEN") {
        let t = v.trim();
        if !t.is_empty() {
            out.api_token = t.to_string();
        }
    }
    out
}

/// Build auth state when `[auth] enabled = true`. Returns `Ok(None)` when disabled.
pub async fn try_build_auth_state(cfg: &AuthConfig) -> Result<Option<AuthState>, String> {
    if !cfg.enabled {
        return Ok(None);
    }
    let cfg = resolve_auth_config(cfg);
    validate_enabled_config(&cfg)?;

    crate::util::ensure_rustls_crypto_provider();
    let http = openidconnect::reqwest::ClientBuilder::new()
        .redirect(openidconnect::reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("auth http client: {e}"))?;

    let issuer = IssuerUrl::new(cfg.issuer_url.trim().trim_end_matches('/').to_string())
        .map_err(|e| format!("invalid issuerUrl: {e}"))?;
    let provider_metadata = CoreProviderMetadata::discover_async(issuer, &http)
        .await
        .map_err(|e| format!("OIDC discovery failed: {e}"))?;

    let client = CoreClient::from_provider_metadata(
        provider_metadata,
        ClientId::new(cfg.client_id.clone()),
        Some(ClientSecret::new(cfg.client_secret.clone())),
    )
    .set_redirect_uri(
        RedirectUrl::new(cfg.redirect_uri.clone())
            .map_err(|e| format!("invalid redirectUri: {e}"))?,
    );

    let session_secret = load_or_create_session_secret()?;
    let session_ttl = Duration::from_secs(cfg.session_ttl_hours.max(1) * 3600);
    let ingest_token = non_empty(cfg.ingest_token);
    let api_token = non_empty(cfg.api_token);
    if ingest_token.is_none() {
        tracing::warn!(
            "auth enabled but ingestToken is empty; non-loopback ingest will return 401"
        );
    }

    Ok(Some(AuthState {
        session_secret,
        session_ttl,
        allowed_emails: normalize_list(&cfg.allowed_emails),
        allowed_domains: normalize_list(&cfg.allowed_domains),
        ingest_token,
        api_token,
        oidc: Some(Arc::new(OidcRuntime {
            client,
            http,
            scopes: if cfg.scopes.is_empty() {
                vec!["openid".into(), "profile".into(), "email".into()]
            } else {
                cfg.scopes
            },
            pending: Mutex::new(HashMap::new()),
        })),
    }))
}

/// Test helper: auth middleware + tokens without live OIDC discovery.
#[cfg(test)]
pub fn test_auth_state(api_token: Option<&str>, ingest_token: Option<&str>) -> AuthState {
    AuthState {
        session_secret: b"0123456789abcdef0123456789abcdef".to_vec(),
        session_ttl: Duration::from_secs(3600),
        allowed_emails: Vec::new(),
        allowed_domains: Vec::new(),
        ingest_token: ingest_token.map(str::to_string),
        api_token: api_token.map(str::to_string),
        oidc: None,
    }
}

fn validate_enabled_config(cfg: &AuthConfig) -> Result<(), String> {
    if cfg.issuer_url.trim().is_empty() {
        return Err("auth.enabled requires issuerUrl".into());
    }
    if cfg.client_id.trim().is_empty() {
        return Err("auth.enabled requires clientId".into());
    }
    if cfg.client_secret.trim().is_empty() {
        return Err("auth.enabled requires clientSecret (or MIZPAH_OIDC_CLIENT_SECRET)".into());
    }
    if cfg.redirect_uri.trim().is_empty() {
        return Err("auth.enabled requires redirectUri".into());
    }
    Ok(())
}

fn non_empty(s: String) -> Option<String> {
    let t = s.trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

fn normalize_list(items: &[String]) -> Vec<String> {
    items
        .iter()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn load_or_create_session_secret() -> Result<Vec<u8>, String> {
    if let Ok(v) = std::env::var("MIZPAH_SESSION_SECRET") {
        let t = v.trim();
        if !t.is_empty() {
            return Ok(t.as_bytes().to_vec());
        }
    }
    let path = session_secret_path().map_err(|e| e.to_string())?;
    if path.exists() {
        let text = fs::read_to_string(&path).map_err(|e| format!("read session secret: {e}"))?;
        let bytes = URL_SAFE_NO_PAD
            .decode(text.trim())
            .map_err(|e| format!("decode session secret: {e}"))?;
        if bytes.len() >= 32 {
            return Ok(bytes);
        }
    }
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|e| format!("generate session secret: {e}"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }
    let encoded = URL_SAFE_NO_PAD.encode(buf);
    crate::util::atomic_write(&path, &encoded)
        .map_err(|e| format!("write session secret: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(buf.to_vec())
}

fn session_secret_path() -> io::Result<PathBuf> {
    Ok(config_dir()?.join(SESSION_SECRET_FILE))
}

/// Empty allowlists → any authenticated email; otherwise email or domain must match.
pub fn email_allowed(email: &str, emails: &[String], domains: &[String]) -> bool {
    let email = email.trim().to_ascii_lowercase();
    if email.is_empty() || !email.contains('@') {
        return false;
    }
    if emails.is_empty() && domains.is_empty() {
        return true;
    }
    if emails.iter().any(|e| e == &email) {
        return true;
    }
    if let Some((_, domain)) = email.split_once('@') {
        return domains.iter().any(|d| d == domain);
    }
    false
}

fn sign_session(secret: &[u8], payload: &SessionPayload) -> Result<String, String> {
    let json = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
    let body = URL_SAFE_NO_PAD.encode(&json);
    let mut mac = HmacSha256::new_from_slice(secret).map_err(|e| e.to_string())?;
    mac.update(body.as_bytes());
    let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Ok(format!("{body}.{sig}"))
}

fn verify_session(secret: &[u8], cookie: &str) -> Option<SessionPayload> {
    let (body, sig) = cookie.split_once('.')?;
    let mut mac = HmacSha256::new_from_slice(secret).ok()?;
    mac.update(body.as_bytes());
    let expected = mac.finalize().into_bytes();
    let got = URL_SAFE_NO_PAD.decode(sig).ok()?;
    if expected.as_slice() != got.as_slice() {
        return None;
    }
    let json = URL_SAFE_NO_PAD.decode(body).ok()?;
    let payload: SessionPayload = serde_json::from_slice(&json).ok()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    if payload.exp <= now {
        return None;
    }
    Some(payload)
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let rest = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?;
    let t = rest.trim();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookie = headers.get(COOKIE)?.to_str().ok()?;
    for part in cookie.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(&format!("{name}=")) {
            return Some(v.to_string());
        }
    }
    None
}

fn request_is_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|p| {
            p.split(',')
                .next()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("https"))
        })
}

fn session_set_cookie(value: &str, max_age: u64, secure: bool) -> HeaderValue {
    let mut s = format!(
        "{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}"
    );
    if secure {
        s.push_str("; Secure");
    }
    HeaderValue::from_str(&s).unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn session_clear_cookie(secure: bool) -> HeaderValue {
    let mut s = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if secure {
        s.push_str("; Secure");
    }
    HeaderValue::from_str(&s).unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn peer_is_loopback(req: &Request) -> bool {
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|ConnectInfo(addr)| addr.ip().is_loopback())
}

pub fn session_authorized(auth: &AuthState, headers: &HeaderMap) -> bool {
    if let Some(token) = bearer_token(headers) {
        if auth
            .api_token
            .as_deref()
            .is_some_and(|expected| expected == token)
        {
            return true;
        }
    }
    if let Some(cookie) = cookie_value(headers, SESSION_COOKIE) {
        return verify_session(&auth.session_secret, &cookie).is_some();
    }
    false
}

pub fn ingest_authorized(auth: &AuthState, req: &Request) -> bool {
    if peer_is_loopback(req) {
        return true;
    }
    let Some(expected) = auth.ingest_token.as_deref() else {
        return false;
    };
    bearer_token(req.headers()).is_some_and(|t| t == expected)
}

pub async fn require_session(
    State(state): State<crate::api::AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Ok(next.run(req).await);
    };
    if session_authorized(auth, req.headers()) {
        return Ok(next.run(req).await);
    }
    Err(ApiError::unauthorized("authentication required"))
}

pub async fn require_ingest(
    State(state): State<crate::api::AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Ok(next.run(req).await);
    };
    if ingest_authorized(auth, &req) {
        return Ok(next.run(req).await);
    }
    Err(ApiError::unauthorized(
        "ingest requires loopback or Authorization: Bearer <ingestToken>",
    ))
}

pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

pub async fn me(
    State(state): State<crate::api::AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Ok(Json(serde_json::json!({ "auth": false })));
    };
    if let Some(token) = bearer_token(&headers) {
        if auth.api_token.as_deref() == Some(token) {
            return Ok(Json(serde_json::json!({
                "auth": true,
                "email": null,
                "name": null,
                "via": "apiToken",
            })));
        }
    }
    let cookie = cookie_value(&headers, SESSION_COOKIE)
        .ok_or_else(|| ApiError::unauthorized("not signed in"))?;
    let session = verify_session(&auth.session_secret, &cookie)
        .ok_or_else(|| ApiError::unauthorized("invalid or expired session"))?;
    Ok(Json(serde_json::json!({
        "auth": true,
        "email": session.email,
        "name": session.name,
        "via": "session",
    })))
}

pub async fn login(State(state): State<crate::api::AppState>) -> Result<Response, ApiError> {
    let auth = state
        .auth
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("auth is not enabled"))?;
    let oidc = auth
        .oidc
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("OIDC is not configured"))?;
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let mut auth_req = oidc.client.authorize_url(
        CoreAuthenticationFlow::AuthorizationCode,
        CsrfToken::new_random,
        Nonce::new_random,
    );
    for scope in &oidc.scopes {
        auth_req = auth_req.add_scope(Scope::new(scope.clone()));
    }
    let (url, csrf, nonce) = auth_req.set_pkce_challenge(pkce_challenge).url();

    {
        let mut pending = oidc.pending.lock().await;
        pending.retain(|_, p| p.created.elapsed() < PENDING_TTL);
        pending.insert(
            csrf.secret().clone(),
            PendingLogin {
                pkce_verifier,
                nonce,
                created: Instant::now(),
            },
        );
    }

    Ok(Redirect::temporary(url.as_str()).into_response())
}

pub async fn callback(
    State(state): State<crate::api::AppState>,
    Query(q): Query<CallbackQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let auth = state
        .auth
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("auth is not enabled"))?;
    let oidc = auth
        .oidc
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("OIDC is not configured"))?;
    if let Some(err) = q.error.as_deref() {
        let detail = q.error_description.as_deref().unwrap_or(err);
        return Err(ApiError::forbidden(format!("OIDC error: {detail}")));
    }
    let code = q
        .code
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("missing code"))?;
    let state_param = q
        .state
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("missing state"))?;

    let pending = {
        let mut map = oidc.pending.lock().await;
        map.remove(state_param)
            .ok_or_else(|| ApiError::forbidden("invalid or expired OIDC state"))?
    };

    let token_response = oidc
        .client
        .exchange_code(AuthorizationCode::new(code.to_string()))
        .map_err(|e| ApiError::bad_gateway(format!("token exchange setup: {e}")))?
        .set_pkce_verifier(pending.pkce_verifier)
        .request_async(&oidc.http)
        .await
        .map_err(|e| ApiError::bad_gateway(format!("token exchange failed: {e}")))?;

    let id_token = token_response
        .id_token()
        .ok_or_else(|| ApiError::bad_gateway("IdP did not return an id_token"))?;
    let claims = id_token
        .claims(&oidc.client.id_token_verifier(), &pending.nonce)
        .map_err(|e| ApiError::forbidden(format!("invalid id_token: {e}")))?;

    let email = claims
        .email()
        .map(|e| e.to_string())
        .filter(|e| !e.is_empty())
        .ok_or_else(|| ApiError::forbidden("id_token missing email claim"))?;
    if !email_allowed(&email, &auth.allowed_emails, &auth.allowed_domains) {
        return Err(ApiError::forbidden(format!(
            "email {email} is not allowed"
        )));
    }
    let name = claims
        .name()
        .and_then(|n| n.get(None))
        .map(|n| n.to_string());

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .as_secs();
    let payload = SessionPayload {
        email,
        name,
        exp: now + auth.session_ttl.as_secs(),
    };
    let cookie_val = sign_session(&auth.session_secret, &payload).map_err(ApiError::internal)?;
    let secure = request_is_https(&headers);
    let mut resp = Redirect::temporary("/").into_response();
    resp.headers_mut().insert(
        SET_COOKIE,
        session_set_cookie(&cookie_val, auth.session_ttl.as_secs(), secure),
    );
    Ok(resp)
}

pub async fn logout(
    State(_state): State<crate::api::AppState>,
    headers: HeaderMap,
) -> Response {
    let secure = request_is_https(&headers);
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut()
        .insert(SET_COOKIE, session_clear_cookie(secure));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_empty_allows_any_email() {
        assert!(email_allowed("a@b.com", &[], &[]));
        assert!(!email_allowed("", &[], &[]));
        assert!(!email_allowed("nope", &[], &[]));
    }

    #[test]
    fn allowlist_email_and_domain() {
        let emails = vec!["alice@example.com".into()];
        let domains = vec!["corp.io".into()];
        assert!(email_allowed("alice@example.com", &emails, &domains));
        assert!(email_allowed("bob@corp.io", &emails, &domains));
        assert!(!email_allowed("eve@other.com", &emails, &domains));
    }

    #[test]
    fn session_roundtrip() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let payload = SessionPayload {
            email: "a@b.com".into(),
            name: Some("A".into()),
            exp: now + 3600,
        };
        let cookie = sign_session(secret, &payload).unwrap();
        let back = verify_session(secret, &cookie).unwrap();
        assert_eq!(back.email, "a@b.com");
        assert_eq!(back.name.as_deref(), Some("A"));
    }

    #[test]
    fn session_rejects_tamper_and_expiry() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let cookie = sign_session(
            secret,
            &SessionPayload {
                email: "a@b.com".into(),
                name: None,
                exp: now + 3600,
            },
        )
        .unwrap();
        assert!(verify_session(secret, &format!("{cookie}x")).is_none());
        let expired = sign_session(
            secret,
            &SessionPayload {
                email: "a@b.com".into(),
                name: None,
                exp: now - 1,
            },
        )
        .unwrap();
        assert!(verify_session(secret, &expired).is_none());
    }

    #[test]
    fn validate_enabled_requires_fields() {
        let mut cfg = AuthConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(validate_enabled_config(&cfg).is_err());
        cfg.issuer_url = "https://idp.example".into();
        cfg.client_id = "mizpah".into();
        cfg.client_secret = "secret".into();
        cfg.redirect_uri = "https://logs.example/api/auth/callback".into();
        assert!(validate_enabled_config(&cfg).is_ok());
    }

    #[test]
    fn resolve_auth_config_env_overrides() {
        let _g = crate::test_support::env_lock();
        std::env::set_var("MIZPAH_OIDC_CLIENT_SECRET", "from-env");
        std::env::set_var("MIZPAH_INGEST_TOKEN", "ingest");
        std::env::set_var("MIZPAH_API_TOKEN", "api");
        let cfg = AuthConfig {
            client_secret: "file".into(),
            ..Default::default()
        };
        let resolved = resolve_auth_config(&cfg);
        assert_eq!(resolved.client_secret, "from-env");
        assert_eq!(resolved.ingest_token, "ingest");
        assert_eq!(resolved.api_token, "api");
        std::env::remove_var("MIZPAH_OIDC_CLIENT_SECRET");
        std::env::remove_var("MIZPAH_INGEST_TOKEN");
        std::env::remove_var("MIZPAH_API_TOKEN");
    }
}
