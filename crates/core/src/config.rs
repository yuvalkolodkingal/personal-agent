//! Canonical, strict, human-editable application configuration.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use thiserror::Error;

/// JSON Schema used by editors and external tooling.
pub const CONFIG_SCHEMA: &str = include_str!("../../../contracts/schema/config.schema.json");

fn schema_version() -> u32 {
    1
}

fn persona_style() -> String {
    "Composed, concise, and quietly witty.".to_owned()
}

fn parallelism() -> u8 {
    3
}

fn delegation_depth() -> u8 {
    3
}

fn opencode_version() -> String {
    personal_agent_runtime::OPENCODE_VERSION.to_owned()
}

fn startup_timeout_ms() -> u64 {
    30_000
}

fn transcript_recording() -> bool {
    true
}

fn enabled() -> bool {
    true
}

fn ui_theme() -> String {
    "midnight".to_owned()
}

fn cyan() -> String {
    "cyan".to_owned()
}

fn locale() -> String {
    "en".to_owned()
}

fn percent_100() -> u16 {
    100
}

fn wake_threshold() -> u16 {
    930
}

fn vad_start() -> u16 {
    600
}

fn vad_stop() -> u16 {
    350
}

fn endpoint_short_ms() -> u64 {
    700
}

fn endpoint_long_ms() -> u64 {
    1_400
}

fn wake_refractory_ms() -> u64 {
    2_000
}

fn voice_language() -> String {
    "en".to_owned()
}

fn whisper_model() -> String {
    "medium-streaming".to_owned()
}

fn default_voice() -> String {
    "Ryan".to_owned()
}

fn working_directory() -> String {
    std::env::var("HOME").unwrap_or_else(|_| ".".to_owned())
}

fn retention_days() -> u16 {
    90
}

fn empty_object() -> Value {
    serde_json::json!({})
}

/// Top-level v1 configuration. Unknown fields fail closed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalAgentConfig {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    pub persona: PersonaConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub privacy: PrivacyConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub voice: VoiceConfig,
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    #[serde(default)]
    pub browser: BrowserConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub automation: AutomationConfig,
    #[serde(default)]
    pub notifications: NotificationConfig,
    #[serde(default)]
    pub updates: UpdateConfig,
    /// Full managed `OpenCode` configuration. Security-owned keys are overwritten
    /// by the runtime overlay and plaintext-looking credential fields are rejected.
    #[serde(default = "empty_object")]
    pub opencode: Value,
    #[serde(default)]
    pub secret_aliases: Vec<KeychainAlias>,
    #[serde(default)]
    pub risk_acknowledgements: Vec<RiskAcknowledgement>,
}

impl Default for PersonalAgentConfig {
    fn default() -> Self {
        Self {
            schema_version: schema_version(),
            persona: PersonaConfig::default(),
            agent: AgentConfig::default(),
            runtime: RuntimeConfig::default(),
            privacy: PrivacyConfig::default(),
            ui: UiConfig::default(),
            voice: VoiceConfig::default(),
            workspace: WorkspaceConfig::default(),
            browser: BrowserConfig::default(),
            memory: MemoryConfig::default(),
            automation: AutomationConfig::default(),
            notifications: NotificationConfig::default(),
            updates: UpdateConfig::default(),
            opencode: empty_object(),
            secret_aliases: Vec::new(),
            risk_acknowledgements: Vec::new(),
        }
    }
}

/// Configurable assistant identity. It describes behavior and never sentience.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaConfig {
    pub name: String,
    #[serde(default = "persona_style")]
    pub style: String,
}

impl Default for PersonaConfig {
    fn default() -> Self {
        Self {
            name: "JARVIS".to_owned(),
            style: persona_style(),
        }
    }
}

/// Bounded agent concurrency controls.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    #[serde(default = "parallelism")]
    pub default_parallelism: u8,
    #[serde(default = "delegation_depth")]
    pub max_delegation_depth: u8,
    #[serde(default = "enabled")]
    pub require_plan_for_multistep: bool,
    #[serde(default = "enabled")]
    pub verify_success_criteria: bool,
    #[serde(default)]
    pub default_token_budget: u64,
    #[serde(default)]
    pub default_cost_budget_microusd: u64,
    #[serde(default)]
    pub default_wall_time_minutes: u32,
    #[serde(default)]
    pub default_tool_call_budget: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            default_parallelism: parallelism(),
            max_delegation_depth: delegation_depth(),
            require_plan_for_multistep: true,
            verify_success_criteria: true,
            default_token_budget: 0,
            default_cost_budget_microusd: 0,
            default_wall_time_minutes: 0,
            default_tool_call_budget: 0,
        }
    }
}

