//! GUI-ready MCP server registry, lifecycle, permissions, and safe tool routing.
//!
//! The manager owns configuration and policy state, not credentials or process
//! handles. Native adapters perform platform operations and credential lookup.
//! MCP tool calls leave this crate only as [`GatewayToolRequest`] values so they
//! can be enforced by the application's existing `ToolGateway`.

#![allow(clippy::missing_errors_doc, clippy::too_many_lines)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

/// Protocol revision implemented by new Personal Agent clients.
pub const CURRENT_PROTOCOL_VERSION: &str = "2026-07-28";
/// Maximum audit events retained in a UI snapshot.
pub const MAX_AUDIT_EVENTS: usize = 500;
/// Maximum lifecycle log lines retained per server.
pub const MAX_SERVER_LOGS: usize = 1_000;

/// MCP revision formatted as an ISO date. Unknown future revisions remain
/// representable so negotiation can fail cleanly rather than losing data.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProtocolVersion(String);

impl ProtocolVersion {
    /// Constructs a validated protocol revision.
    ///
    /// # Errors
    ///
    /// Returns `InvalidProtocolVersion` unless `value` is `YYYY-MM-DD`.
    pub fn new(value: impl Into<String>) -> Result<Self, ManagerError> {
        let value = value.into();
        let valid = value.len() == 10
            && value.as_bytes()[4] == b'-'
            && value.as_bytes()[7] == b'-'
            && value
                .chars()
                .enumerate()
                .all(|(index, character)| matches!(index, 4 | 7) || character.is_ascii_digit());
        if !valid {
            return Err(ManagerError::InvalidProtocolVersion(value));
        }
        Ok(Self(value))
    }

    /// Current MCP revision.
    #[must_use]
    pub fn current() -> Self {
        Self(CURRENT_PROTOCOL_VERSION.into())
    }

    /// Returns the revision string for JSON-RPC initialization.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Selects the newest revision supported by both peers.
#[must_use]
pub fn negotiate_protocol(
    client: &BTreeSet<ProtocolVersion>,
    server: &BTreeSet<ProtocolVersion>,
) -> Option<ProtocolVersion> {
    client.intersection(server).last().cloned()
}

/// Stable OS-keychain locator. It intentionally cannot hold a credential value.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct KeychainReference {
    pub reference_id: String,
    pub service: String,
    pub account_hint: String,
}

/// OAuth grant metadata stored without access, refresh, or client-secret values.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OAuthReference {
    pub grant_id: String,
    pub issuer: String,
    pub client_id: String,
    pub scopes: BTreeSet<String>,
    pub credential: KeychainReference,
    pub expires_at: Option<DateTime<Utc>>,
}

/// A non-secret literal or a native keychain lookup.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BindingValue {
    NonSecret { value: String },
    Keychain { reference: KeychainReference },
}

/// Environment binding for an stdio server.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentBinding {
    pub name: String,
    pub value: BindingValue,
}

/// HTTP header binding. Authorization-like headers require keychain lookup.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HeaderBinding {
    pub name: String,
    pub value: BindingValue,
}

/// Supported native transport definitions, including legacy migration modes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransportDefinition {
    Stdio {
        executable: String,
        arguments: Vec<String>,
        working_directory: Option<String>,
        environment: Vec<EnvironmentBinding>,
    },
    /// MCP 2025+ Streamable HTTP. Stateless mode follows the 2026 core model.
    StreamableHttp {
        endpoint: String,
        stateless: bool,
        headers: Vec<HeaderBinding>,
        oauth: Option<OAuthReference>,
    },
    /// Compatibility-only HTTP+SSE transport for older servers.
    LegacySse {
        endpoint: String,
        headers: Vec<HeaderBinding>,
        oauth: Option<OAuthReference>,
    },
}

impl TransportDefinition {
    /// Human-readable transport label for the GUI.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Stdio { .. } => "stdio",
            Self::StreamableHttp {
                stateless: true, ..
            } => "Streamable HTTP (stateless)",
            Self::StreamableHttp {
                stateless: false, ..
            } => "Streamable HTTP (session)",
            Self::LegacySse { .. } => "Legacy HTTP + SSE",
        }
    }
}

/// Where a server definition came from.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerSource {
    Catalog {
        catalog_id: String,
        publisher: String,
    },
    Manual,
    Imported {
        application: String,
    },
    LocalPackage {
        package: String,
    },
    Remote {
        origin: String,
    },
}

/// Exact native installation command shown to the user before consent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstallRecipe {
    pub program: String,
    pub arguments: Vec<String>,
    pub expected_artifact_sha256: Option<String>,
    pub source_url: Option<String>,
}

impl InstallRecipe {
    /// Quotes individual arguments for display only. Native adapters still use
    /// the structured program and argument array and never invoke a shell.
    #[must_use]
    pub fn display_command(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.arguments.iter().map(String::as_str))
            .map(shell_display_word)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Digest binding consent to the structured recipe.
    #[must_use]
    pub fn digest(&self) -> String {
        digest_json(self)
    }
}

/// Explicit consent bound to an exact operation digest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationConsent {
    pub operation_digest: String,
    pub displayed_text: String,
    pub accepted_at: DateTime<Utc>,
    pub user_confirmed: bool,
}

/// Server definition safe to serialize, persist, and export.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServerDefinition {
    pub id: Uuid,
    pub name: String,
    pub namespace: String,
    pub description: String,
    pub source: ServerSource,
    pub transport: TransportDefinition,
    pub supported_protocols: BTreeSet<ProtocolVersion>,
    pub preferred_protocol: ProtocolVersion,
    pub install: Option<InstallRecipe>,
    pub project_scopes: BTreeSet<String>,
    pub agent_scopes: BTreeSet<String>,
    pub tags: BTreeSet<String>,
}

/// Runtime lifecycle state exposed directly to the GUI.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Draft,
    InstallConsentRequired,
    Installing,
    Disabled,
    Connecting,
    Connected,
    Degraded,
    AuthenticationRequired,
    Crashed,
    UpdateAvailable,
    Updating,
    RollbackAvailable,
    Uninstalling,
    Uninstalled,
}

/// Last health sample, including rolling UX metrics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HealthStatus {
    pub healthy: bool,
    pub checked_at: DateTime<Utc>,
    pub latency_ms: Option<u64>,
    pub error_rate: f32,
    pub consecutive_failures: u32,
    pub message: String,
}

/// Release retained for safe update rollback.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstalledRelease {
    pub version: String,
    pub installed_at: DateTime<Utc>,
    pub artifact_sha256: Option<String>,
    pub recipe: Option<InstallRecipe>,
}

/// Pending package update shown before exact-command consent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdatePlan {
    pub target_version: String,
    pub release_notes_url: Option<String>,
    pub recipe: InstallRecipe,
}

/// MCP tool behavior annotations. Missing annotations remain conservative.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolAnnotations {
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub open_world: bool,
}

/// Tool catalog item. Descriptions and schemas are untrusted server data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub annotations: ToolAnnotations,
    pub resolved_name: String,
}

/// Resource catalog item.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResourceDescriptor {
    pub uri: String,
    pub name: String,
    pub description: String,
    pub mime_type: Option<String>,
}

/// Prompt catalog item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptDescriptor {
    pub name: String,
    pub description: String,
    pub arguments_schema: Option<Value>,
}

