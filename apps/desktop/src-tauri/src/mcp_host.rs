//! Persistent GUI MCP manager bound to the authenticated `OpenCode` MCP runtime.

#![allow(clippy::needless_pass_by_value)] // Tauri deserializes and owns IPC arguments.
#![allow(clippy::large_enum_variant)] // The tagged IPC enum is serialized only at the command boundary.

use super::DesktopState;
use async_trait::async_trait;
use personal_agent_core::{EgressRecord, EgressSource};
use personal_agent_mcp_manager::{
    AdapterError, CURRENT_PROTOCOL_VERSION, CapabilityCatalog, GatewayToolRequest, ImportSource,
    InstalledRelease, InvocationContext, LifecycleState, McpManager, OperationConsent,
    PackageAdapter, ProtocolVersion, RuntimeAdapter, RuntimeHandshake, ServerDefinition,
    ToolAnnotations, ToolDescriptor, ToolRoute, TransportDefinition, import_server_json,
};
use personal_agent_policy::{
    ConsentGrant, DataZone, Effect, Idempotency, Risk, ToolDescriptor as PolicyToolDescriptor,
};
use personal_agent_tools::{ToolCall, ToolError, ToolGateway, ToolImplementation};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Emitter;
use uuid::Uuid;

pub(crate) struct McpHostState {
    path: PathBuf,
    manager: Mutex<McpManager>,
    oauth_attempts: Mutex<OAuthAttemptRegistry>,
}

const MCP_OAUTH_CALLBACK_TIMEOUT: Duration = Duration::from_secs(330);
const MCP_OAUTH_ATTEMPT_TTL: Duration = Duration::from_secs(360);

#[derive(Default)]
struct OAuthAttemptRegistry {
    pending: BTreeMap<Uuid, Instant>,
}

impl OAuthAttemptRegistry {
    fn reserve(&mut self, server_id: Uuid, now: Instant) -> bool {
        self.pending.retain(|_, started_at| {
            now.checked_duration_since(*started_at)
                .is_some_and(|elapsed| elapsed < MCP_OAUTH_ATTEMPT_TTL)
        });
        if self.pending.contains_key(&server_id) {
            return false;
        }
        self.pending.insert(server_id, now);
        true
    }

    fn clear(&mut self, server_id: Uuid) {
        self.pending.remove(&server_id);
    }
}

impl McpHostState {
    pub(crate) fn load(app_data: &Path) -> Result<Self, String> {
        let path = app_data.join("mcp/manager.json");
        let mut manager = if path.exists() {
            serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
                .map_err(|error| format!("MCP manager state is invalid: {error}"))?
        } else {
            McpManager::default()
        };
        manager.recover_after_restart();
        let state = Self {
            path,
            manager: Mutex::new(manager),
            oauth_attempts: Mutex::new(OAuthAttemptRegistry::default()),
        };
        Ok(state)
    }

    fn save(&self, manager: &McpManager) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "MCP state path has no parent".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec_pretty(manager).map_err(|error| error.to_string())?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".manager-")
            .suffix(".json.tmp")
            .tempfile_in(parent)
            .map_err(|error| error.to_string())?;
        #[cfg(unix)]
        temporary
            .as_file()
            .set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .map_err(|error| error.to_string())?;
        temporary
            .write_all(&bytes)
            .map_err(|error| error.to_string())?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| error.to_string())?;
        temporary
            .persist(&self.path)
            .map_err(|error| error.error.to_string())?;
        #[cfg(unix)]
        fs::set_permissions(
            &self.path,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ManagerAction {
    Refresh,
    AddCatalog {
        catalog_id: String,
        install_digest: Option<String>,
    },
    AddManual {
        definition: ServerDefinition,
    },
    PreviewImport {
        source: ImportSource,
        document: String,
    },
    AcceptImport {
        definitions: Vec<ServerDefinition>,
    },
    Connect {
        server_id: Uuid,
    },
    StartOauth {
        server_id: Uuid,
    },
    OpenKeychainSetup {
        server_id: Uuid,
        binding_name: Option<String>,
    },
    Disable {
        server_id: Uuid,
    },
    Restart {
        server_id: Uuid,
    },
    Health {
        server_id: Uuid,
    },
    InstallPreview {
        server_id: Uuid,
    },
    Install {
        server_id: Uuid,
        operation_digest: String,
    },
    UpdatePreview {
        server_id: Uuid,
    },
    Update {
        server_id: Uuid,
        operation_digest: String,
    },
    RollbackPreview {
        server_id: Uuid,
    },
    Rollback {
        server_id: Uuid,
        operation_digest: String,
    },
    UninstallPreview {
        server_id: Uuid,
    },
    Uninstall {
        server_id: Uuid,
        operation_digest: String,
    },
    Purge {
        server_id: Uuid,
    },
    SetScopes {
        server_id: Uuid,
        project_scopes: Vec<String>,
        agent_scopes: Vec<String>,
    },
    SetPermission {
        server_id: Uuid,
        rule: personal_agent_mcp_manager::ToolPermissionRule,
    },
    TestTool {
        server_id: Uuid,
        tool: String,
        arguments: BTreeMap<String, Value>,
        #[serde(default)]
        approval_digest: Option<String>,
    },
    Export {
        server_ids: Option<Vec<Uuid>>,
    },
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct ManagerActionResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot: Option<personal_agent_mcp_manager::ManagerSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    import_preview: Option<personal_agent_mcp_manager::ImportPreview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    test_output: Option<TestToolOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_preview: Option<OperationPreview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    export_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Debug, Serialize)]
struct OperationPreview {
    digest: String,
    display_text: String,
}

#[derive(Debug, Serialize)]
struct TestToolOutput {
    tool: String,
    duration_ms: u64,
    content: Value,
    truncated: bool,
}

#[tauri::command]
pub(crate) fn mcp_manager_snapshot(
    state: tauri::State<'_, McpHostState>,
) -> Result<personal_agent_mcp_manager::ManagerSnapshot, String> {
    state
        .manager
        .lock()
        .map(|manager| manager.snapshot())
        .map_err(|_| "MCP manager lock is poisoned".into())
}

