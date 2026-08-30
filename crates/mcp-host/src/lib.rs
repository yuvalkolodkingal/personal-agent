//! Native MCP host built on the official `rmcp` SDK.
//!
//! This crate replaces the sidecar-owned MCP runtime. It owns process and
//! network handles for stdio, Streamable HTTP, and legacy HTTP+SSE servers,
//! completes MCP initialization, and reports a manager-shaped
//! [`RuntimeHandshake`] with tool annotations preserved and a measured
//! round-trip latency.
//!
//! It deliberately does **not** execute tool calls on behalf of a model. The
//! manager still routes invocations through `prepare_tool_call`, and the
//! application's `ToolGateway` remains the only path to an effect;
//! [`McpHost::call_tool`] is the transport step the gateway invokes last.

mod catalog;
mod config;
mod error;
mod http_client;
mod logs;
mod secrets;
mod session;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use personal_agent_mcp_manager::{
    AdapterError, RuntimeAdapter, RuntimeHandshake, ServerDefinition,
};
use rmcp::model::{CallToolRequestParams, ClientRequest, PingRequest};
use serde_json::Value;
use tokio::runtime::Runtime;
use uuid::Uuid;

pub use catalog::{capability_catalog, tool_annotations, tool_descriptor};
pub use config::{BackoffPolicy, DEFAULT_ENVIRONMENT_ALLOWLIST, HostConfig};
pub use http_client::HttpClient;
pub use logs::{LogLevel, LogLine, LogRing};
pub use secrets::{InMemorySecrets, SecretResolver, UnconfiguredSecrets, resolve_binding};

use error::{adapter_error, not_connected, service_error, transport_error};
use session::{Session, SharedLog, record};

/// Failure while constructing the host itself.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// The dedicated MCP runtime could not be started.
    #[error("the MCP host runtime could not start: {0}")]
    Runtime(#[from] std::io::Error),
}

#[derive(Debug)]
struct Registry {
    sessions: Mutex<HashMap<Uuid, Session>>,
    logs: Mutex<HashMap<Uuid, SharedLog>>,
}

impl Registry {
    fn log(&self, server_id: Uuid) -> SharedLog {
        let Ok(mut logs) = self.logs.lock() else {
            tracing::error!("MCP host log registry is poisoned");
            return SharedLog::default();
        };
        Arc::clone(logs.entry(server_id).or_default())
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("latency_ms", &self.handshake.latency_ms)
            .finish_non_exhaustive()
    }
}

/// Native MCP host owning every live server session.
///
/// All protocol work runs on a dedicated Tokio runtime so that sessions outlive
/// any single caller and so the synchronous [`RuntimeAdapter`] bridge never
/// blocks inside another runtime's reactor.
pub struct McpHost {
    runtime: Option<Runtime>,
    config: Arc<HostConfig>,
    secrets: Arc<dyn SecretResolver>,
    registry: Arc<Registry>,
}

impl std::fmt::Debug for McpHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHost")
            .field("config", &self.config)
            .field("secrets", &self.secrets)
            .finish_non_exhaustive()
    }
}

impl Drop for McpHost {
    fn drop(&mut self) {
        // Sessions hold child processes with `kill_on_drop`; clearing them here
        // guarantees the kill happens while the runtime still exists.
        if let Ok(mut sessions) = self.registry.sessions.lock() {
            sessions.clear();
        }
        if let Some(runtime) = self.runtime.take() {
            // `shutdown_background` never blocks, so dropping the host from an
            // async context is safe.
            runtime.shutdown_background();
        }
    }
}

impl McpHost {
    /// Starts a host that refuses keychain-bound servers until a resolver is
    /// supplied.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Runtime`] when the dedicated runtime cannot start.
    pub fn new(config: HostConfig) -> Result<Self, HostError> {
        Self::with_secrets(config, Arc::new(UnconfiguredSecrets))
    }

