import { useEffect, useMemo, useRef, useState } from "react";
import type {
  JsonSchemaProperty,
  McpCatalogEntry,
  McpImportPreview,
  McpManagedServer,
  McpManagerAction,
  McpManagerActionResult,
  McpManagerController,
  McpManagerSnapshot,
  McpPermissionDecision,
  McpServerDefinition,
  McpTestOutput,
  McpToolDescriptor,
  McpToolPermissionRule,
  McpTransport,
} from "./McpManager.types";
import "./mcp-manager.css";

export type McpManagerProps = {
  snapshot: McpManagerSnapshot;
  controller: McpManagerController;
  catalog?: McpCatalogEntry[];
  onSnapshot?: (snapshot: McpManagerSnapshot) => void;
  className?: string;
};

type DetailTab = "overview" | "tools" | "resources" | "prompts" | "permissions" | "logs";
type WizardMode = "catalog" | "manual" | "import";
type ManualTransport = "stdio" | "streamable_http" | "legacy_sse";

type ConfirmOperation = {
  title: string;
  warning: string;
  displayText: string;
  confirmLabel: string;
  run: () => Promise<void>;
};

const lifecycleLabel: Record<McpManagedServer["state"], string> = {
  draft: "Draft",
  install_consent_required: "Install approval",
  installing: "Installing",
  disabled: "Disabled",
  connecting: "Connecting",
  connected: "Connected",
  degraded: "Degraded",
  authentication_required: "Sign-in required",
  crashed: "Crashed",
  update_available: "Update available",
  updating: "Updating",
  rollback_available: "Rollback available",
  uninstalling: "Uninstalling",
  uninstalled: "Uninstalled",
};