/// Server-advertised catalog after initialization.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CapabilityCatalog {
    pub tools: Vec<ToolDescriptor>,
    pub resources: Vec<ResourceDescriptor>,
    pub prompts: Vec<PromptDescriptor>,
    pub supports_logging: bool,
    pub supports_completions: bool,
    pub supports_resource_subscriptions: bool,
}

/// Permission outcome for a tool call.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Ask,
    Deny,
}

/// Scope at which a permission rule applies.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum PermissionScope {
    Global,
    Profile(String),
    Workspace(String),
    Agent(String),
}

/// Per-tool policy rule consumed before the native `ToolGateway`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolPermissionRule {
    pub tool: String,
    pub scope: PermissionScope,
    pub decision: PermissionDecision,
    pub execution_zone: String,
    pub max_calls_per_minute: u32,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
}

/// Log level for manager lifecycle logs.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// Redacted lifecycle log. Native adapters must not put credentials in message.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServerLog {
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub message: String,
}

/// Fully managed server record returned in GUI snapshots.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ManagedServer {
    pub definition: ServerDefinition,
    pub state: LifecycleState,
    pub enabled: bool,
    pub negotiated_protocol: Option<ProtocolVersion>,
    pub health: Option<HealthStatus>,
    pub catalog: CapabilityCatalog,
    pub permissions: Vec<ToolPermissionRule>,
    pub current_release: Option<InstalledRelease>,
    pub release_history: Vec<InstalledRelease>,
    pub pending_update: Option<UpdatePlan>,
    pub logs: VecDeque<ServerLog>,
    pub last_connected_at: Option<DateTime<Utc>>,
}

/// Metadata-only security/audit event. Tool arguments and credentials are omitted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub server_id: Option<Uuid>,
    pub event_type: String,
    pub outcome: String,
    pub metadata: BTreeMap<String, String>,
}

/// Read-only registry snapshot for the desktop UI.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ManagerSnapshot {
    pub servers: Vec<ManagedServer>,
    pub audit_events: Vec<AuditEvent>,
    pub protocol_version: String,
}

/// Context supplied by the active profile/session when resolving permissions.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct InvocationContext {
    pub profile_id: Option<String>,
    pub workspace_id: Option<String>,
    pub agent_id: Option<String>,
    pub user_confirmed: bool,
}

/// Policy-normalized MCP invocation. This must be passed to `ToolGateway`; the
/// MCP manager must never send it directly to a transport.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct GatewayToolRequest {
    pub request_id: Uuid,
    pub server_id: Uuid,
    pub tool_name: String,
    pub resolved_name: String,
    pub arguments: Value,
    pub protocol_version: ProtocolVersion,
    pub execution_zone: String,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
    pub requires_approval: bool,
    pub destructive: bool,
    pub open_world: bool,
}

/// Outcome of manager-side routing before the native policy gateway.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", content = "request", rename_all = "snake_case")]
pub enum ToolRoute {
    Ready(GatewayToolRequest),
    ApprovalRequired(GatewayToolRequest),
}

/// Result returned by a native runtime adapter after MCP initialize/catalog.
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeHandshake {
    pub server_protocols: BTreeSet<ProtocolVersion>,
    pub catalog: CapabilityCatalog,
    pub latency_ms: u64,
}

/// Native transport boundary. Implementations own process/network handles and
/// retrieve secret values from the OS keychain immediately before use.
pub trait RuntimeAdapter {
    /// Starts/connects and performs MCP initialization.
    fn connect(&mut self, definition: &ServerDefinition) -> Result<RuntimeHandshake, AdapterError>;
    /// Performs a protocol-level health ping.
    fn health(&mut self, definition: &ServerDefinition) -> Result<u64, AdapterError>;
    /// Gracefully terminates the current transport session.
    fn disconnect(&mut self, definition: &ServerDefinition) -> Result<(), AdapterError>;
}

/// Native package boundary. Commands are structured and must not use a shell.
pub trait PackageAdapter {
    fn install(&mut self, recipe: &InstallRecipe) -> Result<InstalledRelease, AdapterError>;
    fn uninstall(&mut self, definition: &ServerDefinition) -> Result<(), AdapterError>;
}

/// Sanitized adapter failure. Native adapters must redact process/network text.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{code}: {message}")]
pub struct AdapterError {
    pub code: String,
    pub message: String,
    pub authentication_required: bool,
}

/// Import source detected or chosen in the GUI wizard.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportSource {
    ClaudeDesktop,
    OpenCode,
    Generic,
}

/// Migration warning which never repeats an imported secret value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportIssue {
    pub server_name: String,
    pub field: String,
    pub code: String,
    pub message: String,
}

/// Secret-free import preview. Definitions remain drafts until reviewed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ImportPreview {
    pub definitions: Vec<ServerDefinition>,
    pub issues: Vec<ImportIssue>,
}

/// Portable export envelope. Credential references may be reconnected but no
/// access token, refresh token, client secret, password, or API key is exported.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SecretFreeExport {
    pub schema_version: u32,
    pub exported_at: DateTime<Utc>,
    pub servers: Vec<ServerDefinition>,
    pub permissions: BTreeMap<Uuid, Vec<ToolPermissionRule>>,
}

/// Manager/validation failures safe for direct GUI display.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ManagerError {
    #[error("server does not exist: {0}")]
    MissingServer(Uuid),
    #[error("server namespace is already in use: {0}")]
    NamespaceCollision(String),
    #[error("server definition is invalid: {0}")]
    InvalidDefinition(String),
    #[error("transport is invalid: {0}")]
    InvalidTransport(String),
    #[error("protocol version is invalid: {0}")]
    InvalidProtocolVersion(String),
    #[error("no mutually supported MCP protocol revision")]
    ProtocolMismatch,
    #[error("operation requires exact, explicit user consent")]
    ConsentRequired,
    #[error("operation consent does not match the displayed operation")]
    ConsentMismatch,
    #[error("server is not in a state that supports this operation: {0:?}")]
    InvalidState(LifecycleState),
    #[error("tool does not exist: {0}")]
    MissingTool(String),
    #[error("tool access is denied by MCP manager policy: {0}")]
    ToolDenied(String),
    #[error("tool arguments do not conform to the advertised input schema: {0}")]
    InvalidArguments(String),
    #[error("adapter operation failed: {0}")]
    Adapter(String),
    #[error("import is invalid: {0}")]
    InvalidImport(String),
    #[error("no rollback release is available")]
    RollbackUnavailable,
}

/// Registry and deterministic lifecycle state machine.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct McpManager {
    servers: BTreeMap<Uuid, ManagedServer>,
    audit_events: VecDeque<AuditEvent>,
}

impl McpManager {
    /// Adds a validated draft. Adding a definition never installs or connects it.
    ///
    /// # Errors
    ///
    /// Rejects unsafe definitions, transports, and namespace collisions.
    pub fn add_server(&mut self, definition: ServerDefinition) -> Result<Uuid, ManagerError> {
        validate_definition(&definition)?;
        if self
            .servers
            .values()
            .any(|server| server.definition.namespace == definition.namespace)
        {
            return Err(ManagerError::NamespaceCollision(
                definition.namespace.clone(),
            ));
        }
        let id = definition.id;
        let state = if definition.install.is_some() {
            LifecycleState::InstallConsentRequired
        } else {
            LifecycleState::Disabled
        };
        self.servers.insert(
            id,
            ManagedServer {
                definition,
                state,
                enabled: false,
                negotiated_protocol: None,
                health: None,
                catalog: CapabilityCatalog::default(),
                permissions: Vec::new(),
                current_release: None,
                release_history: Vec::new(),
                pending_update: None,
                logs: VecDeque::new(),
                last_connected_at: None,
            },
        );
        self.audit(id, "server.added", "success", BTreeMap::new());
        Ok(id)
    }

