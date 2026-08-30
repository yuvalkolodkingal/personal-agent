//! Persistent protocol adapter for local neural speech models.

use crate::{AdmissionPlan, AudioError, LocalModel, ModelArbiter};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use tokio::io::AsyncReadExt;
#[cfg(unix)]
use tokio::net::UnixListener;

#[cfg(unix)]
const TTS_STREAM_MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
#[cfg(unix)]
const TTS_STREAM_MAX_FRAMES: usize = 4_096;
#[cfg(unix)]
const TTS_SOCKET_PATH_MAX_BYTES: usize = 100;

#[cfg(unix)]
struct SocketPathGuard(PathBuf);

#[cfg(unix)]
impl Drop for SocketPathGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(unix)]
fn checked_tts_frame_bytes(prefix: [u8; 4]) -> Result<usize, AudioError> {
    let frame_bytes = usize::try_from(u32::from_le_bytes(prefix))
        .map_err(|_| AudioError::Processing("invalid TTS frame length".into()))?;
    if frame_bytes == 0
        || !frame_bytes.is_multiple_of(2)
        || frame_bytes > TTS_STREAM_MAX_FRAME_BYTES
    {
        return Err(AudioError::Processing(format!(
            "invalid TTS PCM frame length: {frame_bytes}"
        )));
    }
    Ok(frame_bytes)
}

#[cfg(unix)]
fn neural_cuda_library_dirs(python: &Path) -> Vec<PathBuf> {
    let Some(venv) = python.parent().and_then(Path::parent) else {
        return Vec::new();
    };
    let nvidia = venv.join("lib/python3.12/site-packages/nvidia");
    [nvidia.join("cublas/lib"), nvidia.join("cudnn/lib")]
        .into_iter()
        .filter(|path| path.is_dir())
        .collect()
}

#[cfg(unix)]
fn configure_neural_cuda_libraries(command: &mut Command, python: &Path) {
    let mut paths = neural_cuda_library_dirs(python);
    if paths.is_empty() {
        return;
    }
    if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        command.env("LD_LIBRARY_PATH", joined);
    }
}

/// One private Python worker with lazily loaded Moonshine and Qwen models.
pub struct NeuralVoiceRuntime {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl NeuralVoiceRuntime {
    fn reserve_request_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    async fn send_request(
        &mut self,
        id: u64,
        command: &str,
        payload: Value,
    ) -> Result<(), AudioError> {
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
            .map_err(|error| AudioError::Processing(error.to_string()))
    }

    async fn read_response(
        &mut self,
        id: u64,
        command: &str,
        timeout: Duration,
    ) -> Result<Value, AudioError> {
        self.read_response_until(id, command, tokio::time::Instant::now() + timeout)
            .await
    }

    async fn read_response_until(
        &mut self,
        id: u64,
        command: &str,
        deadline: tokio::time::Instant,
    ) -> Result<Value, AudioError> {
        let mut response = String::new();
        tokio::time::timeout_at(deadline, self.stdout.read_line(&mut response))
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
        let mut command = Command::new(python);
        command
            .arg("-u")
            .arg(script)
            .arg("--root")
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(unix)]
        configure_neural_cuda_libraries(&mut command, python);
        let mut child = command
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
        let id = self.reserve_request_id();
        self.send_request(id, command, payload).await?;
        self.read_response(id, command, timeout).await
    }

