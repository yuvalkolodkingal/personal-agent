use super::{ActiveSession, DesktopState, VoicePlayback, configured_runtime};
use personal_agent_agent::Goal;
use personal_agent_audio::{
    NativeVoiceStatus, discover_native_voice, play_wav, synthesize_piper, transcribe_pcm,
};
use personal_agent_core::{CONFIG_SCHEMA, PersonalAgentConfig, parse_config};
use personal_agent_platform::{OsSecretStore, SecretReference, SecretStore};
use personal_agent_runtime::{AgentRuntime, RuntimeAnswer, SessionOptions};
use secrecy::SecretString;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager};
use tokio::process::Command;

fn config_snapshot(state: &DesktopState) -> Result<PersonalAgentConfig, String> {
    state
        .config
        .read()
        .map(|config| config.clone())
        .map_err(|_| "configuration lock is poisoned".to_owned())
}

fn canonical_directory(
    config: &PersonalAgentConfig,
    requested: Option<&str>,
) -> Result<PathBuf, String> {
    let selected = requested
        .filter(|path| !path.trim().is_empty())
        .unwrap_or(&config.runtime.working_directory);
    let directory = std::fs::canonicalize(selected).map_err(|error| error.to_string())?;
    if !directory.is_dir() {
        return Err("working directory is not a directory".to_owned());
    }
    Ok(directory)
}

fn voice_status_for(state: &DesktopState, config: &PersonalAgentConfig) -> NativeVoiceStatus {
    discover_native_voice(
        &state.app_data.join("voice"),
        &config.voice.stt_executable,
        &config.voice.stt_model_path,
        &config.voice.tts_executable,
        &config.voice.tts_model_path,
    )
}

#[tauri::command]
pub(crate) async fn bootstrap(state: tauri::State<'_, DesktopState>) -> Result<Value, String> {
    let config = config_snapshot(&state)?;
    let (projection, history) = {
        let profile = state
            .profile
            .lock()
            .map_err(|_| "profile state lock is poisoned".to_owned())?;
        (
            profile.projection().clone(),
            profile
                .events_after(0, 500)
                .map_err(|error| error.to_string())?,
        )
    };
    let directory = canonical_directory(&config, None)?;
    let mut runtime = state.runtime.lock().await;
    let mut catalog = runtime
        .desktop_catalog(&directory)
        .await
        .unwrap_or_else(|error| json!({"error": error.to_string()}));
    let models = runtime
        .discover_models(Some(&directory))
        .await
        .unwrap_or_default();
    if let Some(object) = catalog.as_object_mut() {
        object.insert(
            "models".to_owned(),
            json!({"available": true, "data": models}),
        );
    }
    drop(runtime);
    let schema: Value = serde_json::from_str(CONFIG_SCHEMA)
        .map_err(|error| format!("configuration schema is invalid: {error}"))?;
    Ok(json!({
        "config": config,
        "config_schema": schema,
        "projection": projection,
        "history": history,
        "catalog": catalog,
        "voice": voice_status_for(&state, &config),
        "app_data": state.app_data,
    }))
}