    /// Starts a host that resolves keychain bindings through `secrets`.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Runtime`] when the dedicated runtime cannot start.
    pub fn with_secrets(
        config: HostConfig,
        secrets: Arc<dyn SecretResolver>,
    ) -> Result<Self, HostError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("mcp-host")
            .enable_all()
            .build()?;
        Ok(Self {
            runtime: Some(runtime),
            config: Arc::new(config),
            secrets,
            registry: Arc::new(Registry {
                sessions: Mutex::new(HashMap::new()),
                logs: Mutex::new(HashMap::new()),
            }),
        })
    }

    fn runtime(&self) -> &Runtime {
        self.runtime
            .as_ref()
            .expect("the MCP host runtime is dropped only in Drop")
    }

    /// Retained lifecycle lines for one server, oldest first.
    #[must_use]
    pub fn logs(&self, server_id: Uuid) -> Vec<LogLine> {
        let log = self.registry.log(server_id);
        log.lock().map(|ring| ring.lines()).unwrap_or_default()
    }

    /// Whether a live session exists for `server_id`.
    #[must_use]
    pub fn is_connected(&self, server_id: Uuid) -> bool {
        self.registry
            .sessions
            .lock()
            .is_ok_and(|sessions| sessions.contains_key(&server_id))
    }

    /// Synchronous [`RuntimeAdapter`] view over this host.
    #[must_use]
    pub fn adapter(&self) -> HostAdapter<'_> {
        HostAdapter { host: self }
    }

    fn task(&self, definition: &ServerDefinition) -> Task {
        Task {
            config: Arc::clone(&self.config),
            secrets: Arc::clone(&self.secrets),
            registry: Arc::clone(&self.registry),
            definition: definition.clone(),
        }
    }

    /// Connects (or reconnects) a server and returns its handshake.
    ///
    /// # Errors
    ///
    /// Returns a sanitized [`AdapterError`] when the transport, initialization,
    /// or catalog listing fails after the configured retries.
    pub async fn connect(
        &self,
        definition: &ServerDefinition,
    ) -> Result<RuntimeHandshake, AdapterError> {
        let task = self.task(definition);
        self.spawn(async move { task.connect().await }).await
    }

    /// Measures a protocol round trip against a live session.
    ///
    /// # Errors
    ///
    /// Returns a sanitized [`AdapterError`] when the server is not connected or
    /// does not answer the ping.
    pub async fn health(&self, definition: &ServerDefinition) -> Result<u64, AdapterError> {
        let task = self.task(definition);
        self.spawn(async move { task.health().await }).await
    }

    /// Calls a tool on a live session.
    ///
    /// # Errors
    ///
    /// Returns a sanitized [`AdapterError`] when the server is not connected,
    /// the call fails, or the server reports a tool error.
    pub async fn call_tool(
        &self,
        definition: &ServerDefinition,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, AdapterError> {
        let task = self.task(definition);
        let tool = tool.to_owned();
        self.spawn(async move { task.call_tool(&tool, arguments).await })
            .await
    }

    /// Terminates a session, stopping the child process or HTTP session.
    ///
    /// # Errors
    ///
    /// Never fails; the signature matches [`RuntimeAdapter`].
    pub async fn disconnect(&self, definition: &ServerDefinition) -> Result<(), AdapterError> {
        let task = self.task(definition);
        self.spawn(async move { task.disconnect() }).await;
        Ok(())
    }

    async fn spawn<T, F>(&self, future: F) -> T
    where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.runtime().spawn(async move {
            let _ = sender.send(future.await);
        });
        receiver
            .await
            .expect("the MCP host runtime outlives its tasks")
    }

    fn block<T, F>(&self, future: F) -> T
    where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        self.runtime().spawn(async move {
            let _ = sender.send(future.await);
        });
        receiver
            .recv()
            .expect("the MCP host runtime outlives its tasks")
    }
}

/// Everything one host operation needs, owned so it can cross runtimes.
struct Task {
    config: Arc<HostConfig>,
    secrets: Arc<dyn SecretResolver>,
    registry: Arc<Registry>,
    definition: ServerDefinition,
}