#[tauri::command]
#[allow(clippy::too_many_lines)]
pub(crate) async fn mcp_manager_execute(
    action: Value,
    host: tauri::State<'_, McpHostState>,
    desktop: tauri::State<'_, DesktopState>,
    app: tauri::AppHandle,
) -> Result<ManagerActionResult, String> {
    let action: ManagerAction =
        serde_json::from_value(action).map_err(|error| format!("invalid MCP action: {error}"))?;
    let mut result = ManagerActionResult::default();
    match action {
        ManagerAction::Refresh => {}
        ManagerAction::AddCatalog {
            catalog_id,
            install_digest,
        } => {
            return Err(format!(
                "catalog entry {catalog_id} is not bundled{}; use Manual or Import",
                install_digest.map_or_else(String::new, |digest| format!(" (digest {digest})"))
            ));
        }
        ManagerAction::AddManual { definition } => {
            let id = with_manager(&host, |manager| manager.add_server(definition))?;
            result.message = Some(format!("MCP server {id} added. Review it, then connect."));
        }
        ManagerAction::PreviewImport { source, document } => {
            result.import_preview =
                Some(import_server_json(&document, source).map_err(|error| error.to_string())?);
        }
        ManagerAction::AcceptImport { definitions } => {
            with_manager(&host, |manager| {
                for definition in definitions {
                    manager.add_server(definition)?;
                }
                Ok(())
            })?;
            result.message = Some("Imported MCP definitions as disabled drafts.".into());
        }
        ManagerAction::Connect { server_id } => {
            if let Err(error) = connect_server(&host, &desktop, server_id).await {
                record_connection_failure(&host, &app, server_id, &error)?;
                if is_authentication_required(&host, server_id)? {
                    result.message = Some(oauth_result_message(
                        start_oauth(&host, &desktop, server_id).await?,
                    ));
                } else {
                    return Err(error);
                }
            } else {
                result.message =
                    Some("Server connected through the local OpenCode runtime.".into());
            }
        }
        ManagerAction::Restart { server_id } => {
            disconnect_runtime(&desktop, &host, server_id).await.ok();
            if let Err(error) = connect_server(&host, &desktop, server_id).await {
                record_connection_failure(&host, &app, server_id, &error)?;
                if is_authentication_required(&host, server_id)? {
                    result.message = Some(oauth_result_message(
                        start_oauth(&host, &desktop, server_id).await?,
                    ));
                } else {
                    return Err(error);
                }
            } else {
                result.message = Some("Server restarted.".into());
            }
        }
        ManagerAction::Disable { server_id } => {
            disconnect_runtime(&desktop, &host, server_id).await?;
            result.message = Some("Server disabled; configuration was retained.".into());
        }
        ManagerAction::Health { server_id } => {
            let start = Instant::now();
            let response = runtime_mcp_state(&desktop).await;
            let latency = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
            with_manager(&host, |manager| {
                let mut adapter = StaticRuntimeAdapter::health(response.is_ok(), latency);
                manager.check_health(server_id, &mut adapter).map(|_| ())
            })?;
            result.message = Some(
                if response.is_ok() {
                    "Server is healthy."
                } else {
                    "Health check failed."
                }
                .into(),
            );
        }
        ManagerAction::InstallPreview { server_id } => {
            let (digest, display_text) =
                read_manager(&host, |manager| manager.install_consent_preview(server_id))?;
            result.operation_preview = Some(OperationPreview {
                digest,
                display_text,
            });
        }
        ManagerAction::Install {
            server_id,
            operation_digest,
        } => {
            with_manager(&host, |manager| {
                let (_, display) = manager.install_consent_preview(server_id)?;
                let consent = consent(operation_digest, display);
                manager.install(server_id, &consent, &mut NativePackageAdapter)
            })?;
            result.message = Some("Installation finished. Connect the server when ready.".into());
        }
        ManagerAction::UpdatePreview { server_id } => {
            let (digest, display_text) =
                read_manager(&host, |manager| manager.update_consent_preview(server_id))?;
            result.operation_preview = Some(OperationPreview {
                digest,
                display_text,
            });
        }
        ManagerAction::Update {
            server_id,
            operation_digest,
        } => {
            with_manager(&host, |manager| {
                let (_, display) = manager.update_consent_preview(server_id)?;
                manager.apply_update(
                    server_id,
                    &consent(operation_digest, display),
                    &mut NativePackageAdapter,
                )
            })?;
            result.message = Some("Update installed with rollback retained.".into());
        }
        ManagerAction::RollbackPreview { server_id } => {
            let (digest, display_text) =
                read_manager(&host, |manager| manager.rollback_consent_preview(server_id))?;
            result.operation_preview = Some(OperationPreview {
                digest,
                display_text,
            });
        }
        ManagerAction::Rollback {
            server_id,
            operation_digest,
        } => {
            with_manager(&host, |manager| {
                let (_, display) = manager.rollback_consent_preview(server_id)?;
                manager.rollback(
                    server_id,
                    &consent(operation_digest, display),
                    &mut NativePackageAdapter,
                )
            })?;
            result.message = Some("Previous release restored.".into());
        }
        ManagerAction::UninstallPreview { server_id } => {
            let (digest, display_text) = read_manager(&host, |manager| {
                manager.uninstall_consent_preview(server_id)
            })?;
            result.operation_preview = Some(OperationPreview {
                digest,
                display_text,
            });
        }
        ManagerAction::Uninstall {
            server_id,
            operation_digest,
        } => {
            with_manager(&host, |manager| {
                let (_, display) = manager.uninstall_consent_preview(server_id)?;
                manager.uninstall(
                    server_id,
                    &consent(operation_digest, display),
                    &mut NativePackageAdapter,
                )
            })?;
            result.message = Some("Server uninstalled; an audit tombstone remains.".into());
        }
        ManagerAction::Purge { server_id } => {
            with_manager(&host, |manager| manager.purge_tombstone(server_id))?;
            result.message = Some("Uninstalled server tombstone purged.".into());
        }
        ManagerAction::SetScopes {
            server_id,
            project_scopes,
            agent_scopes,
        } => {
            with_manager(&host, |manager| {
                manager.set_scopes(
                    server_id,
                    project_scopes.into_iter().collect(),
                    agent_scopes.into_iter().collect(),
                )
            })?;
        }
        ManagerAction::SetPermission { server_id, rule } => {
            with_manager(&host, |manager| manager.set_permission(server_id, rule))?;
        }
        ManagerAction::TestTool {
            server_id,
            tool,
            arguments,
            approval_digest,
        } => {
            let arguments = Value::Object(arguments.into_iter().collect());
            let (digest, display_text) = tool_approval_preview(server_id, &tool, &arguments);
            if approval_digest
                .as_ref()
                .is_some_and(|provided| provided != &digest)
            {
                return Err("MCP tool approval no longer matches the displayed request".into());
            }
            let route = with_manager(&host, |manager| {
                manager.prepare_tool_call(
                    server_id,
                    &tool,
                    arguments.clone(),
                    &InvocationContext {
                        user_confirmed: approval_digest.is_some(),
                        ..InvocationContext::default()
                    },
                )
            })?;
            match route {
                ToolRoute::ApprovalRequired(_) => {
                    result.operation_preview = Some(OperationPreview {
                        digest,
                        display_text,
                    });
                    result.message = Some("Review and approve this MCP tool call.".into());
                }
                ToolRoute::Ready(request) => {
                    let (definition, advertised_tool) = read_manager(&host, |manager| {
                        let server = manager.server(server_id)?;
                        let advertised_tool = server
                            .catalog
                            .tools
                            .iter()
                            .find(|candidate| candidate.resolved_name == request.resolved_name)
                            .cloned()
                            .ok_or_else(|| {
                                personal_agent_mcp_manager::ManagerError::MissingTool(
                                    request.resolved_name.clone(),
                                )
                            })?;
                        Ok((server.definition.clone(), advertised_tool))
                    })?;
                    let input_bytes = serde_json::to_vec(&request.arguments)
                        .map_or(0, |value| u64::try_from(value.len()).unwrap_or(u64::MAX));
                    let destination = definition.namespace.clone();
                    let operation = request.tool_name.clone();
                    let start = Instant::now();
                    let content = execute_through_tool_gateway(
                        &definition,
                        &advertised_tool,
                        request,
                        approval_digest.is_some(),
                    )
                    .await?;
                    desktop
                        .profile
                        .lock()
                        .map_err(|_| "profile state lock is poisoned".to_owned())?
                        .record_egress(EgressRecord {
                            id: Uuid::now_v7(),
                            at: chrono::Utc::now(),
                            source: EgressSource::Mcp,
                            destination,
                            operation,
                            data_class: "tool arguments".into(),
                            size_bytes: Some(input_bytes),
                            purpose: "user-approved MCP test call".into(),
                            session_id: None,
                            scope_key: None,
                        })
                        .map_err(|error| error.to_string())?;
                    result.test_output = Some(TestToolOutput {
                        tool,
                        duration_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                        content,
                        truncated: false,
                    });
                }
            }
        }
        ManagerAction::Export { server_ids } => {
            let export = read_manager(&host, |manager| Ok(manager.export_secret_free()))?;
            let filtered = if let Some(ids) = server_ids {
                let ids = ids.into_iter().collect::<BTreeSet<_>>();
                personal_agent_mcp_manager::SecretFreeExport {
                    servers: export
                        .servers
                        .into_iter()
                        .filter(|server| ids.contains(&server.id))
                        .collect(),
                    permissions: export
                        .permissions
                        .into_iter()
                        .filter(|(id, _)| ids.contains(id))
                        .collect(),
                    ..export
                }
            } else {
                export
            };
            result.export_json =
                Some(serde_json::to_string_pretty(&filtered).map_err(|error| error.to_string())?);
        }
        ManagerAction::StartOauth { server_id } => {
            if !is_authentication_required(&host, server_id)?
                && let Err(error) = connect_server(&host, &desktop, server_id).await
            {
                record_connection_failure(&host, &app, server_id, &error)?;
                if !is_authentication_required(&host, server_id)? {
                    return Err(error);
                }
            }
            result.message = Some(oauth_result_message(
                start_oauth(&host, &desktop, server_id).await?,
            ));
        }
        ManagerAction::OpenKeychainSetup {
            server_id,
            binding_name,
        } => {
            read_manager(&host, |manager| manager.server(server_id).map(|_| ()))?;
            result.message = Some(format!(
                "Add the secret for {} in the OS keychain setup panel.",
                binding_name.unwrap_or_else(|| "this binding".into())
            ));
        }
    }
    let snapshot = read_manager(&host, |manager| Ok(manager.snapshot()))?;
    if let Err(error) = app.emit("mcp-manager://changed", snapshot.clone()) {
        tracing::warn!(%error, "MCP manager snapshot could not be emitted");
    }
    result.snapshot = Some(snapshot);
    Ok(result)
}

