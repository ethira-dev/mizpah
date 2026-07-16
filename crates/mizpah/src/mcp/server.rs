//! Stdio MCP server exposing hub query tools.

use crate::mcp::client::{HubClient, HubError, DEFAULT_LIMIT};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities,
        ServerInfo,
    },
    schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler,
};
use serde::Deserialize;

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

fn hub_err(err: HubError) -> McpError {
    McpError::internal_error(err.to_string(), None)
}

fn json_result(value: impl serde::Serialize) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string_pretty(&value)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
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
        json_result(resp)
    }

    #[tool(
        description = "Get Mizpah hub stats: entry count, approximate bytes used, max bytes, and per-service counts."
    )]
    async fn get_stats(&self) -> Result<CallToolResult, McpError> {
        let resp = self.hub.get_stats().await.map_err(hub_err)?;
        json_result(resp)
    }

    #[tool(
        description = "List discovered JSON property paths (and sample values with occurrence counts) to help write CEL filters. Optionally scope to a service and/or search with q (matches path or sample value)."
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
        json_result(resp)
    }

    #[tool(
        description = "Search in-memory logs with an optional CEL filter. Prefer specific CEL (level, service, contains) and keep limit small (default 20, max 50). Returns newest-first entries plus hasMore for pagination via cursor."
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
        json_result(resp)
    }

    #[tool(
        description = "Fetch a small window of logs around a given entry id (for stack/context expansion). Defaults to 5 before and 5 after."
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
        json_result(resp)
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
                 Use list_properties to discover fields, get_logs_around to expand context around an id. \
                 If tools fail because the hub is unreachable, tell the user to start a stream: \
                 `my-app | mizpah` or `my-app | mizpah --service <name>`."
                    .to_string(),
            )
    }
}