/// Runtime ownership and startup controls.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    #[serde(default = "opencode_version")]
    pub opencode_version: String,
    #[serde(default = "startup_timeout_ms")]
    pub startup_timeout_ms: u64,
    #[serde(default)]
    pub default_provider: String,
    #[serde(default)]
    pub default_model: String,
    #[serde(default)]
    pub small_model: String,
    #[serde(default)]
    pub default_agent: String,
    #[serde(default)]
    pub default_effort: String,
    #[serde(default = "working_directory")]
    pub working_directory: String,
    #[serde(default = "enabled")]
    pub auto_compact: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            opencode_version: opencode_version(),
            startup_timeout_ms: startup_timeout_ms(),
            default_provider: String::new(),
            default_model: String::new(),
            small_model: String::new(),
            default_agent: "build".to_owned(),
            default_effort: String::new(),
            working_directory: working_directory(),
            auto_compact: true,
        }
    }
}

/// Local audit-retention choices.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Each field is an independent privacy preference.
pub struct PrivacyConfig {
    #[serde(default = "transcript_recording")]
    pub record_transcripts: bool,
    #[serde(default)]
    pub record_tool_arguments: bool,
    #[serde(default = "retention_days")]
    pub transcript_retention_days: u16,
    #[serde(default = "enabled")]
    pub redact_secrets: bool,
    #[serde(default)]
    pub guest_mode_by_default: bool,
    #[serde(default)]
    pub analytics: bool,
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        Self {
            record_transcripts: transcript_recording(),
            record_tool_arguments: false,
            transcript_retention_days: retention_days(),
            redact_secrets: true,
            guest_mode_by_default: false,
            analytics: false,
        }
    }
}

/// Desktop presentation, accessibility, and keyboard preferences.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Desktop preferences are independently configurable.
pub struct UiConfig {
    #[serde(default = "ui_theme")]
    pub theme: String,
    #[serde(default = "cyan")]
    pub accent: String,
    #[serde(default = "locale")]
    pub locale: String,
    #[serde(default = "percent_100")]
    pub text_scale_percent: u16,
    #[serde(default)]
    pub reduced_motion: bool,
    #[serde(default = "enabled")]
    pub hud_enabled: bool,
    #[serde(default)]
    pub start_in_hud: bool,
    #[serde(default)]
    pub overlay: bool,
    #[serde(default = "enabled")]
    pub show_reasoning: bool,
    #[serde(default = "enabled")]
    pub show_tool_details: bool,
    #[serde(default = "enabled")]
    pub session_tabs: bool,
    #[serde(default)]
    pub compact_sidebar: bool,
    #[serde(default = "default_palette_hotkey")]
    pub command_palette_hotkey: String,
    #[serde(default = "default_global_hotkey")]
    pub global_hotkey: String,
}

fn default_palette_hotkey() -> String {
    "Ctrl+K".to_owned()
}

fn default_global_hotkey() -> String {
    "Ctrl+Space".to_owned()
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: ui_theme(),
            accent: cyan(),
            locale: locale(),
            text_scale_percent: percent_100(),
            reduced_motion: false,
            hud_enabled: true,
            start_in_hud: false,
            overlay: false,
            show_reasoning: true,
            show_tool_details: true,
            session_tabs: true,
            compact_sidebar: false,
            command_palette_hotkey: default_palette_hotkey(),
            global_hotkey: default_global_hotkey(),
        }
    }
}

