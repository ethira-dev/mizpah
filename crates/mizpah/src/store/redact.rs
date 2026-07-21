//! Best-effort secret redaction before log rows enter the in-memory ring.

use regex::Regex;
use serde_json::{Map, Value};
use std::sync::OnceLock;

fn bearer_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(bearer\s+)([a-z0-9._\-+/=]{8,})").expect("regex"))
}

fn api_key_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)\b(api[_-]?key|access[_-]?token|secret|password)\b(\s*[:=]\s*)([^\s"',}\\]+)"#)
            .expect("regex")
    })
}

fn pem_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"-----BEGIN [A-Z0-9 ]+-----[\s\S]*?-----END [A-Z0-9 ]+-----").expect("regex")
    })
}

const REDACTED: &str = "[REDACTED]";

/// Redact common secret patterns in string leaves of a JSON value (in place).
pub fn redact_value(value: &mut Value) {
    match value {
        Value::String(s) => {
            *s = redact_string(s);
        }
        Value::Array(items) => {
            for item in items {
                redact_value(item);
            }
        }
        Value::Object(map) => redact_object(map),
        _ => {}
    }
}

fn redact_object(map: &mut Map<String, Value>) {
    // Clone keys to allow mutation while inspecting names.
    let keys: Vec<String> = map.keys().cloned().collect();
    for key in keys {
        let key_lower = key.to_ascii_lowercase();
        let sensitive_key = matches!(
            key_lower.as_str(),
            "authorization"
                | "proxy-authorization"
                | "x-api-key"
                | "x-amz-security-token"
                | "password"
                | "passwd"
                | "secret"
                | "api_key"
                | "apikey"
                | "access_token"
                | "refresh_token"
                | "id_token"
                | "private_key"
        );
        if let Some(v) = map.get_mut(&key) {
            if sensitive_key {
                if matches!(v, Value::String(_)) {
                    *v = Value::String(REDACTED.into());
                } else {
                    redact_value(v);
                }
            } else {
                redact_value(v);
            }
        }
    }
}

fn redact_string(input: &str) -> String {
    let mut out = bearer_re()
        .replace_all(input, |caps: &regex::Captures| {
            format!("{}{REDACTED}", &caps[1])
        })
        .into_owned();
    out = api_key_re()
        .replace_all(&out, |caps: &regex::Captures| {
            format!("{}{}{REDACTED}", &caps[1], &caps[2])
        })
        .into_owned();
    out = pem_re().replace_all(&out, REDACTED).into_owned();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_authorization_and_bearer() {
        let mut v = json!({
            "authorization": "Bearer supersecretvalue",
            "msg": "token Bearer abcdefghijklmnop used",
        });
        redact_value(&mut v);
        assert_eq!(v["authorization"], REDACTED);
        assert!(v["msg"].as_str().unwrap().contains(REDACTED));
        assert!(!v["msg"].as_str().unwrap().contains("abcdefghijklmnop"));
    }

    #[test]
    fn redacts_api_key_and_pem() {
        let mut v = json!({
            "msg": "api_key=abcd1234xyz and more",
            "cert": "-----BEGIN PRIVATE KEY-----\nABC\n-----END PRIVATE KEY-----",
        });
        redact_value(&mut v);
        assert!(v["msg"].as_str().unwrap().contains(REDACTED));
        assert_eq!(v["cert"], REDACTED);
    }
}