fn with_manager<T>(
    host: &McpHostState,
    operation: impl FnOnce(&mut McpManager) -> Result<T, personal_agent_mcp_manager::ManagerError>,
) -> Result<T, String> {
    let mut manager = host
        .manager
        .lock()
        .map_err(|_| "MCP manager lock is poisoned".to_owned())?;
    let mut candidate = manager.clone();
    let outcome = operation(&mut candidate);
    host.save(&candidate)?;
    *manager = candidate;
    outcome.map_err(|error| error.to_string())
}

fn read_manager<T>(
    host: &McpHostState,
    operation: impl FnOnce(&McpManager) -> Result<T, personal_agent_mcp_manager::ManagerError>,
) -> Result<T, String> {
    let manager = host
        .manager
        .lock()
        .map_err(|_| "MCP manager lock is poisoned".to_owned())?;
    operation(&manager).map_err(|error| error.to_string())
}

fn record_connection_failure(
    host: &McpHostState,
    app: &tauri::AppHandle,
    server_id: Uuid,
    error: &str,
) -> Result<(), String> {
    preserve_auth_or_record_failure(host, server_id, error)?;
    let snapshot = read_manager(host, |manager| Ok(manager.snapshot()))?;
    if let Err(emit_error) = app.emit("mcp-manager://changed", snapshot) {
        tracing::warn!(%emit_error, "failed MCP connection snapshot could not be emitted");
    }
    Ok(())
}

fn preserve_auth_or_record_failure(
    host: &McpHostState,
    server_id: Uuid,
    error: &str,
) -> Result<(), String> {
    if is_authentication_required(host, server_id)? {
        Ok(())
    } else {
        with_manager(host, |manager| {
            manager.record_restore_failure(server_id, error)
        })
    }
}

fn is_authentication_required(host: &McpHostState, server_id: Uuid) -> Result<bool, String> {
    read_manager(host, |manager| {
        manager
            .server(server_id)
            .map(|server| server.state == LifecycleState::AuthenticationRequired)
    })
}

