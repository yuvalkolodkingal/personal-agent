import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ConfigEditor } from "./ConfigEditor";
import type {
  AppConfig,
  Bootstrap,
  EventEnvelope,
  Projection,
  RuntimeCatalog,
  RuntimeCapability,
  VoiceStatus,
} from "./types";
import { eventPayload } from "./types";
import { useVoiceCapture } from "./useVoiceCapture";

const navigation = [
  "Chat",
  "Goals & tasks",
  "Browser",
  "Projects & terminal",
  "Artifacts",
  "History",
  "Memory",
  "Automations",
  "Integrations",
  "Skills & agents",
  "Usage & egress",
  "Diagnostics",
  "Settings",
] as const;
type Destination = (typeof navigation)[number];
type Json = Record<string, unknown>;
type ChatMessage = {
  id: string;
  role: "user" | "assistant" | "system";
  text: string;
  streaming?: boolean;
  failed?: boolean;
};
type Attachment = { name: string; mime: string; url: string; size: number };
type PendingTurn = {
  sessionId: string;
  promptMessageId: string;
  speak: boolean;
};
type TurnCompletion = {
  text: string;
  speak: boolean;
  status?: string;
  error?: string | null;
};
type VoicePresentation = {
  state: string;
  glyph: string;
  label: string;
  hint: string;
  color: string;
  level: number;
  stoppable: boolean;
};
type FeatureAuditItem = {
  area: string;
  status: "implemented" | "partial" | "not_wired";
  detail: string;
};
const DEFAULT_SESSION_LIMIT = 12;
const SESSION_LIMIT_STEP = 12;

const featureAudit: FeatureAuditItem[] = [
  {
    area: "Chat, OAuth providers and model selection",
    status: "implemented",
    detail:
      "Authenticated OpenCode sidecar, one global model picker, streamed turns and timeout recovery.",
  },
  {
    area: "English voice conversation",
    status: "implemented",
    detail:
      "Moonshine STT, local phrase wake recognition, Smart Turn endpointing and Qwen3-TTS, with Whisper/Piper compatibility fallbacks.",
  },
  {
    area: "Encrypted persistent memory",
    status: "implemented",
    detail:
      "Explicit saves, conversational follow-up capture, recall injection, provenance, review and deletion.",
  },
  {
    area: "Sessions and history",
    status: "implemented",
    detail:
      "Resume, rename, bulk selection, deletion, search and a bounded recent-session rail.",
  },
  {
    area: "Goals and task execution",
    status: "partial",
    detail:
      "Durable goal/task types exist; the desktop does not yet run a background task graph supervisor.",
  },
  {
    area: "Browser workspace",
    status: "partial",
    detail:
      "Browser policy and safety controls exist; an interactive embedded browser controller is not wired.",
  },
  {
    area: "Projects and terminal",
    status: "partial",
    detail:
      "OpenCode files, search, VCS and PTY resources are inspectable; a full persistent terminal surface is not wired.",
  },
  {
    area: "Artifacts",
    status: "partial",
    detail:
      "Artifact metadata is durable; editing, previews and version restoration are not wired.",
  },
  {
    area: "Automations",
    status: "partial",
    detail:
      "The scheduler engine is implemented and tested; desktop-created schedules are not yet executed by a resident runner.",
  },
  {
    area: "Integrations and skills",
    status: "partial",
    detail:
      "Provider OAuth, MCP and agent catalogs are visible; a connector marketplace and lifecycle controls are not wired.",
  },
  {
    area: "Usage, notifications and updates",
    status: "not_wired",
    detail:
      "Policy/config types exist, but normalized cost metering, native notification delivery and automatic updates are not wired.",
  },
];

const voiceStates: Record<
  string,
  Omit<VoicePresentation, "state" | "level">
> = {
  offline: {
    glyph: "○",
    label: "Voice offline",
    hint: "Install a pipeline in Voice Lab",
    color: "#64738f",
    stoppable: false,
  },
  sleeping: {
    glyph: "◡",
    label: "Sleeping",
    hint: "Hold Space or press Super+J",
    color: "#64738f",
    stoppable: false,
  },
  armed: {
    glyph: "◈",
    label: "Wake word armed",
    hint: "Local speech is listening only for “Hey Jarvis”",
    color: "#5bc98f",
    stoppable: false,
  },
  arming: {
    glyph: "◌",
    label: "Arming wake recognition",
    hint: "Opening the private local microphone path",
    color: "#e9b25b",
    stoppable: false,
  },
  wake_detected: {
    glyph: "◆",
    label: "Wake phrase detected",
    hint: "JARVIS is opening a full speech turn",
    color: "#56d9e8",
    stoppable: true,
  },
  loading_model: {
    glyph: "↻",
    label: "Loading speech model",
    hint: "Preparing Moonshine locally",
    color: "#e9b25b",
    stoppable: true,
  },
  requesting: {
    glyph: "◌",
    label: "Opening microphone",
    hint: "Waiting for private device access",
    color: "#e9b25b",
    stoppable: true,
  },
  listening: {
    glyph: "▮",
    label: "Listening",
    hint: "Speak now — release Space to send",
    color: "#56d9e8",
    stoppable: true,
  },
  endpointing: {
    glyph: "⌁",
    label: "Detecting end of turn",
    hint: "Keep speaking to extend your turn",
    color: "#56d9e8",
    stoppable: true,
  },
  transcribing: {
    glyph: "⋯",
    label: "Transcribing",
    hint: "Moonshine Medium · local streaming",
    color: "#8bb4e8",
    stoppable: true,
  },
  thinking: {
    glyph: "◐",
    label: "Thinking",
    hint: "Waiting for the connected model",
    color: "#8bb4e8",
    stoppable: true,
  },
  tool: {
    glyph: "⚙",
    label: "Running tool",
    hint: "The active tool remains permission-gated",
    color: "#e9b25b",
    stoppable: true,
  },
  synthesizing: {
    glyph: "◌",
    label: "Preparing voice",
    hint: "Qwen3-TTS · local GPU",
    color: "#8bb4e8",
    stoppable: true,
  },
  speaking: {
    glyph: "◀",
    label: "Speaking",
    hint: "Qwen3-TTS · barge-in enabled",
    color: "#5bc98f",
    stoppable: true,
  },
  interrupted: {
    glyph: "⏸",
    label: "Interrupted",
    hint: "Playback stopped for your voice",
    color: "#e9b25b",
    stoppable: false,
  },
  recovering: {
    glyph: "↻",
    label: "Recovering",
    hint: "Switching to the compatibility pipeline",
    color: "#e9b25b",
    stoppable: true,
  },
  error: {
    glyph: "✕",
    label: "Voice error",
    hint: "Open Voice Lab for recovery",
    color: "#f1706a",
    stoppable: false,
  },
};

function voicePresentation(state: string, level = 0): VoicePresentation {
  const value = voiceStates[state] ?? voiceStates.sleeping!;
  return { state, level, ...value };
}

type Diagnostic = {
  product: string;
  version: string;
  platform: string;
  arch: string;
  opencode: { pinned: string; topology: string };
  capabilities: Array<{
    id: string;
    backend: string;
    status: { state: string } | string;
  }>;
};

const emptyProjection: Projection = {
  last_sequence: 0,
  active_profile: "default",
  active_session: null,
  goals_total: 0,
  tasks_running: 0,
  approvals_waiting: 0,
  microphone_active: false,
  runtime_healthy: false,
  unclean_shutdowns: 0,
  recovered_unclean_run: false,
};

export const fallbackConfig: AppConfig = {
  schema_version: 1,
  persona: { name: "JARVIS", style: "Composed, concise, and quietly witty." },
  agent: {
    default_parallelism: 3,
    max_delegation_depth: 3,
    require_plan_for_multistep: true,
    verify_success_criteria: true,
    default_token_budget: 0,
    default_cost_budget_microusd: 0,
    default_wall_time_minutes: 0,
    default_tool_call_budget: 0,
  },
  runtime: {
    opencode_version: "1.18.23",
    startup_timeout_ms: 30000,
    default_provider: "",
    default_model: "",
    small_model: "",
    default_agent: "build",
    default_effort: "",
    working_directory: "/",
    auto_compact: true,
  },
  privacy: {
    record_transcripts: true,
    record_tool_arguments: false,
    transcript_retention_days: 90,
    redact_secrets: true,
    guest_mode_by_default: false,
    analytics: false,
  },
  ui: {
    theme: "midnight",
    accent: "cyan",
    locale: "en",
    text_scale_percent: 100,
    reduced_motion: false,
    hud_enabled: true,
    start_in_hud: false,
    overlay: false,
    show_reasoning: true,
    show_tool_details: true,
    session_tabs: true,
    compact_sidebar: false,
    command_palette_hotkey: "Ctrl+K",
    global_hotkey: "Ctrl+Space",
  },
  voice: {
    enabled: true,
    mode: "push-to-talk",
    input_device: "",
    output_device: "",
    language: "en",
    response_language: "en",
    stt_backend: "moonshine",
    stt_model: "medium-streaming",
    stt_executable: "",
    stt_model_path: "",
    tts_backend: "qwen3-tts",
    tts_model: "Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice",
    tts_voice: "Ryan",
    tts_executable: "",
    tts_model_path: "",
    tts_reference_audio: "",
    tts_reference_text: "",
    speech_rate_percent: 100,
    volume_percent: 100,
    input_gain_percent: 100,
    ducking_percent: 30,
    wake_phrases: ["hey jarvis", "jarvis"],
    stop_phrases: ["stop", "cancel"],
    sleep_phrases: ["go to sleep"],
    wake_threshold_milli: 930,
    vad_start_milli: 600,
    vad_stop_milli: 350,
    endpoint_short_ms: 700,
    endpoint_long_ms: 1400,
    pre_roll_ms: 500,
    refractory_ms: 2000,
    wake_enabled: false,
    push_to_talk: true,
    push_to_talk_hotkey: "Space",
    barge_in: true,
    echo_cancellation: true,
    noise_suppression: true,
    automatic_gain_control: true,
    offline_only: true,
    speak_typed_responses: false,
    quiet_mode: false,
    speaker_verification: false,
    meeting_speaker_labels: false,
    vocabulary: [],
    hosted_stt_credential_alias: "",
    hosted_tts_credential_alias: "",
  },
  workspace: {
    default_project: "/",
    restore_sessions: true,
    confirm_session_delete: true,
    open_files_in_app: true,
    terminal_shell: "/bin/sh",
    attachment_limit_mb: 25,
    diff_viewer: true,
  },
  browser: {
    enabled: false,
    isolated_profiles: true,
    personal_profile_opt_in: false,
    quarantine_downloads: true,
    allow_third_party_subresources: false,
    allowed_domains: [],
    blocked_domains: [],
  },
  memory: {
    enabled: true,
    inferred_memory_requires_review: true,
    recall_limit: 12,
    embedding_model: "multilingual-e5-small",
  },
  automation: {
    enabled: true,
    max_concurrency: 2,
    pause_after_failures: 3,
    quiet_hours_start: "",
    quiet_hours_end: "",
    missed_run_policy: "run-once",
  },
  notifications: {
    enabled: true,
    goal_completion: true,
    approvals: true,
    automation_failures: true,
    sound: true,
  },
  updates: {
    channel: "stable",
    check_automatically: true,
    download_automatically: false,
  },
  opencode: {},
  secret_aliases: [],
  risk_acknowledgements: [],
};