function makeId(): string {
  return globalThis.crypto?.randomUUID?.() ?? `mcp-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function splitArguments(value: string): string[] {
  return value
    .split("\n")
    .map((argument) => argument.trim())
    .filter(Boolean);
}

function readableTransport(server: McpManagedServer): string {
  switch (server.definition.transport.kind) {
    case "stdio":
      return "Local stdio";
    case "streamable_http":
      return server.definition.transport.stateless
        ? "Streamable HTTP · stateless"
        : "Streamable HTTP · session";
    case "legacy_sse":
      return "Legacy HTTP + SSE";
  }
}

function shortDate(value?: string | null): string {
  if (!value) return "Never";
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? value : parsed.toLocaleString();
}

function sourceLabel(server: McpManagedServer): string {
  const source = server.definition.source;
  switch (source.kind) {
    case "catalog":
      return `${source.publisher} catalog`;
    case "manual":
      return "Manual configuration";
    case "imported":
      return `Imported from ${source.application}`;
    case "local_package":
      return source.package;
    case "remote":
      return source.origin;
  }
}

function transportIdentity(transport: McpTransport): string {
  if (transport.kind === "stdio") {
    return [transport.executable, ...transport.arguments].join(" ");
  }
  return transport.endpoint;
}

export function McpManager({
  snapshot,
  controller,
  catalog = [],
  onSnapshot,
  className = "",
}: McpManagerProps) {
  const [search, setSearch] = useState("");
  const [statusFilter, setStatusFilter] = useState<"all" | "connected" | "attention" | "disabled">("all");
  const [selectedId, setSelectedId] = useState<string | null>(snapshot.servers[0]?.definition.id ?? null);
  const [detailTab, setDetailTab] = useState<DetailTab>("overview");
  const [wizardOpen, setWizardOpen] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirmOperation, setConfirmOperation] = useState<ConfirmOperation | null>(null);
  const [exportText, setExportText] = useState<string | null>(null);
  const [testOutput, setTestOutput] = useState<McpTestOutput | null>(null);
  const inFlightActions = useRef(new Set<string>());

  useEffect(() => {
    if (selectedId && snapshot.servers.some((server) => server.definition.id === selectedId)) return;
    setSelectedId(snapshot.servers[0]?.definition.id ?? null);
  }, [selectedId, snapshot.servers]);

  const selected = snapshot.servers.find((server) => server.definition.id === selectedId) ?? null;
  const filtered = useMemo(() => {
    const query = search.trim().toLocaleLowerCase();
    return snapshot.servers.filter((server) => {
      const statusMatch =
        statusFilter === "all" ||
        (statusFilter === "connected" && server.state === "connected") ||
        (statusFilter === "disabled" && server.state === "disabled") ||
        (statusFilter === "attention" &&
          ["degraded", "authentication_required", "crashed", "update_available", "install_consent_required"].includes(
            server.state,
          ));
      if (!statusMatch) return false;
      if (!query) return true;
      return [
        server.definition.name,
        server.definition.namespace,
        server.definition.description,
        readableTransport(server),
        ...server.definition.tags,
        ...server.catalog.tools.map((tool) => tool.resolved_name),
      ].some((value) => value.toLocaleLowerCase().includes(query));
    });
  }, [search, snapshot.servers, statusFilter]);

  async function execute(
    action: McpManagerAction,
    key: string = "server_id" in action ? `${action.type}:${action.server_id}` : action.type,
  ): Promise<McpManagerActionResult> {
    if (inFlightActions.current.has(key)) return {};
    inFlightActions.current.add(key);
    setBusy(key);
    setError(null);
    setNotice(null);
    try {
      const result = await controller.execute(action);
      if (result.snapshot) onSnapshot?.(result.snapshot);
      if (result.message) setNotice(result.message);
      if (result.test_output) setTestOutput(result.test_output);
      if (
        action.type === "test_tool" &&
        !action.approval_digest &&
        result.operation_preview
      ) {
        const preview = result.operation_preview;
        setConfirmOperation({
          title: `Run ${action.tool}?`,
          warning:
            "Review the exact MCP tool and arguments. The request will still pass through Personal Agent's native limits and audit trail.",
          displayText: preview.display_text,
          confirmLabel: "Run tool",
          run: async () => {
            await execute({
              ...action,
              approval_digest: preview.digest,
            });
            setConfirmOperation(null);
          },
        });
      }
      return result;
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      throw cause;
    } finally {
      inFlightActions.current.delete(key);
      setBusy(null);
    }
  }

  async function refresh(): Promise<void> {
    try {
      await execute({ type: "refresh" });
    } catch {
      // The shared error banner already contains the native failure.
    }
  }

  async function previewDestructiveOperation(
    server: McpManagedServer,
    previewType: "install_preview" | "update_preview" | "rollback_preview" | "uninstall_preview",
    actionType: "install" | "update" | "rollback" | "uninstall",
    title: string,
    warning: string,
    confirmLabel: string,
  ): Promise<void> {
    try {
      const result = await execute({ type: previewType, server_id: server.definition.id });
      const preview = result.operation_preview;
      if (!preview) throw new Error("Native MCP manager did not return an operation preview.");
      setConfirmOperation({
        title,
        warning,
        displayText: preview.display_text,
        confirmLabel,
        run: async () => {
          await execute({
            type: actionType,
            server_id: server.definition.id,
            operation_digest: preview.digest,
          });
          setConfirmOperation(null);
        },
      });
    } catch {
      // Shared error banner.
    }
  }

  async function exportConfiguration(): Promise<void> {
    try {
      const result = await execute({ type: "export" });
      if (!result.export_json) throw new Error("No export document was returned.");
      setExportText(result.export_json);
    } catch {
      // Shared error banner.
    }
  }

  const connected = snapshot.servers.filter((server) => server.state === "connected").length;
  const attention = snapshot.servers.filter((server) =>
    ["degraded", "authentication_required", "crashed", "update_available", "install_consent_required"].includes(
      server.state,
    ),
  ).length;

  return (
    <section className={`mcp-manager ${className}`} aria-label="MCP Manager">
      <header className="mcp-manager-header">
        <div>
          <span className="mcp-eyebrow">INTEGRATIONS / MODEL CONTEXT PROTOCOL</span>
          <h1>MCP Manager</h1>
          <p>Connect tools without editing JSON. Credentials stay in your OS keychain.</p>
        </div>
        <div className="mcp-header-actions">
          <button className="mcp-button mcp-button-subtle" type="button" onClick={() => void exportConfiguration()}>
            Export safely
          </button>
          <button className="mcp-button mcp-button-subtle" type="button" onClick={() => void refresh()} disabled={busy === "refresh"}>
            {busy === "refresh" ? "Refreshing…" : "Refresh"}
          </button>
          <button className="mcp-button mcp-button-primary" type="button" onClick={() => setWizardOpen(true)}>
            + Add MCP server
          </button>
        </div>
      </header>

      <div className="mcp-summary" aria-label="MCP summary">
        <SummaryMetric label="Servers" value={snapshot.servers.length} />
        <SummaryMetric label="Connected" value={connected} tone="good" />
        <SummaryMetric label="Needs attention" value={attention} tone={attention ? "warn" : undefined} />
        <SummaryMetric
          label="Available tools"
          value={snapshot.servers.reduce((total, server) => total + server.catalog.tools.length, 0)}
        />
        <div className="mcp-protocol-chip" title="Client protocol; older servers are negotiated automatically">
          MCP {snapshot.protocol_version}
        </div>
      </div>

      {error ? (
        <div className="mcp-banner mcp-banner-error" role="alert">
          <span>{error}</span>
          <button type="button" onClick={() => setError(null)} aria-label="Dismiss error">×</button>
        </div>
      ) : null}
      {notice ? (
        <div className="mcp-banner mcp-banner-success" role="status">
          <span>{notice}</span>
          <button type="button" onClick={() => setNotice(null)} aria-label="Dismiss message">×</button>
        </div>
      ) : null}

      <div className="mcp-toolbar">
        <label className="mcp-search">
          <span aria-hidden="true">⌕</span>
          <span className="sr-only">Search MCP servers and tools</span>
          <input
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            placeholder="Search servers, tools, tags…"
          />
          {search ? (
            <button type="button" aria-label="Clear search" onClick={() => setSearch("")}>×</button>
          ) : null}
        </label>
        <div className="mcp-filter-group" role="group" aria-label="Filter servers">
          {(["all", "connected", "attention", "disabled"] as const).map((filter) => (
            <button
              key={filter}
              type="button"
              className={statusFilter === filter ? "active" : ""}
              onClick={() => setStatusFilter(filter)}
            >
              {filter === "all" ? "All" : filter[0]?.toUpperCase() + filter.slice(1)}
            </button>
          ))}
        </div>
      </div>

      <div className={`mcp-content ${selected ? "has-details" : ""}`}>
        <div className="mcp-server-list" aria-label="Configured MCP servers">
          {filtered.length ? (
            filtered.map((server) => (
              <ServerCard
                key={server.definition.id}
                server={server}
                selected={selectedId === server.definition.id}
                busy={busy}
                onSelect={() => setSelectedId(server.definition.id)}
                onAction={(action) => void execute(action).catch(() => undefined)}
                onPreview={(previewType, actionType, title, warning, label) =>
                  void previewDestructiveOperation(server, previewType, actionType, title, warning, label)
                }
              />
            ))
          ) : (
            <div className="mcp-empty">
              <div className="mcp-empty-icon" aria-hidden="true">◇</div>
              <h2>{snapshot.servers.length ? "No servers match" : "No MCP servers yet"}</h2>
              <p>
                {snapshot.servers.length
                  ? "Clear the search or change the status filter."
                  : "Add a verified server, connect a remote endpoint, or import an existing configuration."}
              </p>
              {!snapshot.servers.length ? (
                <button className="mcp-button mcp-button-primary" type="button" onClick={() => setWizardOpen(true)}>
                  Add your first server
                </button>
              ) : null}
            </div>
          )}
        </div>

        {selected ? (
          <ServerDetails
            server={selected}
            tab={detailTab}
            busy={busy}
            onTab={setDetailTab}
            onClose={() => setSelectedId(null)}
            onAction={(action) => void execute(action).catch(() => undefined)}
            onPreview={(previewType, actionType, title, warning, label) =>
              void previewDestructiveOperation(
                selected,
                previewType,
                actionType,
                title,
                warning,
                label,
              )
            }
            testOutput={testOutput}
          />
        ) : null}
      </div>

      {wizardOpen ? (
        <AddServerWizard
          catalog={catalog}
          protocolVersion={snapshot.protocol_version}
          controllerExecute={execute}
          onClose={() => setWizardOpen(false)}
          onAdded={(id) => {
            setSelectedId(id);
            setWizardOpen(false);
          }}
        />
      ) : null}

      {confirmOperation ? (
        <ConfirmationDialog
          operation={confirmOperation}
          busy={Boolean(busy)}
          onClose={() => setConfirmOperation(null)}
        />
      ) : null}

      {exportText ? <ExportDialog value={exportText} onClose={() => setExportText(null)} /> : null}
    </section>
  );
}

function SummaryMetric({
  label,
  value,
  tone,
}: {
  label: string;
  value: number;
  tone?: "good" | "warn";
}) {
  return (
    <div className={`mcp-summary-metric ${tone ?? ""}`}>
      <strong>{value}</strong>
      <span>{label}</span>
    </div>
  );
}

function StatusDot({ state }: { state: McpManagedServer["state"] }) {
  const tone =
    state === "connected"
      ? "good"
      : ["degraded", "update_available", "install_consent_required", "rollback_available"].includes(state)
        ? "warn"
        : ["crashed", "authentication_required"].includes(state)
          ? "bad"
          : "muted";
  return <span className={`mcp-status-dot ${tone}`} aria-hidden="true" />;
}

function ServerCard({
  server,
  selected,
  busy,
  onSelect,
  onAction,
  onPreview,
}: {
  server: McpManagedServer;
  selected: boolean;
  busy: string | null;
  onSelect: () => void;
  onAction: (action: McpManagerAction) => void;
  onPreview: (
    previewType: "install_preview" | "update_preview" | "rollback_preview" | "uninstall_preview",
    actionType: "install" | "update" | "rollback" | "uninstall",
    title: string,
    warning: string,
    label: string,
  ) => void;
}) {
  const id = server.definition.id;
  const working = busy?.includes(id) || ["installing", "connecting", "updating", "uninstalling"].includes(server.state);
  return (
    <article className={`mcp-server-card ${selected ? "selected" : ""}`}>
      <button type="button" className="mcp-server-card-main" onClick={onSelect} aria-label={`Open ${server.definition.name}`}>
        <span className="mcp-server-icon" aria-hidden="true">
          {server.definition.name.trim().slice(0, 1).toUpperCase() || "M"}
        </span>
        <span className="mcp-server-copy">
          <span className="mcp-server-title-row">
            <strong>{server.definition.name}</strong>
            <span className={`mcp-state-chip state-${server.state}`}>
              <StatusDot state={server.state} />
              {lifecycleLabel[server.state]}
            </span>
          </span>
          <span className="mcp-server-description">{server.definition.description || "No description"}</span>
          <span className="mcp-server-meta">
            <span>{readableTransport(server)}</span>
            <span>{server.catalog.tools.length} tools</span>
            <span>{server.health?.latency_ms != null ? `${server.health.latency_ms} ms` : "Not measured"}</span>
          </span>
        </span>
      </button>
      <div className="mcp-card-actions">
        {server.state === "install_consent_required" ? (
          <button
            className="mcp-button mcp-button-primary"
            type="button"
            disabled={working}
            onClick={() => onPreview("install_preview", "install", `Install ${server.definition.name}?`, "Review the exact native command. Personal Agent invokes it directly without a shell.", "Install")}
          >
            Install
          </button>
        ) : null}
        {["disabled", "crashed", "degraded", "rollback_available"].includes(server.state) ? (
          <button
            className="mcp-button mcp-button-primary"
            type="button"
            disabled={working}
            onClick={() => onAction({ type: "connect", server_id: id })}
          >
            {working ? "Connecting…" : "Connect"}
          </button>
        ) : null}
        {server.state === "authentication_required" ? (
          <button className="mcp-button mcp-button-primary" type="button" disabled={working} onClick={() => onAction({ type: "start_oauth", server_id: id })}>
            {working ? "Signing in…" : "Sign in"}
          </button>
        ) : null}
        {server.state === "connected" ? (
          <>
            <button className="mcp-button mcp-button-subtle" type="button" onClick={() => onAction({ type: "health", server_id: id })}>
              Check health
            </button>
            <button className="mcp-button mcp-button-subtle" type="button" onClick={() => onAction({ type: "restart", server_id: id })}>
              Restart
            </button>
            <button className="mcp-button mcp-button-subtle" type="button" onClick={() => onAction({ type: "disable", server_id: id })}>
              Disable
            </button>
          </>
        ) : null}
        {server.pending_update && !["updating", "uninstalling", "uninstalled"].includes(server.state) ? (
          <button
            className="mcp-button mcp-button-primary"
            type="button"
            onClick={() => onPreview("update_preview", "update", `Update ${server.definition.name}?`, "The previous release will be retained for rollback.", "Update")}
          >
            Update to {server.pending_update?.target_version}
          </button>
        ) : null}
        {server.release_history.length && !["updating", "uninstalling", "uninstalled"].includes(server.state) ? (
          <button
            className="mcp-button mcp-button-subtle"
            type="button"
            onClick={() => onPreview("rollback_preview", "rollback", `Roll back ${server.definition.name}?`, "The server will be disabled until it reconnects on the restored release.", "Roll back")}
          >
            Roll back
          </button>
        ) : null}
        <button className="mcp-icon-button" type="button" aria-label={`Open details for ${server.definition.name}`} onClick={onSelect}>
          ›
        </button>
      </div>
    </article>
  );
}

function ServerDetails({
  server,
  tab,
  busy,
  onTab,
  onClose,
  onAction,
  onPreview,
  testOutput,
}: {
  server: McpManagedServer;
  tab: DetailTab;
  busy: string | null;
  onTab: (tab: DetailTab) => void;
  onClose: () => void;
  onAction: (action: McpManagerAction) => void;
  onPreview: (
    previewType: "install_preview" | "update_preview" | "rollback_preview" | "uninstall_preview",
    actionType: "install" | "update" | "rollback" | "uninstall",
    title: string,
    warning: string,
    label: string,
  ) => void;
  testOutput: McpTestOutput | null;
}) {
  return (
    <aside className="mcp-details" aria-label={`${server.definition.name} details`}>
      <header className="mcp-details-header">
        <div className="mcp-server-icon" aria-hidden="true">
          {server.definition.name.trim().slice(0, 1).toUpperCase() || "M"}
        </div>
        <div>
          <span className="mcp-eyebrow">{server.definition.namespace}</span>
          <h2>{server.definition.name}</h2>
          <span className={`mcp-state-chip state-${server.state}`}>
            <StatusDot state={server.state} /> {lifecycleLabel[server.state]}
          </span>
        </div>
        <button className="mcp-icon-button" type="button" onClick={onClose} aria-label="Close server details">×</button>
      </header>
      <nav className="mcp-detail-tabs" aria-label="Server details sections">
        {(["overview", "tools", "resources", "prompts", "permissions", "logs"] as const).map((item) => (
          <button key={item} className={tab === item ? "active" : ""} type="button" onClick={() => onTab(item)}>
            {item[0]?.toUpperCase() + item.slice(1)}
            {item === "tools" ? <span>{server.catalog.tools.length}</span> : null}
            {item === "logs" && server.logs.some((log) => log.level === "error") ? <i aria-label="Errors present" /> : null}
          </button>
        ))}
      </nav>
      <div className="mcp-details-body">
        {tab === "overview" ? <Overview server={server} busy={busy} onAction={onAction} onPreview={onPreview} /> : null}
        {tab === "tools" ? <ToolExplorer server={server} busy={busy} onAction={onAction} testOutput={testOutput} /> : null}
        {tab === "resources" ? <ResourceList server={server} /> : null}
        {tab === "prompts" ? <PromptList server={server} /> : null}
        {tab === "permissions" ? <PermissionsEditor server={server} onAction={onAction} /> : null}
        {tab === "logs" ? <LogViewer server={server} /> : null}
      </div>
    </aside>
  );
}

function Overview({
  server,
  busy,
  onAction,
  onPreview,
}: {
  server: McpManagedServer;
  busy: string | null;
  onAction: (action: McpManagerAction) => void;
  onPreview: (
    previewType: "uninstall_preview",
    actionType: "uninstall",
    title: string,
    warning: string,
    label: string,
  ) => void;
}) {
  const [projectScopes, setProjectScopes] = useState(server.definition.project_scopes.join(", "));
  const [agentScopes, setAgentScopes] = useState(server.definition.agent_scopes.join(", "));
  useEffect(() => {
    setProjectScopes(server.definition.project_scopes.join(", "));
    setAgentScopes(server.definition.agent_scopes.join(", "));
  }, [server.definition.agent_scopes, server.definition.project_scopes]);

  function values(input: string): string[] {
    return [...new Set(input.split(",").map((value) => value.trim()).filter(Boolean))];
  }

  return (
    <div className="mcp-detail-stack">
      <section className="mcp-detail-section">
        <h3>Connection</h3>
        <dl className="mcp-definition-grid">
          <div><dt>Transport</dt><dd>{readableTransport(server)}</dd></div>
          <div><dt>Endpoint / command</dt><dd><code>{transportIdentity(server.definition.transport)}</code></dd></div>
          <div><dt>Protocol</dt><dd>{server.negotiated_protocol ?? "Negotiated at connect"}</dd></div>
          <div><dt>Source</dt><dd>{sourceLabel(server)}</dd></div>
          <div><dt>Last connected</dt><dd>{shortDate(server.last_connected_at)}</dd></div>
          <div><dt>Release</dt><dd>{server.current_release?.version ?? "Externally managed"}</dd></div>
        </dl>
      </section>
      <section className="mcp-detail-section">
        <div className="mcp-section-heading">
          <div><h3>Health</h3><p>Connection quality and recent failures</p></div>
          <button className="mcp-button mcp-button-subtle" type="button" onClick={() => onAction({ type: "health", server_id: server.definition.id })} disabled={!server.enabled}>
            Run check
          </button>
        </div>
        {server.health ? (
          <div className={`mcp-health-panel ${server.health.healthy ? "healthy" : "unhealthy"}`}>
            <strong>{server.health.message}</strong>
            <span>{server.health.latency_ms != null ? `${server.health.latency_ms} ms latency` : "No response"}</span>
            <span>{Math.round(server.health.error_rate * 100)}% rolling errors</span>
            <span>Checked {shortDate(server.health.checked_at)}</span>
          </div>
        ) : <p className="mcp-muted">No health sample yet.</p>}
      </section>
      <section className="mcp-detail-section">
        <h3>Enable scopes</h3>
        <p>Leave both blank to make this server available globally. Scope changes never grant tool permissions.</p>
        <label className="mcp-field">
          <span>Projects <small>comma-separated IDs</small></span>
          <input value={projectScopes} onChange={(event) => setProjectScopes(event.target.value)} placeholder="personal-agent, work-notes" />
        </label>
        <label className="mcp-field">
          <span>Agents <small>comma-separated IDs</small></span>
          <input value={agentScopes} onChange={(event) => setAgentScopes(event.target.value)} placeholder="jarvis, coding-agent" />
        </label>
        <button
          className="mcp-button mcp-button-primary"
          type="button"
          onClick={() => onAction({
            type: "set_scopes",
            server_id: server.definition.id,
            project_scopes: values(projectScopes),
            agent_scopes: values(agentScopes),
          })}
        >
          Save scopes
        </button>
      </section>
      <section className="mcp-detail-section">
        <h3>Authentication</h3>
        <p>Tokens never enter MCP configuration. OAuth grants and API keys are referenced from the OS keychain.</p>
        <div className="mcp-inline-actions">
          <button className="mcp-button mcp-button-subtle" type="button" disabled={busy?.includes(server.definition.id)} onClick={() => onAction({ type: "start_oauth", server_id: server.definition.id })}>
            {busy?.includes(server.definition.id) ? "Signing in…" : "Connect OAuth"}
          </button>
          <button className="mcp-button mcp-button-subtle" type="button" onClick={() => onAction({ type: "open_keychain_setup", server_id: server.definition.id })}>
            Add key securely
          </button>
        </div>
      </section>
      <section className="mcp-detail-section mcp-danger-zone">
        <h3>{server.state === "uninstalled" ? "Remove audit tombstone" : "Uninstall server"}</h3>
        <p>
          {server.state === "uninstalled"
            ? "Permanently remove this already-uninstalled server from the manager."
            : "Disconnect the runtime, remove managed package artifacts when present, and retain an auditable tombstone."}
        </p>
        {server.state === "uninstalled" ? (
          <button className="mcp-button mcp-button-danger" type="button" onClick={() => onAction({ type: "purge", server_id: server.definition.id })}>
            Purge tombstone
          </button>
        ) : (
          <button
            className="mcp-button mcp-button-danger"
            type="button"
            onClick={() => onPreview(
              "uninstall_preview",
              "uninstall",
              `Uninstall ${server.definition.name}?`,
              "This removes the managed package when one exists and disconnects its runtime configuration. An audit tombstone is retained.",
              "Uninstall",
            )}
          >
            Uninstall server
          </button>
        )}
      </section>
    </div>
  );
}

function ToolExplorer({
  server,
  busy,
  onAction,
  testOutput,
}: {
  server: McpManagedServer;
  busy: string | null;
  onAction: (action: McpManagerAction) => void;
  testOutput: McpTestOutput | null;
}) {
  const [query, setQuery] = useState("");
  const visible = server.catalog.tools.filter((tool) =>
    [tool.name, tool.title ?? "", tool.description, tool.resolved_name]
      .join(" ")
      .toLocaleLowerCase()
      .includes(query.toLocaleLowerCase()),
  );
  const [selectedName, setSelectedName] = useState(server.catalog.tools[0]?.resolved_name ?? null);
  useEffect(() => {
    if (selectedName && server.catalog.tools.some((tool) => tool.resolved_name === selectedName)) return;
    setSelectedName(server.catalog.tools[0]?.resolved_name ?? null);
  }, [selectedName, server.catalog.tools]);
  const selected = server.catalog.tools.find((tool) => tool.resolved_name === selectedName) ?? null;
  return (
    <div className="mcp-explorer">
      <label className="mcp-search compact">
        <span aria-hidden="true">⌕</span>
        <span className="sr-only">Search tools</span>
        <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search tools…" />
      </label>
      <div className="mcp-tool-layout">
        <div className="mcp-tool-list">
          {visible.map((tool) => (
            <button key={tool.resolved_name} className={selectedName === tool.resolved_name ? "active" : ""} type="button" onClick={() => setSelectedName(tool.resolved_name)}>
              <span><strong>{tool.title || tool.name}</strong><code>{tool.resolved_name}</code></span>
              <RiskBadges tool={tool} />
            </button>
          ))}
          {!visible.length ? <p className="mcp-muted">No matching tools.</p> : null}
        </div>
        {selected ? (
          <div className="mcp-tool-detail">
            <div className="mcp-section-heading">
              <div><h3>{selected.title || selected.name}</h3><code>{selected.resolved_name}</code></div>
              <RiskBadges tool={selected} />
            </div>
            <p>{selected.description || "No description provided by this server."}</p>
            <p className="mcp-untrusted-note">Server descriptions and schemas are untrusted metadata. They never override Personal Agent policy.</p>
            <GeneratedToolForm
              key={selected.resolved_name}
              tool={selected}
              disabled={!server.enabled || busy === `test:${selected.resolved_name}`}
              onRun={(arguments_) => onAction({
                type: "test_tool",
                server_id: server.definition.id,
                tool: selected.resolved_name,
                arguments: arguments_,
              })}
            />
            {testOutput?.tool === selected.resolved_name ? (
              <section className="mcp-test-output" aria-live="polite">
                <div>
                  <h4>Tool result</h4>
                  <span>{testOutput.duration_ms} ms{testOutput.truncated ? " · truncated" : ""}</span>
                </div>
                <pre>{JSON.stringify(testOutput.content, null, 2)}</pre>
              </section>
            ) : null}
          </div>
        ) : null}
      </div>
    </div>
  );
}

function RiskBadges({ tool }: { tool: McpToolDescriptor }) {
  return (
    <span className="mcp-risk-badges">
      {tool.annotations.read_only ? <em className="safe">Read only</em> : null}
      {tool.annotations.destructive ? <em className="danger">Destructive</em> : null}
      {tool.annotations.open_world ? <em className="warn">External action</em> : null}
      {!tool.annotations.read_only && !tool.annotations.destructive && !tool.annotations.open_world ? <em>Unclassified</em> : null}
    </span>
  );
}

function GeneratedToolForm({
  tool,
  disabled,
  onRun,
}: {
  tool: McpToolDescriptor;
  disabled: boolean;
  onRun: (arguments_: Record<string, unknown>) => void;
}) {
  const properties = Object.entries(tool.input_schema.properties ?? {});
  const required = new Set(tool.input_schema.required ?? []);
  const [values, setValues] = useState<Record<string, string | boolean>>(() =>
    Object.fromEntries(
      properties.map(([name, schema]) => [name, schema.type === "boolean" ? Boolean(schema.default) : String(schema.default ?? "")]),
    ),
  );
  const [formError, setFormError] = useState<string | null>(null);

  function submit(): void {
    try {
      const arguments_: Record<string, unknown> = {};
      for (const [name, schema] of properties) {
        const raw = values[name];
        if (required.has(name) && (raw === "" || raw == null)) throw new Error(`${schema.title ?? name} is required.`);
        if (raw === "" || raw == null) continue;
        if (schema.type === "number" || schema.type === "integer") {
          const value = Number(raw);
          if (!Number.isFinite(value)) throw new Error(`${schema.title ?? name} must be a number.`);
          arguments_[name] = schema.type === "integer" ? Math.trunc(value) : value;
        } else if (schema.type === "boolean") {
          arguments_[name] = Boolean(raw);
        } else if (schema.type === "array" || schema.type === "object") {
          arguments_[name] = JSON.parse(String(raw)) as unknown;
        } else {
          arguments_[name] = String(raw);
        }
      }
      setFormError(null);
      onRun(arguments_);
    } catch (cause) {
      setFormError(cause instanceof Error ? cause.message : String(cause));
    }
  }

  return (
    <form className="mcp-generated-form" onSubmit={(event) => { event.preventDefault(); submit(); }}>
      <div className="mcp-form-heading"><h4>Test tool</h4><span>Runs through ToolGateway and may require approval</span></div>
      {properties.length ? properties.map(([name, schema]) => (
        <GeneratedField
          key={name}
          name={name}
          schema={schema}
          required={required.has(name)}
          value={values[name] ?? ""}
          onChange={(value) => setValues((current) => ({ ...current, [name]: value }))}
        />
      )) : <p className="mcp-muted">This tool has no input fields.</p>}
      {formError ? <p className="mcp-form-error" role="alert">{formError}</p> : null}
      <button className="mcp-button mcp-button-primary" type="submit" disabled={disabled}>
        {disabled && !tool ? "Unavailable" : "Review & run"}
      </button>
    </form>
  );
}

function GeneratedField({
  name,
  schema,
  required,
  value,
  onChange,
}: {
  name: string;
  schema: JsonSchemaProperty;
  required: boolean;
  value: string | boolean;
  onChange: (value: string | boolean) => void;
}) {
  const label = schema.title ?? name;
  if (schema.type === "boolean") {
    return (
      <label className="mcp-toggle-field">
        <input type="checkbox" checked={Boolean(value)} onChange={(event) => onChange(event.target.checked)} />
        <span>{label}{required ? " *" : ""}<small>{schema.description}</small></span>
      </label>
    );
  }
  if (schema.enum?.length) {
    return (
      <label className="mcp-field">
        <span>{label}{required ? " *" : ""}<small>{schema.description}</small></span>
        <select aria-label={`${label}${required ? " *" : ""}`} value={String(value)} onChange={(event) => onChange(event.target.value)}>
          {!required ? <option value="">Not set</option> : null}
          {schema.enum.map((option) => <option key={String(option)} value={String(option)}>{String(option)}</option>)}
        </select>
      </label>
    );
  }
  const structured = schema.type === "array" || schema.type === "object";
  return (
    <label className="mcp-field">
      <span>{label}{required ? " *" : ""}<small>{schema.description}</small></span>
      {structured ? (
        <textarea aria-label={`${label}${required ? " *" : ""}`} value={String(value)} onChange={(event) => onChange(event.target.value)} placeholder={schema.type === "array" ? "[]" : "{}"} rows={3} />
      ) : (
        <input
          aria-label={`${label}${required ? " *" : ""}`}
          type={schema.type === "number" || schema.type === "integer" ? "number" : "text"}
          value={String(value)}
          min={schema.minimum}
          max={schema.maximum}
          step={schema.type === "integer" ? 1 : undefined}
          onChange={(event) => onChange(event.target.value)}
        />
      )}
    </label>
  );
}

function ResourceList({ server }: { server: McpManagedServer }) {
  return (
    <div className="mcp-catalog-list">
      {server.catalog.resources.map((resource) => (
        <article key={resource.uri}><div><strong>{resource.name}</strong><code>{resource.uri}</code></div><span>{resource.mime_type ?? "unknown type"}</span><p>{resource.description}</p></article>
      ))}
      {!server.catalog.resources.length ? <EmptyCatalog title="No resources" text="This server did not advertise any resources." /> : null}
    </div>
  );
}

function PromptList({ server }: { server: McpManagedServer }) {
  return (
    <div className="mcp-catalog-list">
      {server.catalog.prompts.map((prompt) => (
        <article key={prompt.name}><div><strong>{prompt.name}</strong></div><p>{prompt.description}</p></article>
      ))}
      {!server.catalog.prompts.length ? <EmptyCatalog title="No prompts" text="This server did not advertise any prompts." /> : null}
    </div>
  );
}

function EmptyCatalog({ title, text }: { title: string; text: string }) {
  return <div className="mcp-empty compact"><div className="mcp-empty-icon" aria-hidden="true">◇</div><h3>{title}</h3><p>{text}</p></div>;
}

function PermissionsEditor({ server, onAction }: { server: McpManagedServer; onAction: (action: McpManagerAction) => void }) {
  function globalRule(tool: McpToolDescriptor): McpToolPermissionRule {
    return server.permissions.find((rule) => rule.tool === tool.resolved_name && rule.scope.kind === "global") ?? {
      tool: tool.resolved_name,
      scope: { kind: "global" },
      decision: "ask",
      execution_zone: "mcp-restricted",
      max_calls_per_minute: 30,
      timeout_ms: 30_000,
      max_output_bytes: 1_048_576,
    };
  }
  function update(tool: McpToolDescriptor, decision: McpPermissionDecision): void {
    onAction({ type: "set_permission", server_id: server.definition.id, rule: { ...globalRule(tool), decision } });
  }
  return (
    <div className="mcp-detail-stack">
      <section className="mcp-detail-section">
        <h3>Default tool permissions</h3>
        <p>These checks run before every MCP call. Destructive and external actions still require ToolGateway approval even when allowed here.</p>
        <div className="mcp-permission-list">
          {server.catalog.tools.map((tool) => {
            const rule = globalRule(tool);
            return (
              <div className="mcp-permission-row" key={tool.resolved_name}>
                <div><strong>{tool.title || tool.name}</strong><code>{tool.resolved_name}</code><RiskBadges tool={tool} /></div>
                <label><span className="sr-only">Permission for {tool.resolved_name}</span><select value={rule.decision} onChange={(event) => update(tool, event.target.value as McpPermissionDecision)}><option value="ask">Ask every time</option><option value="allow">Allow</option><option value="deny">Disabled</option></select></label>
              </div>
            );
          })}
          {!server.catalog.tools.length ? <p className="mcp-muted">Connect the server to discover tools.</p> : null}
        </div>
      </section>
      <section className="mcp-security-callout">
        <strong>ToolGateway is always in control</strong>
        <p>MCP tools cannot bypass execution zones, egress policy, approvals, output limits, or the audit log.</p>
      </section>
    </div>
  );
}

function LogViewer({ server }: { server: McpManagedServer }) {
  const [level, setLevel] = useState<"all" | "info" | "warn" | "error">("all");
  const logs = [...server.logs].reverse().filter((log) => level === "all" || log.level === level);
  return (
    <div className="mcp-log-viewer">
      <div className="mcp-section-heading"><div><h3>Lifecycle logs</h3><p>Credential-like values are redacted at the native boundary.</p></div><select value={level} onChange={(event) => setLevel(event.target.value as typeof level)}><option value="all">All levels</option><option value="info">Info</option><option value="warn">Warnings</option><option value="error">Errors</option></select></div>
      <div className="mcp-log-lines" aria-live="polite">
        {logs.map((log, index) => <div key={`${log.timestamp}-${index}`} className={`level-${log.level}`}><time>{shortDate(log.timestamp)}</time><span>{log.level.toUpperCase()}</span><code>{log.message}</code></div>)}
        {!logs.length ? <p className="mcp-muted">No matching lifecycle logs.</p> : null}
      </div>
    </div>
  );
}

function AddServerWizard({
  catalog,
  protocolVersion,
  controllerExecute,
  onClose,
  onAdded,
}: {
  catalog: McpCatalogEntry[];
  protocolVersion: string;
  controllerExecute: (action: McpManagerAction, key?: string) => Promise<McpManagerActionResult>;
  onClose: () => void;
  onAdded: (id: string | null) => void;
}) {
  const [mode, setMode] = useState<WizardMode | null>(null);
  const [catalogQuery, setCatalogQuery] = useState("");
  const [catalogEntry, setCatalogEntry] = useState<McpCatalogEntry | null>(null);
  const [commandConsent, setCommandConsent] = useState(false);
  const [manualName, setManualName] = useState("");
  const [manualNamespace, setManualNamespace] = useState("");
  const [manualDescription, setManualDescription] = useState("");
  const [manualTransport, setManualTransport] = useState<ManualTransport>("stdio");
  const [manualExecutable, setManualExecutable] = useState("");
  const [manualArguments, setManualArguments] = useState("");
  const [manualWorkingDirectory, setManualWorkingDirectory] = useState("");
  const [manualEndpoint, setManualEndpoint] = useState("");
  const [manualStateless, setManualStateless] = useState(true);
  const [importSource, setImportSource] = useState<"claude_desktop" | "opencode" | "generic">("opencode");
  const [importDocument, setImportDocument] = useState("");
  const [importPreview, setImportPreview] = useState<McpImportPreview | null>(null);
  const [localError, setLocalError] = useState<string | null>(null);
  const [working, setWorking] = useState(false);

  async function addCatalog(): Promise<void> {
    if (!catalogEntry) return;
    if (catalogEntry.install_command && (!commandConsent || !catalogEntry.install_digest)) {
      setLocalError("Review and approve the exact install command first.");
      return;
    }
    setWorking(true);
    setLocalError(null);
    try {
      const result = await controllerExecute({ type: "add_catalog", catalog_id: catalogEntry.id, install_digest: catalogEntry.install_digest ?? undefined });
      onAdded(result.snapshot?.servers.at(-1)?.definition.id ?? null);
    } catch (cause) {
      setLocalError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setWorking(false);
    }
  }

  function manualDefinition(): McpServerDefinition {
    const transport: McpTransport = manualTransport === "stdio"
      ? {
          kind: "stdio",
          executable: manualExecutable.trim(),
          arguments: splitArguments(manualArguments),
          working_directory: manualWorkingDirectory.trim() || null,
          environment: [],
        }
      : manualTransport === "streamable_http"
        ? { kind: "streamable_http", endpoint: manualEndpoint.trim(), stateless: manualStateless, headers: [], oauth: null }
        : { kind: "legacy_sse", endpoint: manualEndpoint.trim(), headers: [], oauth: null };
    return {
      id: makeId(),
      name: manualName.trim(),
      namespace: manualNamespace.trim().toLocaleLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, ""),
      description: manualDescription.trim(),
      source: { kind: "manual" },
      transport,
      supported_protocols: [protocolVersion, "2025-06-18", "2024-11-05"].filter((value, index, all) => all.indexOf(value) === index),
      preferred_protocol: protocolVersion,
      install: null,
      project_scopes: [],
      agent_scopes: [],
      tags: [manualTransport, "manual"],
    };
  }

  async function addManual(): Promise<void> {
    const definition = manualDefinition();
    if (!definition.name || !definition.namespace) {
      setLocalError("Name and namespace are required.");
      return;
    }
    if ((manualTransport === "stdio" && !manualExecutable.trim()) || (manualTransport !== "stdio" && !manualEndpoint.trim())) {
      setLocalError(manualTransport === "stdio" ? "Executable is required." : "Endpoint is required.");
      return;
    }
    setWorking(true);
    setLocalError(null);
    try {
      await controllerExecute({ type: "add_manual", definition });
      onAdded(definition.id);
    } catch (cause) {
      setLocalError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setWorking(false);
    }
  }

  async function previewImport(): Promise<void> {
    if (!importDocument.trim()) {
      setLocalError("Paste an MCP configuration first.");
      return;
    }
    setWorking(true);
    setLocalError(null);
    try {
      const result = await controllerExecute({ type: "preview_import", source: importSource, document: importDocument });
      if (!result.import_preview) throw new Error("Native MCP manager returned no import preview.");
      setImportPreview(result.import_preview);
    } catch (cause) {
      setLocalError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setWorking(false);
    }
  }

  async function acceptImport(): Promise<void> {
    if (!importPreview?.definitions.length) return;
    setWorking(true);
    setLocalError(null);
    try {
      await controllerExecute({ type: "accept_import", definitions: importPreview.definitions });
      onAdded(importPreview.definitions[0]?.id ?? null);
    } catch (cause) {
      setLocalError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setWorking(false);
    }
  }

  const catalogMatches = catalog.filter((entry) =>
    [entry.name, entry.publisher, entry.description, ...entry.tags].join(" ").toLocaleLowerCase().includes(catalogQuery.toLocaleLowerCase()),
  );

  return (
    <div className="mcp-modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <section className="mcp-modal mcp-wizard" role="dialog" aria-modal="true" aria-labelledby="mcp-add-title">
        <header><div><span className="mcp-eyebrow">SECURE CONNECTION WIZARD</span><h2 id="mcp-add-title">Add MCP server</h2><p>{mode ? "Review capabilities and permissions before anything runs." : "Choose the easiest way to connect."}</p></div><button className="mcp-icon-button" type="button" onClick={onClose} aria-label="Close add server wizard">×</button></header>
        <div className="mcp-wizard-body">
          {!mode ? (
            <div className="mcp-source-grid">
              <button type="button" onClick={() => setMode("catalog")}><span aria-hidden="true">✦</span><strong>Browse catalog</strong><p>Verified and community servers with guided setup.</p><em>Recommended</em></button>
              <button type="button" onClick={() => setMode("manual")}><span aria-hidden="true">⌘</span><strong>Connect manually</strong><p>Add a local command or remote Streamable HTTP endpoint.</p></button>
              <button type="button" onClick={() => setMode("import")}><span aria-hidden="true">⇥</span><strong>Import configuration</strong><p>Migrate from OpenCode, Claude Desktop, or generic JSON.</p></button>
            </div>
          ) : null}
          {mode === "catalog" ? (
            <div className="mcp-wizard-pane">
              <WizardBack onClick={() => { setMode(null); setCatalogEntry(null); }} />
              {!catalogEntry ? (
                <>
                  <label className="mcp-search"><span aria-hidden="true">⌕</span><span className="sr-only">Search catalog</span><input value={catalogQuery} onChange={(event) => setCatalogQuery(event.target.value)} placeholder="Search GitHub, Slack, files…" /></label>
                  <div className="mcp-catalog-grid">
                    {catalogMatches.map((entry) => <button key={entry.id} type="button" onClick={() => { setCatalogEntry(entry); setCommandConsent(false); }}><span className="mcp-server-icon" aria-hidden="true">{entry.icon ?? entry.name.slice(0, 1)}</span><span><strong>{entry.name}{entry.verified ? <i title="Verified publisher">✓</i> : null}</strong><small>{entry.publisher}</small><p>{entry.description}</p><em>{entry.transport === "stdio" ? "Local" : "Remote"}</em></span></button>)}
                    {!catalogMatches.length ? <EmptyCatalog title="No matches" text="Try another search or connect manually." /> : null}
                  </div>
                </>
              ) : (
                <div className="mcp-catalog-review">
                  <div className="mcp-review-title"><span className="mcp-server-icon">{catalogEntry.icon ?? catalogEntry.name.slice(0, 1)}</span><div><h3>{catalogEntry.name}{catalogEntry.verified ? <small>✓ Verified</small> : null}</h3><p>{catalogEntry.publisher}</p></div></div>
                  <p>{catalogEntry.description}</p>
                  <ReviewScopes title="Environment access" values={catalogEntry.requested_environment} empty="No environment variables requested" />
                  <ReviewScopes title="Network access" values={catalogEntry.requested_network_origins} empty="No remote origins requested" />
                  {catalogEntry.install_command ? (
                    <div className="mcp-command-consent"><span>Exact install command</span><code>{catalogEntry.install_command}</code><label><input type="checkbox" checked={commandConsent} onChange={(event) => setCommandConsent(event.target.checked)} /><span>I reviewed this exact command and authorize Personal Agent to run it without a shell.</span></label></div>
                  ) : null}
                  <div className="mcp-security-callout"><strong>Credentials are connected after installation</strong><p>OAuth and API keys are stored only in the OS keychain, never in this configuration.</p></div>
                  <div className="mcp-wizard-footer"><button className="mcp-button mcp-button-subtle" type="button" onClick={() => setCatalogEntry(null)}>Back</button><button className="mcp-button mcp-button-primary" type="button" disabled={working || Boolean(catalogEntry.install_command && !commandConsent)} onClick={() => void addCatalog()}>{working ? "Adding…" : "Add server"}</button></div>
                </div>
              )}
            </div>
          ) : null}
          {mode === "manual" ? (
            <div className="mcp-wizard-pane">
              <WizardBack onClick={() => setMode(null)} />
              <div className="mcp-form-grid two">
                <label className="mcp-field"><span>Name *</span><input value={manualName} onChange={(event) => { setManualName(event.target.value); if (!manualNamespace) setManualNamespace(event.target.value.toLocaleLowerCase().replace(/[^a-z0-9]+/g, "_")); }} placeholder="GitHub tools" /></label>
                <label className="mcp-field"><span>Namespace *</span><input value={manualNamespace} onChange={(event) => setManualNamespace(event.target.value)} placeholder="github" /></label>
              </div>
              <label className="mcp-field"><span>Description</span><input value={manualDescription} onChange={(event) => setManualDescription(event.target.value)} placeholder="What this server provides" /></label>
              <fieldset className="mcp-choice-field"><legend>Transport</legend>{(["stdio", "streamable_http", "legacy_sse"] as const).map((transport) => <label key={transport}><input type="radio" name="transport" checked={manualTransport === transport} onChange={() => setManualTransport(transport)} /><span><strong>{transport === "stdio" ? "Local stdio" : transport === "streamable_http" ? "Streamable HTTP" : "Legacy SSE"}</strong><small>{transport === "stdio" ? "Start a local executable" : transport === "streamable_http" ? "Modern remote MCP transport" : "Compatibility for older servers"}</small></span></label>)}</fieldset>
              {manualTransport === "stdio" ? (
                <>
                  <label className="mcp-field"><span>Executable *</span><input aria-label="Executable *" value={manualExecutable} onChange={(event) => setManualExecutable(event.target.value)} placeholder="npx or /absolute/path/server" /><small>Commands are invoked directly; shell expressions are not evaluated.</small></label>
                  <label className="mcp-field"><span>Arguments <small>one argument per line</small></span><textarea aria-label="Arguments" value={manualArguments} onChange={(event) => setManualArguments(event.target.value)} rows={4} placeholder={"-y\n@modelcontextprotocol/server-filesystem\n/home/you/Documents"} /></label>
                  <label className="mcp-field"><span>Working directory</span><input value={manualWorkingDirectory} onChange={(event) => setManualWorkingDirectory(event.target.value)} placeholder="Optional absolute directory" /></label>
                </>
              ) : (
                <>
                  <label className="mcp-field"><span>HTTPS endpoint *</span><input value={manualEndpoint} onChange={(event) => setManualEndpoint(event.target.value)} placeholder="https://mcp.example.com/v1" /><small>Plain HTTP is accepted only for localhost.</small></label>
                  {manualTransport === "streamable_http" ? <label className="mcp-toggle-field"><input type="checkbox" checked={manualStateless} onChange={(event) => setManualStateless(event.target.checked)} /><span>Use stateless mode<small>Recommended for MCP {protocolVersion}</small></span></label> : null}
                </>
              )}
              <div className="mcp-security-callout"><strong>No secret values here</strong><p>Add OAuth or API-key references through the secure native keychain flow after saving this server.</p></div>
              <div className="mcp-wizard-footer"><button className="mcp-button mcp-button-subtle" type="button" onClick={() => setMode(null)}>Back</button><button className="mcp-button mcp-button-primary" type="button" onClick={() => void addManual()} disabled={working}>{working ? "Validating…" : "Validate & add"}</button></div>
            </div>
          ) : null}
          {mode === "import" ? (
            <div className="mcp-wizard-pane">
              <WizardBack onClick={() => { setMode(null); setImportPreview(null); }} />
              {!importPreview ? (
                <>
                  <label className="mcp-field"><span>Import from</span><select value={importSource} onChange={(event) => setImportSource(event.target.value as typeof importSource)}><option value="opencode">OpenCode</option><option value="claude_desktop">Claude Desktop</option><option value="generic">Generic mcpServers JSON</option></select></label>
                  <label className="mcp-field"><span>Configuration JSON</span><textarea className="mcp-code-input" value={importDocument} onChange={(event) => setImportDocument(event.target.value)} rows={13} spellCheck={false} placeholder={'{\n  "mcpServers": { ... }\n}'} /></label>
                  <div className="mcp-security-callout"><strong>Secrets are discarded during preview</strong><p>Credential values in imported files are not persisted or echoed. The wizard asks you to reconnect each one through the OS keychain.</p></div>
                  <div className="mcp-wizard-footer"><button className="mcp-button mcp-button-subtle" type="button" onClick={() => setMode(null)}>Back</button><button className="mcp-button mcp-button-primary" type="button" onClick={() => void previewImport()} disabled={working}>{working ? "Inspecting…" : "Inspect import"}</button></div>
                </>
              ) : (
                <div className="mcp-import-preview">
                  <h3>Import preview</h3>
                  <p>{importPreview.definitions.length} servers are ready for review. Nothing has been installed or connected.</p>
                  <div className="mcp-import-definitions">{importPreview.definitions.map((definition) => <article key={definition.id}><span className="mcp-server-icon">{definition.name.slice(0, 1)}</span><div><strong>{definition.name}</strong><code>{definition.namespace} · {definition.transport.kind}</code><p>{transportIdentity(definition.transport)}</p></div></article>)}</div>
                  {importPreview.issues.length ? <div className="mcp-import-issues"><h4>Needs attention</h4>{importPreview.issues.map((issue, index) => <div key={`${issue.server_name}-${issue.field}-${index}`}><strong>{issue.server_name}: {issue.field}</strong><p>{issue.message}</p></div>)}</div> : <p className="mcp-ok-text">✓ No migration issues found.</p>}
                  <div className="mcp-wizard-footer"><button className="mcp-button mcp-button-subtle" type="button" onClick={() => setImportPreview(null)}>Back</button><button className="mcp-button mcp-button-primary" type="button" onClick={() => void acceptImport()} disabled={working || !importPreview.definitions.length}>{working ? "Importing…" : `Import ${importPreview.definitions.length} server${importPreview.definitions.length === 1 ? "" : "s"}`}</button></div>
                </div>
              )}
            </div>
          ) : null}
          {localError ? <p className="mcp-form-error" role="alert">{localError}</p> : null}
        </div>
      </section>
    </div>
  );
}

function WizardBack({ onClick }: { onClick: () => void }) {
  return <button className="mcp-back-button" type="button" onClick={onClick}>← Choose another method</button>;
}

function ReviewScopes({ title, values, empty }: { title: string; values: string[]; empty: string }) {
  return <section className="mcp-review-scopes"><h4>{title}</h4>{values.length ? <ul>{values.map((value) => <li key={value}>{value}</li>)}</ul> : <p>{empty}</p>}</section>;
}

function ConfirmationDialog({ operation, busy, onClose }: { operation: ConfirmOperation; busy: boolean; onClose: () => void }) {
  const [confirmed, setConfirmed] = useState(false);
  return (
    <div className="mcp-modal-backdrop" role="presentation">
      <section className="mcp-modal mcp-confirm-modal" role="alertdialog" aria-modal="true" aria-labelledby="mcp-confirm-title">
        <header><div><span className="mcp-eyebrow">EXPLICIT CONSENT REQUIRED</span><h2 id="mcp-confirm-title">{operation.title}</h2></div><button className="mcp-icon-button" type="button" onClick={onClose} aria-label="Cancel operation">×</button></header>
        <div className="mcp-confirm-body"><p>{operation.warning}</p><code>{operation.displayText}</code><label className="mcp-checkbox-consent"><input type="checkbox" checked={confirmed} onChange={(event) => setConfirmed(event.target.checked)} /><span>I reviewed the exact operation above and authorize it.</span></label></div>
        <footer><button className="mcp-button mcp-button-subtle" type="button" onClick={onClose}>Cancel</button><button className="mcp-button mcp-button-danger" type="button" disabled={!confirmed || busy} onClick={() => void operation.run()}>{busy ? "Working…" : operation.confirmLabel}</button></footer>
      </section>
    </div>
  );
}

function ExportDialog({ value, onClose }: { value: string; onClose: () => void }) {
  const [copied, setCopied] = useState(false);
  async function copy(): Promise<void> {
    await navigator.clipboard.writeText(value);
    setCopied(true);
  }
  return (
    <div className="mcp-modal-backdrop" role="presentation">
      <section className="mcp-modal mcp-export-modal" role="dialog" aria-modal="true" aria-labelledby="mcp-export-title">
        <header><div><span className="mcp-eyebrow">SECRET-FREE EXPORT</span><h2 id="mcp-export-title">Export MCP configuration</h2><p>Contains server definitions, keychain reference IDs, and permissions—never credential values.</p></div><button className="mcp-icon-button" type="button" onClick={onClose} aria-label="Close export">×</button></header>
        <textarea className="mcp-code-input" value={value} readOnly rows={18} aria-label="Exported configuration" />
        <footer><button className="mcp-button mcp-button-subtle" type="button" onClick={onClose}>Close</button><button className="mcp-button mcp-button-primary" type="button" onClick={() => void copy()}>{copied ? "Copied" : "Copy JSON"}</button></footer>
      </section>
    </div>
  );
}
