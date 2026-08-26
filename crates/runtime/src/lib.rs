//! Provider-neutral agent runtime boundary and pinned `OpenCode` sidecar adapter.

use async_trait::async_trait;
use personal_agent_contracts::proto::EventEnvelope;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, net::TcpListener, path::PathBuf, process::Stdio};
use thiserror::Error;
use tokio::{
    process::{Child, Command},
    sync::mpsc,
};
use url::Url;
use uuid::Uuid;

/// Stable `OpenCode` sidecar version verified on 2026-08-26.
pub const OPENCODE_VERSION: &str = "1.18.23";

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
    #[error("runtime rejected the request: {0}")]
    Rejected(String),
    #[error("runtime event stream ended unexpectedly")]
    StreamClosed,
}

/// Replaceable boundary consumed by the agent supervisor.
#[async_trait]
pub trait AgentRuntime: Send {
    async fn start(&mut self) -> Result<RuntimeHealth, RuntimeError>;
    async fn health(&mut self) -> Result<RuntimeHealth, RuntimeError>;
    async fn stop(&mut self) -> Result<(), RuntimeError>;
    async fn discover_models(&mut self) -> Result<Vec<ModelCapability>, RuntimeError>;
    async fn begin_session(&mut self, options: SessionOptions) -> Result<String, RuntimeError>;
    async fn resume_session(&mut self, session_id: &str) -> Result<(), RuntimeError>;
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
    pub version: String,
    pub username: String,
    pub password: SecretString,
}

impl OpenCodeConfig {
    /// Create an ephemeral authentication secret for one application run.
    #[must_use]
    pub fn pinned(executable: PathBuf) -> Self {
        Self {
            executable,
            version: OPENCODE_VERSION.into(),
            username: "personal-agent".into(),
            password: SecretString::from(Uuid::new_v4().to_string()),
        }
    }
}

/// Owned `OpenCode` process. The UI never receives this endpoint or credential.
pub struct OpenCodeSidecar {
    config: OpenCodeConfig,
    child: Option<Child>,
    endpoint: Option<Url>,
}

impl OpenCodeSidecar {
    #[must_use]
    pub fn new(config: OpenCodeConfig) -> Self {
        Self {
            config,
            child: None,
            endpoint: None,
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
}

#[async_trait]
impl AgentRuntime for OpenCodeSidecar {
    async fn start(&mut self) -> Result<RuntimeHealth, RuntimeError> {
        if self.child.is_some() {
            return self.health().await;
        }
        let port = Self::reserve_loopback_port()?;
        let mut command = Command::new(&self.config.executable);
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
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        self.child = Some(command.spawn()?);
        self.endpoint =
            Some(Url::parse(&format!("http://127.0.0.1:{port}/")).expect("loopback URL"));
        Ok(RuntimeHealth {
            healthy: true,
            version: self.config.version.clone(),
            detail: "sidecar process started; API compatibility check pending".into(),
        })
    }

    async fn health(&mut self) -> Result<RuntimeHealth, RuntimeError> {
        let child = self.child.as_mut().ok_or(RuntimeError::NotRunning)?;
        match child.try_wait()? {
            None => Ok(RuntimeHealth {
                healthy: true,
                version: self.config.version.clone(),
                detail: "process running".into(),
            }),
            Some(status) => Ok(RuntimeHealth {
                healthy: false,
                version: self.config.version.clone(),
                detail: format!("process exited with {status}"),
            }),
        }
    }

    async fn stop(&mut self) -> Result<(), RuntimeError> {
        if let Some(mut child) = self.child.take() {
            child.kill().await?;
            let _ = child.wait().await;
        }
        self.endpoint = None;
        Ok(())
    }

    async fn discover_models(&mut self) -> Result<Vec<ModelCapability>, RuntimeError> {
        Err(RuntimeError::Rejected(
            "OpenAPI client generation is the M2 compatibility gate".into(),
        ))
    }
    async fn begin_session(&mut self, _options: SessionOptions) -> Result<String, RuntimeError> {
        Err(RuntimeError::Rejected(
            "OpenAPI client generation is the M2 compatibility gate".into(),
        ))
    }
    async fn resume_session(&mut self, _session_id: &str) -> Result<(), RuntimeError> {
        Err(RuntimeError::Rejected(
            "OpenAPI client generation is the M2 compatibility gate".into(),
        ))
    }
    async fn compact_session(&mut self, _session_id: &str) -> Result<(), RuntimeError> {
        Err(RuntimeError::Rejected(
            "OpenAPI client generation is the M2 compatibility gate".into(),
        ))
    }
    async fn fork_session(&mut self, _session_id: &str) -> Result<String, RuntimeError> {
        Err(RuntimeError::Rejected(
            "OpenAPI client generation is the M2 compatibility gate".into(),
        ))
    }
    async fn abort_session(&mut self, _session_id: &str) -> Result<(), RuntimeError> {
        Err(RuntimeError::Rejected(
            "OpenAPI client generation is the M2 compatibility gate".into(),
        ))
    }
    async fn submit(
        &mut self,
        _session_id: &str,
        _prompt: &str,
        _plan: Option<Value>,
    ) -> Result<mpsc::Receiver<EventEnvelope>, RuntimeError> {
        Err(RuntimeError::Rejected(
            "OpenAPI client generation is the M2 compatibility gate".into(),
        ))
    }
    async fn answer(
        &mut self,
        _session_id: &str,
        _answer: RuntimeAnswer,
    ) -> Result<(), RuntimeError> {
        Err(RuntimeError::Rejected(
            "OpenAPI client generation is the M2 compatibility gate".into(),
        ))
    }
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
    async fn discover_models(&mut self) -> Result<Vec<ModelCapability>, RuntimeError> {
        Ok(vec![ModelCapability {
            provider_id: "fixture".into(),
            model_id: "deterministic".into(),
            context_tokens: Some(4096),
            local: true,
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
    async fn resume_session(&mut self, session_id: &str) -> Result<(), RuntimeError> {
        if self.sessions.iter().any(|id| id == session_id) {
            Ok(())
        } else {
            Err(RuntimeError::Rejected("unknown session".into()))
        }
    }
    async fn compact_session(&mut self, session_id: &str) -> Result<(), RuntimeError> {
        self.resume_session(session_id).await
    }
    async fn fork_session(&mut self, session_id: &str) -> Result<String, RuntimeError> {
        self.resume_session(session_id).await?;
        let id = Uuid::now_v7().to_string();
        self.sessions.push(id.clone());
        Ok(id)
    }
    async fn abort_session(&mut self, session_id: &str) -> Result<(), RuntimeError> {
        self.resume_session(session_id).await
    }
    async fn submit(
        &mut self,
        session_id: &str,
        _prompt: &str,
        _plan: Option<Value>,
    ) -> Result<mpsc::Receiver<EventEnvelope>, RuntimeError> {
        self.resume_session(session_id).await?;
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
        self.resume_session(session_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn fake_runtime_streams_scripted_events() {
        let event = EventEnvelope::new(
            1,
            "fixture",
            "default",
            "response.delta",
            &json!({"text":"ok"}),
        )
        .expect("event");
        let mut runtime = FakeRuntime::new(vec![event.clone()]);
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
        assert_eq!(stream.recv().await.expect("event").event_id, event.event_id);
    }
}