fn atomic_save_config(path: &Path, config: &PersonalAgentConfig) -> Result<(), String> {
    let rendered = toml::to_string_pretty(config).map_err(|error| error.to_string())?;
    parse_config(&rendered).map_err(|error| error.to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "configuration path has no parent".to_owned())?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = parent.join(format!(".config-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(rendered.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| error.to_string())?;
        std::fs::rename(&temporary, path).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[tauri::command]
pub(crate) async fn save_config(
    config: Value,
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    let config: PersonalAgentConfig =
        serde_json::from_value(config).map_err(|error| error.to_string())?;
    let rendered = toml::to_string_pretty(&config).map_err(|error| error.to_string())?;
    let validated = parse_config(&rendered).map_err(|error| error.to_string())?;
    atomic_save_config(&state.config_path, &validated.config)?;
    *state
        .config
        .write()
        .map_err(|_| "configuration lock is poisoned".to_owned())? = validated.config.clone();

    let mut runtime = state.runtime.lock().await;
    runtime.stop().await.map_err(|error| error.to_string())?;
    *runtime = configured_runtime(&state, &validated.config);
    let health = runtime.start().await.map_err(|error| error.to_string())?;
    drop(runtime);
    if let Ok(mut profile) = state.profile.lock() {
        let _ = profile.record_runtime_health(&health);
    }
    app.emit("config-updated", &validated.config)
        .map_err(|error| error.to_string())?;
    Ok(json!({"config": validated.config, "runtime": health}))
}

#[tauri::command]
pub(crate) async fn runtime_catalog(
    directory: Option<String>,
    state: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    let config = config_snapshot(&state)?;
    let directory = canonical_directory(&config, directory.as_deref())?;
    let mut runtime = state.runtime.lock().await;
    let mut catalog = runtime
        .desktop_catalog(&directory)
        .await
        .map_err(|error| error.to_string())?;
    let models = runtime
        .discover_models(Some(&directory))
        .await
        .map_err(|error| error.to_string())?;
    if let Some(object) = catalog.as_object_mut() {
        object.insert(
            "models".to_owned(),
            json!({"available": true, "data": models}),
        );
    }
    Ok(catalog)
}

#[tauri::command]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // Native turn orchestration keeps lifecycle outcomes in one auditable path.
pub(crate) async fn chat_send(
    text: String,
    attachments: Option<Vec<Value>>,
    directory: Option<String>,
    model: Option<String>,
    agent: Option<String>,
    effort: Option<String>,
    speak_response: bool,
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    let text = text.trim().to_owned();
    if text.is_empty() {
        return Err("message cannot be blank".to_owned());
    }
    let projection = state
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?
        .submit_user_message(&text)
        .map_err(|error| error.to_string())?;
    let config = config_snapshot(&state)?;
    let directory = canonical_directory(&config, directory.as_deref())?;
    let existing = state.active_session.lock().await.clone();
    let mut runtime = state.runtime.lock().await;
    let session_id = if let Some(active) = existing.filter(|active| active.directory == directory) {
        active.id
    } else {
        let selected_model = model.filter(|value| !value.trim().is_empty()).or_else(|| {
            (!config.runtime.default_model.is_empty()).then(|| {
                if config.runtime.default_model.contains('/') {
                    config.runtime.default_model.clone()
                } else {
                    format!(
                        "{}/{}",
                        config.runtime.default_provider, config.runtime.default_model
                    )
                }
            })
        });
        runtime
            .begin_session(SessionOptions {
                model: selected_model,
                effort: effort.filter(|value| !value.trim().is_empty()),
                agent: agent
                    .filter(|value| !value.trim().is_empty())
                    .or_else(|| Some(config.runtime.default_agent.clone())),
                working_directory: directory.clone(),
                environment: BTreeMap::default(),
            })
            .await
            .map_err(|error| error.to_string())?
    };
    let receiver = runtime
        .submit_with_attachments(&session_id, &text, attachments.unwrap_or_default())
        .await
        .map_err(|error| error.to_string())?;
    drop(runtime);
    *state.active_session.lock().await = Some(ActiveSession {
        id: session_id.clone(),
        directory: directory.clone(),
    });

    let session_for_task = session_id.clone();
    let directory_for_task = directory.display().to_string();
    let mut receiver = receiver;
    tauri::async_runtime::spawn(async move {
        let mut response = String::new();
        let started = Instant::now();
        let mut outcome = "completed";
        let mut failure: Option<String> = None;
        loop {
            let next = tokio::time::timeout(Duration::from_secs(60), receiver.recv()).await;
            let event = match next {
                Ok(Some(event)) => event,
                Ok(None) => {
                    outcome = "failed";
                    failure = Some(
                        "The OpenCode event stream ended before the turn completed.".to_owned(),
                    );
                    break;
                }
                Err(_) => {
                    let state = app.state::<DesktopState>();
                    let status = state
                        .runtime
                        .lock()
                        .await
                        .request_json(
                            reqwest::Method::GET,
                            "/session/status",
                            &[("directory", directory_for_task.clone())],
                            None,
                        )
                        .await;
                    let session_status = status
                        .as_ref()
                        .ok()
                        .and_then(|value| value.get(&session_for_task))
                        .and_then(|value| value.get("type"))
                        .and_then(Value::as_str);
                    if session_status == Some("idle") {
                        break;
                    }
                    if started.elapsed() >= Duration::from_mins(30) {
                        let _ = state
                            .runtime
                            .lock()
                            .await
                            .abort_session(&session_for_task)
                            .await;
                        outcome = "failed";
                        failure = Some(
                            "The turn exceeded the 30 minute safety limit and was stopped."
                                .to_owned(),
                        );
                        break;
                    }
                    continue;
                }
            };
            if event.r#type == "response.delta"
                && let Ok(payload) = event.payload()
                && let Some(delta) = payload
                    .get("delta")
                    .or_else(|| payload.get("text"))
                    .and_then(Value::as_str)
            {
                response.push_str(delta);
            }
            let state = app.state::<DesktopState>();
            if let Ok(mut profile) = state.profile.lock() {
                let _ = profile.record_runtime_event(event.clone());
            }
            let _ = app.emit("runtime-event", &event);
            if event.r#type == "response.failed" {
                outcome = "failed";
                failure = event
                    .payload()
                    .ok()
                    .and_then(|payload| {
                        payload
                            .pointer("/error/data/message")
                            .or_else(|| payload.pointer("/error/message"))
                            .or_else(|| payload.get("message"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .or_else(|| {
                        Some(
                            "The provider could not complete this turn. Check its connection and model settings."
                                .to_owned(),
                        )
                    });
                break;
            }
            if matches!(
                event.r#type.as_str(),
                "runtime.stream_error" | "runtime.stream_closed"
            ) {
                outcome = "failed";
                failure = Some(
                    "The OpenCode response stream disconnected. You can retry this message."
                        .to_owned(),
                );
                break;
            }
            if event.r#type == "response.completed" {
                break;
            }
        }
        let _ = app.emit(
            "runtime-turn-complete",
            json!({
                "session_id": session_for_task,
                "text": response,
                "speak": speak_response,
                "status": outcome,
                "error": failure,
                "elapsed_ms": started.elapsed().as_millis(),
            }),
        );
    });
    Ok(json!({
        "session_id": session_id,
        "directory": directory,
        "projection": projection,
    }))
}

fn valid_session_id(value: &str) -> bool {
    value.starts_with("ses")
        && value.len() <= 128
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

#[tauri::command]
#[allow(clippy::too_many_lines)]
pub(crate) async fn session_action(
    action: String,
    session_id: Option<String>,
    directory: Option<String>,
    title: Option<String>,
    confirmed: Option<bool>,
    state: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    let config = config_snapshot(&state)?;
    let directory = canonical_directory(&config, directory.as_deref())?;
    let directory_text = directory.display().to_string();
    let mut runtime = state.runtime.lock().await;
    if action == "new" {
        let id = runtime
            .begin_session(SessionOptions {
                model: None,
                effort: None,
                agent: Some(config.runtime.default_agent.clone()),
                working_directory: directory.clone(),
                environment: BTreeMap::default(),
            })
            .await
            .map_err(|error| error.to_string())?;
        *state.active_session.lock().await = Some(ActiveSession {
            id: id.clone(),
            directory,
        });
        return Ok(json!({"session_id": id}));
    }
    let session_id = session_id.ok_or_else(|| "session ID is required".to_owned())?;
    if !valid_session_id(&session_id) {
        return Err("session ID is invalid".to_owned());
    }
    let query = [("directory", directory_text)];
    match action.as_str() {
        "resume" => {
            runtime
                .resume_session(&session_id, &directory)
                .await
                .map_err(|error| error.to_string())?;
            *state.active_session.lock().await = Some(ActiveSession {
                id: session_id.clone(),
                directory,
            });
            Ok(json!({"session_id": session_id}))
        }
        "fork" => runtime
            .fork_session(&session_id)
            .await
            .map(|id| json!({"session_id": id}))
            .map_err(|error| error.to_string()),
        "compact" => runtime
            .compact_session(&session_id)
            .await
            .map(|()| json!({"compacted": true}))
            .map_err(|error| error.to_string()),
        "abort" => runtime
            .abort_session(&session_id)
            .await
            .map(|()| json!({"aborted": true}))
            .map_err(|error| error.to_string()),
        "rename" => {
            let title = title
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "session title is required".to_owned())?;
            runtime
                .request_json(
                    reqwest::Method::PATCH,
                    &format!("/session/{session_id}"),
                    &query,
                    Some(json!({"title": title})),
                )
                .await
                .map_err(|error| error.to_string())
        }
        "delete" => {
            if confirmed != Some(true) {
                return Err("session deletion requires confirmation".to_owned());
            }
            runtime
                .request_json(
                    reqwest::Method::DELETE,
                    &format!("/session/{session_id}"),
                    &query,
                    None,
                )
                .await
                .map_err(|error| error.to_string())
        }
        "share" => {
            if confirmed != Some(true) {
                return Err("session sharing requires explicit confirmation".to_owned());
            }
            runtime
                .request_json(
                    reqwest::Method::POST,
                    &format!("/session/{session_id}/share"),
                    &query,
                    None,
                )
                .await
                .map_err(|error| error.to_string())
        }
        "unshare" => runtime
            .request_json(
                reqwest::Method::DELETE,
                &format!("/session/{session_id}/share"),
                &query,
                None,
            )
            .await
            .map_err(|error| error.to_string()),
        _ => Err("unknown session action".to_owned()),
    }
}

#[tauri::command]
pub(crate) async fn runtime_resource(
    kind: String,
    session_id: Option<String>,
    directory: Option<String>,
    path: Option<String>,
    query: Option<String>,
    state: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    let config = config_snapshot(&state)?;
    let directory = canonical_directory(&config, directory.as_deref())?;
    let directory_text = directory.display().to_string();
    let session = session_id.unwrap_or_default();
    if kind.starts_with("session_") && !valid_session_id(&session) {
        return Err("session ID is invalid".to_owned());
    }
    let requested_path = path.unwrap_or_default();
    if requested_path.split('/').any(|part| part == "..") {
        return Err("workspace path traversal is not allowed".to_owned());
    }
    let (route, mut parameters) = match kind.as_str() {
        "session_messages" => (format!("/session/{session}/message"), vec![]),
        "session_todo" => (format!("/session/{session}/todo"), vec![]),
        "session_diff" => (format!("/session/{session}/diff"), vec![]),
        "session_children" => (format!("/session/{session}/children"), vec![]),
        "file_list" => ("/file".to_owned(), vec![("path", requested_path)]),
        "file_content" => ("/file/content".to_owned(), vec![("path", requested_path)]),
        "file_status" => ("/file/status".to_owned(), vec![]),
        "find_text" => (
            "/find".to_owned(),
            vec![("pattern", query.unwrap_or_default())],
        ),
        "find_file" => (
            "/find/file".to_owned(),
            vec![("query", query.unwrap_or_default())],
        ),
        "find_symbol" => (
            "/find/symbol".to_owned(),
            vec![("query", query.unwrap_or_default())],
        ),
        "vcs_diff" => ("/vcs/diff".to_owned(), vec![]),
        "vcs_diff_raw" => ("/vcs/diff/raw".to_owned(), vec![]),
        "vcs_status" => ("/vcs/status".to_owned(), vec![]),
        "pty_list" => ("/pty".to_owned(), vec![]),
        "worktree_list" => ("/experimental/worktree".to_owned(), vec![]),
        "permission_list" => ("/permission".to_owned(), vec![]),
        "question_list" => ("/question".to_owned(), vec![]),
        _ => return Err("unknown runtime resource".to_owned()),
    };
    parameters.push(("directory", directory_text));
    let query = parameters
        .iter()
        .map(|(name, value)| (*name, value.clone()))
        .collect::<Vec<_>>();
    state
        .runtime
        .lock()
        .await
        .request_json(reqwest::Method::GET, &route, &query, None)
        .await
        .map_err(|error| error.to_string())
}

fn valid_runtime_identifier(value: &str, prefix: Option<&str>) -> bool {
    prefix.is_none_or(|expected| value.starts_with(expected))
        && !value.is_empty()
        && value.len() <= 160
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

/// Mutating `OpenCode` operations exposed through a strict allow-list. The
/// renderer never receives the sidecar address or credential.
#[tauri::command]
#[allow(clippy::too_many_lines)]
pub(crate) async fn runtime_operation(
    kind: String,
    identifier: Option<String>,
    session_id: Option<String>,
    directory: Option<String>,
    payload: Value,
    confirmed: Option<bool>,
    state: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    let config = config_snapshot(&state)?;
    let directory = canonical_directory(&config, directory.as_deref())?;
    let directory_text = directory.display().to_string();
    let query = [("directory", directory_text)];
    if serde_json::to_vec(&payload)
        .map_err(|error| error.to_string())?
        .len()
        > 256 * 1024
    {
        return Err("runtime operation payload exceeds 256 KiB".to_owned());
    }

    let id = identifier.unwrap_or_default();
    let session = session_id.unwrap_or_default();
    let (method, route, body) = match kind.as_str() {
        "mcp_connect" | "mcp_disconnect" => {
            if !valid_runtime_identifier(&id, None) {
                return Err("MCP server name is invalid".to_owned());
            }
            let action = if kind == "mcp_connect" {
                "connect"
            } else {
                "disconnect"
            };
            (reqwest::Method::POST, format!("/mcp/{id}/{action}"), None)
        }
        "pty_create" => (reqwest::Method::POST, "/pty".to_owned(), Some(payload)),
        "pty_update" => {
            if !valid_runtime_identifier(&id, Some("pty")) {
                return Err("PTY ID is invalid".to_owned());
            }
            (reqwest::Method::PUT, format!("/pty/{id}"), Some(payload))
        }
        "pty_delete" => {
            if confirmed != Some(true) || !valid_runtime_identifier(&id, Some("pty")) {
                return Err("PTY deletion requires a valid ID and confirmation".to_owned());
            }
            (reqwest::Method::DELETE, format!("/pty/{id}"), None)
        }
        "worktree_create" => (
            reqwest::Method::POST,
            "/experimental/worktree".to_owned(),
            Some(payload),
        ),
        "worktree_delete" => {
            if confirmed != Some(true) {
                return Err("worktree deletion requires confirmation".to_owned());
            }
            (
                reqwest::Method::DELETE,
                "/experimental/worktree".to_owned(),
                Some(payload),
            )
        }
        "worktree_reset" => {
            if confirmed != Some(true) {
                return Err("worktree reset requires confirmation".to_owned());
            }
            (
                reqwest::Method::POST,
                "/experimental/worktree/reset".to_owned(),
                Some(payload),
            )
        }
        "session_command" => {
            if !valid_session_id(&session) {
                return Err("session ID is invalid".to_owned());
            }
            (
                reqwest::Method::POST,
                format!("/session/{session}/command"),
                Some(payload),
            )
        }
        "session_revert" | "session_unrevert" => {
            if !valid_session_id(&session) {
                return Err("session ID is invalid".to_owned());
            }
            let action = if kind == "session_revert" {
                "revert"
            } else {
                "unrevert"
            };
            (
                reqwest::Method::POST,
                format!("/session/{session}/{action}"),
                Some(payload),
            )
        }
        "project_git_init" => (
            reqwest::Method::POST,
            "/project/git/init".to_owned(),
            Some(payload),
        ),
        "vcs_apply" => {
            if confirmed != Some(true) {
                return Err("applying a VCS patch requires confirmation".to_owned());
            }
            (
                reqwest::Method::POST,
                "/vcs/apply".to_owned(),
                Some(payload),
            )
        }
        _ => return Err("unknown runtime operation".to_owned()),
    };
    state
        .runtime
        .lock()
        .await
        .request_json(method, &route, &query, body)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn runtime_answer(
    session_id: String,
    request_id: String,
    answer: Value,
    state: tauri::State<'_, DesktopState>,
) -> Result<(), String> {
    if !valid_session_id(&session_id) || request_id.trim().is_empty() {
        return Err("runtime answer identifiers are invalid".to_owned());
    }
    state
        .runtime
        .lock()
        .await
        .answer(&session_id, RuntimeAnswer { request_id, answer })
        .await
        .map_err(|error| error.to_string())
}

fn persist_domain_event(
    state: &DesktopState,
    event_type: &str,
    payload: &Value,
) -> Result<Value, String> {
    let event = personal_agent_contracts::proto::EventEnvelope::new(
        1,
        "desktop-ui",
        "default",
        event_type,
        payload,
    )
    .map_err(|error| error.to_string())?;
    let projection = state
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?
        .record_runtime_event(event)
        .map_err(|error| error.to_string())?;
    Ok(json!({"projection": projection, "record": payload}))
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)] // Tauri deserializes owned IPC arguments.
pub(crate) fn domain_action(
    domain: String,
    action: String,
    payload: Value,
    state: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    match (domain.as_str(), action.as_str()) {
        ("goal", "create") => {
            let objective = payload
                .get("objective")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "goal objective is required".to_owned())?;
            let criteria = payload
                .get("success_criteria")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if criteria.is_empty() {
                return Err("at least one observable success criterion is required".to_owned());
            }
            let mut goal = Goal::new(objective, criteria, "desktop-ui");
            goal.priority = payload
                .get("priority")
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
                .unwrap_or_default();
            persist_domain_event(&state, "goal.created", &json!(goal))
        }
        ("goal", "pause" | "resume" | "cancel" | "retry") => {
            let id = payload
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "goal ID is required".to_owned())?;
            persist_domain_event(&state, &format!("goal.{action}d"), &json!({"id": id}))
        }
        ("memory", "create") => {
            let content = payload
                .get("content")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "memory content is required".to_owned())?;
            persist_domain_event(
                &state,
                "memory.created",
                &json!({
                    "id": uuid::Uuid::now_v7(),
                    "content": content,
                    "tier": payload.get("tier").and_then(Value::as_str).unwrap_or("semantic"),
                    "trust": "explicit_user",
                    "confidence": 1.0,
                    "sensitivity": payload.get("sensitivity").and_then(Value::as_str).unwrap_or("private"),
                }),
            )
        }
        ("memory", "approve" | "reject" | "delete") => {
            persist_domain_event(&state, &format!("memory.{action}d"), &payload)
        }
        ("automation", "create") => {
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "automation name is required".to_owned())?;
            let prompt = payload
                .get("prompt")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "automation prompt is required".to_owned())?;
            let schedule = payload
                .get("schedule")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "automation schedule is required".to_owned())?;
            persist_domain_event(
                &state,
                "automation.created",
                &json!({
                    "id": uuid::Uuid::now_v7(), "name": name, "prompt": prompt,
                    "schedule": schedule, "enabled": true, "missed_run_policy": "run_once",
                }),
            )
        }
        ("automation", "enable" | "disable" | "delete" | "run") => {
            persist_domain_event(&state, &format!("automation.{action}d"), &payload)
        }
        ("artifact", "create") => {
            let title = payload
                .get("title")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "artifact title is required".to_owned())?;
            persist_domain_event(
                &state,
                "artifact.created",
                &json!({"id": uuid::Uuid::now_v7(), "title": title, "kind": payload.get("kind").and_then(Value::as_str).unwrap_or("text"), "version": 1}),
            )
        }
        ("project", "register") => {
            let path = payload
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| "project path is required".to_owned())?;
            let path = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
            if !path.is_dir() {
                return Err("project path is not a directory".to_owned());
            }
            persist_domain_event(
                &state,
                "project.registered",
                &json!({"id": uuid::Uuid::now_v7(), "path": path, "name": path.file_name().and_then(|value| value.to_str()).unwrap_or("Project")}),
            )
        }
        _ => Err("unknown domain action".to_owned()),
    }
}

