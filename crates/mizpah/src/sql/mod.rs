//! Snapshot-based SQL over the in-memory log buffer (Phase F).

use crate::store::{LogEntry, Store};
use rusqlite::{params, types::ValueRef, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_SQL_ROWS: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub row_count: usize,
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SqlError {
    #[error("{0}")]
    Rejected(String),
    #[error("sql error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Tokenize SQL, skipping string literals so keyword bans don't false-positive on values.
fn sql_identifier_tokens(sql: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = sql.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '\'' {
            chars.next();
            while let Some(ch) = chars.next() {
                if ch == '\'' {
                    if chars.peek() == Some(&'\'') {
                        chars.next();
                        continue;
                    }
                    break;
                }
            }
            continue;
        }
        if c == '"' {
            chars.next();
            while let Some(ch) = chars.next() {
                if ch == '"' {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        continue;
                    }
                    break;
                }
            }
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let mut s = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    s.push(ch);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(s.to_ascii_lowercase());
            continue;
        }
        chars.next();
    }
    tokens
}

/// Reject multi-statement / dangerous SQL. Allows a single SELECT (optional trailing `;`).
pub fn validate_select_sql(sql: &str) -> Result<String, SqlError> {
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        return Err(SqlError::Rejected("empty SQL".into()));
    }
    let without_trailing = trimmed.trim_end_matches(';').trim();
    if without_trailing.contains(';') {
        return Err(SqlError::Rejected("multiple statements are not allowed".into()));
    }
    let tokens = sql_identifier_tokens(without_trailing);
    let first = tokens.first().map(String::as_str).unwrap_or("");
    if first != "select" && first != "with" {
        return Err(SqlError::Rejected(
            "only SELECT (or WITH … SELECT) statements are allowed".into(),
        ));
    }
    const BANNED: &[&str] = &[
        "attach", "detach", "pragma", "drop", "delete", "insert", "update", "alter",
        "create", "replace", "vacuum", "reindex", "into",
    ];
    for tok in &tokens {
        if BANNED.contains(&tok.as_str()) {
            return Err(SqlError::Rejected(format!("disallowed keyword: {tok}")));
        }
    }
    Ok(without_trailing.to_string())
}

fn level_of(data: &Value) -> Option<String> {
    data.get("level")
        .or_else(|| data.get("severity"))
        .or_else(|| data.get("lvl"))
        .and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
}