async fn start_oauth(
    host: &McpHostState,
    desktop: &DesktopState,
    server_id: Uuid,
) -> Result<OAuthStartResult, String> {
    let server = read_manager(host, |manager| manager.server(server_id).cloned())?;
    if !matches!(
        server.definition.transport,
        TransportDefinition::StreamableHttp { .. } | TransportDefinition::LegacySse { .. }
    ) {
        return Err("OAuth is available only for remote MCP servers".into());
    }
    let Some(_attempt) = OAuthAttemptGuard::reserve(host, server_id)? else {
        return Ok(OAuthStartResult::AlreadyInProgress);
    };
    let directory = desktop
        .config
        .read()
        .map_err(|_| "configuration lock is poisoned".to_owned())?
        .runtime
        .working_directory
        .clone();
    // `authenticate` owns OpenCode's callback waiter for up to five minutes.
    // Clone the authenticated API client so this long wait never holds the
    // runtime lifecycle mutex and block chat/session operations.
    let runtime_api = {
        let runtime = desktop.runtime.lock().await;
        runtime.api_client().map_err(|error| error.to_string())?
    };
    runtime_api
        .request_json_with_timeout(
            reqwest::Method::POST,
            &oauth_authenticate_route(&server.definition.namespace),
            &[("directory", directory)],
            None,
            Some(MCP_OAUTH_CALLBACK_TIMEOUT),
        )
        .await
        .map_err(|error| {
            format!(
                "MCP sign-in did not complete: {error}. Close any stale or expired authorization pages, then retry Sign in."
            )
        })?;
    connect_server(host, desktop, server_id)
        .await
        .map_err(|error| {
            format!("Sign-in completed, but the MCP server could not connect: {error}")
        })?;
    Ok(OAuthStartResult::Connected)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OAuthStartResult {
    Connected,
    AlreadyInProgress,
}

fn oauth_result_message(result: OAuthStartResult) -> String {
    match result {
        OAuthStartResult::Connected => {
            "Sign-in completed and the MCP server connected.".into()
        }
        OAuthStartResult::AlreadyInProgress => {
            "Sign-in is already in progress. Finish the existing browser authorization; a second flow was not started."
                .into()
        }
    }
}

fn oauth_authenticate_route(namespace: &str) -> String {
    format!("/mcp/{namespace}/auth/authenticate")
}

struct OAuthAttemptGuard<'a> {
    host: &'a McpHostState,
    server_id: Uuid,
}

impl<'a> OAuthAttemptGuard<'a> {
    fn reserve(host: &'a McpHostState, server_id: Uuid) -> Result<Option<Self>, String> {
        let mut attempts = host
            .oauth_attempts
            .lock()
            .map_err(|_| "MCP OAuth attempt lock is poisoned".to_owned())?;
        if !attempts.reserve(server_id, Instant::now()) {
            return Ok(None);
        }
        Ok(Some(Self { host, server_id }))
    }
}

impl Drop for OAuthAttemptGuard<'_> {
    fn drop(&mut self) {
        let Ok(mut attempts) = self.host.oauth_attempts.lock() else {
            tracing::error!(
                server_id = %self.server_id,
                "MCP OAuth attempt lock is poisoned"
            );
            return;
        };
        attempts.clear(self.server_id);
    }
}

fn consent(operation_digest: String, displayed_text: String) -> OperationConsent {
    OperationConsent {
        operation_digest,
        displayed_text,
        accepted_at: chrono::Utc::now(),
        user_confirmed: true,
    }
}

fn tool_approval_preview(server_id: Uuid, tool: &str, arguments: &Value) -> (String, String) {
    let canonical = json!({
        "server_id": server_id,
        "tool": tool,
        "arguments": arguments,
    });
    let encoded = serde_json::to_vec(&canonical).expect("tool approval payload is serializable");
    let digest =
        Sha256::digest(encoded)
            .iter()
            .fold(String::with_capacity(64), |mut output, byte| {
                use std::fmt::Write as _;
                write!(&mut output, "{byte:02x}").expect("writing to a string cannot fail");
                output
            });
    let arguments = serde_json::to_string_pretty(arguments)
        .unwrap_or_else(|_| "<arguments could not be displayed>".into());
    (
        digest,
        format!("MCP tool: {tool}\nServer: {server_id}\nArguments:\n{arguments}"),
    )
}

fn policy_descriptor(tool: &ToolDescriptor) -> PolicyToolDescriptor {
    let (risk, effect) = if tool.annotations.read_only {
        (Risk::Read, Effect::Observe)
    } else if tool.annotations.destructive {
        (Risk::Irreversible, Effect::ExternalWrite)
    } else {
        // Unknown/non-read-only MCP behavior is treated as consequential and
        // externally effectful until the publisher provides safer annotations.
        (Risk::Consequential, Effect::ExternalWrite)
    };
    PolicyToolDescriptor {
        id: tool.resolved_name.clone(),
        version: CURRENT_PROTOCOL_VERSION.into(),
        description: tool.description.clone(),
        scopes: BTreeSet::from([tool.resolved_name.clone()]),
        risk,
        effect,
        idempotency: if tool.annotations.idempotent {
            Idempotency::Safe
        } else {
            Idempotency::Unsafe
        },
        // MCP currently supplies no native checkpoint/rollback contract.
        reversible: false,
        zones_read: BTreeSet::from([DataZone::UserInstruction]),
        zones_written: if effect == Effect::Observe {
            BTreeSet::new()
        } else {
            BTreeSet::from([DataZone::ConnectorData])
        },
        user_presence: effect != Effect::Observe,
    }
}

struct McpGatewayImplementation {
    descriptor: PolicyToolDescriptor,
    definition: ServerDefinition,
    tool_name: String,
    timeout_ms: u64,
}

#[async_trait]
impl ToolImplementation for McpGatewayImplementation {
    fn descriptor(&self) -> &PolicyToolDescriptor {
        &self.descriptor
    }

    fn validate_input(&self, input: &Value) -> Result<(), ToolError> {
        if input.is_object() {
            Ok(())
        } else {
            Err(ToolError::InvalidInput(
                "MCP tool arguments must be an object".into(),
            ))
        }
    }

    async fn checkpoint(&self, _call: &ToolCall) -> Result<Option<String>, ToolError> {
        Ok(None)
    }