    /// Stream Qwen PCM clauses over a private per-request Unix socket.
    ///
    /// Frames are delivered in wire order as signed 16-bit mono samples. The
    /// generation predicate is consulted while reading both frame headers and
    /// payloads; invalidation closes the socket before another frame is
    /// delivered. The callback is synchronous so a native audio sink can queue
    /// each frame immediately without buffering the full utterance.
    ///
    /// # Errors
    ///
    /// Returns an [`AudioError`] for socket, worker protocol, frame-bound, or
    /// callback failures. Platforms without Unix sockets return an explicit
    /// unavailable reason so the whole-WAV fallback can remain portable.
    #[cfg(unix)]
    #[allow(clippy::too_many_lines)] // Binding, framed reads, cancellation, and the control acknowledgement are one protocol transaction.
    pub async fn tts_stream<F, C>(
        &mut self,
        socket_directory: &Path,
        mut payload: Value,
        generation: u64,
        timeout: Duration,
        mut generation_is_current: C,
        mut on_frame: F,
    ) -> Result<Value, AudioError>
    where
        F: FnMut(&[i16]) -> Result<(), AudioError> + Send,
        C: FnMut() -> bool + Send,
    {
        std::fs::create_dir_all(socket_directory)
            .map_err(|error| AudioError::Processing(error.to_string()))?;
        let private_directory = std::fs::canonicalize(socket_directory)
            .map_err(|error| AudioError::Processing(error.to_string()))?;
        if !private_directory.is_dir() {
            return Err(AudioError::Processing(
                "TTS stream socket parent is not a directory".into(),
            ));
        }
        std::fs::set_permissions(&private_directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| AudioError::Processing(error.to_string()))?;

        let id = self.reserve_request_id();
        let socket_path = private_directory.join(format!(".tts-{}-{id}.sock", std::process::id()));
        if socket_path.as_os_str().as_bytes().len() > TTS_SOCKET_PATH_MAX_BYTES {
            return Err(AudioError::Unavailable(
                "the private voice directory path is too long for a Unix TTS socket".into(),
            ));
        }
        if socket_path.exists() {
            let metadata = std::fs::symlink_metadata(&socket_path)
                .map_err(|error| AudioError::Processing(error.to_string()))?;
            if !metadata.file_type().is_socket() {
                return Err(AudioError::Processing(
                    "refusing to replace a non-socket TTS stream path".into(),
                ));
            }
            std::fs::remove_file(&socket_path)
                .map_err(|error| AudioError::Processing(error.to_string()))?;
        }
        let listener = UnixListener::bind(&socket_path)
            .map_err(|error| AudioError::Processing(error.to_string()))?;
        let _socket_guard = SocketPathGuard(socket_path.clone());
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| AudioError::Processing(error.to_string()))?;

        let request = payload
            .as_object_mut()
            .ok_or_else(|| AudioError::Processing("TTS stream payload must be an object".into()))?;
        request.insert("generation".into(), json!(generation));
        request.insert("socket_path".into(), json!(socket_path.to_string_lossy()));
        let deadline = tokio::time::Instant::now() + timeout;
        tokio::time::timeout_at(deadline, self.send_request(id, "tts_stream", payload))
            .await
            .map_err(|_| AudioError::Processing("tts_stream timed out".into()))??;

        let (mut stream, _) = tokio::time::timeout_at(deadline, listener.accept())
            .await
            .map_err(|_| AudioError::Processing("tts_stream socket accept timed out".into()))?
            .map_err(|error| AudioError::Processing(error.to_string()))?;
        let mut received_frames = 0_usize;
        let mut cancelled = false;