    /// Returns the exact command digest/text which the GUI must display before
    /// enabling its Install confirmation button.
    pub fn install_consent_preview(
        &self,
        server_id: Uuid,
    ) -> Result<(String, String), ManagerError> {
        let recipe = self
            .server(server_id)?
            .definition
            .install
            .as_ref()
            .ok_or_else(|| ManagerError::InvalidDefinition("no install recipe".into()))?;
        Ok((recipe.digest(), recipe.display_command()))
    }

    /// Installs a local server after exact-command consent.
    pub fn install(
        &mut self,
        server_id: Uuid,
        consent: &OperationConsent,
        adapter: &mut impl PackageAdapter,
    ) -> Result<(), ManagerError> {
        let recipe = self
            .server(server_id)?
            .definition
            .install
            .clone()
            .ok_or_else(|| ManagerError::InvalidDefinition("no install recipe".into()))?;
        verify_consent(consent, &recipe.digest(), &recipe.display_command())?;
        self.transition(server_id, LifecycleState::Installing)?;
        match adapter.install(&recipe) {
            Ok(release) => {
                let server = self.server_mut(server_id)?;
                server.current_release = Some(release);
                server.state = LifecycleState::Disabled;
                server.enabled = false;
                push_log(server, LogLevel::Info, "Installation completed");
                self.audit(server_id, "server.installed", "success", BTreeMap::new());
                Ok(())
            }
            Err(error) => {
                let code = error.code.clone();
                self.adapter_failure(server_id, error, LifecycleState::Crashed);
                Err(ManagerError::Adapter(code))
            }
        }
    }

    /// Enables and connects a server through a native transport adapter.
    pub fn connect(
        &mut self,
        server_id: Uuid,
        adapter: &mut impl RuntimeAdapter,
    ) -> Result<(), ManagerError> {
        {
            let server = self.server(server_id)?;
            if matches!(
                server.state,
                LifecycleState::InstallConsentRequired
                    | LifecycleState::Installing
                    | LifecycleState::Uninstalling
                    | LifecycleState::Uninstalled
            ) {
                return Err(ManagerError::InvalidState(server.state));
            }
        }
        self.server_mut(server_id)?.enabled = true;
        self.transition(server_id, LifecycleState::Connecting)?;
        let definition = self.server(server_id)?.definition.clone();
        match adapter.connect(&definition) {
            Ok(handshake) => {
                let Some(negotiated) = negotiate_protocol(
                    &definition.supported_protocols,
                    &handshake.server_protocols,
                ) else {
                    self.adapter_failure(
                        server_id,
                        AdapterError {
                            code: "protocol_mismatch".into(),
                            message: "No mutually supported MCP protocol revision".into(),
                            authentication_required: false,
                        },
                        LifecycleState::Crashed,
                    );
                    self.server_mut(server_id)?.enabled = true;
                    return Err(ManagerError::ProtocolMismatch);
                };
                let catalog = match normalize_catalog(
                    &definition.namespace,
                    handshake.catalog,
                    self.catalog_names_excluding(server_id),
                ) {
                    Ok(catalog) => catalog,
                    Err(error) => {
                        self.adapter_failure(
                            server_id,
                            AdapterError {
                                code: "invalid_catalog".into(),
                                message: error.to_string(),
                                authentication_required: false,
                            },
                            LifecycleState::Crashed,
                        );
                        self.server_mut(server_id)?.enabled = true;
                        return Err(error);
                    }
                };
                let now = Utc::now();
                let server = self.server_mut(server_id)?;
                server.enabled = true;
                server.state = LifecycleState::Connected;
                server.negotiated_protocol = Some(negotiated);
                server.catalog = catalog;
                server.health = Some(HealthStatus {
                    healthy: true,
                    checked_at: now,
                    latency_ms: Some(handshake.latency_ms),
                    error_rate: 0.0,
                    consecutive_failures: 0,
                    message: "Connected".into(),
                });
                server.last_connected_at = Some(now);
                seed_conservative_permissions(server);
                push_log(server, LogLevel::Info, "MCP initialization completed");
                self.audit(server_id, "server.connected", "success", BTreeMap::new());
                Ok(())
            }
            Err(error) => {
                let code = error.code.clone();
                let state = if error.authentication_required {
                    LifecycleState::AuthenticationRequired
                } else {
                    LifecycleState::Crashed
                };
                self.adapter_failure(server_id, error, state);
                // `enabled` is desired configuration state. Preserve it so a
                // transient crash is retried after the next native restart.
                self.server_mut(server_id)?.enabled = true;
                Err(ManagerError::Adapter(code))
            }
        }
    }

    /// Disconnects and disables a server without deleting configuration.
    pub fn disable(
        &mut self,
        server_id: Uuid,
        adapter: &mut impl RuntimeAdapter,
    ) -> Result<(), ManagerError> {
        let definition = self.server(server_id)?.definition.clone();
        if !matches!(
            self.server(server_id)?.state,
            LifecycleState::Connected
                | LifecycleState::Degraded
                | LifecycleState::Crashed
                | LifecycleState::AuthenticationRequired
                | LifecycleState::UpdateAvailable
                | LifecycleState::RollbackAvailable
        ) {
            return Err(ManagerError::InvalidState(self.server(server_id)?.state));
        }
        if let Err(error) = adapter.disconnect(&definition) {
            let code = error.code.clone();
            self.adapter_failure(server_id, error, LifecycleState::Crashed);
            return Err(ManagerError::Adapter(code));
        }
        let server = self.server_mut(server_id)?;
        server.enabled = false;
        server.state = LifecycleState::Disabled;
        server.health = None;
        server.negotiated_protocol = None;
        push_log(server, LogLevel::Info, "Server disabled");
        self.audit(server_id, "server.disabled", "success", BTreeMap::new());
        Ok(())
    }

    /// Reconnects a server. Disconnect errors are logged but do not prevent a
    /// recovery attempt for crashed sessions.
    pub fn restart(
        &mut self,
        server_id: Uuid,
        adapter: &mut impl RuntimeAdapter,
    ) -> Result<(), ManagerError> {
        let definition = self.server(server_id)?.definition.clone();
        let _ = adapter.disconnect(&definition);
        {
            let server = self.server_mut(server_id)?;
            server.state = LifecycleState::Disabled;
            server.enabled = false;
            server.negotiated_protocol = None;
            push_log(server, LogLevel::Info, "Restart requested");
        }
        self.audit(
            server_id,
            "server.restart_requested",
            "pending",
            BTreeMap::new(),
        );
        self.connect(server_id, adapter)
    }

