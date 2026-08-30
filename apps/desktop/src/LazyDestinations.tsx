import { lazy, Suspense, useEffect, useState, type FormEvent } from "react";
import type React from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  AppConfig,
  EventEnvelope,
  Projection,
  RuntimeCatalog,
  VoiceStatus,
} from "./types";
import { eventPayload } from "./types";

const ConnectorManager = lazy(() =>
  import("./ConnectorManager").then(({ ConnectorManager: component }) => ({
    default: component,
  })),
);
const ScreenContext = lazy(() =>
  import("./ScreenContext").then(({ ScreenContext: component }) => ({
    default: component,
  })),
);
const McpManagerHost = lazy(() =>
  import("./McpManagerHost").then(({ McpManagerHost: component }) => ({
    default: component,
  })),
);
const LocalExecutionPanel = lazy(() =>
  import("./LocalExecutionPanel").then(
    ({ LocalExecutionPanel: component }) => ({ default: component }),
  ),
);
const PersistentTerminal = lazy(() =>
  import("./PersistentTerminal").then(({ PersistentTerminal: component }) => ({
    default: component,
  })),
);
const MemorySystemsPanel = lazy(() =>
  import("./MemorySystemsPanel").then(({ MemorySystemsPanel: component }) => ({
    default: component,
  })),
);

type Destination =
  | "Chat"
  | "Goals & tasks"
  | "Browser"
  | "Projects & terminal"
  | "Artifacts"
  | "History"
  | "Memory"
  | "Automations"
  | "Integrations"
  | "Skills & agents"
  | "Usage & egress"
  | "Diagnostics"
  | "Settings";
type Json = Record<string, unknown>;
type SlimBootstrap = {
  config: AppConfig;
  projection: Projection;
  history: EventEnvelope[];
  voice: VoiceStatus;
};
type Diagnostic = {
  product: string;
  version: string;
  platform: string;
  arch: string;
  opencode: { pinned: string; topology: string };
  capabilities: Array<{
    id: string;
    backend: string;
    status:
      | { state: string; reason?: string; remediation?: string }
      | string;
  }>;
};
type VoiceStateReference = Record<
  string,
  {
    glyph: string;
    label: string;
    hint: string;
    color: string;
    stoppable: boolean;
  }
>;

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

function InlineLoading({ label }: { label: string }) {
  return (
    <div className="empty" role="status">
      <span className="thinking-pulse" />
      <strong>Loading {label}…</strong>
    </div>
  );
}
type FeatureAuditItem = {
  area: string;
  status: "implemented" | "partial" | "not_wired";
  detail: string;
};

/// Derive the implementation audit from the live native capability probe.
///
/// The previous hardcoded table drifted out of date the moment a backend
/// changed, so the audit now reports whatever the platform actually reported:
/// `supported` is implemented, `degraded` is partial, and everything else is
/// not wired. The reason and remediation carry the detail.
function auditFromCapabilities(
  capabilities: Diagnostic["capabilities"],
): FeatureAuditItem[] {
  return capabilities.map((capability) => {
    const status =
      typeof capability.status === "string"
        ? { state: capability.status }
        : capability.status;
    const detail = [status.reason, status.remediation]
      .filter((part): part is string => Boolean(part && part.trim()))
      .join(" ");
    return {
      area: capability.id,
      status:
        status.state === "supported"
          ? "implemented"
          : status.state === "degraded"
            ? "partial"
            : "not_wired",
      detail: detail || capability.backend,
    };
  });
}
const DEFAULT_SESSION_LIMIT = 12;
const SESSION_LIMIT_STEP = 12;


