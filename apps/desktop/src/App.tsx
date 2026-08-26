import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ConfigEditor } from "./ConfigEditor";
import type { AppConfig, Bootstrap, EventEnvelope, Projection, RuntimeCatalog, RuntimeCapability, VoiceStatus } from "./types";
import { eventPayload } from "./types";
import { useVoiceCapture } from "./useVoiceCapture";

const navigation = [
  "Chat", "Goals & tasks", "Browser", "Projects & terminal", "Artifacts", "History",
  "Memory", "Automations", "Integrations", "Skills & agents", "Usage & egress", "Diagnostics", "Settings",
] as const;
type Destination = (typeof navigation)[number];
type Json = Record<string, unknown>;
type ChatMessage = { id: string; role: "user" | "assistant" | "system"; text: string; streaming?: boolean };
type Attachment = { name: string; mime: string; url: string; size: number };

type Diagnostic = {
  product: string; version: string; platform: string; arch: string;
  opencode: { pinned: string; topology: string };
  capabilities: Array<{ id: string; backend: string; status: { state: string } | string }>;
};

const emptyProjection: Projection = {
  last_sequence: 0, active_profile: "default", active_session: null, goals_total: 0,
  tasks_running: 0, approvals_waiting: 0, microphone_active: false, runtime_healthy: false,
  unclean_shutdowns: 0, recovered_unclean_run: false,
};

const fallbackConfig: AppConfig = {
  schema_version: 1,
  persona: { name: "JARVIS", style: "Composed, concise, and quietly witty." },
  agent: { default_parallelism: 3, max_delegation_depth: 3, require_plan_for_multistep: true, verify_success_criteria: true, default_token_budget: 0, default_cost_budget_microusd: 0, default_wall_time_minutes: 0, default_tool_call_budget: 0 },
  runtime: { opencode_version: "1.18.23", startup_timeout_ms: 30000, default_provider: "", default_model: "", small_model: "", default_agent: "build", default_effort: "", working_directory: "/", auto_compact: true },
  privacy: { record_transcripts: true, record_tool_arguments: false, transcript_retention_days: 90, redact_secrets: true, guest_mode_by_default: false, analytics: false },
  ui: { theme: "midnight", accent: "cyan", locale: "en", text_scale_percent: 100, reduced_motion: false, hud_enabled: true, start_in_hud: false, overlay: false, show_reasoning: true, show_tool_details: true, session_tabs: true, compact_sidebar: false, command_palette_hotkey: "Ctrl+K", global_hotkey: "Ctrl+Space" },
  voice: { enabled: true, mode: "push-to-talk", input_device: "", output_device: "", language: "en", response_language: "en", stt_backend: "whisper.cpp", stt_model: "base", stt_executable: "", stt_model_path: "", tts_backend: "piper", tts_voice: "en_US-lessac-medium", tts_executable: "", tts_model_path: "", speech_rate_percent: 100, volume_percent: 100, input_gain_percent: 100, ducking_percent: 30, wake_phrases: ["hey jarvis", "jarvis"], stop_phrases: ["stop", "cancel"], sleep_phrases: ["go to sleep"], wake_threshold_milli: 930, vad_start_milli: 600, vad_stop_milli: 350, endpoint_short_ms: 700, endpoint_long_ms: 1400, pre_roll_ms: 500, refractory_ms: 2000, wake_enabled: false, push_to_talk: true, push_to_talk_hotkey: "Space", barge_in: true, echo_cancellation: true, noise_suppression: true, automatic_gain_control: true, offline_only: true, speak_typed_responses: false, quiet_mode: false, speaker_verification: false, meeting_speaker_labels: false, vocabulary: [], hosted_stt_credential_alias: "", hosted_tts_credential_alias: "" },
  workspace: { default_project: "/", restore_sessions: true, confirm_session_delete: true, open_files_in_app: true, terminal_shell: "/bin/sh", attachment_limit_mb: 25, diff_viewer: true },
  browser: { enabled: false, isolated_profiles: true, personal_profile_opt_in: false, quarantine_downloads: true, allow_third_party_subresources: false, allowed_domains: [], blocked_domains: [] },
  memory: { enabled: true, inferred_memory_requires_review: true, recall_limit: 12, embedding_model: "multilingual-e5-small" },
  automation: { enabled: true, max_concurrency: 2, pause_after_failures: 3, quiet_hours_start: "", quiet_hours_end: "", missed_run_policy: "run-once" },
  notifications: { enabled: true, goal_completion: true, approvals: true, automation_failures: true, sound: true },
  updates: { channel: "stable", check_automatically: true, download_automatically: false },
  opencode: {}, secret_aliases: [], risk_acknowledgements: [],
};

const fallbackVoice: VoiceStatus = { stt_ready: false, tts_ready: false, playback_ready: false, details: ["Native voice status is not available."] };
const fallbackDiagnostic: Diagnostic = { product: "Personal Agent", version: "0.1.0", platform: "local", arch: "unknown", opencode: { pinned: "1.18.23", topology: "authenticated-loopback-sidecar" }, capabilities: [] };

function icon(name: string) {
  const symbols: Record<string, string> = { Chat: "◫", "Goals & tasks": "✓", Browser: "◎", "Projects & terminal": "⌘", Artifacts: "◇", History: "↶", Memory: "◉", Automations: "⌁", Integrations: "⊞", "Skills & agents": "♙", "Usage & egress": "⌁", Diagnostics: "△", Settings: "⚙" };
  return symbols[name] ?? "□";
}