    /// Samples runtime health and degrades after repeated failure.
    pub fn check_health(
        &mut self,
        server_id: Uuid,
        adapter: &mut impl RuntimeAdapter,
    ) -> Result<HealthStatus, ManagerError> {
        let definition = self.server(server_id)?.definition.clone();
        if !matches!(
            self.server(server_id)?.state,
            LifecycleState::Connected | LifecycleState::Degraded
        ) {
            return Err(ManagerError::InvalidState(self.server(server_id)?.state));
        }
        let prior = self.server(server_id)?.health.clone();
        let sample = match adapter.health(&definition) {
            Ok(latency_ms) => HealthStatus {
                healthy: true,
                checked_at: Utc::now(),
                latency_ms: Some(latency_ms),
                error_rate: prior.as_ref().map_or(0.0, |status| status.error_rate * 0.8),
                consecutive_failures: 0,
                message: "Healthy".into(),
            },
            Err(error) => {
                let failures = prior
                    .as_ref()
                    .map_or(1, |status| status.consecutive_failures.saturating_add(1));
                HealthStatus {
                    healthy: false,
                    checked_at: Utc::now(),
                    latency_ms: None,
                    error_rate: prior
                        .as_ref()
                        .map_or(1.0, |status| (status.error_rate * 0.8 + 0.2).min(1.0)),
                    consecutive_failures: failures,
                    message: sanitize_adapter_message(&error.message),
                }
            }
        };
        let server = self.server_mut(server_id)?;
        server.state = if sample.healthy {
            LifecycleState::Connected
        } else {
            LifecycleState::Degraded
        };
        server.health = Some(sample.clone());
        push_log(
            server,
            if sample.healthy {
                LogLevel::Debug
            } else {
                LogLevel::Warn
            },
            &sample.message,
        );
        self.audit(
            server_id,
            "server.health_checked",
            if sample.healthy {
                "healthy"
            } else {
                "degraded"
            },
            BTreeMap::new(),
        );
        Ok(sample)
    }

    /// Records an available update without performing it.
    pub fn offer_update(&mut self, server_id: Uuid, plan: UpdatePlan) -> Result<(), ManagerError> {
        validate_install_recipe(&plan.recipe)?;
        if plan.target_version.trim().is_empty() {
            return Err(ManagerError::InvalidDefinition(
                "target version is blank".into(),
            ));
        }
        let server = self.server_mut(server_id)?;
        server.pending_update = Some(plan);
        server.state = LifecycleState::UpdateAvailable;
        push_log(server, LogLevel::Info, "Update is available");
        self.audit(
            server_id,
            "server.update_available",
            "success",
            BTreeMap::new(),
        );
        Ok(())
    }

    /// Returns the exact update command digest/text for GUI confirmation.
    pub fn update_consent_preview(
        &self,
        server_id: Uuid,
    ) -> Result<(String, String), ManagerError> {
        let plan = self
            .server(server_id)?
            .pending_update
            .as_ref()
            .ok_or(ManagerError::InvalidState(self.server(server_id)?.state))?;
        Ok((plan.recipe.digest(), plan.recipe.display_command()))
    }

    /// Applies an update after exact-command consent and retains the old release.
    pub fn apply_update(
        &mut self,
        server_id: Uuid,
        consent: &OperationConsent,
        adapter: &mut impl PackageAdapter,
    ) -> Result<(), ManagerError> {
        let plan = self
            .server(server_id)?
            .pending_update
            .clone()
            .ok_or(ManagerError::InvalidState(self.server(server_id)?.state))?;
        verify_consent(
            consent,
            &plan.recipe.digest(),
            &plan.recipe.display_command(),
        )?;
        self.transition(server_id, LifecycleState::Updating)?;
        match adapter.install(&plan.recipe) {
            Ok(mut release) => {
                release.version = plan.target_version;
                let server = self.server_mut(server_id)?;
                if let Some(previous) = server.current_release.take() {
                    server.release_history.push(previous);
                    if server.release_history.len() > 3 {
                        server.release_history.remove(0);
                    }
                }
                server.current_release = Some(release);
                server.pending_update = None;
                server.state = LifecycleState::RollbackAvailable;
                push_log(
                    server,
                    LogLevel::Info,
                    "Update installed; reconnect required",
                );
                self.audit(server_id, "server.updated", "success", BTreeMap::new());
                Ok(())
            }
            Err(error) => {
                let code = error.code.clone();
                self.adapter_failure(server_id, error, LifecycleState::UpdateAvailable);
                Err(ManagerError::Adapter(code))
            }
        }
    }

    /// Returns the consent digest/text for the latest rollback operation.
    pub fn rollback_consent_preview(
        &self,
        server_id: Uuid,
    ) -> Result<(String, String), ManagerError> {
        let server = self.server(server_id)?;
        let release = server
            .release_history
            .last()
            .ok_or(ManagerError::RollbackUnavailable)?;
        let text = format!("Rollback {} to {}", server.definition.name, release.version);
        Ok((digest_text(&text), text))
    }

    /// Restores the most recent retained release after operation-bound consent.
    pub fn rollback(
        &mut self,
        server_id: Uuid,
        consent: &OperationConsent,
        adapter: &mut impl PackageAdapter,
    ) -> Result<(), ManagerError> {
        let (digest, text) = self.rollback_consent_preview(server_id)?;
        verify_consent(consent, &digest, &text)?;
        let release = self
            .server(server_id)?
            .release_history
            .last()
            .cloned()
            .ok_or(ManagerError::RollbackUnavailable)?;
        let recipe = release
            .recipe
            .clone()
            .ok_or(ManagerError::RollbackUnavailable)?;
        let installed = adapter
            .install(&recipe)
            .map_err(|error| ManagerError::Adapter(error.code))?;
        let server = self.server_mut(server_id)?;
        server.release_history.pop();
        if let Some(current) = server.current_release.take() {
            server.release_history.push(current);
        }
        server.current_release = Some(InstalledRelease {
            version: release.version,
            ..installed
        });
        server.state = LifecycleState::Disabled;
        push_log(
            server,
            LogLevel::Info,
            "Rollback completed; reconnect required",
        );
        self.audit(server_id, "server.rolled_back", "success", BTreeMap::new());
        Ok(())
    }

    /// Returns the exact text/digest required to confirm uninstall.
    pub fn uninstall_consent_preview(
        &self,
        server_id: Uuid,
    ) -> Result<(String, String), ManagerError> {
        let server = self.server(server_id)?;
        let text = format!(
            "Uninstall MCP server {} ({})",
            server.definition.name, server.definition.id
        );
        Ok((digest_text(&text), text))
    }

    /// Uninstalls native artifacts after explicit operation-bound consent. The
    /// tombstone remains auditable until the GUI chooses to purge it.
    pub fn uninstall(
        &mut self,
        server_id: Uuid,
        consent: &OperationConsent,
        adapter: &mut impl PackageAdapter,
    ) -> Result<(), ManagerError> {
        let (digest, text) = self.uninstall_consent_preview(server_id)?;
        verify_consent(consent, &digest, &text)?;
        let definition = self.server(server_id)?.definition.clone();
        let previous_state = self.server(server_id)?.state;
        let previous_enabled = self.server(server_id)?.enabled;
        self.transition(server_id, LifecycleState::Uninstalling)?;
        if let Err(error) = adapter.uninstall(&definition) {
            let code = error.code;
            let server = self.server_mut(server_id)?;
            server.state = previous_state;
            server.enabled = previous_enabled;
            push_log(
                server,
                LogLevel::Error,
                "Uninstall failed; configuration retained",
            );
            self.audit(
                server_id,
                "server.uninstall_failed",
                "failure",
                BTreeMap::from([("code".into(), code.clone())]),
            );
            return Err(ManagerError::Adapter(code));
        }
        let server = self.server_mut(server_id)?;
        server.state = LifecycleState::Uninstalled;
        server.enabled = false;
        server.negotiated_protocol = None;
        server.catalog = CapabilityCatalog::default();
        server.permissions.clear();
        server.current_release = None;
        server.release_history.clear();
        server.pending_update = None;
        push_log(server, LogLevel::Info, "Server uninstalled");
        self.audit(server_id, "server.uninstalled", "success", BTreeMap::new());
        Ok(())
    }