/// Privacy-preserving voice capture and playback configuration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Audio processing toggles map to independent OS constraints.
pub struct VoiceConfig {
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default = "default_voice_mode")]
    pub mode: String,
    #[serde(default)]
    pub input_device: String,
    #[serde(default)]
    pub output_device: String,
    #[serde(default = "voice_language")]
    pub language: String,
    #[serde(default = "voice_language")]
    pub response_language: String,
    #[serde(default = "default_stt_backend")]
    /// Local STT family. `moonshine` selects the persistent neural worker;
    /// `whisper.cpp` keeps the compatibility subprocess path.
    pub stt_backend: String,
    #[serde(default = "whisper_model")]
    /// Neural STT profile. `large-v3-turbo` with the `moonshine` backend is the
    /// opt-in Accurate profile: faster-whisper on CUDA with int8-float16 weights.
    /// Every other neural model value keeps Moonshine Medium Streaming on CPU.
    pub stt_model: String,
    #[serde(default)]
    pub stt_executable: String,
    #[serde(default)]
    pub stt_model_path: String,
    #[serde(default = "default_tts_backend")]
    /// Local TTS tier. `qwen3-tts` uses CUDA and falls back through the
    /// `kokoro` CPU int8 worker before the private Piper subprocess. Selecting
    /// `kokoro` starts at the CPU tier and still retains Piper as final fallback.
    pub tts_backend: String,
    #[serde(default = "default_tts_model")]
    pub tts_model: String,
    #[serde(default = "default_voice")]
    /// Engine voice identifier. Kokoro uses `af_heart` when this is empty or
    /// still contains the Qwen default (`Ryan`).
    pub tts_voice: String,
    #[serde(default)]
    pub tts_executable: String,
    #[serde(default)]
    pub tts_model_path: String,
    #[serde(default)]
    pub tts_reference_audio: String,
    #[serde(default)]
    pub tts_reference_text: String,
    #[serde(default = "percent_100")]
    pub speech_rate_percent: u16,
    #[serde(default = "percent_100")]
    pub volume_percent: u16,
    #[serde(default = "percent_100")]
    pub input_gain_percent: u16,
    #[serde(default = "default_ducking")]
    pub ducking_percent: u16,
    #[serde(default = "wake_phrases")]
    pub wake_phrases: Vec<String>,
    #[serde(default = "stop_phrases")]
    pub stop_phrases: Vec<String>,
    #[serde(default = "sleep_phrases")]
    pub sleep_phrases: Vec<String>,
    #[serde(default = "wake_threshold")]
    pub wake_threshold_milli: u16,
    #[serde(default = "vad_start")]
    pub vad_start_milli: u16,
    #[serde(default = "vad_stop")]
    pub vad_stop_milli: u16,
    #[serde(default = "endpoint_short_ms")]
    pub endpoint_short_ms: u64,
    #[serde(default = "endpoint_long_ms")]
    pub endpoint_long_ms: u64,
    #[serde(default = "default_pre_roll_ms")]
    pub pre_roll_ms: u64,
    #[serde(default = "wake_refractory_ms")]
    pub refractory_ms: u64,
    #[serde(default)]
    pub wake_enabled: bool,
    #[serde(default = "enabled")]
    pub push_to_talk: bool,
    #[serde(default = "default_push_to_talk_hotkey")]
    pub push_to_talk_hotkey: String,
    #[serde(default = "enabled")]
    pub barge_in: bool,
    #[serde(default = "enabled")]
    pub echo_cancellation: bool,
    #[serde(default = "enabled")]
    pub noise_suppression: bool,
    #[serde(default = "enabled")]
    pub automatic_gain_control: bool,
    #[serde(default = "enabled")]
    pub offline_only: bool,
    #[serde(default)]
    pub speak_typed_responses: bool,
    #[serde(default)]
    pub quiet_mode: bool,
    #[serde(default)]
    pub speaker_verification: bool,
    #[serde(default)]
    pub meeting_speaker_labels: bool,
    #[serde(default)]
    pub vocabulary: Vec<String>,
    #[serde(default)]
    pub hosted_stt_credential_alias: String,
    #[serde(default)]
    pub hosted_tts_credential_alias: String,
}