function resourceData<T = unknown>(catalog: RuntimeCatalog, name: string, fallback: T): T {
  const resource = catalog[name];
  return resource?.available && resource.data !== undefined ? resource.data as T : fallback;
}

function asArray(value: unknown): Json[] {
  if (Array.isArray(value)) return value.filter((item): item is Json => Boolean(item) && typeof item === "object");
  if (value && typeof value === "object") return Object.entries(value as Json).map(([name, item]) => typeof item === "object" && item ? { name, ...(item as Json) } : { name, value: item });
  return [];
}

function labelOf(item: Json, fallback = "Untitled") {
  return String(item.title ?? item.name ?? item.id ?? item.slug ?? fallback);
}

function extractMessages(value: unknown): ChatMessage[] {
  return asArray(value).flatMap((entry, index) => {
    const info = (entry.info ?? entry) as Json;
    const role = info.role === "assistant" ? "assistant" : info.role === "user" ? "user" : "system";
    const parts = Array.isArray(entry.parts) ? entry.parts : [];
    const text = parts.map((part) => typeof part === "object" && part && (part as Json).type === "text" ? String((part as Json).text ?? "") : "").join("");
    return text ? [{ id: String(info.id ?? `history-${index}`), role, text }] : [];
  });
}

function records(history: EventEnvelope[], prefix: string) {
  return history.filter((event) => event.type.startsWith(prefix)).map((event) => ({ event, payload: eventPayload(event) })).reverse();
}

function SectionHeader({ eyebrow, title, actions }: { eyebrow: string; title: string; actions?: React.ReactNode }) {
  return <header className="section-header"><div><span className="eyebrow">{eyebrow}</span><h2>{title}</h2></div><div className="header-actions">{actions}</div></header>;
}

function Empty({ title, detail }: { title: string; detail: string }) {
  return <div className="empty"><span>◇</span><strong>{title}</strong><p>{detail}</p></div>;
}

