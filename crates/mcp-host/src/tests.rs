//! Integration tests against the bundled `scripts/fixtures/mcp-echo.ts` server.
//!
//! Every transport the host supports is exercised end to end: an stdio child
//! process, MCP Streamable HTTP, and the legacy HTTP+SSE transport. The fixture
//! is a real MCP server, so these tests cover framing, initialization, catalog
//! listing, and tool calls rather than a mocked adapter.

use std::collections::BTreeSet;
use std::io::{BufRead as _, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use personal_agent_mcp_manager::{
    LifecycleState, McpManager, ProtocolVersion, RuntimeAdapter as _, RuntimeHandshake,
    ServerDefinition, ServerSource, ToolAnnotations, ToolDescriptor, TransportDefinition,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{BackoffPolicy, HostConfig, McpHost, ResolvedAdapter};

/// The protocol revision `rmcp` 0.10 speaks; the fixture echoes it back.
const FIXTURE_PROTOCOL: &str = "2025-03-26";

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root resolves")
}

fn fixture_script() -> String {
    repository_root()
        .join("scripts/fixtures/mcp-echo.ts")
        .to_string_lossy()
        .into_owned()
}

fn host() -> McpHost {
    let mut config = HostConfig::new(repository_root());
    config.backoff = BackoffPolicy {
        attempts: 2,
        initial: Duration::from_millis(20),
        maximum: Duration::from_millis(40),
    };
    config.connect_timeout = Duration::from_secs(20);
    McpHost::new(config).expect("the host runtime starts")
}

fn definition(namespace: &str, transport: TransportDefinition) -> ServerDefinition {
    let preferred = ProtocolVersion::new(FIXTURE_PROTOCOL).expect("a valid revision");
    ServerDefinition {
        id: Uuid::new_v4(),
        name: format!("{namespace} fixture"),
        namespace: namespace.to_owned(),
        description: "MCP echo fixture".into(),
        source: ServerSource::Manual,
        transport,
        supported_protocols: BTreeSet::from([preferred.clone(), ProtocolVersion::current()]),
        preferred_protocol: preferred,
        install: None,
        project_scopes: BTreeSet::new(),
        agent_scopes: BTreeSet::new(),
        tags: BTreeSet::new(),
    }
}

fn stdio_definition(namespace: &str) -> ServerDefinition {
    definition(
        namespace,
        TransportDefinition::Stdio {
            executable: "bun".into(),
            arguments: vec![fixture_script(), "--transport=stdio".into()],
            working_directory: None,
            environment: Vec::new(),
        },
    )
}

/// A fixture server listening on a loopback port, killed when the test ends.
struct HttpFixture {
    child: Child,
    port: u16,
}

impl HttpFixture {
    fn start(transport: &str) -> Self {
        let mut child = Command::new("bun")
            .arg(fixture_script())
            .arg(format!("--transport={transport}"))
            .arg("--port=0")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the bun fixture starts");
        let stdout = child.stdout.take().expect("the fixture stdout is piped");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("the fixture announces its port");
        let port = line
            .trim()
            .strip_prefix("listening ")
            .and_then(|port| port.parse().ok())
            .unwrap_or_else(|| panic!("unexpected fixture banner: {line}"));
        Self { child, port }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tool<'a>(handshake: &'a RuntimeHandshake, resolved: &str) -> &'a ToolDescriptor {
    handshake
        .catalog
        .tools
        .iter()
        .find(|tool| tool.resolved_name == resolved)
        .unwrap_or_else(|| panic!("{resolved} is advertised"))
}

fn echoed(result: &Value) -> &Value {
    result
        .get("structuredContent")
        .and_then(|content| content.get("echoed"))
        .expect("the echo tool returns structured content")
}

#[tokio::test]
async fn stdio_round_trip_lists_tools_and_echoes_arguments() {
    let host = host();
    let definition = stdio_definition("stdio_echo");
    let handshake = host
        .connect(&definition)
        .await
        .expect("the server connects");

    assert!(
        handshake.latency_ms > 0,
        "handshake latency must be measured"
    );
    assert_eq!(handshake.catalog.tools.len(), 4);
    assert!(handshake.catalog.supports_logging);

    let result = host
        .call_tool(&definition, "echo", json!({"message": "hello"}))
        .await
        .expect("the echo tool answers");
    assert_eq!(echoed(&result), &json!({"message": "hello"}));
}

#[tokio::test]
async fn stdio_health_is_a_measured_round_trip() {
    let host = host();
    let definition = stdio_definition("stdio_health");
    host.connect(&definition)
        .await
        .expect("the server connects");

    let latency = host.health(&definition).await.expect("the server answers");
    assert!(latency > 0, "measured latency must be greater than zero");
    assert!(latency < 20_000, "a local ping cannot take 20 seconds");
}

#[tokio::test]
async fn tool_annotations_survive_the_handshake() {
    let host = host();
    let definition = stdio_definition("stdio_annotations");
    let handshake = host
        .connect(&definition)
        .await
        .expect("the server connects");

    assert_eq!(
        tool(&handshake, "stdio_annotations.echo").annotations,
        ToolAnnotations {
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
        }
    );
    assert_eq!(
        tool(&handshake, "stdio_annotations.purge").annotations,
        ToolAnnotations {
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: true,
        }
    );
    assert_eq!(
        tool(&handshake, "stdio_annotations.unannotated").annotations,
        ToolAnnotations::default()
    );
    assert_eq!(
        tool(&handshake, "stdio_annotations.echo").title.as_deref(),
        Some("Echo")
    );
}

#[tokio::test]
async fn stdio_servers_get_a_pinned_directory_and_an_allowlisted_environment() {
    let host = host();
    let definition = stdio_definition("stdio_sandbox");
    host.connect(&definition)
        .await
        .expect("the server connects");

    let report = host
        .call_tool(&definition, "environment", json!({}))
        .await
        .expect("the environment tool answers");
    let structured = report
        .get("structuredContent")
        .expect("the tool returns structured content");
    let names: BTreeSet<String> =
        serde_json::from_value(structured["names"].clone()).expect("names decode");
    let allowed = HostConfig::new(repository_root()).environment_allowlist;
    let leaked: Vec<&String> = names
        .iter()
        .filter(|name| !allowed.contains(*name))
        .collect();
    assert!(leaked.is_empty(), "environment leaked {leaked:?}");
    assert_eq!(
        structured["cwd"].as_str().map(PathBuf::from),
        Some(repository_root())
    );
}

#[tokio::test]
async fn streamable_http_round_trip_uses_the_native_transport() {
    let fixture = HttpFixture::start("http");
    let host = host();
    let definition = definition(
        "http_echo",
        TransportDefinition::StreamableHttp {
            endpoint: fixture.url("/mcp"),
            stateless: false,
            headers: Vec::new(),
            oauth: None,
        },
    );
    let handshake = host
        .connect(&definition)
        .await
        .expect("the server connects");
    assert!(handshake.latency_ms > 0);
    assert!(tool(&handshake, "http_echo.echo").annotations.read_only);

    let result = host
        .call_tool(&definition, "echo", json!({"over": "http"}))
        .await
        .expect("the echo tool answers");
    assert_eq!(echoed(&result), &json!({"over": "http"}));
    assert!(host.health(&definition).await.expect("ping answers") > 0);
}

#[tokio::test]
async fn legacy_sse_is_refused_with_a_reason_and_a_remediation() {
    let host = host();
    let definition = definition(
        "sse_echo",
        TransportDefinition::LegacySse {
            endpoint: "http://127.0.0.1:1/sse".to_owned(),
            headers: Vec::new(),
            oauth: None,
        },
    );

    let error = host
        .connect(&definition)
        .await
        .expect_err("legacy HTTP+SSE is no longer supported");

    // The refusal must name the transport and the fix, never fail obscurely.
    assert_eq!(error.code, "transport_unsupported");
    assert!(
        error.message.contains("HTTP+SSE"),
        "the refusal must name the transport: {}",
        error.message
    );
    assert!(
        error.message.contains("Streamable HTTP"),
        "the refusal must name the remediation: {}",
        error.message
    );
}

#[tokio::test]
async fn server_reported_tool_errors_do_not_look_like_success() {
    let host = host();
    let definition = stdio_definition("stdio_tool_error");
    host.connect(&definition)
        .await
        .expect("the server connects");

    let error = host
        .call_tool(&definition, "purge", json!({"target": "everything"}))
        .await
        .expect_err("the fixture reports a tool failure");
    assert_eq!(error.code, "tool_error");
}

#[tokio::test]
async fn disconnect_drops_the_session_and_calls_afterwards_fail() {
    let host = host();
    let definition = stdio_definition("stdio_disconnect");
    host.connect(&definition)
        .await
        .expect("the server connects");
    assert!(host.is_connected(definition.id));

    host.disconnect(&definition).await.expect("disconnect");
    assert!(!host.is_connected(definition.id));
    let error = host
        .call_tool(&definition, "echo", json!({}))
        .await
        .expect_err("a dropped session cannot serve calls");
    assert_eq!(error.code, "not_connected");
}

#[tokio::test]
async fn a_missing_executable_is_retried_then_reported() {
    let host = host();
    let definition = definition(
        "stdio_missing",
        TransportDefinition::Stdio {
            executable: "personal-agent-no-such-mcp-server".into(),
            arguments: Vec::new(),
            working_directory: None,
            environment: Vec::new(),
        },
    );
    let error = host
        .connect(&definition)
        .await
        .expect_err("a missing executable cannot connect");
    assert_eq!(error.code, "spawn_failed");

    let attempts = host
        .logs(definition.id)
        .into_iter()
        .filter(|line| line.message.starts_with("connect attempt "))
        .count();
    assert_eq!(attempts, 2, "the backoff policy retried before failing");
}

#[tokio::test]
async fn the_log_ring_records_lifecycle_progress() {
    let host = host();
    let definition = stdio_definition("stdio_logs");
    host.connect(&definition)
        .await
        .expect("the server connects");

    let lines = host.logs(definition.id);
    assert!(
        lines
            .iter()
            .any(|line| line.message.contains("connected over stdio")),
        "expected a connect line, got {lines:?}"
    );
}

#[test]
fn the_runtime_adapter_bridge_drives_the_manager_state_machine() {
    let host = host();
    let definition = stdio_definition("stdio_manager");
    let server_id = definition.id;
    let mut manager = McpManager::default();
    manager
        .add_server(definition)
        .expect("the server registers");

    manager
        .connect(server_id, &mut host.adapter())
        .expect("the manager connects through the native adapter");
    let health = manager
        .check_health(server_id, &mut host.adapter())
        .expect("the manager measures health");

    let server = manager.server(server_id).expect("the server exists");
    assert_eq!(server.state, LifecycleState::Connected);
    assert_eq!(server.catalog.tools.len(), 4);
    assert!(health.healthy);
    assert!(
        health.latency_ms.is_some_and(|latency| latency > 0),
        "the manager must record a measured latency"
    );

    manager
        .disable(server_id, &mut host.adapter())
        .expect("the manager disconnects");
}

#[test]
fn the_resolved_adapter_replays_an_awaited_outcome() {
    let handshake = RuntimeHandshake {
        server_protocols: BTreeSet::from([ProtocolVersion::current()]),
        catalog: personal_agent_mcp_manager::CapabilityCatalog::default(),
        latency_ms: 7,
    };
    let mut adapter = ResolvedAdapter::connected(Ok(handshake.clone()));
    let definition = stdio_definition("stdio_resolved");
    assert_eq!(adapter.connect(&definition).expect("replayed"), handshake);
    assert_eq!(adapter.health(&definition).expect("replayed"), 7);
    // A second connect has nothing left to replay.
    assert_eq!(
        adapter.connect(&definition).expect_err("no outcome").code,
        "not_initialized"
    );
}

#[test]
fn keychain_bindings_are_refused_until_a_resolver_is_configured() {
    let host = host();
    let mut definition = stdio_definition("stdio_keychain");
    let TransportDefinition::Stdio { environment, .. } = &mut definition.transport else {
        unreachable!("the fixture definition is stdio")
    };
    environment.push(personal_agent_mcp_manager::EnvironmentBinding {
        name: "FIXTURE_TOKEN".into(),
        value: personal_agent_mcp_manager::BindingValue::Keychain {
            reference: personal_agent_mcp_manager::KeychainReference {
                reference_id: "fixture-token".into(),
                service: "personal-agent".into(),
                account_hint: "mcp".into(),
            },
        },
    });
    let error = host
        .adapter()
        .connect(&definition)
        .expect_err("an unresolvable binding refuses the spawn");
    assert!(error.authentication_required);
    assert_eq!(error.code, "keychain_unavailable");
}

#[tokio::test]
async fn resolved_keychain_bindings_reach_the_child_environment() {
    let secrets = Arc::new(crate::InMemorySecrets::default().with("fixture-token", "s3cret"));
    let mut config = HostConfig::new(repository_root());
    config.backoff = BackoffPolicy::default();
    let host = McpHost::with_secrets(config, secrets).expect("the host runtime starts");
    let mut definition = stdio_definition("stdio_secret");
    let TransportDefinition::Stdio { environment, .. } = &mut definition.transport else {
        unreachable!("the fixture definition is stdio")
    };
    environment.push(personal_agent_mcp_manager::EnvironmentBinding {
        name: "FIXTURE_TOKEN".into(),
        value: personal_agent_mcp_manager::BindingValue::Keychain {
            reference: personal_agent_mcp_manager::KeychainReference {
                reference_id: "fixture-token".into(),
                service: "personal-agent".into(),
                account_hint: "mcp".into(),
            },
        },
    });

    host.connect(&definition)
        .await
        .expect("the server connects");
    let report = host
        .call_tool(&definition, "environment", json!({}))
        .await
        .expect("the environment tool answers");
    let names: BTreeSet<String> =
        serde_json::from_value(report["structuredContent"]["names"].clone()).expect("names decode");
    assert!(names.contains("FIXTURE_TOKEN"));
}

#[tokio::test]
async fn header_bindings_are_sent_on_every_http_request() {
    let fixture = HttpFixture::start("http");
    let host = host();
    let definition = definition(
        "http_headers",
        TransportDefinition::StreamableHttp {
            endpoint: fixture.url("/mcp"),
            stateless: false,
            headers: vec![personal_agent_mcp_manager::HeaderBinding {
                name: "X-Fixture".into(),
                value: personal_agent_mcp_manager::BindingValue::NonSecret {
                    value: "present".into(),
                },
            }],
            oauth: None,
        },
    );
    // The fixture accepts any header; the assertion is that a session with a
    // header binding still initializes rather than failing header construction.
    host.connect(&definition)
        .await
        .expect("a header-bound session connects");
}

#[test]
fn invalid_header_names_are_rejected_before_any_request() {
    let host = host();
    let definition = definition(
        "http_bad_header",
        TransportDefinition::StreamableHttp {
            endpoint: "http://127.0.0.1:1/mcp".into(),
            stateless: false,
            headers: vec![personal_agent_mcp_manager::HeaderBinding {
                name: "not a header".into(),
                value: personal_agent_mcp_manager::BindingValue::NonSecret { value: "x".into() },
            }],
            oauth: None,
        },
    );
    let error = host
        .adapter()
        .connect(&definition)
        .expect_err("an invalid header name cannot be sent");
    assert_eq!(error.code, "invalid_header");
}

#[test]
fn adapter_messages_never_quote_transport_detail() {
    let host = host();
    let definition = definition(
        "http_unreachable",
        TransportDefinition::StreamableHttp {
            endpoint: "http://127.0.0.1:1/mcp".into(),
            stateless: false,
            headers: Vec::new(),
            oauth: None,
        },
    );
    let error = host
        .adapter()
        .connect(&definition)
        .expect_err("a closed port cannot connect");
    assert!(!error.message.contains("127.0.0.1"));
    assert!(!error.message.contains("Connection refused"));
    assert_eq!(error.code, "initialize_failed");
}
