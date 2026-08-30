#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod artifacts_host;
#[cfg(target_os = "linux")]
mod atspi_linux;
mod automation_host;
mod capabilities;
mod connector_oauth;
mod goals_host;
mod mcp_host;
mod native_desktop;
mod native_dictation;
mod perf;
#[cfg(target_os = "linux")]
mod portal_linux;
#[cfg(not(target_os = "linux"))]
#[path = "portal_stub.rs"]
mod portal_linux;
mod pty_host;
mod skills_agents;
mod usage_host;

use personal_agent_core::{
    AppProjection, PersistentMemory, PersonalAgentConfig, ProfileState, load_or_initialize_config,
};
use personal_agent_migration::{LegacyRoots, MigrationConsent, MigrationPlan, MigrationReport};
use personal_agent_platform::{LifecycleMarker, OsSecretStore};
use personal_agent_runtime::{
    AgentRuntime, OpenCodeConfig, OpenCodeSidecar, OpenCodeSidecarControl, RuntimeHandle,
    RuntimeHealth,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tauri::menu::{Menu, MenuItem};
use tauri::path::BaseDirectory;
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tracing_subscriber::util::SubscriberInitExt;

struct DesktopState {
    profile: Arc<Mutex<ProfileState>>,
    memory: Mutex<PersistentMemory>,
    runtime: RuntimeAccess,
    runtime_emergency_control: OpenCodeSidecarControl,
    config: RwLock<PersonalAgentConfig>,
    config_path: PathBuf,
    sidecar_executable: PathBuf,
    safety_plugin: PathBuf,
    active_session: tokio::sync::Mutex<Option<ActiveSession>>,
    pending_memory_sessions: tokio::sync::Mutex<BTreeSet<String>>,
    voice_playback: tokio::sync::Mutex<Option<VoicePlayback>>,
    voice_capture_active: AtomicBool,
    voice_runtime: tokio::sync::Mutex<Option<personal_agent_audio::NeuralVoiceRuntime>>,
    voice_model_arbiter: tokio::sync::Mutex<personal_agent_audio::ModelArbiter>,
    voice_stt_model: tokio::sync::Mutex<Option<personal_agent_audio::LocalModel>>,
    voice_runtime_script: PathBuf,
    voice_runtime_pid: AtomicU32,
    voice_synthesis_active: AtomicBool,
    voice_generation: AtomicU64,
    lifecycle: Mutex<Option<LifecycleMarker>>,
    migration_review: Mutex<Option<MigrationReview>>,
    app_data: PathBuf,
    _log_guard: tracing_appender::non_blocking::WorkerGuard,
}

/// Separates process lifecycle mutation from the cloneable runtime data plane.
///
/// Existing host modules call `lock()` to take a handle; that method acquires
/// only a short-lived read lock and returns immediately. Start, stop and config
/// replacement are the only operations that hold the lifecycle write lock.
struct RuntimeAccess {
    lifecycle: tokio::sync::RwLock<OpenCodeSidecar>,
    handle: tokio::sync::RwLock<RuntimeHandle>,
}

impl RuntimeAccess {
    fn new(sidecar: OpenCodeSidecar) -> Self {
        let handle = sidecar.runtime_handle();
        Self {
            lifecycle: tokio::sync::RwLock::new(sidecar),
            handle: tokio::sync::RwLock::new(handle),
        }
    }

    async fn lock(&self) -> RuntimeHandle {
        self.handle.read().await.clone()
    }

    async fn write(&self) -> tokio::sync::RwLockWriteGuard<'_, OpenCodeSidecar> {
        self.lifecycle.write().await
    }
}

#[derive(Clone)]
struct ActiveSession {
    id: String,
    directory: PathBuf,
}

struct VoicePlayback {
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    native: Option<personal_agent_audio::NativePlaybackControl>,
    stopped: tokio::sync::oneshot::Receiver<()>,
    wav: Option<PathBuf>,
    generation: u64,
}

struct MigrationReview {
    token: String,
    plan: MigrationPlan,
}

#[derive(serde::Serialize)]
struct MigrationReviewResponse {
    review_token: String,
    plan: MigrationPlan,
}

#[derive(serde::Serialize)]
struct MigrationImportResponse {
    report: MigrationReport,
    projection: AppProjection,
    json_report_path: String,
    markdown_report_path: String,
}

