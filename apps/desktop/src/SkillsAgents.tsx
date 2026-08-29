import { useEffect, useMemo, useRef, useState, type ChangeEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AppConfig } from "./types";
import "./skills-agents.css";

type Resource = { available: boolean; data?: unknown; reason?: string };
type ManagedDocument = {
  kind: "agent" | "command";
  name: string;
  content: string;
  digest: string;
  enabled: boolean;
  path_hint: string;
};
type Snapshot = {
  agents: Resource;
  commands: Resource;
  skills: Resource;
  managed_documents: ManagedDocument[];
  default_agent: string;
};
type CatalogItem = Record<string, unknown> & { name: string };
type Editor = {
  kind: "agent" | "command";
  mode: "create" | "edit" | "import";
  name: string;
  content: string;
  expectedDigest?: string;
};

const agentTemplate = `---
description: Describe when this agent should be used
mode: subagent
tools:
  read: true
  edit: true
  bash: true
permission:
  edit: allow
  bash: ask
---
You are a focused user-owned coding agent. Read and edit only inside the active workspace, ask before running shell commands, and follow the configured permissions.
`;
const commandTemplate = `---
description: Describe this command
agent: build
subtask: false
---
Perform this task carefully: $ARGUMENTS
`;

function catalogItems(resource: Resource | undefined): CatalogItem[] {
  if (!resource?.available) return [];
  if (Array.isArray(resource.data)) {
    return resource.data.filter(
      (item): item is CatalogItem =>
        typeof item === "object" && item !== null && typeof (item as CatalogItem).name === "string",
    );
  }
  if (typeof resource.data === "object" && resource.data !== null) {
    return Object.entries(resource.data).map(([name, value]) =>
      typeof value === "object" && value !== null
        ? { ...(value as Record<string, unknown>), name }
        : { name, value },
    );
  }
  return [];
}

function catalogCount(snapshot: Snapshot | null, tab: "agents" | "commands" | "skills") {
  const names = new Set(catalogItems(snapshot?.[tab]).map((item) => item.name));
  if (tab !== "skills") {
    const kind = tab === "agents" ? "agent" : "command";
    for (const document of snapshot?.managed_documents ?? []) {
      if (document.kind === kind) names.add(document.name);
    }
  }
  return names.size;
}

function stringMap(value: unknown): Array<[string, string]> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return [];
  return Object.entries(value as Record<string, unknown>).map(([key, setting]) => [
    key,
    typeof setting === "string" ? setting : JSON.stringify(setting),
  ]);
}

function managedSettings(content: string | undefined) {
  const settings: Record<string, unknown> = {};
  if (!content?.startsWith("---")) return settings;
  let parent = "";
  for (const line of content.split(/\r?\n/).slice(1)) {
    if (line === "---") break;
    const nested = /^\s+/.test(line);
    const separator = line.indexOf(":");
    if (separator < 0) continue;
    const key = line.slice(0, separator).trim();
    const raw = line.slice(separator + 1).trim();
    if (!nested) {
      parent = key;
      settings[key] = raw;
    } else if (parent === "tools" || parent === "permission") {
      const group = (settings[parent] && typeof settings[parent] === "object"
        ? settings[parent]
        : {}) as Record<string, string>;
      group[key] = raw;
      settings[parent] = group;
    }
  }
  return settings;
}

function modelLabel(item: CatalogItem) {
  const model = item.model;
  if (typeof model === "string") return model;
  if (model && typeof model === "object") {
    const value = model as Record<string, unknown>;
    return [value.providerID, value.modelID].filter(Boolean).join("/");
  }
  return "Runtime default";
}