    async fn execute(&self, call: &ToolCall) -> Result<Value, ToolError> {
        tokio::time::timeout(
            std::time::Duration::from_millis(self.timeout_ms),
            test_mcp_tool(&self.definition, &self.tool_name, call.input.clone()),
        )
        .await
        .map_err(|_| ToolError::Execution("MCP tool exceeded its policy timeout".into()))?
        .map_err(ToolError::Execution)
    }

    async fn verify(&self, _call: &ToolCall, _output: &Value) -> Result<(), ToolError> {
        // Transport helpers accept only successful MCP JSON-RPC results, which
        // is the observable postcondition for this explicit test invocation.
        Ok(())
    }
}

async fn execute_through_tool_gateway(
    definition: &ServerDefinition,
    advertised_tool: &ToolDescriptor,
    request: GatewayToolRequest,
    user_approved: bool,
) -> Result<Value, String> {
    let descriptor = policy_descriptor(advertised_tool);
    let tool_id = descriptor.id.clone();
    let effect = descriptor.effect;
    let implementation = Arc::new(McpGatewayImplementation {
        descriptor,
        definition: definition.clone(),
        tool_name: request.tool_name,
        timeout_ms: request.timeout_ms,
    });
    let mut gateway = ToolGateway::new(request.max_output_bytes);
    gateway.register(implementation);
    let goal_id = Uuid::new_v4();
    let target = definition.namespace.clone();
    let call = ToolCall {
        call_id: request.request_id,
        goal_id,
        task_id: None,
        tool_id: tool_id.clone(),
        target: target.clone(),
        input: request.arguments,
        input_zones: BTreeSet::from([DataZone::UserInstruction]),
        granted_scopes: BTreeSet::from([tool_id.clone()]),
        estimated_cost_usd: 0.0,
        background: false,
        user_present: true,
        checkpoint_available: false,
    };
    let grants = user_approved.then(|| ConsentGrant {
        id: Uuid::new_v4(),
        goal_id,
        task_id: None,
        tool_ids: BTreeSet::from([tool_id]),
        effects: BTreeSet::from([effect]),
        target_patterns: BTreeSet::from([target]),
        expires_at: chrono::Utc::now() + chrono::Duration::minutes(5),
        maximum_calls: 1,
        calls_used: 0,
        cost_ceiling_usd: Some(0.0),
        background: false,
        revoked: false,
    });
    gateway
        .call(call, grants.as_slice())
        .await
        .map(|output| output.value)
        .map_err(|error| error.to_string())
}

async fn connect_server(
    host: &McpHostState,
    desktop: &DesktopState,
    server_id: Uuid,
) -> Result<(), String> {
    let definition = read_manager(host, |manager| {
        manager
            .server(server_id)
            .map(|server| server.definition.clone())
    })?;
    let entry = opencode_mcp_entry(&definition)?;
    let directory = desktop
        .config
        .read()
        .map_err(|_| "configuration lock is poisoned".to_owned())?
        .runtime
        .working_directory
        .clone();
    let query = [("directory", directory)];
    let runtime = desktop.runtime.lock().await;
    let state = runtime
        .request_json(
            reqwest::Method::POST,
            "/mcp",
            &query,
            Some(json!({"name": definition.namespace.clone(), "config": entry})),
        )
        .await
        .map_err(|error| error.to_string())?;
    drop(runtime);
    let catalog = catalog_from_runtime(&definition.namespace, &state);
    with_manager(host, |manager| {
        let runtime_server = state.get(&definition.namespace).unwrap_or(&state);
        let runtime_status = runtime_server
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("failed");
        let mut adapter = match runtime_status {
            "connected" => {
                StaticRuntimeAdapter::connected(definition.supported_protocols.clone(), catalog)
            }
            "needs_auth" | "needs_client_registration" => {
                StaticRuntimeAdapter::connection_failure(AdapterError {
                    code: "authentication_required".into(),
                    message: "This MCP server requires browser sign-in".into(),
                    authentication_required: true,
                })
            }
            "disabled" => StaticRuntimeAdapter::connection_failure(adapter_error(
                "runtime_disabled",
                "OpenCode registered the MCP server in a disabled state",
            )),
            _ => StaticRuntimeAdapter::connection_failure(adapter_error(
                "runtime_failed",
                runtime_server
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("OpenCode could not connect the MCP server"),
            )),
        };
        manager.connect(server_id, &mut adapter)
    })
}

/// Rehydrates persisted desired-enabled definitions into the fresh `OpenCode`
/// sidecar. One broken server cannot prevent the remaining runtime from
/// completing startup.
pub(crate) async fn restore_enabled_servers(
    host: &McpHostState,
    desktop: &DesktopState,
) -> Result<personal_agent_mcp_manager::ManagerSnapshot, String> {
    let server_ids = read_manager(host, |manager| Ok(manager.enabled_server_ids()))?;
    for server_id in server_ids {
        if let Err(error) = connect_server(host, desktop, server_id).await {
            tracing::warn!(%server_id, %error, "persisted MCP server could not be restored");
            preserve_auth_or_record_failure(host, server_id, &error)?;
        }
    }
    read_manager(host, |manager| Ok(manager.snapshot()))
}

async fn disconnect_runtime(
    desktop: &DesktopState,
    host: &McpHostState,
    server_id: Uuid,
) -> Result<(), String> {
    let definition = read_manager(host, |manager| {
        manager
            .server(server_id)
            .map(|server| server.definition.clone())
    })?;
    let directory = desktop
        .config
        .read()
        .map_err(|_| "configuration lock is poisoned".to_owned())?
        .runtime
        .working_directory
        .clone();
    let runtime = desktop.runtime.lock().await;
    if let Err(error) = runtime
        .request_json(
            reqwest::Method::POST,
            &format!("/mcp/{}/disconnect", definition.namespace),
            &[("directory", directory.clone())],
            None,
        )
        .await
    {
        tracing::warn!(%error, namespace = %definition.namespace, "OpenCode MCP disconnect reported an error");
    }
    drop(runtime);
    with_manager(host, |manager| {
        manager.disable(server_id, &mut StaticRuntimeAdapter::disconnected())
    })
}

async fn runtime_mcp_state(desktop: &DesktopState) -> Result<Value, String> {
    let directory = desktop
        .config
        .read()
        .map_err(|_| "configuration lock is poisoned".to_owned())?
        .runtime
        .working_directory
        .clone();
    desktop
        .runtime
        .lock()
        .await
        .request_json(
            reqwest::Method::GET,
            "/mcp",
            &[("directory", directory)],
            None,
        )
        .await
        .map_err(|error| error.to_string())
}