function ChatView({ config, catalog, projection, voiceStatus, messages, setMessages, activeSession, setActiveSession, onProjection, onHistory, onCatalog, onVoice }: {
  config: AppConfig; catalog: RuntimeCatalog; projection: Projection; voiceStatus: VoiceStatus;
  messages: ChatMessage[]; setMessages: React.Dispatch<React.SetStateAction<ChatMessage[]>>;
  activeSession: string; setActiveSession: (id: string) => void; onProjection: (p: Projection) => void;
  onHistory: (event: EventEnvelope) => void; onCatalog: (catalog: RuntimeCatalog) => void; onVoice: (status: VoiceStatus) => void;
}) {
  const [composer, setComposer] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [model, setModel] = useState(config.runtime.default_model);
  const [agent, setAgent] = useState(config.runtime.default_agent);
  const [effort, setEffort] = useState(config.runtime.default_effort);
  const [sessionMenu, setSessionMenu] = useState(false);
  const endRef = useRef<HTMLDivElement>(null);
  const sessions = asArray(resourceData(catalog, "sessions", []));
  const models = resourceData<RuntimeCapability[]>(catalog, "models", []);
  const agents = asArray(resourceData(catalog, "agents", []));
  const permissions = asArray(resourceData(catalog, "permissions", []));
  const questions = asArray(resourceData(catalog, "questions", []));

  const refreshCatalog = useCallback(async () => {
    try { onCatalog(await invoke<RuntimeCatalog>("runtime_catalog", { directory: config.runtime.working_directory })); } catch (caught) { setError(String(caught)); }
  }, [config.runtime.working_directory, onCatalog]);

  const send = useCallback(async (raw: string, fromVoice = false) => {
    const text = raw.trim();
    if (!text || busy) return;
    setBusy(true); setError("");
    setMessages((current) => [...current, { id: crypto.randomUUID(), role: "user", text }, { id: "streaming", role: "assistant", text: "", streaming: true }]);
    setComposer("");
    try {
      if (text.startsWith("/") && activeSession) {
        const [command, ...args] = text.slice(1).split(/\s+/);
        const result = await invoke<Json>("runtime_operation", { kind: "session_command", sessionId: activeSession, directory: config.runtime.working_directory, payload: { command, arguments: args.join(" "), agent, model, variant: effort }, confirmed: false });
        const extracted = extractMessages([result]);
        setMessages((current) => current.filter((item) => item.id !== "streaming").concat(extracted));
        setBusy(false);
      } else {
        const response = await invoke<{ session_id: string; projection: Projection }>("chat_send", {
          text, directory: config.runtime.working_directory, model, agent, effort,
          speakResponse: fromVoice || config.voice.speak_typed_responses,
          attachments: attachments.map((item) => ({ type: "file", mime: item.mime, filename: item.name, url: item.url })),
        });
        setActiveSession(response.session_id); onProjection(response.projection); setAttachments([]);
      }
    } catch (caught) {
      setMessages((current) => current.filter((item) => item.id !== "streaming")); setError(String(caught)); setBusy(false);
    }
  }, [activeSession, agent, attachments, busy, config.runtime.working_directory, config.voice.speak_typed_responses, effort, model, onProjection, setActiveSession, setMessages]);

  const voice = useVoiceCapture(config, (transcript) => { setComposer(transcript); void send(transcript, true); }, onProjection);

  useEffect(() => {
    if (typeof endRef.current?.scrollIntoView === "function") {
      endRef.current.scrollIntoView({ behavior: config.ui.reduced_motion ? "auto" : "smooth" });
    }
  }, [config.ui.reduced_motion, messages]);
  useEffect(() => {
    const unlisten: Array<() => void> = [];
    void listen<EventEnvelope>("runtime-event", ({ payload }) => {
      onHistory(payload);
      if (payload.type === "response.delta") {
        const delta = String(eventPayload(payload).delta ?? eventPayload(payload).text ?? "");
        if (delta) setMessages((current) => current.map((item) => item.id === "streaming" ? { ...item, text: item.text + delta } : item));
      }
    }).then((fn) => unlisten.push(fn));
    void listen<{ text: string; speak: boolean }>("runtime-turn-complete", ({ payload }) => {
      setMessages((current) => current.map((item) => item.id === "streaming" ? { ...item, id: crypto.randomUUID(), text: item.text || payload.text || "Completed without a text response.", streaming: false } : item));
      setBusy(false); void refreshCatalog();
      if (payload.speak && payload.text && voiceStatus.tts_ready) void invoke("voice_speak", { text: payload.text }).catch((caught) => setError(String(caught)));
    }).then((fn) => unlisten.push(fn));
    return () => unlisten.forEach((fn) => fn());
  }, [onHistory, refreshCatalog, setMessages, voiceStatus.tts_ready]);

  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      if (event.code !== "Space" || !config.voice.push_to_talk || !config.voice.enabled) return;
      const target = event.target as HTMLElement | null;
      if (target?.matches("input, textarea, select, [contenteditable=true]")) return;
      event.preventDefault();
      if (event.type === "keydown" && !event.repeat && voice.state !== "listening") {
        if (config.voice.barge_in) void invoke("voice_stop");
        void voice.start();
      }
      if (event.type === "keyup" && voice.state === "listening") void voice.stop();
    };
    window.addEventListener("keydown", shortcut); window.addEventListener("keyup", shortcut);
    return () => { window.removeEventListener("keydown", shortcut); window.removeEventListener("keyup", shortcut); };
  }, [config.voice.barge_in, config.voice.enabled, config.voice.push_to_talk, voice]);

  const sessionAction = async (action: string, sessionId = activeSession) => {
    setError("");
    try {
      const title = action === "rename" ? window.prompt("New session title") ?? "" : null;
      if (action === "delete" && !window.confirm("Delete this session permanently?")) return;
      if (action === "share" && !window.confirm("Create a public share link for this session?")) return;
      const result = await invoke<Json>("session_action", { action, sessionId: sessionId || null, directory: config.runtime.working_directory, title, confirmed: ["delete", "share"].includes(action) });
      const id = String(result.session_id ?? sessionId ?? "");
      if (["new", "resume", "fork"].includes(action) && id) {
        setActiveSession(id);
        if (action === "resume") {
          const history = await invoke("runtime_resource", { kind: "session_messages", sessionId: id, directory: config.runtime.working_directory, path: null, query: null });
          setMessages(extractMessages(history));
        } else setMessages([]);
      }
      await refreshCatalog(); setSessionMenu(false);
    } catch (caught) { setError(String(caught)); }
  };

  const addFiles = async (files: FileList | null) => {
    if (!files) return;
    const maximum = Number(config.workspace.attachment_limit_mb ?? 25) * 1024 * 1024;
    try {
      const next = await Promise.all(Array.from(files).map(async (file) => {
        if (file.size > maximum) throw new Error(`${file.name} exceeds the ${config.workspace.attachment_limit_mb} MiB attachment limit`);
        const url = await new Promise<string>((resolve, reject) => { const reader = new FileReader(); reader.onload = () => resolve(String(reader.result)); reader.onerror = () => reject(reader.error); reader.readAsDataURL(file); });
        return { name: file.name, mime: file.type || "application/octet-stream", url, size: file.size };
      }));
      setAttachments((current) => [...current, ...next]);
    } catch (caught) { setError(String(caught)); }
  };

  return <div className="chat-layout">
    <aside className="session-rail"><button className="new-session" onClick={() => void sessionAction("new", "")}>＋ New session</button><div className="session-list">{sessions.length ? sessions.map((session) => <button key={String(session.id)} className={activeSession === session.id ? "active" : ""} onClick={() => void sessionAction("resume", String(session.id))}><strong>{labelOf(session, "Session")}</strong><small>{String(session.id)}</small></button>) : <p>No saved sessions</p>}</div></aside>
    <section className="conversation">
      <div className="conversation-toolbar"><select aria-label="Model" value={model} onChange={(event) => setModel(event.target.value)}><option value="">Default model</option>{models.map((item) => <option key={`${item.provider_id}/${item.model_id}`} value={`${item.provider_id}/${item.model_id}`}>{item.provider_id} / {item.model_id}</option>)}</select><select aria-label="Agent" value={agent} onChange={(event) => setAgent(event.target.value)}><option value="build">Build</option>{agents.map((item) => <option key={labelOf(item)} value={String(item.name ?? item.id)}>{labelOf(item)}</option>)}</select><select aria-label="Reasoning effort" value={effort} onChange={(event) => setEffort(event.target.value)}><option value="">Default effort</option><option>low</option><option>medium</option><option>high</option><option>xhigh</option></select><span className={`runtime-dot ${projection.runtime_healthy ? "online" : "offline"}`}>{projection.runtime_healthy ? "Runtime connected" : "Runtime unavailable"}</span><div className="session-actions"><button onClick={() => setSessionMenu((value) => !value)}>•••</button>{sessionMenu && <div>{["rename", "fork", "compact", busy ? "abort" : "", "share", "unshare", "delete"].filter(Boolean).map((action) => <button key={action} onClick={() => void sessionAction(action)} disabled={!activeSession}>{action}</button>)}</div>}</div></div>
      <div className="messages" aria-live="polite">{!messages.length && <div className="welcome"><div className="reactor-mini"><i /><span /></div><span className="eyebrow">{config.persona.name} · LOCAL CORE</span><h2>What shall we build?</h2><p>Use local voice, attach files or images, run slash commands, and let the bounded OpenCode runtime work inside <b>{config.runtime.working_directory}</b>.</p><div>{["Inspect this project", "Explain the current changes", "Run the test suite"].map((text) => <button key={text} onClick={() => setComposer(text)}>{text}</button>)}</div></div>}{messages.map((message) => <article key={message.id} className={`chat-message ${message.role}`}><div className="message-avatar">{message.role === "user" ? "Y" : "J"}</div><div><header><strong>{message.role === "user" ? "You" : config.persona.name}</strong>{message.streaming && <span>thinking / working…</span>}</header><p>{message.text || "…"}</p>{message.role === "assistant" && message.text && <div className="message-actions"><button onClick={() => void navigator.clipboard.writeText(message.text)}>Copy</button><button disabled={!voiceStatus.tts_ready} onClick={() => void invoke("voice_speak", { text: message.text })}>Speak</button></div>}</div></article>)}<div ref={endRef} /></div>
      {attachments.length > 0 && <div className="attachments">{attachments.map((item, index) => <span key={`${item.name}-${index}`}>{item.name}<button aria-label={`Remove ${item.name}`} onClick={() => setAttachments((current) => current.filter((_, position) => position !== index))}>×</button></span>)}</div>}{(error || voice.error) && <p className="error-banner" role="alert">{error || voice.error}</p>}
      <form className="composer" onSubmit={(event) => { event.preventDefault(); void send(composer); }}><label className="attach-button" title="Attach images or files">＋<input aria-label="Attach files" type="file" multiple onChange={(event) => void addFiles(event.target.files)} /></label><textarea aria-label="Message JARVIS" rows={1} value={composer} placeholder="Message JARVIS…  (/ for commands, @ for files)" onChange={(event) => setComposer(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void send(composer); } }} /><button type="button" className={`voice-button ${voice.state}`} aria-label={voice.state === "listening" ? "Stop voice capture" : "Start voice capture"} disabled={!voiceStatus.stt_ready || voice.state === "transcribing"} onClick={() => voice.state === "listening" ? void voice.stop() : void voice.start()}><span style={{ transform: `scale(${1 + voice.level * .35})` }}>●</span>{voice.state === "transcribing" ? "…" : "MIC"}</button><button className="send-button" type="submit" disabled={!composer.trim() || busy}>↑</button></form>
      <footer className="voice-strip"><span className={projection.microphone_active ? "live" : ""}>● {projection.microphone_active ? "MICROPHONE LIVE" : "MICROPHONE PRIVATE"}</span><span>STT {voiceStatus.stt_ready ? "READY" : "MISSING"}</span><span>TTS {voiceStatus.tts_ready ? "READY" : "MISSING"}</span><button onClick={() => void invoke("voice_stop")}>Stop audio</button></footer>
    </section>
    <aside className="activity-rail"><div className="rail-card"><span className="eyebrow">ACTIVE SESSION</span><strong>{activeSession || "No active session"}</strong><small>{config.runtime.working_directory}</small><div className="rail-metrics"><span>{projection.tasks_running}<small>tasks</small></span><span>{projection.approvals_waiting}<small>approvals</small></span></div></div><div className="rail-card"><span className="eyebrow">PERMISSIONS & QUESTIONS</span>{permissions.length + questions.length === 0 ? <p>No requests waiting.</p> : [...permissions, ...questions].map((request) => { const permission = Boolean(request.permission); return <div className="request" key={String(request.id)}><strong>{labelOf(request, "Runtime request")}</strong><small>{String(request.permission ?? request.question ?? "Action requires your answer")}</small><div><button onClick={() => void invoke("runtime_answer", { sessionId: activeSession, requestId: request.id, answer: permission ? { kind: "permission", reply: "once" } : { kind: "question", answers: [["yes"]] } }).then(refreshCatalog)}>Allow once</button><button onClick={() => void invoke("runtime_answer", { sessionId: activeSession, requestId: request.id, answer: permission ? { kind: "permission", reply: "reject" } : { kind: "question", reject: true } }).then(refreshCatalog)}>Reject</button></div></div>; })}</div><div className="rail-card"><span className="eyebrow">VOICE LINK</span><div className="voice-orb"><i /><b /></div><strong>{voiceStatus.stt_ready && voiceStatus.tts_ready ? "Native voice ready" : "Voice setup required"}</strong><small>{voiceStatus.details.join(" ")}</small>{!(voiceStatus.stt_ready && voiceStatus.tts_ready) && <button className="primary" onClick={async () => { try { onVoice(await invoke<VoiceStatus>("voice_install", { component: "all" })); } catch (caught) { setError(String(caught)); } }}>Install offline voice</button>}</div></aside>
  </div>;
}

