//! Desktop IPC for browser, connectors, and private local execution.

#![allow(clippy::needless_pass_by_value)] // Tauri deserializes and owns IPC arguments.
#![allow(clippy::too_many_arguments)] // Connector IPC mirrors an explicit request envelope.

use super::DesktopState;
use crate::native_desktop::CommandNativeBridge;
use crate::native_dictation::{
    NativeDictationApplyResult, NativeDictationSession, NativeDictationStatus,
};
use crate::portal_linux::{PortalStatus, WaylandPortalManager};
use base64::Engine as _;
use personal_agent_audio::{
    CommandRouter, DictationEngine, DictationMode, DictationUpdate, EditOperation, EditReceipt,
    EditStrategy, LatencyReport, PartialTranscript, RouteContext, VoiceRoute,
};
use personal_agent_browser::{
    BrowserEngine, BrowserPolicy, NodeHandle, PageSnapshot, WebDriverBrowser, WebDriverConfig,
    WebDriverProcess,
};
use personal_agent_connectors::{
    ConnectorAction, ConnectorAuth, ConnectorConfig, ConnectorError, ConnectorGrant, ConnectorKind,
    ConnectorRequest, CredentialProvider, RestConnector,
};
use personal_agent_context::{
    BridgeDesktopBackend, CaptureScope, DesktopActionOutcome, DesktopActionRequest, DesktopBackend,
    DesktopCoordinator, ScreenPrivacyPolicy,
};
use personal_agent_core::{EgressRecord, EgressSource};
use personal_agent_local_execution::{
    CommandSpec, DockerRequest, ExecutionPolicy, ExecutionResult, LocalExecutor,
};
use personal_agent_platform::{OsSecretStore, SecretReference, SecretStore};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::Mutex as AsyncMutex;
use url::Url;
use uuid::Uuid;

pub(crate) struct CapabilityState {
    connectors_path: PathBuf,
    connectors: Mutex<Vec<ConnectorConfig>>,
    browser: AsyncMutex<BrowserRuntime>,
    dictation: Mutex<DictationEngine>,
    native_dictation: AsyncMutex<NativeDictationSession>,
    portal: Arc<WaylandPortalManager>,
    desktop: AsyncMutex<DesktopCoordinator<BridgeDesktopBackend<CommandNativeBridge>>>,
}

struct BrowserRuntime {
    driver: Option<WebDriverProcess>,
    engine: Option<WebDriverBrowser>,
}

impl CapabilityState {
    pub(crate) fn load(app_data: &Path) -> Result<Self, String> {
        let connectors_path = app_data.join("connectors/config.json");
        let connectors = if connectors_path.exists() {
            serde_json::from_slice(&fs::read(&connectors_path).map_err(|error| error.to_string())?)
                .map_err(|error| format!("connector configuration is invalid: {error}"))?
        } else {
            Vec::new()
        };
        let portal = WaylandPortalManager::live();
        Ok(Self {
            connectors_path,
            connectors: Mutex::new(connectors),
            browser: AsyncMutex::new(BrowserRuntime {
                driver: None,
                engine: None,
            }),
            dictation: Mutex::new(DictationEngine::default()),
            native_dictation: AsyncMutex::new(NativeDictationSession::discover()),
            desktop: AsyncMutex::new({
                let bridge = CommandNativeBridge::discover_with_portal(portal.clone());
                let plan = bridge.backend_plan();
                DesktopCoordinator::new(
                    BridgeDesktopBackend::with_plan(bridge, plan),
                    ScreenPrivacyPolicy::default(),
                )
            }),
            portal,
        })
    }

    fn save_connectors(&self, connectors: &[ConnectorConfig]) -> Result<(), String> {
        atomic_save_connectors(&self.connectors_path, connectors)
    }

    pub(crate) fn mutate_connectors<T>(
        &self,
        operation: impl FnOnce(&mut Vec<ConnectorConfig>) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut connectors = self
            .connectors
            .lock()
            .map_err(|_| "connector state lock is poisoned".to_owned())?;
        let mut candidate = connectors.clone();
        let result = operation(&mut candidate)?;
        self.save_connectors(&candidate)?;
        *connectors = candidate;
        Ok(result)
    }