fn opencode_mcp_entry(definition: &ServerDefinition) -> Result<Value, String> {
    match &definition.transport {
        TransportDefinition::Stdio {
            executable,
            arguments,
            working_directory,
            environment,
        } => {
            let mut command = vec![executable.clone()];
            command.extend(arguments.clone());
            let mut env = serde_json::Map::new();
            for binding in environment {
                match &binding.value {
                    personal_agent_mcp_manager::BindingValue::NonSecret { value } => { env.insert(binding.name.clone(), json!(value)); }
                    personal_agent_mcp_manager::BindingValue::Keychain { .. } => return Err("OpenCode-managed stdio servers cannot receive keychain bindings until you complete keychain setup".into()),
                }
            }
            Ok(
                json!({"type": "local", "command": command, "environment": env, "enabled": true, "cwd": working_directory}),
            )
        }
        TransportDefinition::StreamableHttp {
            endpoint, headers, ..
        }
        | TransportDefinition::LegacySse {
            endpoint, headers, ..
        } => {
            let mut values = serde_json::Map::new();
            for header in headers {
                match &header.value {
                    personal_agent_mcp_manager::BindingValue::NonSecret { value } => {
                        values.insert(header.name.clone(), json!(value));
                    }
                    personal_agent_mcp_manager::BindingValue::Keychain { .. } => {
                        return Err(
                            "Complete keychain/OAuth setup before connecting this remote server"
                                .into(),
                        );
                    }
                }
            }
            Ok(json!({"type": "remote", "url": endpoint, "headers": values, "enabled": true}))
        }
    }
}

fn catalog_from_runtime(namespace: &str, state: &Value) -> CapabilityCatalog {
    let server = state.get(namespace).unwrap_or(state);
    let tools = server
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| {
            let name = tool.get("name").and_then(Value::as_str)?.to_owned();
            Some(ToolDescriptor {
                resolved_name: format!("{namespace}.{name}"),
                name,
                title: tool.get("title").and_then(Value::as_str).map(str::to_owned),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                input_schema: tool
                    .get("inputSchema")
                    .or_else(|| tool.get("input_schema"))
                    .cloned()
                    .unwrap_or_else(|| json!({"type":"object"})),
                output_schema: tool
                    .get("outputSchema")
                    .or_else(|| tool.get("output_schema"))
                    .cloned(),
                annotations: ToolAnnotations::default(),
            })
        })
        .collect();
    CapabilityCatalog {
        tools,
        ..CapabilityCatalog::default()
    }
}

struct StaticRuntimeAdapter {
    handshake: Option<RuntimeHandshake>,
    connection_error: Option<AdapterError>,
    health: Result<u64, AdapterError>,
}

impl StaticRuntimeAdapter {
    fn connected(protocols: BTreeSet<ProtocolVersion>, catalog: CapabilityCatalog) -> Self {
        Self {
            handshake: Some(RuntimeHandshake {
                server_protocols: protocols,
                catalog,
                latency_ms: 1,
            }),
            connection_error: None,
            health: Ok(1),
        }
    }
    fn connection_failure(error: AdapterError) -> Self {
        Self {
            handshake: None,
            connection_error: Some(error),
            health: Ok(0),
        }
    }
    fn health(healthy: bool, latency: u64) -> Self {
        Self {
            handshake: None,
            connection_error: None,
            health: if healthy {
                Ok(latency)
            } else {
                Err(adapter_error(
                    "health_failed",
                    "OpenCode MCP health endpoint failed",
                ))
            },
        }
    }
    fn disconnected() -> Self {
        Self {
            handshake: None,
            connection_error: None,
            health: Ok(0),
        }
    }
}

impl RuntimeAdapter for StaticRuntimeAdapter {
    fn connect(
        &mut self,
        _definition: &ServerDefinition,
    ) -> Result<RuntimeHandshake, AdapterError> {
        self.handshake.take().ok_or_else(|| {
            self.connection_error.take().unwrap_or_else(|| {
                adapter_error("not_initialized", "MCP handshake was unavailable")
            })
        })
    }
    fn health(&mut self, _definition: &ServerDefinition) -> Result<u64, AdapterError> {
        self.health.clone()
    }
    fn disconnect(&mut self, _definition: &ServerDefinition) -> Result<(), AdapterError> {
        Ok(())
    }
}

fn adapter_error(code: &str, message: &str) -> AdapterError {
    AdapterError {
        code: code.into(),
        message: message.into(),
        authentication_required: false,
    }
}

struct NativePackageAdapter;

impl PackageAdapter for NativePackageAdapter {
    fn install(
        &mut self,
        recipe: &personal_agent_mcp_manager::InstallRecipe,
    ) -> Result<InstalledRelease, AdapterError> {
        let output = Command::new(&recipe.program)
            .args(&recipe.arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|_| adapter_error("spawn_failed", "installer could not start"))?;
        if !output.status.success() {
            return Err(adapter_error(
                "install_failed",
                "installer returned a non-zero status",
            ));
        }
        Ok(InstalledRelease {
            version: "installed".into(),
            installed_at: chrono::Utc::now(),
            artifact_sha256: recipe.expected_artifact_sha256.clone(),
            recipe: Some(recipe.clone()),
        })
    }

    fn uninstall(&mut self, definition: &ServerDefinition) -> Result<(), AdapterError> {
        // Manual and remote definitions have no native package artifact. Their
        // uninstall operation removes the persisted/runtime configuration only.
        let Some(recipe) = definition.install.as_ref() else {
            return Ok(());
        };
        let (program, args) = uninstall_command(recipe).ok_or_else(|| {
            adapter_error(
                "manual_uninstall",
                "this package manager has no safe automatic uninstall mapping",
            )
        })?;
        let status = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| adapter_error("uninstall_spawn_failed", "uninstaller could not start"))?;
        if status.success() {
            Ok(())
        } else {
            Err(adapter_error(
                "uninstall_failed",
                "uninstaller returned a non-zero status",
            ))
        }
    }
}

