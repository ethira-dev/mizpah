//! CDP event → hub ingest line mappers.

use base64::Engine;
use serde_json::{json, Map, Value};
use url::Url;

pub(crate) const BODY_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct IngestItem {
    pub service: String,
    pub line: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedBody {
    pub data: String,
    pub encoding: &'static str,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingNetwork {
    pub session_id: String,
    pub request_id: String,
    pub method: String,
    pub url: String,
    pub resource_type: String,
    pub request_headers: Value,
    pub request_body: Option<EncodedBody>,
    pub status: Option<u64>,
    pub mime_type: Option<String>,
    pub response_headers: Option<Value>,
    pub started_at: f64,
}

/// Derive hub service name from a page URL (`location.host` semantics).
pub fn service_from_page_url(page_url: &str) -> String {
    let trimmed = page_url.trim();
    if trimmed.is_empty() || trimmed == "about:blank" {
        return "browser".into();
    }
    let Ok(url) = Url::parse(trimmed) else {
        return "browser".into();
    };
    match url.scheme() {
        "chrome" | "chrome-extension" | "devtools" | "chrome-search" | "chrome-untrusted" => {
            return "chrome-internal".into();
        }
        "file" => return "file".into(),
        _ => {}
    }
    let host = match url.host_str() {
        Some(h) if !h.is_empty() => h,
        _ => return "browser".into(),
    };
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

pub fn should_emit_network(resource_type: &str, all_network: bool) -> bool {
    if all_network {
        return true;
    }
    matches!(resource_type, "Document" | "XHR" | "Fetch" | "WebSocket")
}

pub fn should_fetch_body(resource_type: &str) -> bool {
    matches!(resource_type, "Document" | "XHR" | "Fetch")
}

pub fn skip_body_url(url: &str) -> bool {
    url.starts_with("data:") || url.starts_with("blob:")
}

/// Truncate and encode a body as utf8 or base64.
pub fn encode_body_bytes(bytes: &[u8]) -> EncodedBody {
    let truncated = bytes.len() > BODY_MAX_BYTES;
    let slice = if truncated {
        &bytes[..BODY_MAX_BYTES]
    } else {
        bytes
    };
    if let Ok(s) = std::str::from_utf8(slice) {
        if !s.contains('\0') {
            return EncodedBody {
                data: s.to_string(),
                encoding: "utf8",
                truncated,
            };
        }
    }
    EncodedBody {
        data: base64::engine::general_purpose::STANDARD.encode(slice),
        encoding: "base64",
        truncated,
    }
}

pub fn encode_body_str(s: &str) -> EncodedBody {
    encode_body_bytes(s.as_bytes())
}

pub(crate) fn decode_cdp_body(body_val: &Value) -> Option<EncodedBody> {
    let body = body_val.get("body")?.as_str()?;
    let base64_encoded = body_val
        .get("base64Encoded")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if base64_encoded {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(body)
            .ok()?;
        Some(encode_body_bytes(&bytes))
    } else {
        Some(encode_body_str(body))
    }
}

pub(crate) fn extract_request_body(request: &Value) -> Option<EncodedBody> {
    if let Some(post) = request.get("postData").and_then(|v| v.as_str()) {
        return Some(encode_body_str(post));
    }
    if let Some(entries) = request.get("postDataEntries").and_then(|v| v.as_array()) {
        let mut combined = String::new();
        for entry in entries {
            if let Some(bytes) = entry.get("bytes").and_then(|v| v.as_str()) {
                if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(bytes) {
                    if let Ok(s) = String::from_utf8(decoded) {
                        combined.push_str(&s);
                        continue;
                    }
                }
                combined.push_str(bytes);
            }
        }
        if !combined.is_empty() {
            return Some(encode_body_str(&combined));
        }
    }
    None
}

fn remote_object_to_json(obj: &Value) -> Value {
    if let Some(v) = obj.get("value") {
        return v.clone();
    }
    if let Some(unserializable) = obj.get("unserializableValue") {
        return unserializable.clone();
    }
    if let Some(preview) = obj.get("preview") {
        if let Some(props) = preview.get("properties").and_then(|p| p.as_array()) {
            let mut map = Map::new();
            for p in props {
                let name = p
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let val = p
                    .get("value")
                    .cloned()
                    .or_else(|| {
                        p.get("valuePreview")
                            .and_then(|v| v.get("description"))
                            .cloned()
                    })
                    .unwrap_or(Value::Null);
                if !name.is_empty() {
                    map.insert(name, val);
                }
            }
            if !map.is_empty() {
                return Value::Object(map);
            }
        }
    }
    let type_name = obj
        .get("className")
        .or_else(|| obj.get("type"))
        .and_then(|t| t.as_str())
        .unwrap_or("object");
    let description = obj
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or(type_name);
    json!({
        "_type": type_name,
        "description": description,
    })
}

fn console_level(cdp_type: &str) -> &'static str {
    match cdp_type {
        "error" | "assert" => "error",
        "warning" => "warn",
        "info" => "info",
        "debug" | "verbose" => "debug",
        "trace" => "trace",
        _ => "log",
    }
}

pub(crate) fn map_console_api(params: &Value, page_url: &str, host: &str) -> Option<IngestItem> {
    let level = console_level(params.get("type").and_then(|t| t.as_str()).unwrap_or("log"));
    let args_raw = params.get("args").and_then(|a| a.as_array());
    let args: Vec<Value> = args_raw
        .map(|a| a.iter().map(remote_object_to_json).collect())
        .unwrap_or_default();
    let msg = format_console_msg(&args);
    let ts = params.get("timestamp").cloned().unwrap_or(Value::Null);
    let payload = json!({
        "source": "browser",
        "kind": "console",
        "browser": "chrome",
        "level": level,
        "msg": msg,
        "args": args,
        "pageUrl": page_url,
        "host": host,
        "hostname": host_only(host),
        "ts": ts,
    });
    Some(IngestItem {
        service: host.to_string(),
        line: payload.to_string(),
    })
}

fn format_console_msg(args: &[Value]) -> String {
    if args.is_empty() {
        return String::new();
    }
    args.iter()
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn host_only(host: &str) -> String {
    host.split(':').next().unwrap_or(host).to_string()
}

pub(crate) fn map_log_entry(params: &Value, page_url: &str, host: &str) -> Option<IngestItem> {
    let entry = params.get("entry")?;
    let level = match entry
        .get("level")
        .and_then(|l| l.as_str())
        .unwrap_or("info")
    {
        "error" => "error",
        "warning" => "warn",
        "verbose" => "debug",
        _ => "info",
    };
    let msg = entry
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let url = entry
        .get("url")
        .and_then(|u| u.as_str())
        .unwrap_or(page_url);
    let payload = json!({
        "source": "browser",
        "kind": "console",
        "browser": "chrome",
        "level": level,
        "msg": msg,
        "args": [msg],
        "pageUrl": url,
        "host": host,
        "hostname": host_only(host),
        "ts": entry.get("timestamp").cloned().unwrap_or(Value::Null),
    });
    Some(IngestItem {
        service: host.to_string(),
        line: payload.to_string(),
    })
}

pub(crate) fn map_exception(params: &Value, page_url: &str, host: &str) -> Option<IngestItem> {
    let details = params.get("exceptionDetails")?;
    let msg = details
        .get("text")
        .and_then(|t| t.as_str())
        .or_else(|| {
            details
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(|d| d.as_str())
        })
        .unwrap_or("uncaught exception")
        .to_string();
    let payload = json!({
        "source": "browser",
        "kind": "console",
        "browser": "chrome",
        "level": "error",
        "msg": msg,
        "args": [msg],
        "pageUrl": page_url,
        "host": host,
        "hostname": host_only(host),
        "exception": details,
        "ts": params.get("timestamp").cloned().unwrap_or(Value::Null),
    });
    Some(IngestItem {
        service: host.to_string(),
        line: payload.to_string(),
    })
}

pub(crate) fn map_network_finished(
    pending: &PendingNetwork,
    response_body: Option<EncodedBody>,
    duration_ms: Option<f64>,
    page_url: &str,
    host: &str,
) -> Option<IngestItem> {
    let mut payload = json!({
        "source": "browser",
        "kind": "network",
        "browser": "chrome",
        "requestId": pending.request_id,
        "method": pending.method,
        "url": pending.url,
        "status": pending.status,
        "mimeType": pending.mime_type,
        "resourceType": pending.resource_type,
        "durationMs": duration_ms,
        "requestHeaders": pending.request_headers,
        "responseHeaders": pending.response_headers.clone().unwrap_or(Value::Object(Map::new())),
        "pageUrl": page_url,
        "host": host,
        "hostname": host_only(host),
    });
    let obj = payload.as_object_mut()?;
    if let Some(rb) = &pending.request_body {
        obj.insert("requestBody".into(), Value::String(rb.data.clone()));
        obj.insert(
            "requestBodyEncoding".into(),
            Value::String(rb.encoding.into()),
        );
        obj.insert("requestBodyTruncated".into(), Value::Bool(rb.truncated));
    }
    if let Some(rb) = response_body {
        obj.insert("responseBody".into(), Value::String(rb.data));
        obj.insert(
            "responseBodyEncoding".into(),
            Value::String(rb.encoding.into()),
        );
        obj.insert("responseBodyTruncated".into(), Value::Bool(rb.truncated));
    }
    Some(IngestItem {
        service: host.to_string(),
        line: payload.to_string(),
    })
}

pub(crate) fn map_network_failed(
    pending: &PendingNetwork,
    error_text: &str,
    canceled: bool,
    page_url: &str,
    host: &str,
) -> Option<IngestItem> {
    let mut payload = json!({
        "source": "browser",
        "kind": "network",
        "browser": "chrome",
        "requestId": pending.request_id,
        "method": pending.method,
        "url": pending.url,
        "status": pending.status,
        "mimeType": pending.mime_type,
        "resourceType": pending.resource_type,
        "requestHeaders": pending.request_headers,
        "responseHeaders": pending.response_headers.clone().unwrap_or(Value::Object(Map::new())),
        "errorText": error_text,
        "canceled": canceled,
        "pageUrl": page_url,
        "host": host,
        "hostname": host_only(host),
    });
    let obj = payload.as_object_mut()?;
    if let Some(rb) = &pending.request_body {
        obj.insert("requestBody".into(), Value::String(rb.data.clone()));
        obj.insert(
            "requestBodyEncoding".into(),
            Value::String(rb.encoding.into()),
        );
        obj.insert("requestBodyTruncated".into(), Value::Bool(rb.truncated));
    }
    Some(IngestItem {
        service: host.to_string(),
        line: payload.to_string(),
    })
}

pub(crate) fn resolve_service(override_svc: Option<&str>, host: &str) -> String {
    if let Some(s) = override_svc {
        let t = s.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    let t = host.trim();
    if t.is_empty() {
        "browser".into()
    } else {
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn service_from_localhost_with_port() {
        assert_eq!(
            service_from_page_url("http://localhost:5173/dashboard"),
            "localhost:5173"
        );
    }

    #[test]
    fn service_from_https_default_port() {
        assert_eq!(
            service_from_page_url("https://app.example.com/path"),
            "app.example.com"
        );
    }

    #[test]
    fn service_fallbacks() {
        assert_eq!(service_from_page_url("about:blank"), "browser");
        assert_eq!(service_from_page_url(""), "browser");
        assert_eq!(
            service_from_page_url("chrome://settings"),
            "chrome-internal"
        );
        assert_eq!(service_from_page_url("file:///tmp/x.html"), "file");
    }

    #[test]
    fn network_filter_defaults() {
        assert!(should_emit_network("Fetch", false));
        assert!(should_emit_network("XHR", false));
        assert!(should_emit_network("Document", false));
        assert!(should_emit_network("WebSocket", false));
        assert!(!should_emit_network("Image", false));
        assert!(should_emit_network("Image", true));
        assert!(should_fetch_body("Fetch"));
        assert!(!should_fetch_body("Image"));
        assert!(skip_body_url("data:text/plain,hi"));
        assert!(skip_body_url("blob:https://x/1"));
        assert!(!skip_body_url("https://api.example/v1"));
    }

    #[test]
    fn encode_body_utf8_and_truncate() {
        let small = encode_body_str("hello");
        assert_eq!(small.encoding, "utf8");
        assert!(!small.truncated);
        assert_eq!(small.data, "hello");

        let big = vec![b'a'; BODY_MAX_BYTES + 10];
        let enc = encode_body_bytes(&big);
        assert!(enc.truncated);
        assert_eq!(enc.data.len(), BODY_MAX_BYTES);
        assert_eq!(enc.encoding, "utf8");
    }

    #[test]
    fn encode_body_base64_for_binary() {
        let bytes = [0u8, 1, 2, 255, 0, 3];
        let enc = encode_body_bytes(&bytes);
        assert_eq!(enc.encoding, "base64");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&enc.data)
            .unwrap();
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn resolve_service_override() {
        assert_eq!(resolve_service(Some("web"), "localhost:5173"), "web");
        assert_eq!(resolve_service(None, "localhost:5173"), "localhost:5173");
        assert_eq!(
            resolve_service(Some("  "), "localhost:5173"),
            "localhost:5173"
        );
    }

    #[test]
    fn map_console_log() {
        let params = json!({
            "type": "log",
            "args": [
                {"type": "string", "value": "hello"},
                {"type": "number", "value": 42}
            ],
            "timestamp": 1.5
        });
        let item = map_console_api(&params, "http://localhost:5173/", "localhost:5173").unwrap();
        assert_eq!(item.service, "localhost:5173");
        let v: Value = serde_json::from_str(&item.line).unwrap();
        assert_eq!(v["kind"], "console");
        assert_eq!(v["level"], "log");
        assert_eq!(v["msg"], "hello 42");
        assert_eq!(v["host"], "localhost:5173");
    }

    #[test]
    fn map_console_warning_to_warn() {
        let params = json!({
            "type": "warning",
            "args": [{"type": "string", "value": "careful"}],
            "timestamp": 1
        });
        let item = map_console_api(&params, "https://a.com/", "a.com").unwrap();
        let v: Value = serde_json::from_str(&item.line).unwrap();
        assert_eq!(v["level"], "warn");
    }

    #[test]
    fn map_network_includes_bodies() {
        let pending = PendingNetwork {
            session_id: "s1".into(),
            request_id: "r1".into(),
            method: "POST".into(),
            url: "https://api.example.com/v1".into(),
            resource_type: "Fetch".into(),
            request_headers: json!({"content-type": "application/json"}),
            request_body: Some(encode_body_str(r#"{"a":1}"#)),
            status: Some(201),
            mime_type: Some("application/json".into()),
            response_headers: Some(json!({"content-type": "application/json"})),
            started_at: 1.0,
        };
        let item = map_network_finished(
            &pending,
            Some(encode_body_str(r#"{"id":1}"#)),
            Some(42.5),
            "https://app.example.com/",
            "app.example.com",
        )
        .unwrap();
        assert_eq!(item.service, "app.example.com");
        let v: Value = serde_json::from_str(&item.line).unwrap();
        assert_eq!(v["kind"], "network");
        assert_eq!(v["status"], 201);
        assert_eq!(v["requestBody"], r#"{"a":1}"#);
        assert_eq!(v["responseBody"], r#"{"id":1}"#);
        assert_eq!(v["durationMs"], 42.5);
        assert_eq!(v["host"], "app.example.com");
    }

    #[test]
    fn group_services_for_batch() {
        let items = vec![
            IngestItem {
                service: "a.com".into(),
                line: r#"{"n":1}"#.into(),
            },
            IngestItem {
                service: "b.com".into(),
                line: r#"{"n":2}"#.into(),
            },
            IngestItem {
                service: "a.com".into(),
                line: r#"{"n":3}"#.into(),
            },
        ];
        let mut groups: HashMap<String, Vec<String>> = HashMap::new();
        for item in items {
            let service = resolve_service(None, &item.service);
            groups.entry(service).or_default().push(item.line);
        }
        assert_eq!(groups.get("a.com").unwrap().len(), 2);
        assert_eq!(groups.get("b.com").unwrap().len(), 1);
    }
}
