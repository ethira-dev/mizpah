//! Unit tests for every vendored format pack.

use super::*;
use serde_json::{json, Value};
use std::collections::HashSet;

/// Synthetic samples for JSON packs that ship without `sample` lines upstream.
fn json_fixture(pack_id: &str) -> Option<String> {
    let v: Value = match pack_id {
        "bunyan_log" => json!({
            "v": 0,
            "level": 30,
            "name": "app",
            "hostname": "h",
            "pid": 1,
            "time": "2020-01-01T00:00:00.000Z",
            "msg": "hello"
        }),
        "pino_log" => json!({
            "level": 30,
            "time": 1577836800000u64,
            "pid": 1,
            "hostname": "h",
            "msg": "hello"
        }),
        "journald_json_log" => json!({
            "MESSAGE": "unit failed",
            "PRIORITY": "3",
            "__REALTIME_TIMESTAMP": "1577836800000000"
        }),
        "ecs_log" => json!({
            "@timestamp": "2020-01-01T00:00:00.000Z",
            "log.level": "error",
            "message": "boom",
            "ecs.version": "1.0.0"
        }),
        "caddy_log" => json!({
            "ts": 1577836800.0,
            "status": 200,
            "request": {
                "method": "GET",
                "host": "example.com",
                "uri": "/",
                "client_ip": "127.0.0.1",
                "proto": "HTTP/1.1"
            }
        }),
        "cloudflare_json_log" => json!({
            "EdgeEndTimestamp": "2020-01-01T00:00:00Z",
            "CacheCacheStatus": "hit",
            "ClientIP": "1.2.3.4",
            "ClientRequestMethod": "GET",
            "ClientRequestURI": "/"
        }),
        "github_events_log" => json!({
            "created_at": "2020-01-01T00:00:00Z",
            "type": "PushEvent",
            "id": "1",
            "actor": {"login": "octocat"},
            "repo": {"name": "octocat/Hello-World"}
        }),
        "macosuni_log" => json!({
            "timestamp": "2020-01-01 00:00:00.000000-0000",
            "messageType": "Error",
            "eventMessage": "failed",
            "processImagePath": "/usr/bin/test"
        }),
        "mongodb_json_log" => json!({
            "t": {"$date": "2020-01-01T00:00:00.000Z"},
            "s": "I",
            "c": "NETWORK",
            "id": 22943,
            "ctx": "listener",
            "msg": "Connection accepted"
        }),
        "nextcloud" => json!({
            "time": "2020-01-01T00:00:00+00:00",
            "level": 3,
            "message": "hello",
            "app": "core",
            "user": "admin"
        }),
        "web_robot_log" => json!({
            "ip": "1.2.3.4",
            "method": "GET",
            "resource": "/index.html",
            "status": 200,
            "user_agent": "bot"
        }),
        _ => return None,
    };
    Some(v.to_string())
}

#[test]
fn registry_loads_all_non_converter_packs() {
    let ids = loaded_pack_ids();
    assert!(
        ids.len() >= 210,
        "expected ~210+ active packs, got {}",
        ids.len()
    );
    assert!(
        ids.iter().any(|id| id == "pcap_log"),
        "pcap_log should be registered (post-convert JSON pack)"
    );
    assert!(
        ids.iter().any(|id| id == "otel_collector_log"),
        "otel_collector_log should be registered"
    );
    assert!(ids.iter().any(|id| id == "windows_evtx_log"));
    assert!(ids.iter().any(|id| id == "vault_audit_log"));
}