fn valid_provider_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

fn valid_oauth_url(value: &str) -> bool {
    value.len() <= 8_192
        && (value.starts_with("https://")
            || value.starts_with("http://127.0.0.1:")
            || value.starts_with("http://localhost:"))
}

fn open_external_url(value: &str) -> Result<(), String> {
    if !valid_oauth_url(value) {
        return Err("provider returned an invalid authorization URL".to_owned());
    }
    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    command
        .arg(value)
        .spawn()
        .map_err(|error| format!("could not open the authorization page: {error}"))?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn provider_oauth_authorize(
    provider_id: String,
    method: u32,
    inputs: Option<BTreeMap<String, String>>,
    open_browser: bool,
    state: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    if !valid_provider_id(&provider_id) || method > 32 {
        return Err("provider or authentication method is invalid".to_owned());
    }
    let inputs = inputs.unwrap_or_default();
    if inputs.len() > 32
        || inputs.iter().any(|(key, value)| {
            key.is_empty()
                || key.len() > 128
                || !key.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
                })
                || value.len() > 4_096
        })
    {
        return Err("provider authentication inputs are invalid".to_owned());
    }
    let config = config_snapshot(&state)?;
    let directory = canonical_directory(&config, None)?.display().to_string();
    let authorization = state
        .runtime
        .lock()
        .await
        .request_json(
            reqwest::Method::POST,
            &format!("/provider/{provider_id}/oauth/authorize"),
            &[("directory", directory)],
            Some(json!({"method": method, "inputs": inputs})),
        )
        .await
        .map_err(|error| error.to_string())?;
    let url = authorization
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| "provider did not return an authorization URL".to_owned())?;
    if !valid_oauth_url(url) {
        return Err("provider returned an invalid authorization URL".to_owned());
    }
    if open_browser {
        open_external_url(url)?;
    }
    Ok(authorization)
}