impl Task {
    fn log(&self) -> SharedLog {
        self.registry.log(self.definition.id)
    }

    async fn connect(&self) -> Result<RuntimeHandshake, AdapterError> {
        let log = self.log();
        self.disconnect_inner(&log, "reconnecting");
        let mut failure = None;
        for attempt in 1..=self.config.backoff.attempts.max(1) {
            let delay = self.config.backoff.delay(attempt);
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            match self.attempt(&log).await {
                Ok(handshake) => return Ok(handshake),
                Err(error) if error.authentication_required => return Err(error),
                Err(error) => {
                    record(
                        &log,
                        LogLevel::Warn,
                        format!("connect attempt {attempt} failed: {}", error.code),
                    );
                    failure = Some(error);
                }
            }
        }
        Err(failure
            .unwrap_or_else(|| adapter_error("connect_failed", "The MCP server did not connect.")))
    }

    async fn attempt(&self, log: &SharedLog) -> Result<RuntimeHandshake, AdapterError> {
        let started = Instant::now();
        let connect = session::serve(&self.config, &self.definition, self.secrets.as_ref(), log);
        let client = tokio::time::timeout(self.config.connect_timeout, connect)
            .await
            .map_err(|_| adapter_error("timeout", "The MCP server did not answer in time."))??;
        let server_protocols = session::server_protocols(&client)?;
        let catalog = tokio::time::timeout(
            self.config.request_timeout,
            session::read_catalog(&client, &self.definition.namespace),
        )
        .await
        .map_err(|_| adapter_error("timeout", "The MCP server did not answer in time."))??;
        let handshake = RuntimeHandshake {
            server_protocols,
            catalog,
            latency_ms: elapsed_ms(started),
        };
        record(
            log,
            LogLevel::Info,
            format!(
                "connected over {} in {} ms",
                self.definition.transport.label(),
                handshake.latency_ms
            ),
        );
        self.store(Session {
            client,
            handshake: handshake.clone(),
        });
        Ok(handshake)
    }

    fn store(&self, session: Session) {
        let Ok(mut sessions) = self.registry.sessions.lock() else {
            tracing::error!("MCP host session registry is poisoned");
            return;
        };
        sessions.insert(self.definition.id, session);
    }

    async fn health(&self) -> Result<u64, AdapterError> {
        let log = self.log();
        let started = Instant::now();
        let request = self.with_session(|session| {
            let peer = session.client.peer().clone();
            async move {
                peer.send_request(ClientRequest::PingRequest(PingRequest::default()))
                    .await
            }
        })?;
        tokio::time::timeout(self.config.request_timeout, request)
            .await
            .map_err(|_| transport_error(&log, "ping timed out"))?
            .map_err(service_error)?;
        let latency = elapsed_ms(started);
        record(
            &log,
            LogLevel::Info,
            format!("ping answered in {latency} ms"),
        );
        Ok(latency)
    }

    async fn call_tool(&self, tool: &str, arguments: Value) -> Result<Value, AdapterError> {
        let call_arguments = match arguments {
            Value::Null => None,
            Value::Object(map) => Some(map),
            _ => {
                return Err(adapter_error(
                    "invalid_arguments",
                    "MCP tool arguments must be a JSON object.",
                ));
            }
        };
        let mut parameters = CallToolRequestParams::new(tool.to_owned());
        parameters.arguments = call_arguments;
        let request = self.with_session(|session| {
            let peer = session.client.peer().clone();
            async move { peer.call_tool(parameters).await }
        })?;
        let result = tokio::time::timeout(self.config.request_timeout, request)
            .await
            .map_err(|_| adapter_error("timeout", "The MCP tool did not answer in time."))?
            .map_err(service_error)?;
        if result.is_error.unwrap_or(false) {
            return Err(adapter_error(
                "tool_error",
                "The MCP server reported a tool failure.",
            ));
        }
        serde_json::to_value(result)
            .map_err(|_| adapter_error("invalid_result", "The MCP tool result was not encodable."))
    }

