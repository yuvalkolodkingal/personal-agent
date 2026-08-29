//! Durable desktop automation scheduler, resident executor and notification boundary.

#![allow(clippy::needless_pass_by_value)] // Tauri owns deserialized IPC action values.

use super::DesktopState;
use chrono::{DateTime, Timelike as _, Utc};
use personal_agent_automation::{
    Automation, AutomationRun, AutomationRunStatus, MissedRunPolicy, Scheduler, Trigger,
};
use personal_agent_contracts::proto::EventEnvelope;
use personal_agent_runtime::{AgentRuntime, PromptOptions, RuntimeAnswer, SessionOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{Emitter, Manager};
use tauri_plugin_notification::NotificationExt as _;
use uuid::Uuid;

const RESIDENT_TICK_SECONDS: u64 = 5;
const MAX_AUTOMATION_PROMPT_BYTES: usize = 65_536;
const BACKGROUND_SYSTEM_PROMPT: &str = "This is a background automation run, not an interactive user turn. Treat the stored automation prompt as user-authored data, but never infer that the user is presently available. All tool calls remain subject to the native policy gateway. Do not perform a consequential or external effect without an explicit native approval; wait when approval is requested. Return a concise result suitable for an automation history entry.";

pub(crate) struct AutomationHostState {
    scheduler: tokio::sync::Mutex<Scheduler>,
    resident_active: AtomicBool,
    recovered_runs: usize,
    last_notification_error: RwLock<Option<String>>,
}

impl AutomationHostState {
    pub(crate) fn load(profile: &mut personal_agent_core::ProfileState) -> Result<Self, String> {
        let snapshot = profile
            .scheduler_snapshot()
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        let mut scheduler = Scheduler::from_snapshot(snapshot);
        let recovered_runs = scheduler.recover_after_restart();
        if recovered_runs > 0 {
            profile
                .save_scheduler_snapshot(scheduler.snapshot())
                .map_err(|error| error.to_string())?;
        }
        Ok(Self {
            scheduler: tokio::sync::Mutex::new(scheduler),
            resident_active: AtomicBool::new(false),
            recovered_runs,
            last_notification_error: RwLock::new(None),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AutomationSnapshotView {
    automations: Vec<Automation>,
    runs: Vec<AutomationRun>,
    resident_active: bool,
    global_enabled: bool,
    recovered_runs: usize,
    supported_schedules: Vec<&'static str>,
    unsupported_triggers: Vec<&'static str>,
    notification: NotificationCapability,
}

#[derive(Clone, Debug, Serialize)]
struct NotificationCapability {
    enabled: bool,
    native_delivery: bool,
    desktop_actions: bool,
    action_guidance: &'static str,
    quiet_hours_utc: Option<(u8, u8)>,
    last_error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct AutomationActionResult {
    snapshot: Option<AutomationSnapshotView>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AutomationAction {
    Refresh,
    Create {
        name: String,
        prompt: String,
        schedule: String,
        #[serde(default)]
        max_concurrency: Option<u8>,
        #[serde(default)]
        pause_after_failures: Option<u8>,
        #[serde(default)]
        missed_run_policy: Option<MissedRunPolicy>,
        #[serde(default)]
        maximum_catch_up_runs: Option<u8>,
        #[serde(default)]
        notification_route: Option<String>,
    },
    SetEnabled {
        automation_id: Uuid,
        enabled: bool,
    },
    RunNow {
        automation_id: Uuid,
    },
    Delete {
        automation_id: Uuid,
        confirmed: bool,
    },
    AnswerApproval {
        schedule_key: String,
        allow: bool,
    },
}

#[tauri::command]
pub(crate) async fn automation_snapshot(
    host: tauri::State<'_, AutomationHostState>,
    desktop: tauri::State<'_, DesktopState>,
) -> Result<AutomationSnapshotView, String> {
    snapshot_view(&host, &desktop).await
}

#[tauri::command]
#[allow(clippy::too_many_lines)] // One tagged dispatcher keeps IPC state transitions auditable.
pub(crate) async fn automation_execute(
    action: Value,
    app: tauri::AppHandle,
    host: tauri::State<'_, AutomationHostState>,
    desktop: tauri::State<'_, DesktopState>,
) -> Result<AutomationActionResult, String> {
    let action: AutomationAction = serde_json::from_value(action)
        .map_err(|error| format!("invalid automation action: {error}"))?;
    let mut message = None;
    match action {
        AutomationAction::Refresh => {}
        AutomationAction::Create {
            name,
            prompt,
            schedule,
            max_concurrency,
            pause_after_failures,
            missed_run_policy,
            maximum_catch_up_runs,
            notification_route,
        } => {
            let name = bounded_text(&name, "automation name", 256)?;
            let prompt = bounded_text(&prompt, "automation prompt", MAX_AUTOMATION_PROMPT_BYTES)?;
            let now = Utc::now();
            let (trigger, next_due_at) = parse_schedule(&schedule, now)?;
            let config = desktop
                .config
                .read()
                .map_err(|_| "configuration lock is poisoned".to_owned())?
                .clone();
            let automation = Automation {
                id: Uuid::now_v7(),
                name: name.clone(),
                goal_template: prompt,
                trigger,
                enabled: true,
                max_concurrency: max_concurrency.unwrap_or(1),
                missed_run_policy: missed_run_policy.unwrap_or_else(|| {
                    missed_policy_from_config(&config.automation.missed_run_policy)
                }),
                consecutive_failures: 0,
                pause_after_failures: pause_after_failures
                    .unwrap_or(config.automation.pause_after_failures),
                previous_state: None,
                next_due_at,
                maximum_catch_up_runs: maximum_catch_up_runs.unwrap_or(3),
                quiet_hours_utc: quiet_hours_from_config(
                    &config.automation.quiet_hours_start,
                    &config.automation.quiet_hours_end,
                )?,
                notification_route: notification_route
                    .filter(|route| !route.trim().is_empty())
                    .unwrap_or_else(|| "desktop".into()),
            };
            mutate_scheduler(&host, &desktop, |scheduler| {
                scheduler
                    .upsert(automation.clone())
                    .map_err(|error| error.to_string())
            })
            .await?;
            record_activity(
                &desktop,
                "automation.created",
                &json!({"id": automation.id, "name": automation.name, "trigger": automation.trigger, "enabled": true}),
            )?;
            message = Some(format!("Automation “{name}” is scheduled and persisted."));
        }
        AutomationAction::SetEnabled {
            automation_id,
            enabled,
        } => {
            mutate_scheduler(&host, &desktop, |scheduler| {
                scheduler
                    .set_enabled(automation_id, enabled)
                    .map_err(|error| error.to_string())
            })
            .await?;
            record_activity(
                &desktop,
                if enabled {
                    "automation.enabled"
                } else {
                    "automation.disabled"
                },
                &json!({"id": automation_id}),
            )?;
            message = Some(if enabled {
                "Automation enabled.".into()
            } else {
                "Automation disabled; its history was retained.".into()
            });
        }
        AutomationAction::RunNow { automation_id } => {
            if !desktop
                .config
                .read()
                .map_err(|_| "configuration lock is poisoned".to_owned())?
                .automation
                .enabled
            {
                return Err("Automations are globally disabled in Settings".into());
            }
            let key = mutate_scheduler(&host, &desktop, |scheduler| {
                scheduler
                    .run_now(automation_id, Utc::now())
                    .map_err(|error| error.to_string())
            })
            .await?;
            record_activity(
                &desktop,
                "automation.run_requested",
                &json!({"id": automation_id, "schedule_key": key}),
            )?;
            drain_queued(&app).await?;
            message = Some("Automation run queued in the resident executor.".into());
        }
        AutomationAction::Delete {
            automation_id,
            confirmed,
        } => {
            if !confirmed {
                return Err(
                    "deleting an automation and its run history requires confirmation".into(),
                );
            }
            mutate_scheduler(&host, &desktop, |scheduler| {
                scheduler
                    .remove(automation_id)
                    .map_err(|error| error.to_string())
            })
            .await?;
            record_activity(
                &desktop,
                "automation.deleted",
                &json!({"id": automation_id}),
            )?;
            message = Some("Automation and its run history deleted.".into());
        }
        AutomationAction::AnswerApproval {
            schedule_key,
            allow,
        } => {
            let run = {
                let scheduler = host.scheduler.lock().await;
                scheduler
                    .snapshot()
                    .runs
                    .get(&schedule_key)
                    .cloned()
                    .ok_or_else(|| "automation run does not exist".to_owned())?
            };
            if run.status != AutomationRunStatus::WaitingApproval {
                return Err("automation run is not waiting for approval".into());
            }
            let session_id = run
                .runtime_session_id
                .ok_or_else(|| "the runtime session ended; run the automation again".to_owned())?;
            let request_id = run
                .runtime_request_id
                .ok_or_else(|| "the approval request is no longer available".to_owned())?;
            desktop
                .runtime
                .lock()
                .await
                .answer(
                    &session_id,
                    RuntimeAnswer {
                        request_id,
                        answer: json!({"kind":"permission", "reply": if allow { "once" } else { "reject" }}),
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            mutate_scheduler(&host, &desktop, |scheduler| {
                scheduler
                    .resume_after_approval(&schedule_key)
                    .map_err(|error| error.to_string())
            })
            .await?;
            record_activity(
                &desktop,
                if allow {
                    "automation.approval_allowed"
                } else {
                    "automation.approval_rejected"
                },
                &json!({"schedule_key": schedule_key}),
            )?;
            message = Some(if allow {
                "Allowed once. The existing automation turn is resuming.".into()
            } else {
                "Approval rejected. The automation turn is finishing safely.".into()
            });
        }
    }
    let snapshot = snapshot_view(&host, &desktop).await?;
    let _ = app.emit("automation://changed", snapshot.clone());
    Ok(AutomationActionResult {
        snapshot: Some(snapshot),
        message,
    })
}

pub(crate) fn ensure_resident_executor(app: tauri::AppHandle) {
    let host = app.state::<AutomationHostState>();
    if host.resident_active.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(RESIDENT_TICK_SECONDS));
        loop {
            timer.tick().await;
            if let Err(error) = resident_tick(&app).await {
                tracing::warn!(%error, "resident automation tick failed");
            }
        }
    });
}

async fn resident_tick(app: &tauri::AppHandle) -> Result<(), String> {
    let host = app.state::<AutomationHostState>();
    let desktop = app.state::<DesktopState>();
    if !desktop
        .config
        .read()
        .map_err(|_| "configuration lock is poisoned".to_owned())?
        .automation
        .enabled
    {
        return Ok(());
    }
    let queued = mutate_scheduler(&host, &desktop, |scheduler| {
        scheduler
            .evaluate(Utc::now())
            .map_err(|error| error.to_string())
    })
    .await?;
    if !queued.is_empty() {
        let snapshot = snapshot_view(&host, &desktop).await?;
        let _ = app.emit("automation://changed", snapshot);
    }
    drain_queued(app).await
}

async fn drain_queued(app: &tauri::AppHandle) -> Result<(), String> {
    let host = app.state::<AutomationHostState>();
    let desktop = app.state::<DesktopState>();
    let maximum = usize::from(
        desktop
            .config
            .read()
            .map_err(|_| "configuration lock is poisoned".to_owned())?
            .automation
            .max_concurrency,
    );
    let keys = {
        let scheduler = host.scheduler.lock().await;
        let active = scheduler
            .snapshot()
            .runs
            .values()
            .filter(|run| {
                matches!(
                    run.status,
                    AutomationRunStatus::Running | AutomationRunStatus::WaitingApproval
                )
            })
            .count();
        scheduler
            .snapshot()
            .runs
            .values()
            .filter(|run| run.status == AutomationRunStatus::Queued)
            .take(maximum.saturating_sub(active))
            .map(|run| run.schedule_key.clone())
            .collect::<Vec<_>>()
    };
    for key in keys {
        mutate_scheduler(&host, &desktop, |scheduler| {
            scheduler.start(&key).map_err(|error| error.to_string())
        })
        .await?;
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = run_automation(&app, &key).await {
                tracing::warn!(schedule_key = %key, %error, "automation execution failed");
                let host = app.state::<AutomationHostState>();
                let desktop = app.state::<DesktopState>();
                let _ = mutate_scheduler(&host, &desktop, |scheduler| {
                    scheduler
                        .fail_active(&key, &error)
                        .map_err(|scheduler_error| scheduler_error.to_string())
                })
                .await;
                let _ = emit_snapshot(&app).await;
                notify_run(&app, &key, NotificationKind::Failure).await;
            }
        });
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn run_automation(app: &tauri::AppHandle, key: &str) -> Result<(), String> {
    let host = app.state::<AutomationHostState>();
    let desktop = app.state::<DesktopState>();
    let (automation, run_id) = {
        let scheduler = host.scheduler.lock().await;
        let run = scheduler
            .snapshot()
            .runs
            .get(key)
            .ok_or_else(|| "automation run disappeared".to_owned())?;
        let automation = scheduler
            .snapshot()
            .automations
            .get(&run.automation_id)
            .cloned()
            .ok_or_else(|| "automation definition disappeared".to_owned())?;
        (automation, run.id)
    };
    let config = desktop
        .config
        .read()
        .map_err(|_| "configuration lock is poisoned".to_owned())?
        .clone();
    let directory = PathBuf::from(&config.runtime.working_directory);
    if !directory.is_dir() {
        return Err("automation working directory is unavailable".into());
    }
    let requested_model = (!config.runtime.default_model.trim().is_empty()).then(|| {
        if config.runtime.default_model.contains('/') {
            config.runtime.default_model.clone()
        } else {
            format!(
                "{}/{}",
                config.runtime.default_provider, config.runtime.default_model
            )
        }
    });
    let requested_agent = (!config.runtime.default_agent.trim().is_empty())
        .then(|| config.runtime.default_agent.clone());
    let requested_effort = (!config.runtime.default_effort.trim().is_empty())
        .then(|| config.runtime.default_effort.clone());
    let (session_id, submission, api_client) = {
        let mut runtime = desktop.runtime.lock().await;
        let session_id = runtime
            .begin_session(SessionOptions {
                model: requested_model.clone(),
                effort: requested_effort.clone(),
                agent: requested_agent.clone(),
                working_directory: directory.clone(),
                environment: BTreeMap::new(),
            })
            .await
            .map_err(|error| error.to_string())?;
        let submission = runtime
            .submit_with_attachments(
                &session_id,
                &automation.goal_template,
                Vec::new(),
                PromptOptions {
                    model: requested_model.as_deref(),
                    agent: requested_agent.as_deref(),
                    effort: requested_effort.as_deref(),
                    system: Some(BACKGROUND_SYSTEM_PROMPT),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        let api_client = runtime.api_client().map_err(|error| error.to_string())?;
        (session_id, submission, api_client)
    };
    mutate_scheduler(&host, &desktop, |scheduler| {
        scheduler
            .bind_runtime_session(key, &session_id)
            .map_err(|error| error.to_string())
    })
    .await?;
    record_activity(
        &desktop,
        "automation.run_started",
        &json!({"id": automation.id, "schedule_key": key, "session_id": session_id}),
    )?;
    emit_snapshot(app).await?;

    let prompt_message_id = submission.message_id;
    let mut events = submission.events;
    let deadline_seconds = if config.agent.default_wall_time_minutes > 0 {
        u64::from(config.agent.default_wall_time_minutes).saturating_mul(60)
    } else {
        30 * 60
    };
    let deadline = tokio::time::sleep(Duration::from_secs(deadline_seconds));
    tokio::pin!(deadline);
    let mut response = String::new();
    let mut success = false;
    let mut failure = None;
    loop {
        let mut event = tokio::select! {
            event = events.recv() => {
                let Some(event) = event else {
                    failure = Some("The automation event stream ended before completion.".to_owned());
                    break;
                };
                event
            },
            () = &mut deadline => {
                let _ = desktop.runtime.lock().await.abort_session(&session_id).await;
                failure = Some(format!(
                    "The automation exceeded its {deadline_seconds} second wall-time budget."
                ));
                break;
            }
        };
        event.agent_id = Some(format!("automation:{}", automation.id));
        event.task_id = Some(run_id.to_string());
        let budget = {
            let mut profile = desktop
                .profile
                .lock()
                .map_err(|_| "profile state lock is poisoned".to_owned())?;
            profile
                .record_runtime_event(event.clone())
                .map_err(|error| error.to_string())?;
            profile
                .usage_snapshot()
                .map_err(|error| error.to_string())?
                .check_budget(
                    &format!("automation:{}", automation.id),
                    configured_budget_limits(&config),
                )
        };
        if budget.exceeded() {
            let _ = desktop
                .runtime
                .lock()
                .await
                .abort_session(&session_id)
                .await;
            failure = Some(format!("Automation stopped because its {budget}."));
            break;
        }
        let _ = app.emit("automation-runtime-event", &event);
        if event.r#type == "response.delta"
            && let Ok(payload) = event.payload()
            && let Some(delta) = payload
                .get("delta")
                .or_else(|| payload.get("text"))
                .and_then(Value::as_str)
        {
            response.push_str(delta);
        }
        if event.r#type == "approval.requested" {
            let payload = event.payload().map_err(|error| error.to_string())?;
            let request_id = payload
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| "runtime approval omitted its request ID".to_owned())?;
            let reason = payload
                .get("permission")
                .or_else(|| payload.get("action"))
                .and_then(Value::as_str)
                .unwrap_or("Consequential background action");
            mutate_scheduler(&host, &desktop, |scheduler| {
                scheduler
                    .bind_approval(key, &session_id, request_id, reason)
                    .map_err(|error| error.to_string())
            })
            .await?;
            record_activity(
                &desktop,
                "automation.approval_requested",
                &json!({"id": automation.id, "schedule_key": key, "reason": reason}),
            )?;
            emit_snapshot(app).await?;
            notify_run(app, key, NotificationKind::Approval).await;
            continue;
        }
        if event.r#type == "clarification.requested" {
            let _ = desktop
                .runtime
                .lock()
                .await
                .abort_session(&session_id)
                .await;
            failure = Some(
                "Background clarification is not yet resumable. Run this automation interactively."
                    .into(),
            );
            break;
        }
        if event.r#type == "response.failed" {
            failure = event.payload().ok().and_then(|payload| {
                payload
                    .pointer("/error/data/message")
                    .or_else(|| payload.pointer("/error/message"))
                    .or_else(|| payload.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            break;
        }
        if matches!(
            event.r#type.as_str(),
            "runtime.stream_error" | "runtime.stream_closed"
        ) {
            failure = Some("The automation runtime stream disconnected.".into());
            break;
        }
        if event.r#type == "response.completed" {
            success = true;
            break;
        }
    }
    if success
        && let Ok(messages) = api_client
            .request_json(
                reqwest::Method::GET,
                &format!("/session/{session_id}/message"),
                &[("directory", directory.display().to_string())],
                None,
            )
            .await
        && let Some(final_text) = assistant_text(&messages, &prompt_message_id)
    {
        response = final_text;
    }
    let summary = if success {
        if response.trim().is_empty() {
            "Automation completed without a text result.".into()
        } else {
            response
        }
    } else {
        failure.unwrap_or_else(|| "Automation failed without a diagnostic.".into())
    };
    mutate_scheduler(&host, &desktop, |scheduler| {
        let status = scheduler
            .snapshot()
            .runs
            .get(key)
            .map(|run| run.status)
            .ok_or_else(|| "automation run disappeared".to_owned())?;
        if status == AutomationRunStatus::WaitingApproval {
            scheduler
                .fail_active(key, &summary)
                .map_err(|error| error.to_string())
        } else {
            scheduler
                .finish(key, success, None)
                .map_err(|error| error.to_string())?;
            scheduler
                .set_result_summary(key, &summary)
                .map_err(|error| error.to_string())
        }
    })
    .await?;
    record_activity(
        &desktop,
        if success {
            "automation.run_completed"
        } else {
            "automation.run_failed"
        },
        &json!({"id": automation.id, "schedule_key": key}),
    )?;
    emit_snapshot(app).await?;
    notify_run(
        app,
        key,
        if success {
            NotificationKind::Completion
        } else {
            NotificationKind::Failure
        },
    )
    .await;
    Ok(())
}

fn assistant_text(messages: &Value, prompt_message_id: &str) -> Option<String> {
    messages.as_array()?.iter().rev().find_map(|message| {
        if message.pointer("/info/role").and_then(Value::as_str) != Some("assistant")
            || message.pointer("/info/parentID").and_then(Value::as_str) != Some(prompt_message_id)
        {
            return None;
        }
        let text = message
            .get("parts")?
            .as_array()?
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<String>();
        (!text.trim().is_empty()).then_some(text)
    })
}

async fn mutate_scheduler<T>(
    host: &AutomationHostState,
    desktop: &DesktopState,
    operation: impl FnOnce(&mut Scheduler) -> Result<T, String>,
) -> Result<T, String> {
    let mut scheduler = host.scheduler.lock().await;
    let mut candidate = scheduler.clone();
    let result = operation(&mut candidate)?;
    desktop
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?
        .save_scheduler_snapshot(candidate.snapshot())
        .map_err(|error| error.to_string())?;
    *scheduler = candidate;
    Ok(result)
}

async fn snapshot_view(
    host: &AutomationHostState,
    desktop: &DesktopState,
) -> Result<AutomationSnapshotView, String> {
    let config = desktop
        .config
        .read()
        .map_err(|_| "configuration lock is poisoned".to_owned())?
        .clone();
    let snapshot = host.scheduler.lock().await.snapshot().clone();
    let mut automations = snapshot.automations.into_values().collect::<Vec<_>>();
    automations.sort_by(|left, right| left.name.cmp(&right.name));
    let mut runs = snapshot.runs.into_values().collect::<Vec<_>>();
    runs.sort_by_key(|run| std::cmp::Reverse(run.scheduled_for));
    let quiet_hours_utc = quiet_hours_from_config(
        &config.automation.quiet_hours_start,
        &config.automation.quiet_hours_end,
    )
    .ok()
    .flatten();
    Ok(AutomationSnapshotView {
        automations,
        runs,
        resident_active: host.resident_active.load(Ordering::SeqCst),
        global_enabled: config.automation.enabled,
        recovered_runs: host.recovered_runs,
        supported_schedules: vec![
            "daily at HH:MM (UTC)",
            "every N seconds/minutes/hours",
            "*/N * * * *",
            "RFC 3339 timestamp",
            "now",
        ],
        unsupported_triggers: vec![
            "file/directory watchers",
            "calendar/email events",
            "webhooks",
            "network/device changes",
            "semantic monitors",
        ],
        notification: NotificationCapability {
            enabled: config.notifications.enabled,
            native_delivery: true,
            desktop_actions: false,
            action_guidance: "Desktop notification buttons are not provided by Tauri; approve or reject inside Personal Agent.",
            quiet_hours_utc,
            last_error: host
                .last_notification_error
                .read()
                .map_err(|_| "notification status lock is poisoned".to_owned())?
                .clone(),
        },
    })
}

async fn emit_snapshot(app: &tauri::AppHandle) -> Result<(), String> {
    let host = app.state::<AutomationHostState>();
    let desktop = app.state::<DesktopState>();
    let snapshot = snapshot_view(&host, &desktop).await?;
    app.emit("automation://changed", snapshot)
        .map_err(|error| error.to_string())
}

#[derive(Clone, Copy)]
enum NotificationKind {
    Completion,
    Approval,
    Failure,
}

async fn notify_run(app: &tauri::AppHandle, key: &str, kind: NotificationKind) {
    let host = app.state::<AutomationHostState>();
    let desktop = app.state::<DesktopState>();
    let config = match desktop.config.read() {
        Ok(config) => config.clone(),
        Err(_) => return,
    };
    let (automation, run) = {
        let scheduler = host.scheduler.lock().await;
        let Some(run) = scheduler.snapshot().runs.get(key).cloned() else {
            return;
        };
        let Some(automation) = scheduler
            .snapshot()
            .automations
            .get(&run.automation_id)
            .cloned()
        else {
            return;
        };
        (automation, run)
    };
    let category_enabled = match kind {
        NotificationKind::Completion => config.notifications.task_completion,
        NotificationKind::Approval => config.notifications.approvals,
        NotificationKind::Failure => config.notifications.failures,
    };
    if !config.notifications.enabled
        || !category_enabled
        || automation.notification_route != "desktop"
        || is_quiet_hour(Utc::now(), automation.quiet_hours_utc)
    {
        return;
    }
    let (title, body) = match kind {
        NotificationKind::Completion => (
            format!("{} completed", automation.name),
            "Open Personal Agent to review the automation result.".to_owned(),
        ),
        NotificationKind::Approval => (
            format!("{} needs approval", automation.name),
            run.approval_reason
                .unwrap_or_else(|| "A background action is waiting for review.".into()),
        ),
        NotificationKind::Failure => (
            format!("{} failed", automation.name),
            "Open Personal Agent for the sanitized failure and retry controls.".to_owned(),
        ),
    };
    let result = app.notification().builder().title(title).body(body).show();
    if let Ok(mut error) = host.last_notification_error.write() {
        *error = result.err().map(|value| value.to_string());
    }
    let _ = emit_snapshot(app).await;
}

fn record_activity(
    desktop: &DesktopState,
    event_type: &str,
    payload: &Value,
) -> Result<(), String> {
    let event = EventEnvelope::new(1, "automation-host", "default", event_type, payload)
        .map_err(|error| error.to_string())?;
    desktop
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?
        .record_runtime_event(event)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn bounded_text(value: &str, label: &str, maximum: usize) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > maximum || value.contains('\0') {
        return Err(format!(
            "{label} must be non-empty and at most {maximum} bytes"
        ));
    }
    Ok(value.into())
}

fn configured_budget_limits(
    config: &personal_agent_core::PersonalAgentConfig,
) -> personal_agent_core::BudgetLimits {
    personal_agent_core::BudgetLimits {
        tokens: (config.agent.default_token_budget > 0)
            .then_some(config.agent.default_token_budget),
        cost_microusd: (config.agent.default_cost_budget_microusd > 0)
            .then_some(config.agent.default_cost_budget_microusd),
        tool_calls: (config.agent.default_tool_call_budget > 0)
            .then_some(config.agent.default_tool_call_budget),
    }
}

fn missed_policy_from_config(value: &str) -> MissedRunPolicy {
    match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "skip" => MissedRunPolicy::Skip,
        "catch-up-bounded" | "catchup-bounded" => MissedRunPolicy::CatchUpBounded,
        _ => MissedRunPolicy::RunOnce,
    }
}

fn parse_schedule(
    schedule: &str,
    now: DateTime<Utc>,
) -> Result<(Trigger, Option<DateTime<Utc>>), String> {
    let schedule = schedule.trim();
    if schedule.eq_ignore_ascii_case("now") {
        return Ok((Trigger::Once { at: now }, Some(now)));
    }
    if let Ok(at) = DateTime::parse_from_rfc3339(schedule) {
        let at = at.with_timezone(&Utc);
        if at < now {
            return Err("one-time automation timestamp is in the past".into());
        }
        return Ok((Trigger::Once { at }, Some(at)));
    }
    let words = schedule.to_ascii_lowercase();
    if let Some(time) = words.strip_prefix("daily at ") {
        let (hour, minute) = parse_clock(time)?;
        let mut at = now
            .date_naive()
            .and_hms_opt(u32::from(hour), u32::from(minute), 0)
            .ok_or_else(|| "daily schedule time is invalid".to_owned())?
            .and_utc();
        if at <= now {
            at += chrono::Duration::days(1);
        }
        return Ok((Trigger::Interval { seconds: 86_400 }, Some(at)));
    }
    if let Some(rest) = words.strip_prefix("every ") {
        let parts = rest.split_whitespace().collect::<Vec<_>>();
        if parts.len() == 2 {
            let amount = parts[0]
                .parse::<u64>()
                .ok()
                .filter(|amount| *amount > 0)
                .ok_or_else(|| "interval amount must be a positive integer".to_owned())?;
            let multiplier = match parts[1].trim_end_matches('s') {
                "second" => 1,
                "minute" => 60,
                "hour" => 3_600,
                _ => return Err("interval unit must be seconds, minutes, or hours".into()),
            };
            let seconds = amount
                .checked_mul(multiplier)
                .ok_or_else(|| "automation interval is too large".to_owned())?;
            let due = now
                .checked_add_signed(chrono::Duration::seconds(
                    i64::try_from(seconds).unwrap_or(i64::MAX),
                ))
                .ok_or_else(|| "automation interval is too large".to_owned())?;
            return Ok((Trigger::Interval { seconds }, Some(due)));
        }
    }
    let fields = schedule.split_whitespace().collect::<Vec<_>>();
    if fields.len() == 5 && fields[1..] == ["*", "*", "*", "*"] {
        let minutes = fields[0]
            .strip_prefix("*/")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| "cron minute interval must be */N".to_owned())?;
        let seconds = minutes
            .checked_mul(60)
            .ok_or_else(|| "cron interval is too large".to_owned())?;
        let due = now
            .checked_add_signed(chrono::Duration::seconds(
                i64::try_from(seconds).unwrap_or(i64::MAX),
            ))
            .ok_or_else(|| "cron interval is too large".to_owned())?;
        return Ok((
            Trigger::Cron {
                expression: schedule.into(),
            },
            Some(due),
        ));
    }
    Err("unsupported schedule; use daily at HH:MM, every N minutes, */N * * * *, an RFC 3339 timestamp, or now".into())
}

fn parse_clock(value: &str) -> Result<(u8, u8), String> {
    let (hour, minute) = value
        .split_once(':')
        .ok_or_else(|| "time must use HH:MM".to_owned())?;
    let hour = hour
        .parse::<u8>()
        .ok()
        .filter(|hour| *hour < 24)
        .ok_or_else(|| "hour must be between 00 and 23".to_owned())?;
    let minute = minute
        .parse::<u8>()
        .ok()
        .filter(|minute| *minute < 60)
        .ok_or_else(|| "minute must be between 00 and 59".to_owned())?;
    Ok((hour, minute))
}

fn quiet_hours_from_config(start: &str, end: &str) -> Result<Option<(u8, u8)>, String> {
    if start.trim().is_empty() && end.trim().is_empty() {
        return Ok(None);
    }
    if start.trim().is_empty() || end.trim().is_empty() {
        return Err("quiet hours require both a start and end time".into());
    }
    let (start_hour, _) = parse_clock(start)?;
    let (end_hour, _) = parse_clock(end)?;
    Ok(Some((start_hour, end_hour)))
}

fn is_quiet_hour(now: DateTime<Utc>, quiet: Option<(u8, u8)>) -> bool {
    let Some((start, end)) = quiet else {
        return false;
    };
    let hour = u8::try_from(now.hour()).unwrap_or_default();
    if start == end {
        return true;
    }
    if start < end {
        hour >= start && hour < end
    } else {
        hour >= start || hour < end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_parser_supports_ui_formats_and_rejects_ambiguous_text() {
        let now = DateTime::parse_from_rfc3339("2026-08-28T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let (daily, due) = parse_schedule("daily at 09:30", now).unwrap();
        assert_eq!(daily, Trigger::Interval { seconds: 86_400 });
        assert_eq!(due.unwrap().to_rfc3339(), "2026-08-28T09:30:00+00:00");
        let (interval, _) = parse_schedule("every 15 minutes", now).unwrap();
        assert_eq!(interval, Trigger::Interval { seconds: 900 });
        assert!(matches!(
            parse_schedule("*/5 * * * *", now).unwrap().0,
            Trigger::Cron { .. }
        ));
        assert!(parse_schedule("sometime tomorrow", now).is_err());
    }

    #[test]
    fn quiet_hours_handle_cross_midnight_and_equal_all_day_window() {
        let late = DateTime::parse_from_rfc3339("2026-08-28T23:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let midday = DateTime::parse_from_rfc3339("2026-08-28T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(is_quiet_hour(late, Some((22, 7))));
        assert!(!is_quiet_hour(midday, Some((22, 7))));
        assert!(is_quiet_hour(midday, Some((8, 8))));
    }

    #[test]
    fn background_system_prompt_requires_native_approval() {
        assert!(BACKGROUND_SYSTEM_PROMPT.contains("native policy gateway"));
        assert!(BACKGROUND_SYSTEM_PROMPT.contains("explicit native approval"));
        assert!(BACKGROUND_SYSTEM_PROMPT.contains("not an interactive user turn"));
    }
}
