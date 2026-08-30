//! Transport construction and the live MCP client session.

use std::collections::BTreeSet;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use personal_agent_mcp_manager::{
    AdapterError, CapabilityCatalog, EnvironmentBinding, HeaderBinding, ProtocolVersion,
    RuntimeHandshake, ServerDefinition, TransportDefinition,
};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rmcp::model::{ClientCapabilities, ClientInfo, Implementation};
use rmcp::service::{RoleClient, RunningService, ServiceExt as _};
use rmcp::transport::sse_client::{SseClientConfig, SseClientTransport};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::transport::{ConfigureCommandExt as _, TokioChildProcess};
use tokio::io::{AsyncBufReadExt as _, BufReader};

use crate::catalog::capability_catalog;
use crate::config::HostConfig;
use crate::error::{adapter_error, initialize_error, service_error};
use crate::http_client::HttpClient;
use crate::logs::{LogLevel, LogRing};
use crate::secrets::{SecretResolver, resolve_binding};

/// A connected MCP client plus the catalog captured at initialization.
pub(crate) struct Session {
    pub(crate) client: RunningService<RoleClient, ClientInfo>,
    pub(crate) handshake: RuntimeHandshake,
}

pub(crate) type SharedLog = Arc<Mutex<LogRing>>;

pub(crate) fn record(log: &SharedLog, level: LogLevel, message: impl Into<String>) {
    let Ok(mut ring) = log.lock() else {
        tracing::error!("MCP host log ring is poisoned");
        return;
    };
    ring.push(level, message);
}

/// Borrowed stdio transport fields, kept together so the spawn helpers stay
/// within the workspace argument-count limit.
struct StdioSpec<'a> {
    executable: &'a str,
    arguments: &'a [String],
    working_directory: Option<&'a str>,
    environment: &'a [EnvironmentBinding],
}

/// Client identity and the protocol revision this definition prefers.
fn client_info(config: &HostConfig, definition: &ServerDefinition) -> ClientInfo {
    let protocol_version = serde_json::from_value(serde_json::Value::String(
        definition.preferred_protocol.as_str().to_owned(),
    ))
    .unwrap_or_default();
    ClientInfo {
        protocol_version,
        capabilities: ClientCapabilities::default(),
        client_info: Implementation {
            name: config.client_name.clone(),
            version: config.client_version.clone(),
            title: None,
            icons: None,
            website_url: None,
        },
    }
}

fn header_map(
    secrets: &dyn SecretResolver,
    headers: &[HeaderBinding],
) -> Result<HeaderMap, AdapterError> {
    let mut map = HeaderMap::new();
    for binding in headers {
        let name = HeaderName::try_from(binding.name.as_str())
            .map_err(|_| adapter_error("invalid_header", "A header binding name is not valid."))?;
        let value = resolve_binding(secrets, &binding.value)?;
        let mut value = HeaderValue::try_from(value)
            .map_err(|_| adapter_error("invalid_header", "A header binding value is not valid."))?;
        value.set_sensitive(true);
        map.insert(name, value);
    }
    Ok(map)
}

fn http_client(
    secrets: &dyn SecretResolver,
    headers: &[HeaderBinding],
) -> Result<HttpClient, AdapterError> {
    let client = reqwest::Client::builder()
        .default_headers(header_map(secrets, headers)?)
        .build()
        .map_err(|_| adapter_error("transport_failed", "The HTTP client could not be created."))?;
    Ok(HttpClient::new(client))
}

/// Spawns an stdio server with a cleared environment and a pinned directory.
fn stdio_command(
    config: &HostConfig,
    secrets: &dyn SecretResolver,
    spec: &StdioSpec<'_>,
) -> Result<tokio::process::Command, AdapterError> {
    if spec.executable.trim().is_empty() {
        return Err(adapter_error(
            "invalid_definition",
            "The server command is empty.",
        ));
    }
    let directory = spec.working_directory.map_or_else(
        || config.working_directory.clone(),
        std::convert::Into::into,
    );
    let mut command = tokio::process::Command::new(spec.executable);
    // Arguments are passed as argv; no shell ever interprets this command.
    command
        .args(spec.arguments)
        .env_clear()
        .current_dir(directory)
        .kill_on_drop(true);
    for (name, value) in config.inherited_environment() {
        command.env(name, value);
    }
    for binding in spec.environment {
        command.env(&binding.name, resolve_binding(secrets, &binding.value)?);
    }
    Ok(command)
}

fn pump_stderr(stderr: tokio::process::ChildStderr, log: SharedLog) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            record(&log, LogLevel::Warn, line);
        }
    });
}

