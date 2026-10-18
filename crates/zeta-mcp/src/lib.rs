//! `ZetaMcp`: an MCP server whose tools and resources are a running node's operations, reached over its HTTP API.

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams,
    ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ServerCapabilities,
    ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde_json::{Value, json};
use zeta_client::{ClientError, NodeClient, ToolDef, op_from_call, op_tools, system_prompt};
use zeta_core::types::StreamId;

/// The committed guide, compiled in: the same build's vocabulary, so the tool
/// list and instructions stay available while the node restarts.
const FALLBACK_MD: &str = include_str!("../../../AGENTS.md");

/// `(uri, name, description)` of the fixed resources.
const RESOURCES: [(&str, &str, &str); 5] = [
    (
        "zeta://sources",
        "sources",
        "The node's sources (source_list).",
    ),
    ("zeta://jobs", "jobs", "The node's jobs (job_list)."),
    ("zeta://streams", "streams", "The node's live streams."),
    (
        "zeta://schema",
        "schema",
        "JSON schemas of operations, results, errors and stage kinds.",
    ),
    (
        "zeta://agents-md",
        "agents-md",
        "The node's agent guide (AGENTS.md).",
    ),
];
const STREAM_PREFIX: &str = "zeta://streams/";
const TAIL_FRAMES: u32 = 20;
const TAIL_TIMEOUT_MS: u32 = 2000;

pub struct ZetaMcp {
    client: NodeClient,
}

impl ZetaMcp {
    pub fn new(client: NodeClient) -> Self {
        Self { client }
    }

    /// The node's tools, or the compiled-in schema's when the node does not answer.
    async fn tools(&self) -> Vec<ToolDef> {
        match tokio::join!(self.client.schema(), self.client.agents_md()) {
            (Ok(schema), Ok(md)) => op_tools(&schema, &md),
            _ => op_tools(&zeta_core::ops::schema(), FALLBACK_MD),
        }
    }

    /// Apply a tool call as an operation: `(is_error, json)`.
    async fn call(&self, name: &str, args: Value) -> (bool, Value) {
        let op = match op_from_call(name, args, &self.tools().await) {
            Ok(op) => op,
            Err(e) => return (true, json!(e)),
        };
        match self.client.apply_value(op).await {
            Ok(Ok(v)) => (false, v),
            Ok(Err(e)) => (true, e),
            Err(e) => (true, json!({"error": "other", "message": e.to_string()})),
        }
    }

    /// An operation result as JSON, or the error as a resource-read failure.
    async fn op_json(&self, op: &str) -> Result<Value, McpError> {
        flatten(self.client.apply_value(json!({ "op": op })).await)
    }
}

fn client_error(e: ClientError) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

fn flatten(r: Result<Result<Value, Value>, ClientError>) -> Result<Value, McpError> {
    match r.map_err(client_error)? {
        Ok(v) => Ok(v),
        Err(e) => Err(McpError::internal_error(e.to_string(), Some(e))),
    }
}

fn json_contents(v: &Value, uri: &str) -> ResourceContents {
    ResourceContents::text(v.to_string(), uri).with_mime_type("application/json")
}

impl ServerHandler for ZetaMcp {
    fn get_info(&self) -> ServerConfig {
        let caps = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        ServerConfig::new(caps)
            .with_server_info(Implementation::new("zeta-mcp", zeta_core::VERSION))
            .with_instructions(system_prompt(FALLBACK_MD))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools = self
            .tools()
            .await
            .into_iter()
            .map(|t| {
                let schema = match t.input_schema {
                    Value::Object(o) => o,
                    _ => Default::default(),
                };
                Tool::new(t.name, t.description, Arc::new(schema))
            })
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let args = Value::Object(request.arguments.unwrap_or_default());
        let (is_error, v) = self.call(&request.name, args).await;
        let content = vec![ContentBlock::text(v.to_string())];
        let result = if is_error {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
        };
        Ok(result.into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let mut resources: Vec<Resource> = RESOURCES
            .iter()
            .map(|(uri, name, desc)| {
                let mime = if *name == "agents-md" {
                    "text/markdown"
                } else {
                    "application/json"
                };
                Resource::new(*uri, *name)
                    .with_description(*desc)
                    .with_mime_type(mime)
            })
            .collect();
        if let Ok(streams) = self.client.streams().await {
            resources.extend(streams.into_iter().map(|s| {
                let id = s.id.0;
                Resource::new(format!("{STREAM_PREFIX}{id}"), format!("stream {id}"))
                    .with_description(format!("Recent frames of the {:?} stream {id}.", s.kind))
                    .with_mime_type("application/json")
            }));
        }
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let uri = request.uri.as_str();
        let contents = match uri {
            "zeta://sources" => json_contents(&self.op_json("source_list").await?, uri),
            "zeta://jobs" => json_contents(&self.op_json("job_list").await?, uri),
            "zeta://streams" => {
                let streams = self.client.streams().await.map_err(client_error)?;
                json_contents(&json!(streams), uri)
            }
            "zeta://schema" => {
                json_contents(&self.client.schema().await.map_err(client_error)?, uri)
            }
            "zeta://agents-md" => {
                let md = self.client.agents_md().await.map_err(client_error)?;
                ResourceContents::text(md, uri).with_mime_type("text/markdown")
            }
            _ => match uri.strip_prefix(STREAM_PREFIX) {
                Some(id) if !id.is_empty() => {
                    let stream = StreamId(id.to_string());
                    let tail = self
                        .client
                        .tail(&stream, TAIL_FRAMES, TAIL_TIMEOUT_MS)
                        .await;
                    json_contents(&flatten(tail)?, uri)
                }
                _ => {
                    return Err(McpError::resource_not_found(
                        format!("unknown resource {uri}"),
                        Some(json!({ "uri": uri })),
                    ));
                }
            },
        };
        Ok(ReadResourceResult::new(vec![contents]).into())
    }
}
