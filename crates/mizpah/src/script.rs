//! Line-oriented headless scripts (Phase B).

use crate::mcp::HubClient;
use serde_json::Value;
use std::fs;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub enum ScriptCmd {
    Query {
        cel: String,
        limit: Option<usize>,
    },
    Aggregate {
        cel: String,
        group_by: Vec<String>,
        limit: Option<usize>,
    },
}

/// Parse a line-oriented script (`#` comments, `query …`, `aggregate …`).
pub fn parse_script(text: &str) -> Result<Vec<ScriptCmd>, ScriptError> {
    let mut cmds = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (verb, rest) = line
            .split_once(char::is_whitespace)
            .map_or((line, ""), |(a, b)| (a, b.trim()));
        match verb.to_ascii_lowercase().as_str() {
            "query" => {
                let (cel, limit) = parse_cel_and_limit(rest);
                cmds.push(ScriptCmd::Query { cel, limit });
            }
            "aggregate" => {
                let (cel, group_by, limit) = parse_aggregate(rest)?;
                cmds.push(ScriptCmd::Aggregate {
                    cel,
                    group_by,
                    limit,
                });
            }
            other => {
                return Err(ScriptError::Message(format!(
                    "line {}: unknown command {other:?}",
                    i + 1
                )));
            }
        }
    }
    Ok(cmds)
}

fn parse_cel_and_limit(rest: &str) -> (String, Option<usize>) {
    // `query level == "error" --limit 20` or `query --limit 20 level == "error"`
    let mut limit = None;
    let mut parts = Vec::new();
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i] == "--limit" || tokens[i] == "-n" {
            if let Some(n) = tokens.get(i + 1).and_then(|s| s.parse().ok()) {
                limit = Some(n);
                i += 2;
                continue;
            }
        }
        parts.push(tokens[i]);
        i += 1;
    }
    (parts.join(" "), limit)
}

fn parse_aggregate(rest: &str) -> Result<(String, Vec<String>, Option<usize>), ScriptError> {
    // aggregate --group-by level,service --limit 10 level == "error"
    let mut limit = None;
    let mut group_by = Vec::new();
    let mut parts = Vec::new();
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i] == "--limit" || tokens[i] == "-n" {
            if let Some(n) = tokens.get(i + 1).and_then(|s| s.parse().ok()) {
                limit = Some(n);
                i += 2;
                continue;
            }
        }
        if tokens[i] == "--group-by" || tokens[i] == "-g" {
            if let Some(g) = tokens.get(i + 1) {
                group_by = g
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                i += 2;
                continue;
            }
        }
        parts.push(tokens[i]);
        i += 1;
    }
    Ok((parts.join(" "), group_by, limit))
}