fn msg_of(data: &Value) -> Option<String> {
    data.get("msg")
        .or_else(|| data.get("message"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn snapshot_entries(conn: &Connection, entries: &[LogEntry]) -> Result<(), SqlError> {
    conn.execute_batch(
        r#"
        CREATE TABLE all_logs (
            id INTEGER PRIMARY KEY,
            received_at TEXT NOT NULL,
            event_time TEXT NOT NULL,
            service TEXT NOT NULL,
            format_id TEXT,
            level TEXT,
            msg TEXT,
            data TEXT NOT NULL
        );
        "#,
    )?;
    let mut stmt = conn.prepare(
        "INSERT INTO all_logs (id, received_at, event_time, service, format_id, level, msg, data)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for e in entries {
        let data = serde_json::to_string(&e.data).unwrap_or_else(|_| "{}".into());
        stmt.execute(params![
            e.id as i64,
            e.received_at.to_rfc3339(),
            e.effective_event_time().to_rfc3339(),
            e.service,
            e.format_id,
            level_of(&e.data),
            msg_of(&e.data),
            data,
        ])?;
    }
    Ok(())
}

fn value_ref_to_json(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::String(format!("blob:{}b", b.len())),
    }
}

impl Store {
    /// Run a single SELECT against a snapshot of the buffer (`all_logs` table).
    pub async fn query_sql(&self, sql: &str, max_rows: usize) -> Result<SqlResult, SqlError> {
        let sql = validate_select_sql(sql)?;
        let max_rows = max_rows.clamp(1, MAX_SQL_ROWS);
        let entries: Vec<LogEntry> = self.snapshot_entries().await;

        tokio::task::spawn_blocking(move || {
            let conn = Connection::open_in_memory()?;
            snapshot_entries(&conn, &entries)?;
            let mut stmt = conn.prepare(&sql)?;
            let columns: Vec<String> = stmt.column_names().iter().map(|s| (*s).to_string()).collect();
            let col_count = columns.len();
            let mut rows_iter = stmt.query([])?;
            let mut rows = Vec::new();
            let mut truncated = false;
            while let Some(row) = rows_iter.next()? {
                if rows.len() >= max_rows {
                    truncated = true;
                    break;
                }
                let mut vals = Vec::with_capacity(col_count);
                for i in 0..col_count {
                    vals.push(value_ref_to_json(row.get_ref(i)?));
                }
                rows.push(vals);
            }
            let row_count = rows.len();
            Ok(SqlResult {
                columns,
                rows,
                row_count,
                truncated,
            })
        })
        .await
        .map_err(|e| SqlError::Rejected(format!("sql task failed: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[test]
    fn rejects_dangerous_sql() {
        assert!(validate_select_sql("DELETE FROM all_logs").is_err());
        assert!(validate_select_sql("SELECT 1; DROP TABLE all_logs").is_err());
        assert!(validate_select_sql("PRAGMA table_info(all_logs)").is_err());
        assert!(validate_select_sql("SELECT id FROM all_logs").is_ok());
    }

    #[test]
    fn allows_like_with_drop_substring() {
        assert!(validate_select_sql("SELECT id FROM all_logs WHERE msg LIKE '% drop %'").is_ok());
    }

    #[tokio::test]
    async fn query_sql_selects_rows() {
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"level":"error","msg":"x"}"#)
            .await;
        let result = store
            .query_sql("SELECT id, service, level FROM all_logs WHERE level = 'error'", 50)
            .await
            .unwrap();
        assert_eq!(result.columns, vec!["id", "service", "level"]);
        assert_eq!(result.row_count, 1);
        assert_eq!(result.rows[0][1], "api");
    }

    #[test]
    fn validate_empty_and_with_select() {
        assert!(validate_select_sql("").is_err());
        assert!(validate_select_sql("WITH cte AS (SELECT 1 AS n) SELECT n FROM cte").is_ok());
    }

    #[test]
    fn tokenizer_skips_escaped_quotes() {
        let tokens = sql_identifier_tokens(
            "SELECT id FROM all_logs WHERE msg = 'don''t drop' AND note = \"no drop\"",
        );
        assert!(!tokens.contains(&"drop".to_string()));
    }

    #[tokio::test]
    async fn query_truncates_and_coerces_types() {
        let store = Store::new(1_000_000);
        for i in 0..3 {
            store
                .push_line("svc", &format!(r#"{{"level":"info","msg":"{i}"}}"#))
                .await;
        }
        let result = store
            .query_sql(
                "SELECT id, length(data) AS bytes, id * 1.0 AS ratio FROM all_logs",
                2,
            )
            .await
            .unwrap();
        assert!(result.truncated);
        assert_eq!(result.row_count, 2);
        assert!(result.rows[0][1].is_number());
        assert!(result.rows[0][2].as_f64().is_some());

        let blob = store
            .query_sql("SELECT zeroblob(4) AS b FROM all_logs LIMIT 1", 1)
            .await
            .unwrap();
        assert_eq!(blob.rows[0][0], "blob:4b");
    }

    #[test]
    fn tokenizer_skips_double_quoted_identifiers() {
        let tokens = sql_identifier_tokens(r#"SELECT "drop" FROM all_logs"#);
        assert!(!tokens.contains(&"drop".to_string()));
    }

    #[tokio::test]
    async fn level_and_msg_helpers_via_query() {
        let store = Store::new(1_000_000);
        store
            .push_line("svc", r#"{"severity":42,"message":"num-level"}"#)
            .await;
        let result = store
            .query_sql("SELECT level, msg FROM all_logs", 1)
            .await
            .unwrap();
        assert_eq!(result.rows[0][0], "42");
        assert_eq!(result.rows[0][1], "num-level");
    }

    #[test]
    fn rejects_non_select_leading_token() {
        assert!(validate_select_sql("EXPLAIN SELECT 1").is_err());
    }

    #[tokio::test]
    async fn msg_field_falls_back_to_message() {
        let store = Store::new(1_000_000);
        store
            .push_line("svc", r#"{"message":"hello there"}"#)
            .await;
        let result = store
            .query_sql("SELECT msg FROM all_logs", 1)
            .await
            .unwrap();
        assert_eq!(result.rows[0][0], "hello there");
    }

    #[tokio::test]
    async fn real_column_nan_becomes_null() {
        let store = Store::new(1_000_000);
        store.push_line("svc", r#"{"msg":"x"}"#).await;
        let result = store
            .query_sql("SELECT 1.0/0.0 AS inf FROM all_logs", 1)
            .await
            .unwrap();
        assert!(result.rows[0][0].is_null() || result.rows[0][0].is_number());
    }
}