const fallbackVoice: VoiceStatus = {
  stt_ready: false,
  tts_ready: false,
  playback_ready: false,
  configured_stt_backend: "moonshine",
  configured_tts_backend: "qwen3-tts",
  active_stt_backend: "moonshine",
  active_tts_backend: "qwen3-tts",
  degraded: false,
  neural_runtime_ready: false,
  moonshine_ready: false,
  smart_turn_ready: false,
  qwen_ready: false,
  details: ["Native voice status is not available."],
};
const fallbackDiagnostic: Diagnostic = {
  product: "Personal Agent",
  version: "0.1.0",
  platform: "local",
  arch: "unknown",
  opencode: { pinned: "1.18.23", topology: "authenticated-loopback-sidecar" },
  capabilities: [],
};

function icon(name: string) {
  const symbols: Record<string, string> = {
    Chat: "◫",
    "Goals & tasks": "✓",
    Browser: "◎",
    "Projects & terminal": "⌘",
    Artifacts: "◇",
    History: "↶",
    Memory: "◉",
    Automations: "⌁",
    Integrations: "⊞",
    "Skills & agents": "♙",
    "Usage & egress": "⌁",
    Diagnostics: "△",
    Settings: "⚙",
  };
  return symbols[name] ?? "□";
}

function resourceData<T = unknown>(
  catalog: RuntimeCatalog,
  name: string,
  fallback: T,
): T {
  const resource = catalog[name];
  return resource?.available && resource.data !== undefined
    ? (resource.data as T)
    : fallback;
}

function asArray(value: unknown): Json[] {
  if (Array.isArray(value))
    return value.filter(
      (item): item is Json => Boolean(item) && typeof item === "object",
    );
  if (value && typeof value === "object")
    return Object.entries(value as Json).map(([name, item]) =>
      typeof item === "object" && item
        ? { name, ...(item as Json) }
        : { name, value: item },
    );
  return [];
}

function labelOf(item: Json, fallback = "Untitled") {
  return String(item.title ?? item.name ?? item.id ?? item.slug ?? fallback);
}

function sessionTimestamp(session: Json) {
  const time =
    session.time && typeof session.time === "object"
      ? (session.time as Json)
      : {};
  return Number(
    time.updated ?? session.updated_at ?? session.updated ?? time.created ?? 0,
  );
}

function sessionAge(session: Json) {
  const timestamp = sessionTimestamp(session);
  if (!timestamp) return "saved";
  const milliseconds =
    timestamp > 10_000_000_000 ? timestamp : timestamp * 1000;
  const minutes = Math.max(0, Math.floor((Date.now() - milliseconds) / 60_000));
  if (minutes < 1) return "now";
  if (minutes < 60) return `${minutes}m`;
  if (minutes < 1_440) return `${Math.floor(minutes / 60)}h`;
  return `${Math.floor(minutes / 1_440)}d`;
}

function extractMessages(value: unknown): ChatMessage[] {
  return asArray(value).flatMap((entry, index) => {
    const info = (entry.info ?? entry) as Json;
    const role =
      info.role === "assistant"
        ? "assistant"
        : info.role === "user"
          ? "user"
          : "system";
    const parts = Array.isArray(entry.parts) ? entry.parts : [];
    const text = parts
      .map((part) =>
        typeof part === "object" && part && (part as Json).type === "text"
          ? String((part as Json).text ?? "")
          : "",
      )
      .join("");
    return text
      ? [{ id: String(info.id ?? `history-${index}`), role, text }]
      : [];
  });
}

function records(history: EventEnvelope[], prefix: string) {
  return history
    .filter((event) => event.type.startsWith(prefix))
    .map((event) => ({ event, payload: eventPayload(event) }))
    .reverse();
}

function SectionHeader({
  eyebrow,
  title,
  actions,
}: {
  eyebrow: string;
  title: string;
  actions?: React.ReactNode;
}) {
  return (
    <header className="section-header">
      <div>
        <span className="eyebrow">{eyebrow}</span>
        <h2>{title}</h2>
      </div>
      <div className="header-actions">{actions}</div>
    </header>
  );
}

function Empty({ title, detail }: { title: string; detail: string }) {
  return (
    <div className="empty">
      <span>◇</span>
      <strong>{title}</strong>
      <p>{detail}</p>
    </div>
  );
}