fn default_voice_mode() -> String {
    "push-to-talk".to_owned()
}
fn default_stt_backend() -> String {
    "moonshine".to_owned()
}
fn default_tts_backend() -> String {
    "qwen3-tts".to_owned()
}
fn default_tts_model() -> String {
    "Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice".to_owned()
}
fn default_ducking() -> u16 {
    30
}
fn default_pre_roll_ms() -> u64 {
    500
}
fn default_push_to_talk_hotkey() -> String {
    "Space".to_owned()
}
fn wake_phrases() -> Vec<String> {
    vec!["hey jarvis".to_owned(), "jarvis".to_owned()]
}
fn stop_phrases() -> Vec<String> {
    vec!["stop".to_owned(), "cancel".to_owned()]
}
fn sleep_phrases() -> Vec<String> {
    vec!["go to sleep".to_owned()]
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: default_voice_mode(),
            input_device: String::new(),
            output_device: String::new(),
            language: voice_language(),
            response_language: voice_language(),
            stt_backend: default_stt_backend(),
            stt_model: whisper_model(),
            stt_executable: String::new(),
            stt_model_path: String::new(),
            tts_backend: default_tts_backend(),
            tts_model: default_tts_model(),
            tts_voice: default_voice(),
            tts_executable: String::new(),
            tts_model_path: String::new(),
            tts_reference_audio: String::new(),
            tts_reference_text: String::new(),
            speech_rate_percent: 100,
            volume_percent: 100,
            input_gain_percent: 100,
            ducking_percent: default_ducking(),
            wake_phrases: wake_phrases(),
            stop_phrases: stop_phrases(),
            sleep_phrases: sleep_phrases(),
            wake_threshold_milli: wake_threshold(),
            vad_start_milli: vad_start(),
            vad_stop_milli: vad_stop(),
            endpoint_short_ms: endpoint_short_ms(),
            endpoint_long_ms: endpoint_long_ms(),
            pre_roll_ms: default_pre_roll_ms(),
            refractory_ms: wake_refractory_ms(),
            wake_enabled: false,
            push_to_talk: true,
            push_to_talk_hotkey: default_push_to_talk_hotkey(),
            barge_in: true,
            echo_cancellation: true,
            noise_suppression: true,
            automatic_gain_control: true,
            offline_only: true,
            speak_typed_responses: false,
            quiet_mode: false,
            speaker_verification: false,
            meeting_speaker_labels: false,
            vocabulary: Vec::new(),
            hosted_stt_credential_alias: String::new(),
            hosted_tts_credential_alias: String::new(),
        }
    }
}

impl VoiceConfig {
    /// Whether this configuration selects the opt-in CUDA Accurate STT profile.
    ///
    /// The backend remains `moonshine` for compatibility with existing config
    /// and frontend capture routing; `stt_model` chooses the worker engine.
    #[must_use]
    pub fn uses_faster_whisper(&self) -> bool {
        self.stt_backend == "moonshine" && self.stt_model == "large-v3-turbo"
    }
}

/// Project, terminal, attachment, and session defaults.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Workspace behavior uses independent user preferences.
pub struct WorkspaceConfig {
    #[serde(default = "working_directory")]
    pub default_project: String,
    #[serde(default = "enabled")]
    pub restore_sessions: bool,
    #[serde(default = "enabled")]
    pub confirm_session_delete: bool,
    #[serde(default = "enabled")]
    pub open_files_in_app: bool,
    #[serde(default = "default_terminal_shell")]
    pub terminal_shell: String,
    #[serde(default = "default_attachment_limit")]
    pub attachment_limit_mb: u16,
    #[serde(default = "enabled")]
    pub diff_viewer: bool,
}
fn default_terminal_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned())
}
fn default_attachment_limit() -> u16 {
    25
}
impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            default_project: working_directory(),
            restore_sessions: true,
            confirm_session_delete: true,
            open_files_in_app: true,
            terminal_shell: default_terminal_shell(),
            attachment_limit_mb: default_attachment_limit(),
            diff_viewer: true,
        }
    }
}

/// Isolated browser defaults.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Browser policy toggles are intentionally explicit.
pub struct BrowserConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "enabled")]
    pub isolated_profiles: bool,
    #[serde(default)]
    pub personal_profile_opt_in: bool,
    #[serde(default = "enabled")]
    pub quarantine_downloads: bool,
    #[serde(default)]
    pub allow_third_party_subresources: bool,
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    #[serde(default)]
    pub blocked_domains: Vec<String>,
}
impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            isolated_profiles: true,
            personal_profile_opt_in: false,
            quarantine_downloads: true,
            allow_third_party_subresources: false,
            allowed_domains: Vec::new(),
            blocked_domains: Vec::new(),
        }
    }
}

/// Memory retrieval and review defaults.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryConfig {
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default = "enabled")]
    pub inferred_memory_requires_review: bool,
    #[serde(default = "default_recall_limit")]
    pub recall_limit: u16,
    #[serde(default = "default_embedding_model")]
    pub embedding_model: String,
}
fn default_recall_limit() -> u16 {
    12
}
fn default_embedding_model() -> String {
    "multilingual-e5-small".to_owned()
}
impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            inferred_memory_requires_review: true,
            recall_limit: default_recall_limit(),
            embedding_model: default_embedding_model(),
        }
    }
}

