//! Provider-neutral agent runtime boundary and pinned `OpenCode` sidecar adapter.

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures_util::StreamExt;
use personal_agent_contracts::proto::EventEnvelope;
use personal_agent_policy::{DataZone, Effect, Idempotency, Risk, ToolDescriptor};
use personal_agent_tools::{ToolCall, ToolError, ToolGateway, ToolImplementation};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::TcpListener,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, RwLock},
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener as TokioTcpListener, TcpStream},
    process::{Child, Command},
    sync::mpsc,
    task::JoinHandle,
};
use url::Url;
use uuid::Uuid;

#[allow(clippy::all, dead_code)]
mod opencode_api {
    progenitor::generate_api!(
        spec = "../../contracts/openapi/opencode-1.18.23.client.json",
        interface = Builder,
        tags = Merged,
    );
}

/// Stable `OpenCode` sidecar version verified on 2026-08-26.
pub const OPENCODE_VERSION: &str = "1.18.23";

/// SHA-256 of the authenticated `/doc` response from the pinned sidecar.
pub const OPENCODE_OPENAPI_SHA256: &str =
    "dfb7d42a555389f0c662fa2b4a8af1d61633c96710cf54bce3ff2404e2e7d896";

/// Narrow ambient allowlist for OS/runtime mechanics. Provider credentials,
/// proxy settings, package-manager configuration, and `OpenCode` overrides are
/// intentionally absent and must be onboarded through native secret storage.
const SAFE_AMBIENT_ENVIRONMENT: &[&str] = &[
    "PATH",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NIX_SSL_CERT_FILE",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
];

fn create_private_directory(path: &Path) -> Result<(), RuntimeError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(RuntimeError::Rejected(format!(
                "OpenCode profile directory must not be a symlink: {}",
                path.display()
            )));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(RuntimeError::Rejected(format!(
                "OpenCode profile path is not a directory: {}",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Runtime health reported without leaking credentials.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeHealth {
    pub healthy: bool,
    pub version: String,
    pub detail: String,
}

/// Provider and model visible to the user.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelCapability {
    pub provider_id: String,
    pub model_id: String,
    pub context_tokens: Option<u64>,
    pub local: bool,
    pub reasoning: bool,
    pub tool_calls: bool,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
}

/// Session isolation and selection policy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionOptions {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub agent: Option<String>,
    pub working_directory: PathBuf,
    pub environment: BTreeMap<String, String>,
}

/// Permission or clarification answer returned to the runtime.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuntimeAnswer {
    pub request_id: String,
    pub answer: Value,
}

/// Runtime operation failure with a stable code for recovery policy.
#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("runtime is not running")]
    NotRunning,
    #[error("runtime process failed: {0}")]
    Process(#[from] std::io::Error),
    #[error("runtime HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("runtime version mismatch: expected {expected}, found {found}")]
    IncompatibleVersion { expected: String, found: String },
    #[error("runtime API contract mismatch: expected SHA-256 {expected}, found {found}")]
    IncompatibleApiContract { expected: String, found: String },
    #[error("runtime did not become healthy before timeout: {0}")]
    HealthTimeout(String),
    #[error("runtime rejected the request: {0}")]
    Rejected(String),
    #[error("runtime event stream ended unexpectedly")]
    StreamClosed,
    #[error(transparent)]
    Event(#[from] personal_agent_contracts::EventError),
}

/// Replaceable boundary consumed by the agent supervisor.
#[async_trait]
pub trait AgentRuntime: Send {
    async fn start(&mut self) -> Result<RuntimeHealth, RuntimeError>;
    async fn health(&mut self) -> Result<RuntimeHealth, RuntimeError>;
    async fn stop(&mut self) -> Result<(), RuntimeError>;
    async fn discover_models(
        &mut self,
        working_directory: Option<&Path>,
    ) -> Result<Vec<ModelCapability>, RuntimeError>;
    async fn begin_session(&mut self, options: SessionOptions) -> Result<String, RuntimeError>;
    async fn resume_session(
        &mut self,
        session_id: &str,
        working_directory: &Path,
    ) -> Result<(), RuntimeError>;
    async fn compact_session(&mut self, session_id: &str) -> Result<(), RuntimeError>;
    async fn fork_session(&mut self, session_id: &str) -> Result<String, RuntimeError>;
    async fn abort_session(&mut self, session_id: &str) -> Result<(), RuntimeError>;
    async fn submit(
        &mut self,
        session_id: &str,
        prompt: &str,
        plan: Option<Value>,
    ) -> Result<mpsc::Receiver<EventEnvelope>, RuntimeError>;
    async fn answer(&mut self, session_id: &str, answer: RuntimeAnswer)
    -> Result<(), RuntimeError>;
}

/// Configuration for the initial stable sidecar topology.
pub struct OpenCodeConfig {
    pub executable: PathBuf,
    pub safety_plugin: PathBuf,
    /// Application-owned profile root. `OpenCode` never reads the user's ambient
    /// `OpenCode`, package-manager, or provider configuration.
    pub profile_root: PathBuf,
    pub version: String,
    pub username: String,
    pub password: SecretString,
    pub startup_timeout: Duration,
    /// Provider definitions admitted by native onboarding. Project-local
    /// `opencode.json` files and `.opencode` directories are never loaded.
    pub providers: BTreeMap<String, Value>,
    pub default_model: Option<String>,
    pub small_model: Option<String>,
    /// Managed `OpenCode` features such as agents, commands, MCP, instructions,
    /// providers, keybinds, and formatters. Runtime security keys overwrite it.
    pub managed_config: Value,
}

impl OpenCodeConfig {
    /// Create an ephemeral authentication secret for one application run.
    #[must_use]
    pub fn pinned(executable: PathBuf, safety_plugin: PathBuf, profile_root: PathBuf) -> Self {
        Self {
            executable,
            safety_plugin,
            profile_root,
            version: OPENCODE_VERSION.into(),
            username: "personal-agent".into(),
            password: SecretString::from(Uuid::new_v4().to_string()),
            startup_timeout: Duration::from_secs(30),
            providers: BTreeMap::new(),
            default_model: None,
            small_model: None,
            managed_config: serde_json::json!({}),
        }
    }
}

struct RuntimeStatusTool {
    descriptor: ToolDescriptor,
}

impl RuntimeStatusTool {
    fn new() -> Self {
        Self {
            descriptor: ToolDescriptor {
                id: "runtime.status".into(),
                version: "1.0.0".into(),
                description: "Report the native Personal Agent tool gateway status".into(),
                scopes: BTreeSet::from(["runtime.observe".into()]),
                risk: Risk::Read,
                effect: Effect::Observe,
                idempotency: Idempotency::Safe,
                reversible: false,
                zones_read: BTreeSet::from([DataZone::TrustedLocalState]),
                zones_written: BTreeSet::from([DataZone::AgentGenerated]),
                user_presence: false,
            },
        }
    }
}

#[async_trait]
impl ToolImplementation for RuntimeStatusTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn validate_input(&self, input: &Value) -> Result<(), ToolError> {
        if input.as_object().is_some_and(serde_json::Map::is_empty) {
            Ok(())
        } else {
            Err(ToolError::InvalidInput("empty object required".into()))
        }
    }

    async fn checkpoint(&self, _: &ToolCall) -> Result<Option<String>, ToolError> {
        Ok(None)
    }

    async fn execute(&self, _: &ToolCall) -> Result<Value, ToolError> {
        Ok(serde_json::json!({
            "ready": true,
            "boundary": "native-tool-gateway",
        }))
    }

    async fn verify(&self, _: &ToolCall, output: &Value) -> Result<(), ToolError> {
        if output.get("ready").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(ToolError::Postcondition(
                "native gateway did not report ready".into(),
            ))
        }
    }
}

#[derive(Deserialize)]
struct BridgeRequest {
    session_id: String,
    directory: String,
}

struct NativeToolBridge {
    endpoint: Url,
    token: SecretString,
    gateway: Arc<tokio::sync::Mutex<ToolGateway>>,
    task: JoinHandle<()>,
}

impl NativeToolBridge {
    async fn start(sessions: Arc<RwLock<BTreeMap<String, PathBuf>>>) -> Result<Self, RuntimeError> {
        let listener = TokioTcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let endpoint = Url::parse(&format!(
            "http://127.0.0.1:{}/v1/tools/runtime-status",
            address.port()
        ))
        .expect("native bridge loopback URL");
        let token = SecretString::from(Uuid::new_v4().to_string());
        let token_for_task = token.expose_secret().to_owned();
        let mut gateway = ToolGateway::new(16 * 1024);
        gateway.register(Arc::new(RuntimeStatusTool::new()));
        let gateway = Arc::new(tokio::sync::Mutex::new(gateway));
        let gateway_for_task = Arc::clone(&gateway);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, peer)) = listener.accept().await else {
                    break;
                };
                if !peer.ip().is_loopback() {
                    continue;
                }
                let gateway = Arc::clone(&gateway_for_task);
                let sessions = Arc::clone(&sessions);
                let token = token_for_task.clone();
                tokio::spawn(async move {
                    let _ = handle_bridge_connection(stream, &token, gateway, sessions).await;
                });
            }
        });
        Ok(Self {
            endpoint,
            token,
            gateway,
            task,
        })
    }

    async fn audit_count(&self) -> usize {
        self.gateway.lock().await.audits().len()
    }
}