function ChatView({
  config,
  catalog,
  projection,
  voiceStatus,
  model,
  setModel,
  messages,
  setMessages,
  activeSession,
  setActiveSession,
  onProjection,
  onHistory,
  onCatalog,
  onVoice,
  onOpenProviders,
  onVoicePresentation,
}: {
  config: AppConfig;
  catalog: RuntimeCatalog;
  projection: Projection;
  voiceStatus: VoiceStatus;
  model: string;
  setModel: (model: string) => void;
  messages: ChatMessage[];
  setMessages: React.Dispatch<React.SetStateAction<ChatMessage[]>>;
  activeSession: string;
  setActiveSession: (id: string) => void;
  onProjection: (p: Projection) => void;
  onHistory: (event: EventEnvelope) => void;
  onCatalog: (catalog: RuntimeCatalog) => void;
  onVoice: (status: VoiceStatus) => void;
  onOpenProviders: () => void;
  onVoicePresentation?: (presentation: VoicePresentation) => void;
}) {
  const [composer, setComposer] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [agent, setAgent] = useState(config.runtime.default_agent);
  const [effort, setEffort] = useState(config.runtime.default_effort);
  const [sessionMenu, setSessionMenu] = useState(false);
  const [selectingSessions, setSelectingSessions] = useState(false);
  const [selectedSessions, setSelectedSessions] = useState<string[]>([]);
  const [bulkSessionBusy, setBulkSessionBusy] = useState(false);
  const [sessionLimit, setSessionLimit] = useState(DEFAULT_SESSION_LIMIT);
  const [sessionQuery, setSessionQuery] = useState("");
  const [sessionFilter, setSessionFilter] = useState<"All" | "Voice" | "Chat">(
    "All",
  );
  const [railOpen, setRailOpen] = useState(
    () => typeof window === "undefined" || window.innerWidth > 1050,
  );
  const [inspectorOpen, setInspectorOpen] = useState(
    () => typeof window === "undefined" || window.innerWidth > 1320,
  );
  const [modelPalette, setModelPalette] = useState(false);
  const [modelQuery, setModelQuery] = useState("");
  const [turnStage, setTurnStage] = useState("Ready");
  const [turnSeconds, setTurnSeconds] = useState(0);
  const [playbackState, setPlaybackState] = useState("idle");
  const [pendingTurn, setPendingTurn] = useState<PendingTurn | null>(null);
  const messageListRef = useRef<HTMLDivElement>(null);
  const pushToTalkPressed = useRef(false);
  const completionHandled = useRef(false);
  useEffect(() => {
    let compactRail = window.innerWidth <= 1050;
    let compactInspector = window.innerWidth <= 1320;
    const adjustPanels = () => {
      const nextCompactRail = window.innerWidth <= 1050;
      const nextCompactInspector = window.innerWidth <= 1320;
      if (nextCompactRail !== compactRail) {
        compactRail = nextCompactRail;
        setRailOpen(!nextCompactRail);
      }
      if (nextCompactInspector !== compactInspector) {
        compactInspector = nextCompactInspector;
        setInspectorOpen(!nextCompactInspector);
      }
    };
    window.addEventListener("resize", adjustPanels);
    return () => window.removeEventListener("resize", adjustPanels);
  }, []);
  const sessions = useMemo(
    () =>
      asArray(resourceData(catalog, "sessions", [])).sort(
        (left, right) => sessionTimestamp(right) - sessionTimestamp(left),
      ),
    [catalog],
  );
  const filteredSessions = useMemo(
    () =>
      sessions.filter((session) => {
        const searchable =
          `${labelOf(session)} ${String(session.id ?? "")} ${String(session.mode ?? "")} ${String(session.source ?? "")} ${String(session.input_modality ?? "")}`.toLowerCase();
        const isVoice =
          session.voice === true ||
          session.from_voice === true ||
          /\b(voice|audio|speech)\b/.test(
            `${String(session.mode ?? "")} ${String(session.source ?? "")} ${String(session.input_modality ?? "")}`.toLowerCase(),
          );
        const matchesKind =
          sessionFilter === "All" ||
          (sessionFilter === "Voice" ? isVoice : !isVoice);
        return (
          matchesKind && searchable.includes(sessionQuery.trim().toLowerCase())
        );
      }),
    [sessionFilter, sessionQuery, sessions],
  );
  const visibleSessions = useMemo(() => {
    const limited = filteredSessions.slice(0, sessionLimit);
    const active = sessions.find(
      (session) => String(session.id) === activeSession,
    );
    if (
      !active ||
      limited.some((session) => String(session.id) === String(active.id))
    )
      return limited;
    return [...limited.slice(0, Math.max(0, sessionLimit - 1)), active];
  }, [activeSession, filteredSessions, sessionLimit, sessions]);
  const hiddenSessionCount = Math.max(
    0,
    filteredSessions.length - sessionLimit,
  );
  const models = resourceData<RuntimeCapability[]>(catalog, "models", []);
  const visibleModels = useMemo(
    () =>
      models.filter((item) =>
        `${item.provider_id} ${item.model_id} ${item.local ? "local" : "remote"} ${item.reasoning ? "reasoning" : ""}`
          .toLowerCase()
          .includes(modelQuery.trim().toLowerCase()),
      ),
    [modelQuery, models],
  );
  const modelGroups = useMemo(
    () =>
      visibleModels.reduce<Record<string, RuntimeCapability[]>>(
        (groups, item) => {
          (groups[item.provider_id] ??= []).push(item);
          return groups;
        },
        {},
      ),
    [visibleModels],
  );
  const agents = asArray(resourceData(catalog, "agents", []));
  const permissions = asArray(resourceData(catalog, "permissions", []));
  const questions = asArray(resourceData(catalog, "questions", []));

  const refreshCatalog = useCallback(async () => {
    try {
      onCatalog(
        await invoke<RuntimeCatalog>("runtime_catalog", {
          directory: config.runtime.working_directory,
        }),
      );
    } catch (caught) {
      setError(String(caught));
    }
  }, [config.runtime.working_directory, onCatalog]);

  const finalizeTurn = useCallback(
    (payload: TurnCompletion) => {
      if (completionHandled.current) return;
      completionHandled.current = true;
      const failed = payload.status === "failed";
      setMessages((current) =>
        current.map((item) =>
          item.id === "streaming"
            ? {
                ...item,
                id: crypto.randomUUID(),
                text:
                  item.text ||
                  payload.text ||
                  payload.error ||
                  (failed
                    ? "The turn failed before a response was produced."
                    : "Completed without a text response."),
                streaming: false,
                failed,
              }
            : item,
        ),
      );
      setBusy(false);
      setPendingTurn(null);
      setTurnStage(failed ? "Needs attention" : "Ready");
      if (payload.error) setError(payload.error);
      void refreshCatalog();
      if (payload.speak && payload.text && voiceStatus.tts_ready)
        void invoke("voice_speak", { text: payload.text }).catch((caught) =>
          setError(String(caught)),
        );
    },
    [refreshCatalog, setMessages, voiceStatus.tts_ready],
  );

  const send = useCallback(
    async (raw: string, fromVoice = false) => {
      const text = raw.trim();
      if (!text || busy) return;
      completionHandled.current = false;
      setPendingTurn(null);
      setBusy(true);
      setError("");
      setTurnStage("Connecting to model");
      setTurnSeconds(0);
      setMessages((current) => [
        ...current,
        { id: crypto.randomUUID(), role: "user", text },
        { id: "streaming", role: "assistant", text: "", streaming: true },
      ]);
      setComposer("");
      try {
        if (text.startsWith("/") && activeSession) {
          const [command, ...args] = text.slice(1).split(/\s+/);
          const result = await invoke<Json>("runtime_operation", {
            kind: "session_command",
            sessionId: activeSession,
            directory: config.runtime.working_directory,
            payload: {
              command,
              arguments: args.join(" "),
              agent,
              model,
              variant: effort,
            },
            confirmed: false,
          });
          const extracted = extractMessages([result]);
          setMessages((current) =>
            current.filter((item) => item.id !== "streaming").concat(extracted),
          );
          completionHandled.current = true;
          setBusy(false);
        } else {
          const speak = fromVoice || config.voice.speak_typed_responses;
          const response = await invoke<{
            session_id: string;
            message_id: string;
            projection: Projection;
          }>("chat_send", {
            text,
            directory: config.runtime.working_directory,
            model,
            agent,
            effort,
            speakResponse: speak,
            attachments: attachments.map((item) => ({
              type: "file",
              mime: item.mime,
              filename: item.name,
              url: item.url,
            })),
          });
          setActiveSession(response.session_id);
          setPendingTurn({
            sessionId: response.session_id,
            promptMessageId: response.message_id,
            speak,
          });
          onProjection(response.projection);
          setAttachments([]);
        }
      } catch (caught) {
        completionHandled.current = true;
        setMessages((current) =>
          current.filter((item) => item.id !== "streaming"),
        );
        setError(String(caught));
        setBusy(false);
      }
    },
    [
      activeSession,
      agent,
      attachments,
      busy,
      config.runtime.working_directory,
      config.voice.speak_typed_responses,
      effort,
      model,
      onProjection,
      setActiveSession,
      setMessages,
    ],
  );

  const voice = useVoiceCapture(
    config,
    (transcript) => {
      setComposer(transcript);
      void send(transcript, true);
    },
    onProjection,
    setComposer,
    voiceStatus.stt_ready,
    busy || playbackState !== "idle",
  );
  const activeStt = voiceStatus.stt_ready
    ? voiceStatus.active_stt_backend || config.voice.stt_backend
    : "missing";
  const activeTts = voiceStatus.tts_ready
    ? voiceStatus.active_tts_backend || config.voice.tts_backend
    : "missing";
  const voiceLabel =
    playbackState === "speaking"
      ? "Speaking…"
      : playbackState === "synthesizing"
        ? "Preparing voice…"
        : playbackState === "recovering"
          ? "Loading fallback voice…"
          : voice.state === "arming"
            ? "Arming Hey Jarvis…"
            : voice.state === "armed"
              ? "Say Hey Jarvis"
              : voice.state === "wake_detected"
                ? "Wake phrase detected"
                : voice.state === "loading_model"
                  ? "Loading Moonshine…"
                  : voice.state === "requesting"
                    ? "Opening microphone…"
                    : voice.state === "listening"
                      ? "Listening…"
                      : voice.state === "endpointing"
                        ? "Finishing your turn…"
                        : voice.state === "transcribing"
                          ? "Finalizing transcript…"
                          : voice.state === "error"
                            ? "Voice needs attention"
                            : "Tap to talk";
  const capturing = [
    "loading_model",
    "requesting",
    "listening",
    "endpointing",
  ].includes(voice.state);
  const startVoice = useCallback(async () => {
    if (config.voice.barge_in)
      await invoke("voice_stop").catch(() => undefined);
    await voice.start();
  }, [config.voice.barge_in, voice.start]);
  const presentedVoiceState =
    voice.state !== "idle"
      ? voice.state
      : playbackState !== "idle"
        ? playbackState
        : busy
          ? turnStage.toLowerCase().includes("tool") ||
            turnStage.toLowerCase().startsWith("using")
            ? "tool"
            : "thinking"
          : !voiceStatus.stt_ready || !voiceStatus.tts_ready
            ? "offline"
            : config.voice.wake_enabled
              ? "armed"
              : "sleeping";

  useEffect(
    () =>
      onVoicePresentation?.(
        voicePresentation(presentedVoiceState, voice.level),
      ),
    [onVoicePresentation, presentedVoiceState, voice.level],
  );
  useEffect(() => {
    const open = () => setModelPalette(true);
    const close = (event: KeyboardEvent) => {
      if (event.key === "Escape") setModelPalette(false);
    };
    window.addEventListener("personal-agent:model-palette", open);
    window.addEventListener("keydown", close);
    return () => {
      window.removeEventListener("personal-agent:model-palette", open);
      window.removeEventListener("keydown", close);
    };
  }, []);

  useEffect(() => {
    const list = messageListRef.current;
    if (list && typeof list.scrollTo === "function") {
      list.scrollTo({
        top: list.scrollHeight,
        behavior: config.ui.reduced_motion ? "auto" : "smooth",
      });
    }
  }, [config.ui.reduced_motion, messages]);
  useEffect(() => {
    const unlisten: Array<() => void> = [];
    void listen<EventEnvelope>("runtime-event", ({ payload }) => {
      onHistory(payload);
      if (payload.type === "response.started") setTurnStage("Thinking");
      if (payload.type === "reasoning.available") setTurnStage("Reasoning");
      if (payload.type === "tool.started")
        setTurnStage(`Using ${String(eventPayload(payload).tool ?? "a tool")}`);
      if (payload.type === "tool.completed")
        setTurnStage("Reviewing tool result");
      if (payload.type === "response.retrying")
        setTurnStage(
          `Provider retry ${String(eventPayload(payload).attempt ?? "")}`.trim(),
        );
      if (payload.type === "response.delta") {
        setTurnStage("Responding");
        const delta = String(
          eventPayload(payload).delta ?? eventPayload(payload).text ?? "",
        );
        if (delta)
          setMessages((current) =>
            current.map((item) =>
              item.id === "streaming"
                ? { ...item, text: item.text + delta }
                : item,
            ),
          );
      }
    }).then((fn) => unlisten.push(fn));
    void listen<TurnCompletion>("runtime-turn-complete", ({ payload }) =>
      finalizeTurn(payload),
    ).then((fn) => unlisten.push(fn));
    void listen<{
      state: string;
      detail?: string;
      engine?: string;
      interrupted?: boolean;
    }>("voice-state", ({ payload }) => {
      setPlaybackState(payload.interrupted ? "interrupted" : payload.state);
      if (payload.interrupted)
        window.setTimeout(() => setPlaybackState("idle"), 1200);
      if (payload.state === "recovering" && payload.detail)
        setError(`Voice recovered with fallback: ${payload.detail}`);
    }).then((fn) => unlisten.push(fn));
    return () => unlisten.forEach((fn) => fn());
  }, [finalizeTurn, onHistory, setMessages]);

  useEffect(() => {
    if (!busy || !pendingTurn) return;
    let disposed = false;
    let polling = false;
    const poll = async () => {
      if (disposed || polling || completionHandled.current) return;
      polling = true;
      try {
        const result = await invoke<{
          completed: boolean;
          text: string;
          error?: string | null;
        }>("chat_turn_status", {
          sessionId: pendingTurn.sessionId,
          promptMessageId: pendingTurn.promptMessageId,
          directory: config.runtime.working_directory,
        });
        if (!disposed && result.completed)
          finalizeTurn({
            text: result.text,
            error: result.error,
            status: result.error ? "failed" : "completed",
            speak: pendingTurn.speak,
          });
      } catch {
        // The native event path remains active; transient recovery failures retry.
      } finally {
        polling = false;
      }
    };
    void poll();
    const interval = window.setInterval(() => void poll(), 1000);
    const timeout = window.setTimeout(
      () =>
        finalizeTurn({
          text: "",
          error:
            "The model did not produce a completed response within two minutes. You can retry this message.",
          status: "failed",
          speak: false,
        }),
      120_000,
    );
    return () => {
      disposed = true;
      window.clearInterval(interval);
      window.clearTimeout(timeout);
    };
  }, [busy, config.runtime.working_directory, finalizeTurn, pendingTurn]);

  useEffect(() => {
    if (!busy) return;
    const timer = window.setInterval(
      () => setTurnSeconds((seconds) => seconds + 1),
      1000,
    );
    return () => window.clearInterval(timer);
  }, [busy]);

  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      if (
        event.code !== "Space" ||
        !config.voice.push_to_talk ||
        !config.voice.enabled
      )
        return;
      const target = event.target as HTMLElement | null;
      if (target?.matches("input, textarea, select, [contenteditable=true]"))
        return;
      event.preventDefault();
      if (
        event.type === "keydown" &&
        !event.repeat &&
        voice.state !== "listening"
      ) {
        pushToTalkPressed.current = true;
        void startVoice().then(() => {
          if (!pushToTalkPressed.current) void voice.stop();
        });
      }
      if (event.type === "keyup") {
        pushToTalkPressed.current = false;
        void voice.stop();
      }
    };
    window.addEventListener("keydown", shortcut);
    window.addEventListener("keyup", shortcut);
    return () => {
      window.removeEventListener("keydown", shortcut);
      window.removeEventListener("keyup", shortcut);
    };
  }, [config.voice.enabled, config.voice.push_to_talk, startVoice, voice]);

  const sessionAction = async (action: string, sessionId = activeSession) => {
    setError("");
    try {
      const title =
        action === "rename" ? (window.prompt("New session title") ?? "") : null;
      if (
        action === "delete" &&
        !window.confirm("Delete this session permanently?")
      )
        return;
      if (
        action === "share" &&
        !window.confirm("Create a public share link for this session?")
      )
        return;
      const result = await invoke<Json>("session_action", {
        action,
        sessionId: sessionId || null,
        directory: config.runtime.working_directory,
        title,
        confirmed: ["delete", "share"].includes(action),
      });
      const id = String(result.session_id ?? sessionId ?? "");
      if (["new", "resume", "fork"].includes(action) && id) {
        setActiveSession(id);
        if (action === "resume") {
          const history = await invoke("runtime_resource", {
            kind: "session_messages",
            sessionId: id,
            directory: config.runtime.working_directory,
            path: null,
            query: null,
          });
          setMessages(extractMessages(history));
        } else setMessages([]);
      }
      await refreshCatalog();
      setSessionMenu(false);
    } catch (caught) {
      setError(String(caught));
    }
  };

  const toggleSessionSelection = (sessionId: string) => {
    setSelectedSessions((current) =>
      current.includes(sessionId)
        ? current.filter((id) => id !== sessionId)
        : [...current, sessionId],
    );
  };

  const bulkSessionAction = async (
    action: "compact" | "unshare" | "delete",
  ) => {
    if (!selectedSessions.length || bulkSessionBusy) return;
    if (
      action === "delete" &&
      !window.confirm(
        `Delete ${selectedSessions.length} selected sessions permanently?`,
      )
    )
      return;
    setBulkSessionBusy(true);
    setError("");
    try {
      for (const sessionId of selectedSessions) {
        await invoke("session_action", {
          action,
          sessionId,
          directory: config.runtime.working_directory,
          title: null,
          confirmed: action === "delete",
        });
      }
      if (action === "delete" && selectedSessions.includes(activeSession)) {
        setActiveSession("");
        setMessages([]);
      }
      setSelectedSessions([]);
      setSelectingSessions(false);
      await refreshCatalog();
    } catch (caught) {
      setError(`Bulk ${action} stopped: ${String(caught)}`);
      await refreshCatalog();
    } finally {
      setBulkSessionBusy(false);
    }
  };

  const stopTurn = async () => {
    if (!activeSession) return;
    try {
      await invoke("session_action", {
        action: "abort",
        sessionId: activeSession,
        directory: config.runtime.working_directory,
        title: null,
        confirmed: false,
      });
      setMessages((current) =>
        current.map((item) =>
          item.id === "streaming"
            ? {
                ...item,
                id: crypto.randomUUID(),
                text: item.text || "Stopped.",
                streaming: false,
                failed: true,
              }
            : item,
        ),
      );
      completionHandled.current = true;
      setPendingTurn(null);
      setBusy(false);
      setTurnStage("Stopped");
      setError("");
    } catch (caught) {
      setError(String(caught));
    }
  };

  useEffect(() => {
    const stopCurrentActivity = () => {
      void invoke("voice_stop").catch(() => undefined);
      if (busy) void stopTurn();
      if (capturing) voice.cancel();
    };
    window.addEventListener("personal-agent:voice-stop", stopCurrentActivity);
    return () =>
      window.removeEventListener(
        "personal-agent:voice-stop",
        stopCurrentActivity,
      );
  });

  const addFiles = async (files: FileList | null) => {
    if (!files) return;
    const maximum =
      Number(config.workspace.attachment_limit_mb ?? 25) * 1024 * 1024;
    try {
      const next = await Promise.all(
        Array.from(files).map(async (file) => {
          if (file.size > maximum)
            throw new Error(
              `${file.name} exceeds the ${config.workspace.attachment_limit_mb} MiB attachment limit`,
            );
          const url = await new Promise<string>((resolve, reject) => {
            const reader = new FileReader();
            reader.onload = () => resolve(String(reader.result));
            reader.onerror = () => reject(reader.error);
            reader.readAsDataURL(file);
          });
          return {
            name: file.name,
            mime: file.type || "application/octet-stream",
            url,
            size: file.size,
          };
        }),
      );
      setAttachments((current) => [...current, ...next]);
    } catch (caught) {
      setError(String(caught));
    }
  };

  return (
    <div
      className={`chat-layout ${railOpen ? "" : "rail-closed"} ${inspectorOpen ? "" : "inspector-closed"}`}
    >
      {railOpen && (
        <aside className="session-rail">
          <div className="session-primary-actions session-rail-header">
            <button
              className="new-session"
              onClick={() => void sessionAction("new", "")}
            >
              ＋ New session
            </button>
            <button
              aria-label={
                selectingSessions
                  ? "Finish selecting sessions"
                  : "Select sessions"
              }
              onClick={() => {
                setSelectingSessions((value) => !value);
                if (selectingSessions) setSelectedSessions([]);
              }}
            >
              {selectingSessions ? "Done" : "Select"}
            </button>
            <label className="session-search">
              <span aria-hidden="true">⌕</span>
              <input
                aria-label="Search sessions"
                value={sessionQuery}
                onChange={(event) => setSessionQuery(event.target.value)}
                placeholder="Search sessions"
              />
            </label>
            <div className="session-filter-row">
              {(["All", "Voice", "Chat"] as const).map((filter) => (
                <button
                  key={filter}
                  className={sessionFilter === filter ? "active" : ""}
                  onClick={() => {
                    setSessionFilter(filter);
                    setSessionLimit(DEFAULT_SESSION_LIMIT);
                  }}
                >
                  {filter}
                </button>
              ))}
              <span>{filteredSessions.length}</span>
            </div>
          </div>
          {selectingSessions && (
            <div className="session-bulk-bar">
              <strong>{selectedSessions.length} selected</strong>
              <div>
                <button
                  onClick={() =>
                    setSelectedSessions(
                      visibleSessions.map((session) => String(session.id)),
                    )
                  }
                >
                  All visible
                </button>
                <button onClick={() => setSelectedSessions([])}>Clear</button>
              </div>
              <div>
                <button
                  disabled={!selectedSessions.length || bulkSessionBusy}
                  onClick={() => void bulkSessionAction("compact")}
                >
                  Compact
                </button>
                <button
                  disabled={!selectedSessions.length || bulkSessionBusy}
                  onClick={() => void bulkSessionAction("unshare")}
                >
                  Unshare
                </button>
                <button
                  className="danger"
                  disabled={!selectedSessions.length || bulkSessionBusy}
                  onClick={() => void bulkSessionAction("delete")}
                >
                  Delete
                </button>
              </div>
            </div>
          )}
          <div className="session-list">
            {visibleSessions.length ? (
              visibleSessions.map((session) => {
                const sessionId = String(session.id);
                const selected = selectedSessions.includes(sessionId);
                return (
                  <div
                    key={sessionId}
                    className={`session-entry ${activeSession === sessionId ? "active" : ""} ${selected ? "selected" : ""}`}
                  >
                    {selectingSessions && (
                      <input
                        type="checkbox"
                        aria-label={`Select session ${sessionId}`}
                        checked={selected}
                        onChange={() => toggleSessionSelection(sessionId)}
                      />
                    )}
                    <button
                      className="session-open"
                      onClick={() =>
                        selectingSessions
                          ? toggleSessionSelection(sessionId)
                          : void sessionAction("resume", sessionId)
                      }
                    >
                      <span className="session-status-dot" aria-hidden="true" />
                      <span className="session-copy">
                        <strong>{labelOf(session, "Session")}</strong>
                        <small>
                          {sessionAge(session)} ·{" "}
                          {String(
                            session.message_count ??
                              session.messages ??
                              "saved session",
                          )}
                        </small>
                      </span>
                    </button>
                  </div>
                );
              })
            ) : (
              <p>No saved sessions</p>
            )}
          </div>
          {filteredSessions.length > DEFAULT_SESSION_LIMIT && (
            <div className="session-history-controls">
              <small>
                Showing {visibleSessions.length} of {filteredSessions.length}
              </small>
              {hiddenSessionCount > 0 ? (
                <button
                  onClick={() =>
                    setSessionLimit((limit) =>
                      Math.min(
                        filteredSessions.length,
                        limit + SESSION_LIMIT_STEP,
                      ),
                    )
                  }
                >
                  Show {Math.min(SESSION_LIMIT_STEP, hiddenSessionCount)} older
                </button>
              ) : (
                <button onClick={() => setSessionLimit(DEFAULT_SESSION_LIMIT)}>
                  Hide older
                </button>
              )}
            </div>
          )}
        </aside>
      )}
      <section className="conversation">
        <div className="conversation-toolbar">
          <button
            className="panel-toggle"
            aria-label="Toggle session rail"
            onClick={() => setRailOpen((value) => !value)}
          >
            ◧
          </button>
          <div className="conversation-context">
            <strong>
              {activeSession
                ? sessions.find(
                    (session) => String(session.id) === activeSession,
                  )
                  ? labelOf(
                      sessions.find(
                        (session) => String(session.id) === activeSession,
                      )!,
                    )
                  : "Active session"
                : "New session"}
            </strong>
            <small>{config.runtime.working_directory}</small>
          </div>
          <button className="provider-shortcut" onClick={onOpenProviders}>
            ＋ Connect provider
          </button>
          <select
            aria-label="Agent"
            value={agent}
            onChange={(event) => setAgent(event.target.value)}
          >
            <option value="build">Build</option>
            {agents.map((item) => (
              <option key={labelOf(item)} value={String(item.name ?? item.id)}>
                {labelOf(item)}
              </option>
            ))}
          </select>
          <select
            aria-label="Reasoning effort"
            value={effort}
            onChange={(event) => setEffort(event.target.value)}
          >
            <option value="">Default effort</option>
            <option>low</option>
            <option>medium</option>
            <option>high</option>
            <option>xhigh</option>
          </select>
          {busy ? (
            <div className="turn-progress">
              <span className="thinking-pulse" />
              <strong>{turnStage}</strong>
              <small>{turnSeconds}s</small>
              <button onClick={() => void stopTurn()}>Stop</button>
            </div>
          ) : (
            <span
              className={`runtime-dot ${projection.runtime_healthy ? "online" : "offline"}`}
            >
              {projection.runtime_healthy ? "Ready" : "Runtime unavailable"}
            </span>
          )}
          <div className="session-actions">
            <button
              aria-label="Session actions"
              onClick={() => setSessionMenu((value) => !value)}
            >
              •••
            </button>
            {sessionMenu && (
              <div>
                {[
                  "rename",
                  "fork",
                  "compact",
                  "share",
                  "unshare",
                  "delete",
                ].map((action) => (
                  <button
                    key={action}
                    onClick={() => void sessionAction(action)}
                    disabled={!activeSession}
                  >
                    {action}
                  </button>
                ))}
              </div>
            )}
          </div>
          <button
            className="panel-toggle"
            aria-label="Toggle inspector"
            onClick={() => setInspectorOpen((value) => !value)}
          >
            ◨
          </button>
        </div>
        <div className="messages" ref={messageListRef} aria-live="polite">
          {!messages.length && (
            <div className="welcome">
              <button
                className={`voice-wave-panel ${presentedVoiceState}`}
                aria-label={voiceLabel}
                disabled={
                  !voiceStatus.stt_ready ||
                  ["endpointing", "transcribing"].includes(voice.state)
                }
                onClick={() =>
                  capturing ? void voice.stop() : void startVoice()
                }
              >
                <span className="wave-meta">INPUT · 16 KHZ MONO</span>
                <span className="wave-state">{voiceLabel.toUpperCase()}</span>
                <span className="waveform" aria-hidden="true">
                  {Array.from({ length: 48 }, (_, index) => (
                    <i
                      key={index}
                      style={{
                        height: `${Math.max(4, (Math.sin(index * 1.7) * 0.5 + 0.5) * 34 * Math.max(0.18, voice.level))}px`,
                      }}
                    />
                  ))}
                </span>
              </button>
              <h2>Ready when you are.</h2>
              <p>
                {config.voice.wake_enabled && voiceStatus.stt_ready ? (
                  <>
                    Say <strong>“Hey Jarvis”</strong>,{" "}
                  </>
                ) : (
                  <>Enable wake recognition in Voice settings, or </>
                )}
                hold{" "}
                <kbd>{config.voice.push_to_talk_hotkey || "Space"}</kbd> to
                talk, or just type. Everything runs on this machine unless you
                connect a remote provider.
              </p>
              <div className="starter-grid">
                {(
                  [
                    [
                      "◫",
                      "Why did the tests fail?",
                      "Reads the diff, runs tests, and proposes a patch",
                    ],
                    [
                      "◎",
                      "Summarize my open work",
                      "Reviews active sessions and durable goals",
                    ],
                    [
                      "⌘",
                      "Inspect this project",
                      "Files, terminals, worktrees, and version control",
                    ],
                    [
                      "⌁",
                      "Draft a morning briefing",
                      "Creates a local, approval-gated automation",
                    ],
                  ] as const
                ).map(([glyph, title, detail]) => (
                  <button key={title} onClick={() => setComposer(title)}>
                    <i>{glyph}</i>
                    <span>
                      <strong>{title}</strong>
                      <small>{detail}</small>
                    </span>
                  </button>
                ))}
              </div>
              <small className="voice-hint">
                {activeStt} → Smart Turn → {activeTts}
                {voiceStatus.degraded ? " · fallback active" : ""}
              </small>
            </div>
          )}
          {messages.map((message) => (
            <article
              key={message.id}
              className={`chat-message ${message.role} ${message.failed ? "failed" : ""}`}
            >
              <div className="message-avatar">
                {message.role === "user"
                  ? "Y"
                  : config.persona.name.slice(0, 1)}
              </div>
              <div>
                <header>
                  <strong>
                    {message.role === "user" ? "You" : config.persona.name}
                  </strong>
                  {message.streaming && (
                    <span>
                      <i className="thinking-pulse" />
                      {turnStage} · {turnSeconds}s
                    </span>
                  )}
                  {message.failed && (
                    <span className="message-failed">Stopped / failed</span>
                  )}
                </header>
                <p>{message.text || "…"}</p>
                {message.role === "assistant" && message.text && (
                  <div className="message-actions">
                    <button
                      onClick={() =>
                        void navigator.clipboard.writeText(message.text)
                      }
                    >
                      Copy
                    </button>
                    <button
                      disabled={!voiceStatus.tts_ready}
                      onClick={() =>
                        void invoke("voice_speak", { text: message.text })
                      }
                    >
                      Speak
                    </button>
                  </div>
                )}
              </div>
            </article>
          ))}
        </div>
        {voice.partialTranscript && capturing && (
          <div className="live-transcript-dock" role="status">
            <span>PARTIAL</span>
            <p>{voice.partialTranscript}</p>
            <i />
          </div>
        )}
        {attachments.length > 0 && (
          <div className="attachments">
            {attachments.map((item, index) => (
              <span key={`${item.name}-${index}`}>
                {item.name}
                <button
                  aria-label={`Remove ${item.name}`}
                  onClick={() =>
                    setAttachments((current) =>
                      current.filter((_, position) => position !== index),
                    )
                  }
                >
                  ×
                </button>
              </span>
            ))}
          </div>
        )}
        {error && (
          <p className="error-banner" role="alert">
            {error}
          </p>
        )}
        {voice.error && (
          <p className="error-banner voice-error" role="alert">
            <strong>Microphone:</strong> {voice.error}
          </p>
        )}
        <form
          className="composer"
          onSubmit={(event) => {
            event.preventDefault();
            if (busy) void stopTurn();
            else void send(composer);
          }}
        >
          <label className="attach-button" title="Attach images or files">
            ＋
            <input
              aria-label="Attach files"
              type="file"
              multiple
              onChange={(event) => void addFiles(event.target.files)}
            />
          </label>
          <textarea
            aria-label="Message JARVIS"
            rows={1}
            value={composer}
            placeholder="Message JARVIS…  (/ for commands, @ for files)"
            onChange={(event) => setComposer(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                void send(composer);
              }
            }}
          />
          <button
            type="button"
            className={`voice-button ${voice.state}`}
            aria-label={
              capturing ? "Stop voice capture" : "Start voice capture"
            }
            disabled={
              !voiceStatus.stt_ready ||
              ["endpointing", "transcribing"].includes(voice.state)
            }
            onClick={() => (capturing ? void voice.stop() : void startVoice())}
          >
            <span style={{ transform: `scale(${1 + voice.level * 0.35})` }}>
              {capturing ? "▮" : "◎"}
            </span>
            {voice.state === "loading_model"
              ? "LOAD"
              : voice.state === "requesting"
                ? "OPEN"
                : ["endpointing", "transcribing"].includes(voice.state)
                  ? "…"
                  : voice.state === "listening"
                    ? "LIVE"
                    : "MIC"}
          </button>
          <button
            className="send-button"
            type="submit"
            aria-label={busy ? "Stop" : "↑"}
            disabled={!busy && !composer.trim()}
          >
            {busy ? "Stop" : "Send"}
          </button>
        </form>
        <footer className="voice-strip">
          <span className={projection.microphone_active ? "live" : ""}>
            ●{" "}
            {projection.microphone_active
              ? "MICROPHONE LIVE"
              : "MICROPHONE PRIVATE"}
          </span>
          <span>STT {activeStt.toUpperCase()}</span>
          <span>TTS {activeTts.toUpperCase()}</span>
          <span className={`voice-runtime-state ${playbackState}`}>
            {playbackState === "speaking"
              ? "SPEAKING"
              : voice.state.replaceAll("_", " ").toUpperCase()}
          </span>
          <button onClick={() => void invoke("voice_stop")}>Stop audio</button>
        </footer>
      </section>
      {inspectorOpen && (
        <aside className="activity-rail">
          <div className={`rail-card voice-status-card ${presentedVoiceState}`}>
            <header>
              <span className="eyebrow">VOICE STATE</span>
              <b
                style={{ color: voicePresentation(presentedVoiceState).color }}
              >
                {voicePresentation(presentedVoiceState).glyph}
              </b>
            </header>
            <strong>{voicePresentation(presentedVoiceState).label}</strong>
            <small>{voicePresentation(presentedVoiceState).hint}</small>
            {voicePresentation(presentedVoiceState).stoppable && (
              <button
                onClick={() =>
                  window.dispatchEvent(new Event("personal-agent:voice-stop"))
                }
              >
                Stop {presentedVoiceState.replaceAll("_", " ")}
              </button>
            )}
            <dl>
              <div>
                <dt>Input</dt>
                <dd>{config.voice.input_device || "System default"}</dd>
              </div>
              <div>
                <dt>Privacy</dt>
                <dd>
                  {projection.microphone_active
                    ? "mic open · local only"
                    : "mic closed"}
                </dd>
              </div>
              <div>
                <dt>Pipeline</dt>
                <dd>Balanced</dd>
              </div>
              <div>
                <dt>Barge-in</dt>
                <dd>{config.voice.barge_in ? "enabled" : "disabled"}</dd>
              </div>
            </dl>
          </div>
          <div className="rail-card">
            <span className="eyebrow">ACTIVE SESSION</span>
            <strong>{activeSession || "No active session"}</strong>
            <small>{config.runtime.working_directory}</small>
            <div className="rail-metrics">
              <span>
                {projection.tasks_running}
                <small>tasks</small>
              </span>
              <span>
                {projection.approvals_waiting}
                <small>approvals</small>
              </span>
            </div>
          </div>
          <div className="rail-card">
            <span className="eyebrow">PERMISSIONS & QUESTIONS</span>
            {permissions.length + questions.length === 0 ? (
              <p>No requests waiting.</p>
            ) : (
              [...permissions, ...questions].map((request) => {
                const permission = Boolean(request.permission);
                return (
                  <div className="request" key={String(request.id)}>
                    <strong>{labelOf(request, "Runtime request")}</strong>
                    <small>
                      {String(
                        request.permission ??
                          request.question ??
                          "Action requires your answer",
                      )}
                    </small>
                    <div>
                      <button
                        onClick={() =>
                          void invoke("runtime_answer", {
                            sessionId: activeSession,
                            requestId: request.id,
                            answer: permission
                              ? { kind: "permission", reply: "once" }
                              : { kind: "question", answers: [["yes"]] },
                          }).then(refreshCatalog)
                        }
                      >
                        Allow once
                      </button>
                      <button
                        onClick={() =>
                          void invoke("runtime_answer", {
                            sessionId: activeSession,
                            requestId: request.id,
                            answer: permission
                              ? { kind: "permission", reply: "reject" }
                              : { kind: "question", reject: true },
                          }).then(refreshCatalog)
                        }
                      >
                        Reject
                      </button>
                    </div>
                  </div>
                );
              })
            )}
          </div>
          <div
            className={`rail-card voice-link ${voice.state} ${playbackState}`}
          >
            <span className="eyebrow">VOICE LINK</span>
            <div className="voice-orb">
              <i />
              <b />
            </div>
            <strong>{voiceLabel}</strong>
            <small>
              {activeStt} STT · {activeTts} TTS
              {voiceStatus.smart_turn_ready
                ? " · Smart Turn"
                : voiceStatus.degraded
                  ? " · reduced mode"
                  : ""}
            </small>
            {!(
              voiceStatus.moonshine_ready &&
              voiceStatus.smart_turn_ready &&
              voiceStatus.qwen_ready
            ) && (
              <button
                aria-label="Install offline voice"
                className="primary"
                onClick={async () => {
                  try {
                    onVoice(
                      await invoke<VoiceStatus>("voice_install", {
                        component: "balanced",
                      }),
                    );
                  } catch (caught) {
                    setError(String(caught));
                  }
                }}
              >
                Install Balanced neural voice
              </button>
            )}
          </div>
        </aside>
      )}
      {modelPalette && (
        <div
          className="model-palette-backdrop"
          onMouseDown={() => setModelPalette(false)}
        >
          <section
            className="model-palette"
            role="dialog"
            aria-label="Model and provider selector"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <header>
              <span>⌕</span>
              <input
                autoFocus
                value={modelQuery}
                onChange={(event) => setModelQuery(event.target.value)}
                placeholder="Search providers, models, capabilities…"
              />
              <kbd>ESC</kbd>
            </header>
            <div className="model-palette-summary">
              <span>{visibleModels.length} available models</span>
              <button onClick={onOpenProviders}>Manage providers</button>
            </div>
            <div className="model-palette-list">
              {visibleModels.length ? (
                Object.entries(modelGroups).map(([provider, items]) => (
                  <div className="model-group" key={provider}>
                    <header>
                      <span className="provider-initial">
                        {provider.slice(0, 1).toUpperCase()}
                      </span>
                      <strong>{provider}</strong>
                      <small>
                        {items.some((item) => item.local)
                          ? "LOCAL"
                          : "CONNECTED"}
                      </small>
                    </header>
                    {items.map((item) => {
                      const id = `${item.provider_id}/${item.model_id}`;
                      return (
                        <button
                          key={id}
                          className={model === id ? "selected" : ""}
                          onClick={() => {
                            setModel(id);
                            setModelPalette(false);
                            setModelQuery("");
                          }}
                        >
                          <span>
                            <strong>{item.model_id}</strong>
                            <small>
                              {item.reasoning && <i>reasoning</i>}
                              {item.tool_calls && <i>tools</i>}
                              {item.input_modalities.includes("image") && (
                                <i>vision</i>
                              )}
                              {item.local && <i>local</i>}
                            </small>
                          </span>
                          <b>
                            {item.context_tokens
                              ? `${Math.round(item.context_tokens / 1000)}k ctx`
                              : "context n/a"}
                          </b>
                          <em>{model === id ? "✓" : ""}</em>
                        </button>
                      );
                    })}
                  </div>
                ))
              ) : (
                <div className="model-palette-empty">
                  <strong>No models match</strong>
                  <p>Connect a provider or change your search.</p>
                  <button onClick={onOpenProviders}>Open provider setup</button>
                </div>
              )}
            </div>
            <footer>
              <span>Fallback chain</span>
              <b>{model || "Automatic"}</b>
              <i>→</i>
              <b>{config.runtime.small_model || "No fallback configured"}</b>
              <small>↑↓ move · ↵ select · Esc close</small>
            </footer>
          </section>
        </div>
      )}
    </div>
  );
}