fn uninstall_command(
    recipe: &personal_agent_mcp_manager::InstallRecipe,
) -> Option<(String, Vec<String>)> {
    let package = recipe.arguments.last()?.clone();
    match recipe.program.as_str() {
        "npm" | "pnpm" | "yarn" => Some((
            recipe.program.clone(),
            vec!["uninstall".into(), "--global".into(), package],
        )),
        "pipx" => Some(("pipx".into(), vec!["uninstall".into(), package])),
        "uv" if recipe.arguments.first().is_some_and(|arg| arg == "tool") => Some((
            "uv".into(),
            vec!["tool".into(), "uninstall".into(), package],
        )),
        "cargo" => Some(("cargo".into(), vec!["uninstall".into(), package])),
        _ => None,
    }
}

async fn test_mcp_tool(
    definition: &ServerDefinition,
    tool: &str,
    arguments: Value,
) -> Result<Value, String> {
    match &definition.transport {
        TransportDefinition::StreamableHttp {
            endpoint, headers, ..
        } => {
            let mut request = reqwest::Client::new().post(endpoint).header("Accept", "application/json, text/event-stream").json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":arguments}}));
            for header in headers {
                if let personal_agent_mcp_manager::BindingValue::NonSecret { value } = &header.value
                {
                    request = request.header(&header.name, value);
                }
            }
            let response = request.send().await.map_err(|error| error.to_string())?;
            let status = response.status();
            let value = response
                .json::<Value>()
                .await
                .map_err(|error| error.to_string())?;
            if status.is_success() {
                Ok(value.get("result").cloned().unwrap_or(value))
            } else {
                Err(format!("MCP HTTP {status}"))
            }
        }
        TransportDefinition::LegacySse { .. } => {
            Err("legacy SSE tool tests use the connected OpenCode agent runtime".into())
        }
        TransportDefinition::Stdio {
            executable,
            arguments: server_args,
            environment,
            working_directory,
        } => {
            test_stdio_tool(
                executable,
                server_args,
                environment,
                working_directory.as_deref(),
                tool,
                arguments,
            )
            .await
        }
    }
}

