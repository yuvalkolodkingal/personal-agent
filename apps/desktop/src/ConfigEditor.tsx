import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  AppConfig,
  RuntimeCapability,
  RuntimeCatalog,
  VoiceStatus,
} from "./types";

type JsonObject = Record<string, unknown>;
type AuthPrompt = {
  type: "text" | "select";
  key: string;
  message: string;
  placeholder?: string;
  options?: Array<{ label: string; value: string; hint?: string }>;
  when?: { key: string; op: "eq" | "neq"; value: string };
};
type AuthMethod = {
  type: "oauth" | "api";
  label: string;
  prompts?: AuthPrompt[];
};
type Authorization = {
  url: string;
  method: "auto" | "code";
  instructions: string;
};

const sectionGroups = [
  { label: "Agent", sections: ["persona", "runtime", "agent"] },
  { label: "Experience", sections: ["voice", "ui", "workspace"] },
  { label: "Intelligence", sections: ["memory", "automation", "browser"] },
  { label: "Trust", sections: ["privacy", "notifications", "updates"] },
  { label: "Developer", sections: ["opencode"] },
] as const;

const allConfigSections = sectionGroups.flatMap((group) => [...group.sections]);

function title(value: string) {
  return value
    .replaceAll("_", " ")
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function cloneWithPath(
  source: JsonObject,
  path: string[],
  value: unknown,
): JsonObject {
  const next = structuredClone(source);
  let cursor: JsonObject = next;
  path.slice(0, -1).forEach((part) => {
    cursor = cursor[part] as JsonObject;
  });
  cursor[path.at(-1)!] = value;
  return next;
}

function Field({
  name,
  value,
  onChange,
}: {
  name: string;
  value: unknown;
  onChange: (value: unknown) => void;
}) {
  if (typeof value === "boolean") {
    return (
      <label className="setting-row setting-toggle">
        <span>
          <strong>{title(name)}</strong>
          <small>{value ? "Enabled" : "Disabled"}</small>
        </span>
        <input
          type="checkbox"
          checked={value}
          onChange={(event) => onChange(event.target.checked)}
        />
      </label>
    );
  }
  if (typeof value === "number") {
    return (
      <label className="setting-row">
        <span>
          <strong>{title(name)}</strong>
        </span>
        <input
          type="number"
          value={value}
          onChange={(event) => onChange(Number(event.target.value))}
        />
      </label>
    );
  }
  if (Array.isArray(value)) {
    return (
      <label className="setting-row setting-wide">
        <span>
          <strong>{title(name)}</strong>
          <small>One item per line</small>
        </span>
        <textarea
          rows={Math.min(5, Math.max(2, value.length + 1))}
          value={value.join("\n")}
          onChange={(event) =>
            onChange(
              event.target.value
                .split("\n")
                .map((item) => item.trim())
                .filter(Boolean),
            )
          }
        />
      </label>
    );
  }
  if (value && typeof value === "object")
    return <JsonField name={name} value={value} onChange={onChange} />;
  const secretLike = /(token|password|secret|credential|api.?key)/i.test(name);
  return (
    <label className="setting-row">
      <span>
        <strong>{title(name)}</strong>
        {secretLike && (
          <small>Reference only—secret values stay out of config</small>
        )}
      </span>
      <input
        type="text"
        value={String(value ?? "")}
        onChange={(event) => onChange(event.target.value)}
      />
    </label>
  );
}

function JsonField({
  name,
  value,
  onChange,
}: {
  name: string;
  value: unknown;
  onChange: (value: unknown) => void;
}) {
  const [text, setText] = useState(() => JSON.stringify(value, null, 2));
  const [error, setError] = useState("");
  useEffect(() => setText(JSON.stringify(value, null, 2)), [value]);
  return (
    <label className="setting-row setting-wide">
      <span>
        <strong>{title(name)}</strong>
        <small>
          Full OpenCode-compatible JSON. Security-owned keys are enforced
          natively.
        </small>
      </span>
      <textarea
        className="json-editor"
        rows={14}
        value={text}
        onChange={(event) => {
          const next = event.target.value;
          setText(next);
          try {
            onChange(JSON.parse(next));
            setError("");
          } catch (caught) {
            setError(String(caught));
          }
        }}
      />
      {error && <em className="field-error">{error}</em>}
    </label>
  );
}

function SectionFields({
  section,
  value,
  update,
}: {
  section: string;
  value: JsonObject;
  update: (path: string[], value: unknown) => void;
}) {
  if (section === "opencode")
    return (
      <JsonField
        name="managed OpenCode configuration"
        value={value}
        onChange={(next) => update([], next)}
      />
    );
  return (
    <div className="settings-grid">
      {Object.entries(value).map(([name, field]) => (
        <Field
          key={name}
          name={name}
          value={field}
          onChange={(next) => update([name], next)}
        />
      ))}
    </div>
  );
}

function providersFrom(catalog: RuntimeCatalog) {
  const auth = (catalog.provider_auth?.data ?? {}) as Record<
    string,
    AuthMethod[]
  >;
  const providerData = catalog.providers?.data as
    JsonObject | JsonObject[] | undefined;
  const providerObject =
    providerData && !Array.isArray(providerData) ? providerData : undefined;
  const all = Array.isArray(providerData)
    ? providerData
    : Array.isArray(providerObject?.all)
      ? (providerObject.all as JsonObject[])
      : [];
  const connected = new Set(
    Array.isArray(providerObject?.connected)
      ? (providerObject.connected as unknown[]).map(String)
      : [],
  );
  const models = Array.isArray(catalog.models?.data) ? catalog.models.data : [];
  const names = new Set([
    ...Object.keys(auth),
    ...all.map((item) => String(item.id ?? item.name ?? "")),
    ...models.map((item) => item.provider_id),
  ]);
  return [...names]
    .filter(Boolean)
    .map((id) => {
      const metadata = all.find((item) => item.id === id || item.name === id);
      return {
        id,
        name: String(metadata?.name ?? id),
        methods: auth[id] ?? [],
        connected: connected.has(id),
        modelCount: models.filter((model) => model.provider_id === id).length,
      };
    })
    .sort(
      (left, right) =>
        Number(right.connected) - Number(left.connected) ||
        left.name.localeCompare(right.name),
    );
}

function ProviderConnections({
  catalog,
  directory,
  onCatalog,
  onMessage,
  onDefaultModel,
}: {
  catalog: RuntimeCatalog;
  directory: string;
  onCatalog: (catalog: RuntimeCatalog) => void;
  onMessage: (message: string) => void;
  onDefaultModel: (model: RuntimeCapability) => void;
}) {
  const providers = useMemo(() => providersFrom(catalog), [catalog]);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState("");
  const [inputs, setInputs] = useState<Record<string, string>>({});
  const [apiKey, setApiKey] = useState("");
  const [working, setWorking] = useState(false);
  const [authorization, setAuthorization] = useState<{
    provider: string;
    index: number;
    value: Authorization;
  } | null>(null);
  const [code, setCode] = useState("");
  const selectedProvider =
    providers.find((provider) => provider.id === selected) ?? providers[0];
  const visible = providers.filter((provider) =>
    `${provider.name} ${provider.id}`
      .toLowerCase()
      .includes(query.toLowerCase()),
  );
  const models = (catalog.models?.data ?? []).filter(
    (model) => !selectedProvider || model.provider_id === selectedProvider.id,
  );

  useEffect(() => {
    if (!selected && providers[0]) setSelected(providers[0].id);
  }, [providers, selected]);
  const refresh = async () =>
    onCatalog(await invoke<RuntimeCatalog>("runtime_catalog", { directory }));
  const beginOauth = async (method: AuthMethod, index: number) => {
    if (!selectedProvider) return;
    setWorking(true);
    onMessage("Opening secure provider sign-in…");
    try {
      const value = await invoke<Authorization>("provider_oauth_authorize", {
        providerId: selectedProvider.id,
        method: index,
        inputs,
        openBrowser: true,
      });
      setAuthorization({ provider: selectedProvider.id, index, value });
      onMessage(`${method.label} sign-in opened in your browser.`);
    } catch (caught) {
      onMessage(String(caught));
    } finally {
      setWorking(false);
    }
  };
  const finishOauth = async () => {
    if (!authorization) return;
    setWorking(true);
    onMessage("Completing provider authorization…");
    try {
      await invoke("provider_oauth_callback", {
        providerId: authorization.provider,
        method: authorization.index,
        code: authorization.value.method === "code" ? code : null,
      });
      await refresh();
      setAuthorization(null);
      setCode("");
      onMessage(
        `${authorization.provider} is connected through OpenCode OAuth.`,
      );
    } catch (caught) {
      onMessage(String(caught));
    } finally {
      setWorking(false);
    }
  };
  const connectKey = async () => {
    if (!selectedProvider) return;
    setWorking(true);
    try {
      await invoke("provider_set_key", {
        providerId: selectedProvider.id,
        key: apiKey,
      });
      await refresh();
      setApiKey("");
      onMessage(
        `${selectedProvider.name} is connected. Its key is stored in the OS keychain.`,
      );
    } catch (caught) {
      onMessage(String(caught));
    } finally {
      setWorking(false);
    }
  };
  const revoke = async () => {
    if (!selectedProvider) return;
    setWorking(true);
    try {
      await invoke("provider_revoke", { providerId: selectedProvider.id });
      await refresh();
      onMessage(`${selectedProvider.name} credentials were removed.`);
    } catch (caught) {
      onMessage(String(caught));
    } finally {
      setWorking(false);
    }
  };

  return (
    <div className="provider-workspace">
      <section className="provider-browser">
        <div className="provider-search">
          <span>⌕</span>
          <input
            aria-label="Search providers"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Search providers…"
          />
          <b>{providers.length}</b>
        </div>
        <div className="provider-grid">
          {visible.map((provider) => (
            <button
              key={provider.id}
              className={selectedProvider?.id === provider.id ? "selected" : ""}
              onClick={() => {
                setSelected(provider.id);
                setInputs({});
              }}
            >
              <span className="provider-logo">
                {provider.name.slice(0, 1).toUpperCase()}
              </span>
              <span>
                <strong>{provider.name}</strong>
                <small>
                  {provider.modelCount} models ·{" "}
                  {provider.methods.some((method) => method.type === "oauth")
                    ? "OAuth"
                    : "API key"}
                </small>
              </span>
              <i className={provider.connected ? "connected" : ""}>
                {provider.connected ? "Connected" : "Set up"}
              </i>
            </button>
          ))}
        </div>
      </section>
      <section className="provider-detail">
        {selectedProvider ? (
          <>
            <header>
              <span className="provider-logo large">
                {selectedProvider.name.slice(0, 1).toUpperCase()}
              </span>
              <div>
                <span className="eyebrow">MODEL PROVIDER</span>
                <h3>{selectedProvider.name}</h3>
                <p>
                  {selectedProvider.connected
                    ? "Connected to your private OpenCode runtime"
                    : "Choose a supported sign-in method"}
                </p>
              </div>
              <button
                className="danger-quiet"
                disabled={working || !selectedProvider.connected}
                onClick={revoke}
              >
                Disconnect
              </button>
            </header>
            <div className="auth-methods">
              {selectedProvider.methods.map((method, index) => (
                <article key={`${method.label}-${index}`}>
                  <div>
                    <span className="auth-icon">
                      {method.type === "oauth" ? "↗" : "#"}
                    </span>
                    <span>
                      <strong>{method.label}</strong>
                      <small>
                        {method.type === "oauth"
                          ? "Sign in in your browser; no API key required"
                          : "Use a provider-issued API key"}
                      </small>
                    </span>
                  </div>
                  {method.prompts
                    ?.filter(
                      (prompt) =>
                        !prompt.when ||
                        (prompt.when.op === "eq"
                          ? inputs[prompt.when.key] === prompt.when.value
                          : inputs[prompt.when.key] !== prompt.when.value),
                    )
                    .map((prompt) => (
                      <label key={prompt.key}>
                        {prompt.message}
                        {prompt.type === "select" ? (
                          <select
                            value={inputs[prompt.key] ?? ""}
                            onChange={(event) =>
                              setInputs((current) => ({
                                ...current,
                                [prompt.key]: event.target.value,
                              }))
                            }
                          >
                            <option value="">Select…</option>
                            {prompt.options?.map((option) => (
                              <option key={option.value} value={option.value}>
                                {option.label}
                              </option>
                            ))}
                          </select>
                        ) : (
                          <input
                            placeholder={prompt.placeholder}
                            value={inputs[prompt.key] ?? ""}
                            onChange={(event) =>
                              setInputs((current) => ({
                                ...current,
                                [prompt.key]: event.target.value,
                              }))
                            }
                          />
                        )}
                      </label>
                    ))}
                  {method.type === "oauth" ? (
                    <button
                      className="primary"
                      disabled={working}
                      onClick={() => void beginOauth(method, index)}
                    >
                      Continue with browser
                    </button>
                  ) : (
                    <div className="key-connect">
                      <input
                        aria-label={`${selectedProvider.name} API key`}
                        type="password"
                        value={apiKey}
                        onChange={(event) => setApiKey(event.target.value)}
                        placeholder="Paste API key"
                      />
                      <button
                        className="primary"
                        disabled={working || !apiKey.trim()}
                        onClick={connectKey}
                      >
                        Connect key
                      </button>
                    </div>
                  )}
                </article>
              ))}
              {!selectedProvider.methods.length && (
                <article>
                  <div>
                    <span className="auth-icon">#</span>
                    <span>
                      <strong>API key</strong>
                      <small>
                        This provider does not advertise an interactive OAuth
                        method.
                      </small>
                    </span>
                  </div>
                  <div className="key-connect">
                    <input
                      aria-label={`${selectedProvider.name} API key`}
                      type="password"
                      value={apiKey}
                      onChange={(event) => setApiKey(event.target.value)}
                      placeholder="Paste API key"
                    />
                    <button
                      className="primary"
                      disabled={working || !apiKey.trim()}
                      onClick={connectKey}
                    >
                      Connect key
                    </button>
                  </div>
                </article>
              )}
            </div>
            <div className="model-picker">
              <div>
                <strong>Available models</strong>
                <small>
                  Make one the default for new voice and chat sessions.
                </small>
              </div>
              {models.length ? (
                <div>
                  {models.slice(0, 20).map((model) => (
                    <button
                      key={`${model.provider_id}/${model.model_id}`}
                      onClick={() => onDefaultModel(model)}
                    >
                      <span>{model.model_id}</span>
                      <small>
                        {model.reasoning ? "Reasoning" : "Standard"}
                        {model.local ? " · Local" : ""}
                      </small>
                      <b>Use default</b>
                    </button>
                  ))}
                </div>
              ) : (
                <p>Connect this provider to discover its models.</p>
              )}
            </div>
          </>
        ) : (
          <div className="provider-empty">
            No providers were discovered. Check Diagnostics to confirm the
            OpenCode runtime is online.
          </div>
        )}
      </section>
      {authorization && (
        <div
          className="oauth-backdrop"
          role="dialog"
          aria-label="Complete provider sign in"
        >
          <section className="oauth-dialog">
            <button
              className="oauth-close"
              onClick={() => setAuthorization(null)}
            >
              ×
            </button>
            <span className="provider-logo large">↗</span>
            <span className="eyebrow">SECURE PROVIDER SIGN-IN</span>
            <h3>Finish in your browser</h3>
            <p>
              {authorization.value.instructions ||
                `Authorize ${authorization.provider}, then return here.`}
            </p>
            <code>{authorization.value.url}</code>
            {authorization.value.method === "code" && (
              <label>
                Authorization code
                <input
                  autoFocus
                  value={code}
                  onChange={(event) => setCode(event.target.value)}
                  placeholder="Paste the code from the provider"
                />
              </label>
            )}
            <div>
              <button
                onClick={() =>
                  void navigator.clipboard.writeText(authorization.value.url)
                }
              >
                Copy link
              </button>
              <button
                className="primary"
                disabled={
                  working ||
                  (authorization.value.method === "code" && !code.trim())
                }
                onClick={finishOauth}
              >
                {working ? "Connecting…" : "I completed sign-in"}
              </button>
            </div>
          </section>
        </div>
      )}
    </div>
  );
}

type Props = {
  config: AppConfig;
  catalog: RuntimeCatalog;
  voice: VoiceStatus;
  autostart: boolean | null;
  onAutostart: () => void;
  onConfig: (config: AppConfig) => void;
  onVoice: (voice: VoiceStatus) => void;
  onCatalog: (catalog: RuntimeCatalog) => void;
  initialSection?: string;
};

export function ConfigEditor({
  config,
  catalog,
  voice,
  autostart,
  onAutostart,
  onConfig,
  onVoice,
  onCatalog,
  initialSection = "voice",
}: Props) {
  const [draft, setDraft] = useState<AppConfig>(() => structuredClone(config));
  const [section, setSection] = useState("voice");
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState("");
  const [installing, setInstalling] = useState(false);
  const [testingVoice, setTestingVoice] = useState(false);
  const [preview, setPreview] = useState("Systems ready. How can I help?");
  const activeStt =
    voice.active_stt_backend || config.voice.stt_backend || "missing";
  const activeTts =
    voice.active_tts_backend || config.voice.tts_backend || "missing";
  useEffect(() => setDraft(structuredClone(config)), [config]);
  useEffect(() => setSection(initialSection), [initialSection]);
  useEffect(() => {
    let dispose: (() => void) | undefined;
    void listen<{
      phase?: string;
      asset?: string;
      downloaded?: number;
      total?: number;
    }>("voice-install-progress", ({ payload }) => {
      if (payload.phase) setMessage(payload.phase);
      else if (payload.asset)
        setMessage(
          `Downloading ${payload.asset}${payload.total ? ` · ${Math.round(((payload.downloaded ?? 0) / payload.total) * 100)}%` : ""}`,
        );
    }).then((fn) => {
      dispose = fn;
    });
    return () => dispose?.();
  }, []);

  const update = (path: string[], value: unknown) => {
    if (section === "opencode" && path.length === 0) {
      setDraft((current) => ({
        ...current,
        opencode: value as Record<string, unknown>,
      }));
      return;
    }
    setDraft(
      (current) =>
        cloneWithPath(
          current as unknown as JsonObject,
          [section, ...path],
          value,
        ) as unknown as AppConfig,
    );
  };
  const save = async () => {
    setSaving(true);
    setMessage("");
    try {
      const result = await invoke<{ config: AppConfig }>("save_config", {
        config: draft,
      });
      setDraft(result.config);
      onConfig(result.config);
      setMessage(
        "Saved. The authenticated runtime restarted with your new configuration.",
      );
    } catch (caught) {
      setMessage(String(caught));
    } finally {
      setSaving(false);
    }
  };
  const installVoice = async () => {
    setInstalling(true);
    setMessage("Preparing Moonshine Medium Streaming and Qwen3-TTS 0.6B…");
    try {
      const status = await invoke<VoiceStatus>("voice_install", {
        component: "balanced",
      });
      onVoice(status);
      setDraft((current) => ({
        ...current,
        voice: {
          ...current.voice,
          language: "en",
          response_language: "en",
          stt_backend: "moonshine",
          stt_model: "medium-streaming",
          tts_backend: "qwen3-tts",
          tts_model: "Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice",
          tts_voice: "Ryan",
        },
      }));
      setMessage(
        "Balanced neural voice is installed. Save changes to make it the active English voice profile.",
      );
    } catch (caught) {
      setMessage(String(caught));
    } finally {
      setInstalling(false);
    }
  };
  const testVoice = async () => {
    setTestingVoice(true);
    setMessage(`Running a private ${activeTts} → ${activeStt} round-trip…`);
    try {
      const result = await invoke<{
        transcript: string;
        synthesis_ms: number;
        recognition_ms: number;
      }>("voice_self_test");
      setMessage(
        `Voice pipeline passed in ${result.synthesis_ms + result.recognition_ms} ms. Heard: “${result.transcript}”`,
      );
    } catch (caught) {
      setMessage(String(caught));
    } finally {
      setTestingVoice(false);
    }
  };
  const setDefaultModel = (model: RuntimeCapability) => {
    setDraft((current) => ({
      ...current,
      runtime: {
        ...current.runtime,
        default_provider: model.provider_id,
        default_model: model.model_id,
      },
    }));
    setMessage(
      `${model.provider_id}/${model.model_id} selected. Save changes to make it the default.`,
    );
  };
  const selectVoicePreset = (
    preset: "Balanced" | "Low latency" | "Compatibility",
  ) => {
    setDraft((current) => ({
      ...current,
      voice: {
        ...current.voice,
        language: "en",
        response_language: "en",
        stt_backend: preset === "Compatibility" ? "whisper.cpp" : "moonshine",
        stt_model: preset === "Compatibility" ? "base" : "medium-streaming",
        tts_backend: preset === "Balanced" ? "qwen3-tts" : "piper",
        tts_model:
          preset === "Balanced"
            ? "Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice"
            : current.voice.tts_model,
        tts_voice: preset === "Balanced" ? "Ryan" : "en_US-lessac-medium",
      },
    }));
    setMessage(
      `${preset} English voice preset selected. Save changes to activate it.`,
    );
  };
  const activePreset =
    draft.voice.stt_backend === "whisper.cpp"
      ? "Compatibility"
      : draft.voice.tts_backend === "piper"
        ? "Low latency"
        : "Balanced";

  return (
    <div className="settings-layout">
      <aside className="settings-nav" aria-label="Settings sections">
        <div className="settings-nav-title">
          <span>⚙</span>
          <div>
            <strong>Settings</strong>
            <small>Everything, in one place</small>
          </div>
        </div>
        <button
          aria-label="Providers & models"
          className={section === "providers" ? "active featured" : "featured"}
          onClick={() => setSection("providers")}
        >
          <span aria-hidden="true">◈</span>Providers & models
        </button>
        {sectionGroups.map((group) => (
          <div className="settings-group" key={group.label}>
            <small>{group.label}</small>
            {group.sections.map((item) => (
              <button
                key={item}
                className={section === item ? "active" : ""}
                onClick={() => setSection(item)}
              >
                {title(item)}
              </button>
            ))}
          </div>
        ))}
        <div className="settings-group">
          <small>System</small>
          <button
            className={section === "system" ? "active" : ""}
            onClick={() => setSection("system")}
          >
            System
          </button>
          <button
            className={section === "advanced" ? "active" : ""}
            onClick={() => setSection("advanced")}
          >
            Advanced full config
          </button>
        </div>
      </aside>
      <section className="settings-content">
        <header>
          <div>
            <span className="eyebrow">PERSONALIZE YOUR AGENT</span>
            <h2>{section === "voice" ? "Voice Lab" : title(section)}</h2>
            <p>
              {section === "providers"
                ? "Connect accounts and choose which intelligence powers your agent."
                : section === "voice"
                  ? "Shape how your agent listens, speaks, and responds."
                  : "Changes remain local until you save them."}
            </p>
          </div>
          <button
            aria-label="Save all changes"
            className="primary save-settings"
            onClick={save}
            disabled={saving}
          >
            {saving ? "Saving…" : "Save changes"}
          </button>
        </header>
        {section === "providers" && (
          <ProviderConnections
            catalog={catalog}
            directory={draft.runtime.working_directory}
            onCatalog={onCatalog}
            onMessage={setMessage}
            onDefaultModel={setDefaultModel}
          />
        )}
        {allConfigSections.includes(
          section as (typeof allConfigSections)[number],
        ) && (
          <>
            {section === "voice" && (
              <div className="voice-lab">
                <section className="voice-presets">
                  <header>
                    <div>
                      <span className="eyebrow">ENGLISH VOICE PIPELINE</span>
                      <h3>Choose a behavior preset</h3>
                    </div>
                    <small>All audio stays local</small>
                  </header>
                  <div>
                    {(
                      [
                        [
                          "Balanced",
                          "Best local quality",
                          "Moonshine · Smart Turn · Qwen3-TTS",
                        ],
                        [
                          "Low latency",
                          "Fast spoken responses",
                          "Moonshine · Smart Turn · Piper",
                        ],
                        [
                          "Compatibility",
                          "Lowest resource use",
                          "Whisper.cpp · silence · Piper",
                        ],
                      ] as const
                    ).map(([name, detail, engines]) => (
                      <button
                        key={name}
                        className={activePreset === name ? "active" : ""}
                        onClick={() => selectVoicePreset(name)}
                      >
                        <span>{activePreset === name ? "✓" : ""}</span>
                        <strong>{name}</strong>
                        <small>{detail}</small>
                        <i>{engines}</i>
                      </button>
                    ))}
                  </div>
                </section>
                <div className="voice-studio">
                  <div className="voice-preview-orb">
                    <i />
                    <b>J</b>
                  </div>
                  <div>
                    <span className="eyebrow">
                      {activePreset.toUpperCase()} PRESET
                    </span>
                    <h3>{draft.voice.tts_voice}</h3>
                    <p>
                      {voice.moonshine_ready &&
                      voice.smart_turn_ready &&
                      voice.qwen_ready
                        ? "The full neural speech loop is installed and ready."
                        : voice.stt_ready && voice.tts_ready
                          ? "Compatibility speech is ready; install Balanced for the full neural loop."
                          : "Install the local voice bundle to begin."}
                    </p>
                    <div>
                      <input
                        aria-label="Voice preview text"
                        value={preview}
                        onChange={(event) => setPreview(event.target.value)}
                      />
                      <button
                        className="primary"
                        disabled={!voice.tts_ready || !preview.trim()}
                        onClick={() =>
                          void invoke("voice_speak", { text: preview }).catch(
                            (caught) => setMessage(String(caught)),
                          )
                        }
                      >
                        ▶ Preview
                      </button>
                      <button onClick={() => void invoke("voice_stop")}>
                        Stop
                      </button>
                      <button
                        aria-label="Test STT + TTS"
                        disabled={
                          testingVoice || !voice.stt_ready || !voice.tts_ready
                        }
                        onClick={() => void testVoice()}
                      >
                        {testingVoice ? "Testing…" : "Run loop test"}
                      </button>
                      <button
                        disabled={
                          installing ||
                          (voice.moonshine_ready &&
                            voice.smart_turn_ready &&
                            voice.qwen_ready)
                        }
                        onClick={installVoice}
                      >
                        {installing
                          ? "Installing…"
                          : voice.moonshine_ready &&
                              voice.smart_turn_ready &&
                              voice.qwen_ready
                            ? "Installed"
                            : "Install neural stack"}
                      </button>
                    </div>
                  </div>
                </div>
                <section className="voice-engine-table">
                  <header>
                    <strong>ACTIVE PIPELINE</strong>
                    <span>Engine</span>
                    <span>Execution</span>
                    <span>Status</span>
                  </header>
                  <div>
                    <b>Speech recognition</b>
                    <span>{activeStt}</span>
                    <span>CPU · 16 kHz</span>
                    <i className={voice.stt_ready ? "ready" : ""}>
                      {voice.stt_ready ? "READY" : "MISSING"}
                    </i>
                  </div>
                  <div>
                    <b>Turn detection</b>
                    <span>
                      {voice.smart_turn_ready
                        ? "Smart Turn v3.2"
                        : "Adaptive silence"}
                    </span>
                    <span>CPU · local</span>
                    <i className={voice.smart_turn_ready ? "ready" : ""}>
                      {voice.smart_turn_ready ? "READY" : "FALLBACK"}
                    </i>
                  </div>
                  <div>
                    <b>Speech synthesis</b>
                    <span>{activeTts}</span>
                    <span>
                      {activeTts === "qwen3-tts" ? "GPU · BF16" : "CPU · local"}
                    </span>
                    <i className={voice.tts_ready ? "ready" : ""}>
                      {voice.tts_ready ? "READY" : "MISSING"}
                    </i>
                  </div>
                </section>
                <section className="voice-wake-panel">
                  <div>
                    <span className="eyebrow">HANDS-FREE WAKE</span>
                    <strong>Open wake recognition</strong>
                    <small>
                      Continuously listens for configured phrases with the
                      local STT engine. Speech after the wake phrase becomes
                      the command automatically.
                    </small>
                  </div>
                  <label>
                    <span>Wake phrases</span>
                    <input
                      aria-label="Wake phrases"
                      value={draft.voice.wake_phrases.join(", ")}
                      onChange={(event) =>
                        update(
                          ["wake_phrases"],
                          event.target.value
                            .split(",")
                            .map((phrase) => phrase.trim())
                            .filter(Boolean),
                        )
                      }
                    />
                  </label>
                  <label className="wake-toggle">
                    <span>
                      {draft.voice.wake_enabled ? "ARMED ON SAVE" : "OFF"}
                    </span>
                    <input
                      aria-label="Enable open wake recognition"
                      type="checkbox"
                      checked={draft.voice.wake_enabled}
                      disabled={!voice.stt_ready}
                      onChange={(event) =>
                        update(["wake_enabled"], event.target.checked)
                      }
                    />
                  </label>
                </section>
                <section className="voice-latency">
                  <header>
                    <strong>REALTIME TURN TIMELINE</strong>
                    <span>capture → transcript → model → speech</span>
                  </header>
                  <div>
                    <i />
                    <i />
                    <i />
                    <i />
                  </div>
                  <footer>
                    <span>
                      Capture<small>streaming</small>
                    </span>
                    <span>
                      Endpoint<small>{draft.voice.endpoint_short_ms} ms</small>
                    </span>
                    <span>
                      Model<small>provider-bound</small>
                    </span>
                    <span>
                      Speech<small>interruptible</small>
                    </span>
                  </footer>
                </section>
              </div>
            )}
            <SectionFields
              section={section}
              value={(draft as unknown as JsonObject)[section] as JsonObject}
              update={update}
            />
          </>
        )}
        {section === "advanced" && (
          <div className="settings-grid">
            <JsonField
              name="complete application configuration"
              value={draft}
              onChange={(next) => setDraft(next as AppConfig)}
            />
          </div>
        )}
        {section === "system" && (
          <div className="settings-grid">
            <label className="setting-row setting-toggle">
              <span>
                <strong>Start at login</strong>
                <small>Launch quietly for instant voice access</small>
              </span>
              <input
                aria-label="Toggle start at login"
                type="checkbox"
                disabled={autostart === null}
                checked={autostart ?? false}
                onChange={onAutostart}
              />
            </label>
            <div className="setting-row">
              <span>
                <strong>Global summon shortcut</strong>
                <small>Show and focus JARVIS from anywhere</small>
              </span>
              <kbd>Super + J</kbd>
            </div>
            <div className="setting-row">
              <span>
                <strong>Balanced neural voice</strong>
                <small>{voice.details.join(" ")}</small>
              </span>
              <button
                className="primary"
                disabled={
                  installing ||
                  (voice.moonshine_ready &&
                    voice.smart_turn_ready &&
                    voice.qwen_ready)
                }
                onClick={installVoice}
              >
                {installing
                  ? "Installing…"
                  : voice.moonshine_ready &&
                      voice.smart_turn_ready &&
                      voice.qwen_ready
                    ? "Installed"
                    : "Install Balanced voice"}
              </button>
            </div>
          </div>
        )}
        {message && (
          <p className="settings-message" role="status">
            {message}
            <button aria-label="Dismiss message" onClick={() => setMessage("")}>
              ×
            </button>
          </p>
        )}
      </section>
    </div>
  );
}