function DomainView({
  destination,
  history,
  catalog,
  projection,
  setHistory,
  setCatalog,
  setProjection,
}: {
  destination: Destination;
  history: EventEnvelope[];
  catalog: RuntimeCatalog;
  projection: Projection;
  setHistory: React.Dispatch<React.SetStateAction<EventEnvelope[]>>;
  setCatalog: (catalog: RuntimeCatalog) => void;
  setProjection: (p: Projection) => void;
}) {
  const [fields, setFields] = useState({ first: "", second: "", third: "" });
  const [error, setError] = useState("");
  const map: Partial<
    Record<
      Destination,
      {
        domain: string;
        prefix: string;
        title: string;
        empty: string;
        labels: string[];
      }
    >
  > = {
    "Goals & tasks": {
      domain: "goal",
      prefix: "goal.",
      title: "Durable goals and task graphs",
      empty: "Create a goal with observable success criteria.",
      labels: ["Objective", "Success criteria (one per line)", "Priority"],
    },
    Memory: {
      domain: "memory",
      prefix: "memory.",
      title: "Trusted, reviewable memory",
      empty: "Explicit memories keep provenance and stay under your control.",
      labels: ["Memory content", "Tier", "Sensitivity"],
    },
    Automations: {
      domain: "automation",
      prefix: "automation.",
      title: "Schedules and monitors",
      empty: "Automations retain approval, privacy, and failure policies.",
      labels: ["Name", "Prompt", "Schedule (for example: daily at 09:00)"],
    },
    Artifacts: {
      domain: "artifact",
      prefix: "artifact.",
      title: "Versioned artifacts",
      empty:
        "Create a durable text, code, document, image, audio, or report artifact.",
      labels: ["Title", "Kind", ""],
    },
  };
  const spec = map[destination]!;
  const memories = resourceData<Json[]>(catalog, "memories", []);
  const items = destination === "Memory" ? [] : records(history, spec.prefix);
  const refresh = async () => {
    const bootstrap = await invoke<Bootstrap>("bootstrap");
    setHistory(bootstrap.history);
    setCatalog(bootstrap.catalog);
    setProjection(bootstrap.projection);
  };
  const memoryAction = async (
    action: "approve" | "reject" | "delete",
    id: string,
  ) => {
    setError("");
    try {
      await invoke("domain_action", {
        domain: "memory",
        action,
        payload: { id },
      });
      await refresh();
    } catch (caught) {
      setError(String(caught));
    }
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setError("");
    let payload: Json = {};
    if (spec.domain === "goal")
      payload = {
        objective: fields.first,
        success_criteria: fields.second.split("\n").filter(Boolean),
        priority: Number(fields.third || 0),
      };
    if (spec.domain === "memory")
      payload = {
        content: fields.first,
        tier: fields.second || "semantic",
        sensitivity: fields.third || "private",
      };
    if (spec.domain === "automation")
      payload = {
        name: fields.first,
        prompt: fields.second,
        schedule: fields.third,
      };
    if (spec.domain === "artifact")
      payload = { title: fields.first, kind: fields.second || "text" };
    try {
      const result = await invoke<{ projection: Projection }>("domain_action", {
        domain: spec.domain,
        action: "create",
        payload,
      });
      setProjection(result.projection);
      setFields({ first: "", second: "", third: "" });
      await refresh();
    } catch (caught) {
      setError(String(caught));
    }
  };
  return (
    <div className="page-grid">
      <section className="page-main">
        <SectionHeader eyebrow={destination.toUpperCase()} title={spec.title} />
        <form className="create-form" onSubmit={submit}>
          {spec.labels.map(
            (label, index) =>
              label && (
                <label key={label}>
                  {label}
                  {spec.domain === "memory" && index > 0 ? (
                    <select
                      value={index === 1 ? fields.second : fields.third}
                      onChange={(event) =>
                        setFields((current) => ({
                          ...current,
                          [index === 1 ? "second" : "third"]:
                            event.target.value,
                        }))
                      }
                    >
                      {(index === 1
                        ? [
                            "semantic",
                            "working",
                            "episodic",
                            "procedural",
                            "project",
                            "relationship",
                          ]
                        : ["private", "sensitive", "public"]
                      ).map((option) => (
                        <option key={option}>{option}</option>
                      ))}
                    </select>
                  ) : index === 1 &&
                    ["goal", "automation"].includes(spec.domain) ? (
                    <textarea
                      value={fields.second}
                      onChange={(event) =>
                        setFields((current) => ({
                          ...current,
                          second: event.target.value,
                        }))
                      }
                    />
                  ) : (
                    <input
                      value={
                        index === 0
                          ? fields.first
                          : index === 1
                            ? fields.second
                            : fields.third
                      }
                      onChange={(event) =>
                        setFields((current) => ({
                          ...current,
                          [index === 0
                            ? "first"
                            : index === 1
                              ? "second"
                              : "third"]: event.target.value,
                        }))
                      }
                    />
                  )}
                </label>
              ),
          )}
          <button className="primary">Create</button>
          {error && <p className="field-error">{error}</p>}
        </form>
      </section>
      <aside className="page-side">
        <div className="card-label">
          RECORDS{" "}
          <b>{destination === "Memory" ? memories.length : items.length}</b>
        </div>
        {destination === "Memory" && memories.length > 0 ? (
          <div className="memory-list">
            {memories.map((memory) => {
              const id = String(memory.id ?? "");
              const trust = String(memory.trust ?? "unknown");
              return (
                <article key={id}>
                  <header>
                    <span>{String(memory.tier ?? "semantic")}</span>
                    <b>{trust.replaceAll("_", " ")}</b>
                  </header>
                  <strong>{String(memory.content ?? "")}</strong>
                  <small>
                    {String(memory.sensitivity ?? "private")} · confidence{" "}
                    {Number(memory.confidence ?? 0).toFixed(2)}
                  </small>
                  <footer>
                    {trust === "proposed_inference" && (
                      <>
                        <button
                          onClick={() => void memoryAction("approve", id)}
                        >
                          Approve
                        </button>
                        <button onClick={() => void memoryAction("reject", id)}>
                          Reject
                        </button>
                      </>
                    )}
                    <button
                      className="danger"
                      onClick={() => void memoryAction("delete", id)}
                    >
                      Delete
                    </button>
                  </footer>
                </article>
              );
            })}
          </div>
        ) : !items.length ? (
          <Empty title="No records yet" detail={spec.empty} />
        ) : (
          <div className="record-list">
            {items.map(({ event, payload }) => (
              <article key={event.event_id}>
                <strong>
                  {String(
                    payload.objective ??
                      payload.content ??
                      payload.name ??
                      payload.title ??
                      event.type,
                  )}
                </strong>
                <small>
                  {event.type} · sequence {event.monotonic_sequence}
                </small>
                <pre>{JSON.stringify(payload, null, 2)}</pre>
              </article>
            ))}
          </div>
        )}
      </aside>
      <div className="metric-bar">
        <span>
          {projection.goals_total}
          <small>goals</small>
        </span>
        <span>
          {projection.tasks_running}
          <small>running tasks</small>
        </span>
        <span>
          {projection.approvals_waiting}
          <small>approvals</small>
        </span>
      </div>
    </div>
  );
}