export function DomainView({
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
  const memoryStyles = resourceData<Json[]>(catalog, "memory_styles", []);
  const memoryProjects = resourceData<{ nodes?: Json[]; relations?: Json[] }>(
    catalog,
    "memory_projects",
    {},
  );
  const items = destination === "Memory" ? [] : records(history, spec.prefix);
  useEffect(() => {
    let disposed = false;
    void invoke<RuntimeCatalog>("runtime_catalog", { includeMemory: true })
      .then((next) => {
        if (!disposed) setCatalog(next);
      })
      .catch((caught) => {
        if (!disposed) setError(String(caught));
      });
    return () => {
      disposed = true;
    };
  }, [setCatalog]);
  const refresh = async () => {
    const [bootstrap, nextCatalog] = await Promise.all([
      invoke<SlimBootstrap>("bootstrap"),
      invoke<RuntimeCatalog>("runtime_catalog", { includeMemory: true }),
    ]);
    setHistory(bootstrap.history);
    setCatalog(nextCatalog);
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
        {destination === "Memory" && (
          <Suspense fallback={<InlineLoading label="memory systems" />}>
            <MemorySystemsPanel
              memories={memories}
              styles={memoryStyles}
              projects={memoryProjects}
              onChanged={refresh}
            />
          </Suspense>
        )}
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

export function ProjectView({
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
    if (tab === "terminal") return;
    void load(
      tab === "diff"
        ? "vcs_diff"
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
        <div className="terminal-view terminal-stack">
          <Suspense fallback={<InlineLoading label="terminal" />}>
            <PersistentTerminal
              workingDirectory={config.runtime.working_directory}
              shell={String(config.workspace.terminal_shell ?? "")}
            />
            <details className="captured-execution">
              <summary>Captured commands and Docker jobs</summary>
              <LocalExecutionPanel
                workingDirectory={config.runtime.working_directory}
              />
            </details>
          </Suspense>
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

export function IntegrationsView({
  catalog,
}: {
  catalog: RuntimeCatalog;
  config: AppConfig;
  onCatalog: (catalog: RuntimeCatalog) => void;
}) {
  const providers = asArray(resourceData(catalog, "providers", []));
  return (
    <section className="catalog-page">
      <SectionHeader
        eyebrow="EXTENSIONS"
        title="Providers, MCP servers and integrations"
      />
      <Suspense fallback={<InlineLoading label="integrations" />}>
        <ConnectorManager />
        <McpManagerHost />
      </Suspense>
      <div className="catalog-columns integrations-providers">
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
      </div>
    </section>
  );
}

export function HistoryView({ history }: { history: EventEnvelope[] }) {
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
export function BrowserView({ config }: { config: AppConfig }) {
  const browser = config.browser as Json;
  const [address, setAddress] = useState("https://example.com");
  const [browserName, setBrowserName] = useState("firefox");
  const [snapshot, setSnapshot] = useState<Json | null>(null);
  const [opened, setOpened] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const run = async (
    operation: "open" | "navigate" | "snapshot" | "close" | "takeover",
  ) => {
    setBusy(true);
    setError("");
    try {
      if (operation === "open") {
        setSnapshot(
          await invoke<Json>("browser_open", {
            browserName,
            profileId: `desktop-${crypto.randomUUID()}`,
          }),
        );
        setOpened(true);
      } else if (operation === "navigate") {
        setSnapshot(await invoke<Json>("browser_navigate", { url: address }));
      } else if (operation === "close") {
        await invoke("browser_close");
        setOpened(false);
        setSnapshot(null);
      } else {
        const result = await invoke<Json>("browser_action", {
          operation,
          handle: null,
          text: null,
        });
        if (operation === "snapshot") setSnapshot(result);
      }
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy(false);
    }
  };
  const nodeAction = async (operation: "click" | "type", handle: unknown) => {
    const text = operation === "type" ? window.prompt("Text to type") : null;
    if (operation === "type" && text === null) return;
    setBusy(true);
    setError("");
    try {
      setSnapshot(
        await invoke<Json>("browser_action", { operation, handle, text }),
      );
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy(false);
    }
  };
  const handles =
    snapshot && Array.isArray(snapshot.handles) ? snapshot.handles : [];
  return (
    <section className="browser-page">
      <SectionHeader
        eyebrow="ISOLATED BROWSER"
        title="Browser automation boundaries"
      />
      {error && <p className="error-banner">{error}</p>}
      <div className="browser-frame">
        <div className="browser-address">
          <span>◎</span>
          <select
            aria-label="Browser engine"
            value={browserName}
            onChange={(event) => setBrowserName(event.target.value)}
            disabled={opened}
          >
            <option value="firefox">Firefox</option>
            <option value="chrome">Chrome</option>
            <option value="MicrosoftEdge">Edge</option>
            <option value="safari">Safari</option>
          </select>
          <input
            value={address}
            onChange={(event) => setAddress(event.target.value)}
            placeholder="https://…"
          />
          {!opened ? (
            <button
              disabled={!browser.enabled || busy}
              onClick={() => void run("open")}
            >
              {busy ? "Opening…" : "Open isolated profile"}
            </button>
          ) : (
            <>
              <button disabled={busy} onClick={() => void run("navigate")}>
                Go
              </button>
              <button disabled={busy} onClick={() => void run("snapshot")}>
                Refresh DOM
              </button>
              <button disabled={busy} onClick={() => void run("takeover")}>
                Take over
              </button>
              <button disabled={busy} onClick={() => void run("close")}>
                Close
              </button>
            </>
          )}
        </div>
        {snapshot ? (
          <div className="browser-snapshot">
            <header>
              <div>
                <strong>{String(snapshot.title ?? "Untitled page")}</strong>
                <small>{String(snapshot.url ?? address)}</small>
              </div>
              <span>generation {String(snapshot.generation ?? "?")}</span>
            </header>
            <pre>{String(snapshot.text ?? "")}</pre>
            <div className="browser-handle-list">
              {handles.map((handle, index) => (
                <article key={index}>
                  <span>Interactive element {index + 1}</span>
                  <button onClick={() => void nodeAction("click", handle)}>
                    Click
                  </button>
                  <button onClick={() => void nodeAction("type", handle)}>
                    Type
                  </button>
                </article>
              ))}
            </div>
          </div>
        ) : (
          <Empty
            title={
              browser.enabled
                ? "Open an isolated browser profile"
                : "Browser automation is off"
            }
            detail="Enable it in Settings. The app starts an installed WebDriver, uses DOM-first handles, and invalidates every handle after the page changes."
          />
        )}
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
      <Suspense fallback={<InlineLoading label="screen context" />}>
        <ScreenContext />
      </Suspense>
    </section>
  );
}
export function DiagnosticsView({
  diagnostic,
  catalog,
  projection,
  voice,
  voiceStates,
}: {
  diagnostic: Diagnostic;
  catalog: RuntimeCatalog;
  projection: Projection;
  voice: VoiceStatus;
  voiceStates: VoiceStateReference;
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
      <div className="capability-table" aria-label="Capability and resource truth">
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
        {auditFromCapabilities(diagnostic.capabilities).map((item) => (
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