/// Scheduler and proactive-work defaults.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutomationConfig {
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default = "default_automation_concurrency")]
    pub max_concurrency: u8,
    #[serde(default = "default_failure_limit")]
    pub pause_after_failures: u8,
    #[serde(default)]
    pub quiet_hours_start: String,
    #[serde(default)]
    pub quiet_hours_end: String,
    #[serde(default = "default_missed_run_policy")]
    pub missed_run_policy: String,
}
fn default_automation_concurrency() -> u8 {
    2
}
fn default_failure_limit() -> u8 {
    3
}
fn default_missed_run_policy() -> String {
    "run-once".to_owned()
}
impl Default for AutomationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_concurrency: default_automation_concurrency(),
            pause_after_failures: default_failure_limit(),
            quiet_hours_start: String::new(),
            quiet_hours_end: String::new(),
            missed_run_policy: default_missed_run_policy(),
        }
    }
}

/// Native notification routing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Notification channels are independently selectable.
pub struct NotificationConfig {
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default = "enabled")]
    pub task_completion: bool,
    #[serde(default = "enabled")]
    pub approvals: bool,
    #[serde(default = "enabled")]
    pub failures: bool,
    #[serde(default)]
    pub sound: bool,
}
impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            task_completion: true,
            approvals: true,
            failures: true,
            sound: false,
        }
    }
}

/// Signed update channel preferences.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateConfig {
    #[serde(default = "default_update_channel")]
    pub channel: String,
    #[serde(default = "enabled")]
    pub check_on_startup: bool,
    #[serde(default)]
    pub automatic_download: bool,
    #[serde(default)]
    pub automatic_install: bool,
}
fn default_update_channel() -> String {
    "stable".to_owned()
}
impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            channel: default_update_channel(),
            check_on_startup: true,
            automatic_download: false,
            automatic_install: false,
        }
    }
}

/// A reference to an operating-system keychain entry, never a secret value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeychainAlias(pub String);

/// Risk levels that require a durable, explicit acknowledgement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RiskLevel {
    Consequential,
    Irreversible,
}

/// Acknowledgement is configuration, not a blanket permission grant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskAcknowledgement {
    pub scope: String,
    pub risk: RiskLevel,
    pub acknowledged: bool,
}

/// Parsed configuration plus safe defaults that were materialized.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigLoad {
    pub config: PersonalAgentConfig,
    pub repaired_fields: Vec<&'static str>,
}

impl ConfigLoad {
    /// Serialize the validated and default-filled configuration.
    ///
    /// # Errors
    ///
    /// Returns an error only if TOML serialization fails.
    pub fn repaired_toml(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(&self.config).map_err(ConfigError::Serialize)
    }
}

/// Configuration parsing or semantic validation failure.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration is not valid TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("configuration cannot be serialized: {0}")]
    Serialize(toml::ser::Error),
    #[error("unsupported configuration schema version {0}")]
    SchemaVersion(u32),
    #[error("persona.name must not be blank")]
    BlankPersona,
    #[error("agent.default_parallelism must be between 1 and 8")]
    Parallelism,
    #[error("agent.max_delegation_depth must be at most 8")]
    DelegationDepth,
    #[error("runtime.opencode_version must match bundled version {expected}")]
    RuntimeVersion { expected: &'static str },
    #[error("runtime.startup_timeout_ms must be between 1000 and 120000")]
    StartupTimeout,
    #[error("secret alias must use keychain:// and contain a non-empty service/account path")]
    SecretAlias,
    #[error("risk acknowledgement scope must not be blank")]
    BlankRiskScope,
    #[error("risk acknowledgement must set acknowledged = true")]
    RiskNotAcknowledged,
    #[error("ui.text_scale_percent must be between 75 and 200")]
    TextScale,
    #[error("voice percentage and probability values are outside their supported range")]
    VoiceRange,
    #[error("voice VAD stop threshold must be lower than the start threshold")]
    VoiceVad,
    #[error("this release supports English voice input and output only")]
    VoiceLanguage,
    #[error("privacy.transcript_retention_days must be between 0 and 3650")]
    Retention,
    #[error("runtime.working_directory must not be blank")]
    WorkingDirectory,
    #[error("opencode must be an object and must not contain plaintext credential fields")]
    UnsafeOpenCodeConfig,
}