function ProjectView({
  config,
  onCatalog,
}: {
  config: AppConfig;
  catalog: RuntimeCatalog;
  onCatalog: (catalog: RuntimeCatalog) => void;
}) {
  const [tab, setTab] = useState("files");
  const [path, setPath] = useState("");
  const [query, setQuery] = useState("");
  const [data, setData] = useState<unknown>(null);
  const [error, setError] = useState("");
  const load = async (kind: string, requestedPath = path) => {
    setError("");
    try {
      setData(
        await invoke("runtime_resource", {
          kind,
          sessionId: null,
          directory: config.runtime.working_directory,
          path: requestedPath || null,
          query: query || null,
        }),
      );
    } catch (caught) {
      setError(String(caught));
    }
  };
  useEffect(() => {
    void load(
      tab === "diff"
        ? "vcs_diff"
        : tab === "terminal"
          ? "pty_list"
          : tab === "worktrees"
            ? "worktree_list"
            : "file_list",
    );
  }, [tab]);
  const entries = asArray(data);
  return (
    <section className="workbench">
      <SectionHeader
        eyebrow="OPENCODE WORKSPACE"
        title="Projects, files, VCS and terminals"
        actions={
          <>
            <input
              aria-label="Working directory"
              value={config.runtime.working_directory}
              readOnly
            />
            <button
              onClick={async () =>
                onCatalog(
                  await invoke("runtime_catalog", {
                    directory: config.runtime.working_directory,
                  }),
                )
              }
            >
              Refresh
            </button>
          </>
        }
      />
      <div className="tabbar">
        {["files", "search", "diff", "terminal", "worktrees"].map((item) => (
          <button
            key={item}
            className={tab === item ? "active" : ""}
            onClick={() => setTab(item)}
          >
            {item}
          </button>
        ))}
      </div>
      {error && <p className="error-banner">{error}</p>}
      {tab === "files" && (
        <div className="file-workspace">
          <aside>
            <div className="path-input">
              <input
                value={path}
                placeholder="relative path"
                onChange={(event) => setPath(event.target.value)}
              />
              <button onClick={() => void load("file_list")}>Open</button>
            </div>
            {entries.map((entry) => (
              <button
                key={labelOf(entry)}
                onClick={() => {
                  const next = String(entry.path ?? entry.name ?? "");
                  setPath(next);
                  void load(
                    entry.type === "directory" ? "file_list" : "file_content",
                    next,
                  );
                }}
              >
                {String(entry.type ?? "file") === "directory" ? "▸" : "·"}{" "}
                {labelOf(entry)}
              </button>
            ))}
          </aside>
          <pre className="code-view">
            {typeof data === "string" ? data : JSON.stringify(data, null, 2)}
          </pre>
        </div>
      )}
      {tab === "search" && (
        <div className="search-view">
          <div>
            <input
              value={query}
              placeholder="Search files, text or symbols"
              onChange={(event) => setQuery(event.target.value)}
            />
            <button onClick={() => void load("find_text")}>Text</button>
            <button onClick={() => void load("find_file")}>Files</button>
            <button onClick={() => void load("find_symbol")}>Symbols</button>
          </div>
          <pre>{JSON.stringify(data, null, 2)}</pre>
        </div>
      )}
      {tab === "diff" && (
        <pre className="diff-view">{JSON.stringify(data, null, 2)}</pre>
      )}
      {tab === "terminal" && (
        <div className="terminal-view">
          <div className="terminal-toolbar">
            <button
              onClick={async () => {
                try {
                  setData(
                    await invoke("runtime_operation", {
                      kind: "pty_create",
                      identifier: null,
                      sessionId: null,
                      directory: config.runtime.working_directory,
                      payload: {
                        command: config.workspace.terminal_shell,
                        cwd: config.runtime.working_directory,
                        title: "Personal Agent terminal",
                      },
                      confirmed: false,
                    }),
                  );
                } catch (caught) {
                  setError(String(caught));
                }
              }}
            >
              New terminal
            </button>
          </div>
          <pre>{JSON.stringify(data, null, 2)}</pre>
          <p>
            PTY lifecycle is native. OpenCode streams interactive terminal bytes
            over its authenticated PTY channel; active sessions and controls
            remain visible here.
          </p>
        </div>
      )}
      {tab === "worktrees" && (
        <div className="worktree-view">
          <button
            className="primary"
            onClick={async () => {
              try {
                setData(
                  await invoke("runtime_operation", {
                    kind: "worktree_create",
                    identifier: null,
                    sessionId: null,
                    directory: config.runtime.working_directory,
                    payload: {},
                    confirmed: false,
                  }),
                );
              } catch (caught) {
                setError(String(caught));
              }
            }}
          >
            Create isolated worktree
          </button>
          <pre>{JSON.stringify(data, null, 2)}</pre>
        </div>
      )}
    </section>
  );
}