#[tauri::command]
pub(crate) async fn provider_oauth_callback(
    provider_id: String,
    method: u32,
    code: Option<String>,
    state: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    if !valid_provider_id(&provider_id) || method > 32 {
        return Err("provider or authentication method is invalid".to_owned());
    }
    let code = code.map(|value| value.trim().to_owned());
    if code.as_ref().is_some_and(|value| value.len() > 16_384) {
        return Err("authorization code is too long".to_owned());
    }
    let config = config_snapshot(&state)?;
    let directory = canonical_directory(&config, None)?.display().to_string();
    state
        .runtime
        .lock()
        .await
        .request_json(
            reqwest::Method::POST,
            &format!("/provider/{provider_id}/oauth/callback"),
            &[("directory", directory)],
            Some(json!({"method": method, "code": code})),
        )
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn provider_set_key(
    provider_id: String,
    key: String,
    state: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    if !valid_provider_id(&provider_id) || key.trim().is_empty() || key.len() > 16_384 {
        return Err("provider ID or credential is invalid".to_owned());
    }
    let reference = SecretReference {
        service: "dev.personal-agent.provider".to_owned(),
        account: provider_id.clone(),
    };
    OsSecretStore
        .put(&reference, &SecretString::from(key.clone()))
        .map_err(|error| error.to_string())?;
    let result = state
        .runtime
        .lock()
        .await
        .request_json(
            reqwest::Method::PUT,
            &format!("/auth/{provider_id}"),
            &[],
            Some(json!({"type": "api", "key": key})),
        )
        .await
        .map_err(|error| error.to_string());
    if result.is_err() {
        let _ = OsSecretStore.delete(&reference);
    }
    result
}

#[tauri::command]
pub(crate) async fn provider_revoke(
    provider_id: String,
    state: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    if !valid_provider_id(&provider_id) {
        return Err("provider ID is invalid".to_owned());
    }
    let result = state
        .runtime
        .lock()
        .await
        .request_json(
            reqwest::Method::DELETE,
            &format!("/auth/{provider_id}"),
            &[],
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
    let reference = SecretReference {
        service: "dev.personal-agent.provider".to_owned(),
        account: provider_id,
    };
    let _ = OsSecretStore.delete(&reference);
    Ok(result)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri provides framework-owned state.
pub(crate) fn voice_status(
    state: tauri::State<'_, DesktopState>,
) -> Result<NativeVoiceStatus, String> {
    let config = config_snapshot(&state)?;
    Ok(voice_status_for(&state, &config))
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri deserializes owned IPC arguments.
pub(crate) fn microphone_state(
    active: bool,
    mode: String,
    state: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    let event = personal_agent_contracts::proto::EventEnvelope::new(
        1,
        "audio-capture",
        "default",
        "audio.privacy_state",
        &json!({"active": active, "mode": mode}),
    )
    .map_err(|error| error.to_string())?;
    let projection = state
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?
        .record_runtime_event(event)
        .map_err(|error| error.to_string())?;
    Ok(json!(projection))
}

#[tauri::command]
pub(crate) async fn voice_transcribe(
    samples: Vec<f32>,
    sample_rate_hz: u32,
    state: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    if samples.len()
        > usize::try_from(sample_rate_hz)
            .unwrap_or(0)
            .saturating_mul(600)
    {
        return Err("voice capture exceeds the ten-minute limit".to_owned());
    }
    let config = config_snapshot(&state)?;
    let status = voice_status_for(&state, &config);
    let executable = status
        .whisper_executable
        .ok_or_else(|| status.details.join(" "))?;
    let model = status
        .whisper_model
        .ok_or_else(|| status.details.join(" "))?;
    let transcript = transcribe_pcm(
        &executable,
        &model,
        &state.app_data.join("voice/runtime"),
        &samples,
        sample_rate_hz,
        &config.voice.language,
    )
    .await
    .map_err(|error| error.to_string())?;
    Ok(json!(transcript))
}

#[tauri::command]
pub(crate) async fn voice_speak(
    text: String,
    state: tauri::State<'_, DesktopState>,
) -> Result<Value, String> {
    if text.trim().is_empty() || text.len() > 65_536 {
        return Err("speech text must contain 1 to 65536 bytes".to_owned());
    }
    voice_stop_inner(&state).await;
    let config = config_snapshot(&state)?;
    if config.voice.quiet_mode {
        return Ok(json!({"spoken": false, "reason": "quiet mode"}));
    }
    let status = voice_status_for(&state, &config);
    let executable = status
        .piper_executable
        .ok_or_else(|| status.details.join(" "))?;
    let model = status.piper_model.ok_or_else(|| status.details.join(" "))?;
    let player = status
        .playback_command
        .ok_or_else(|| status.details.join(" "))?;
    let model_config = model.with_extension("onnx.json");
    let wav = synthesize_piper(
        &executable,
        &model,
        Some(&model_config),
        &state.app_data.join("voice/runtime"),
        &text,
        config.voice.speech_rate_percent,
    )
    .await
    .map_err(|error| error.to_string())?;
    let child = play_wav(
        &player,
        &wav,
        &config.voice.output_device,
        config.voice.volume_percent,
    )
    .map_err(|error| error.to_string())?;
    *state.voice_playback.lock().await = Some(VoicePlayback { child, wav });
    Ok(json!({"spoken": true}))
}

async fn voice_stop_inner(state: &DesktopState) {
    if let Some(mut playback) = state.voice_playback.lock().await.take() {
        let _ = playback.child.kill().await;
        let _ = playback.child.wait().await;
        let _ = std::fs::remove_file(playback.wav);
    }
}

#[tauri::command]
pub(crate) async fn voice_stop(state: tauri::State<'_, DesktopState>) -> Result<Value, String> {
    voice_stop_inner(&state).await;
    Ok(json!({"stopped": true}))
}

struct VoiceAsset {
    name: &'static str,
    url: &'static str,
    sha256: &'static str,
}

const WHISPER_LINUX_X64: VoiceAsset = VoiceAsset {
    name: "whisper-bin-ubuntu-x64.tar.gz",
    url: "https://github.com/ggml-org/whisper.cpp/releases/download/b4938/whisper-bin-ubuntu-x64.tar.gz",
    sha256: "f4cfc1f969a13805908fb72043ce7cc896eb42e0b8afbe841dc8e7298923b061",
};
const WHISPER_LINUX_ARM64: VoiceAsset = VoiceAsset {
    name: "whisper-bin-ubuntu-arm64.tar.gz",
    url: "https://github.com/ggml-org/whisper.cpp/releases/download/b4938/whisper-bin-ubuntu-arm64.tar.gz",
    sha256: "94a33318650c57cc3d9a91439e0e3f0b94ba96bacd34203a06db395cf9204e40",
};
const WHISPER_MODEL_BASE: VoiceAsset = VoiceAsset {
    name: "ggml-base.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
    sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
};
const PIPER_LINUX_X64: VoiceAsset = VoiceAsset {
    name: "piper_linux_x86_64.tar.gz",
    url: "https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_linux_x86_64.tar.gz",
    sha256: "a50cb45f355b7af1f6d758c1b360717877ba0a398cc8cbe6d2a7a3a26e225992",
};
const PIPER_VOICE: VoiceAsset = VoiceAsset {
    name: "en_US-lessac-medium.onnx",
    url: "https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/en/en_US/lessac/medium/en_US-lessac-medium.onnx",
    sha256: "5efe09e69902187827af646e1a6e9d269dee769f9877d17b16b1b46eeaaf019f",
};
const PIPER_VOICE_CONFIG: VoiceAsset = VoiceAsset {
    name: "en_US-lessac-medium.onnx.json",
    url: "https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/en/en_US/lessac/medium/en_US-lessac-medium.onnx.json",
    sha256: "efe19c417bed055f2d69908248c6ba650fa135bc868b0e6abb3da181dab690a0",
};

async fn download_voice_asset(
    asset: &VoiceAsset,
    destination: &Path,
    app: &tauri::AppHandle,
) -> Result<(), String> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let partial = destination.with_extension(format!("partial-{}", uuid::Uuid::new_v4()));
    let mut response = reqwest::Client::new()
        .get(asset.url)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let total = response.content_length();
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&partial)
        .map_err(|error| error.to_string())?;
    let mut digest = Sha256::new();
    let mut downloaded = 0_u64;
    let result = async {
        while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
            file.write_all(&chunk).map_err(|error| error.to_string())?;
            digest.update(&chunk);
            downloaded = downloaded.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            let _ = app.emit(
                "voice-install-progress",
                json!({"asset": asset.name, "downloaded": downloaded, "total": total}),
            );
        }
        file.sync_all().map_err(|error| error.to_string())?;
        let mut found = String::with_capacity(64);
        for byte in digest.finalize() {
            write!(&mut found, "{byte:02x}").expect("writing to String cannot fail");
        }
        if found != asset.sha256 {
            return Err(format!(
                "voice asset digest mismatch for {}: expected {}, found {}",
                asset.name, asset.sha256, found
            ));
        }
        std::fs::rename(&partial, destination).map_err(|error| error.to_string())
    }
    .await;
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

fn promote_directory(staged: &Path, destination: &Path) -> Result<(), String> {
    let previous = destination.with_extension(format!("previous-{}", uuid::Uuid::new_v4()));
    if destination.exists() {
        std::fs::rename(destination, &previous).map_err(|error| error.to_string())?;
    }
    if let Err(error) = std::fs::rename(staged, destination) {
        if previous.exists() {
            let _ = std::fs::rename(&previous, destination);
        }
        return Err(error.to_string());
    }
    Ok(())
}

async fn install_tar_asset(
    asset: &VoiceAsset,
    root: &Path,
    destination: &Path,
    strip_components: bool,
    nested: Option<&str>,
    app: &tauri::AppHandle,
) -> Result<(), String> {
    let archive = root.join("downloads").join(asset.name);
    download_voice_asset(asset, &archive, app).await?;
    let staging = root.join(format!(".staging-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
    let mut command = Command::new("tar");
    command.args(["-xzf"]).arg(&archive).arg("-C").arg(&staging);
    if strip_components {
        command.arg("--strip-components=1");
    }
    let output = command.output().await.map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "voice archive extraction failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let promoted = nested.map_or_else(|| staging.clone(), |name| staging.join(name));
    if !promoted.is_dir() {
        return Err("voice archive did not contain the expected directory".to_owned());
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    promote_directory(&promoted, destination)?;
    let _ = std::fs::remove_file(archive);
    if staging.exists() {
        let _ = std::fs::remove_dir_all(staging);
    }
    Ok(())
}

async fn install_whisper_engine(root: &Path, app: &tauri::AppHandle) -> Result<(), String> {
    let asset = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => &WHISPER_LINUX_X64,
        ("linux", "aarch64") => &WHISPER_LINUX_ARM64,
        _ => return Err("automatic Whisper installation is not available for this platform; select a custom executable in Voice settings".to_owned()),
    };
    install_tar_asset(asset, root, &root.join("whisper/bin"), true, None, app).await
}

async fn install_piper_engine(root: &Path, app: &tauri::AppHandle) -> Result<(), String> {
    if (std::env::consts::OS, std::env::consts::ARCH) != ("linux", "x86_64") {
        return Err("automatic Piper installation is not available for this platform; select a custom executable in Voice settings".to_owned());
    }
    install_tar_asset(
        &PIPER_LINUX_X64,
        root,
        &root.join("piper"),
        false,
        Some("piper"),
        app,
    )
    .await
}

async fn install_piper_voice(root: &Path, app: &tauri::AppHandle) -> Result<(), String> {
    let voices = root.join("piper/voices");
    download_voice_asset(&PIPER_VOICE, &voices.join(PIPER_VOICE.name), app).await?;
    download_voice_asset(
        &PIPER_VOICE_CONFIG,
        &voices.join(PIPER_VOICE_CONFIG.name),
        app,
    )
    .await
}

#[tauri::command]
pub(crate) async fn voice_install(
    component: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
) -> Result<NativeVoiceStatus, String> {
    let root = state.app_data.join("voice");
    match component.as_str() {
        "whisper" => install_whisper_engine(&root, &app).await?,
        "whisper-model" => {
            download_voice_asset(
                &WHISPER_MODEL_BASE,
                &root.join("whisper/models/ggml-base.bin"),
                &app,
            )
            .await?;
        }
        "piper" => install_piper_engine(&root, &app).await?,
        "piper-voice" => install_piper_voice(&root, &app).await?,
        "all" => {
            install_whisper_engine(&root, &app).await?;
            download_voice_asset(
                &WHISPER_MODEL_BASE,
                &root.join("whisper/models/ggml-base.bin"),
                &app,
            )
            .await?;
            install_piper_engine(&root, &app).await?;
            install_piper_voice(&root, &app).await?;
        }
        _ => return Err("unknown voice component".to_owned()),
    }
    let config = config_snapshot(&state)?;
    let status = voice_status_for(&state, &config);
    app.emit("voice-install-complete", &status)
        .map_err(|error| error.to_string())?;
    Ok(status)
}