export function SkillsAgents({
  config,
  onConfig,
}: {
  config: AppConfig;
  onConfig: (config: AppConfig) => void;
}) {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [tab, setTab] = useState<"agents" | "commands" | "skills">("agents");
  const [query, setQuery] = useState("");
  const [editor, setEditor] = useState<Editor | null>(null);
  const [confirmed, setConfirmed] = useState(false);
  const [busy, setBusy] = useState("");
  const [message, setMessage] = useState("");
  const [editorError, setEditorError] = useState("");
  const importRef = useRef<HTMLInputElement>(null);

  const refresh = async () => {
    setBusy("refresh");
    try {
      setSnapshot(await invoke<Snapshot>("skills_agents_snapshot"));
      setMessage("");
    } catch (caught) {
      setMessage(String(caught));
    } finally {
      setBusy("");
    }
  };
  useEffect(() => void refresh(), []);

  const resource = snapshot?.[tab];
  const visible = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const items = catalogItems(resource);
    if (tab !== "skills") {
      const kind = tab === "agents" ? "agent" : "command";
      for (const document of snapshot?.managed_documents ?? []) {
        if (document.kind === kind && !items.some((item) => item.name === document.name)) {
          items.push({
            name: document.name,
            description:
              managedSettings(document.content).description ??
              "User-owned document in the managed OpenCode profile.",
            disabled: !document.enabled,
          });
        }
      }
    }
    return items.filter((item) =>
      !needle || JSON.stringify(item).toLowerCase().includes(needle),
    );
  }, [query, resource, snapshot?.managed_documents, tab]);
  const managed = (kind: "agent" | "command", name: string) =>
    snapshot?.managed_documents.find((item) => item.kind === kind && item.name === name);

  const beginCreate = (kind: "agent" | "command") => {
    setConfirmed(false);
    setEditorError("");
    setEditor({
      kind,
      mode: "create",
      name: "",
      content: kind === "agent" ? agentTemplate : commandTemplate,
    });
  };
  const beginEdit = (document: ManagedDocument) => {
    setConfirmed(false);
    setEditorError("");
    setEditor({
      kind: document.kind,
      mode: "edit",
      name: document.name,
      content: document.content,
      expectedDigest: document.digest,
    });
  };
  const importMarkdown = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;
    if (!file.name.toLowerCase().endsWith(".md") || file.size > 128 * 1024) {
      setMessage("Import must be a Markdown file no larger than 128 KiB.");
      return;
    }
    setConfirmed(false);
    setEditorError("");
    setEditor({
      kind: tab === "commands" ? "command" : "agent",
      mode: "import",
      name: file.name.replace(/\.md$/i, "").toLowerCase().replace(/[^a-z0-9_-]+/g, "-"),
      content: await file.text(),
    });
  };

  const saveDocument = async () => {
    if (!editor || !confirmed) return;
    setBusy("editor");
    try {
      const result = await invoke<{ message: string }>("skills_agents_write", {
        kind: editor.kind,
        name: editor.name.trim(),
        content: editor.content,
        mode: editor.mode,
        expectedDigest: editor.expectedDigest ?? null,
        confirmed: true,
      });
      setEditor(null);
      setConfirmed(false);
      await refresh();
      setMessage(result.message);
    } catch (caught) {
      setEditorError(String(caught));
    } finally {
      setBusy("");
    }
  };

  const setEnabled = async (document: ManagedDocument) => {
    const enabled = !document.enabled;
    if (!window.confirm(`${enabled ? "Enable" : "Disable"} ${document.name}?`)) return;
    setBusy(document.name);
    try {
      const result = await invoke<{ message: string }>("skills_agents_set_enabled", {
        name: document.name,
        enabled,
        expectedDigest: document.digest,
        confirmed: true,
      });
      await refresh();
      setMessage(result.message);
    } catch (caught) {
      setMessage(String(caught));
    } finally {
      setBusy("");
    }
  };

  const remove = async (document: ManagedDocument) => {
    if (!window.confirm(`Delete user-owned ${document.kind} ${document.name}?`)) return;
    setBusy(document.name);
    try {
      const result = await invoke<{ message: string }>("skills_agents_delete", {
        kind: document.kind,
        name: document.name,
        expectedDigest: document.digest,
        confirmed: true,
      });
      await refresh();
      setMessage(result.message);
    } catch (caught) {
      setMessage(String(caught));
    } finally {
      setBusy("");
    }
  };

  const chooseAgent = async (name: string) => {
    setBusy(`choose-${name}`);
    try {
      const next = structuredClone(config);
      next.runtime.default_agent = name;
      const result = await invoke<{ config: AppConfig }>("save_config", { config: next });
      onConfig(result.config);
      await refresh();
      setMessage(`${name} is now the default agent.`);
    } catch (caught) {
      setMessage(String(caught));
    } finally {
      setBusy("");
    }
  };

  return (
    <section className="skills-workspace">
      <header className="skills-header">
        <div>
          <small>AUTHENTICATED OPENCODE RUNTIME</small>
          <h2>Skills &amp; agents</h2>
          <p>Discover runtime capabilities and manage only your own agent and command Markdown.</p>
        </div>
        <div className="skills-actions">
          <button onClick={() => void refresh()} disabled={busy === "refresh"}>Refresh</button>
          <button onClick={() => beginCreate("agent")}>+ Agent</button>
          <button onClick={() => beginCreate("command")}>+ Command</button>
          {tab !== "skills" && <button onClick={() => importRef.current?.click()}>Import {tab === "commands" ? "command" : "agent"}</button>}
          <input ref={importRef} aria-label="Import Markdown" className="sr-only" type="file" accept=".md,text/markdown" onChange={(event) => void importMarkdown(event)} />
        </div>
      </header>

      <div className="skills-toolbar">
        <div role="tablist" aria-label="Catalog type">
          {(["agents", "commands", "skills"] as const).map((name) => (
            <button key={name} role="tab" aria-selected={tab === name} onClick={() => setTab(name)}>
              {name} <span>{catalogCount(snapshot, name)}</span>
            </button>
          ))}
        </div>
        <input aria-label="Search skills and agents" value={query} onChange={(event) => setQuery(event.target.value)} placeholder={`Search ${tab}…`} />
      </div>

      {tab === "skills" && (
        <div className="skills-safety" role="note">
          <strong>Discovery only</strong>
          <span>Skills reported by agents are never installed automatically. Review and install them through a separate, explicit trust workflow.</span>
        </div>
      )}
      {message && <div className="skills-message" role="status">{message}</div>}
      {!snapshot && !message && <p className="skills-loading">Loading the authenticated runtime catalog…</p>}
      {resource && !resource.available && <div className="skills-message error">{resource.reason ?? "This runtime catalog is unavailable."}</div>}

      <div className="skills-grid">
        {visible.map((item) => {
          const kind = tab === "commands" ? "command" : "agent";
          const document = tab === "skills" ? undefined : managed(kind, item.name);
          const local = managedSettings(document?.content);
          const tools = stringMap(item.tools ?? local.tools);
          const permissions = stringMap(item.permission ?? local.permission);
          const mode = String(item.mode ?? local.mode ?? "all");
          const selectedModel = item.model ?? local.model;
          return (
            <article className="skills-card" key={item.name}>
              <div className="skills-card-title">
                <div><h3>{item.name}</h3><small>{document ? "USER-OWNED" : item.builtIn ? "BUILT-IN" : "RUNTIME"}</small></div>
                {document && <span className={document.enabled ? "enabled" : "disabled"}>{document.enabled ? "Enabled" : "Disabled"}</span>}
              </div>
              <p>{String(item.description ?? item.source ?? "Available through the authenticated runtime.")}</p>
              {document && <code className="skills-path">{document.path_hint}</code>}
              {tab === "agents" && <dl><div><dt>Mode</dt><dd>{mode}</dd></div><div><dt>Model</dt><dd>{selectedModel == null ? modelLabel(item) : typeof selectedModel === "string" ? selectedModel : modelLabel(item)}</dd></div>{(item.steps ?? item.maxSteps ?? local.steps ?? local.maxSteps) != null && <div><dt>Max steps</dt><dd>{String(item.steps ?? item.maxSteps ?? local.steps ?? local.maxSteps)}</dd></div>}</dl>}
              {tab === "commands" && <dl><div><dt>Agent</dt><dd>{String(item.agent ?? local.agent ?? "default")}</dd></div><div><dt>Model</dt><dd>{item.model ?? local.model ? String(item.model ?? local.model) : modelLabel(item)}</dd></div></dl>}
              {tools.length > 0 && <div className="skills-setting"><strong>Tools</strong><div>{tools.map(([name, value]) => <span key={name}>{name}: {value}</span>)}</div></div>}
              {permissions.length > 0 && <div className="skills-setting"><strong>Permissions</strong><div>{permissions.map(([name, value]) => <span key={name}>{name}: {value}</span>)}</div></div>}
              {tab === "commands" && typeof item.template === "string" && <pre>{item.template}</pre>}
              <footer>
                {tab === "agents" && <button disabled={snapshot?.default_agent === item.name || busy === `choose-${item.name}` || document?.enabled === false || mode === "subagent"} title={mode === "subagent" ? "Subagents cannot be the session default" : undefined} onClick={() => void chooseAgent(item.name)}>{snapshot?.default_agent === item.name ? "Default agent" : mode === "subagent" ? "Subagent only" : "Use as default"}</button>}
                {document && <button onClick={() => beginEdit(document)}>Edit Markdown</button>}
                {document?.kind === "agent" && <button disabled={busy === document.name || (document.enabled && snapshot?.default_agent === document.name)} title={document.enabled && snapshot?.default_agent === document.name ? "Choose another default agent first" : undefined} onClick={() => void setEnabled(document)}>{document.enabled ? "Disable" : "Enable"}</button>}
                {document && <button className="danger" disabled={busy === document.name || (document.kind === "agent" && snapshot?.default_agent === document.name)} title={document.kind === "agent" && snapshot?.default_agent === document.name ? "Choose another default agent first" : undefined} onClick={() => void remove(document)}>Delete</button>}
              </footer>
            </article>
          );
        })}
      </div>
      {snapshot && visible.length === 0 && <div className="skills-empty">No {tab} match this view.</div>}

      {editor && (
        <div className="skills-dialog-backdrop" role="presentation">
          <section className="skills-dialog" role="dialog" aria-modal="true" aria-labelledby="skills-editor-title">
            <header><div><small>{editor.mode.toUpperCase()} USER-OWNED {editor.kind.toUpperCase()}</small><h3 id="skills-editor-title">Markdown editor</h3></div><button aria-label="Close editor" onClick={() => setEditor(null)}>×</button></header>
            <label>Name <input aria-label="Document name" value={editor.name} disabled={editor.mode === "edit"} onChange={(event) => { setConfirmed(false); setEditorError(""); setEditor({ ...editor, name: event.target.value }); }} placeholder="lowercase-slug" /></label>
            <label>Frontmatter and prompt <textarea aria-label="Document Markdown" value={editor.content} onChange={(event) => { setConfirmed(false); setEditorError(""); setEditor({ ...editor, content: event.target.value }); }} spellCheck={false} /></label>
            <p>Allowed paths are restricted to the managed OpenCode profile. Secrets, arbitrary paths and skill installation are not accepted.</p>
            {editorError && <div className="skills-message error" role="alert">{editorError}</div>}
            <label className="skills-confirm"><input type="checkbox" checked={confirmed} onChange={(event) => setConfirmed(event.target.checked)} />I reviewed this exact Markdown and confirm the {editor.mode}.</label>
            <footer><button onClick={() => setEditor(null)}>Cancel</button><button className="primary" disabled={!confirmed || !editor.name.trim() || busy === "editor"} onClick={() => void saveDocument()}>Save {editor.kind}</button></footer>
          </section>
        </div>
      )}
    </section>
  );
}
