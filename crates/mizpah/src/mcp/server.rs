//! Stdio MCP server exposing hub query tools.

use crate::mcp::client::{HubClient, HubClientError, DEFAULT_LIMIT};
use crate::mcp::format::{format_logs, format_properties, format_value};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities,
        ServerInfo,
    },
    schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler,
};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct MizpahMcp {
    hub: HubClient,
    #[allow(dead_code)] // used by #[tool_handler] generated code
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchLogsArgs {
    /// CEL filter expression (e.g. `level == "error"`). Empty matches all.
    #[serde(default)]
    pub q: Option<String>,
    /// Optional service name filter.
    #[serde(default)]
    pub service: Option<String>,
    /// Max entries to return (default 20, max 50).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Pagination cursor: return entries with id strictly less than this.
    #[serde(default)]
    pub cursor: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListPropertiesArgs {
    /// Optional service to scope discovered property paths.
    #[serde(default)]
    pub service: Option<String>,
    /// Optional case-insensitive search over property paths and sample values.
    #[serde(default)]
    pub q: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetLogsAroundArgs {
    /// Log entry id to center the window on.
    pub id: u64,
    /// How many older entries to include (default 5).
    #[serde(default)]
    pub before: Option<usize>,
    /// How many newer entries to include (default 5).
    #[serde(default)]
    pub after: Option<usize>,
    /// Optional service name filter.
    #[serde(default)]
    pub service: Option<String>,
    /// Optional CEL filter.
    #[serde(default)]
    pub q: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AggregateLogsArgs {
    /// CEL pre-filter (optional).
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub service: Option<String>,
    /// Comma-separated or list of group-by paths (default: service).
    #[serde(default)]
    pub group_by: Option<Vec<String>>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetTraceArgs {
    /// Trace / request / correlation id.
    pub opid: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct QuerySqlArgs {
    /// Single SELECT against `all_logs` snapshot.
    pub sql: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NavLevelArgs {
    /// Start from this entry id (use current selection).
    pub from_id: u64,
    /// `next` or `prev` (default next).
    #[serde(default)]
    pub direction: Option<String>,
    /// Comma-separated levels (default error,warn).
    #[serde(default)]
    pub levels: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListTracesArgs {
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SpectrogramArgs {
    /// Field path to heat-map (default level).
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub time_buckets: Option<usize>,
}

fn hub_err(err: HubClientError) -> McpError {
    McpError::internal_error(err.to_string(), None)
}

fn toon_result(value: impl Serialize) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(
        format_value(&value),
    )]))
}

#[tool_router]
impl MizpahMcp {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            hub: HubClient::new(base_url),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "List service names currently present in the Mizpah in-memory log buffer."
    )]
    async fn list_services(&self) -> Result<CallToolResult, McpError> {
        let resp = self.hub.list_services().await.map_err(hub_err)?;
        toon_result(resp)
    }

    #[tool(
        description = "Get Mizpah hub stats: entry count, approximate bytes used, max bytes, and per-service counts."
    )]
    async fn get_stats(&self) -> Result<CallToolResult, McpError> {
        let resp = self.hub.get_stats().await.map_err(hub_err)?;
        toon_result(resp)
    }

    #[tool(
        description = "List discovered JSON property paths (and sample values with occurrence counts) to help write CEL filters. Optionally scope to a service and/or search with q (matches path or sample value). Results are TOON (token-efficient)."
    )]
    async fn list_properties(
        &self,
        Parameters(args): Parameters<ListPropertiesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .hub
            .list_properties(args.service.as_deref(), args.q.as_deref())
            .await
            .map_err(hub_err)?;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            format_properties(resp),
        )]))
    }

    #[tool(
        description = "Search in-memory logs with an optional CEL filter. Prefer specific CEL (level, service, contains) and keep limit small (default 20, max 50). Returns newest-first entries plus hasMore for pagination via cursor. Results are TOON (token-efficient); `_mzp` is omitted."
    )]
    async fn search_logs(
        &self,
        Parameters(args): Parameters<SearchLogsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .hub
            .search_logs(
                args.service.as_deref(),
                args.q.as_deref(),
                args.limit.or(Some(DEFAULT_LIMIT)),
                args.cursor,
            )
            .await
            .map_err(hub_err)?;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            format_logs(resp),
        )]))
    }

    #[tool(
        description = "Fetch a small window of logs around a given entry id (for stack/context expansion). Defaults to 5 before and 5 after. Results are TOON (token-efficient); `_mzp` is omitted."
    )]
    async fn get_logs_around(
        &self,
        Parameters(args): Parameters<GetLogsAroundArgs>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .hub
            .get_logs_around(
                args.id,
                args.before.unwrap_or(5),
                args.after.unwrap_or(5),
                args.service.as_deref(),
                args.q.as_deref(),
            )
            .await
            .map_err(hub_err)?;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            format_logs(resp),
        )]))
    }

    #[tool(
        description = "Aggregate in-memory logs (GROUP BY). Use for top-N counts by service/level/field. Keep limit small (default 20, max 50). Prefer over dumping rows. Results are TOON."
    )]
    async fn aggregate_logs(
        &self,
        Parameters(args): Parameters<AggregateLogsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let group_by = args.group_by.unwrap_or_else(|| vec!["service".into()]);
        let resp = self
            .hub
            .aggregate_logs(
                args.service.as_deref(),
                args.q.as_deref(),
                &group_by,
                args.limit.or(Some(DEFAULT_LIMIT)),
            )
            .await
            .map_err(hub_err)?;
        toon_result(resp)
    }

    #[tool(
        description = "Fetch all buffered logs for a trace/request/correlation id (oldest-first). Hard-capped. Prefer this over many search_logs calls when correlating one request."
    )]
    async fn get_trace(
        &self,
        Parameters(args): Parameters<GetTraceArgs>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .hub
            .get_trace(&args.opid, args.limit.or(Some(DEFAULT_LIMIT)))
            .await
            .map_err(hub_err)?;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            format_logs(resp),
        )]))
    }

    #[tool(
        description = "Run a single SELECT against a snapshot table `all_logs` (columns: id, received_at, event_time, service, format_id, level, msg, data). Max 50 rows via MCP. Prefer CEL search_logs for simple filters."
    )]
    async fn query_sql(
        &self,
        Parameters(args): Parameters<QuerySqlArgs>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .hub
            .query_sql(&args.sql, args.limit.or(Some(DEFAULT_LIMIT)))
            .await
            .map_err(hub_err)?;
        toon_result(resp)
    }

    #[tool(
        description = "List bookmarks / tags / comments on buffered log entries. Results are TOON."
    )]
    async fn list_bookmarks(&self) -> Result<CallToolResult, McpError> {
        let resp = self.hub.list_bookmarks().await.map_err(hub_err)?;
        toon_result(resp)
    }

    #[tool(
        description = "List distinct traces currently in the buffer (opid, counts, time range). Keep limit small."
    )]
    async fn list_traces(
        &self,
        Parameters(args): Parameters<ListTracesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .hub
            .list_traces(args.limit.or(Some(DEFAULT_LIMIT)))
            .await
            .map_err(hub_err)?;
        toon_result(resp)
    }

    #[tool(
        description = "Navigate to the next/previous error or warn in the buffer (hub-wide, not just loaded page)."
    )]
    async fn nav_level(
        &self,
        Parameters(args): Parameters<NavLevelArgs>,
    ) -> Result<CallToolResult, McpError> {
        let direction = args.direction.as_deref().unwrap_or("next");
        let levels: Vec<&str> = args
            .levels
            .as_deref()
            .unwrap_or("error,warn")
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        let entry = self
            .hub
            .nav_level(args.from_id, direction, &levels)
            .await
            .map_err(hub_err)?;
        toon_result(serde_json::json!({ "entry": entry }))
    }

    #[tool(
        description = "Time × field heat-map (spectrogram) over the buffer. Default field=level. Results are TOON."
    )]
    async fn spectrogram(
        &self,
        Parameters(args): Parameters<SpectrogramArgs>,
    ) -> Result<CallToolResult, McpError> {
        let field = args.field.as_deref().unwrap_or("level");
        let resp = self
            .hub
            .spectrogram(field, args.time_buckets)
            .await
            .map_err(hub_err)?;
        toon_result(resp)
    }
}