    /// Permanently removes only an already-uninstalled tombstone.
    pub fn purge_tombstone(&mut self, server_id: Uuid) -> Result<(), ManagerError> {
        if self.server(server_id)?.state != LifecycleState::Uninstalled {
            return Err(ManagerError::InvalidState(self.server(server_id)?.state));
        }
        self.servers.remove(&server_id);
        self.audit(
            server_id,
            "server.tombstone_purged",
            "success",
            BTreeMap::new(),
        );
        Ok(())
    }

    /// Creates/replaces a per-tool rule. Tool names are resolved names, such as
    /// `github.search_issues`.
    pub fn set_permission(
        &mut self,
        server_id: Uuid,
        rule: ToolPermissionRule,
    ) -> Result<(), ManagerError> {
        validate_permission(&rule)?;
        let server = self.server_mut(server_id)?;
        if !server
            .catalog
            .tools
            .iter()
            .any(|tool| tool.resolved_name == rule.tool)
        {
            return Err(ManagerError::MissingTool(rule.tool));
        }
        server
            .permissions
            .retain(|existing| !(existing.tool == rule.tool && existing.scope == rule.scope));
        server.permissions.push(rule);
        self.audit(
            server_id,
            "tool.permission_changed",
            "success",
            BTreeMap::new(),
        );
        Ok(())
    }

    /// Replace the project and agent scopes selected in the GUI.
    pub fn set_scopes(
        &mut self,
        server_id: Uuid,
        project_scopes: BTreeSet<String>,
        agent_scopes: BTreeSet<String>,
    ) -> Result<(), ManagerError> {
        if project_scopes
            .iter()
            .chain(&agent_scopes)
            .any(|scope| scope.trim().is_empty() || scope.len() > 512 || scope.contains('\0'))
        {
            return Err(ManagerError::InvalidDefinition(
                "scope identifiers must be non-empty and bounded".into(),
            ));
        }
        let server = self.server_mut(server_id)?;
        server.definition.project_scopes = project_scopes;
        server.definition.agent_scopes = agent_scopes;
        self.audit(
            server_id,
            "server.scopes_changed",
            "success",
            BTreeMap::new(),
        );
        Ok(())
    }

    /// Normalizes process-bound lifecycle state after the desktop host starts.
    /// Returns server IDs whose persisted desired-enabled state should be
    /// synchronized into the newly created MCP runtime.
    pub fn recover_after_restart(&mut self) -> Vec<Uuid> {
        let mut reconnect = Vec::new();
        let mut recovered = Vec::new();
        for (id, server) in &mut self.servers {
            let previous = server.state;
            match server.state {
                LifecycleState::Installing => {
                    server.state = LifecycleState::InstallConsentRequired;
                    server.enabled = false;
                }
                LifecycleState::Updating => {
                    server.state = if server.pending_update.is_some() {
                        LifecycleState::UpdateAvailable
                    } else {
                        LifecycleState::Crashed
                    };
                    server.enabled = false;
                }
                LifecycleState::Uninstalling => {
                    server.state = LifecycleState::Crashed;
                    server.enabled = false;
                }
                LifecycleState::Draft
                | LifecycleState::InstallConsentRequired
                | LifecycleState::Disabled
                | LifecycleState::Uninstalled => {
                    server.enabled = false;
                }
                _ if server.enabled => {
                    server.state = LifecycleState::Connecting;
                    server.negotiated_protocol = None;
                    server.health = None;
                    reconnect.push(*id);
                }
                _ => {}
            }
            if server.state != previous {
                push_log(
                    server,
                    LogLevel::Info,
                    "Recovered persisted MCP state after application restart",
                );
                recovered.push(*id);
            }
        }
        for id in recovered {
            self.audit(id, "server.restart_recovered", "success", BTreeMap::new());
        }
        reconnect
    }

    /// IDs configured to reconnect whenever the native runtime is recreated.
    #[must_use]
    pub fn enabled_server_ids(&self) -> Vec<Uuid> {
        self.servers
            .iter()
            .filter_map(|(id, server)| server.enabled.then_some(*id))
            .collect()
    }

    /// Records a failed native-runtime restoration while preserving desired
    /// enablement for a future retry. Adapter details are sanitized.
    pub fn record_restore_failure(
        &mut self,
        server_id: Uuid,
        message: &str,
    ) -> Result<(), ManagerError> {
        let message = sanitize_adapter_message(message);
        let server = self.server_mut(server_id)?;
        server.enabled = true;
        server.state = LifecycleState::Degraded;
        server.negotiated_protocol = None;
        server.health = Some(HealthStatus {
            healthy: false,
            checked_at: Utc::now(),
            latency_ms: None,
            error_rate: 1.0,
            consecutive_failures: server
                .health
                .as_ref()
                .map_or(1, |health| health.consecutive_failures.saturating_add(1)),
            message: message.clone(),
        });
        push_log(server, LogLevel::Warn, &message);
        self.audit(
            server_id,
            "server.restore_failed",
            "degraded",
            BTreeMap::new(),
        );
        Ok(())
    }

    /// Validates and normalizes a tool call for the native `ToolGateway`.
    /// Arguments are deliberately excluded from the manager's audit event.
    pub fn prepare_tool_call(
        &mut self,
        server_id: Uuid,
        resolved_tool: &str,
        arguments: Value,
        context: &InvocationContext,
    ) -> Result<ToolRoute, ManagerError> {
        let (tool, protocol, rule) = {
            let server = self.server(server_id)?;
            if server.state != LifecycleState::Connected || !server.enabled {
                return Err(ManagerError::InvalidState(server.state));
            }
            let tool = server
                .catalog
                .tools
                .iter()
                .find(|tool| tool.resolved_name == resolved_tool)
                .cloned()
                .ok_or_else(|| ManagerError::MissingTool(resolved_tool.into()))?;
            validate_json_arguments(&tool.input_schema, &arguments)?;
            let protocol = server
                .negotiated_protocol
                .clone()
                .ok_or(ManagerError::ProtocolMismatch)?;
            let rule = resolve_permission(&server.permissions, resolved_tool, context);
            (tool, protocol, rule)
        };
        if rule.decision == PermissionDecision::Deny {
            self.audit(
                server_id,
                "tool.route_denied",
                "denied",
                BTreeMap::from([("tool".into(), resolved_tool.into())]),
            );
            return Err(ManagerError::ToolDenied(resolved_tool.into()));
        }
        let requires_approval = rule.decision == PermissionDecision::Ask
            || !tool.annotations.read_only
            || tool.annotations.destructive
            || tool.annotations.open_world;
        let request = GatewayToolRequest {
            request_id: Uuid::new_v4(),
            server_id,
            tool_name: tool.name,
            resolved_name: tool.resolved_name,
            arguments,
            protocol_version: protocol,
            execution_zone: rule.execution_zone,
            timeout_ms: rule.timeout_ms,
            max_output_bytes: rule.max_output_bytes,
            requires_approval,
            destructive: tool.annotations.destructive,
            open_world: tool.annotations.open_world,
        };
        self.audit(
            server_id,
            "tool.routed_to_gateway",
            if requires_approval {
                "approval_required"
            } else {
                "ready"
            },
            BTreeMap::from([("tool".into(), resolved_tool.into())]),
        );
        if requires_approval && !context.user_confirmed {
            Ok(ToolRoute::ApprovalRequired(request))
        } else {
            Ok(ToolRoute::Ready(request))
        }
    }