#[test]
fn every_pack_compiles_and_has_samples_exercised() {
    let reg = registry();
    let mut exercised: HashSet<String> = HashSet::new();
    let mut failures: Vec<String> = Vec::new();

    for pack in &reg.packs {
        let mut lines: Vec<String> = pack.samples.iter().map(|(l, _)| l.clone()).collect();
        if lines.is_empty() {
            if let Some(fx) = json_fixture(&pack.pack_id) {
                lines.push(fx);
            } else if pack.kind == FormatKind::Json {
                // rust_tracing_log has upstream samples; others need fixtures
                failures.push(format!(
                    "{}: json pack has no samples and no fixture",
                    pack.pack_id
                ));
                continue;
            } else {
                failures.push(format!("{}: text pack has no samples", pack.pack_id));
                continue;
            }
        }

        for (i, line) in lines.iter().enumerate() {
            // Multiline samples: detect/parse against the first line only.
            let line = line.lines().next().unwrap_or(line.as_str());
            match pack.kind {
                FormatKind::Text => {
                    if pack.detect_text(line) < 0.5 {
                        failures.push(format!(
                            "{} sample[{i}]: detect failed: {}",
                            pack.pack_id,
                            &line[..line.len().min(80)]
                        ));
                        continue;
                    }
                    match pack.parse_text(line) {
                        None => failures
                            .push(format!("{} sample[{i}]: parse returned None", pack.pack_id)),
                        Some(norm) => {
                            let expected = stable_format_id(&pack.pack_id);
                            if norm.format_id != expected {
                                failures.push(format!(
                                    "{} sample[{i}]: format_id {} != {}",
                                    pack.pack_id, norm.format_id, expected
                                ));
                            } else {
                                exercised.insert(pack.pack_id.clone());
                            }
                        }
                    }
                }
                FormatKind::Json => {
                    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(line) else {
                        failures.push(format!("{} sample[{i}]: not a JSON object", pack.pack_id));
                        continue;
                    };
                    if pack.json_confidence(&obj) < 0.5 {
                        failures.push(format!(
                            "{} sample[{i}]: json confidence too low",
                            pack.pack_id
                        ));
                        continue;
                    }
                    match pack.parse_json(&obj) {
                        None => {
                            failures.push(format!("{} sample[{i}]: parse_json None", pack.pack_id))
                        }
                        Some(norm) => {
                            let expected = stable_format_id(&pack.pack_id);
                            if norm.format_id != expected {
                                failures.push(format!(
                                    "{} sample[{i}]: format_id {} != {}",
                                    pack.pack_id, norm.format_id, expected
                                ));
                            } else {
                                exercised.insert(pack.pack_id.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    let untested: Vec<_> = reg
        .packs
        .iter()
        .map(|p| p.pack_id.clone())
        .filter(|id| !exercised.contains(id))
        .collect();

    // Allowlist must stay empty for merge.
    const UNTESTED_ALLOWLIST: &[&str] = &[];
    let unexpected: Vec<_> = untested
        .iter()
        .filter(|id| !UNTESTED_ALLOWLIST.contains(&id.as_str()))
        .cloned()
        .collect();

    assert!(
        !(!failures.is_empty() || !unexpected.is_empty()),
        "pack sample failures ({}):\n{}\nuntested packs: {:?}",
        failures.len(),
        failures.join("\n"),
        unexpected
    );

    assert_eq!(
        exercised.len(),
        reg.packs.len(),
        "every loaded pack must be exercised"
    );
}

#[test]
fn parse_with_format_hint_stable_syslog_id() {
    let line = r#"<34>Oct 11 22:14:15 host app: fail"#;
    let norm = parse_with_format_hint(line, "syslog").expect("syslog hint");
    assert_eq!(norm.format_id, "syslog");
}

#[test]
fn parse_with_format_hint_json_pack() {
    let line = r#"{"@timestamp":"2020-01-01T00:00:00.000Z","log.level":"error","message":"boom","ecs.version":"1.0.0"}"#;
    let norm = parse_with_format_hint(line, "ecs_log").expect("ecs json");
    assert_eq!(norm.format_id, "ecs_log");
}

#[test]
fn registry_get_and_text_packs() {
    let reg = registry();
    assert!(reg.get("syslog_log").is_some());
    assert!(reg.text_packs().next().is_some());
    assert!(reg.json_packs().next().is_some());
    assert!(detect_pack_text("not a log line at all").is_none());
}

#[test]
fn stable_ids_for_primary_packs() {
    assert_eq!(stable_format_id("syslog_log"), "syslog");
    assert_eq!(stable_format_id("bunyan_log"), "bunyan");
    assert_eq!(stable_format_id("postgres_log"), "postgres_log");
}

#[test]
fn match_keys_reject_partial_json() {
    use crate::formats::classify_pack_json;
    // Looks Vault-ish (has time) but missing auth/request — must not win vault_audit_log.
    let obj = serde_json::json!({
        "time": "2020-01-01T00:00:00Z",
        "type": "request",
        "data": {"x": 1}
    });
    let Value::Object(map) = obj else { panic!("obj") };
    if let Some(norm) = classify_pack_json(&map) {
        assert_ne!(
            norm.format_id, "vault_audit_log",
            "partial object must not match vault_audit_log"
        );
    }
}

#[test]
fn rust_tracing_sample_via_engine() {
    let reg = registry();
    let pack = reg.get("rust_tracing_log").expect("rust_tracing_log");
    let line = &pack.samples[0].0;
    let norm = pack
        .parse_json(
            &serde_json::from_str::<Value>(line)
                .unwrap()
                .as_object()
                .unwrap()
                .clone(),
        )
        .expect("parse");
    assert_eq!(norm.format_id, "rust_tracing_log");
}