function DomainView({ destination, history, projection, setHistory, setProjection }: { destination: Destination; history: EventEnvelope[]; projection: Projection; setHistory: React.Dispatch<React.SetStateAction<EventEnvelope[]>>; setProjection: (p: Projection) => void }) {
  const [fields, setFields] = useState({ first: "", second: "", third: "" }); const [error, setError] = useState("");
  const map: Partial<Record<Destination, { domain: string; prefix: string; title: string; empty: string; labels: string[] }>> = {
    "Goals & tasks": { domain: "goal", prefix: "goal.", title: "Durable goals and task graphs", empty: "Create a goal with observable success criteria.", labels: ["Objective", "Success criteria (one per line)", "Priority"] }, Memory: { domain: "memory", prefix: "memory.", title: "Trusted, reviewable memory", empty: "Explicit memories keep provenance and stay under your control.", labels: ["Memory content", "Tier", "Sensitivity"] }, Automations: { domain: "automation", prefix: "automation.", title: "Schedules and monitors", empty: "Automations retain approval, privacy, and failure policies.", labels: ["Name", "Prompt", "Schedule (for example: daily at 09:00)"] }, Artifacts: { domain: "artifact", prefix: "artifact.", title: "Versioned artifacts", empty: "Create a durable text, code, document, image, audio, or report artifact.", labels: ["Title", "Kind", ""] },
  };
  const spec = map[destination]!; const items = records(history, spec.prefix);
  const submit = async (event: FormEvent) => { event.preventDefault(); setError(""); let payload: Json = {}; if (spec.domain === "goal") payload = { objective: fields.first, success_criteria: fields.second.split("\n").filter(Boolean), priority: Number(fields.third || 0) }; if (spec.domain === "memory") payload = { content: fields.first, tier: fields.second || "semantic", sensitivity: fields.third || "private" }; if (spec.domain === "automation") payload = { name: fields.first, prompt: fields.second, schedule: fields.third }; if (spec.domain === "artifact") payload = { title: fields.first, kind: fields.second || "text" }; try { const result = await invoke<{ projection: Projection }>("domain_action", { domain: spec.domain, action: "create", payload }); setProjection(result.projection); setFields({ first: "", second: "", third: "" }); const bootstrap = await invoke<Bootstrap>("bootstrap"); setHistory(bootstrap.history); } catch (caught) { setError(String(caught)); } };
  return <div className="page-grid"><section className="page-main"><SectionHeader eyebrow={destination.toUpperCase()} title={spec.title} /><form className="create-form" onSubmit={submit}>{spec.labels.map((label, index) => label && <label key={label}>{label}{(index === 1 && ["goal", "automation"].includes(spec.domain)) ? <textarea value={fields.second} onChange={(event) => setFields((current) => ({ ...current, second: event.target.value }))} /> : <input value={index === 0 ? fields.first : index === 1 ? fields.second : fields.third} onChange={(event) => setFields((current) => ({ ...current, [index === 0 ? "first" : index === 1 ? "second" : "third"]: event.target.value }))} />}</label>)}<button className="primary">Create</button>{error && <p className="field-error">{error}</p>}</form></section><aside className="page-side"><div className="card-label">RECORDS <b>{items.length}</b></div>{!items.length ? <Empty title="No records yet" detail={spec.empty} /> : <div className="record-list">{items.map(({ event, payload }) => <article key={event.event_id}><strong>{String(payload.objective ?? payload.content ?? payload.name ?? payload.title ?? event.type)}</strong><small>{event.type} · sequence {event.monotonic_sequence}</small><pre>{JSON.stringify(payload, null, 2)}</pre></article>)}</div>}</aside><div className="metric-bar"><span>{projection.goals_total}<small>goals</small></span><span>{projection.tasks_running}<small>running tasks</small></span><span>{projection.approvals_waiting}<small>approvals</small></span></div></div>;
}