    /// Exports definitions and policy without native credential values.
    #[must_use]
    pub fn export_secret_free(&self) -> SecretFreeExport {
        SecretFreeExport {
            schema_version: 1,
            exported_at: Utc::now(),
            servers: self
                .servers
                .values()
                .filter(|server| server.state != LifecycleState::Uninstalled)
                .map(|server| server.definition.clone())
                .collect(),
            permissions: self
                .servers
                .iter()
                .filter(|(_, server)| server.state != LifecycleState::Uninstalled)
                .map(|(id, server)| (*id, server.permissions.clone()))
                .collect(),
        }
    }

    /// Produces a GUI snapshot with newest audit events first.
    #[must_use]
    pub fn snapshot(&self) -> ManagerSnapshot {
        ManagerSnapshot {
            servers: self.servers.values().cloned().collect(),
            audit_events: self.audit_events.iter().rev().cloned().collect(),
            protocol_version: CURRENT_PROTOCOL_VERSION.into(),
        }
    }

    /// Returns a managed server.
    pub fn server(&self, id: Uuid) -> Result<&ManagedServer, ManagerError> {
        self.servers.get(&id).ok_or(ManagerError::MissingServer(id))
    }

    fn server_mut(&mut self, id: Uuid) -> Result<&mut ManagedServer, ManagerError> {
        self.servers
            .get_mut(&id)
            .ok_or(ManagerError::MissingServer(id))
    }

    fn transition(&mut self, id: Uuid, next: LifecycleState) -> Result<(), ManagerError> {
        self.server_mut(id)?.state = next;
        Ok(())
    }

    fn catalog_names_excluding(&self, server_id: Uuid) -> BTreeSet<String> {
        self.servers
            .iter()
            .filter(|(id, _)| **id != server_id)
            .flat_map(|(_, server)| server.catalog.tools.iter())
            .map(|tool| tool.resolved_name.clone())
            .collect()
    }

    fn adapter_failure(&mut self, id: Uuid, error: AdapterError, state: LifecycleState) {
        if let Ok(server) = self.server_mut(id) {
            server.state = state;
            server.enabled = false;
            push_log(
                server,
                LogLevel::Error,
                &format!(
                    "{}: {}",
                    error.code,
                    sanitize_adapter_message(&error.message)
                ),
            );
        }
        self.audit(
            id,
            "server.adapter_error",
            "failure",
            BTreeMap::from([("code".into(), error.code)]),
        );
    }

    fn audit(
        &mut self,
        server_id: Uuid,
        event_type: &str,
        outcome: &str,
        metadata: BTreeMap<String, String>,
    ) {
        self.audit_events.push_back(AuditEvent {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            server_id: Some(server_id),
            event_type: event_type.into(),
            outcome: outcome.into(),
            metadata,
        });
        while self.audit_events.len() > MAX_AUDIT_EVENTS {
            self.audit_events.pop_front();
        }
    }
}

/// Imports common Claude Desktop, `OpenCode`, or generic `mcpServers` JSON.
/// Credential-looking environment/header values are discarded, never returned.
pub fn import_server_json(
    input: &str,
    source: ImportSource,
) -> Result<ImportPreview, ManagerError> {
    let document: Value = serde_json::from_str(input)
        .map_err(|error| ManagerError::InvalidImport(error.to_string()))?;
    let servers = find_import_servers(&document, source)
        .ok_or_else(|| ManagerError::InvalidImport("no MCP server map was found".into()))?;
    let mut preview = ImportPreview::default();
    for (name, raw) in servers {
        if !raw.is_object() {
            preview.issues.push(import_issue(
                name,
                "server",
                "invalid_shape",
                "Server entry is not an object",
            ));
            continue;
        }
        let namespace = unique_import_namespace(name, &preview.definitions);
        let mut issues = Vec::new();
        let transport = if let Some(command) = raw.get("command").and_then(Value::as_str) {
            let arguments = raw
                .get("args")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let environment = import_environment(name, raw.get("env"), &mut issues);
            TransportDefinition::Stdio {
                executable: command.into(),
                arguments,
                working_directory: raw.get("cwd").and_then(Value::as_str).map(str::to_owned),
                environment,
            }
        } else if let Some(endpoint) = raw
            .get("url")
            .or_else(|| raw.get("endpoint"))
            .and_then(Value::as_str)
        {
            let legacy = raw
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| matches!(kind, "sse" | "http-sse"));
            let headers = import_headers(name, raw.get("headers"), &mut issues);
            if legacy {
                TransportDefinition::LegacySse {
                    endpoint: endpoint.into(),
                    headers,
                    oauth: None,
                }
            } else {
                TransportDefinition::StreamableHttp {
                    endpoint: endpoint.into(),
                    stateless: raw
                        .get("stateless")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                    headers,
                    oauth: None,
                }
            }
        } else {
            preview.issues.push(import_issue(
                name,
                "transport",
                "missing_transport",
                "Provide either a structured command or HTTP endpoint",
            ));
            continue;
        };
        let source_name = match source {
            ImportSource::ClaudeDesktop => "Claude Desktop",
            ImportSource::OpenCode => "OpenCode",
            ImportSource::Generic => "Generic JSON",
        };
        let mut protocols = BTreeSet::new();
        protocols.insert(ProtocolVersion::current());
        protocols.insert(ProtocolVersion::new("2025-06-18")?);
        protocols.insert(ProtocolVersion::new("2024-11-05")?);
        let definition = ServerDefinition {
            id: Uuid::new_v4(),
            name: name.clone(),
            namespace,
            description: format!("Imported from {source_name}; review before enabling"),
            source: ServerSource::Imported {
                application: source_name.into(),
            },
            transport,
            supported_protocols: protocols,
            preferred_protocol: ProtocolVersion::current(),
            install: None,
            project_scopes: BTreeSet::new(),
            agent_scopes: BTreeSet::new(),
            tags: BTreeSet::from(["imported".into(), "review-required".into()]),
        };
        match validate_definition(&definition) {
            Ok(()) => preview.definitions.push(definition),
            Err(error) => issues.push(import_issue(
                name,
                "definition",
                "validation_failed",
                &error.to_string(),
            )),
        }
        preview.issues.extend(issues);
    }
    Ok(preview)
}

/// Validates a complete server definition before persistence.
pub fn validate_definition(definition: &ServerDefinition) -> Result<(), ManagerError> {
    if definition.name.trim().is_empty() {
        return Err(ManagerError::InvalidDefinition("name is blank".into()));
    }
    if sanitize_identifier(&definition.namespace) != definition.namespace
        || definition.namespace.is_empty()
    {
        return Err(ManagerError::InvalidDefinition(
            "namespace must contain lowercase letters, numbers, and underscores".into(),
        ));
    }
    if definition.supported_protocols.is_empty()
        || !definition
            .supported_protocols
            .contains(&definition.preferred_protocol)
    {
        return Err(ManagerError::InvalidDefinition(
            "preferred protocol must be present in supported protocols".into(),
        ));
    }
    validate_transport(&definition.transport)?;
    if let Some(recipe) = &definition.install {
        validate_install_recipe(recipe)?;
    }
    Ok(())
}