function IntegrationsView({
  catalog,
  config,
  onCatalog,
}: {
  catalog: RuntimeCatalog;
  config: AppConfig;
  onCatalog: (catalog: RuntimeCatalog) => void;
}) {
  const [error, setError] = useState("");
  const providers = asArray(resourceData(catalog, "providers", []));
  const mcp = asArray(resourceData(catalog, "mcp", []));
  const toggle = async (name: string, connect: boolean) => {
    try {
      await invoke("runtime_operation", {
        kind: connect ? "mcp_connect" : "mcp_disconnect",
        identifier: name,
        sessionId: null,
        directory: config.runtime.working_directory,
        payload: {},
        confirmed: false,
      });
      onCatalog(
        await invoke("runtime_catalog", {
          directory: config.runtime.working_directory,
        }),
      );
    } catch (caught) {
      setError(String(caught));
    }
  };
  return (
    <section className="catalog-page">
      <SectionHeader
        eyebrow="EXTENSIONS"
        title="Providers, MCP servers and integrations"
      />
      {error && <p className="error-banner">{error}</p>}
      <div className="catalog-columns">
        <div>
          <h3>Providers</h3>
          {providers.length ? (
            providers.map((item) => (
              <article key={labelOf(item)}>
                <strong>{labelOf(item)}</strong>
                <small>
                  {String(
                    item.source ?? item.status ?? "Available through OpenCode",
                  )}
                </small>
              </article>
            ))
          ) : (
            <Empty
              title="No provider metadata"
              detail={
                catalog.providers?.reason ?? "Connect a provider in Settings."
              }
            />
          )}
        </div>
        <div>
          <h3>MCP servers</h3>
          {mcp.length ? (
            mcp.map((item) => {
              const name = String(item.name ?? item.id);
              const connected = ["connected", "ready"].includes(
                String(item.status).toLowerCase(),
              );
              return (
                <article key={name}>
                  <strong>{name}</strong>
                  <small>{String(item.status ?? "disconnected")}</small>
                  <button onClick={() => void toggle(name, !connected)}>
                    {connected ? "Disconnect" : "Connect"}
                  </button>
                </article>
              );
            })
          ) : (
            <Empty
              title="No MCP servers configured"
              detail="Add MCP configuration under Settings → OpenCode."
            />
          )}
        </div>
      </div>
    </section>
  );
}

