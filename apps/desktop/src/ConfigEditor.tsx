import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AppConfig, RuntimeCatalog, VoiceStatus } from "./types";

type JsonObject = Record<string, unknown>;

const sectionOrder = [
  "persona", "runtime", "agent", "voice", "ui", "workspace", "privacy",
  "browser", "memory", "automation", "notifications", "updates", "opencode",
] as const;

function title(value: string) {
  return value.replaceAll("_", " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function cloneWithPath(source: JsonObject, path: string[], value: unknown): JsonObject {
  const next = structuredClone(source);
  let cursor: JsonObject = next;
  path.slice(0, -1).forEach((part) => {
    cursor = cursor[part] as JsonObject;
  });
  cursor[path.at(-1)!] = value;
  return next;
}

function Field({ name, value, onChange }: { name: string; value: unknown; onChange: (value: unknown) => void }) {
  if (typeof value === "boolean") {
    return <label className="setting-row setting-toggle"><span><strong>{title(name)}</strong></span><input type="checkbox" checked={value} onChange={(event) => onChange(event.target.checked)} /></label>;
  }
  if (typeof value === "number") {
    return <label className="setting-row"><span><strong>{title(name)}</strong></span><input type="number" value={value} onChange={(event) => onChange(Number(event.target.value))} /></label>;
  }
  if (Array.isArray(value)) {
    return <label className="setting-row setting-wide"><span><strong>{title(name)}</strong><small>One item per line</small></span><textarea rows={Math.min(5, Math.max(2, value.length + 1))} value={value.join("\n")} onChange={(event) => onChange(event.target.value.split("\n").map((item) => item.trim()).filter(Boolean))} /></label>;
  }
  if (value && typeof value === "object") {
    return <JsonField name={name} value={value} onChange={onChange} />;
  }
  const secretLike = /(token|password|secret|credential|api.?key)/i.test(name);
  return <label className="setting-row"><span><strong>{title(name)}</strong>{secretLike && <small>Store aliases only; secret values belong in the keychain</small>}</span><input type="text" value={String(value ?? "")} onChange={(event) => onChange(event.target.value)} /></label>;
}

function JsonField({ name, value, onChange }: { name: string; value: unknown; onChange: (value: unknown) => void }) {
  const [text, setText] = useState(() => JSON.stringify(value, null, 2));
  const [error, setError] = useState("");
  return <label className="setting-row setting-wide"><span><strong>{title(name)}</strong><small>Full OpenCode-compatible JSON; security-owned keys are enforced natively</small></span><textarea className="json-editor" rows={14} value={text} onChange={(event) => {
    const next = event.target.value;
    setText(next);
    try { onChange(JSON.parse(next)); setError(""); } catch (caught) { setError(String(caught)); }
  }} />{error && <em className="field-error">{error}</em>}</label>;
}

function SectionFields({ section, value, update }: { section: string; value: JsonObject; update: (path: string[], value: unknown) => void }) {
  if (section === "opencode") return <JsonField name="managed OpenCode configuration" value={value} onChange={(next) => update([], next)} />;
  return <div className="settings-grid">{Object.entries(value).map(([name, field]) => <Field key={name} name={name} value={field} onChange={(next) => update([name], next)} />)}</div>;
}

type Props = {
  config: AppConfig;
  catalog: RuntimeCatalog;
  voice: VoiceStatus;
  autostart: boolean | null;
  onAutostart: () => void;
  onConfig: (config: AppConfig) => void;
  onVoice: (voice: VoiceStatus) => void;
};

export function ConfigEditor({ config, catalog, voice, autostart, onAutostart, onConfig, onVoice }: Props) {
  const [draft, setDraft] = useState<AppConfig>(() => structuredClone(config));
  const [section, setSection] = useState<string>("voice");
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState("");
  const [provider, setProvider] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [installing, setInstalling] = useState(false);
  const providerNames = useMemo(() => {
    const data = catalog.providers?.data;
    if (Array.isArray(data)) return data.map((item) => String((item as JsonObject).id ?? (item as JsonObject).name ?? "")).filter(Boolean);
    if (data && typeof data === "object") return Object.keys(data as JsonObject);
    return [];
  }, [catalog]);

  useEffect(() => setDraft(structuredClone(config)), [config]);

  const update = (path: string[], value: unknown) => {
    if (section === "opencode" && path.length === 0) {
      setDraft((current) => ({ ...current, opencode: value as Record<string, unknown> }));
      return;
    }
    setDraft((current) => cloneWithPath(current as unknown as JsonObject, [section, ...path], value) as unknown as AppConfig);
  };

  const save = async () => {
    setSaving(true); setMessage("");
    try {
      const result = await invoke<{ config: AppConfig }>("save_config", { config: draft });
      setDraft(result.config); onConfig(result.config); setMessage("Configuration saved and the authenticated runtime restarted.");
    } catch (caught) { setMessage(String(caught)); }
    finally { setSaving(false); }
  };

  const installVoice = async () => {
    setInstalling(true); setMessage("Downloading verified offline voice components…");
    try { const status = await invoke<VoiceStatus>("voice_install", { component: "all" }); onVoice(status); setMessage("Offline Whisper STT and Piper TTS are installed."); }
    catch (caught) { setMessage(String(caught)); }
    finally { setInstalling(false); }
  };

  const connectProvider = async () => {
    setMessage("");
    try { await invoke("provider_set_key", { providerId: provider, key: apiKey }); setApiKey(""); setMessage(`${provider} is connected; its credential is in the OS keychain.`); }
    catch (caught) { setMessage(String(caught)); }
  };

  return <div className="settings-layout">
    <aside className="settings-nav" aria-label="Settings sections">
      {sectionOrder.map((item) => <button key={item} className={section === item ? "active" : ""} onClick={() => setSection(item)}>{title(item)}</button>)}
      <button className={section === "providers" ? "active" : ""} onClick={() => setSection("providers")}>Providers & models</button>
      <button className={section === "system" ? "active" : ""} onClick={() => setSection("system")}>System</button>
      <button className={section === "advanced" ? "active" : ""} onClick={() => setSection("advanced")}>Advanced full config</button>
    </aside>
    <section className="settings-content">
      <header><div><span className="eyebrow">CONFIGURATION</span><h2>{title(section)}</h2></div><button className="primary" onClick={save} disabled={saving}>{saving ? "Saving…" : "Save all changes"}</button></header>
      {sectionOrder.includes(section as typeof sectionOrder[number]) && <SectionFields section={section} value={(draft as unknown as JsonObject)[section] as JsonObject} update={update} />}
      {section === "advanced" && <div className="settings-grid"><JsonField name="complete application configuration" value={draft} onChange={(next) => setDraft(next as AppConfig)} /></div>}
      {section === "providers" && <div className="settings-grid">
        <label className="setting-row"><span><strong>Provider ID</strong><small>Choose a discovered provider or type its exact ID</small></span><input list="providers" value={provider} onChange={(event) => setProvider(event.target.value)} /><datalist id="providers">{providerNames.map((name) => <option key={name} value={name} />)}</datalist></label>
        <label className="setting-row"><span><strong>API key</strong><small>Sent once to native keychain storage; never written to config</small></span><input type="password" value={apiKey} onChange={(event) => setApiKey(event.target.value)} /></label>
        <div className="setting-actions"><button className="primary" disabled={!provider.trim() || !apiKey.trim()} onClick={connectProvider}>Connect provider</button><button disabled={!provider.trim()} onClick={async () => { try { await invoke("provider_revoke", { providerId: provider }); setMessage(`${provider} credential revoked.`); } catch (caught) { setMessage(String(caught)); } }}>Revoke</button></div>
        <div className="setting-wide provider-list"><strong>Discovered models</strong><pre>{JSON.stringify(catalog.models?.data ?? [], null, 2)}</pre></div>
      </div>}
      {section === "system" && <div className="settings-grid">
        <label className="setting-row setting-toggle"><span><strong>Start at login</strong><small>Per-user desktop autostart</small></span><input aria-label="Toggle start at login" type="checkbox" disabled={autostart === null} checked={autostart ?? false} onChange={onAutostart} /></label>
        <div className="setting-row"><span><strong>Native voice bundle</strong><small>{voice.details.join(" ")}</small></span><button className="primary" disabled={installing || (voice.stt_ready && voice.tts_ready)} onClick={installVoice}>{installing ? "Installing…" : voice.stt_ready && voice.tts_ready ? "Installed" : "Install STT + TTS"}</button></div>
      </div>}
      {message && <p className="settings-message" role="status">{message}</p>}
    </section>
  </div>;
}
