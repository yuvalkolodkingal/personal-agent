//! Persistent protocol adapter for local neural speech models.

use crate::{AdmissionPlan, AudioError, LocalModel, ModelArbiter};
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

    /// Ask the worker to unload one GPU model. Unload is idempotent: `false`
    /// means the model was already absent, not that the command failed.
    ///
    /// # Errors
    ///
    /// Returns an [`AudioError`] for CPU-only models or worker protocol errors.
    pub async fn unload_model(
        &mut self,
        model: LocalModel,
        timeout: Duration,
    ) -> Result<bool, AudioError> {
        if !model.uses_gpu() {
            return Err(AudioError::Processing(format!(
                "{} is a CPU fallback and is not governed by the VRAM arbiter",
                model.worker_id()
            )));
        }
        let result = self
            .request("unload", json!({ "model": model.worker_id() }), timeout)
            .await?;
        if result.get("model").and_then(Value::as_str) != Some(model.worker_id())
            || result.get("loaded").and_then(Value::as_bool) != Some(false)
        {
            return Err(AudioError::Processing(format!(
                "worker returned an invalid unload acknowledgement for {}",
                model.worker_id()
            )));
        }
        Ok(result
            .get("unloaded")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    /// Reserve room for a GPU model, unloading each lowest-priority idle model
    /// selected by the arbiter before admitting the requested load.
    ///
    /// CPU fallbacks are admitted without sending worker unload commands.
    ///
    /// # Errors
    ///
    /// Returns an [`AudioError`] if admission is impossible, an unload fails,
    /// or the registry changes before admission can be committed.
    pub async fn prepare_model_load(
        &mut self,
        arbiter: &mut ModelArbiter,
        model: LocalModel,
        timeout: Duration,
    ) -> Result<AdmissionPlan, AudioError> {
        let plan = arbiter
            .plan_admission(model)
            .map_err(|error| AudioError::Unavailable(error.to_string()))?;
        for candidate in plan.models_to_unload() {
            self.unload_model(*candidate, timeout).await?;
            // Reconcile after each successful idempotent acknowledgement so a
            // later protocol failure cannot leave the registry overstating a
            // model that the worker already released.
            arbiter.mark_unloaded(*candidate);
        }
        let final_plan = arbiter
            .plan_admission(model)
            .map_err(|error| AudioError::Unavailable(error.to_string()))?;
        if !final_plan.models_to_unload().is_empty() {
            return Err(AudioError::Processing(
                "model registry changed while worker unloads were in flight".into(),
            ));
        }
        arbiter
            .commit_admission(&final_plan)
            .map_err(|error| AudioError::Unavailable(error.to_string()))?;
        Ok(plan)
    }

    /// Stop the worker immediately. Model state will load again on demand.
    pub fn terminate(&mut self) {
        let _ = self.child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn find_python() -> PathBuf {
        for variable in ["PYTHON", "PYTHON3"] {
            if let Some(path) = std::env::var_os(variable).map(PathBuf::from)
                && path.is_file()
            {
                return path;
            }
        }
        let executable = if cfg!(windows) {
            "python.exe"
        } else {
            "python3"
        };
        if let Some(paths) = std::env::var_os("PATH") {
            for directory in std::env::split_paths(&paths) {
                let candidate = directory.join(executable);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
        panic!("python3 is required for the real voice-worker protocol test");
    }

    fn unique_test_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "personal-agent-perf9-{}-{nonce}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn real_worker_protocol_unloads_before_admitting_a_model() {
        let root = unique_test_root();
        std::fs::create_dir_all(&root).expect("test voice root");
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/voice-runtime.py")
            .canonicalize()
            .expect("voice worker script");
        let mut runtime = NeuralVoiceRuntime::start(&find_python(), &script, &root)
            .await
            .expect("start real voice worker");

        let mut arbiter = ModelArbiter::with_ceiling_mib(2_900);
        for model in [
            LocalModel::VisionGrounding,
            LocalModel::FasterWhisperLargeV3TurboInt8,
        ] {
            let plan = arbiter.plan_admission(model).expect("seed plan");
            arbiter.commit_admission(&plan).expect("seed admission");
        }
        let plan = runtime
            .prepare_model_load(&mut arbiter, LocalModel::Qwen3Tts, Duration::from_secs(5))
            .await
            .expect("prepare Qwen load");
        assert_eq!(plan.models_to_unload(), &[LocalModel::VisionGrounding]);
        assert!(!arbiter.is_loaded(LocalModel::VisionGrounding));
        assert!(arbiter.is_loaded(LocalModel::FasterWhisperLargeV3TurboInt8));
        assert!(arbiter.is_loaded(LocalModel::Qwen3Tts));

        let status = runtime
            .request("status", Value::Null, Duration::from_secs(5))
            .await
            .expect("worker status");
        assert_eq!(
            status
                .get("vision_grounding_loaded")
                .and_then(Value::as_bool),
            Some(false)
        );
        runtime.terminate();
        drop(runtime);
        std::fs::remove_dir_all(root).expect("remove test voice root");
    }
}