impl Drop for NativeToolBridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle_bridge_connection(
    mut stream: TcpStream,
    token: &str,
    gateway: Arc<tokio::sync::Mutex<ToolGateway>>,
    sessions: Arc<RwLock<BTreeMap<String, PathBuf>>>,
) -> Result<(), std::io::Error> {
    let response = match read_bridge_request(&mut stream).await {
        Ok((authorization, request)) if authorization == format!("Bearer {token}") => {
            execute_bridge_request(request, gateway, sessions).await
        }
        Ok(_) => (401, serde_json::json!({"error":"unauthorized"})),
        Err(status) => (status, serde_json::json!({"error":"invalid request"})),
    };
    write_bridge_response(&mut stream, response.0, &response.1).await
}

async fn read_bridge_request(stream: &mut TcpStream) -> Result<(String, BridgeRequest), u16> {
    const MAX_REQUEST_BYTES: usize = 32 * 1024;
    let mut bytes = Vec::with_capacity(2048);
    let header_end = loop {
        if bytes.len() >= MAX_REQUEST_BYTES {
            return Err(413);
        }
        let mut chunk = [0_u8; 2048];
        let count = stream.read(&mut chunk).await.map_err(|_| 400_u16)?;
        if count == 0 {
            return Err(400);
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..header_end]).map_err(|_| 400_u16)?;
    let mut lines = headers.split("\r\n");
    if lines.next() != Some("POST /v1/tools/runtime-status HTTP/1.1") {
        return Err(404);
    }
    let mut authorization = None;
    let mut content_length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "authorization" => authorization = Some(value.trim().to_owned()),
            "content-length" => {
                content_length = Some(value.trim().parse::<usize>().map_err(|_| 400_u16)?);
            }
            _ => {}
        }
    }
    let content_length = content_length.ok_or(411_u16)?;
    if header_end.saturating_add(content_length) > MAX_REQUEST_BYTES {
        return Err(413);
    }
    while bytes.len() < header_end + content_length {
        let mut chunk = [0_u8; 2048];
        let count = stream.read(&mut chunk).await.map_err(|_| 400_u16)?;
        if count == 0 {
            return Err(400);
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(413);
        }
    }
    let request = serde_json::from_slice(&bytes[header_end..header_end + content_length])
        .map_err(|_| 400_u16)?;
    Ok((authorization.ok_or(401_u16)?, request))
}

async fn execute_bridge_request(
    request: BridgeRequest,
    gateway: Arc<tokio::sync::Mutex<ToolGateway>>,
    sessions: Arc<RwLock<BTreeMap<String, PathBuf>>>,
) -> (u16, Value) {
    let Ok(directory) = std::fs::canonicalize(&request.directory) else {
        return (403, serde_json::json!({"error":"session scope denied"}));
    };
    let authorized = sessions
        .read()
        .ok()
        .and_then(|registered| registered.get(&request.session_id).cloned())
        .is_some_and(|registered| registered == directory);
    if !authorized {
        return (403, serde_json::json!({"error":"session scope denied"}));
    }
    let call = ToolCall {
        call_id: Uuid::now_v7(),
        goal_id: Uuid::nil(),
        task_id: None,
        tool_id: "runtime.status".into(),
        target: directory.display().to_string(),
        input: serde_json::json!({}),
        input_zones: BTreeSet::from([DataZone::AgentGenerated]),
        granted_scopes: BTreeSet::from(["runtime.observe".into()]),
        estimated_cost_usd: 0.0,
        background: false,
        user_present: true,
        checkpoint_available: false,
    };
    match gateway.lock().await.call(call, &[]).await {
        Ok(output) => (200, output.value),
        Err(_) => (403, serde_json::json!({"error":"tool call denied"})),
    }
}

async fn write_bridge_response(
    stream: &mut TcpStream,
    status: u16,
    body: &Value,
) -> Result<(), std::io::Error> {
    let encoded = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        411 => "Length Required",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        encoded.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(&encoded).await?;
    stream.shutdown().await
}

/// Owned `OpenCode` process. The UI never receives this endpoint or credential.
pub struct OpenCodeSidecar {
    config: OpenCodeConfig,
    child: Option<Child>,
    endpoint: Option<Url>,
    client: Option<opencode_api::Client>,
    session_directories: Arc<RwLock<BTreeMap<String, PathBuf>>>,
    tool_bridge: Option<NativeToolBridge>,
}

impl OpenCodeSidecar {
    #[must_use]
    pub fn new(config: OpenCodeConfig) -> Self {
        Self {
            config,
            child: None,
            endpoint: None,
            client: None,
            session_directories: Arc::new(RwLock::new(BTreeMap::new())),
            tool_bridge: None,
        }
    }

    fn reserve_loopback_port() -> Result<u16, std::io::Error> {
        Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
    }

    /// Endpoint retained inside the native runtime adapter for generated-client calls.
    #[must_use]
    pub fn endpoint(&self) -> Option<&Url> {
        self.endpoint.as_ref()
    }