fn validate_transport(transport: &TransportDefinition) -> Result<(), ManagerError> {
    match transport {
        TransportDefinition::Stdio {
            executable,
            environment,
            ..
        } => {
            if executable.trim().is_empty() || executable.contains('\0') {
                return Err(ManagerError::InvalidTransport(
                    "stdio executable is blank or invalid".into(),
                ));
            }
            let mut names = BTreeSet::new();
            for binding in environment {
                validate_binding(&binding.name, &binding.value, true)?;
                if !names.insert(binding.name.to_ascii_uppercase()) {
                    return Err(ManagerError::InvalidTransport(format!(
                        "duplicate environment binding {}",
                        binding.name
                    )));
                }
            }
        }
        TransportDefinition::StreamableHttp {
            endpoint, headers, ..
        }
        | TransportDefinition::LegacySse {
            endpoint, headers, ..
        } => {
            let url = Url::parse(endpoint)
                .map_err(|error| ManagerError::InvalidTransport(error.to_string()))?;
            let loopback = url
                .host_str()
                .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1"));
            if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
                return Err(ManagerError::InvalidTransport(
                    "remote MCP endpoints must use HTTPS".into(),
                ));
            }
            let mut names = BTreeSet::new();
            for binding in headers {
                validate_binding(&binding.name, &binding.value, false)?;
                if !names.insert(binding.name.to_ascii_lowercase()) {
                    return Err(ManagerError::InvalidTransport(format!(
                        "duplicate header {}",
                        binding.name
                    )));
                }
            }
        }
    }
    Ok(())
}

fn validate_binding(
    name: &str,
    value: &BindingValue,
    environment: bool,
) -> Result<(), ManagerError> {
    if name.trim().is_empty() || name.contains(['\r', '\n', '\0']) {
        return Err(ManagerError::InvalidTransport(
            "invalid binding name".into(),
        ));
    }
    let sensitive = is_sensitive_name(name);
    if sensitive && matches!(value, BindingValue::NonSecret { .. }) {
        return Err(ManagerError::InvalidTransport(format!(
            "credential-like {} {} must use an OS-keychain reference",
            if environment {
                "environment variable"
            } else {
                "header"
            },
            name
        )));
    }
    match value {
        BindingValue::NonSecret { value } if value.contains('\0') => Err(
            ManagerError::InvalidTransport("binding contains a null byte".into()),
        ),
        BindingValue::Keychain { reference }
            if reference.reference_id.trim().is_empty() || reference.service.trim().is_empty() =>
        {
            Err(ManagerError::InvalidTransport(
                "keychain reference is incomplete".into(),
            ))
        }
        _ => Ok(()),
    }
}

fn validate_install_recipe(recipe: &InstallRecipe) -> Result<(), ManagerError> {
    if recipe.program.trim().is_empty() || recipe.program.contains('\0') {
        return Err(ManagerError::InvalidDefinition(
            "install program is blank or invalid".into(),
        ));
    }
    if recipe
        .arguments
        .iter()
        .any(|argument| argument.contains('\0'))
    {
        return Err(ManagerError::InvalidDefinition(
            "install argument contains a null byte".into(),
        ));
    }
    if let Some(digest) = &recipe.expected_artifact_sha256
        && (digest.len() != 64
            || !digest
                .chars()
                .all(|character| character.is_ascii_hexdigit()))
    {
        return Err(ManagerError::InvalidDefinition(
            "artifact SHA-256 is invalid".into(),
        ));
    }
    if let Some(source) = &recipe.source_url {
        let url = Url::parse(source)
            .map_err(|error| ManagerError::InvalidDefinition(error.to_string()))?;
        if url.scheme() != "https" {
            return Err(ManagerError::InvalidDefinition(
                "package source must use HTTPS".into(),
            ));
        }
    }
    Ok(())
}

fn validate_permission(rule: &ToolPermissionRule) -> Result<(), ManagerError> {
    if rule.tool.trim().is_empty()
        || rule.execution_zone.trim().is_empty()
        || rule.max_calls_per_minute == 0
        || rule.timeout_ms == 0
        || rule.max_output_bytes == 0
    {
        return Err(ManagerError::InvalidDefinition(
            "permission rule has an empty or zero limit".into(),
        ));
    }
    Ok(())
}

fn verify_consent(
    consent: &OperationConsent,
    digest: &str,
    displayed_text: &str,
) -> Result<(), ManagerError> {
    if !consent.user_confirmed {
        return Err(ManagerError::ConsentRequired);
    }
    if consent.operation_digest != digest || consent.displayed_text != displayed_text {
        return Err(ManagerError::ConsentMismatch);
    }
    Ok(())
}

fn normalize_catalog(
    namespace: &str,
    mut catalog: CapabilityCatalog,
    mut occupied: BTreeSet<String>,
) -> Result<CapabilityCatalog, ManagerError> {
    let mut local_names = BTreeSet::new();
    for tool in &mut catalog.tools {
        let normalized = sanitize_identifier(&tool.name);
        if normalized.is_empty() {
            return Err(ManagerError::InvalidDefinition(format!(
                "tool name is invalid: {}",
                tool.name
            )));
        }
        let base = format!("{namespace}.{normalized}");
        let mut resolved = base.clone();
        let mut suffix = 2_u32;
        while occupied.contains(&resolved) || local_names.contains(&resolved) {
            resolved = format!("{base}_{suffix}");
            suffix = suffix.saturating_add(1);
        }
        tool.resolved_name.clone_from(&resolved);
        local_names.insert(resolved.clone());
        occupied.insert(resolved);
        if !tool.input_schema.is_object() {
            tool.input_schema = serde_json::json!({ "type": "object", "properties": {} });
        }
    }
    Ok(catalog)
}

fn seed_conservative_permissions(server: &mut ManagedServer) {
    let existing: BTreeSet<_> = server
        .permissions
        .iter()
        .map(|rule| (rule.tool.clone(), rule.scope.clone()))
        .collect();
    for tool in &server.catalog.tools {
        let key = (tool.resolved_name.clone(), PermissionScope::Global);
        if existing.contains(&key) {
            continue;
        }
        server.permissions.push(ToolPermissionRule {
            tool: tool.resolved_name.clone(),
            scope: PermissionScope::Global,
            decision: PermissionDecision::Ask,
            execution_zone: "mcp-restricted".into(),
            max_calls_per_minute: 30,
            timeout_ms: 30_000,
            max_output_bytes: 1_048_576,
        });
    }
}

fn resolve_permission(
    rules: &[ToolPermissionRule],
    tool_name: &str,
    context: &InvocationContext,
) -> ToolPermissionRule {
    let candidates = [
        context
            .agent_id
            .as_ref()
            .map(|id| PermissionScope::Agent(id.clone())),
        context
            .workspace_id
            .as_ref()
            .map(|id| PermissionScope::Workspace(id.clone())),
        context
            .profile_id
            .as_ref()
            .map(|id| PermissionScope::Profile(id.clone())),
        Some(PermissionScope::Global),
    ];
    for scope in candidates.into_iter().flatten() {
        if let Some(rule) = rules
            .iter()
            .find(|rule| rule.tool == tool_name && rule.scope == scope)
        {
            return rule.clone();
        }
    }
    ToolPermissionRule {
        tool: tool_name.into(),
        scope: PermissionScope::Global,
        decision: PermissionDecision::Ask,
        execution_zone: "mcp-restricted".into(),
        max_calls_per_minute: 30,
        timeout_ms: 30_000,
        max_output_bytes: 1_048_576,
    }
}