async fn test_stdio_tool(
    executable: &str,
    server_args: &[String],
    environment: &[personal_agent_mcp_manager::EnvironmentBinding],
    cwd: Option<&str>,
    tool: &str,
    arguments: Value,
) -> Result<Value, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut command = tokio::process::Command::new(executable);
    command
        .args(server_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    for binding in environment {
        if let personal_agent_mcp_manager::BindingValue::NonSecret { value } = &binding.value {
            command.env(&binding.name, value);
        }
    }
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "MCP stdin is unavailable".to_owned())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "MCP stdout is unavailable".to_owned())?;
    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":personal_agent_mcp_manager::CURRENT_PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"Personal Agent","version":env!("CARGO_PKG_VERSION")}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":tool,"arguments":arguments}}),
    ];
    for message in messages {
        stdin
            .write_all(
                serde_json::to_string(&message)
                    .map_err(|error| error.to_string())?
                    .as_bytes(),
            )
            .await
            .map_err(|error| error.to_string())?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|error| error.to_string())?;
    }
    stdin.flush().await.map_err(|error| error.to_string())?;
    let read = async {
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await.map_err(|error| error.to_string())? {
            let value: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
            if value.get("id").and_then(Value::as_i64) == Some(2) {
                return value.get("result").cloned().ok_or_else(|| {
                    value
                        .get("error")
                        .cloned()
                        .unwrap_or(Value::String("tool call failed".into()))
                        .to_string()
                });
            }
        }
        Err("MCP server closed before returning the tool result".into())
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), read)
        .await
        .map_err(|_| "MCP tool test timed out".to_owned())?;
    let _ = child.kill().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use personal_agent_mcp_manager::{
        BindingValue, EnvironmentBinding, KeychainReference, LifecycleState, ServerSource,
    };

    fn definition(name: &str, namespace: &str) -> ServerDefinition {
        let protocols = BTreeSet::from([
            ProtocolVersion::current(),
            ProtocolVersion::new("2025-06-18").unwrap(),
        ]);
        ServerDefinition {
            id: Uuid::new_v4(),
            name: name.into(),
            namespace: namespace.into(),
            description: "Persistent test server".into(),
            source: ServerSource::Manual,
            transport: TransportDefinition::Stdio {
                executable: "mcp-test".into(),
                arguments: vec!["--stdio".into()],
                working_directory: None,
                environment: Vec::new(),
            },
            supported_protocols: protocols,
            preferred_protocol: ProtocolVersion::current(),
            install: None,
            project_scopes: BTreeSet::new(),
            agent_scopes: BTreeSet::new(),
            tags: BTreeSet::from(["test".into()]),
        }
    }

    #[test]
    fn native_auth_requirement_becomes_a_sign_in_state() {
        let mut manager = McpManager::default();
        let definition = definition("OAuth", "oauth");
        let id = definition.id;
        manager.add_server(definition).unwrap();
        let mut adapter = StaticRuntimeAdapter::connection_failure(AdapterError {
            code: "authentication_required".into(),
            message: "Browser sign-in required".into(),
            authentication_required: true,
        });
        assert!(manager.connect(id, &mut adapter).is_err());
        let server = manager.server(id).unwrap();
        assert!(server.enabled);
        assert_eq!(server.state, LifecycleState::AuthenticationRequired);
    }

    #[test]
    fn persisted_definition_and_desired_enabled_state_survive_host_reload() {
        let directory = tempfile::tempdir().unwrap();
        let host = McpHostState::load(directory.path()).unwrap();
        let definition = definition("Persistent", "persistent");
        let id = definition.id;
        let protocols = definition.supported_protocols.clone();
        with_manager(&host, |manager| manager.add_server(definition)).unwrap();
        with_manager(&host, |manager| {
            manager.connect(
                id,
                &mut StaticRuntimeAdapter::connected(protocols, CapabilityCatalog::default()),
            )
        })
        .unwrap();
        drop(host);

        let manager_path = directory.path().join("mcp/manager.json");
        let persisted_before_reload = fs::read(&manager_path).unwrap();
        let restored = McpHostState::load(directory.path()).unwrap();
        assert_eq!(fs::read(&manager_path).unwrap(), persisted_before_reload);
        let snapshot = read_manager(&restored, |manager| Ok(manager.snapshot())).unwrap();
        let server = snapshot
            .servers
            .iter()
            .find(|server| server.definition.id == id)
            .unwrap();
        assert!(server.enabled);
        assert_eq!(server.state, LifecycleState::Connecting);
        assert_eq!(
            read_manager(&restored, |manager| Ok(manager.enabled_server_ids())).unwrap(),
            vec![id]
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(manager_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn persistence_failure_does_not_commit_in_memory_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let blocker = directory.path().join("not-a-directory");
        fs::write(&blocker, b"file").unwrap();
        let host = McpHostState {
            path: blocker.join("manager.json"),
            manager: Mutex::new(McpManager::default()),
            oauth_attempts: Mutex::new(OAuthAttemptRegistry::default()),
        };
        let result = with_manager(&host, |manager| {
            manager.add_server(definition("Rejected", "rejected"))
        });
        assert!(result.is_err());
        assert!(host.manager.lock().unwrap().snapshot().servers.is_empty());
    }

    #[test]
    fn opencode_sync_never_materializes_keychain_references_as_values() {
        let mut definition = definition("Keychain", "keychain");
        definition.transport = TransportDefinition::Stdio {
            executable: "mcp-test".into(),
            arguments: Vec::new(),
            working_directory: None,
            environment: vec![EnvironmentBinding {
                name: "GITHUB_TOKEN".into(),
                value: BindingValue::Keychain {
                    reference: KeychainReference {
                        reference_id: "github-token".into(),
                        service: "personal-agent-mcp".into(),
                        account_hint: "GitHub".into(),
                    },
                },
            }],
        };
        let error = opencode_mcp_entry(&definition).unwrap_err();
        assert!(error.contains("keychain"));
        assert!(!error.contains("github-token"));
    }

    #[test]
    fn tool_approval_is_argument_bound_and_policy_mapping_is_conservative() {
        let id = Uuid::new_v4();
        let first = tool_approval_preview(id, "github.search", &json!({"query":"one"}));
        let second = tool_approval_preview(id, "github.search", &json!({"query":"two"}));
        assert_ne!(first.0, second.0);
        let descriptor = policy_descriptor(&ToolDescriptor {
            name: "unknown".into(),
            title: None,
            description: "Unclassified remote tool".into(),
            input_schema: json!({"type":"object"}),
            output_schema: None,
            annotations: ToolAnnotations::default(),
            resolved_name: "remote.unknown".into(),
        });
        assert_eq!(descriptor.risk, Risk::Consequential);
        assert_eq!(descriptor.effect, Effect::ExternalWrite);
        assert!(!descriptor.reversible);
    }

    #[test]
    fn oauth_uses_callback_owning_route_and_single_flight_state() {
        let route = oauth_authenticate_route("composio");
        assert_eq!(route, "/mcp/composio/auth/authenticate");
        assert_ne!(route, "/mcp/composio/auth");
        assert_eq!(
            oauth_result_message(OAuthStartResult::Connected),
            "Sign-in completed and the MCP server connected."
        );

        let server_id = Uuid::new_v4();
        let other_server_id = Uuid::new_v4();
        let now = Instant::now();
        let mut attempts = OAuthAttemptRegistry::default();
        assert!(attempts.reserve(server_id, now));
        assert!(!attempts.reserve(server_id, now + Duration::from_secs(1)));
        assert!(attempts.reserve(other_server_id, now + Duration::from_secs(1)));
        attempts.clear(server_id);
        assert!(attempts.reserve(server_id, now + Duration::from_secs(2)));
    }

    #[test]
    fn abandoned_oauth_attempt_expires_without_blocking_a_fresh_start() {
        let server_id = Uuid::new_v4();
        let now = Instant::now();
        let mut attempts = OAuthAttemptRegistry::default();
        assert!(attempts.reserve(server_id, now));
        assert!(attempts.reserve(server_id, now + MCP_OAUTH_ATTEMPT_TTL));
    }

    #[tokio::test]
    async fn ready_mcp_request_executes_through_gateway_and_redacts_output() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let response = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            let body =
                r#"{"jsonrpc":"2.0","id":1,"result":{"token":"do-not-render","name":"visible"}}"#;
            let message = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(message.as_bytes()).await.unwrap();
        });
        let mut definition = definition("Gateway", "gateway");
        definition.transport = TransportDefinition::StreamableHttp {
            endpoint: format!("http://{address}"),
            stateless: true,
            headers: Vec::new(),
            oauth: None,
        };
        let advertised = ToolDescriptor {
            name: "read".into(),
            title: None,
            description: "Read data".into(),
            input_schema: json!({"type":"object"}),
            output_schema: None,
            annotations: ToolAnnotations {
                read_only: true,
                destructive: false,
                idempotent: true,
                open_world: false,
            },
            resolved_name: "gateway.read".into(),
        };
        let request = GatewayToolRequest {
            request_id: Uuid::new_v4(),
            server_id: definition.id,
            tool_name: "read".into(),
            resolved_name: "gateway.read".into(),
            arguments: json!({}),
            protocol_version: ProtocolVersion::current(),
            execution_zone: "mcp-restricted".into(),
            timeout_ms: 5_000,
            max_output_bytes: 4_096,
            requires_approval: false,
            destructive: false,
            open_world: false,
        };
        let output = execute_through_tool_gateway(&definition, &advertised, request, false)
            .await
            .unwrap();
        response.await.unwrap();
        assert_eq!(output["token"], "[REDACTED]");
        assert_eq!(output["name"], "visible");
    }

    #[tokio::test]
    async fn gateway_refuses_consequential_mcp_request_without_native_consent() {
        let definition = definition("Gateway", "gateway");
        let advertised = ToolDescriptor {
            name: "delete".into(),
            title: None,
            description: "Delete data".into(),
            input_schema: json!({"type":"object"}),
            output_schema: None,
            annotations: ToolAnnotations {
                read_only: false,
                destructive: true,
                idempotent: false,
                open_world: true,
            },
            resolved_name: "gateway.delete".into(),
        };
        let request = GatewayToolRequest {
            request_id: Uuid::new_v4(),
            server_id: definition.id,
            tool_name: "delete".into(),
            resolved_name: "gateway.delete".into(),
            arguments: json!({}),
            protocol_version: ProtocolVersion::current(),
            execution_zone: "mcp-restricted".into(),
            timeout_ms: 5_000,
            max_output_bytes: 4_096,
            requires_approval: true,
            destructive: true,
            open_world: true,
        };
        let error = execute_through_tool_gateway(&definition, &advertised, request, false)
            .await
            .unwrap_err();
        assert!(error.contains("requires approval"));
    }

    #[test]
    fn manual_definition_uninstall_needs_no_package_command() {
        assert!(
            NativePackageAdapter
                .uninstall(&definition("Remote", "remote"))
                .is_ok()
        );
    }
}