    fn build_generated_client(&self) -> Result<opencode_api::Client, RuntimeError> {
        let endpoint = self.endpoint.as_ref().ok_or(RuntimeError::NotRunning)?;
        let credential = BASE64_STANDARD.encode(format!(
            "{}:{}",
            self.config.username,
            self.config.password.expose_secret()
        ));
        let authorization = reqwest::header::HeaderValue::from_str(&format!("Basic {credential}"))
            .map_err(|_| {
                RuntimeError::Rejected("runtime authentication header is invalid".into())
            })?;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::AUTHORIZATION, authorization);
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(opencode_api::Client::new_with_client(
            endpoint.as_str().trim_end_matches('/'),
            client,
        ))
    }

    fn generated_client(&self) -> Result<&opencode_api::Client, RuntimeError> {
        self.client.as_ref().ok_or(RuntimeError::NotRunning)
    }

    fn registered_session_directory(&self, session_id: &str) -> Result<String, RuntimeError> {
        self.session_directories
            .read()
            .map_err(|_| RuntimeError::Rejected("runtime session registry is unavailable".into()))?
            .get(session_id)
            .map(|path| path.display().to_string())
            .ok_or_else(|| {
                RuntimeError::Rejected(
                    "session working directory is not registered; resume it before use".into(),
                )
            })
    }

    fn register_session_directory(
        &self,
        session_id: String,
        directory: PathBuf,
    ) -> Result<(), RuntimeError> {
        self.session_directories
            .write()
            .map_err(|_| RuntimeError::Rejected("runtime session registry is unavailable".into()))?
            .insert(session_id, directory);
        Ok(())
    }

    /// Count native tool calls audited during this runtime process.
    pub async fn tool_audit_count(&self) -> usize {
        match &self.tool_bridge {
            Some(bridge) => bridge.audit_count().await,
            None => 0,
        }
    }

    fn raw_client(&self) -> Result<reqwest::Client, RuntimeError> {
        let credential = BASE64_STANDARD.encode(format!(
            "{}:{}",
            self.config.username,
            self.config.password.expose_secret()
        ));
        let authorization = reqwest::header::HeaderValue::from_str(&format!("Basic {credential}"))
            .map_err(|_| RuntimeError::Rejected("runtime authorization is invalid".into()))?;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::AUTHORIZATION, authorization);
        reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(Into::into)
    }

    /// Call one reviewed `OpenCode` operation without disclosing the loopback
    /// endpoint or per-run credential to the renderer.
    ///
    /// # Errors
    ///
    /// Returns an error if the sidecar is unavailable, the route is not a
    /// canonical API path, the request fails, or the response is not JSON.
    pub async fn request_json(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Value, RuntimeError> {
        if !path.starts_with('/') || path.contains("..") || path.contains(['?', '#']) {
            return Err(RuntimeError::Rejected(
                "runtime API path is not canonical".into(),
            ));
        }
        let endpoint = self.endpoint.as_ref().ok_or(RuntimeError::NotRunning)?;
        let url = endpoint
            .join(path.trim_start_matches('/'))
            .map_err(|_| RuntimeError::Rejected("runtime API URL is invalid".into()))?;
        let mut request = self.raw_client()?.request(method, url).query(query);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            let detail = String::from_utf8_lossy(&bytes)
                .chars()
                .take(2_000)
                .collect::<String>();
            return Err(RuntimeError::Rejected(format!(
                "OpenCode API returned HTTP {status}: {detail}"
            )));
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| RuntimeError::Rejected("OpenCode API returned invalid JSON".into()))
    }

    /// Read the complete `OpenCode` Desktop catalog through the native boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when the workspace cannot be canonicalized. Individual
    /// unavailable resources remain explicit inside the returned catalog.
    pub async fn desktop_catalog(&self, directory: &Path) -> Result<Value, RuntimeError> {
        let directory = std::fs::canonicalize(directory)?;
        let directory = directory.display().to_string();
        let query = [("directory", directory)];
        let resources = [
            ("sessions", "/session"),
            ("session_status", "/session/status"),
            ("providers", "/provider"),
            ("provider_auth", "/provider/auth"),
            ("agents", "/agent"),
            ("commands", "/command"),
            ("skills", "/skill"),
            ("mcp", "/mcp"),
            ("projects", "/project"),
            ("path", "/path"),
            ("vcs", "/vcs"),
            ("vcs_status", "/vcs/status"),
            ("permissions", "/permission"),
            ("questions", "/question"),
            ("config", "/config"),
            ("config_providers", "/config/providers"),
            ("shells", "/pty/shells"),
        ];
        let mut catalog = serde_json::Map::new();
        for (name, route) in resources {
            let value = match self
                .request_json(reqwest::Method::GET, route, &query, None)
                .await
            {
                Ok(value) => serde_json::json!({"available": true, "data": value}),
                Err(error) => serde_json::json!({
                    "available": false,
                    "reason": error.to_string()
                }),
            };
            catalog.insert(name.to_owned(), value);
        }
        Ok(Value::Object(catalog))
    }

    fn safety_config_overlay(&self) -> Result<String, RuntimeError> {
        let plugin = std::fs::canonicalize(&self.config.safety_plugin)?;
        if !plugin.is_file() {
            return Err(RuntimeError::Rejected(
                "OpenCode safety plugin is not a file".into(),
            ));
        }
        let plugin_url = Url::from_file_path(plugin).map_err(|()| {
            RuntimeError::Rejected("OpenCode safety plugin path is invalid".into())
        })?;
        let mut overlay = self.config.managed_config.clone();
        if !overlay.is_object() {
            return Err(RuntimeError::Rejected(
                "managed OpenCode configuration must be an object".into(),
            ));
        }
        let object = overlay
            .as_object_mut()
            .expect("validated OpenCode overlay is an object");
        object.insert("autoupdate".to_owned(), Value::Bool(false));
        object.insert(
            "plugin".to_owned(),
            Value::Array(vec![Value::String(plugin_url.to_string())]),
        );
        object.insert("share".to_owned(), Value::String("disabled".to_owned()));
        let agents = object
            .entry("agent".to_owned())
            .or_insert_with(|| serde_json::json!({}));
        if !agents.is_object() {
            return Err(RuntimeError::Rejected(
                "managed OpenCode agent configuration must be an object".into(),
            ));
        }
        let agents = agents.as_object_mut().expect("validated agent object");
        agents.insert("title".to_owned(), serde_json::json!({"disable": true}));
        agents.insert("summary".to_owned(), serde_json::json!({"disable": true}));
        if !self.config.providers.is_empty() {
            object.insert(
                "enabled_providers".to_owned(),
                Value::Array(
                    self.config
                        .providers
                        .keys()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
            object.insert(
                "provider".to_owned(),
                serde_json::to_value(&self.config.providers).map_err(|_| {
                    RuntimeError::Rejected("OpenCode provider configuration is invalid".into())
                })?,
            );
        }
        if let Some(model) = &self.config.default_model {
            object.insert("model".to_owned(), Value::String(model.clone()));
        }
        if let Some(model) = &self.config.small_model {
            object.insert("small_model".to_owned(), Value::String(model.clone()));
        }
        serde_json::to_string(&overlay)
            .map_err(|_| RuntimeError::Rejected("OpenCode safety overlay is invalid".into()))
    }

    fn prepare_profile(&self) -> Result<BTreeMap<String, String>, RuntimeError> {
        if !self.config.profile_root.is_absolute() {
            return Err(RuntimeError::Rejected(
                "OpenCode profile root must be an absolute path".into(),
            ));
        }
        let roots = [
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_STATE_HOME", "state"),
            ("APPDATA", "config"),
            ("LOCALAPPDATA", "data"),
            ("HOME", "home"),
            ("USERPROFILE", "home"),
            ("TMPDIR", "tmp"),
            ("TMP", "tmp"),
            ("TEMP", "tmp"),
        ];
        create_private_directory(&self.config.profile_root)?;
        let mut environment = BTreeMap::new();
        for (name, relative) in roots {
            let path = self.config.profile_root.join(relative);
            create_private_directory(&path)?;
            environment.insert(name.to_owned(), path.display().to_string());
        }
        let config_dir = self.config.profile_root.join("config/opencode");
        create_private_directory(&config_dir)?;
        environment.insert(
            "OPENCODE_CONFIG_DIR".to_owned(),
            config_dir.display().to_string(),
        );
        environment.insert(
            "OPENCODE_TEST_HOME".to_owned(),
            self.config.profile_root.join("home").display().to_string(),
        );
        environment.insert("OPENCODE_DISABLE_AUTOUPDATE".to_owned(), "1".to_owned());
        environment.insert("OPENCODE_DISABLE_PROJECT_CONFIG".to_owned(), "1".to_owned());
        Ok(environment)
    }

    fn isolated_command(&self) -> Result<Command, RuntimeError> {
        let managed = self.prepare_profile()?;
        let mut command = Command::new(&self.config.executable);
        command.env_clear();
        for name in SAFE_AMBIENT_ENVIRONMENT {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command.envs(managed);
        Ok(command)
    }

    async fn executable_version(&self) -> Result<String, RuntimeError> {
        let mut command = self.isolated_command()?;
        let output = command
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .await?;
        if !output.status.success() {
            return Err(RuntimeError::Rejected(format!(
                "OpenCode version probe exited with {}",
                output.status
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    async fn api_health(&mut self) -> Result<RuntimeHealth, RuntimeError> {
        let endpoint = self.endpoint.as_ref().ok_or(RuntimeError::NotRunning)?;
        let child = self.child.as_mut().ok_or(RuntimeError::NotRunning)?;
        if let Some(status) = child.try_wait()? {
            return Ok(RuntimeHealth {
                healthy: false,
                version: self.config.version.clone(),
                detail: format!("process exited with {status}"),
            });
        }
        let response = reqwest::Client::new()
            .get(endpoint.join("api/health").expect("health URL"))
            .basic_auth(
                &self.config.username,
                Some(self.config.password.expose_secret()),
            )
            .timeout(Duration::from_secs(2))
            .send()
            .await?;
        let status = response.status();
        let payload: Value = response.json().await?;
        let healthy = status.is_success()
            && payload
                .get("healthy")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        Ok(RuntimeHealth {
            healthy,
            version: self.config.version.clone(),
            detail: if healthy {
                "authenticated loopback API healthy".to_owned()
            } else {
                format!("health endpoint returned HTTP {status}")
            },
        })
    }

    async fn verify_openapi_contract(&self) -> Result<(), RuntimeError> {
        let endpoint = self.endpoint.as_ref().ok_or(RuntimeError::NotRunning)?;
        if self.child.is_none() {
            return Err(RuntimeError::NotRunning);
        }
        let response = reqwest::Client::new()
            .get(endpoint.join("doc").expect("OpenAPI URL"))
            .basic_auth(
                &self.config.username,
                Some(self.config.password.expose_secret()),
            )
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?;
        let document = response.bytes().await?;
        let digest = Sha256::digest(&document);
        let mut found = String::with_capacity(digest.len() * 2);
        for byte in digest {
            write!(&mut found, "{byte:02x}").expect("writing to String cannot fail");
        }
        if found == OPENCODE_OPENAPI_SHA256 {
            Ok(())
        } else {
            Err(RuntimeError::IncompatibleApiContract {
                expected: OPENCODE_OPENAPI_SHA256.to_owned(),
                found,
            })
        }
    }

    async fn await_health(&mut self) -> Result<RuntimeHealth, RuntimeError> {
        let deadline = tokio::time::Instant::now() + self.config.startup_timeout;
        loop {
            let detail = match self.api_health().await {
                Ok(health) if health.healthy => return Ok(health),
                Ok(health) => health.detail,
                Err(error) => error.to_string(),
            };
            if tokio::time::Instant::now() >= deadline {
                return Err(RuntimeError::HealthTimeout(detail));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Submit text plus `OpenCode` file/image parts while keeping the sidecar
    /// endpoint and authentication entirely inside the native boundary.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid or oversized attachments, an unknown
    /// session, a rejected prompt, or a failed authenticated sidecar request.
    pub async fn submit_with_attachments(
        &mut self,
        session_id: &str,
        prompt: &str,
        attachments: Vec<Value>,
    ) -> Result<mpsc::Receiver<EventEnvelope>, RuntimeError> {
        if prompt.trim().is_empty() {
            return Err(RuntimeError::Rejected("prompt must not be blank".into()));
        }
        let mut parts = vec![serde_json::json!({"type": "text", "text": prompt})];
        for attachment in attachments {
            let object = attachment.as_object().ok_or_else(|| {
                RuntimeError::Rejected("prompt attachment must be an object".into())
            })?;
            let mime = object.get("mime").and_then(Value::as_str).unwrap_or("");
            let url = object.get("url").and_then(Value::as_str).unwrap_or("");
            let filename = object
                .get("filename")
                .and_then(Value::as_str)
                .unwrap_or("attachment");
            if object.get("type").and_then(Value::as_str) != Some("file")
                || mime.is_empty()
                || mime.len() > 255
                || filename.is_empty()
                || filename.len() > 512
                || !url.starts_with("data:")
                || url.len() > 36 * 1024 * 1024
            {
                return Err(RuntimeError::Rejected(
                    "prompt attachment is invalid or exceeds the 25 MiB limit".into(),
                ));
            }
            parts.push(serde_json::json!({
                "type": "file", "mime": mime, "filename": filename, "url": url,
            }));
        }
        let directory = self.registered_session_directory(session_id)?;
        let client = self.generated_client()?.clone();
        let event_response = client
            .event_subscribe()
            .directory(&directory)
            .send()
            .await
            .map_err(|_| api_failure("session event subscription"))?;
        let session = client
            .session_get()
            .session_id(session_id)
            .directory(&directory)
            .send()
            .await
            .map_err(|_| api_failure("session prompt context"))?;
        let session = serde_json::to_value(session.into_inner())
            .map_err(|_| api_failure("session prompt context response"))?;
        let message_id = format!("msg_{}", Uuid::new_v4().simple());
        let mut prompt_body = serde_json::Map::from_iter([
            ("messageID".to_owned(), Value::String(message_id.clone())),
            ("parts".to_owned(), Value::Array(parts)),
        ]);
        if let (Some(provider_id), Some(model_id)) = (
            session.pointer("/model/providerID").and_then(Value::as_str),
            session.pointer("/model/id").and_then(Value::as_str),
        ) {
            prompt_body.insert(
                "model".to_owned(),
                serde_json::json!({"providerID": provider_id, "modelID": model_id}),
            );
        }
        if let Some(agent) = session.get("agent").and_then(Value::as_str) {
            prompt_body.insert("agent".to_owned(), Value::String(agent.to_owned()));
        }
        if let Some(variant) = session.pointer("/model/variant").and_then(Value::as_str) {
            prompt_body.insert("variant".to_owned(), Value::String(variant.to_owned()));
        }
        let body = generated_body::<opencode_api::types::SessionPromptAsyncBody>(
            Value::Object(prompt_body),
            "session prompt body",
        )?;
        client
            .session_prompt_async()
            .session_id(session_id)
            .directory(&directory)
            .body(body)
            .send()
            .await
            .map_err(|_| api_failure("session prompt"))?;
        let (tx, rx) = mpsc::channel(128);
        tx.send(runtime_event(
            1,
            session_id,
            "response.admitted",
            &serde_json::json!({"message_id": message_id}),
        )?)
        .await
        .map_err(|_| RuntimeError::StreamClosed)?;
        let session_id = session_id.to_owned();
        tokio::spawn(forward_sse_events(
            event_response.into_inner(),
            tx,
            session_id,
            2,
        ));
        Ok(rx)
    }
}

#[async_trait]
impl AgentRuntime for OpenCodeSidecar {
    async fn start(&mut self) -> Result<RuntimeHealth, RuntimeError> {
        if self.child.is_some() {
            return self.health().await;
        }
        if self.config.version != OPENCODE_VERSION {
            return Err(RuntimeError::IncompatibleVersion {
                expected: OPENCODE_VERSION.to_owned(),
                found: self.config.version.clone(),
            });
        }
        let found = self.executable_version().await?;
        if found != self.config.version {
            return Err(RuntimeError::IncompatibleVersion {
                expected: self.config.version.clone(),
                found,
            });
        }
        let safety_overlay = self.safety_config_overlay()?;
        let bridge = NativeToolBridge::start(Arc::clone(&self.session_directories)).await?;
        let port = Self::reserve_loopback_port()?;
        let mut command = self.isolated_command()?;
        command
            .args([
                "serve",
                "--hostname",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .env("OPENCODE_SERVER_USERNAME", &self.config.username)
            .env(
                "OPENCODE_SERVER_PASSWORD",
                self.config.password.expose_secret(),
            )
            .env("OPENCODE_CONFIG_CONTENT", safety_overlay)
            .env("PERSONAL_AGENT_TOOL_GATEWAY_URL", bridge.endpoint.as_str())
            .env(
                "PERSONAL_AGENT_TOOL_GATEWAY_TOKEN",
                bridge.token.expose_secret(),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = command.spawn()?;
        self.tool_bridge = Some(bridge);
        self.child = Some(child);
        self.endpoint =
            Some(Url::parse(&format!("http://127.0.0.1:{port}/")).expect("loopback URL"));
        self.client = match self.build_generated_client() {
            Ok(client) => Some(client),
            Err(error) => {
                let _ = self.stop().await;
                return Err(error);
            }
        };
        match self.await_health().await {
            Ok(health) => match self.verify_openapi_contract().await {
                Ok(()) => Ok(health),
                Err(error) => {
                    let _ = self.stop().await;
                    Err(error)
                }
            },
            Err(error) => {
                let _ = self.stop().await;
                Err(error)
            }
        }
    }

    async fn health(&mut self) -> Result<RuntimeHealth, RuntimeError> {
        self.api_health().await
    }

    async fn stop(&mut self) -> Result<(), RuntimeError> {
        let mut failure = None;
        if let Some(mut child) = self.child.take() {
            if let Err(error) = child.kill().await {
                failure = Some(error);
            } else if let Err(error) = child.wait().await {
                failure = Some(error);
            }
        }
        self.endpoint = None;
        self.client = None;
        self.tool_bridge = None;
        if let Ok(mut sessions) = self.session_directories.write() {
            sessions.clear();
        }
        failure.map_or(Ok(()), |error| Err(error.into()))
    }

    async fn discover_models(
        &mut self,
        working_directory: Option<&Path>,
    ) -> Result<Vec<ModelCapability>, RuntimeError> {
        let mut request = self.generated_client()?.provider_list();
        let directory = working_directory
            .map(std::fs::canonicalize)
            .transpose()?
            .map(|path| path.display().to_string());
        if let Some(directory) = directory.as_deref() {
            request = request.directory(directory);
        }
        let response = request
            .send()
            .await
            .map_err(|_| api_failure("model discovery"))?;
        let value = serde_json::to_value(response.into_inner())
            .map_err(|_| api_failure("model response decoding"))?;
        let providers = value
            .get("all")
            .and_then(Value::as_array)
            .ok_or_else(|| api_failure("provider response shape"))?;
        Ok(providers
            .iter()
            .flat_map(|provider| {
                let provider_id = provider
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                provider
                    .get("models")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .filter_map(move |(catalog_id, model)| {
                        if provider_id.is_empty() {
                            return None;
                        }
                        let model_id = model
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or(catalog_id)
                            .to_owned();
                        Some(ModelCapability {
                            local: is_local_provider(&provider_id),
                            reasoning: model
                                .pointer("/capabilities/reasoning")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            tool_calls: model
                                .pointer("/capabilities/toolcall")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            provider_id: provider_id.clone(),
                            model_id,
                            context_tokens: numeric_u64(model.pointer("/limit/context")),
                            input_modalities: enabled_modalities(
                                model.pointer("/capabilities/input"),
                            ),
                            output_modalities: enabled_modalities(
                                model.pointer("/capabilities/output"),
                            ),
                        })
                    })
            })
            .collect())
    }
    async fn begin_session(&mut self, options: SessionOptions) -> Result<String, RuntimeError> {
        if !options.environment.is_empty() {
            return Err(RuntimeError::Rejected(
                "per-session environment variables are not supported by the pinned runtime API"
                    .into(),
            ));
        }
        if options.effort.is_some() && options.model.is_none() {
            return Err(RuntimeError::Rejected(
                "model effort requires an explicit provider/model selection".into(),
            ));
        }
        let directory = std::fs::canonicalize(&options.working_directory)?;
        if !directory.is_dir() {
            return Err(RuntimeError::Rejected(
                "session working directory is not a directory".into(),
            ));
        }
        let directory_text = directory.display().to_string();
        let mut body = serde_json::Map::new();
        if let Some(agent) = options.agent {
            body.insert("agent".to_owned(), Value::String(agent));
        }
        if let Some(model) = options.model {
            let (provider_id, model_id) = model.split_once('/').ok_or_else(|| {
                RuntimeError::Rejected("model must use provider/model syntax".into())
            })?;
            if provider_id.is_empty() || model_id.is_empty() {
                return Err(RuntimeError::Rejected(
                    "model must use non-empty provider/model syntax".into(),
                ));
            }
            body.insert(
                "model".to_owned(),
                serde_json::json!({
                    "providerID": provider_id,
                    "id": model_id,
                    "variant": options.effort,
                }),
            );
        }
        let body = generated_body::<opencode_api::types::SessionCreateBody>(
            Value::Object(body),
            "session create body",
        )?;
        let response = self
            .generated_client()?
            .session_create()
            .directory(&directory_text)
            .body(body)
            .send()
            .await
            .map_err(|_| api_failure("session create"))?;
        let session_id = response_identifier(response.into_inner(), "/id", "session create")?;
        self.register_session_directory(session_id.clone(), directory)?;
        Ok(session_id)
    }
    async fn resume_session(
        &mut self,
        session_id: &str,
        working_directory: &Path,
    ) -> Result<(), RuntimeError> {
        let directory = std::fs::canonicalize(working_directory)?;
        let directory_text = directory.display().to_string();
        self.generated_client()?
            .session_get()
            .session_id(session_id)
            .directory(&directory_text)
            .send()
            .await
            .map_err(|_| api_failure("session resume"))?;
        self.register_session_directory(session_id.to_owned(), directory)?;
        Ok(())
    }
    async fn compact_session(&mut self, session_id: &str) -> Result<(), RuntimeError> {
        let directory = self.registered_session_directory(session_id)?;
        let session = self
            .generated_client()?
            .session_get()
            .session_id(session_id)
            .directory(&directory)
            .send()
            .await
            .map_err(|_| api_failure("session compact model lookup"))?;
        let session = serde_json::to_value(session.into_inner())
            .map_err(|_| api_failure("session compact model response"))?;
        let provider_id = session
            .pointer("/model/providerID")
            .and_then(Value::as_str)
            .ok_or_else(|| api_failure("session compact model selection"))?;
        let model_id = session
            .pointer("/model/id")
            .and_then(Value::as_str)
            .ok_or_else(|| api_failure("session compact model selection"))?;
        let body = generated_body::<opencode_api::types::SessionSummarizeBody>(
            serde_json::json!({
                "providerID": provider_id,
                "modelID": model_id,
                "auto": false,
            }),
            "session compact body",
        )?;
        self.generated_client()?
            .session_summarize()
            .session_id(session_id)
            .directory(&directory)
            .body(body)
            .send()
            .await
            .map_err(|_| api_failure("session compact"))?;
        Ok(())
    }
    async fn fork_session(&mut self, session_id: &str) -> Result<String, RuntimeError> {
        let directory = self.registered_session_directory(session_id)?;
        let body = generated_body::<opencode_api::types::SessionForkBody>(
            serde_json::json!({}),
            "session fork body",
        )?;
        let response = self
            .generated_client()?
            .session_fork()
            .session_id(session_id)
            .directory(&directory)
            .body(body)
            .send()
            .await
            .map_err(|_| api_failure("session fork"))?;
        let fork = response_identifier(response.into_inner(), "/id", "session fork")?;
        self.register_session_directory(fork.clone(), PathBuf::from(directory))?;
        Ok(fork)
    }
    async fn abort_session(&mut self, session_id: &str) -> Result<(), RuntimeError> {
        let directory = self.registered_session_directory(session_id)?;
        self.generated_client()?
            .session_abort()
            .session_id(session_id)
            .directory(&directory)
            .send()
            .await
            .map_err(|_| api_failure("session abort"))?;
        Ok(())
    }
    async fn submit(
        &mut self,
        session_id: &str,
        prompt: &str,
        plan: Option<Value>,
    ) -> Result<mpsc::Receiver<EventEnvelope>, RuntimeError> {
        if plan.is_some() {
            return Err(RuntimeError::Rejected(
                "structured plan submission is not supported by the pinned runtime API".into(),
            ));
        }
        self.submit_with_attachments(session_id, prompt, Vec::new())
            .await
    }
    async fn answer(
        &mut self,
        session_id: &str,
        answer: RuntimeAnswer,
    ) -> Result<(), RuntimeError> {
        let kind = answer
            .answer
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| RuntimeError::Rejected("runtime answer kind is required".into()))?;
        let directory = self.registered_session_directory(session_id)?;
        match kind {
            "permission" => {
                let body = generated_body::<opencode_api::types::PermissionReplyBody>(
                    serde_json::json!({
                        "reply": answer.answer.get("reply"),
                        "message": answer.answer.get("message"),
                    }),
                    "permission answer",
                )?;
                self.generated_client()?
                    .permission_reply()
                    .request_id(&answer.request_id)
                    .directory(&directory)
                    .body(body)
                    .send()
                    .await
                    .map_err(|_| api_failure("permission answer"))?;
            }
            "question" if answer.answer.get("reject") == Some(&Value::Bool(true)) => {
                self.generated_client()?
                    .question_reject()
                    .request_id(&answer.request_id)
                    .directory(&directory)
                    .send()
                    .await
                    .map_err(|_| api_failure("question rejection"))?;
            }
            "question" => {
                let body = generated_body::<opencode_api::types::QuestionReplyBody>(
                    serde_json::json!({"answers": answer.answer.get("answers")}),
                    "question answer",
                )?;
                self.generated_client()?
                    .question_reply()
                    .request_id(&answer.request_id)
                    .directory(&directory)
                    .body(body)
                    .send()
                    .await
                    .map_err(|_| api_failure("question answer"))?;
            }
            _ => {
                return Err(RuntimeError::Rejected(
                    "runtime answer kind must be permission or question".into(),
                ));
            }
        }
        Ok(())
    }
}

fn api_failure(operation: &str) -> RuntimeError {
    RuntimeError::Rejected(format!("OpenCode {operation} failed"))
}

fn generated_body<T>(value: Value, operation: &str) -> Result<T, RuntimeError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(value).map_err(|_| api_failure(operation))
}

fn response_identifier<T>(
    response: T,
    pointer: &str,
    operation: &str,
) -> Result<String, RuntimeError>
where
    T: Serialize,
{
    serde_json::to_value(response)
        .ok()
        .and_then(|value| {
            value
                .pointer(pointer)
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .ok_or_else(|| api_failure(&format!("{operation} response shape")))
}

fn numeric_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value.as_u64().or_else(|| {
            let number = value.as_f64()?;
            if number.is_finite() && number >= 0.0 && number.fract() == 0.0 {
                format!("{number:.0}").parse().ok()
            } else {
                None
            }
        })
    })
}

fn enabled_modalities(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, enabled)| enabled.as_bool() == Some(true))
        .map(|(modality, _)| modality.clone())
        .collect()
}

fn is_local_provider(provider_id: &str) -> bool {
    let provider = provider_id.to_ascii_lowercase();
    ["ollama", "lmstudio", "llama.cpp", "local"]
        .iter()
        .any(|marker| provider.contains(marker))
}

fn runtime_event(
    sequence: u64,
    session_id: &str,
    event_type: &str,
    payload: &Value,
) -> Result<EventEnvelope, RuntimeError> {
    let mut event = EventEnvelope::new(sequence, "opencode", "default", event_type, payload)?;
    event.session_id = Some(session_id.to_owned());
    Ok(event)
}

fn selected_properties(properties: &Value, names: &[&str]) -> Value {
    Value::Object(
        names
            .iter()
            .filter_map(|name| {
                properties
                    .get(*name)
                    .cloned()
                    .map(|value| ((*name).to_owned(), value))
            })
            .collect(),
    )
}

#[allow(clippy::too_many_lines)] // Explicit exhaustive normalization keeps upstream data filtering reviewable.
fn normalize_upstream_event(
    sequence: u64,
    fallback_session_id: &str,
    upstream: &Value,
    known_part_type: Option<&str>,
) -> Result<(EventEnvelope, bool), RuntimeError> {
    let upstream_type = upstream
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let properties = upstream.get("properties").unwrap_or(&Value::Null);
    let session_id = properties
        .get("sessionID")
        .and_then(Value::as_str)
        .unwrap_or(fallback_session_id);
    let (event_type, payload, terminal) = match upstream_type {
        "message.part.delta" => match known_part_type {
            Some("text") => (
                "response.delta",
                selected_properties(properties, &["messageID", "partID", "delta"]),
                false,
            ),
            Some("reasoning") => (
                "reasoning.available",
                selected_properties(properties, &["messageID", "partID"]),
                false,
            ),
            _ => (
                "runtime.upstream_event",
                serde_json::json!({"upstream_type": upstream_type}),
                false,
            ),
        },
        "message.part.updated" => {
            let part = properties.get("part").unwrap_or(&Value::Null);
            let part_type = part
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            match part_type {
                "tool" => {
                    let status = part
                        .pointer("/state/status")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    let event_type = match status {
                        "running" => "tool.started",
                        "completed" => "tool.completed",
                        "error" => "tool.failed",
                        _ => "tool.progress",
                    };
                    (
                        event_type,
                        serde_json::json!({
                            "assistantMessageID": part.get("messageID"),
                            "callID": part.get("callID"),
                            "tool": part.get("tool"),
                            "status": status,
                        }),
                        false,
                    )
                }
                "reasoning" => (
                    "reasoning.available",
                    selected_properties(part, &["messageID", "id"]),
                    false,
                ),
                "step-start" => (
                    "response.started",
                    selected_properties(part, &["messageID", "id"]),
                    false,
                ),
                "step-finish" => (
                    "response.step_completed",
                    selected_properties(part, &["messageID", "reason", "cost", "tokens"]),
                    false,
                ),
                "text" => (
                    "response.text_state",
                    selected_properties(part, &["messageID", "id", "time"]),
                    false,
                ),
                _ => (
                    "runtime.upstream_event",
                    serde_json::json!({"upstream_type": upstream_type, "part_type": part_type}),
                    false,
                ),
            }
        }
        "session.next.text.delta" => (
            "response.delta",
            selected_properties(properties, &["assistantMessageID", "textID", "delta"]),
            false,
        ),
        "session.next.reasoning.started" | "session.next.reasoning.delta" => (
            "reasoning.available",
            selected_properties(properties, &["assistantMessageID", "reasoningID"]),
            false,
        ),
        "session.next.reasoning.ended" => (
            "reasoning.completed",
            selected_properties(properties, &["assistantMessageID", "reasoningID"]),
            false,
        ),
        "session.next.tool.called" => (
            "tool.started",
            selected_properties(
                properties,
                &["assistantMessageID", "callID", "tool", "provider"],
            ),
            false,
        ),
        "session.next.tool.input.started"
        | "session.next.tool.input.delta"
        | "session.next.tool.input.ended"
        | "session.next.tool.progress" => (
            "tool.progress",
            selected_properties(properties, &["assistantMessageID", "callID", "tool"]),
            false,
        ),
        "session.next.tool.success" => (
            "tool.completed",
            selected_properties(
                properties,
                &["assistantMessageID", "callID", "outputPaths", "provider"],
            ),
            false,
        ),
        "session.next.tool.failed" => (
            "tool.failed",
            selected_properties(properties, &["assistantMessageID", "callID", "tool"]),
            false,
        ),
        "permission.v2.asked" | "permission.asked" => (
            "approval.requested",
            selected_properties(
                properties,
                &[
                    "id",
                    "action",
                    "permission",
                    "resources",
                    "patterns",
                    "save",
                ],
            ),
            false,
        ),
        "question.v2.asked" | "question.asked" => (
            "clarification.requested",
            selected_properties(properties, &["id", "questions"]),
            false,
        ),
        "session.next.step.started" => (
            "response.started",
            selected_properties(properties, &["assistantMessageID"]),
            false,
        ),
        "session.next.step.ended" => (
            "response.step_completed",
            selected_properties(
                properties,
                &["assistantMessageID", "finish", "cost", "tokens", "files"],
            ),
            false,
        ),
        "session.next.step.failed" | "session.error" => (
            "response.failed",
            selected_properties(properties, &["assistantMessageID", "error"]),
            true,
        ),
        "session.status" => {
            let status = properties
                .pointer("/status/type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            match status {
                "idle" => (
                    "response.completed",
                    serde_json::json!({"terminal": true, "source": "session.status"}),
                    true,
                ),
                "retry" => (
                    "response.retrying",
                    selected_properties(
                        properties.get("status").unwrap_or(&Value::Null),
                        &["attempt", "message", "next"],
                    ),
                    false,
                ),
                "busy" => (
                    "response.started",
                    serde_json::json!({"source": "session.status"}),
                    false,
                ),
                _ => (
                    "runtime.upstream_event",
                    serde_json::json!({"upstream_type": upstream_type, "status": status}),
                    false,
                ),
            }
        }
        "session.idle" => (
            "response.completed",
            serde_json::json!({"terminal": true}),
            true,
        ),
        _ => (
            "runtime.upstream_event",
            serde_json::json!({"upstream_type": upstream_type}),
            false,
        ),
    };
    Ok((
        runtime_event(sequence, session_id, event_type, &payload)?,
        terminal,
    ))
}

fn upstream_session_id(upstream: &Value) -> Option<&str> {
    upstream
        .pointer("/properties/sessionID")
        .or_else(|| upstream.pointer("/properties/part/sessionID"))
        .or_else(|| upstream.pointer("/properties/info/sessionID"))
        .and_then(Value::as_str)
}

fn frame_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|at| (at, 2));
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| (at, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(found), None) | (None, Some(found)) => Some(found),
        (None, None) => None,
    }
}

fn take_sse_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let (at, delimiter_length) = frame_delimiter(buffer)?;
    let frame = buffer[..at].to_vec();
    buffer.drain(..at + delimiter_length);
    Some(frame)
}

fn parse_sse_frame(frame: &[u8]) -> Result<Option<Value>, RuntimeError> {
    let frame = std::str::from_utf8(frame)
        .map_err(|_| RuntimeError::Rejected("OpenCode event stream was not UTF-8".into()))?;
    let data = frame
        .lines()
        .filter_map(|line| line.trim_end_matches('\r').strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return Ok(None);
    }
    if data == "[DONE]" {
        return Ok(None);
    }
    let outer: Value = serde_json::from_str(&data)
        .map_err(|_| RuntimeError::Rejected("OpenCode event frame was not JSON".into()))?;
    if let Some(inner) = outer.get("data").and_then(Value::as_str) {
        return serde_json::from_str(inner)
            .map(Some)
            .map_err(|_| RuntimeError::Rejected("OpenCode durable event was not JSON".into()));
    }
    Ok(Some(outer))
}

async fn forward_sse_events(
    mut stream: progenitor_client::ByteStream,
    tx: mpsc::Sender<EventEnvelope>,
    session_id: String,
    mut sequence: u64,
) {
    let mut buffer = Vec::new();
    let mut part_types = BTreeMap::<String, String>::new();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            let _ = tx
                .send(
                    runtime_event(
                        sequence,
                        &session_id,
                        "runtime.stream_error",
                        &serde_json::json!({"recoverable": true}),
                    )
                    .expect("static runtime error event"),
                )
                .await;
            return;
        };
        buffer.extend_from_slice(&chunk);
        if buffer.len() > 1_048_576 {
            let _ = tx
                .send(
                    runtime_event(
                        sequence,
                        &session_id,
                        "runtime.stream_error",
                        &serde_json::json!({"reason": "frame exceeded 1 MiB"}),
                    )
                    .expect("static runtime error event"),
                )
                .await;
            return;
        }
        while let Some(frame) = take_sse_frame(&mut buffer) {
            let upstream = match parse_sse_frame(&frame) {
                Ok(Some(upstream)) => upstream,
                Ok(None) => continue,
                Err(_) => {
                    let _ = tx
                        .send(
                            runtime_event(
                                sequence,
                                &session_id,
                                "runtime.stream_error",
                                &serde_json::json!({"reason": "invalid event frame"}),
                            )
                            .expect("static runtime error event"),
                        )
                        .await;
                    return;
                }
            };
            if upstream_session_id(&upstream) != Some(session_id.as_str()) {
                continue;
            }
            if upstream.get("type").and_then(Value::as_str) == Some("message.part.updated")
                && let (Some(part_id), Some(part_type)) = (
                    upstream
                        .pointer("/properties/part/id")
                        .and_then(Value::as_str),
                    upstream
                        .pointer("/properties/part/type")
                        .and_then(Value::as_str),
                )
            {
                part_types.insert(part_id.to_owned(), part_type.to_owned());
            }
            let known_part_type = upstream
                .pointer("/properties/partID")
                .and_then(Value::as_str)
                .and_then(|part_id| part_types.get(part_id))
                .map(String::as_str);
            let Ok((event, terminal)) =
                normalize_upstream_event(sequence, &session_id, &upstream, known_part_type)
            else {
                return;
            };
            if tx.send(event).await.is_err() {
                return;
            }
            sequence += 1;
            if terminal {
                return;
            }
        }
    }
    let _ = tx
        .send(
            runtime_event(
                sequence,
                &session_id,
                "runtime.stream_closed",
                &serde_json::json!({"terminal": true, "completed": false}),
            )
            .expect("static runtime closed event"),
        )
        .await;
}

/// Deterministic provider used by CI and safety evaluations.
pub struct FakeRuntime {
    running: bool,
    pub scripted_events: Vec<EventEnvelope>,
    sessions: Vec<String>,
}

impl FakeRuntime {
    #[must_use]
    pub fn new(scripted_events: Vec<EventEnvelope>) -> Self {
        Self {
            running: false,
            scripted_events,
            sessions: Vec::new(),
        }
    }

    fn require_session(&self, session_id: &str) -> Result<(), RuntimeError> {
        if self.sessions.iter().any(|id| id == session_id) {
            Ok(())
        } else {
            Err(RuntimeError::Rejected("unknown session".into()))
        }
    }
}

#[async_trait]
impl AgentRuntime for FakeRuntime {
    async fn start(&mut self) -> Result<RuntimeHealth, RuntimeError> {
        self.running = true;
        Ok(RuntimeHealth {
            healthy: true,
            version: "fake-1".into(),
            detail: "deterministic fixture provider".into(),
        })
    }
    async fn health(&mut self) -> Result<RuntimeHealth, RuntimeError> {
        Ok(RuntimeHealth {
            healthy: self.running,
            version: "fake-1".into(),
            detail: "deterministic fixture provider".into(),
        })
    }
    async fn stop(&mut self) -> Result<(), RuntimeError> {
        self.running = false;
        Ok(())
    }
    async fn discover_models(
        &mut self,
        _working_directory: Option<&Path>,
    ) -> Result<Vec<ModelCapability>, RuntimeError> {
        Ok(vec![ModelCapability {
            provider_id: "fixture".into(),
            model_id: "deterministic".into(),
            context_tokens: Some(4096),
            local: true,
            reasoning: false,
            tool_calls: true,
            input_modalities: vec!["text".into()],
            output_modalities: vec!["text".into()],
        }])
    }
    async fn begin_session(&mut self, _options: SessionOptions) -> Result<String, RuntimeError> {
        if !self.running {
            return Err(RuntimeError::NotRunning);
        }
        let id = Uuid::now_v7().to_string();
        self.sessions.push(id.clone());
        Ok(id)
    }
    async fn resume_session(
        &mut self,
        session_id: &str,
        _working_directory: &Path,
    ) -> Result<(), RuntimeError> {
        self.require_session(session_id)
    }
    async fn compact_session(&mut self, session_id: &str) -> Result<(), RuntimeError> {
        self.require_session(session_id)
    }
    async fn fork_session(&mut self, session_id: &str) -> Result<String, RuntimeError> {
        self.require_session(session_id)?;
        let id = Uuid::now_v7().to_string();
        self.sessions.push(id.clone());
        Ok(id)
    }
    async fn abort_session(&mut self, session_id: &str) -> Result<(), RuntimeError> {
        self.require_session(session_id)
    }
    async fn submit(
        &mut self,
        session_id: &str,
        _prompt: &str,
        _plan: Option<Value>,
    ) -> Result<mpsc::Receiver<EventEnvelope>, RuntimeError> {
        self.require_session(session_id)?;
        let (tx, rx) = mpsc::channel(self.scripted_events.len().max(1));
        for event in self.scripted_events.clone() {
            tx.send(event)
                .await
                .map_err(|_| RuntimeError::StreamClosed)?;
        }
        Ok(rx)
    }
    async fn answer(
        &mut self,
        session_id: &str,
        _answer: RuntimeAnswer,
    ) -> Result<(), RuntimeError> {
        self.require_session(session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn spawn_openai_compatible_fixture(metadata_path: &std::path::Path) -> (Child, u16) {
        let port = OpenCodeSidecar::reserve_loopback_port().expect("fixture port");
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/fixtures/openai-compatible.ts");
        let mut child = Command::new("bun")
            .arg(script)
            .arg(format!("--port={port}"))
            .arg(format!("--metadata-path={}", metadata_path.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("start synthetic provider");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                Duration::from_millis(100),
            )
            .is_ok()
            {
                break;
            }
            assert!(
                child
                    .try_wait()
                    .expect("synthetic provider status")
                    .is_none(),
                "synthetic provider exited during startup"
            );
            assert!(
                tokio::time::Instant::now() < deadline,
                "synthetic provider startup timeout"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        (child, port)
    }

    fn safety_plugin() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/opencode-plugin/src/index.ts")
    }

    fn isolated_sidecar_config(executable: PathBuf, profile: &std::path::Path) -> OpenCodeConfig {
        let mut config = OpenCodeConfig::pinned(
            executable,
            safety_plugin(),
            profile.join("opencode-profile"),
        );
        config.startup_timeout = Duration::from_secs(60);
        config
    }

    fn bundled_opencode() -> PathBuf {
        let target = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            "x86_64-unknown-linux-gnu"
        } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            "aarch64-unknown-linux-gnu"
        } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            "x86_64-apple-darwin"
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            "aarch64-apple-darwin"
        } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            "x86_64-pc-windows-msvc"
        } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
            "aarch64-pc-windows-msvc"
        } else {
            panic!("unsupported OpenCode test target")
        };
        let extension = if cfg!(target_os = "windows") {
            ".exe"
        } else {
            ""
        };
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../apps/desktop/src-tauri/binaries/opencode-{target}{extension}"
        ))
    }

    #[test]
    fn sidecar_profile_is_private_and_excludes_ambient_credentials() {
        let temp = tempfile::tempdir().expect("temp profile");
        let config = isolated_sidecar_config(PathBuf::from("unused-opencode"), temp.path());
        let runtime = OpenCodeSidecar::new(config);
        let environment = runtime.prepare_profile().expect("managed profile");
        let expected_root = temp.path().join("opencode-profile");
        assert_eq!(
            environment.get("HOME").map(String::as_str),
            Some(expected_root.join("home").to_string_lossy().as_ref())
        );
        assert_eq!(
            environment.get("OPENCODE_CONFIG_DIR").map(String::as_str),
            Some(
                expected_root
                    .join("config/opencode")
                    .to_string_lossy()
                    .as_ref()
            )
        );
        for forbidden in [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENCODE_CONFIG_CONTENT",
            "HTTP_PROXY",
            "NPM_CONFIG_USERCONFIG",
        ] {
            assert!(!environment.contains_key(forbidden));
            assert!(!SAFE_AMBIENT_ENVIRONMENT.contains(&forbidden));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(expected_root)
                    .expect("profile metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[tokio::test]
    async fn native_tool_bridge_requires_authentication_and_registered_session_scope() {
        let temp = tempfile::tempdir().expect("temp profile");
        let directory = std::fs::canonicalize(temp.path()).expect("canonical fixture directory");
        let sessions = Arc::new(RwLock::new(BTreeMap::new()));
        let bridge = NativeToolBridge::start(Arc::clone(&sessions))
            .await
            .expect("native bridge");
        let client = reqwest::Client::new();
        let body = serde_json::json!({
            "session_id": "session-fixture",
            "directory": directory.display().to_string(),
        });
        let unauthorized = client
            .post(bridge.endpoint.clone())
            .bearer_auth("wrong-token")
            .json(&body)
            .send()
            .await
            .expect("unauthorized request");
        assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);
        let unregistered = client
            .post(bridge.endpoint.clone())
            .bearer_auth(bridge.token.expose_secret())
            .json(&body)
            .send()
            .await
            .expect("unregistered request");
        assert_eq!(unregistered.status(), reqwest::StatusCode::FORBIDDEN);
        sessions
            .write()
            .expect("session registry")
            .insert("session-fixture".into(), directory);
        let accepted = client
            .post(bridge.endpoint.clone())
            .bearer_auth(bridge.token.expose_secret())
            .json(&body)
            .send()
            .await
            .expect("registered request");
        assert_eq!(accepted.status(), reqwest::StatusCode::OK);
        assert_eq!(
            accepted
                .json::<Value>()
                .await
                .expect("native bridge response")["boundary"],
            "native-tool-gateway"
        );
        assert_eq!(bridge.audit_count().await, 1);
    }

    #[tokio::test]
    async fn bundled_sidecar_is_exact_authenticated_and_healthy() {
        let executable = bundled_opencode();
        assert!(
            executable.is_file(),
            "missing bundled sidecar; run `bun run sidecar:fetch`"
        );
        let temp = tempfile::tempdir().expect("temp profile");
        let config = isolated_sidecar_config(executable, temp.path());
        let mut runtime = OpenCodeSidecar::new(config);
        let health = runtime.start().await.expect("authenticated sidecar start");
        assert!(health.healthy);
        assert_eq!(health.version, OPENCODE_VERSION);
        assert_eq!(
            runtime.endpoint().expect("endpoint").host_str(),
            Some("127.0.0.1")
        );
        let session = runtime
            .begin_session(SessionOptions {
                model: None,
                effort: None,
                agent: Some("build".to_owned()),
                working_directory: temp.path().to_owned(),
                environment: BTreeMap::new(),
            })
            .await
            .expect("generated session create");
        runtime
            .resume_session(&session, temp.path())
            .await
            .expect("generated session get");
        let fork = runtime
            .fork_session(&session)
            .await
            .expect("generated session fork");
        runtime
            .resume_session(&fork, temp.path())
            .await
            .expect("generated fork get");
        assert!(matches!(
            runtime.compact_session(&session).await,
            Err(RuntimeError::Rejected(_))
        ));
        runtime
            .abort_session(&session)
            .await
            .expect("generated interrupt");
        runtime.stop().await.expect("sidecar stop");
        assert!(runtime.endpoint().is_none());
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // One readable scenario documents the full compatibility path.
    async fn isolated_sidecar_streams_a_synthetic_provider_tool_turn() {
        let executable = bundled_opencode();
        assert!(
            executable.is_file(),
            "missing bundled sidecar; run `bun run sidecar:fetch`"
        );
        let temp = tempfile::tempdir().expect("temp profile");
        let fixture_metadata = temp.path().join("provider-requests.json");
        let (mut provider, provider_port) =
            spawn_openai_compatible_fixture(&fixture_metadata).await;
        let fixture_provider = json!({
            "npm": "@ai-sdk/openai-compatible",
            "name": "Synthetic fixture provider",
            "options": {
                "baseURL": format!("http://127.0.0.1:{provider_port}/v1"),
                "apiKey": "synthetic-fixture-token"
            },
            "models": {
                "deterministic": {
                    "name": "Deterministic fixture",
                    "tool_call": true,
                    "limit": {"context": 4096, "output": 1024}
                }
            }
        });
        let untrusted_project_config = json!({
            "$schema": "https://opencode.ai/config.json",
            "enabled_providers": ["poison"],
            "model": "poison/project-config-must-not-load",
            "provider": {
                "poison": {
                    "npm": "@ai-sdk/openai-compatible",
                    "name": "Project config canary",
                    "models": {
                        "project-config-must-not-load": {
                            "name": "Project config must not load"
                        }
                    }
                }
            }
        });
        std::fs::write(
            temp.path().join("opencode.json"),
            serde_json::to_vec_pretty(&untrusted_project_config).expect("project config JSON"),
        )
        .expect("project config");

        let mut config = isolated_sidecar_config(executable, temp.path());
        config.providers.insert("fixture".into(), fixture_provider);
        config.default_model = Some("fixture/deterministic".into());
        config.small_model = Some("fixture/deterministic".into());
        let mut runtime = OpenCodeSidecar::new(config);
        runtime.start().await.expect("isolated sidecar start");
        let models = runtime
            .discover_models(Some(temp.path()))
            .await
            .expect("fixture model discovery");
        let model_ids = models
            .iter()
            .map(|model| format!("{}/{}", model.provider_id, model.model_id))
            .collect::<Vec<_>>();
        assert!(
            model_ids
                .iter()
                .any(|model| model == "fixture/deterministic"),
            "isolated fixture model is missing from {model_ids:?}"
        );
        assert!(
            model_ids.iter().all(|model| !model.starts_with("poison/")),
            "project-local OpenCode config crossed the runtime boundary: {model_ids:?}"
        );
        let fixture_model = models
            .iter()
            .find(|model| model.provider_id == "fixture" && model.model_id == "deterministic")
            .expect("fixture model");
        assert!(
            fixture_model.tool_calls,
            "fixture model must advertise tools"
        );
        let session = runtime
            .begin_session(SessionOptions {
                model: Some("fixture/deterministic".to_owned()),
                effort: None,
                agent: Some("build".to_owned()),
                working_directory: temp.path().to_owned(),
                environment: BTreeMap::new(),
            })
            .await
            .expect("fixture session");
        let mut events = runtime
            .submit(
                &session,
                "Call the Personal Agent native gateway status tool.",
                None,
            )
            .await
            .expect("fixture prompt");
        let mut event_types = Vec::new();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(60), events.recv())
                .await
                .expect("fixture turn timeout")
                .expect("fixture event stream closed before terminal");
            let terminal = matches!(
                event.r#type.as_str(),
                "response.completed" | "response.failed"
            );
            event_types.push(event.r#type);
            if terminal {
                break;
            }
        }
        let provider_requests: Value = serde_json::from_slice(
            &std::fs::read(&fixture_metadata).expect("synthetic provider request metadata"),
        )
        .expect("synthetic provider request metadata JSON");
        assert!(
            event_types.iter().any(|event| event == "tool.started"),
            "fixture turn did not start a tool: events={event_types:?}, requests={provider_requests}"
        );
        assert!(
            event_types.iter().any(|event| event == "tool.completed"),
            "fixture turn did not complete a tool: events={event_types:?}, requests={provider_requests}"
        );
        assert!(
            event_types.iter().any(|event| event == "response.delta"),
            "fixture turn did not stream text: {event_types:?}"
        );
        assert_eq!(
            event_types.last().map(String::as_str),
            Some("response.completed")
        );
        let advertised_tools = provider_requests
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|request| {
                request
                    .get("toolNames")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter_map(Value::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            advertised_tools.contains("personal_agent_gateway_status"),
            "OpenCode did not advertise the native gateway tool: {provider_requests}"
        );
        for forbidden in [
            "apply_patch",
            "bash",
            "edit",
            "execute",
            "glob",
            "grep",
            "patch",
            "read",
            "task",
            "webfetch",
            "websearch",
            "write",
        ] {
            assert!(
                !advertised_tools.contains(forbidden),
                "safety plugin exposed forbidden built-in {forbidden}: {advertised_tools:?}"
            );
        }
        assert_eq!(runtime.tool_audit_count().await, 1);
        runtime.stop().await.expect("sidecar stop");
        provider.kill().await.expect("fixture provider stop");
        let _ = provider.wait().await;
    }

    #[tokio::test]
    async fn sidecar_rejects_unpinned_configuration_before_spawn() {
        let temp = tempfile::tempdir().expect("temp profile");
        let mut config = OpenCodeConfig::pinned(
            PathBuf::from("missing-opencode"),
            safety_plugin(),
            temp.path().join("opencode-profile"),
        );
        config.version = "9.9.9".to_owned();
        let mut runtime = OpenCodeSidecar::new(config);
        assert!(matches!(
            runtime.start().await,
            Err(RuntimeError::IncompatibleVersion { .. })
        ));
        assert!(runtime.endpoint().is_none());
    }

    #[tokio::test]
    async fn fake_runtime_streams_a_deterministic_tool_turn_to_terminal() {
        let scripted = [
            ("response.delta", json!({"text":"checking"})),
            (
                "tool.started",
                json!({"call_id":"call_fixture","tool":"fixture.read"}),
            ),
            (
                "tool.completed",
                json!({"call_id":"call_fixture","tool":"fixture.read","ok":true}),
            ),
            ("response.completed", json!({"terminal":true})),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (event_type, payload))| {
            EventEnvelope::new(
                u64::try_from(index + 1).expect("small fixture sequence"),
                "fixture",
                "default",
                event_type,
                &payload,
            )
            .expect("fixture event")
        })
        .collect::<Vec<_>>();
        let mut runtime = FakeRuntime::new(scripted);
        runtime.start().await.expect("start");
        let session = runtime
            .begin_session(SessionOptions {
                model: None,
                effort: None,
                agent: None,
                working_directory: PathBuf::from("/tmp"),
                environment: BTreeMap::new(),
            })
            .await
            .expect("session");
        let mut stream = runtime
            .submit(&session, "hello", None)
            .await
            .expect("submit");
        let mut event_types = Vec::new();
        while let Some(event) = stream.recv().await {
            event_types.push(event.r#type);
        }
        assert_eq!(
            event_types,
            [
                "response.delta",
                "tool.started",
                "tool.completed",
                "response.completed"
            ]
        );
    }

    #[test]
    fn sse_frames_survive_chunk_boundaries_and_redact_sensitive_runtime_fields() {
        let upstream = json!({
            "id": "evt_test",
            "type": "session.next.tool.called",
            "properties": {
                "sessionID": "ses_test",
                "assistantMessageID": "msg_test",
                "callID": "call_test",
                "tool": "personal_agent.execute",
                "input": {"secret": "must-not-survive"},
                "provider": {"executed": false}
            }
        });
        let outer = json!({"id":"1","event":"message","data":upstream.to_string()});
        let wire = format!("event: message\ndata: {outer}\n\n");
        let split = wire.len() / 2;
        let mut buffer = wire.as_bytes()[..split].to_vec();
        assert!(take_sse_frame(&mut buffer).is_none());
        buffer.extend_from_slice(&wire.as_bytes()[split..]);
        let frame = take_sse_frame(&mut buffer).expect("complete frame");
        let parsed = parse_sse_frame(&frame).expect("parse").expect("event");
        let (event, terminal) =
            normalize_upstream_event(7, "ses_fallback", &parsed, None).expect("normalize");
        assert!(!terminal);
        assert_eq!(event.r#type, "tool.started");
        assert_eq!(event.session_id.as_deref(), Some("ses_test"));
        let payload = event.payload().expect("payload");
        assert!(payload.get("input").is_none());
        assert!(
            !serde_json::to_string(&payload)
                .expect("payload JSON")
                .contains("must-not-survive")
        );
    }

    #[test]
    fn reasoning_delta_becomes_availability_without_reasoning_text() {
        let upstream = json!({
            "type": "session.next.reasoning.delta",
            "properties": {
                "sessionID": "ses_test",
                "assistantMessageID": "msg_test",
                "reasoningID": "reasoning_test",
                "delta": "private chain of thought"
            }
        });
        let (event, _) =
            normalize_upstream_event(1, "ses_test", &upstream, None).expect("normalize");
        assert_eq!(event.r#type, "reasoning.available");
        assert!(
            !serde_json::to_string(&event.payload().expect("payload"))
                .expect("payload JSON")
                .contains("private chain of thought")
        );
    }

    #[test]
    fn session_status_idle_is_terminal_and_retry_remains_live() {
        let idle = json!({
            "type": "session.status",
            "properties": {"sessionID": "ses_test", "status": {"type": "idle"}}
        });
        let (event, terminal) =
            normalize_upstream_event(1, "ses_test", &idle, None).expect("idle status");
        assert_eq!(event.r#type, "response.completed");
        assert!(terminal);

        let retry = json!({
            "type": "session.status",
            "properties": {"sessionID": "ses_test", "status": {"type": "retry", "attempt": 2, "message": "rate limited"}}
        });
        let (event, terminal) =
            normalize_upstream_event(2, "ses_test", &retry, None).expect("retry status");
        assert_eq!(event.r#type, "response.retrying");
        assert!(!terminal);
    }
}
