export type Projection = {
  last_sequence: number;
  active_profile: string;
  active_session: string | null;
  goals_total: number;
  tasks_running: number;
  approvals_waiting: number;
  microphone_active: boolean;
  runtime_healthy: boolean;
  unclean_shutdowns: number;
  recovered_unclean_run: boolean;
  recent_events?: Array<{ sequence: number; event_type: string; origin: string }>;
};

export type EventEnvelope = {
  schema_version: number;
  event_id: string;
  wall_clock_timestamp: string;
  monotonic_sequence: number;
  origin: string;
  profile_id: string;
  session_id?: string | null;
  type: string;
  payload_json: number[];
};

export type RuntimeCapability = {
  provider_id: string;
  model_id: string;
  context_tokens?: number | null;
  local: boolean;
  reasoning: boolean;
  tool_calls: boolean;
  input_modalities: string[];
  output_modalities: string[];
};

export type CatalogResource<T = unknown> = {
  available: boolean;
  data?: T;
  reason?: string;
};

export type RuntimeCatalog = Record<string, CatalogResource> & {
  models?: CatalogResource<RuntimeCapability[]>;
};

export type VoiceStatus = {
  stt_ready: boolean;
  tts_ready: boolean;
  playback_ready: boolean;
  configured_stt_backend: string;
  configured_tts_backend: string;
  active_stt_backend: string;
  active_tts_backend: string;
  degraded: boolean;
  neural_runtime_ready: boolean;
  moonshine_ready: boolean;
  smart_turn_ready: boolean;
  qwen_ready: boolean;
  moonshine_model?: string | null;
  qwen_model?: string | null;
  neural_python?: string | null;
  whisper_executable?: string | null;
  whisper_model?: string | null;
  piper_executable?: string | null;
  piper_model?: string | null;
  playback_command?: string | null;
  details: string[];
};

export type AppConfig = {
  schema_version: number;
  persona: { name: string; style: string };
  agent: Record<string, string | number | boolean>;
  runtime: {
    opencode_version: string;
    startup_timeout_ms: number;
    default_provider: string;
    default_model: string;
    small_model: string;
    default_agent: string;
    default_effort: string;
    working_directory: string;
    auto_compact: boolean;
  };
  privacy: Record<string, string | number | boolean>;
  ui: {
    theme: string;
    accent: string;
    locale: string;
    text_scale_percent: number;
    reduced_motion: boolean;
    hud_enabled: boolean;
    start_in_hud: boolean;
    overlay: boolean;
    show_reasoning: boolean;
    show_tool_details: boolean;
    session_tabs: boolean;
    compact_sidebar: boolean;
    command_palette_hotkey: string;
    global_hotkey: string;
  };
  voice: {
    enabled: boolean;
    mode: string;
    input_device: string;
    output_device: string;
    language: string;
    response_language: string;
    stt_backend: string;
    stt_model: string;
    stt_executable: string;
    stt_model_path: string;
    tts_backend: string;
    tts_model: string;
    tts_voice: string;
    tts_executable: string;
    tts_model_path: string;
    tts_reference_audio: string;
    tts_reference_text: string;
    speech_rate_percent: number;
    volume_percent: number;
    input_gain_percent: number;
    ducking_percent: number;
    wake_phrases: string[];
    stop_phrases: string[];
    sleep_phrases: string[];
    wake_threshold_milli: number;
    vad_start_milli: number;
    vad_stop_milli: number;
    endpoint_short_ms: number;
    endpoint_long_ms: number;
    pre_roll_ms: number;
    refractory_ms: number;
    wake_enabled: boolean;
    push_to_talk: boolean;
    push_to_talk_hotkey: string;
    barge_in: boolean;
    echo_cancellation: boolean;
    noise_suppression: boolean;
    automatic_gain_control: boolean;
    offline_only: boolean;
    speak_typed_responses: boolean;
    quiet_mode: boolean;
    speaker_verification: boolean;
    meeting_speaker_labels: boolean;
    vocabulary: string[];
    hosted_stt_credential_alias: string;
    hosted_tts_credential_alias: string;
  };
  workspace: Record<string, string | number | boolean>;
  browser: Record<string, string | number | boolean | string[]>;
  memory: Record<string, string | number | boolean>;
  automation: Record<string, string | number | boolean>;
  notifications: Record<string, string | number | boolean>;
  updates: Record<string, string | number | boolean>;
  opencode: Record<string, unknown>;
  secret_aliases: string[];
  risk_acknowledgements: unknown[];
};

export type Bootstrap = {
  config: AppConfig;
  config_schema: Record<string, unknown>;
  projection: Projection;
  history: EventEnvelope[];
  catalog: RuntimeCatalog;
  voice: VoiceStatus;
  app_data: string;
};

export function eventPayload(event: EventEnvelope): Record<string, unknown> {
  try {
    return JSON.parse(new TextDecoder().decode(Uint8Array.from(event.payload_json))) as Record<string, unknown>;
  } catch {
    return {};
  }
}