async fn serve_stdio(
    config: &HostConfig,
    definition: &ServerDefinition,
    secrets: &dyn SecretResolver,
    log: &SharedLog,
    spec: &StdioSpec<'_>,
) -> Result<RunningService<RoleClient, ClientInfo>, AdapterError> {
    let command = stdio_command(config, secrets, spec)?;
    let (process, stderr) = TokioChildProcess::builder(command.configure(|_| {}))
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            record(log, LogLevel::Error, format!("spawn failed: {error}"));
            adapter_error("spawn_failed", "The MCP server process could not start.")
        })?;
    if let Some(stderr) = stderr {
        pump_stderr(stderr, Arc::clone(log));
    }
    client_info(config, definition)
        .serve(process)
        .await
        .map_err(|error| initialize_error(log, &error))
}

async fn serve_streamable_http(
    config: &HostConfig,
    definition: &ServerDefinition,
    secrets: &dyn SecretResolver,
    log: &SharedLog,
    endpoint: &str,
    stateless: bool,
    headers: &[HeaderBinding],
) -> Result<RunningService<RoleClient, ClientInfo>, AdapterError> {
    let transport = StreamableHttpClientTransport::with_client(
        http_client(secrets, headers)?,
        StreamableHttpClientTransportConfig {
            allow_stateless: stateless,
            ..StreamableHttpClientTransportConfig::with_uri(endpoint)
        },
    );
    client_info(config, definition)
        .serve(transport)
        .await
        .map_err(|error| initialize_error(log, &error))
}

async fn serve_legacy_sse(
    config: &HostConfig,
    definition: &ServerDefinition,
    secrets: &dyn SecretResolver,
    log: &SharedLog,
    endpoint: &str,
    headers: &[HeaderBinding],
) -> Result<RunningService<RoleClient, ClientInfo>, AdapterError> {
    let transport = SseClientTransport::start_with_client(
        http_client(secrets, headers)?,
        SseClientConfig {
            sse_endpoint: endpoint.into(),
            ..SseClientConfig::default()
        },
    )
    .await
    .map_err(|error| {
        record(log, LogLevel::Error, format!("sse connect failed: {error}"));
        adapter_error(
            "transport_failed",
            "The MCP server did not open a legacy SSE stream.",
        )
    })?;
    client_info(config, definition)
        .serve(transport)
        .await
        .map_err(|error| initialize_error(log, &error))
}

/// Opens one transport and completes MCP initialization.
pub(crate) async fn serve(
    config: &HostConfig,
    definition: &ServerDefinition,
    secrets: &dyn SecretResolver,
    log: &SharedLog,
) -> Result<RunningService<RoleClient, ClientInfo>, AdapterError> {
    match &definition.transport {
        TransportDefinition::Stdio {
            executable,
            arguments,
            working_directory,
            environment,
        } => {
            let spec = StdioSpec {
                executable,
                arguments,
                working_directory: working_directory.as_deref(),
                environment,
            };
            serve_stdio(config, definition, secrets, log, &spec).await
        }
        TransportDefinition::StreamableHttp {
            endpoint,
            stateless,
            headers,
            ..
        } => {
            serve_streamable_http(
                config, definition, secrets, log, endpoint, *stateless, headers,
            )
            .await
        }
        TransportDefinition::LegacySse {
            endpoint, headers, ..
        } => serve_legacy_sse(config, definition, secrets, log, endpoint, headers).await,
    }
}

/// Lists tools, resources, and prompts advertised by an initialized server.
pub(crate) async fn read_catalog(
    client: &RunningService<RoleClient, ClientInfo>,
    namespace: &str,
) -> Result<CapabilityCatalog, AdapterError> {
    let Some(info) = client.peer_info() else {
        return Err(adapter_error(
            "not_initialized",
            "The MCP server did not return initialization details.",
        ));
    };
    let capabilities = info.capabilities.clone();
    let tools = if capabilities.tools.is_some() {
        client.list_all_tools().await.map_err(service_error)?
    } else {
        Vec::new()
    };
    let resources = if capabilities.resources.is_some() {
        client.list_all_resources().await.map_err(service_error)?
    } else {
        Vec::new()
    };
    let prompts = if capabilities.prompts.is_some() {
        client.list_all_prompts().await.map_err(service_error)?
    } else {
        Vec::new()
    };
    Ok(capability_catalog(
        namespace,
        &capabilities,
        &tools,
        &resources,
        &prompts,
    ))
}

/// The protocol revision the server accepted, as a manager value.
pub(crate) fn server_protocols(
    client: &RunningService<RoleClient, ClientInfo>,
) -> Result<BTreeSet<ProtocolVersion>, AdapterError> {
    let info = client.peer_info().ok_or_else(|| {
        adapter_error(
            "not_initialized",
            "The MCP server did not return initialization details.",
        )
    })?;
    let version = ProtocolVersion::new(info.protocol_version.to_string()).map_err(|_| {
        adapter_error(
            "protocol_mismatch",
            "The MCP server reported an unusable protocol revision.",
        )
    })?;
    Ok(BTreeSet::from([version]))
}