function CatalogView({
  catalog,
}: {
  destination: Destination;
  catalog: RuntimeCatalog;
}) {
  return (
    <section className="catalog-page">
      <SectionHeader
        eyebrow="OPENCODE CATALOG"
        title="Agents, commands and skills"
      />
      <div className="catalog-columns three">
        {["agents", "commands", "skills"].map((name) => (
          <div key={name}>
            <h3>{name}</h3>
            {asArray(resourceData(catalog, name, [])).map((item) => (
              <article key={labelOf(item)}>
                <strong>{labelOf(item)}</strong>
                <small>
                  {String(
                    item.description ?? item.mode ?? item.source ?? "Available",
                  )}
                </small>
              </article>
            ))}
          </div>
        ))}
      </div>
    </section>
  );
}

function HistoryView({ history }: { history: EventEnvelope[] }) {
  const exportHistory = () => {
    const url = URL.createObjectURL(
      new Blob([JSON.stringify(history, null, 2)], {
        type: "application/json",
      }),
    );
    const link = document.createElement("a");
    link.href = url;
    link.download = "personal-agent-history.json";
    link.click();
    URL.revokeObjectURL(url);
  };
  return (
    <section className="history-page">
      <SectionHeader
        eyebrow="ENCRYPTED EVENT STREAM"
        title="History and audit trail"
        actions={<button onClick={exportHistory}>Export JSON</button>}
      />
      <ol>
        {[...history].reverse().map((event) => (
          <li key={event.event_id}>
            <b>{event.monotonic_sequence}</b>
            <div>
              <strong>{event.type}</strong>
              <small>
                {event.origin} · {event.wall_clock_timestamp}
              </small>
              <pre>{JSON.stringify(eventPayload(event), null, 2)}</pre>
            </div>
          </li>
        ))}
      </ol>
    </section>
  );
}
function BrowserView({ config }: { config: AppConfig }) {
  const browser = config.browser as Json;
  return (
    <section className="browser-page">
      <SectionHeader
        eyebrow="ISOLATED BROWSER"
        title="Browser automation boundaries"
      />
      <div className="browser-frame">
        <div className="browser-address">
          <span>◎</span>
          <input
            readOnly
            value={
              browser.enabled
                ? "Isolated browser enabled"
                : "Browser disabled in configuration"
            }
          />
          <button disabled={!browser.enabled}>Open profile</button>
        </div>
        <Empty
          title={
            browser.enabled
              ? "Browser profile is ready for an agent session"
              : "Browser automation is off"
          }
          detail="Enable it in Settings. Isolated profiles, quarantined downloads, domain controls, and personal-profile opt-in are enforced by configuration."
        />
      </div>
      <div className="policy-cards">
        {Object.entries(browser).map(([name, value]) => (
          <article key={name}>
            <strong>{name.replaceAll("_", " ")}</strong>
            <span>
              {Array.isArray(value)
                ? value.join(", ") || "none"
                : String(value)}
            </span>
          </article>
        ))}
      </div>
    </section>
  );
}
function DiagnosticsView({
  diagnostic,
  catalog,
  projection,
  voice,
}: {
  diagnostic: Diagnostic;
  catalog: RuntimeCatalog;
  projection: Projection;
  voice: VoiceStatus;
}) {
  const voiceHealthy = voice.stt_ready && voice.tts_ready;
  const diagnosticBundle = JSON.stringify(
    {
      generated_at: new Date().toISOString(),
      diagnostic,
      runtime_healthy: projection.runtime_healthy,
      voice,
      catalog: Object.fromEntries(
        Object.entries(catalog).map(([name, resource]) => [
          name,
          {
            available: resource.available,
            reason: resource.reason ?? null,
          },
        ]),
      ),
    },
    null,
    2,
  );
  return (
    <section className="diagnostics-page">
      <SectionHeader eyebrow="SYSTEM HEALTH" title="Diagnostics" />
      <div className="diagnostic-summary">
        <div>
          <strong>
            {projection.runtime_healthy && voiceHealthy
              ? "All core systems are ready"
              : "One or more systems need attention"}
          </strong>
          <small>
            Runtime, speech, and provider discovery report their real state—no
            simulated readiness.
          </small>
        </div>
        <button
          onClick={() => void navigator.clipboard.writeText(diagnosticBundle)}
        >
          Copy diagnostic bundle
        </button>
      </div>
      {(!projection.runtime_healthy || !voiceHealthy) && (
        <div className="diagnostic-recovery" role="status">
          <b>RECOVERY AVAILABLE</b>
          <span>
            {!projection.runtime_healthy
              ? "OpenCode is not reporting healthy. Check Providers & models, then retry discovery."
              : "Open Voice Lab to install or select a working local speech pipeline."}
          </span>
        </div>
      )}
      <div className="health-grid">
        <article>
          <span>01</span>
          <strong>OpenCode runtime</strong>
          <b className={projection.runtime_healthy ? "good" : "warn"}>
            {projection.runtime_healthy ? "READY" : "DEGRADED"}
          </b>
          <small>
            {diagnostic.opencode.pinned} · {diagnostic.opencode.topology}
          </small>
        </article>
        <article>
          <span>02</span>
          <strong>Speech to text</strong>
          <b className={voice.stt_ready ? "good" : "warn"}>
            {voice.stt_ready ? "READY" : "UNAVAILABLE"}
          </b>
          <small>
            {voice.active_stt_backend ||
              voice.whisper_executable ||
              "Install STT in Voice Lab"}
          </small>
        </article>
        <article>
          <span>03</span>
          <strong>Text to speech</strong>
          <b className={voice.tts_ready ? "good" : "warn"}>
            {voice.tts_ready ? "READY" : "UNAVAILABLE"}
          </b>
          <small>
            {voice.active_tts_backend ||
              voice.piper_executable ||
              "Install TTS in Voice Lab"}
          </small>
        </article>
        <article>
          <span>04</span>
          <strong>Neural turn detector</strong>
          <b className={voice.smart_turn_ready ? "good" : "warn"}>
            {voice.smart_turn_ready ? "READY" : "FALLBACK"}
          </b>
          <small>
            {voice.smart_turn_ready ? "Smart Turn v3.2" : "Adaptive silence"}
          </small>
        </article>
      </div>
      <section className="voice-state-reference">
        <header>
          <div>
            <span className="eyebrow">VOICE STATE REFERENCE</span>
            <strong>One visible state for every stage</strong>
          </div>
          <small>Header status uses these same live signals</small>
        </header>
        <div>
          {Object.entries(voiceStates).map(([state, presentation]) => (
            <article key={state}>
              <b style={{ color: presentation.color }}>{presentation.glyph}</b>
              <span>
                <strong>{presentation.label}</strong>
                <small>{state.replaceAll("_", " ")}</small>
              </span>
            </article>
          ))}
        </div>
      </section>
      <h3 className="diagnostic-table-title">Capability and resource truth</h3>
      <div className="capability-table">
        {diagnostic.capabilities.map((item) => (
          <div key={item.id}>
            <strong>{item.id}</strong>
            <span>{item.backend}</span>
            <b>
              {typeof item.status === "string"
                ? item.status
                : item.status.state}
            </b>
          </div>
        ))}
        {Object.entries(catalog).map(([name, resource]) => (
          <div key={name}>
            <strong>{name}</strong>
            <span>{resource.reason ?? "Authenticated sidecar API"}</span>
            <b className={resource.available ? "good" : "warn"}>
              {resource.available ? "AVAILABLE" : "UNAVAILABLE"}
            </b>
          </div>
        ))}
      </div>
      <h3 className="diagnostic-table-title">Implementation audit</h3>
      <div className="feature-audit" aria-label="Implementation audit">
        {featureAudit.map((item) => (
          <article key={item.area}>
            <span
              className={`audit-status audit-${item.status}`}
              aria-label={item.status.replaceAll("_", " ")}
            >
              {item.status === "implemented"
                ? "✓"
                : item.status === "partial"
                  ? "◐"
                  : "○"}
            </span>
            <div>
              <strong>{item.area}</strong>
              <small>{item.detail}</small>
            </div>
            <b>{item.status.replaceAll("_", " ")}</b>
          </article>
        ))}
      </div>
    </section>
  );
}
function UsageView({ history }: { history: EventEnvelope[] }) {
  const runtime = history.filter(
    (event) =>
      event.origin.includes("runtime") || event.type.startsWith("response."),
  );
  const tools = history.filter((event) => event.type.includes("tool"));
  return (
    <section className="usage-page">
      <SectionHeader
        eyebrow="LOCAL ACCOUNTING"
        title="Usage, cost and outbound data"
      />
      <div className="metric-cards">
        <article>
          <b>{runtime.length}</b>
          <span>runtime events</span>
        </article>
        <article>
          <b>{tools.length}</b>
          <span>tool events</span>
        </article>
        <article>
          <b>0</b>
          <span>secret values recorded</span>
        </article>
        <article>
          <b>{new Set(history.map((event) => event.origin)).size}</b>
          <span>event origins</span>
        </article>
      </div>
      <p>
        Provider token and cost totals appear when emitted by the selected
        model. Egress records preserve destination, data kind, purpose, and size
        without storing credentials.
      </p>
    </section>
  );
}