function ProjectView({ config, onCatalog }: { config: AppConfig; catalog: RuntimeCatalog; onCatalog: (catalog: RuntimeCatalog) => void }) {
  const [tab, setTab] = useState("files"); const [path, setPath] = useState(""); const [query, setQuery] = useState(""); const [data, setData] = useState<unknown>(null); const [error, setError] = useState("");
  const load = async (kind: string, requestedPath = path) => { setError(""); try { setData(await invoke("runtime_resource", { kind, sessionId: null, directory: config.runtime.working_directory, path: requestedPath || null, query: query || null })); } catch (caught) { setError(String(caught)); } };
  useEffect(() => { void load(tab === "diff" ? "vcs_diff" : tab === "terminal" ? "pty_list" : tab === "worktrees" ? "worktree_list" : "file_list"); }, [tab]);
  const entries = asArray(data);
  return <section className="workbench"><SectionHeader eyebrow="OPENCODE WORKSPACE" title="Projects, files, VCS and terminals" actions={<><input aria-label="Working directory" value={config.runtime.working_directory} readOnly /><button onClick={async () => onCatalog(await invoke("runtime_catalog", { directory: config.runtime.working_directory }))}>Refresh</button></>} /><div className="tabbar">{["files", "search", "diff", "terminal", "worktrees"].map((item) => <button key={item} className={tab === item ? "active" : ""} onClick={() => setTab(item)}>{item}</button>)}</div>{error && <p className="error-banner">{error}</p>}{tab === "files" && <div className="file-workspace"><aside><div className="path-input"><input value={path} placeholder="relative path" onChange={(event) => setPath(event.target.value)} /><button onClick={() => void load("file_list")}>Open</button></div>{entries.map((entry) => <button key={labelOf(entry)} onClick={() => { const next = String(entry.path ?? entry.name ?? ""); setPath(next); void load(entry.type === "directory" ? "file_list" : "file_content", next); }}>{String(entry.type ?? "file") === "directory" ? "▸" : "·"} {labelOf(entry)}</button>)}</aside><pre className="code-view">{typeof data === "string" ? data : JSON.stringify(data, null, 2)}</pre></div>}{tab === "search" && <div className="search-view"><div><input value={query} placeholder="Search files, text or symbols" onChange={(event) => setQuery(event.target.value)} /><button onClick={() => void load("find_text")}>Text</button><button onClick={() => void load("find_file")}>Files</button><button onClick={() => void load("find_symbol")}>Symbols</button></div><pre>{JSON.stringify(data, null, 2)}</pre></div>}{tab === "diff" && <pre className="diff-view">{JSON.stringify(data, null, 2)}</pre>}{tab === "terminal" && <div className="terminal-view"><div className="terminal-toolbar"><button onClick={async () => { try { setData(await invoke("runtime_operation", { kind: "pty_create", identifier: null, sessionId: null, directory: config.runtime.working_directory, payload: { command: config.workspace.terminal_shell, cwd: config.runtime.working_directory, title: "Personal Agent terminal" }, confirmed: false })); } catch (caught) { setError(String(caught)); } }}>New terminal</button></div><pre>{JSON.stringify(data, null, 2)}</pre><p>PTY lifecycle is native. OpenCode streams interactive terminal bytes over its authenticated PTY channel; active sessions and controls remain visible here.</p></div>}{tab === "worktrees" && <div className="worktree-view"><button className="primary" onClick={async () => { try { setData(await invoke("runtime_operation", { kind: "worktree_create", identifier: null, sessionId: null, directory: config.runtime.working_directory, payload: {}, confirmed: false })); } catch (caught) { setError(String(caught)); } }}>Create isolated worktree</button><pre>{JSON.stringify(data, null, 2)}</pre></div>}</section>;
}