#[tool_handler]
impl ServerHandler for MizpahMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(
                "Mizpah exposes the live in-memory JSON log hub. \
                 Prefer CEL filters via search_logs (e.g. level == \"error\", msg.contains(\"timeout\")). \
                 Keep limits small (default 20, max 50) — never dump the full buffer. \
                 Tool results are TOON (Token-Oriented Object Notation), not pretty JSON — denser for context. \
                 MCP log rows omit `_mzp` receiver metadata. \
                 Use list_properties to discover fields, get_logs_around to expand context around an id. \
                 Use aggregate_logs for top-N counts, get_trace / list_traces for request correlation, query_sql for SELECT analytics, \
                 list_bookmarks / spectrogram / nav_level for bookmarks, heat-maps, and error navigation. \
                 If tools fail because the hub is unreachable, tell the user to start a stream: \
                 `my-app | mizpah` or `my-app | mizpah --service <name>`."
                    .to_string(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // HubClient builds a reqwest Client (TLS crypto FFI — unsupported under Miri).
    #[cfg(not(miri))]
    #[test]
    fn mcp_server_constructs_with_tool_router() {
        crate::util::ensure_rustls_crypto_provider();
        let mcp = MizpahMcp::new("http://127.0.0.1:3149");
        let info = mcp.get_info();
        let instructions = info.instructions.unwrap_or_default();
        assert!(!instructions.is_empty());
        assert!(
            instructions.contains("TOON"),
            "server instructions should mention TOON results"
        );
        let _ = mcp.tool_router;
    }

    #[cfg(not(miri))]
    use crate::test_support::spawn_test_hub;
    #[cfg(not(miri))]
    use rmcp::handler::server::wrapper::Parameters;

    #[cfg(not(miri))]
    fn text_content(block: &ContentBlock) -> &str {
        match block {
            ContentBlock::Text(t) => &t.text,
            _ => panic!("expected text content"),
        }
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn tool_list_services() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"msg":"test"}"#).await;
        store.push_line("backend", r#"{"msg":"test"}"#).await;

        let mcp = MizpahMcp::new(url);
        let result = mcp.list_services().await.unwrap();
        assert_eq!(result.content.len(), 1);
        let text = text_content(&result.content[0]);
        assert!(text.contains("api") || text.contains("backend"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn tool_get_stats() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"msg":"test"}"#).await;

        let mcp = MizpahMcp::new(url);
        let result = mcp.get_stats().await.unwrap();
        assert_eq!(result.content.len(), 1);
        let text = text_content(&result.content[0]);
        assert!(text.contains("entries") || !text.is_empty());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn tool_list_properties() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"error","msg":"boom"}"#)
            .await;

        let mcp = MizpahMcp::new(url);
        let args = ListPropertiesArgs {
            service: Some("api".into()),
            q: None,
        };
        let result = mcp.list_properties(Parameters(args)).await.unwrap();
        assert_eq!(result.content.len(), 1);
        let text = text_content(&result.content[0]);
        assert!(text.contains("level") || text.contains("msg"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn tool_search_logs() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"error","msg":"boom"}"#)
            .await;
        store
            .push_line("api", r#"{"level":"info","msg":"ok"}"#)
            .await;

        let mcp = MizpahMcp::new(url);
        let args = SearchLogsArgs {
            q: Some(r#"level == "error""#.into()),
            service: Some("api".into()),
            limit: Some(10),
            cursor: None,
        };
        let result = mcp.search_logs(Parameters(args)).await.unwrap();
        assert_eq!(result.content.len(), 1);
        let text = text_content(&result.content[0]);
        assert!(text.contains("boom"));
        assert!(!text.contains("ok"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn tool_get_logs_around() {
        let (url, store) = spawn_test_hub().await;
        for i in 0..10 {
            store.push_line("api", &format!(r#"{{"n":{i}}}"#)).await;
        }

        let mcp = MizpahMcp::new(url);

        // Get all logs to find a middle ID
        let search_args = SearchLogsArgs {
            q: None,
            service: None,
            limit: Some(50),
            cursor: None,
        };
        let all = mcp.search_logs(Parameters(search_args)).await.unwrap();
        let text = text_content(&all.content[0]);

        // Extract an ID from the response (parse the TOON format)
        let lines: Vec<&str> = text.lines().collect();
        let id_line = lines.iter().find(|l| l.contains("id:")).unwrap();
        let id: u64 = id_line.split(':').nth(1).unwrap().trim().parse().unwrap();

        let args = GetLogsAroundArgs {
            id,
            before: Some(2),
            after: Some(2),
            service: None,
            q: None,
        };
        let result = mcp.get_logs_around(Parameters(args)).await.unwrap();
        assert_eq!(result.content.len(), 1);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn tool_aggregate_logs() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"level":"error"}"#).await;
        store.push_line("api", r#"{"level":"error"}"#).await;
        store.push_line("api", r#"{"level":"info"}"#).await;

        let mcp = MizpahMcp::new(url);
        let args = AggregateLogsArgs {
            q: None,
            service: Some("api".into()),
            group_by: Some(vec!["level".into()]),
            limit: Some(10),
        };
        let result = mcp.aggregate_logs(Parameters(args)).await.unwrap();
        assert_eq!(result.content.len(), 1);
        let text = text_content(&result.content[0]);
        assert!(text.contains("error") || text.contains("info"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn tool_get_trace() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"opid":"req-123","msg":"start"}"#)
            .await;
        store
            .push_line("api", r#"{"opid":"req-123","msg":"end"}"#)
            .await;

        let mcp = MizpahMcp::new(url);
        let args = GetTraceArgs {
            opid: "req-123".into(),
            limit: Some(50),
        };
        let result = mcp.get_trace(Parameters(args)).await.unwrap();
        assert_eq!(result.content.len(), 1);
        let text = text_content(&result.content[0]);
        assert!(text.contains("start") && text.contains("end"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn tool_query_sql() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"error","msg":"boom"}"#)
            .await;

        let mcp = MizpahMcp::new(url);
        let args = QuerySqlArgs {
            sql: "SELECT service, level, msg FROM all_logs WHERE level = 'error'".into(),
            limit: Some(10),
        };
        let result = mcp.query_sql(Parameters(args)).await.unwrap();
        assert_eq!(result.content.len(), 1);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn tool_list_bookmarks() {
        let (url, _store) = spawn_test_hub().await;

        let mcp = MizpahMcp::new(url);
        let result = mcp.list_bookmarks().await.unwrap();
        assert_eq!(result.content.len(), 1);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn tool_list_traces() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"opid":"req-123","msg":"test"}"#)
            .await;

        let mcp = MizpahMcp::new(url);
        let args = ListTracesArgs { limit: Some(10) };
        let result = mcp.list_traces(Parameters(args)).await.unwrap();
        assert_eq!(result.content.len(), 1);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn tool_nav_level() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"info","msg":"ok"}"#)
            .await;
        store
            .push_line("api", r#"{"level":"error","msg":"boom"}"#)
            .await;

        let mcp = MizpahMcp::new(url);

        // Get first entry ID
        let search_args = SearchLogsArgs {
            q: None,
            service: None,
            limit: Some(1),
            cursor: None,
        };
        let first = mcp.search_logs(Parameters(search_args)).await.unwrap();
        let text = text_content(&first.content[0]);
        let lines: Vec<&str> = text.lines().collect();
        let id_line = lines.iter().find(|l| l.contains("id:")).unwrap();
        let id: u64 = id_line.split(':').nth(1).unwrap().trim().parse().unwrap();

        let args = NavLevelArgs {
            from_id: id,
            direction: Some("next".into()),
            levels: Some("error".into()),
        };
        let result = mcp.nav_level(Parameters(args)).await.unwrap();
        assert_eq!(result.content.len(), 1);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn tool_spectrogram() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"level":"error"}"#).await;
        store.push_line("api", r#"{"level":"info"}"#).await;

        let mcp = MizpahMcp::new(url);
        let args = SpectrogramArgs {
            field: Some("level".into()),
            time_buckets: Some(10),
        };
        let result = mcp.spectrogram(Parameters(args)).await.unwrap();
        assert_eq!(result.content.len(), 1);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn tool_search_logs_empty_query() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"msg":"test"}"#).await;

        let mcp = MizpahMcp::new(url);
        let args = SearchLogsArgs {
            q: None,
            service: None,
            limit: None,
            cursor: None,
        };
        let result = mcp.search_logs(Parameters(args)).await.unwrap();
        assert_eq!(result.content.len(), 1);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn tool_aggregate_default_group_by() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"msg":"test"}"#).await;

        let mcp = MizpahMcp::new(url);
        let args = AggregateLogsArgs {
            q: None,
            service: None,
            group_by: None,
            limit: None,
        };
        let result = mcp.aggregate_logs(Parameters(args)).await.unwrap();
        assert_eq!(result.content.len(), 1);
    }
}