export function App() {
  const [active, setActive] = useState<Destination>("Chat");
  const [config, setConfig] = useState<AppConfig>(fallbackConfig);
  const [catalog, setCatalog] = useState<RuntimeCatalog>({});
  const [projection, setProjection] = useState<Projection>(emptyProjection);
  const [history, setHistory] = useState<EventEnvelope[]>([]);
  const [voice, setVoice] = useState<VoiceStatus>(fallbackVoice);
  const [diagnostic, setDiagnostic] = useState<Diagnostic>(fallbackDiagnostic);
  const [selectedModel, setSelectedModel] = useState("");
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [activeSession, setActiveSession] = useState("");
  const [palette, setPalette] = useState(false);
  const [paletteQuery, setPaletteQuery] = useState("");
  const [bootError, setBootError] = useState("");
  const [booting, setBooting] = useState(true);
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [settingsSection, setSettingsSection] = useState("voice");
  const [voiceUi, setVoiceUi] = useState<VoicePresentation>(() =>
    voicePresentation("offline"),
  );
  useEffect(() => {
    void invoke<Bootstrap>("bootstrap")
      .then((data) => {
        setConfig(data.config);
        setCatalog(data.catalog);
        setProjection(data.projection);
        setHistory(data.history);
        setVoice(data.voice);
        const projectedSession = data.projection.active_session ?? "";
        const availableSessions = asArray(
          resourceData(data.catalog, "sessions", []),
        );
        setActiveSession(
          availableSessions.some(
            (session) => String(session.id) === projectedSession,
          )
            ? projectedSession
            : "",
        );
        document.documentElement.dataset.theme = data.config.ui.accent;
      })
      .catch((caught) => setBootError(String(caught)))
      .finally(() => setBooting(false));
    void invoke<Diagnostic>("diagnostics")
      .then(setDiagnostic)
      .catch(() => undefined);
    void invoke<boolean>("autostart_status")
      .then(setAutostart)
      .catch(() => setAutostart(false));
  }, []);
  useEffect(() => {
    document.documentElement.style.fontSize = `${config.ui.text_scale_percent}%`;
    document.documentElement.lang = config.ui.locale;
    document.documentElement.classList.toggle(
      "reduce-motion",
      config.ui.reduced_motion,
    );
    document.documentElement.dataset.theme = config.ui.accent;
  }, [config.ui]);
  useEffect(() => {
    const models = resourceData<RuntimeCapability[]>(catalog, "models", []);
    if (!models.length) return;
    const ids = new Set(
      models.map((item) => `${item.provider_id}/${item.model_id}`),
    );
    if (selectedModel && ids.has(selectedModel)) return;
    const providerResource = resourceData<Json>(catalog, "providers", {});
    const connected = Array.isArray(providerResource.connected)
      ? providerResource.connected.map(String)
      : [];
    const providerConfig = resourceData<Json>(catalog, "config_providers", {});
    const defaults =
      providerConfig.default && typeof providerConfig.default === "object"
        ? (providerConfig.default as Json)
        : {};
    const configured = config.runtime.default_model
      ? config.runtime.default_model.includes("/")
        ? config.runtime.default_model
        : `${config.runtime.default_provider}/${config.runtime.default_model}`
      : "";
    const candidate = [
      configured,
      ...connected.map((provider) =>
        defaults[provider] ? `${provider}/${String(defaults[provider])}` : "",
      ),
      ...models.map((item) => `${item.provider_id}/${item.model_id}`),
    ].find((id) => ids.has(id));
    if (candidate) setSelectedModel(candidate);
  }, [catalog, config.runtime, selectedModel]);
  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setPalette((value) => !value);
      }
      if (event.key === "Escape") setPalette(false);
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, []);
  const filteredNavigation = useMemo(
    () =>
      navigation.filter((item) =>
        item.toLowerCase().includes(paletteQuery.toLowerCase()),
      ),
    [paletteQuery],
  );
  const refreshCatalog = useCallback(
    (next: RuntimeCatalog) => setCatalog(next),
    [],
  );
  const addHistory = useCallback(
    (event: EventEnvelope) =>
      setHistory((current) =>
        current.some((item) => item.event_id === event.event_id)
          ? current
          : [...current, event],
      ),
    [],
  );
  const toggleAutostart = () => {
    if (autostart === null) return;
    void invoke<boolean>("set_autostart", { enabled: !autostart })
      .then(setAutostart)
      .catch((caught) => setBootError(String(caught)));
  };
  let content: React.ReactNode;
  if (active === "Chat")
    content = (
      <ChatView
        config={config}
        catalog={catalog}
        projection={projection}
        voiceStatus={voice}
        model={selectedModel}
        setModel={setSelectedModel}
        messages={messages}
        setMessages={setMessages}
        activeSession={activeSession}
        setActiveSession={setActiveSession}
        onProjection={setProjection}
        onHistory={addHistory}
        onCatalog={refreshCatalog}
        onVoice={setVoice}
        onVoicePresentation={setVoiceUi}
        onOpenProviders={() => {
          setSettingsSection("providers");
          setActive("Settings");
        }}
      />
    );
  else if (
    ["Goals & tasks", "Memory", "Automations", "Artifacts"].includes(active)
  )
    content = (
      <DomainView
        destination={active}
        history={history}
        catalog={catalog}
        projection={projection}
        setHistory={setHistory}
        setCatalog={setCatalog}
        setProjection={setProjection}
      />
    );
  else if (active === "Projects & terminal")
    content = (
      <ProjectView config={config} catalog={catalog} onCatalog={setCatalog} />
    );
  else if (active === "Integrations")
    content = (
      <IntegrationsView
        catalog={catalog}
        config={config}
        onCatalog={setCatalog}
      />
    );
  else if (active === "Skills & agents")
    content = <CatalogView destination={active} catalog={catalog} />;
  else if (active === "History") content = <HistoryView history={history} />;
  else if (active === "Browser") content = <BrowserView config={config} />;
  else if (active === "Diagnostics")
    content = (
      <DiagnosticsView
        diagnostic={diagnostic}
        catalog={catalog}
        projection={projection}
        voice={voice}
      />
    );
  else if (active === "Usage & egress")
    content = <UsageView history={history} />;
  else
    content = (
      <ConfigEditor
        config={config}
        catalog={catalog}
        voice={voice}
        autostart={autostart}
        onAutostart={toggleAutostart}
        onConfig={setConfig}
        onVoice={setVoice}
        onCatalog={setCatalog}
        initialSection={settingsSection}
      />
    );
  return (
    <div
      className={`app-shell ${config.ui.compact_sidebar ? "compact-sidebar" : ""}`}
    >
      <header className="app-chrome">
        <div className="chrome-brand">
          <span>
            <i />
          </span>
          <strong>Personal Agent</strong>
          <small>BOUNDED</small>
        </div>
        <button
          className={`global-voice-status ${voiceUi.state}`}
          aria-label={`${voiceUi.label}. ${voiceUi.hint}`}
          onClick={() => {
            if (voiceUi.stoppable)
              window.dispatchEvent(new Event("personal-agent:voice-stop"));
            else {
              setSettingsSection("voice");
              setActive("Settings");
            }
          }}
        >
          <b style={{ color: voiceUi.color }}>{voiceUi.glyph}</b>
          <strong>{voiceUi.label}</strong>
          <span>{voiceUi.hint}</span>
          <i>
            <em
              style={{
                width: `${Math.max(3, voiceUi.level * 100)}%`,
                background: voiceUi.color,
              }}
            />
          </i>
        </button>
        <div className="chrome-actions">
          <button
            className="global-model-trigger"
            aria-label={`Model selector: ${selectedModel || "choose model"}`}
            onClick={() => {
              setActive("Chat");
              window.requestAnimationFrame(() =>
                window.dispatchEvent(new Event("personal-agent:model-palette")),
              );
            }}
          >
            <b>
              {(selectedModel.split("/")[0] || "A").slice(0, 1).toUpperCase()}
            </b>
            <span>{selectedModel || "Choose model"}</span>
            <i>⌄</i>
          </button>
          <kbd>⌘K</kbd>
          <button
            aria-label="Open settings"
            onClick={() => setActive("Settings")}
          >
            ⚙
          </button>
        </div>
      </header>
      <aside className="sidebar">
        <div className="nav-heading">
          <span>WORKSPACE</span>
          <b>⌄</b>
        </div>
        <nav>
          {navigation.map((item) => (
            <button
              key={item}
              className={active === item ? "active" : ""}
              aria-current={active === item ? "page" : undefined}
              aria-label={item}
              onClick={() => setActive(item)}
            >
              <i>{icon(item)}</i>
              <span>{item}</span>
              {item === "Goals & tasks" && projection.tasks_running > 0 && (
                <b>{projection.tasks_running}</b>
              )}
            </button>
          ))}
        </nav>
        <div className="profile">
          <span>YK</span>
          <div>
            <strong>Studio profile</strong>
            <small>● local · offline-capable</small>
          </div>
        </div>
      </aside>
      <main>
        <h1 className="sr-only">{active}</h1>
        {bootError && (
          <div className="boot-error">
            {bootError}
            <button onClick={() => setBootError("")}>×</button>
          </div>
        )}
        <div className="content">
          {content}
          {booting && (
            <div className="startup-shield" role="status">
              <span className="thinking-pulse" />
              <strong>Starting your private agent…</strong>
              <small>Connecting OpenCode and checking local voice</small>
            </div>
          )}
        </div>
        <footer className="app-footer">
          <span
            className={
              booting ? "" : projection.runtime_healthy ? "good" : "warn"
            }
          >
            ● CORE{" "}
            {booting
              ? "STARTING"
              : projection.runtime_healthy
                ? "ONLINE"
                : "DEGRADED"}
          </span>
          <span>
            MICROPHONE {projection.microphone_active ? "LIVE" : "PRIVATE"}
          </span>
          <span>PRIVATE MODE</span>
          <span className="footer-right">
            LINUX · X86_64 <b>v{diagnostic.version}</b>
          </span>
        </footer>
      </main>
      {palette && (
        <div className="palette-backdrop" onMouseDown={() => setPalette(false)}>
          <section
            className="command-palette"
            role="dialog"
            aria-label="COMMAND PALETTE"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <header>
              <span>⌕</span>
              <input
                autoFocus
                value={paletteQuery}
                placeholder="Go to…"
                onChange={(event) => setPaletteQuery(event.target.value)}
              />
              <kbd>ESC</kbd>
            </header>
            {filteredNavigation.map((item) => (
              <button
                key={item}
                onClick={() => {
                  setActive(item);
                  setPalette(false);
                  setPaletteQuery("");
                }}
              >
                <i>{icon(item)}</i>
                <span>{item}</span>
              </button>
            ))}
          </section>
        </div>
      )}
    </div>
  );
}