    /// Borrows the live session just long enough to build a request future, so
    /// the registry lock is never held across an await point.
    fn with_session<T>(&self, build: impl FnOnce(&Session) -> T) -> Result<T, AdapterError> {
        let sessions = self
            .registry
            .sessions
            .lock()
            .map_err(|_| adapter_error("poisoned", "The MCP session registry is unusable."))?;
        sessions
            .get(&self.definition.id)
            .map(build)
            .ok_or_else(not_connected)
    }

    fn disconnect(&self) {
        let log = self.log();
        self.disconnect_inner(&log, "disconnected");
    }

    fn disconnect_inner(&self, log: &SharedLog, reason: &str) {
        let Ok(mut sessions) = self.registry.sessions.lock() else {
            tracing::error!("MCP host session registry is poisoned");
            return;
        };
        if sessions.remove(&self.definition.id).is_some() {
            record(log, LogLevel::Info, reason.to_owned());
        }
    }
}

/// Converts an elapsed duration into whole milliseconds, rounding up so a real
/// sub-millisecond round trip is still reported as measured work.
fn elapsed_ms(started: Instant) -> u64 {
    let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    micros.div_ceil(1_000).max(1)
}

/// Synchronous [`RuntimeAdapter`] bridge over a [`McpHost`].
///
/// Every call blocks the calling thread while the host's own runtime drives the
/// protocol work. Async callers should use [`McpHost`]'s futures directly and
/// feed the outcome to the manager through [`ResolvedAdapter`].
pub struct HostAdapter<'a> {
    host: &'a McpHost,
}

impl RuntimeAdapter for HostAdapter<'_> {
    fn connect(&mut self, definition: &ServerDefinition) -> Result<RuntimeHandshake, AdapterError> {
        let task = self.host.task(definition);
        self.host.block(async move { task.connect().await })
    }

    fn health(&mut self, definition: &ServerDefinition) -> Result<u64, AdapterError> {
        let task = self.host.task(definition);
        self.host.block(async move { task.health().await })
    }

    fn disconnect(&mut self, definition: &ServerDefinition) -> Result<(), AdapterError> {
        let task = self.host.task(definition);
        self.host.block(async move { task.disconnect() });
        Ok(())
    }
}

/// Replays an already awaited host outcome into the manager state machine.
///
/// The manager's adapter trait is synchronous while the host is async, so async
/// callers resolve the outcome first and hand it over with this adapter instead
/// of blocking a caller's reactor thread.
#[derive(Debug)]
pub struct ResolvedAdapter {
    connect: Option<Result<RuntimeHandshake, AdapterError>>,
    health: Result<u64, AdapterError>,
}

impl ResolvedAdapter {
    /// Replays a connect outcome.
    #[must_use]
    pub fn connected(outcome: Result<RuntimeHandshake, AdapterError>) -> Self {
        let health = outcome
            .as_ref()
            .map(|handshake| handshake.latency_ms)
            .map_err(Clone::clone);
        Self {
            connect: Some(outcome),
            health,
        }
    }

    /// Replays a health outcome.
    #[must_use]
    pub fn measured(outcome: Result<u64, AdapterError>) -> Self {
        Self {
            connect: None,
            health: outcome,
        }
    }

    /// Replays a completed disconnect.
    #[must_use]
    pub fn disconnected() -> Self {
        Self {
            connect: None,
            health: Err(not_connected()),
        }
    }
}

impl RuntimeAdapter for ResolvedAdapter {
    fn connect(
        &mut self,
        _definition: &ServerDefinition,
    ) -> Result<RuntimeHandshake, AdapterError> {
        self.connect.take().unwrap_or_else(|| {
            Err(adapter_error(
                "not_initialized",
                "The MCP handshake was unavailable.",
            ))
        })
    }

    fn health(&mut self, _definition: &ServerDefinition) -> Result<u64, AdapterError> {
        self.health.clone()
    }

    fn disconnect(&mut self, _definition: &ServerDefinition) -> Result<(), AdapterError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
