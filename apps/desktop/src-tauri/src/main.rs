#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;

use personal_agent_core::{
    AppProjection, PersonalAgentConfig, ProfileState, load_or_initialize_config,
};
use personal_agent_migration::{LegacyRoots, MigrationConsent, MigrationPlan, MigrationReport};
use personal_agent_platform::{LifecycleMarker, OsSecretStore};
use personal_agent_runtime::{
    AgentRuntime, OpenCodeApiClient, OpenCodeConfig, OpenCodeSidecar, RuntimeHealth,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::Duration;
use tauri::menu::{Menu, MenuItem};
use tauri::path::BaseDirectory;
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, WindowEvent};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tracing_subscriber::util::SubscriberInitExt;

struct DesktopState {
    profile: Mutex<ProfileState>,
    runtime: tokio::sync::Mutex<OpenCodeSidecar>,
    turn_clients: RwLock<BTreeMap<String, OpenCodeApiClient>>,
    config: RwLock<PersonalAgentConfig>,
    config_path: PathBuf,
    sidecar_executable: PathBuf,
    safety_plugin: PathBuf,
    active_session: tokio::sync::Mutex<Option<ActiveSession>>,
    voice_playback: tokio::sync::Mutex<Option<VoicePlayback>>,
    lifecycle: Mutex<Option<LifecycleMarker>>,
    migration_review: Mutex<Option<MigrationReview>>,
    app_data: PathBuf,
    _log_guard: tracing_appender::non_blocking::WorkerGuard,
}

#[derive(Clone)]
struct ActiveSession {
    id: String,
    directory: PathBuf,
}

struct VoicePlayback {
    child: tokio::process::Child,
    wav: PathBuf,
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

fn runtime_from_parts(
    executable: PathBuf,
    safety_plugin: PathBuf,
    app_data: &Path,
    config: &PersonalAgentConfig,
) -> OpenCodeSidecar {
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
    OpenCodeSidecar::new(runtime_config)
}

fn configured_runtime(state: &DesktopState, config: &PersonalAgentConfig) -> OpenCodeSidecar {
    runtime_from_parts(
        state.sidecar_executable.clone(),
        state.safety_plugin.clone(),
        &state.app_data,
        config,
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
    if let Some(window) = app.get_webview_window("main")
        && let Err(error) = window.show().and_then(|()| window.set_focus())
    {
        tracing::warn!(%error, "main window could not be shown");
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
fn diagnostics() -> serde_json::Value {
    personal_agent_core::diagnostic_snapshot()
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

fn clean_shutdown(app: &tauri::AppHandle) {
    let state = app.state::<DesktopState>();
    if let Ok(mut runtime) = state.runtime.try_lock()
        && let Err(error) = tauri::async_runtime::block_on(runtime.stop())
    {
        tracing::warn!(%error, "runtime did not stop cleanly");
    }
    if let Ok(mut lifecycle) = state.lifecycle.lock()
        && let Some(marker) = lifecycle.take()
        && let Err(error) = marker.finish()
    {
        tracing::warn!(%error, "clean lifecycle marker removal failed");
    }
    if let Ok(mut playback) = state.voice_playback.try_lock()
        && let Some(mut playback) = playback.take()
    {
        let _ = tauri::async_runtime::block_on(playback.child.kill());
        let _ = std::fs::remove_file(playback.wav);
    }
    tracing::info!("desktop host stopped");
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
                if let Err(error) = window.hide() {
                    tracing::warn!(%error, "main window could not be hidden");
                }
            }
        })
        .setup(|app| {
            let app_data = app.path().app_data_dir()?;
            let log_guard = init_logging(&app_data.join("logs"))?;
            tracing::info!(
                version = env!("CARGO_PKG_VERSION"),
                platform = std::env::consts::OS,
                architecture = std::env::consts::ARCH,
                "desktop host starting"
            );
            let config_path = app_data.join("config.toml");
            let config = load_or_initialize_config(&config_path)?;
            let lifecycle = LifecycleMarker::begin(
                &app_data.join("lifecycle/run-state.json"),
                env!("CARGO_PKG_VERSION"),
            )?;
            let previous_unclean_run = lifecycle.previous_unclean_run();
            let database = app_data.join("profiles/default.db");
            let mut profile = ProfileState::open(&database, "default", &OsSecretStore)?;
            profile.record_lifecycle_start(previous_unclean_run)?;

            let safety_plugin = app
                .path()
                .resolve("opencode-plugin/index.ts", BaseDirectory::Resource)?;
            let sidecar_executable = sidecar_path()?;
            let sidecar = runtime_from_parts(
                sidecar_executable.clone(),
                safety_plugin.clone(),
                &app_data,
                &config.config,
            );
            app.manage(DesktopState {
                profile: Mutex::new(profile),
                runtime: tokio::sync::Mutex::new(sidecar),
                turn_clients: RwLock::new(BTreeMap::new()),
                config: RwLock::new(config.config),
                config_path,
                sidecar_executable,
                safety_plugin,
                active_session: tokio::sync::Mutex::new(None),
                voice_playback: tokio::sync::Mutex::new(None),
                lifecycle: Mutex::new(Some(lifecycle)),
                migration_review: Mutex::new(None),
                app_data,
                _log_guard: log_guard,
            });
            install_media_permission_handler(app)?;
            if let Err(error) = app
                .global_shortcut()
                .register(Shortcut::new(Some(Modifiers::SUPER), Code::KeyJ))
            {
                tracing::warn!(%error, "Super+J global shortcut could not be registered");
            }
            install_tray(app)?;

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<DesktopState>();
                let health = match state.runtime.lock().await.start().await {
                    Ok(health) => health,
                    Err(error) => RuntimeHealth {
                        healthy: false,
                        version: personal_agent_runtime::OPENCODE_VERSION.to_owned(),
                        detail: error.to_string(),
                    },
                };
                tracing::info!(healthy = health.healthy, version = %health.version, "runtime health updated");
                persist_runtime_health(&state, &health);
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
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
            api::provider_oauth_authorize,
            api::provider_oauth_callback,
            api::provider_set_key,
            api::provider_revoke,
            api::voice_status,
            api::microphone_state,
            api::voice_transcribe,
            api::voice_speak,
            api::voice_self_test,
            api::voice_stop,
            api::voice_install,
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