function IntegrationsView({ catalog, config, onCatalog }: { catalog: RuntimeCatalog; config: AppConfig; onCatalog: (catalog: RuntimeCatalog) => void }) {
  const [error, setError] = useState(""); const providers = asArray(resourceData(catalog, "providers", [])); const mcp = asArray(resourceData(catalog, "mcp", []));
  const toggle = async (name: string, connect: boolean) => { try { await invoke("runtime_operation", { kind: connect ? "mcp_connect" : "mcp_disconnect", identifier: name, sessionId: null, directory: config.runtime.working_directory, payload: {}, confirmed: false }); onCatalog(await invoke("runtime_catalog", { directory: config.runtime.working_directory })); } catch (caught) { setError(String(caught)); } };
  return <section className="catalog-page"><SectionHeader eyebrow="EXTENSIONS" title="Providers, MCP servers and integrations" />{error && <p className="error-banner">{error}</p>}<div className="catalog-columns"><div><h3>Providers</h3>{providers.length ? providers.map((item) => <article key={labelOf(item)}><strong>{labelOf(item)}</strong><small>{String(item.source ?? item.status ?? "Available through OpenCode")}</small></article>) : <Empty title="No provider metadata" detail={catalog.providers?.reason ?? "Connect a provider in Settings."} />}</div><div><h3>MCP servers</h3>{mcp.length ? mcp.map((item) => { const name = String(item.name ?? item.id); const connected = ["connected", "ready"].includes(String(item.status).toLowerCase()); return <article key={name}><strong>{name}</strong><small>{String(item.status ?? "disconnected")}</small><button onClick={() => void toggle(name, !connected)}>{connected ? "Disconnect" : "Connect"}</button></article>; }) : <Empty title="No MCP servers configured" detail="Add MCP configuration under Settings → OpenCode." />}</div></div></section>;
}

function CatalogView({ catalog }: { destination: Destination; catalog: RuntimeCatalog }) { return <section className="catalog-page"><SectionHeader eyebrow="OPENCODE CATALOG" title="Agents, commands and skills" /><div className="catalog-columns three">{["agents", "commands", "skills"].map((name) => <div key={name}><h3>{name}</h3>{asArray(resourceData(catalog, name, [])).map((item) => <article key={labelOf(item)}><strong>{labelOf(item)}</strong><small>{String(item.description ?? item.mode ?? item.source ?? "Available")}</small></article>)}</div>)}</div></section>; }

