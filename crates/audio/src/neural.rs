//! Persistent protocol adapter for local neural speech models.

use crate::AudioError;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// One private Python worker with lazily loaded Moonshine and Qwen models.
pub struct NeuralVoiceRuntime {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl NeuralVoiceRuntime {
    /// Operating-system process identifier used for immediate barge-in cancellation.
    #[must_use]
    pub fn process_id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Start the worker and validate its protocol greeting.
    ///
    /// # Errors
    ///
    /// Returns an [`AudioError`] when the process cannot start or its greeting is invalid.
    pub async fn start(python: &Path, script: &Path, root: &Path) -> Result<Self, AudioError> {
        if !python.is_file() || !script.is_file() {
            return Err(AudioError::Unavailable(
                "the neural voice runtime is not installed".into(),
            ));
        }
        let mut child = Command::new(python)
            .arg("-u")
            .arg(script)
            .arg("--root")
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| AudioError::Processing(error.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AudioError::Processing("voice worker stdin is unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AudioError::Processing("voice worker stdout is unavailable".into()))?;
        let mut runtime = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        };
        let mut greeting = String::new();
        tokio::time::timeout(
            Duration::from_secs(15),
            runtime.stdout.read_line(&mut greeting),
        )
        .await
        .map_err(|_| AudioError::Processing("neural voice runtime startup timed out".into()))?
        .map_err(|error| AudioError::Processing(error.to_string()))?;
        let value: Value = serde_json::from_str(&greeting)
            .map_err(|error| AudioError::Processing(format!("invalid voice greeting: {error}")))?;
        if value.get("ready").and_then(Value::as_bool) != Some(true) {
            return Err(AudioError::Processing(
                "neural voice runtime rejected startup".into(),
            ));
        }
        Ok(runtime)
    }

    /// Send one bounded request. The caller serializes access to this worker.
    ///
    /// # Errors
    ///
    /// Returns an [`AudioError`] for worker I/O, protocol, process, or timeout failures.
    pub async fn request(
        &mut self,
        command: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, AudioError> {
        if self
            .child
            .try_wait()
            .map_err(|error| AudioError::Processing(error.to_string()))?
            .is_some()
        {
            return Err(AudioError::Processing(
                "neural voice runtime exited unexpectedly".into(),
            ));
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let mut request = payload.as_object().cloned().unwrap_or_default();
        request.insert("id".into(), json!(id));
        request.insert("command".into(), json!(command));
        let mut encoded = serde_json::to_vec(&request)
            .map_err(|error| AudioError::Processing(error.to_string()))?;
        encoded.push(b'\n');
        self.stdin
            .write_all(&encoded)
            .await
            .map_err(|error| AudioError::Processing(error.to_string()))?;
        self.stdin
            .flush()
            .await
            .map_err(|error| AudioError::Processing(error.to_string()))?;
        let mut response = String::new();
        tokio::time::timeout(timeout, self.stdout.read_line(&mut response))
            .await
            .map_err(|_| AudioError::Processing(format!("{command} timed out")))?
            .map_err(|error| AudioError::Processing(error.to_string()))?;
        let value: Value = serde_json::from_str(&response)
            .map_err(|error| AudioError::Processing(format!("invalid voice response: {error}")))?;
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            return Err(AudioError::Processing(
                "neural voice response was out of sequence".into(),
            ));
        }
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(AudioError::Processing(
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("neural voice request failed")
                    .to_owned(),
            ));
        }
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Stop the worker immediately. Model state will load again on demand.
    pub fn terminate(&mut self) {
        let _ = self.child.start_kill();
    }
}