fn sidecar_path() -> Result<PathBuf, std::io::Error> {
    let executable = std::env::current_exe()?;
    let directory = executable.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "application executable has no parent directory",
        )
    })?;
    Ok(directory.join(if cfg!(target_os = "windows") {
        "opencode.exe"
    } else {
        "opencode"
    }))
}

fn runtime_config_from_parts(
    executable: PathBuf,
    safety_plugin: PathBuf,
    app_data: &Path,
    config: &PersonalAgentConfig,
) -> OpenCodeConfig {
    let mut runtime_config = OpenCodeConfig::pinned(
        executable,
        safety_plugin,
        app_data.join("runtime/opencode-profile"),
    );
    runtime_config.startup_timeout = Duration::from_millis(config.runtime.startup_timeout_ms);
    runtime_config.default_model = (!config.runtime.default_model.trim().is_empty()).then(|| {
        if config.runtime.default_model.contains('/') {
            config.runtime.default_model.clone()
        } else {
            format!(
                "{}/{}",
                config.runtime.default_provider, config.runtime.default_model
            )
        }
    });
    runtime_config.small_model =
        (!config.runtime.small_model.trim().is_empty()).then(|| config.runtime.small_model.clone());
    runtime_config.managed_config = config.opencode.clone();
    runtime_config.providers = config
        .opencode
        .get("provider")
        .and_then(serde_json::Value::as_object)
        .map(|providers| {
            providers
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    runtime_config
}

fn runtime_from_parts(
    executable: PathBuf,
    safety_plugin: PathBuf,
    app_data: &Path,
    config: &PersonalAgentConfig,
) -> OpenCodeSidecar {
    OpenCodeSidecar::new(runtime_config_from_parts(
        executable,
        safety_plugin,
        app_data,
        config,
    ))
}

fn configured_runtime(state: &DesktopState, config: &PersonalAgentConfig) -> OpenCodeSidecar {
    OpenCodeSidecar::with_emergency_control(
        runtime_config_from_parts(
            state.sidecar_executable.clone(),
            state.safety_plugin.clone(),
            &state.app_data,
            config,
        ),
        &state.runtime_emergency_control,
    )
}

fn init_logging(
    directory: &Path,
) -> Result<tracing_appender::non_blocking::WorkerGuard, std::io::Error> {
    std::fs::create_dir_all(directory)?;
    let appender = tracing_appender::rolling::daily(directory, "personal-agent.jsonl");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .with_target(true)
        .with_writer(writer)
        .finish()
        .try_init()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(guard)
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        // A minimized window is reliably recoverable on Wayland. Calling show alone on a
        // previously hidden WebKit window can leave the process alive without remapping a
        // surface, so restore every relevant bit of window state before requesting focus.
        let result = window
            .show()
            .and_then(|()| window.unminimize())
            .and_then(|()| window.set_focus());
        if let Err(error) = result {
            tracing::warn!(%error, "main window could not be restored");
        }
    } else {
        tracing::warn!("main window is unavailable");
    }
}

fn install_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Personal Agent", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut builder = TrayIconBuilder::with_id("personal-agent")
        .menu(&menu)
        .tooltip("Personal Agent")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri provides an owned application handle.
fn diagnostics(app: tauri::AppHandle) -> serde_json::Value {
    let mut diagnostics = personal_agent_core::diagnostic_snapshot();
    let perf = perf::report();
    if let Some(object) = diagnostics.as_object_mut() {
        object.insert("perf".to_owned(), perf.clone());
    }
    if let Err(error) = app.emit("perf-report", &perf) {
        tracing::warn!(%error, "performance report could not be emitted");
    }
    diagnostics
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri command arguments are framework-owned values.
fn projection(state: tauri::State<'_, DesktopState>) -> Result<AppProjection, String> {
    state
        .profile
        .lock()
        .map(|profile| profile.projection().clone())
        .map_err(|_| "profile state lock is poisoned".to_owned())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri deserializes and owns IPC arguments.
fn submit_message(
    text: String,
    state: tauri::State<'_, DesktopState>,
) -> Result<AppProjection, String> {
    state
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?
        .submit_user_message(&text)
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri provides an owned application handle.
fn autostart_status(app: tauri::AppHandle) -> Result<bool, String> {
    app.autolaunch()
        .is_enabled()
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri provides an owned application handle.
fn set_autostart(enabled: bool, app: tauri::AppHandle) -> Result<bool, String> {
    let manager = app.autolaunch();
    if enabled {
        manager.enable()
    } else {
        manager.disable()
    }
    .map_err(|error| error.to_string())?;
    manager.is_enabled().map_err(|error| error.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri deserializes and owns IPC arguments.
fn migration_dry_run(
    config_root: String,
    data_root: String,
    opencode_auth: Option<String>,
    state: tauri::State<'_, DesktopState>,
) -> Result<MigrationReviewResponse, String> {
    if config_root.trim().is_empty() || data_root.trim().is_empty() {
        return Err("both legacy configuration and data roots are required".to_owned());
    }
    let roots = LegacyRoots {
        config_root: PathBuf::from(config_root),
        data_root: PathBuf::from(data_root),
        opencode_auth: opencode_auth
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from),
    };
    let plan =
        personal_agent_migration::discover_profile(&roots).map_err(|error| error.to_string())?;
    let review_token = uuid::Uuid::new_v4().to_string();
    let review = MigrationReview {
        token: review_token.clone(),
        plan: plan.clone(),
    };
    *state
        .migration_review
        .lock()
        .map_err(|_| "migration review lock is poisoned".to_owned())? = Some(review);
    tracing::info!(
        input_count = plan.inputs.len(),
        source_fingerprint = %plan.source_fingerprint,
        "legacy migration dry run reviewed"
    );
    Ok(MigrationReviewResponse { review_token, plan })
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri deserializes and owns IPC arguments.
fn migration_import(
    review_token: String,
    confirmed: bool,
    adopt_opencode_auth: bool,
    state: tauri::State<'_, DesktopState>,
) -> Result<MigrationImportResponse, String> {
    if !confirmed {
        return Err("legacy migration requires explicit confirmation".to_owned());
    }
    let reviewed = state
        .migration_review
        .lock()
        .map_err(|_| "migration review lock is poisoned".to_owned())?
        .take()
        .ok_or_else(|| "run and review a migration dry run first".to_owned())?;
    if reviewed.token != review_token {
        return Err("migration review token does not match the latest dry run".to_owned());
    }
    let current = personal_agent_migration::discover_profile(&reviewed.plan.roots)
        .map_err(|error| error.to_string())?;
    if current.source_fingerprint != reviewed.plan.source_fingerprint {
        return Err("legacy source changed after review; run a new dry run".to_owned());
    }
    let mut profile = state
        .profile
        .lock()
        .map_err(|_| "profile state lock is poisoned".to_owned())?;
    let report = profile
        .import_legacy(
            &current,
            MigrationConsent {
                copy_personal_data: true,
                adopt_opencode_auth,
            },
        )
        .map_err(|error| error.to_string())?;
    let projection = profile.projection().clone();
    let written =
        personal_agent_migration::write_reports(&report, &state.app_data.join("migration-reports"))
            .map_err(|error| error.to_string())?;
    tracing::info!(
        imported = report.summary.imported,
        already_present = report.summary.already_present,
        skipped = report.summary.skipped,
        invalid = report.summary.invalid,
        "legacy migration completed"
    );
    Ok(MigrationImportResponse {
        report,
        projection,
        json_report_path: written.json.to_string_lossy().into_owned(),
        markdown_report_path: written.markdown.to_string_lossy().into_owned(),
    })
}

fn persist_runtime_health(state: &DesktopState, health: &RuntimeHealth) {
    if let Ok(mut profile) = state.profile.lock() {
        if let Err(error) = profile.record_runtime_health(health) {
            tracing::error!(%error, "runtime health could not be persisted");
        }
    } else {
        tracing::error!("profile state lock is poisoned");
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeShutdownPath {
    LifecycleLock,
    EmergencyControl,
}

async fn shutdown_runtime(
    runtime: &RuntimeAccess,
    emergency_control: &OpenCodeSidecarControl,
    lock_timeout: Duration,
) -> Result<RuntimeShutdownPath, personal_agent_runtime::RuntimeError> {
    if let Ok(mut runtime) = tokio::time::timeout(lock_timeout, runtime.write()).await {
        tracing::info!(path = "lifecycle-lock", "runtime shutdown path selected");
        runtime.stop().await?;
        Ok(RuntimeShutdownPath::LifecycleLock)
    } else {
        tracing::warn!(
            path = "emergency-control",
            timeout_ms = lock_timeout.as_millis(),
            "runtime lifecycle lock timed out; aborting sessions before child termination"
        );
        if let Err(error) = emergency_control.abort_all_sessions().await {
            tracing::warn!(%error, "runtime sessions could not all be aborted before termination");
        }
        emergency_control.kill().await?;
        Ok(RuntimeShutdownPath::EmergencyControl)
    }
}

fn clean_shutdown(app: &tauri::AppHandle) {
    if let Some(capabilities) = app.try_state::<capabilities::CapabilityState>() {
        tauri::async_runtime::block_on(capabilities.shutdown_portal());
    }
    let pty = app.state::<pty_host::PtyHostState>();
    tauri::async_runtime::block_on(pty.shutdown());
    let state = app.state::<DesktopState>();
    if let Some(goals) = app.try_state::<goals_host::GoalsHostState>() {
        tauri::async_runtime::block_on(goals.shutdown_resident());
        if let Err(error) = goals.flush_persistence(&state.profile) {
            tracing::warn!(%error, "goal snapshots could not be flushed during shutdown");
        }
    }
    if let Some(automations) = app.try_state::<automation_host::AutomationHostState>() {
        tauri::async_runtime::block_on(automations.shutdown_resident());
        if let Err(error) = automations.flush_persistence(&state.profile) {
            tracing::warn!(%error, "automation snapshot could not be flushed during shutdown");
        }
    }
    if let Some(mcp) = app.try_state::<mcp_host::McpHostState>()
        && let Err(error) = mcp.flush_persistence()
    {
        tracing::warn!(%error, "MCP snapshot could not be flushed during shutdown");
    }
    if let Err(error) = tauri::async_runtime::block_on(shutdown_runtime(
        &state.runtime,
        &state.runtime_emergency_control,
        Duration::from_secs(5),
    )) {
        tracing::warn!(%error, "runtime did not stop cleanly");
    }
    if let Ok(mut lifecycle) = state.lifecycle.lock()
        && let Some(marker) = lifecycle.take()
        && let Err(error) = marker.finish()
    {
        tracing::warn!(%error, "clean lifecycle marker removal failed");
    }
    tauri::async_runtime::block_on(api::shutdown_voice_playback(&state));
    tracing::info!("desktop host stopped");
}

struct DeferredNativeStates {
    capabilities: Result<capabilities::CapabilityState, String>,
    mcp: Result<mcp_host::McpHostState, String>,
}

async fn load_deferred_native_states(
    app_data: PathBuf,
    startup_readiness: perf::StartupReadiness,
) -> Result<DeferredNativeStates, String> {
    startup_readiness.wait_for_window_paint().await;
    tokio::task::spawn_blocking(move || DeferredNativeStates {
        capabilities: perf::startup_phase(
            "capability_probe",
            &tracing::info_span!("startup.capability_probe"),
            || capabilities::CapabilityState::load(&app_data),
        ),
        mcp: perf::startup_phase("mcp_load", &tracing::info_span!("startup.mcp_load"), || {
            mcp_host::McpHostState::load(&app_data)
        }),
    })
    .await
    .map_err(|error| format!("deferred native startup task failed: {error}"))
}

#[cfg(target_os = "linux")]
fn install_media_permission_handler(app: &tauri::App) -> tauri::Result<()> {
    use webkit2gtk::{
        PermissionRequestExt, UserMediaPermissionRequest, UserMediaPermissionRequestExt,
        WebViewExt, glib::prelude::Cast,
    };

    if let Some(window) = app.get_webview_window("main") {
        window.with_webview(|platform| {
            platform.inner().connect_permission_request(|_, request| {
                if let Some(media) = request.downcast_ref::<UserMediaPermissionRequest>() {
                    if media.is_for_audio_device() && !media.is_for_video_device() {
                        request.allow();
                    } else {
                        request.deny();
                    }
                    true
                } else {
                    false
                }
            });
        })?;
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn install_media_permission_handler(_: &tauri::App) -> tauri::Result<()> {
    Ok(())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri injects managed state into owned command arguments.
fn startup_window_painted(startup_readiness: tauri::State<'_, perf::StartupReadiness>) {
    startup_readiness.mark_window_painted();
}

#[allow(clippy::too_many_lines)] // Tauri setup keeps lifecycle ordering explicit in one entrypoint.
fn main() {
    let summon_shortcut = Shortcut::new(Some(Modifiers::SUPER), Code::KeyJ);
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, arguments, _working_directory| {
            tracing::info!(argument_count = arguments.len(), "secondary launch redirected");
            show_main_window(app);
        }))
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .app_name("Personal Agent")
                .arg("--autostart")
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    if shortcut == &summon_shortcut && event.state() == ShortcutState::Pressed {
                        show_main_window(app);
                    }
                })
                .build(),
        )
        .on_window_event(|window, event| {
            if window.label() == "main"
                && let WindowEvent::CloseRequested { api, .. } = event
            {
                api.prevent_close();
                // The native close control must mean quit. Super+J and the desktop launcher
                // start the user service again when the user wants to reopen the app.
                window.app_handle().exit(0);
            }
        })
        .setup(|app| {
            let native_setup_started = std::time::Instant::now();
            let app_data = perf::startup_phase(
                "app_data",
                &tracing::info_span!("startup.app_data"),
                || app.path().app_data_dir(),
            )?;
            let log_guard = perf::startup_phase(
                "logging",
                &tracing::info_span!("startup.logging"),
                || init_logging(&app_data.join("logs")),
            )?;
            let native_setup_span = tracing::info_span!("startup.native_setup");
            let _native_setup_guard = native_setup_span.enter();
            tracing::info!(
                version = env!("CARGO_PKG_VERSION"),
                platform = std::env::consts::OS,
                architecture = std::env::consts::ARCH,
                "desktop host starting"
            );
            let config_path = app_data.join("config.toml");
            let config = perf::startup_phase(
                "config_load",
                &tracing::info_span!("startup.config_load"),
                || load_or_initialize_config(&config_path),
            )?;
            let lifecycle = perf::startup_phase(
                "lifecycle",
                &tracing::info_span!("startup.lifecycle"),
                || {
                    LifecycleMarker::begin(
                        &app_data.join("lifecycle/run-state.json"),
                        env!("CARGO_PKG_VERSION"),
                    )
                },
            )?;
            let previous_unclean_run = lifecycle.previous_unclean_run();
            let database = app_data.join("profiles/default.db");
            let mut profile = perf::startup_phase(
                "db_open",
                &tracing::info_span!("startup.db_open"),
                || ProfileState::open(&database, "default", &OsSecretStore),
            )?;
            perf::startup_phase(
                "lifecycle_replay",
                &tracing::info_span!("startup.lifecycle_replay"),
                || profile.record_lifecycle_start(previous_unclean_run),
            )?;
            let automation_state = perf::startup_phase(
                "automation_load",
                &tracing::info_span!("startup.automation_load"),
                || {
                    automation_host::AutomationHostState::load(&mut profile)
                        .map_err(std::io::Error::other)
                },
            )?;
            let goals_state = perf::startup_phase(
                "goals_replay",
                &tracing::info_span!("startup.goals_replay"),
                || {
                    goals_host::GoalsHostState::load(
                        &mut profile,
                        &config.config.runtime.working_directory,
                    )
                    .map_err(std::io::Error::other)
                },
            )?;
            let memory = perf::startup_phase(
                "memory_load",
                &tracing::info_span!("startup.memory_load"),
                || -> Result<PersistentMemory, personal_agent_core::CoreError> {
                    Ok(if let Some(memory) = profile.persistent_memory_snapshot()? {
                        memory
                    } else {
                        PersistentMemory::from_store(
                            profile.memory_snapshot()?.unwrap_or_default(),
                        )
                    })
                },
            )?;
            let profile = Arc::new(Mutex::new(profile));

            let (safety_plugin, voice_runtime_script) = perf::startup_phase(
                "resource_paths",
                &tracing::info_span!("startup.resource_paths"),
                || -> tauri::Result<_> {
                    Ok((
                        app.path()
                            .resolve("opencode-plugin/index.ts", BaseDirectory::Resource)?,
                        app.path().resolve(
                            "voice-runtime/voice-runtime.py",
                            BaseDirectory::Resource,
                        )?,
                    ))
                },
            )?;
            let (sidecar_executable, sidecar) = perf::startup_phase(
                "runtime_config",
                &tracing::info_span!("startup.runtime_config"),
                || -> Result<_, std::io::Error> {
                    let sidecar_executable = sidecar_path()?;
                    let sidecar = runtime_from_parts(
                        sidecar_executable.clone(),
                        safety_plugin.clone(),
                        &app_data,
                        &config.config,
                    );
                    Ok((sidecar_executable, sidecar))
                },
            )?;
            let runtime_emergency_control = sidecar.emergency_control();
            let deferred_app_data = app_data.clone();
            let startup_readiness = perf::StartupReadiness::default();
            perf::startup_phase(
                "state_install",
                &tracing::info_span!("startup.state_install"),
                || {
                    app.manage(pty_host::PtyHostState::default());
                    app.manage(connector_oauth::ConnectorOAuthState::default());
                    app.manage(startup_readiness.clone());
                    app.manage(automation_state);
                    app.manage(goals_state);
                    app.manage(DesktopState {
                        profile,
                        memory: Mutex::new(memory),
                        runtime: RuntimeAccess::new(sidecar),
                        runtime_emergency_control,
                        config: RwLock::new(config.config),
                        config_path,
                        sidecar_executable,
                        safety_plugin,
                        active_session: tokio::sync::Mutex::new(None),
                        pending_memory_sessions: tokio::sync::Mutex::new(BTreeSet::new()),
                        voice_playback: tokio::sync::Mutex::new(None),
                        voice_capture_active: AtomicBool::new(false),
                        voice_runtime: tokio::sync::Mutex::new(None),
                        voice_model_arbiter: tokio::sync::Mutex::new(
                            personal_agent_audio::ModelArbiter::new(),
                        ),
                        voice_stt_model: tokio::sync::Mutex::new(None),
                        voice_runtime_script,
                        voice_runtime_pid: AtomicU32::new(0),
                        voice_synthesis_active: AtomicBool::new(false),
                        voice_generation: AtomicU64::new(0),
                        lifecycle: Mutex::new(Some(lifecycle)),
                        migration_review: Mutex::new(None),
                        app_data,
                        _log_guard: log_guard,
                    });
                },
            );
            perf::startup_phase(
                "media_permissions",
                &tracing::info_span!("startup.media_permissions"),
                || install_media_permission_handler(app),
            )?;
            perf::startup_phase(
                "global_shortcut",
                &tracing::info_span!("startup.global_shortcut"),
                || {
                    if let Err(error) = app
                        .global_shortcut()
                        .register(Shortcut::new(Some(Modifiers::SUPER), Code::KeyJ))
                    {
                        tracing::warn!(%error, "Super+J global shortcut could not be registered");
                    }
                },
            );
            perf::startup_phase("tray", &tracing::info_span!("startup.tray"), || {
                install_tray(app)
            })?;

            let handle = app.handle().clone();
            let deferred_startup_readiness = startup_readiness;
            perf::startup_phase(
                "runtime_spawn",
                &tracing::info_span!("startup.runtime_spawn"),
                || {
                    tauri::async_runtime::spawn(async move {
                        let runtime_handle = handle.clone();
                        let runtime_start = async move {
                            let state = runtime_handle.state::<DesktopState>();
                            match state.runtime.write().await.start().await {
                                Ok(health) => health,
                                Err(error) => RuntimeHealth {
                                    healthy: false,
                                    version: personal_agent_runtime::OPENCODE_VERSION.to_owned(),
                                    detail: error.to_string(),
                                },
                            }
                        };
                        let deferred_startup = load_deferred_native_states(
                            deferred_app_data,
                            deferred_startup_readiness,
                        );
                        let (health, deferred_states) =
                            tokio::join!(runtime_start, deferred_startup);

                        match deferred_states {
                            Ok(DeferredNativeStates { capabilities, mcp }) => {
                                match capabilities {
                                    Ok(capability_state) => {
                                        let capabilities =
                                            capability_state.diagnostic_capabilities().await;
                                        if handle.manage(capability_state) {
                                            perf::record_startup_milestone("capabilities_ready");
                                            if let Err(error) = handle.emit(
                                                "capabilities-ready",
                                                serde_json::json!({
                                                    "capabilities": capabilities,
                                                    "error": null,
                                                }),
                                            ) {
                                                tracing::warn!(%error, "capability readiness could not be emitted");
                                            }
                                        } else {
                                            tracing::warn!("capability state was already installed");
                                        }
                                    }
                                    Err(error) => {
                                        tracing::error!(%error, "native capabilities could not be loaded");
                                        if let Err(emit_error) = handle.emit(
                                            "capabilities-ready",
                                            serde_json::json!({
                                                "capabilities": null,
                                                "error": error,
                                            }),
                                        ) {
                                            tracing::warn!(%emit_error, "capability load failure could not be emitted");
                                        }
                                    }
                                }
                                match mcp {
                                    Ok(mcp_state) => {
                                        if handle.manage(mcp_state) {
                                            let mcp = handle.state::<mcp_host::McpHostState>();
                                            match mcp_host::mcp_manager_snapshot(mcp) {
                                                Ok(snapshot) => {
                                                    if let Err(error) = handle
                                                        .emit("mcp-manager://changed", snapshot)
                                                    {
                                                        tracing::warn!(%error, "loaded MCP snapshot could not be emitted");
                                                    }
                                                }
                                                Err(error) => {
                                                    tracing::warn!(%error, "loaded MCP snapshot could not be read");
                                                }
                                            }
                                        } else {
                                            tracing::warn!("MCP host state was already installed");
                                        }
                                    }
                                    Err(error) => {
                                        tracing::error!(%error, "MCP host state could not be loaded");
                                    }
                                }
                            }
                            Err(error) => {
                                tracing::error!(%error, "deferred native startup failed");
                            }
                        }

                        let state = handle.state::<DesktopState>();
                        tracing::info!(healthy = health.healthy, version = %health.version, "runtime health updated");
                        persist_runtime_health(&state, &health);
                        // Pre-synthesize the acknowledgement phrases off the
                        // startup path so the first spoken reply is a cache hit.
                        let warmup_handle = handle.clone();
                        tauri::async_runtime::spawn(async move {
                            let warmup_state = warmup_handle.state::<DesktopState>();
                            api::warmup_tts_phrase_cache(&warmup_state).await;
                        });
                        if health.healthy {
                            automation_host::ensure_resident_executor(handle.clone());
                            goals_host::ensure_resident_executor(handle.clone());
                            if let Some(mcp) = handle.try_state::<mcp_host::McpHostState>() {
                                match mcp_host::restore_enabled_servers(&mcp, &state).await {
                                    Ok(snapshot) => {
                                        if let Err(error) =
                                            handle.emit("mcp-manager://changed", snapshot)
                                        {
                                            tracing::warn!(%error, "restored MCP snapshot could not be emitted");
                                        }
                                    }
                                    Err(error) => {
                                        tracing::warn!(%error, "persisted MCP servers could not be synchronized");
                                    }
                                }
                            } else {
                                tracing::warn!("persisted MCP servers were not restored because manager loading failed");
                            }
                        }
                    });
                },
            );
            perf::record_startup_phase("native_setup", native_setup_started.elapsed());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            startup_window_painted,
            diagnostics,
            projection,
            submit_message,
            autostart_status,
            set_autostart,
            api::bootstrap,
            api::save_config,
            api::runtime_catalog,
            api::chat_send,
            api::chat_turn_status,
            api::session_action,
            api::runtime_resource,
            api::runtime_operation,
            api::runtime_answer,
            api::domain_action,
            automation_host::automation_snapshot,
            automation_host::automation_execute,
            goals_host::goals_snapshot,
            goals_host::goals_execute,
            usage_host::usage_snapshot,
            usage_host::usage_export,
            artifacts_host::artifact_snapshot,
            artifacts_host::artifact_create,
            artifacts_host::artifact_add_version,
            artifacts_host::artifact_restore_version,
            artifacts_host::artifact_content,
            artifacts_host::artifact_action,
            artifacts_host::artifact_export,
            api::provider_oauth_authorize,
            api::provider_oauth_callback,
            api::provider_set_key,
            api::provider_revoke,
            api::voice_status,
            api::microphone_state,
            api::voice_transcribe,
            api::voice_stream_start,
            api::voice_stream_chunk,
            api::voice_stream_stop,
            api::voice_stream_cancel,
            api::voice_wake_start,
            api::voice_wake_chunk,
            api::voice_wake_stop,
            api::voice_turn_complete,
            api::voice_speak,
            api::voice_self_test,
            api::voice_stop,
            api::voice_install,
            capabilities::connector_list,
            capabilities::connector_create,
            capabilities::connector_action,
            capabilities::connector_set_grants,
            capabilities::connector_execute,
            connector_oauth::connector_oauth_authorize,
            connector_oauth::connector_oauth_cancel,
            connector_oauth::connector_oauth_refresh,
            connector_oauth::connector_oauth_revoke,
            capabilities::browser_open,
            capabilities::browser_navigate,
            capabilities::browser_action,
            capabilities::browser_close,
            capabilities::local_execute,
            capabilities::docker_execute,
            capabilities::dictation_ingest,
            capabilities::dictation_apply,
            capabilities::voice_route,
            capabilities::dictation_latency_report,
            capabilities::dictation_reset,
            capabilities::native_dictation_status,
            capabilities::native_dictation_arm,
            capabilities::native_dictation_disarm,
            capabilities::native_dictation_stage,
            capabilities::native_dictation_discard,
            capabilities::native_dictation_confirm,
            capabilities::native_dictation_undo,
            capabilities::desktop_status,
            capabilities::desktop_set_capture,
            capabilities::desktop_snapshot,
            capabilities::desktop_execute,
            capabilities::portal_status,
            capabilities::portal_connect,
            capabilities::portal_cancel,
            capabilities::portal_disconnect,
            pty_host::pty_capability,
            pty_host::pty_list,
            pty_host::pty_create,
            pty_host::pty_reconnect,
            pty_host::pty_input,
            pty_host::pty_resize,
            pty_host::pty_read,
            pty_host::pty_terminate,
            mcp_host::mcp_manager_snapshot,
            mcp_host::mcp_manager_execute,
            skills_agents::skills_agents_snapshot,
            skills_agents::skills_agents_write,
            skills_agents::skills_agents_set_enabled,
            skills_agents::skills_agents_delete,
            migration_dry_run,
            migration_import,
        ]);

    let app = builder
        .build(tauri::generate_context!())
        .expect("Personal Agent desktop host could not be built");
    app.run(|app, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            clean_shutdown(app);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn app_paint_precedes_deferred_gdbus_probe_in_perf_report() {
        fn parsed_perf_report() -> serde_json::Value {
            let rendered = serde_json::to_string(&perf::report()).expect("render perf report");
            serde_json::from_str(&rendered).expect("parse perf report")
        }

        fn last_event_order(report: &serde_json::Value, span: &str, event: &str) -> Option<u64> {
            report
                .pointer("/last_cold_start/span_timeline")?
                .as_array()?
                .iter()
                .filter(|entry| {
                    entry.get("span").and_then(serde_json::Value::as_str) == Some(span)
                        && entry.get("event").and_then(serde_json::Value::as_str) == Some(event)
                })
                .filter_map(|entry| entry.get("order").and_then(serde_json::Value::as_u64))
                .next_back()
        }

        let directory = tempfile::tempdir().expect("temporary app data");
        let startup_readiness = perf::StartupReadiness::default();
        let gdbus_before = last_event_order(&parsed_perf_report(), "gdbus_probe", "start");
        let deferred = tokio::spawn(load_deferred_native_states(
            directory.path().to_path_buf(),
            startup_readiness.clone(),
        ));
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        assert_eq!(
            last_event_order(&parsed_perf_report(), "gdbus_probe", "start"),
            gdbus_before,
            "gdbus probe started before the renderer paint barrier opened"
        );

        startup_readiness.mark_window_painted();
        let states = deferred
            .await
            .expect("deferred startup join")
            .expect("deferred startup");
        states.capabilities.expect("capability state");
        states.mcp.expect("MCP state");

        let report = parsed_perf_report();
        let paint =
            last_event_order(&report, "window_paint", "milestone").expect("window paint milestone");
        let gdbus =
            last_event_order(&report, "gdbus_probe", "start").expect("gdbus probe span start");
        assert!(paint < gdbus, "window paint must precede the gdbus probe");
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[tokio::test]
    async fn shutdown_timeout_stops_sidecar_while_lifecycle_lock_is_held() {
        let executable = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/opencode-x86_64-unknown-linux-gnu");
        assert!(
            executable.is_file(),
            "missing bundled sidecar; run `bun run sidecar:fetch`"
        );
        let safety_plugin = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../packages/opencode-plugin/src/index.ts");
        let profile = tempfile::tempdir().expect("temporary sidecar profile");
        let mut config = OpenCodeConfig::pinned(
            executable,
            safety_plugin,
            profile.path().join("opencode-profile"),
        );
        config.startup_timeout = Duration::from_secs(60);
        let mut sidecar = OpenCodeSidecar::new(config);
        let health = sidecar.start().await.expect("start bundled sidecar");
        assert!(health.healthy);
        let emergency_control = sidecar.emergency_control();
        let runtime = RuntimeAccess::new(sidecar);
        let mut held_runtime = runtime.write().await;

        let path = shutdown_runtime(&runtime, &emergency_control, Duration::from_millis(25))
            .await
            .expect("emergency sidecar shutdown");

        assert_eq!(path, RuntimeShutdownPath::EmergencyControl);
        assert!(matches!(
            held_runtime.health().await,
            Err(personal_agent_runtime::RuntimeError::NotRunning)
        ));
    }
}