/// Run a script file against a hub URL; prints JSON results to stdout.
pub async fn run_script(path: &Path, hub_url: &str) -> Result<(), ScriptError> {
    let text = fs::read_to_string(path)?;
    let cmds = parse_script(&text)?;
    let hub = HubClient::new(hub_url.trim_end_matches('/'));
    for cmd in cmds {
        match cmd {
            ScriptCmd::Query { cel, limit } => {
                let q = if cel.is_empty() {
                    None
                } else {
                    Some(cel.as_str())
                };
                let resp = hub
                    .search_logs(None, q, limit, None)
                    .await
                    .map_err(|e| ScriptError::Message(e.to_string()))?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&resp).unwrap_or_default()
                );
            }
            ScriptCmd::Aggregate {
                cel,
                group_by,
                limit,
            } => {
                let q = if cel.is_empty() {
                    None
                } else {
                    Some(cel.as_str())
                };
                let resp = hub
                    .aggregate_logs(None, q, &group_by, limit)
                    .await
                    .map_err(|e| ScriptError::Message(e.to_string()))?;
                let v: Value = resp;
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::spawn_test_hub;

    #[test]
    fn parses_query_and_comments() {
        let cmds = parse_script(
            r#"
# find errors
query level == "error" --limit 5
aggregate --group-by level level != ""
"#,
        )
        .unwrap();
        assert_eq!(cmds.len(), 2);
        match &cmds[0] {
            ScriptCmd::Query { cel, limit } => {
                assert!(cel.contains("error"));
                assert_eq!(*limit, Some(5));
            }
            _ => panic!("expected query"),
        }
        match &cmds[1] {
            ScriptCmd::Aggregate {
                cel,
                group_by,
                limit,
            } => {
                assert!(cel.contains("level"));
                assert_eq!(group_by, &["level"]);
                assert_eq!(*limit, None);
            }
            _ => panic!("expected aggregate"),
        }
    }

    #[test]
    fn parse_query_limit_before_cel_and_short_flag() {
        let cmds = parse_script("query --limit 3 level == \"warn\"\nquery -n 7 x").unwrap();
        assert_eq!(cmds.len(), 2);
        match &cmds[0] {
            ScriptCmd::Query { cel, limit } => {
                assert_eq!(cel, "level == \"warn\"");
                assert_eq!(*limit, Some(3));
            }
            _ => panic!("expected query"),
        }
        match &cmds[1] {
            ScriptCmd::Query { cel, limit } => {
                assert_eq!(cel, "x");
                assert_eq!(*limit, Some(7));
            }
            _ => panic!("expected query"),
        }
    }

    #[test]
    fn parse_aggregate_with_group_short_flag_and_limit() {
        let cmds = parse_script("aggregate -g service,level --limit 2 status >= 400").unwrap();
        assert_eq!(cmds.len(), 1);
        match &cmds[0] {
            ScriptCmd::Aggregate {
                cel,
                group_by,
                limit,
            } => {
                assert_eq!(cel, "status >= 400");
                assert_eq!(group_by, &["service", "level"]);
                assert_eq!(*limit, Some(2));
            }
            _ => panic!("expected aggregate"),
        }
    }

    #[test]
    fn parse_unknown_command_reports_line_number() {
        let err = parse_script("noop something").unwrap_err();
        assert!(err.to_string().contains("line 1"));
        assert!(err.to_string().contains("noop"));
    }

    #[test]
    fn parse_skips_blank_lines_and_hash_comments() {
        let cmds = parse_script("\n# comment\n\nquery true\n").unwrap();
        assert_eq!(cmds.len(), 1);
    }

    #[test]
    fn parse_single_word_query_has_empty_cel() {
        let cmds = parse_script("query").unwrap();
        match &cmds[0] {
            ScriptCmd::Query { cel, limit } => {
                assert!(cel.is_empty());
                assert_eq!(*limit, None);
            }
            _ => panic!("expected query"),
        }
    }

    #[tokio::test]
    async fn run_script_executes_query_and_aggregate() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"error","msg":"boom"}"#)
            .await;
        store
            .push_line("api", r#"{"level":"info","msg":"ok"}"#)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.mzp");
        fs::write(
            &path,
            r#"
query level == "error" --limit 5
aggregate --group-by level level != ""
"#,
        )
        .unwrap();

        run_script(&path, &url).await.unwrap();
    }

    #[tokio::test]
    async fn run_script_missing_file_errors() {
        let err = run_script(Path::new("/no/such/script.mzp"), "http://127.0.0.1:1")
            .await
            .unwrap_err();
        assert!(matches!(err, ScriptError::Io(_)));
    }

    #[test]
    fn parse_aggregate_group_by_only_and_short_limit() {
        let cmds = parse_script("aggregate -g level,service -n 9").unwrap();
        match &cmds[0] {
            ScriptCmd::Aggregate {
                cel,
                group_by,
                limit,
            } => {
                assert!(cel.is_empty());
                assert_eq!(group_by, &["level", "service"]);
                assert_eq!(*limit, Some(9));
            }
            _ => panic!("expected aggregate"),
        }
    }

    #[test]
    fn parse_aggregate_skips_empty_group_segments() {
        let cmds = parse_script("aggregate --group-by level,,service x").unwrap();
        match &cmds[0] {
            ScriptCmd::Aggregate { group_by, cel, .. } => {
                assert_eq!(group_by, &["level", "service"]);
                assert_eq!(cel, "x");
            }
            _ => panic!("expected aggregate"),
        }
    }

    #[test]
    fn script_error_message_display() {
        let err = ScriptError::Message("hub down".into());
        assert_eq!(err.to_string(), "hub down");
    }

    #[tokio::test]
    async fn run_script_empty_query_matches_all() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"level":"info"}"#).await;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("all.mzp");
        fs::write(&path, "query\n").unwrap();
        run_script(&path, &url).await.unwrap();
    }

    #[test]
    fn parse_limit_without_numeric_value_is_ignored() {
        let cmds = parse_script("query --limit not-a-number x == 1").unwrap();
        match &cmds[0] {
            ScriptCmd::Query { cel, limit } => {
                assert_eq!(*limit, None);
                assert!(cel.contains("not-a-number"));
            }
            _ => panic!("expected query"),
        }
    }

    #[tokio::test]
    async fn run_script_hub_unreachable_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.mzp");
        fs::write(&path, "query level == \"error\"\n").unwrap();
        let err = run_script(&path, "http://127.0.0.1:1").await.unwrap_err();
        assert!(matches!(err, ScriptError::Message(_)));
    }
}