/// Lightweight JSON Schema validation for GUI-generated tool forms. The native
/// `ToolGateway` should perform full schema validation again at execution time.
fn validate_json_arguments(schema: &Value, arguments: &Value) -> Result<(), ManagerError> {
    if schema.get("type").and_then(Value::as_str) == Some("object") && !arguments.is_object() {
        return Err(ManagerError::InvalidArguments(
            "tool expects an object".into(),
        ));
    }
    let Some(object) = arguments.as_object() else {
        return Ok(());
    };
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for field in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(field) {
                return Err(ManagerError::InvalidArguments(format!(
                    "required field is missing: {field}"
                )));
            }
        }
    }
    if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false)
        && let Some(properties) = schema.get("properties").and_then(Value::as_object)
        && let Some(field) = object.keys().find(|field| !properties.contains_key(*field))
    {
        return Err(ManagerError::InvalidArguments(format!(
            "unknown field: {field}"
        )));
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, value) in object {
            if let Some(property_schema) = properties.get(name)
                && let Some(expected) = property_schema.get("type").and_then(Value::as_str)
                && !json_type_matches(expected, value)
            {
                return Err(ManagerError::InvalidArguments(format!(
                    "field {name} must be {expected}"
                )));
            }
        }
    }
    Ok(())
}

fn json_type_matches(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn find_import_servers(document: &Value, source: ImportSource) -> Option<&Map<String, Value>> {
    let candidates: &[&[&str]] = match source {
        ImportSource::ClaudeDesktop => &[&["mcpServers"]],
        ImportSource::OpenCode => &[&["mcp", "servers"], &["mcpServers"], &["mcp"]],
        ImportSource::Generic => &[&["mcpServers"], &["mcp", "servers"], &["servers"]],
    };
    for path in candidates {
        let mut current = document;
        let mut found = true;
        for key in *path {
            let Some(next) = current.get(*key) else {
                found = false;
                break;
            };
            current = next;
        }
        if found && let Some(map) = current.as_object() {
            return Some(map);
        }
    }
    None
}

fn import_environment(
    server_name: &str,
    raw: Option<&Value>,
    issues: &mut Vec<ImportIssue>,
) -> Vec<EnvironmentBinding> {
    let Some(values) = raw.and_then(Value::as_object) else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(|(name, value)| {
            let value = value.as_str()?;
            import_binding(server_name, name, value, "env", issues).map(|value| {
                EnvironmentBinding {
                    name: name.clone(),
                    value,
                }
            })
        })
        .collect()
}

fn import_headers(
    server_name: &str,
    raw: Option<&Value>,
    issues: &mut Vec<ImportIssue>,
) -> Vec<HeaderBinding> {
    let Some(values) = raw.and_then(Value::as_object) else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(|(name, value)| {
            let value = value.as_str()?;
            import_binding(server_name, name, value, "headers", issues).map(|value| HeaderBinding {
                name: name.clone(),
                value,
            })
        })
        .collect()
}

fn import_binding(
    server_name: &str,
    name: &str,
    value: &str,
    field: &str,
    issues: &mut Vec<ImportIssue>,
) -> Option<BindingValue> {
    if let Some(alias) = placeholder_alias(value) {
        return Some(BindingValue::Keychain {
            reference: KeychainReference {
                reference_id: sanitize_identifier(alias),
                service: "personal-agent-mcp".into(),
                account_hint: name.into(),
            },
        });
    }
    if is_sensitive_name(name) || looks_like_secret(value) {
        issues.push(import_issue(
            server_name,
            &format!("{field}.{name}"),
            "secret_omitted",
            "Credential value was discarded. Reconnect it through the OS keychain.",
        ));
        return None;
    }
    Some(BindingValue::NonSecret {
        value: value.into(),
    })
}

fn placeholder_alias(value: &str) -> Option<&str> {
    value
        .strip_prefix("${")
        .and_then(|inner| inner.strip_suffix('}'))
        .or_else(|| {
            value
                .strip_prefix("{{")
                .and_then(|inner| inner.strip_suffix("}}"))
        })
}

fn unique_import_namespace(name: &str, definitions: &[ServerDefinition]) -> String {
    let base = {
        let value = sanitize_identifier(name);
        if value.is_empty() {
            "mcp_server".into()
        } else {
            value
        }
    };
    let occupied: BTreeSet<_> = definitions
        .iter()
        .map(|definition| definition.namespace.as_str())
        .collect();
    if !occupied.contains(base.as_str()) {
        return base;
    }
    for suffix in 2..=10_000 {
        let candidate = format!("{base}_{suffix}");
        if !occupied.contains(candidate.as_str()) {
            return candidate;
        }
    }
    format!("{base}_{}", Uuid::new_v4().simple())
}

fn import_issue(server_name: &str, field: &str, code: &str, message: &str) -> ImportIssue {
    ImportIssue {
        server_name: server_name.into(),
        field: field.into(),
        code: code.into(),
        message: message.into(),
    }
}

fn push_log(server: &mut ManagedServer, level: LogLevel, message: &str) {
    server.logs.push_back(ServerLog {
        timestamp: Utc::now(),
        level,
        message: sanitize_adapter_message(message),
    });
    while server.logs.len() > MAX_SERVER_LOGS {
        server.logs.pop_front();
    }
}

fn sanitize_adapter_message(message: &str) -> String {
    let mut output = message.replace(['\r', '\n'], " ");
    for marker in ["token=", "key=", "secret=", "password=", "authorization:"] {
        let mut search_from = 0;
        while let Some(relative_start) = output.to_ascii_lowercase()[search_from..].find(marker) {
            let start = search_from + relative_start;
            let value_start = start + marker.len();
            let value_end = output[value_start..]
                .find(char::is_whitespace)
                .map_or(output.len(), |offset| value_start + offset);
            output.replace_range(value_start..value_end, "[REDACTED]");
            search_from = value_start + "[REDACTED]".len();
        }
    }
    output.truncate(1_000);
    output
}

fn is_sensitive_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "authorization",
        "api_key",
        "apikey",
        "token",
        "secret",
        "password",
        "private_key",
        "client_secret",
        "cookie",
    ]
    .iter()
    .any(|part| name.contains(part))
}

fn looks_like_secret(value: &str) -> bool {
    let trimmed = value.trim();
    (trimmed.len() >= 24 && !trimmed.contains(char::is_whitespace))
        || ["sk-", "ghp_", "github_pat_", "xoxb-", "bearer "]
            .iter()
            .any(|prefix| trimmed.to_ascii_lowercase().starts_with(prefix))
}

fn sanitize_identifier(value: &str) -> String {
    let mut result = String::new();
    let mut separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            result.push(character);
            separator = false;
        } else if !separator && !result.is_empty() {
            result.push('_');
            separator = true;
        }
    }
    while result.ends_with('_') {
        result.pop();
    }
    result
}

fn shell_display_word(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-._/:=@+".contains(character))
    {
        value.into()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn digest_json(value: &impl Serialize) -> String {
    let bytes = serde_json::to_vec(value).expect("serializable operation");
    hex_digest(Sha256::digest(bytes).as_slice())
}

fn digest_text(value: &str) -> String {
    hex_digest(Sha256::digest(value.as_bytes()).as_slice())
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests;