    pub(crate) fn connector(&self, id: Uuid) -> Result<ConnectorConfig, String> {
        self.connectors
            .lock()
            .map_err(|_| "connector state lock is poisoned".to_owned())?
            .iter()
            .find(|connector| connector.id == id)
            .cloned()
            .ok_or_else(|| "connector does not exist".to_owned())
    }

    pub(crate) async fn shutdown_portal(&self) {
        self.portal.disconnect().await;
    }
}

fn atomic_save_connectors(path: &Path, connectors: &[ConnectorConfig]) -> Result<(), String> {
    let rendered = serde_json::to_vec_pretty(connectors).map_err(|error| error.to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "connector configuration has no parent directory".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = parent.join(format!(".connectors-{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(&rendered)
            .and_then(|()| file.sync_all())
            .map_err(|error| error.to_string())?;
        fs::rename(&temporary, path).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn concurrent_connector_saves_remain_atomic() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("connectors/config.json");
        let first = vec![ConnectorConfig::built_in(ConnectorKind::GitHub, "first")];
        let second = vec![ConnectorConfig::built_in(ConnectorKind::Gmail, "second")];
        let barrier = Arc::new(Barrier::new(2));

        std::thread::scope(|scope| {
            let first_path = path.clone();
            let first_barrier = Arc::clone(&barrier);
            let first_payload = first.clone();
            let first_save = scope.spawn(move || {
                first_barrier.wait();
                atomic_save_connectors(&first_path, &first_payload)
            });

            let second_path = path.clone();
            let second_barrier = Arc::clone(&barrier);
            let second_payload = second.clone();
            let second_save = scope.spawn(move || {
                second_barrier.wait();
                atomic_save_connectors(&second_path, &second_payload)
            });

            first_save
                .join()
                .expect("first save thread")
                .expect("first save");
            second_save
                .join()
                .expect("second save thread")
                .expect("second save");
        });

        let persisted: Vec<ConnectorConfig> =
            serde_json::from_slice(&fs::read(&path).expect("persisted connector configuration"))
                .expect("valid connector configuration");
        assert!(persisted == first || persisted == second);
        assert!(
            fs::read_dir(path.parent().expect("connector directory"))
                .expect("connector directory entries")
                .all(|entry| !entry
                    .expect("connector directory entry")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp"))
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&path)
                    .expect("connector configuration metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}

#[tauri::command]
pub(crate) async fn portal_status(
    state: tauri::State<'_, CapabilityState>,
) -> Result<PortalStatus, String> {
    let status = state.portal.status();
    if status.interfaces.screencast_version.is_none()
        && status.interfaces.remote_desktop_version.is_none()
        && matches!(status.phase, crate::portal_linux::PortalSessionPhase::Idle)
    {
        Ok(state.portal.probe().await)
    } else {
        Ok(status)
    }
}

#[tauri::command]
pub(crate) async fn portal_connect(
    request_control: bool,
    parent_window: Option<String>,
    state: tauri::State<'_, CapabilityState>,
) -> Result<PortalStatus, String> {
    let parent_window = parent_window.unwrap_or_default();
    if parent_window.len() > 512
        || parent_window.contains('\0')
        || (!parent_window.is_empty()
            && !parent_window.starts_with("wayland:")
            && !parent_window.starts_with("x11:"))
    {
        return Err("portal parent window handle is invalid".into());
    }
    state.portal.connect(request_control, &parent_window).await
}

#[tauri::command]
pub(crate) async fn portal_cancel(
    state: tauri::State<'_, CapabilityState>,
) -> Result<PortalStatus, String> {
    Ok(state.portal.cancel().await)
}

#[tauri::command]
pub(crate) async fn portal_disconnect(
    state: tauri::State<'_, CapabilityState>,
) -> Result<PortalStatus, String> {
    Ok(state.portal.disconnect().await)
}

#[derive(serde::Serialize)]
pub(crate) struct DesktopContextResponse {
    snapshot: personal_agent_context::ActiveViewSnapshot,
    frame_png_base64: Option<String>,
}

#[derive(serde::Serialize)]
pub(crate) struct DesktopActionResponse {
    receipt: personal_agent_context::DesktopActionReceipt,
    snapshot: personal_agent_context::ActiveViewSnapshot,
}

#[tauri::command]
pub(crate) async fn desktop_status(
    state: tauri::State<'_, CapabilityState>,
) -> Result<personal_agent_context::DesktopBackendStatus, String> {
    Ok(state.desktop.lock().await.backend().status())
}

#[tauri::command]
pub(crate) async fn desktop_set_capture(
    enabled: bool,
    allow_full_display: bool,
    state: tauri::State<'_, CapabilityState>,
) -> Result<(), String> {
    let mut desktop = state.desktop.lock().await;
    let mut policy = desktop.privacy_policy().clone();
    policy.capture_enabled = enabled;
    policy.allow_full_display_capture = allow_full_display;
    desktop.set_privacy_policy(policy);
    Ok(())
}

#[tauri::command]
pub(crate) async fn desktop_snapshot(
    capture_pixels: bool,
    state: tauri::State<'_, CapabilityState>,
) -> Result<DesktopContextResponse, String> {
    let desktop = state.desktop.lock().await;
    let mut snapshot = desktop
        .snapshot(&CaptureScope::ActiveWindow)
        .await
        .map_err(|error| error.to_string())?;
    let frame_png_base64 = if capture_pixels {
        let frame = desktop
            .capture(&CaptureScope::ActiveWindow, &snapshot)
            .await
            .map_err(|error| error.to_string())?;
        snapshot.frame = Some(frame.descriptor.clone());
        Some(encode_png(&frame)?)
    } else {
        None
    };
    Ok(DesktopContextResponse {
        snapshot,
        frame_png_base64,
    })
}

#[tauri::command]
pub(crate) async fn desktop_execute(
    request: DesktopActionRequest,
    state: tauri::State<'_, CapabilityState>,
) -> Result<DesktopActionResponse, String> {
    let desktop = state.desktop.lock().await;
    let before = desktop
        .snapshot(&CaptureScope::ActiveWindow)
        .await
        .map_err(|error| error.to_string())?;
    let DesktopActionOutcome {
        receipt, snapshot, ..
    } = desktop
        .execute(&request, before)
        .await
        .map_err(|error| error.to_string())?;
    Ok(DesktopActionResponse { receipt, snapshot })
}

fn encode_png(frame: &personal_agent_context::CapturedFrame) -> Result<String, String> {
    let image = image::RgbaImage::from_raw(
        frame.descriptor.width,
        frame.descriptor.height,
        frame.bytes.clone(),
    )
    .ok_or_else(|| "captured frame dimensions do not match its bytes".to_owned())?;
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()))
}

#[derive(serde::Serialize)]
pub(crate) struct DictationApplyResult {
    receipts: Vec<EditReceipt>,
    rejected: Vec<RejectedEdit>,
}

#[derive(serde::Serialize)]
struct RejectedEdit {
    operation: EditOperation,
    reason: String,
}

#[tauri::command]
pub(crate) fn dictation_ingest(
    event: PartialTranscript,
    state: tauri::State<'_, CapabilityState>,
) -> Result<DictationUpdate, String> {
    state
        .dictation
        .lock()
        .map_err(|_| "dictation state lock is poisoned".to_owned())?
        .ingest(event)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn dictation_latency_report(
    state: tauri::State<'_, CapabilityState>,
) -> Result<LatencyReport, String> {
    state
        .dictation
        .lock()
        .map(|engine| engine.latency_report())
        .map_err(|_| "dictation state lock is poisoned".to_owned())
}

#[tauri::command]
pub(crate) fn dictation_reset(
    mode: DictationMode,
    state: tauri::State<'_, CapabilityState>,
) -> Result<(), String> {
    *state
        .dictation
        .lock()
        .map_err(|_| "dictation state lock is poisoned".to_owned())? = DictationEngine::new(mode);
    Ok(())
}

#[tauri::command]
pub(crate) fn voice_route(transcript: String, context: RouteContext) -> VoiceRoute {
    CommandRouter.route(&transcript, context)
}

#[tauri::command]
pub(crate) fn dictation_apply(operations: Vec<EditOperation>) -> DictationApplyResult {
    let applied_at_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default();
    let mut receipts = Vec::new();
    let mut rejected = Vec::new();
    for operation in operations {
        match operation {
            EditOperation::CommitTransaction { transaction_id } => receipts.push(EditReceipt {
                transaction_id: Some(transaction_id),
                applied_at_ms,
                strategy: EditStrategy::SessionState,
                verified: true,
            }),
            EditOperation::SetMode { .. } => receipts.push(EditReceipt {
                transaction_id: None,
                applied_at_ms,
                strategy: EditStrategy::SessionState,
                verified: true,
            }),
            operation => rejected.push(RejectedEdit {
                operation,
                reason: "no focused editable target was supplied; keep the edit in the in-app dictation buffer"
                    .into(),
            }),
        }
    }
    DictationApplyResult { receipts, rejected }
}

#[tauri::command]
pub(crate) async fn native_dictation_status(
    state: tauri::State<'_, CapabilityState>,
) -> Result<NativeDictationStatus, String> {
    Ok(state.native_dictation.lock().await.status())
}

#[tauri::command]
pub(crate) async fn native_dictation_arm(
    delay_ms: u64,
    state: tauri::State<'_, CapabilityState>,
) -> Result<NativeDictationStatus, String> {
    state.native_dictation.lock().await.arm(delay_ms).await
}

#[tauri::command]
pub(crate) async fn native_dictation_disarm(
    state: tauri::State<'_, CapabilityState>,
) -> Result<NativeDictationStatus, String> {
    Ok(state.native_dictation.lock().await.disarm())
}

#[tauri::command]
pub(crate) async fn native_dictation_stage(
    update: DictationUpdate,
    state: tauri::State<'_, CapabilityState>,
) -> Result<NativeDictationStatus, String> {
    state.native_dictation.lock().await.stage(update).await
}

#[tauri::command]
pub(crate) async fn native_dictation_discard(
    state: tauri::State<'_, CapabilityState>,
) -> Result<NativeDictationStatus, String> {
    Ok(state.native_dictation.lock().await.discard())
}

#[tauri::command]
pub(crate) async fn native_dictation_confirm(
    confirmed: bool,
    delay_ms: u64,
    state: tauri::State<'_, CapabilityState>,
) -> Result<NativeDictationApplyResult, String> {
    state
        .native_dictation
        .lock()
        .await
        .confirm(confirmed, delay_ms)
        .await
}

#[tauri::command]
pub(crate) async fn native_dictation_undo(
    confirmed: bool,
    delay_ms: u64,
    state: tauri::State<'_, CapabilityState>,
) -> Result<NativeDictationApplyResult, String> {
    state
        .native_dictation
        .lock()
        .await
        .undo(confirmed, delay_ms)
        .await
}

fn connector_kind(value: &str) -> Result<ConnectorKind, String> {
    match value {
        "github" => Ok(ConnectorKind::GitHub),
        "gmail" => Ok(ConnectorKind::Gmail),
        "google_calendar" => Ok(ConnectorKind::GoogleCalendar),
        "slack" => Ok(ConnectorKind::Slack),
        "microsoft_graph" => Ok(ConnectorKind::MicrosoftGraph),
        "custom_rest" => Ok(ConnectorKind::CustomRest),
        _ => Err("unknown connector kind".into()),
    }
}

#[tauri::command]
pub(crate) fn connector_list(
    state: tauri::State<'_, CapabilityState>,
) -> Result<Vec<ConnectorConfig>, String> {
    state
        .connectors
        .lock()
        .map(|connectors| connectors.clone())
        .map_err(|_| "connector state lock is poisoned".into())
}

#[tauri::command]
pub(crate) fn connector_create(
    kind: String,
    display_name: String,
    base_url: Option<String>,
    credential: Option<String>,
    state: tauri::State<'_, CapabilityState>,
) -> Result<ConnectorConfig, String> {
    let mut config = ConnectorConfig::built_in(connector_kind(&kind)?, display_name);
    if let Some(base_url) = base_url.filter(|value| !value.trim().is_empty()) {
        config.base_url = Url::parse(&base_url).map_err(|error| error.to_string())?;
    }
    let secret_reference = if let Some(credential) = credential.filter(|value| !value.is_empty()) {
        let reference = SecretReference {
            service: "personal-agent-connector".into(),
            account: config.id.to_string(),
        };
        OsSecretStore
            .put(&reference, &SecretString::from(credential))
            .map_err(|error| error.to_string())?;
        config.auth = ConnectorAuth::BearerToken {
            keychain_alias: reference.alias(),
        };
        Some(reference)
    } else {
        None
    };
    config.validate().map_err(|error| error.to_string())?;
    let result = state.mutate_connectors(|connectors| {
        connectors.push(config.clone());
        Ok(config)
    });
    if result.is_err()
        && let Some(reference) = secret_reference
    {
        let _ = OsSecretStore.delete(&reference);
    }
    result
}

#[tauri::command]
pub(crate) async fn connector_action(
    id: String,
    operation: String,
    confirmed: bool,
    state: tauri::State<'_, CapabilityState>,
) -> Result<Value, String> {
    let id = Uuid::parse_str(&id).map_err(|_| "connector ID is invalid".to_owned())?;
    if operation == "test" {
        let config = state
            .connectors
            .lock()
            .map_err(|_| "connector state lock is poisoned".to_owned())?
            .iter()
            .find(|connector| connector.id == id)
            .cloned()
            .ok_or_else(|| "connector does not exist".to_owned())?;
        config.validate().map_err(|error| error.to_string())?;
        let result = reqwest::Client::new()
            .get(config.base_url.clone())
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await;
        return match result {
            Ok(response) => Ok(json!({"state": "reachable", "status": response.status().as_u16()})),
            Err(error) => Err(format!("connector endpoint is unreachable: {error}")),
        };
    }
    let removed_secrets = state.mutate_connectors(|connectors| {
        let index = connectors
            .iter()
            .position(|connector| connector.id == id)
            .ok_or_else(|| "connector does not exist".to_owned())?;
        match operation.as_str() {
            "enable" => connectors[index].enabled = true,
            "disable" => connectors[index].enabled = false,
            "delete" if confirmed => {
                let references = match &connectors[index].auth {
                    ConnectorAuth::OAuth2 {
                        keychain_alias,
                        refresh_keychain_alias,
                        ..
                    } => [Some(keychain_alias), refresh_keychain_alias.as_ref()]
                        .into_iter()
                        .flatten()
                        .filter_map(|alias| SecretReference::parse(alias).ok())
                        .collect(),
                    ConnectorAuth::BearerToken { keychain_alias } => {
                        SecretReference::parse(keychain_alias)
                            .ok()
                            .into_iter()
                            .collect()
                    }
                    ConnectorAuth::None => Vec::new(),
                };
                connectors.remove(index);
                return Ok(references);
            }
            "delete" => return Err("connector removal requires confirmation".into()),
            _ => return Err("unknown connector action".into()),
        }
        Ok(Vec::new())
    })?;
    for reference in removed_secrets {
        let _ = OsSecretStore.delete(&reference);
    }
    Ok(json!({"ok": true}))
}

#[tauri::command]
pub(crate) fn connector_set_grants(
    id: String,
    grants: Vec<ConnectorGrant>,
    confirmed: bool,
    state: tauri::State<'_, CapabilityState>,
) -> Result<ConnectorConfig, String> {
    let id = Uuid::parse_str(&id).map_err(|_| "connector ID is invalid".to_owned())?;
    if grants.len() > 128 {
        return Err("connector grant list exceeds the 128-grant limit".into());
    }
    for grant in &grants {
        if grant.resource.trim().is_empty()
            || grant.resource.len() > 128
            || grant
                .resource
                .chars()
                .any(|value| matches!(value, '\0' | '\n' | '\r'))
        {
            return Err("connector resource grant is invalid".into());
        }
        if grant.action != ConnectorAction::Read && !confirmed {
            return Err("connector write grants require explicit confirmation".into());
        }
    }
    state.mutate_connectors(|connectors| {
        let connector = connectors
            .iter_mut()
            .find(|connector| connector.id == id)
            .ok_or_else(|| "connector does not exist".to_owned())?;
        connector.grants = grants.into_iter().collect();
        connector.validate().map_err(|error| error.to_string())?;
        Ok(connector.clone())
    })
}

struct KeychainCredentials;

#[async_trait::async_trait]
impl CredentialProvider for KeychainCredentials {
    async fn bearer_token(&self, alias: &str) -> Result<String, ConnectorError> {
        let reference =
            SecretReference::parse(alias).map_err(|_| ConnectorError::CredentialsUnavailable)?;
        OsSecretStore
            .get(&reference)
            .map(|secret| secret.expose_secret().to_owned())
            .map_err(|_| ConnectorError::CredentialsUnavailable)
    }
}

#[tauri::command]
pub(crate) async fn connector_execute(
    id: String,
    resource: String,
    action: ConnectorAction,
    method: String,
    path: String,
    query: BTreeMap<String, String>,
    body: Option<Value>,
    idempotency_key: Option<String>,
    state: tauri::State<'_, CapabilityState>,
    desktop: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    use personal_agent_connectors::Connector as _;
    let id = Uuid::parse_str(&id).map_err(|_| "connector ID is invalid".to_owned())?;
    crate::connector_oauth::refresh_if_needed(id, &state).await?;
    let config = state
        .connectors
        .lock()
        .map_err(|_| "connector state lock is poisoned".to_owned())?
        .iter()
        .find(|connector| connector.id == id)
        .cloned()
        .ok_or_else(|| "connector does not exist".to_owned())?;
    let destination = format!("{:?}", config.kind).to_ascii_lowercase();
    let request_bytes = serde_json::to_vec(&json!({"query": &query, "body": &body}))
        .map_or(0, |value| u64::try_from(value.len()).unwrap_or(u64::MAX));
    let operation = method.trim().to_ascii_uppercase();
    let connector = RestConnector::new(config, KeychainCredentials);
    let response = connector
        .execute(ConnectorRequest {
            operation_id: Uuid::now_v7(),
            grant: ConnectorGrant { resource, action },
            method,
            path,
            query,
            body,
            idempotency_key,
        })
        .await
        .map_err(|error| error.to_string())?;
    record_egress(
        &desktop,
        EgressRecord {
            id: Uuid::now_v7(),
            at: chrono::Utc::now(),
            source: EgressSource::Connector,
            destination,
            operation,
            data_class: "connector request".into(),
            size_bytes: Some(request_bytes),
            purpose: "user-authorized connector action".into(),
            session_id: None,
            scope_key: None,
        },
    )?;
    serde_json::to_value(response).map_err(|error| error.to_string())
}

fn record_egress(desktop: &DesktopState, record: EgressRecord) -> Result<(), String> {
    desktop
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?
        .record_egress(record)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn browser_policy(state: &DesktopState) -> Result<(bool, BrowserPolicy), String> {
    let config = state
        .config
        .read()
        .map_err(|_| "configuration lock is poisoned".to_owned())?;
    Ok((
        config.browser.enabled,
        BrowserPolicy {
            allowed_domains: config.browser.allowed_domains.iter().cloned().collect(),
            blocked_domains: config.browser.blocked_domains.iter().cloned().collect(),
            allow_third_party_subresources: config.browser.allow_third_party_subresources,
        },
    ))
}

#[tauri::command]
pub(crate) async fn browser_open(
    browser_name: String,
    profile_id: String,
    desktop: tauri::State<'_, DesktopState>,
    capabilities: tauri::State<'_, CapabilityState>,
) -> Result<PageSnapshot, String> {
    let (enabled, policy) = browser_policy(&desktop)?;
    if !enabled {
        return Err("browser automation is disabled in Settings".into());
    }
    let endpoint = Url::parse("http://127.0.0.1:4444/").expect("constant driver URL");
    let driver = WebDriverProcess::start(&browser_name, endpoint.clone())
        .await
        .map_err(|error| error.to_string())?;
    let mut engine = WebDriverBrowser::new(WebDriverConfig {
        endpoint,
        browser_name,
        capabilities: BTreeMap::new(),
        policy,
    });
    engine
        .open_isolated_profile(&profile_id)
        .await
        .map_err(|error| error.to_string())?;
    let snapshot = engine.snapshot().await.map_err(|error| error.to_string())?;
    let mut runtime = capabilities.browser.lock().await;
    runtime.driver = Some(driver);
    runtime.engine = Some(engine);
    Ok(snapshot)
}

#[tauri::command]
pub(crate) async fn browser_navigate(
    url: String,
    state: tauri::State<'_, CapabilityState>,
    desktop: tauri::State<'_, DesktopState>,
) -> Result<PageSnapshot, String> {
    let url = Url::parse(&url).map_err(|error| error.to_string())?;
    let destination = url.origin().ascii_serialization();
    let request_bytes = u64::try_from(url.as_str().len()).unwrap_or(u64::MAX);
    let mut runtime = state.browser.lock().await;
    let snapshot = runtime
        .engine
        .as_mut()
        .ok_or_else(|| "open a browser profile first".to_owned())?
        .navigate(&url)
        .await
        .map_err(|error| error.to_string())?;
    drop(runtime);
    record_egress(
        &desktop,
        EgressRecord {
            id: Uuid::now_v7(),
            at: chrono::Utc::now(),
            source: EgressSource::Web,
            destination,
            operation: "navigate".into(),
            data_class: "URL metadata".into(),
            size_bytes: Some(request_bytes),
            purpose: "user-requested browser navigation".into(),
            session_id: None,
            scope_key: None,
        },
    )?;
    Ok(snapshot)
}

#[tauri::command]
pub(crate) async fn browser_action(
    operation: String,
    handle: Option<NodeHandle>,
    text: Option<String>,
    state: tauri::State<'_, CapabilityState>,
    desktop: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    let mut runtime = state.browser.lock().await;
    let engine = runtime
        .engine
        .as_mut()
        .ok_or_else(|| "open a browser profile first".to_owned())?;
    let destination = engine
        .snapshot()
        .await
        .map_err(|error| error.to_string())?
        .url
        .origin()
        .ascii_serialization();
    let size_bytes = match operation.as_str() {
        "type" => {
            Some(u64::try_from(text.as_deref().unwrap_or_default().len()).unwrap_or(u64::MAX))
        }
        "snapshot" | "click" => Some(0),
        _ => None,
    };
    let value = match operation.as_str() {
        "snapshot" => {
            serde_json::to_value(engine.snapshot().await.map_err(|error| error.to_string())?)
        }
        "click" => serde_json::to_value(
            engine
                .click(&handle.ok_or_else(|| "node handle is required".to_owned())?)
                .await
                .map_err(|error| error.to_string())?,
        ),
        "type" => serde_json::to_value(
            engine
                .type_text(
                    &handle.ok_or_else(|| "node handle is required".to_owned())?,
                    text.as_deref().unwrap_or_default(),
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "takeover" => {
            engine.takeover().await.map_err(|error| error.to_string())?;
            Ok(json!({"taken_over": true}))
        }
        _ => return Err("unknown browser action".into()),
    };
    let value = value.map_err(|error| error.to_string())?;
    drop(runtime);
    if let Some(size_bytes) = size_bytes {
        record_egress(
            &desktop,
            EgressRecord {
                id: Uuid::now_v7(),
                at: chrono::Utc::now(),
                source: EgressSource::Web,
                destination,
                operation: operation.clone(),
                data_class: if operation == "type" {
                    "typed text"
                } else {
                    "browser request metadata"
                }
                .into(),
                size_bytes: Some(size_bytes),
                purpose: "user-requested browser action".into(),
                session_id: None,
                scope_key: None,
            },
        )?;
    }
    Ok(value)
}

#[tauri::command]
pub(crate) async fn browser_close(state: tauri::State<'_, CapabilityState>) -> Result<(), String> {
    let mut runtime = state.browser.lock().await;
    if let Some(engine) = &mut runtime.engine {
        engine.close().await.map_err(|error| error.to_string())?;
    }
    if let Some(driver) = &mut runtime.driver {
        driver.stop().await.map_err(|error| error.to_string())?;
    }
    runtime.engine = None;
    runtime.driver = None;
    Ok(())
}

fn execution_policy(state: &DesktopState) -> Result<ExecutionPolicy, String> {
    let config = state
        .config
        .read()
        .map_err(|_| "configuration lock is poisoned".to_owned())?;
    Ok(ExecutionPolicy {
        workspace_roots: vec![PathBuf::from(&config.runtime.working_directory)],
        allowed_programs: BTreeSet::new(),
        allowed_environment: ["PATH", "LANG", "LC_ALL", "TERM", "COLORTERM"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        allow_network: !config.voice.offline_only,
        allow_interactive_shell: true,
        allow_docker: true,
        require_approval_for_destructive: true,
    })
}

#[tauri::command]
pub(crate) async fn local_execute(
    spec: CommandSpec,
    confirmed: bool,
    state: tauri::State<'_, DesktopState>,
) -> Result<ExecutionResult, String> {
    LocalExecutor {
        policy: execution_policy(&state)?,
    }
    .run(spec, confirmed)
    .await
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn docker_execute(
    request: DockerRequest,
    confirmed: bool,
    state: tauri::State<'_, DesktopState>,
) -> Result<ExecutionResult, String> {
    LocalExecutor {
        policy: execution_policy(&state)?,
    }
    .run_docker(request, confirmed)
    .await
    .map_err(|error| error.to_string())
}