function HistoryView({ history }: { history: EventEnvelope[] }) { const exportHistory = () => { const url = URL.createObjectURL(new Blob([JSON.stringify(history, null, 2)], { type: "application/json" })); const link = document.createElement("a"); link.href = url; link.download = "personal-agent-history.json"; link.click(); URL.revokeObjectURL(url); }; return <section className="history-page"><SectionHeader eyebrow="ENCRYPTED EVENT STREAM" title="History and audit trail" actions={<button onClick={exportHistory}>Export JSON</button>} /><ol>{[...history].reverse().map((event) => <li key={event.event_id}><b>{event.monotonic_sequence}</b><div><strong>{event.type}</strong><small>{event.origin} · {event.wall_clock_timestamp}</small><pre>{JSON.stringify(eventPayload(event), null, 2)}</pre></div></li>)}</ol></section>; }
function BrowserView({ config }: { config: AppConfig }) { const browser = config.browser as Json; return <section className="browser-page"><SectionHeader eyebrow="ISOLATED BROWSER" title="Browser automation boundaries" /><div className="browser-frame"><div className="browser-address"><span>◎</span><input readOnly value={browser.enabled ? "Isolated browser enabled" : "Browser disabled in configuration"} /><button disabled={!browser.enabled}>Open profile</button></div><Empty title={browser.enabled ? "Browser profile is ready for an agent session" : "Browser automation is off"} detail="Enable it in Settings. Isolated profiles, quarantined downloads, domain controls, and personal-profile opt-in are enforced by configuration." /></div><div className="policy-cards">{Object.entries(browser).map(([name, value]) => <article key={name}><strong>{name.replaceAll("_", " ")}</strong><span>{Array.isArray(value) ? value.join(", ") || "none" : String(value)}</span></article>)}</div></section>; }
function DiagnosticsView({ diagnostic, catalog, projection, voice }: { diagnostic: Diagnostic; catalog: RuntimeCatalog; projection: Projection; voice: VoiceStatus }) { return <section className="diagnostics-page"><SectionHeader eyebrow="SYSTEM HEALTH" title="Diagnostics and capability truth" /><div className="health-grid"><article><strong>OpenCode runtime</strong><b className={projection.runtime_healthy ? "good" : "warn"}>{projection.runtime_healthy ? "READY" : "DEGRADED"}</b><small>{diagnostic.opencode.pinned} · {diagnostic.opencode.topology}</small></article><article><strong>Speech to text</strong><b className={voice.stt_ready ? "good" : "warn"}>{voice.stt_ready ? "READY" : "UNAVAILABLE"}</b><small>{voice.whisper_executable ?? "Install Whisper in Settings"}</small></article><article><strong>Text to speech</strong><b className={voice.tts_ready ? "good" : "warn"}>{voice.tts_ready ? "READY" : "UNAVAILABLE"}</b><small>{voice.piper_executable ?? "Install Piper in Settings"}</small></article></div><div className="capability-table">{diagnostic.capabilities.map((item) => <div key={item.id}><strong>{item.id}</strong><span>{item.backend}</span><b>{typeof item.status === "string" ? item.status : item.status.state}</b></div>)}{Object.entries(catalog).map(([name, resource]) => <div key={name}><strong>{name}</strong><span>{resource.reason ?? "Authenticated sidecar API"}</span><b className={resource.available ? "good" : "warn"}>{resource.available ? "AVAILABLE" : "UNAVAILABLE"}</b></div>)}</div></section>; }
function UsageView({ history }: { history: EventEnvelope[] }) { const runtime = history.filter((event) => event.origin.includes("runtime") || event.type.startsWith("response.")); const tools = history.filter((event) => event.type.includes("tool")); return <section className="usage-page"><SectionHeader eyebrow="LOCAL ACCOUNTING" title="Usage, cost and outbound data" /><div className="metric-cards"><article><b>{runtime.length}</b><span>runtime events</span></article><article><b>{tools.length}</b><span>tool events</span></article><article><b>0</b><span>secret values recorded</span></article><article><b>{new Set(history.map((event) => event.origin)).size}</b><span>event origins</span></article></div><p>Provider token and cost totals appear when emitted by the selected model. Egress records preserve destination, data kind, purpose, and size without storing credentials.</p></section>; }

