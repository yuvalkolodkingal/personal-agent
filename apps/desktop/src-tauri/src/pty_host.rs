//! Native boundary for persistent workspace PTYs.
//!
//! `OpenCode` owns the platform PTY implementation (Unix PTY, macOS PTY and
//! Windows `ConPTY`). This host owns authentication, workspace validation,
//! bounded scrollback and renderer-safe lifecycle commands. The renderer never
//! receives a sidecar URL, credential or websocket ticket.

use crate::DesktopState;
use personal_agent_runtime::{
    OpenCodeApiClient, PtySocketCommand, PtySocketConnection, PtySocketEvent,
};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::State;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};

const MAX_SCROLLBACK_BYTES: usize = 1024 * 1024;
const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_ARGUMENTS: usize = 64;
const SAFE_ENVIRONMENT_KEYS: &[&str] = &["COLORTERM", "LANG", "LC_ALL", "LC_CTYPE", "TERM", "TZ"];

/// State for native PTY attachments. PTY processes remain owned by the pinned
/// sidecar and survive renderer remounts or websocket detach/reconnect.
#[derive(Default)]
pub struct PtyHostState {
    sessions: tokio::sync::Mutex<BTreeMap<String, ManagedPty>>,
}

struct ManagedPty {
    workspace: PathBuf,
    buffer: Arc<Mutex<TerminalBuffer>>,
    commands: Option<mpsc::Sender<PtySocketCommand>>,
    socket_task: Option<JoinHandle<()>>,
    reader_task: Option<JoinHandle<()>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RuntimePty {
    id: String,
    title: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    cwd: String,
    status: String,
    pid: u32,
    #[serde(default, rename = "exitCode")]
    exit_code: Option<i32>,
}

/// Renderer-safe terminal metadata.
#[derive(Clone, Debug, Serialize)]
pub struct PtySnapshot {
    id: String,
    title: String,
    command: String,
    args: Vec<String>,
    cwd: String,
    status: String,
    pid: u32,
    exit_code: Option<i32>,
    attached: bool,
    connection: String,
    cursor: u64,
    revision: u64,
    scrollback_bytes: usize,
    scrollback_limit_bytes: usize,
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PtyReadResponse {
    id: String,
    data: String,
    reset: bool,
    revision: u64,
    cursor: u64,
    connection: String,
    error: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PtyCreateRequest {
    directory: Option<String>,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    cwd: Option<String>,
    title: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    confirmed: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PtyInputRequest {
    id: String,
    data: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PtyResizeRequest {
    id: String,
    directory: Option<String>,
    rows: u16,
    cols: u16,
}

#[derive(Clone, Debug)]
struct OutputChunk {
    revision: u64,
    text: String,
}

#[derive(Clone, Debug)]
struct TerminalBuffer {
    chunks: VecDeque<OutputChunk>,
    bytes: usize,
    revision: u64,
    cursor: u64,
    connection: String,
    error: Option<String>,
}

impl Default for TerminalBuffer {
    fn default() -> Self {
        Self {
            chunks: VecDeque::new(),
            bytes: 0,
            revision: 0,
            cursor: 0,
            connection: "detached".into(),
            error: None,
        }
    }
}

impl TerminalBuffer {
    fn push(&mut self, mut text: String) {
        let cursor_advance = u64::try_from(text.encode_utf16().count()).unwrap_or(u64::MAX);
        if text.len() > MAX_SCROLLBACK_BYTES {
            let mut start = text.len() - MAX_SCROLLBACK_BYTES;
            while !text.is_char_boundary(start) {
                start += 1;
            }
            text.drain(..start);
            self.chunks.clear();
            self.bytes = 0;
        }
        self.revision = self.revision.saturating_add(1);
        self.cursor = self.cursor.saturating_add(cursor_advance);
        self.bytes = self.bytes.saturating_add(text.len());
        self.chunks.push_back(OutputChunk {
            revision: self.revision,
            text,
        });
        while self.bytes > MAX_SCROLLBACK_BYTES {
            let Some(removed) = self.chunks.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(removed.text.len());
        }
    }

    fn read(&self, after_revision: Option<u64>) -> (String, bool) {
        let first = self
            .chunks
            .front()
            .map_or(self.revision, |item| item.revision);
        let after = after_revision.unwrap_or(0);
        let reset = after_revision.is_none() || after.saturating_add(1) < first;
        let data = self
            .chunks
            .iter()
            .filter(|chunk| reset || chunk.revision > after)
            .map(|chunk| chunk.text.as_str())
            .collect();
        (data, reset)
    }
}

async fn runtime_client(state: &DesktopState) -> Result<OpenCodeApiClient, String> {
    state
        .runtime
        .lock()
        .await
        .api_client()
        .map_err(|error| error.to_string())
}

fn configured_workspace(
    state: &DesktopState,
    requested: Option<&str>,
) -> Result<(PathBuf, String), String> {
    let config = state
        .config
        .read()
        .map_err(|_| "configuration lock is poisoned".to_owned())?;
    let root = std::fs::canonicalize(&config.runtime.working_directory)
        .map_err(|error| format!("workspace is unavailable: {error}"))?;
    let requested = requested
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&config.runtime.working_directory);
    let selected = std::fs::canonicalize(requested)
        .map_err(|error| format!("terminal directory is unavailable: {error}"))?;
    if !selected.is_dir() || !selected.starts_with(&root) {
        return Err("terminal directory leaves the configured workspace".into());
    }
    Ok((selected, config.workspace.terminal_shell.clone()))
}

fn validate_start(
    workspace: &Path,
    configured_shell: &str,
    request: &PtyCreateRequest,
) -> Result<PathBuf, String> {
    if request.command != configured_shell {
        return Err("terminal program must match the configured workspace shell".into());
    }
    if request.command.is_empty()
        || request.command.len() > 4_096
        || request.command.contains('\0')
        || request.args.len() > MAX_ARGUMENTS
    {
        return Err("terminal program or argument count exceeds the native limit".into());
    }
    if request
        .args
        .iter()
        .any(|value| value.len() > 4_096 || value.contains('\0'))
    {
        return Err("terminal argument exceeds the native limit".into());
    }
    let cwd = request.cwd.as_deref().unwrap_or_else(|| {
        workspace
            .to_str()
            .expect("canonical workspace path was already accepted")
    });
    let cwd = std::fs::canonicalize(cwd)
        .map_err(|error| format!("terminal working directory is unavailable: {error}"))?;
    if !cwd.is_dir() || !cwd.starts_with(workspace) {
        return Err("terminal working directory leaves the configured workspace".into());
    }
    if request.env.len() > SAFE_ENVIRONMENT_KEYS.len()
        || request.env.iter().any(|(key, value)| {
            !SAFE_ENVIRONMENT_KEYS.contains(&key.as_str())
                || value.len() > 4_096
                || value.contains('\0')
        })
    {
        return Err("terminal environment contains a non-reviewed variable or value".into());
    }
    let executable = Path::new(&request.command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&request.command)
        .to_ascii_lowercase();
    let privileged = ["doas", "pkexec", "su", "sudo"].contains(&executable.as_str());
    let command_mode = request.args.iter().any(|argument| {
        matches!(
            argument.to_ascii_lowercase().as_str(),
            "-c" | "/c" | "--command" | "-command"
        )
    });
    if (privileged || command_mode) && !request.confirmed {
        return Err(
            "confirmation required before starting a privileged or command-mode terminal".into(),
        );
    }
    if request.title.as_ref().is_some_and(|title| {
        title.is_empty() || title.len() > 160 || title.chars().any(char::is_control)
    }) {
        return Err("terminal title is invalid".into());
    }
    Ok(cwd)
}

fn validate_identifier(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("terminal identifier is not canonical".into());
    }
    Ok(())
}

fn runtime_pty(
    value: Value,
    expected_id: Option<&str>,
    workspace: &Path,
    expected_cwd: Option<&Path>,
) -> Result<RuntimePty, String> {
    let mut info = serde_json::from_value::<RuntimePty>(value)
        .map_err(|_| "OpenCode returned invalid PTY metadata".to_owned())?;
    validate_identifier(&info.id)?;
    if expected_id.is_some_and(|expected| expected != info.id) {
        return Err("OpenCode returned metadata for a different terminal".into());
    }
    let cwd = std::fs::canonicalize(&info.cwd)
        .map_err(|_| "OpenCode returned an unavailable PTY working directory".to_owned())?;
    if !cwd.is_dir() || !cwd.starts_with(workspace) {
        return Err("OpenCode returned a PTY outside the configured workspace".into());
    }
    if expected_cwd.is_some_and(|expected| expected != cwd) {
        return Err("OpenCode returned a different PTY working directory".into());
    }
    info.cwd = cwd.display().to_string();
    Ok(info)
}

fn snapshot(info: RuntimePty, buffer: Option<&Arc<Mutex<TerminalBuffer>>>) -> PtySnapshot {
    let buffer = buffer.and_then(|buffer| buffer.lock().ok());
    PtySnapshot {
        id: info.id,
        title: info.title,
        command: info.command,
        args: info.args,
        cwd: info.cwd,
        status: info.status,
        pid: info.pid,
        exit_code: info.exit_code,
        attached: buffer
            .as_ref()
            .is_some_and(|buffer| buffer.connection == "connected"),
        connection: buffer
            .as_ref()
            .map_or_else(|| "detached".into(), |buffer| buffer.connection.clone()),
        cursor: buffer.as_ref().map_or(0, |buffer| buffer.cursor),
        revision: buffer.as_ref().map_or(0, |buffer| buffer.revision),
        scrollback_bytes: buffer.as_ref().map_or(0, |buffer| buffer.bytes),
        scrollback_limit_bytes: MAX_SCROLLBACK_BYTES,
        error: buffer.as_ref().and_then(|buffer| buffer.error.clone()),
    }
}

async fn stop_connection(session: &mut ManagedPty) {
    let close_queued = session
        .commands
        .take()
        .is_none_or(|commands| commands.try_send(PtySocketCommand::Close).is_ok());
    if let Some(mut task) = session.socket_task.take() {
        if close_queued {
            if timeout(Duration::from_secs(1), &mut task).await.is_err() {
                task.abort();
            }
        } else {
            task.abort();
        }
    }
    if let Some(mut task) = session.reader_task.take()
        && timeout(Duration::from_secs(1), &mut task).await.is_err()
    {
        task.abort();
    }
}

fn start_reader(
    buffer: Arc<Mutex<TerminalBuffer>>,
    mut connection: PtySocketConnection,
) -> (
    mpsc::Sender<PtySocketCommand>,
    JoinHandle<()>,
    JoinHandle<()>,
) {
    let commands = connection.commands.clone();
    let socket_task = connection.task;
    let reader_task = tokio::spawn(async move {
        if let Ok(mut state) = buffer.lock() {
            state.connection = "connected".into();
            state.error = None;
        }
        while let Some(event) = connection.events.recv().await {
            let Ok(mut state) = buffer.lock() else {
                break;
            };
            match event {
                PtySocketEvent::Output(output) => state.push(output),
                PtySocketEvent::Cursor(cursor) => state.cursor = cursor,
                PtySocketEvent::Closed { code, reason } => {
                    state.connection = "detached".into();
                    if code != 1000 {
                        state.error = Some(if reason.is_empty() {
                            format!("terminal connection closed with code {code}")
                        } else {
                            reason
                        });
                    }
                }
                PtySocketEvent::Error(error) => {
                    state.connection = "degraded".into();
                    state.error = Some(error);
                }
            }
        }
        if let Ok(mut state) = buffer.lock()
            && state.connection == "connected"
        {
            state.connection = "detached".into();
        }
    });
    (commands, socket_task, reader_task)
}

async fn attach(
    host: &PtyHostState,
    client: &OpenCodeApiClient,
    id: &str,
    workspace: &Path,
) -> Result<Arc<Mutex<TerminalBuffer>>, String> {
    let mut previous = host.sessions.lock().await.remove(id);
    if let Some(session) = previous.as_mut() {
        stop_connection(session).await;
    }
    let buffer = previous.map_or_else(
        || Arc::new(Mutex::new(TerminalBuffer::default())),
        |session| {
            if session.workspace == workspace {
                session.buffer
            } else {
                Arc::new(Mutex::new(TerminalBuffer::default()))
            }
        },
    );
    let cursor = buffer.lock().map_or(0, |state| state.cursor);
    if let Ok(mut state) = buffer.lock() {
        state.connection = "connecting".into();
        state.error = None;
    }
    let connection = match client.connect_pty(id, workspace, cursor).await {
        Ok(connection) => connection,
        Err(error) => {
            if let Ok(mut state) = buffer.lock() {
                state.connection = "degraded".into();
                state.error = Some(error.to_string());
            }
            host.sessions.lock().await.insert(
                id.to_owned(),
                ManagedPty {
                    workspace: workspace.to_path_buf(),
                    buffer: buffer.clone(),
                    commands: None,
                    socket_task: None,
                    reader_task: None,
                },
            );
            return Err("terminal websocket could not be attached".into());
        }
    };
    let (commands, socket_task, reader_task) = start_reader(buffer.clone(), connection);
    host.sessions.lock().await.insert(
        id.to_owned(),
        ManagedPty {
            workspace: workspace.to_path_buf(),
            buffer: buffer.clone(),
            commands: Some(commands),
            socket_task: Some(socket_task),
            reader_task: Some(reader_task),
        },
    );
    Ok(buffer)
}

/// Report the PTY backend without overstating native verification.
#[tauri::command]
pub fn pty_capability() -> Value {
    json!({
        "available": true,
        "backend": "opencode-pinned-pty",
        "platform": std::env::consts::OS,
        "native_verified": cfg!(target_os = "linux"),
        "persistence": "runtime-lifetime",
        "reconnect": "absolute-cursor",
        "scrollback_limit_bytes": MAX_SCROLLBACK_BYTES,
        "detail": if cfg!(target_os = "linux") {
            "Live PTY adapter verified on Linux; authentication and websocket transport remain native-owned."
        } else {
            "The pinned runtime provides the platform PTY backend, but this build target has not been live-verified by this host."
        }
    })
}

#[tauri::command]
pub async fn pty_list(
    state: State<'_, DesktopState>,
    host: State<'_, PtyHostState>,
    directory: Option<String>,
) -> Result<Vec<PtySnapshot>, String> {
    let (workspace, _) = configured_workspace(&state, directory.as_deref())?;
    let client = runtime_client(&state).await?;
    let value = client
        .request_json(
            Method::GET,
            "/pty",
            &[("directory", workspace.display().to_string())],
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
    let infos = serde_json::from_value::<Vec<Value>>(value)
        .map_err(|_| "OpenCode returned an invalid PTY list".to_owned())?;
    let sessions = host.sessions.lock().await;
    infos
        .into_iter()
        .map(|value| {
            let info = runtime_pty(value, None, &workspace, None)?;
            let buffer = sessions.get(&info.id).map(|item| &item.buffer);
            Ok(snapshot(info, buffer))
        })
        .collect::<Result<Vec<_>, String>>()
}

#[tauri::command]
pub async fn pty_create(
    state: State<'_, DesktopState>,
    host: State<'_, PtyHostState>,
    request: PtyCreateRequest,
) -> Result<PtySnapshot, String> {
    let (workspace, configured_shell) = configured_workspace(&state, request.directory.as_deref())?;
    let cwd = validate_start(&workspace, &configured_shell, &request)?;
    let client = runtime_client(&state).await?;
    let value = client
        .request_json(
            Method::POST,
            "/pty",
            &[("directory", workspace.display().to_string())],
            Some(json!({
                "command": request.command,
                "args": request.args,
                "cwd": cwd,
                "title": request.title.unwrap_or_else(|| "Personal Agent terminal".into()),
                "env": request.env,
            })),
        )
        .await
        .map_err(|error| error.to_string())?;
    let info = runtime_pty(value, None, &workspace, Some(&cwd))?;
    let buffer = attach(&host, &client, &info.id, &workspace).await?;
    Ok(snapshot(info, Some(&buffer)))
}

#[tauri::command]
pub async fn pty_reconnect(
    state: State<'_, DesktopState>,
    host: State<'_, PtyHostState>,
    id: String,
    directory: Option<String>,
) -> Result<PtySnapshot, String> {
    let (workspace, _) = configured_workspace(&state, directory.as_deref())?;
    validate_identifier(&id)?;
    let client = runtime_client(&state).await?;
    let value = client
        .request_json(
            Method::GET,
            &format!("/pty/{id}"),
            &[("directory", workspace.display().to_string())],
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
    let info = runtime_pty(value, Some(&id), &workspace, None)?;
    let buffer = attach(&host, &client, &info.id, &workspace).await?;
    Ok(snapshot(info, Some(&buffer)))
}

#[tauri::command]
pub async fn pty_input(
    host: State<'_, PtyHostState>,
    request: PtyInputRequest,
) -> Result<(), String> {
    validate_identifier(&request.id)?;
    if request.data.is_empty() || request.data.len() > MAX_INPUT_BYTES {
        return Err("terminal input is empty or exceeds the native 64 KiB limit".into());
    }
    let sender = host
        .sessions
        .lock()
        .await
        .get(&request.id)
        .and_then(|session| session.commands.clone())
        .ok_or_else(|| "terminal is detached; reconnect before sending input".to_owned())?;
    sender
        .send(PtySocketCommand::Input(request.data))
        .await
        .map_err(|_| "terminal input channel is closed".to_owned())
}

#[tauri::command]
pub async fn pty_resize(
    state: State<'_, DesktopState>,
    request: PtyResizeRequest,
) -> Result<PtySnapshot, String> {
    if !(2..=512).contains(&request.rows) || !(2..=512).contains(&request.cols) {
        return Err("terminal dimensions must be between 2 and 512".into());
    }
    let (workspace, _) = configured_workspace(&state, request.directory.as_deref())?;
    validate_identifier(&request.id)?;
    let client = runtime_client(&state).await?;
    let value = client
        .request_json(
            Method::PUT,
            &format!("/pty/{}", request.id),
            &[("directory", workspace.display().to_string())],
            Some(json!({"size": {"rows": request.rows, "cols": request.cols}})),
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(snapshot(
        runtime_pty(value, Some(&request.id), &workspace, None)?,
        None,
    ))
}

#[tauri::command]
pub async fn pty_read(
    host: State<'_, PtyHostState>,
    id: String,
    after_revision: Option<u64>,
) -> Result<PtyReadResponse, String> {
    validate_identifier(&id)?;
    let buffer = host
        .sessions
        .lock()
        .await
        .get(&id)
        .map(|session| session.buffer.clone())
        .ok_or_else(|| "terminal has no native attachment".to_owned())?;
    let state = buffer
        .lock()
        .map_err(|_| "terminal scrollback lock is poisoned".to_owned())?;
    let (data, reset) = state.read(after_revision);
    Ok(PtyReadResponse {
        id,
        data,
        reset,
        revision: state.revision,
        cursor: state.cursor,
        connection: state.connection.clone(),
        error: state.error.clone(),
    })
}

#[tauri::command]
pub async fn pty_terminate(
    state: State<'_, DesktopState>,
    host: State<'_, PtyHostState>,
    id: String,
    directory: Option<String>,
    confirmed: bool,
) -> Result<(), String> {
    if !confirmed {
        return Err("confirmation required before terminating a terminal process".into());
    }
    let (workspace, _) = configured_workspace(&state, directory.as_deref())?;
    validate_identifier(&id)?;
    let client = runtime_client(&state).await?;
    client
        .request_json(
            Method::DELETE,
            &format!("/pty/{id}"),
            &[("directory", workspace.display().to_string())],
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
    if let Some(mut session) = host.sessions.lock().await.remove(&id) {
        stop_connection(&mut session).await;
    }
    Ok(())
}

impl PtyHostState {
    pub async fn shutdown(&self) {
        let sessions = {
            let mut sessions = self.sessions.lock().await;
            std::mem::take(&mut *sessions)
        };
        for (_, mut session) in sessions {
            stop_connection(&mut session).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_request(workspace: &Path) -> PtyCreateRequest {
        PtyCreateRequest {
            directory: None,
            command: "/bin/sh".into(),
            args: Vec::new(),
            cwd: Some(workspace.display().to_string()),
            title: Some("Workspace".into()),
            env: BTreeMap::from([("TERM".into(), "xterm-256color".into())]),
            confirmed: false,
        }
    }

    #[test]
    fn structured_start_rejects_program_substitution_and_command_mode() {
        let temp = tempfile::tempdir().expect("workspace");
        let mut request = create_request(temp.path());
        request.command = "/bin/bash".into();
        assert!(
            validate_start(temp.path(), "/bin/sh", &request)
                .expect_err("program substitution")
                .contains("configured workspace shell")
        );

        request.command = "/bin/sh".into();
        request.args = vec!["-c".into(), "printf safe".into()];
        assert!(
            validate_start(temp.path(), "/bin/sh", &request)
                .expect_err("command confirmation")
                .contains("confirmation required")
        );
        request.confirmed = true;
        assert_eq!(
            validate_start(temp.path(), "/bin/sh", &request).expect("confirmed command"),
            temp.path()
        );
    }

    #[test]
    fn start_rejects_external_cwd_and_unreviewed_environment() {
        let workspace = tempfile::tempdir().expect("workspace");
        let external = tempfile::tempdir().expect("external");
        let mut request = create_request(workspace.path());
        request.cwd = Some(external.path().display().to_string());
        assert!(
            validate_start(workspace.path(), "/bin/sh", &request)
                .expect_err("external cwd")
                .contains("leaves")
        );
        request.cwd = Some(workspace.path().display().to_string());
        request.env.insert("OPENAI_API_KEY".into(), "secret".into());
        assert!(
            validate_start(workspace.path(), "/bin/sh", &request)
                .expect_err("secret env")
                .contains("non-reviewed")
        );
    }

    #[test]
    fn scrollback_is_bounded_and_reports_reset_after_eviction() {
        let mut buffer = TerminalBuffer::default();
        buffer.push("old".into());
        buffer.push("x".repeat(MAX_SCROLLBACK_BYTES));
        assert!(buffer.bytes <= MAX_SCROLLBACK_BYTES);
        let (data, reset) = buffer.read(Some(0));
        assert!(reset);
        assert_eq!(data.len(), MAX_SCROLLBACK_BYTES);
    }

    #[test]
    fn scrollback_incremental_reads_and_utf16_cursor_are_stable() {
        let mut buffer = TerminalBuffer::default();
        buffer.push("A😀".into());
        let first = buffer.revision;
        buffer.push("B".into());
        let (data, reset) = buffer.read(Some(first));
        assert!(!reset);
        assert_eq!(data, "B");
        assert_eq!(buffer.cursor, 4);
    }

    #[test]
    fn oversized_output_retains_the_absolute_cursor() {
        let mut buffer = TerminalBuffer::default();
        let output = format!("{}😀", "x".repeat(MAX_SCROLLBACK_BYTES));
        let expected_cursor = u64::try_from(output.encode_utf16().count()).expect("cursor");
        buffer.push(output);
        assert_eq!(buffer.bytes, MAX_SCROLLBACK_BYTES);
        assert_eq!(buffer.cursor, expected_cursor);
    }

    #[test]
    fn runtime_metadata_must_match_the_requested_terminal_and_workspace() {
        let workspace = tempfile::tempdir().expect("workspace");
        let external = tempfile::tempdir().expect("external");
        let metadata = |id: &str, cwd: &Path| {
            json!({
                "id": id,
                "title": "Terminal",
                "command": "/bin/sh",
                "args": [],
                "cwd": cwd,
                "status": "running",
                "pid": 42
            })
        };

        assert!(
            runtime_pty(
                metadata("pty_two", workspace.path()),
                Some("pty_one"),
                workspace.path(),
                None,
            )
            .expect_err("mismatched id")
            .contains("different terminal")
        );
        assert!(
            runtime_pty(
                metadata("../other", workspace.path()),
                None,
                workspace.path(),
                None,
            )
            .expect_err("non-canonical id")
            .contains("identifier")
        );
        assert!(
            runtime_pty(
                metadata("pty_one", external.path()),
                Some("pty_one"),
                workspace.path(),
                None,
            )
            .expect_err("external cwd")
            .contains("outside")
        );
    }
}