        'frames: loop {
            if !generation_is_current() {
                cancelled = true;
                break;
            }
            let mut prefix = [0_u8; 4];
            let mut prefix_read = 0_usize;
            while prefix_read < prefix.len() {
                if !generation_is_current() {
                    cancelled = true;
                    break 'frames;
                }
                tokio::select! {
                    biased;
                    () = tokio::time::sleep_until(deadline) => {
                        return Err(AudioError::Processing("tts_stream timed out while reading a frame length".into()));
                    }
                    result = stream.read(&mut prefix[prefix_read..]) => {
                        let count = result.map_err(|error| AudioError::Processing(error.to_string()))?;
                        if count == 0 {
                            if prefix_read == 0 {
                                break 'frames;
                            }
                            return Err(AudioError::Processing("truncated TTS frame length".into()));
                        }
                        prefix_read += count;
                    }
                    () = tokio::time::sleep(Duration::from_millis(10)) => {}
                }
            }
            let frame_bytes = checked_tts_frame_bytes(prefix)?;
            if received_frames >= TTS_STREAM_MAX_FRAMES {
                return Err(AudioError::Processing(
                    "TTS stream exceeds the frame-count limit".into(),
                ));
            }
            // The peer-controlled length is checked above before allocation.
            let mut encoded = vec![0_u8; frame_bytes];
            let mut payload_read = 0_usize;
            while payload_read < encoded.len() {
                if !generation_is_current() {
                    cancelled = true;
                    break 'frames;
                }
                tokio::select! {
                    biased;
                    () = tokio::time::sleep_until(deadline) => {
                        return Err(AudioError::Processing("tts_stream timed out while reading PCM".into()));
                    }
                    result = stream.read(&mut encoded[payload_read..]) => {
                        let count = result.map_err(|error| AudioError::Processing(error.to_string()))?;
                        if count == 0 {
                            return Err(AudioError::Processing("truncated TTS PCM frame".into()));
                        }
                        payload_read += count;
                    }
                    () = tokio::time::sleep(Duration::from_millis(10)) => {}
                }
            }
            if !generation_is_current() {
                cancelled = true;
                break;
            }
            let samples = encoded
                .as_chunks::<2>()
                .0
                .iter()
                .map(|sample| i16::from_le_bytes(*sample))
                .collect::<Vec<_>>();
            on_frame(&samples)?;
            received_frames += 1;
        }
        drop(stream);
        drop(listener);

        let mut result = self.read_response_until(id, "tts_stream", deadline).await?;
        if result.get("generation").and_then(Value::as_u64) != Some(generation) {
            return Err(AudioError::Processing(
                "TTS stream response generation did not match the request".into(),
            ));
        }
        if !cancelled
            && result.get("frames").and_then(Value::as_u64) != u64::try_from(received_frames).ok()
        {
            return Err(AudioError::Processing(
                "TTS stream frame count did not match the control response".into(),
            ));
        }
        if !cancelled && received_frames == 0 {
            return Err(AudioError::Processing(
                "TTS stream completed without an audio frame".into(),
            ));
        }
        let sample_rate_hz = result
            .get("sample_rate_hz")
            .and_then(Value::as_u64)
            .ok_or_else(|| AudioError::Processing("TTS stream omitted its sample rate".into()))?;
        if !(8_000..=192_000).contains(&sample_rate_hz) {
            return Err(AudioError::Processing(
                "TTS stream returned an invalid sample rate".into(),
            ));
        }
        if let Some(object) = result.as_object_mut() {
            object.insert("received_frames".into(), json!(received_frames));
            if cancelled {
                object.insert("cancelled".into(), json!(true));
            }
        }
        Ok(result)
    }

    /// Return an explicit fallback reason on platforms without Unix sockets.
    #[cfg(not(unix))]
    pub async fn tts_stream<F, C>(
        &mut self,
        socket_directory: &Path,
        payload: Value,
        generation: u64,
        timeout: Duration,
        generation_is_current: C,
        on_frame: F,
    ) -> Result<Value, AudioError>
    where
        F: FnMut(&[i16]) -> Result<(), AudioError> + Send,
        C: FnMut() -> bool + Send,
    {
        let _ = (
            socket_directory,
            payload,
            generation,
            timeout,
            generation_is_current,
            on_frame,
        );
        Err(AudioError::Unavailable(
            "streaming neural TTS requires Unix domain sockets; use whole-WAV synthesis".into(),
        ))
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
    #[cfg(unix)]
    use std::sync::atomic::{AtomicU64, Ordering};
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

    #[cfg(unix)]
    #[test]
    fn worker_cuda_library_path_uses_exact_venv_runtime_directories() {
        let root = unique_test_root();
        let python = root.join("venv/bin/python");
        let cublas = root.join("venv/lib/python3.12/site-packages/nvidia/cublas/lib");
        let cudnn = root.join("venv/lib/python3.12/site-packages/nvidia/cudnn/lib");
        std::fs::create_dir_all(python.parent().expect("python parent")).expect("venv bin");
        std::fs::create_dir_all(&cublas).expect("cuBLAS fixture");
        std::fs::create_dir_all(&cudnn).expect("cuDNN fixture");

        assert_eq!(neural_cuda_library_dirs(&python), [cublas, cudnn]);
        std::fs::remove_dir_all(root).expect("remove CUDA path fixture");
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

    #[cfg(unix)]
    fn fake_tts_worker(root: &Path) -> PathBuf {
        let script = root.join("fake-tts-worker.py");
        std::fs::write(
            &script,
            r#"import json
import socket
import struct
import sys
import time

print(json.dumps({"ready": True, "protocol": 1}), flush=True)
for raw in sys.stdin:
    request = json.loads(raw)
    request_id = request.get("id")
    generation = request.get("generation")
    if generation == 99:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
            stream.connect(request["socket_path"])
            time.sleep(2)
        result = {
            "generation": generation,
            "sample_rate_hz": 24000,
            "clause_count": 1,
            "frames": 0,
            "cancelled": False,
        }
        print(json.dumps({"id": request_id, "ok": True, "result": result}), flush=True)
        continue
    frames = [
        struct.pack("<2h", 101, 102),
        struct.pack("<3h", 201, 202, 203),
        struct.pack("<h", 301),
    ]
    if generation != 7:
        frames.extend(struct.pack("<h", value) for value in range(400, 420))
    sent = 0
    cancelled = False
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.connect(request["socket_path"])
        for frame in frames:
            try:
                stream.sendall(struct.pack("<I", len(frame)) + frame)
            except (BrokenPipeError, ConnectionResetError):
                cancelled = True
                break
            sent += 1
            time.sleep(0.025)
    result = {
        "generation": generation,
        "sample_rate_hz": 24000,
        "clause_count": len(frames),
        "frames": sent,
        "cancelled": cancelled,
    }
    print(json.dumps({"id": request_id, "ok": True, "result": result}), flush=True)
"#,
        )
        .expect("write fake TTS worker");
        script
    }

    #[cfg(unix)]
    #[test]
    fn tts_frame_length_is_bounded_before_allocation() {
        assert!(checked_tts_frame_bytes(0_u32.to_le_bytes()).is_err());
        assert!(checked_tts_frame_bytes(3_u32.to_le_bytes()).is_err());
        assert_eq!(
            checked_tts_frame_bytes(
                u32::try_from(TTS_STREAM_MAX_FRAME_BYTES)
                    .expect("frame bound fits u32")
                    .to_le_bytes()
            )
            .expect("maximum bounded frame"),
            TTS_STREAM_MAX_FRAME_BYTES
        );
        assert!(checked_tts_frame_bytes(u32::MAX.to_le_bytes()).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_worker_streams_ordered_clauses_and_generation_cancels_the_reader() {
        let root = unique_test_root();
        let neural_root = root.join("neural");
        let socket_root = root.join("runtime");
        std::fs::create_dir_all(&neural_root).expect("test neural root");
        let script = fake_tts_worker(&root);
        let mut runtime = NeuralVoiceRuntime::start(&find_python(), &script, &neural_root)
            .await
            .expect("start fake TTS worker");

        let generation = AtomicU64::new(7);
        let mut clauses = Vec::new();
        let complete = runtime
            .tts_stream(
                &socket_root,
                json!({"text": "One. Two; three?", "voice": "Ryan"}),
                7,
                Duration::from_secs(5),
                || generation.load(Ordering::SeqCst) == 7,
                |frame| {
                    clauses.push(frame.to_vec());
                    Ok(())
                },
            )
            .await
            .expect("stream three clauses");
        assert_eq!(
            clauses,
            vec![vec![101, 102], vec![201, 202, 203], vec![301]]
        );
        assert_eq!(
            clauses.into_iter().flatten().collect::<Vec<_>>(),
            vec![101, 102, 201, 202, 203, 301]
        );
        assert_eq!(complete.get("received_frames"), Some(&json!(3)));
        assert_eq!(complete.get("cancelled"), Some(&json!(false)));
        assert_eq!(
            std::fs::read_dir(&socket_root)
                .expect("private socket directory")
                .count(),
            0,
            "completed request left its private socket behind"
        );

        generation.store(8, Ordering::SeqCst);
        let mut delivered_after_bump = 0_usize;
        let cancelled = runtime
            .tts_stream(
                &socket_root,
                json!({"text": "One. Two. Three. Four."}),
                8,
                Duration::from_secs(5),
                || generation.load(Ordering::SeqCst) == 8,
                |_| {
                    delivered_after_bump += 1;
                    // Exercise invalidation from the actual PCM delivery path.
                    generation.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await
            .expect("cancel stream after generation bump");
        assert_eq!(delivered_after_bump, 1);
        assert_eq!(cancelled.get("received_frames"), Some(&json!(1)));
        assert_eq!(cancelled.get("cancelled"), Some(&json!(true)));
        assert_eq!(
            std::fs::read_dir(&socket_root)
                .expect("private socket directory")
                .count(),
            0,
            "cancelled request left its private socket behind"
        );

        generation.store(99, Ordering::SeqCst);
        let stalled_at = std::time::Instant::now();
        let stalled = runtime
            .tts_stream(
                &socket_root,
                json!({"text": "Connected but stalled."}),
                99,
                Duration::from_millis(75),
                || generation.load(Ordering::SeqCst) == 99,
                |_| Ok(()),
            )
            .await
            .expect_err("a connected silent worker must time out");
        assert!(stalled.to_string().contains("timed out"));
        assert!(
            stalled_at.elapsed() < Duration::from_secs(1),
            "the request deadline was not applied to the frame reader"
        );
        assert_eq!(
            std::fs::read_dir(&socket_root)
                .expect("private socket directory")
                .count(),
            0,
            "timed-out request left its private socket behind"
        );

        runtime.terminate();
        drop(runtime);
        std::fs::remove_dir_all(root).expect("remove fake TTS worker root");
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