export function App() {
  const [active, setActive] = useState<Destination>("Chat"); const [config, setConfig] = useState<AppConfig>(fallbackConfig); const [catalog, setCatalog] = useState<RuntimeCatalog>({}); const [projection, setProjection] = useState<Projection>(emptyProjection); const [history, setHistory] = useState<EventEnvelope[]>([]); const [voice, setVoice] = useState<VoiceStatus>(fallbackVoice); const [diagnostic, setDiagnostic] = useState<Diagnostic>(fallbackDiagnostic); const [messages, setMessages] = useState<ChatMessage[]>([]); const [activeSession, setActiveSession] = useState(""); const [palette, setPalette] = useState(false); const [paletteQuery, setPaletteQuery] = useState(""); const [bootError, setBootError] = useState(""); const [autostart, setAutostart] = useState<boolean | null>(null);
  useEffect(() => { void invoke<Bootstrap>("bootstrap").then((data) => { setConfig(data.config); setCatalog(data.catalog); setProjection(data.projection); setHistory(data.history); setVoice(data.voice); setActiveSession(data.projection.active_session ?? ""); document.documentElement.dataset.theme = data.config.ui.accent; }).catch((caught) => setBootError(String(caught))); void invoke<Diagnostic>("diagnostics").then(setDiagnostic).catch(() => undefined); void invoke<boolean>("autostart_status").then(setAutostart).catch(() => setAutostart(false)); }, []);
  useEffect(() => { document.documentElement.style.fontSize = `${config.ui.text_scale_percent}%`; document.documentElement.lang = config.ui.locale; document.documentElement.classList.toggle("reduce-motion", config.ui.reduced_motion); document.documentElement.dataset.theme = config.ui.accent; }, [config.ui]);
  useEffect(() => { const key = (event: KeyboardEvent) => { if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") { event.preventDefault(); setPalette((value) => !value); } if (event.key === "Escape") setPalette(false); }; window.addEventListener("keydown", key); return () => window.removeEventListener("keydown", key); }, []);
  const filteredNavigation = useMemo(() => navigation.filter((item) => item.toLowerCase().includes(paletteQuery.toLowerCase())), [paletteQuery]); const refreshCatalog = useCallback((next: RuntimeCatalog) => setCatalog(next), []); const addHistory = useCallback((event: EventEnvelope) => setHistory((current) => current.some((item) => item.event_id === event.event_id) ? current : [...current, event]), []); const toggleAutostart = () => { if (autostart === null) return; void invoke<boolean>("set_autostart", { enabled: !autostart }).then(setAutostart).catch((caught) => setBootError(String(caught))); };
  let content: React.ReactNode;
  if (active === "Chat") content = <ChatView config={config} catalog={catalog} projection={projection} voiceStatus={voice} messages={messages} setMessages={setMessages} activeSession={activeSession} setActiveSession={setActiveSession} onProjection={setProjection} onHistory={addHistory} onCatalog={refreshCatalog} onVoice={setVoice} />; else if (["Goals & tasks", "Memory", "Automations", "Artifacts"].includes(active)) content = <DomainView destination={active} history={history} projection={projection} setHistory={setHistory} setProjection={setProjection} />; else if (active === "Projects & terminal") content = <ProjectView config={config} catalog={catalog} onCatalog={setCatalog} />; else if (active === "Integrations") content = <IntegrationsView catalog={catalog} config={config} onCatalog={setCatalog} />; else if (active === "Skills & agents") content = <CatalogView destination={active} catalog={catalog} />; else if (active === "History") content = <HistoryView history={history} />; else if (active === "Browser") content = <BrowserView config={config} />; else if (active === "Diagnostics") content = <DiagnosticsView diagnostic={diagnostic} catalog={catalog} projection={projection} voice={voice} />; else if (active === "Usage & egress") content = <UsageView history={history} />; else content = <ConfigEditor config={config} catalog={catalog} voice={voice} autostart={autostart} onAutostart={toggleAutostart} onConfig={setConfig} onVoice={setVoice} />;
  return <div className={`app-shell ${config.ui.compact_sidebar ? "compact-sidebar" : ""}`}><aside className="sidebar"><div className="brand"><div className="brand-mark"><i /><b /></div><div><strong>PERSONAL<br />AGENT</strong><small>{config.persona.name} · BOUNDED</small></div></div><nav>{navigation.map((item) => <button key={item} className={active === item ? "active" : ""} aria-current={active === item ? "page" : undefined} aria-label={item} onClick={() => setActive(item)}><i>{icon(item)}</i><span>{item}</span>{item === "Goals & tasks" && projection.tasks_running > 0 && <b>{projection.tasks_running}</b>}</button>)}</nav><div className="profile"><span>Y</span><div><strong>Default profile</strong><small>Private · Local state</small></div></div></aside><main><header className="topbar"><div><span className="eyebrow">WORKSPACE / {active.toUpperCase()}</span><h1>{active === "Chat" ? "Good morning, Yuval." : active}</h1></div><div className="top-actions"><button><b>O</b> OpenCode <small>{diagnostic.opencode.pinned}</small></button><button>{config.persona.name} <small>{config.runtime.default_agent || "agent"}</small></button><button onClick={() => setConfig((current) => ({ ...current, ui: { ...current.ui, compact_sidebar: !current.ui.compact_sidebar } }))}>HUD</button><button onClick={() => setActive("Settings")}>{config.ui.accent.toUpperCase()}</button><button aria-label="Open command palette" onClick={() => setPalette(true)}><kbd>⌘</kbd><kbd>K</kbd></button></div></header>{bootError && <div className="boot-error">{bootError}<button onClick={() => setBootError("")}>×</button></div>}<div className="content">{content}</div><footer className="app-footer"><span className={projection.runtime_healthy ? "good" : "warn"}>● CORE {projection.runtime_healthy ? "ONLINE" : "DEGRADED"}</span><span>MICROPHONE {projection.microphone_active ? "LIVE" : "PRIVATE"}</span><span>PRIVATE MODE</span><span className="footer-right">LINUX · X86_64 <b>v{diagnostic.version}</b></span></footer></main>{palette && <div className="palette-backdrop" onMouseDown={() => setPalette(false)}><section className="command-palette" role="dialog" aria-label="COMMAND PALETTE" onMouseDown={(event) => event.stopPropagation()}><header><span>⌕</span><input autoFocus value={paletteQuery} placeholder="Go to…" onChange={(event) => setPaletteQuery(event.target.value)} /><kbd>ESC</kbd></header>{filteredNavigation.map((item) => <button key={item} onClick={() => { setActive(item); setPalette(false); setPaletteQuery(""); }}><i>{icon(item)}</i><span>{item}</span></button>)}</section></div>}</div>;
}