/// Canonical configuration file failure.
#[derive(Debug, Error)]
pub enum ConfigFileError {
    #[error("configuration file cannot be accessed: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Config(#[from] ConfigError),
}

/// Render the safe default configuration used for a fresh profile.
///
/// # Errors
///
/// Returns an error only if TOML serialization fails.
pub fn default_config_toml() -> Result<String, ConfigError> {
    toml::to_string_pretty(&PersonalAgentConfig::default()).map_err(ConfigError::Serialize)
}

/// Load canonical TOML, creating a private safe-default file only when absent.
///
/// Existing invalid configuration is never overwritten or silently repaired on
/// disk. The returned value identifies safe defaults that were materialized in
/// memory so the UI can explain them before an explicit save.
///
/// # Errors
///
/// Returns an I/O or strict configuration-validation error.
pub fn load_or_initialize_config(path: &Path) -> Result<ConfigLoad, ConfigFileError> {
    match fs::read_to_string(path) {
        Ok(input) => Ok(parse_config(&input)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let rendered = default_config_toml()?;
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(path) {
                Ok(mut file) => {
                    file.write_all(rendered.as_bytes())?;
                    file.sync_all()?;
                    Ok(parse_config(&rendered)?)
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    Ok(parse_config(&fs::read_to_string(path)?)?)
                }
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// Parse strict TOML, materialize safe defaults, then validate security invariants.
///
/// # Errors
///
/// Unknown fields, malformed TOML, unsupported values, raw-looking secret aliases,
/// and incomplete risk acknowledgements are rejected.
pub fn parse_config(input: &str) -> Result<ConfigLoad, ConfigError> {
    let shape: toml::Value = toml::from_str(input)?;
    let mut repaired_fields = Vec::new();
    for field in [
        "agent",
        "runtime",
        "privacy",
        "ui",
        "voice",
        "workspace",
        "browser",
        "memory",
        "automation",
        "notifications",
        "updates",
        "opencode",
        "secret_aliases",
        "risk_acknowledgements",
    ] {
        if shape.get(field).is_none() {
            repaired_fields.push(field);
        }
    }
    if shape.get("schema_version").is_none() {
        repaired_fields.push("schema_version");
    }
    if shape
        .get("persona")
        .and_then(|persona| persona.get("style"))
        .is_none()
    {
        repaired_fields.push("persona.style");
    }

    let config: PersonalAgentConfig = toml::from_str(input)?;
    validate(&config)?;
    Ok(ConfigLoad {
        config,
        repaired_fields,
    })
}

fn validate(config: &PersonalAgentConfig) -> Result<(), ConfigError> {
    if config.schema_version != schema_version() {
        return Err(ConfigError::SchemaVersion(config.schema_version));
    }
    if config.persona.name.trim().is_empty() {
        return Err(ConfigError::BlankPersona);
    }
    if !(1..=8).contains(&config.agent.default_parallelism) {
        return Err(ConfigError::Parallelism);
    }
    if config.agent.max_delegation_depth > 8 {
        return Err(ConfigError::DelegationDepth);
    }
    if config.runtime.opencode_version != personal_agent_runtime::OPENCODE_VERSION {
        return Err(ConfigError::RuntimeVersion {
            expected: personal_agent_runtime::OPENCODE_VERSION,
        });
    }
    if !(1_000..=120_000).contains(&config.runtime.startup_timeout_ms) {
        return Err(ConfigError::StartupTimeout);
    }
    if config.runtime.working_directory.trim().is_empty() {
        return Err(ConfigError::WorkingDirectory);
    }
    if !(75..=200).contains(&config.ui.text_scale_percent) {
        return Err(ConfigError::TextScale);
    }
    if config.privacy.transcript_retention_days > 3_650 {
        return Err(ConfigError::Retention);
    }
    let voice = &config.voice;
    if voice.language != "en" || voice.response_language != "en" {
        return Err(ConfigError::VoiceLanguage);
    }
    if voice.wake_threshold_milli > 1_000
        || voice.vad_start_milli > 1_000
        || voice.vad_stop_milli > 1_000
        || voice.vad_stop_milli >= voice.vad_start_milli
        || !(25..=300).contains(&voice.speech_rate_percent)
        || voice.volume_percent > 200
        || voice.input_gain_percent > 800
        || voice.ducking_percent > 100
    {
        return if voice.vad_stop_milli >= voice.vad_start_milli {
            Err(ConfigError::VoiceVad)
        } else {
            Err(ConfigError::VoiceRange)
        };
    }
    if !safe_opencode_config(&config.opencode) {
        return Err(ConfigError::UnsafeOpenCodeConfig);
    }
    for alias in &config.secret_aliases {
        if personal_agent_platform::SecretReference::parse(&alias.0).is_err() {
            return Err(ConfigError::SecretAlias);
        }
    }
    for acknowledgement in &config.risk_acknowledgements {
        if acknowledgement.scope.trim().is_empty() {
            return Err(ConfigError::BlankRiskScope);
        }
        if !acknowledgement.acknowledged {
            return Err(ConfigError::RiskNotAcknowledged);
        }
    }
    Ok(())
}

fn safe_opencode_config(value: &Value) -> bool {
    fn visit(value: &Value) -> bool {
        match value {
            Value::Object(object) => object.iter().all(|(key, value)| {
                let normalized = key.to_ascii_lowercase().replace(['_', '-'], "");
                !["apikey", "token", "password", "secret", "credential"]
                    .iter()
                    .any(|needle| normalized.contains(needle))
                    && visit(value)
            }),
            Value::Array(values) => values.iter().all(visit),
            _ => true,
        }
    }
    let Some(object) = value.as_object() else {
        return false;
    };
    visit(&Value::Object(object.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "schema_version = 1\n[persona]\nname = 'JARVIS'\n";

    #[test]
    fn config_materializes_safe_defaults_and_roundtrips() {
        let loaded = parse_config(MINIMAL).expect("minimal config");
        assert_eq!(loaded.config.agent.default_parallelism, 3);
        assert!(loaded.repaired_fields.contains(&"runtime"));
        assert!(loaded.repaired_fields.contains(&"persona.style"));
        let rendered = loaded.repaired_toml().expect("serialize");
        let second = parse_config(&rendered).expect("roundtrip");
        assert_eq!(loaded.config, second.config);
        assert!(second.repaired_fields.is_empty());
    }

    #[test]
    fn config_rejects_unknown_fields() {
        let input = format!("{MINIMAL}\nsecret = 'plaintext'\n");
        assert!(matches!(parse_config(&input), Err(ConfigError::Parse(_))));
    }

    #[test]
    fn config_rejects_invalid_risk_acknowledgement() {
        let input = format!(
            "{MINIMAL}\n[[risk_acknowledgements]]\nscope = 'external.purchase'\nrisk = 'irreversible'\nacknowledged = false\n"
        );
        assert!(matches!(
            parse_config(&input),
            Err(ConfigError::RiskNotAcknowledged)
        ));
    }

    #[test]
    fn config_accepts_only_keychain_aliases() {
        let raw = "schema_version = 1\nsecret_aliases = ['super-secret-value']\n[persona]\nname = 'JARVIS'\n";
        assert!(matches!(parse_config(raw), Err(ConfigError::SecretAlias)));
        let alias = "schema_version = 1\nsecret_aliases = ['keychain://openai/default']\n[persona]\nname = 'JARVIS'\n";
        assert!(parse_config(alias).is_ok());
    }

    #[test]
    fn config_schema_remains_draft_2020_12() {
        let schema: serde_json::Value = serde_json::from_str(CONFIG_SCHEMA).expect("schema JSON");
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert!(schema["properties"]["risk_acknowledgements"].is_object());
    }

    #[test]
    fn accurate_stt_requires_the_neural_backend_and_large_v3_turbo_model() {
        let mut voice = VoiceConfig {
            stt_model: "large-v3-turbo".to_owned(),
            ..VoiceConfig::default()
        };
        assert!(voice.uses_faster_whisper());
        voice.stt_backend = "whisper.cpp".to_owned();
        assert!(!voice.uses_faster_whisper());
        voice.stt_backend = "moonshine".to_owned();
        voice.stt_model = "medium-streaming".to_owned();
        assert!(!voice.uses_faster_whisper());
    }

    #[test]
    fn config_file_initializes_once_and_preserves_invalid_edits() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("config.toml");
        let initialized = load_or_initialize_config(&path).expect("initialize");
        assert_eq!(initialized.config.persona.name, "JARVIS");
        assert!(initialized.repaired_fields.is_empty());

        fs::write(&path, "invalid = true\n").expect("edit fixture");
        assert!(load_or_initialize_config(&path).is_err());
        assert_eq!(fs::read_to_string(&path).expect("read"), "invalid = true\n");
    }
}
