import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type StatusValue =
  | { state: "supported" }
  | { state: "degraded" | "unsupported"; reason: string; remediation?: string };
type PermissionValue = {
  state: "granted" | "denied" | "not_determined" | "unavailable";
  guidance?: string;
  reason?: string;
};
type DesktopStatus = {
  connected: boolean;
  connection_detail: string;
  plan: {
    platform: string;
    session: string;
    screen_capture_backend: string;
    accessibility_backend: string;
    input_backend: string;
    capabilities: Array<{ id: string; backend: string; status: StatusValue }>;
  };
  permissions: Record<string, PermissionValue>;
};
type PortalStatus = {
  interfaces: {
    screencast_version?: number;
    remote_desktop_version?: number;
    available_source_types: number;
    available_cursor_modes: number;
  };
  phase: "idle" | "probing" | "creating" | "selecting" | "awaiting_consent" | "active" | "cancelling" | "cancelled" | "failed";
  consent: "required" | "requesting" | "granted" | "cancelled" | "denied" | "unavailable";
  kind?: "screen_cast" | "remote_desktop";
  streams: Array<{ node_id: number; position?: [number, number]; size?: [number, number] }>;
  pipewire_transport: boolean;
  detail: string;
};

function permissionGranted(value: PermissionValue | undefined) {
  return value?.state === "granted";
}

function permissionDetail(value: PermissionValue | undefined) {
  if (!value) return "Permission state unavailable";
  if (value.state === "granted") return "Granted by the operating system";
  return value.guidance ?? value.reason ?? value.state.replaceAll("_", " ");
}

function capabilityDetail(status: StatusValue) {
  if (status.state === "supported") return "Native backend ready";
  return status.remediation ? `${status.reason} · ${status.remediation}` : status.reason;
}
type Handle = {
  window_id: string;
  generation: { epoch: number; sequence: number };
  opaque_id: string;
};
type ContextResponse = {
  snapshot: {
    generation: { epoch: number; sequence: number };
    observed_at_unix_ms: number;
    view: {
      application_id: string;
      application_name: string;
      title: string;
      secure_surface: boolean;
    };
    nodes: Array<{
      handle: Handle;
      role: string;
      name: string;
      value?: string;
      states: string[];
      actions: string[];
    }>;
    backend: string;
    degraded_reasons: string[];
  };
  frame_png_base64?: string;
};

export function ScreenContext() {
  const [status, setStatus] = useState<DesktopStatus | null>(null);
  const [context, setContext] = useState<ContextResponse | null>(null);
  const [capture, setCapture] = useState(false);
  const [busy, setBusy] = useState(false);
  const [portalBusy, setPortalBusy] = useState(false);
  const [portalStatus, setPortalStatus] = useState<PortalStatus | null>(null);
  const [error, setError] = useState("");

  const canObserve = Boolean(
    status?.connected && permissionGranted(status.permissions.accessibility),
  );
  const canCapture = Boolean(
    canObserve && permissionGranted(status?.permissions.screen_capture),
  );
  const canControl = Boolean(
    status?.connected && permissionGranted(status.permissions.input_control),
  );

  useEffect(() => {
    void invoke<DesktopStatus>("desktop_status").then(setStatus).catch((caught) => setError(String(caught)));
    void invoke<PortalStatus>("portal_status").then(setPortalStatus).catch((caught) => setError(String(caught)));
  }, []);

  const connectPortal = async (requestControl: boolean) => {
    setPortalBusy(true);
    setError("");
    setPortalStatus((current) => current ? { ...current, phase: "awaiting_consent", consent: "requesting", detail: "Waiting for the system portal selection" } : current);
    try {
      setPortalStatus(await invoke<PortalStatus>("portal_connect", { requestControl, parentWindow: "" }));
    } catch (caught) {
      setError(String(caught));
      try {
        setPortalStatus(await invoke<PortalStatus>("portal_status"));
      } catch {
        // Keep the primary portal error visible.
      }
    } finally {
      setPortalBusy(false);
    }
  };

  const cancelPortal = async () => {
    setError("");
    try {
      setPortalStatus(await invoke<PortalStatus>("portal_cancel"));
    } catch (caught) {
      setError(String(caught));
    }
  };

  const disconnectPortal = async () => {
    setPortalBusy(true);
    setError("");
    try {
      setPortalStatus(await invoke<PortalStatus>("portal_disconnect"));
    } catch (caught) {
      setError(String(caught));
    } finally {
      setPortalBusy(false);
    }
  };

  const observe = async (withPixels = capture) => {
    setBusy(true);
    setError("");
    try {
      await invoke("desktop_set_capture", { enabled: true, allowFullDisplay: false });
      setContext(await invoke<ContextResponse>("desktop_snapshot", { capturePixels: withPixels }));
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy(false);
    }
  };

  const act = async (
    action: Record<string, unknown>,
    effect: string,
    postconditions: Array<Record<string, unknown>>,
  ) => {
    setBusy(true);
    setError("");
    try {
      await invoke("desktop_execute", {
        request: {
          request_id: crypto.randomUUID(),
          action,
          authorization: {
            user_present: true,
            approved_effects: [effect],
            sensitive_text_approved: effect === "write_text",
          },
          postconditions,
        },
      });
      await observe(false);
    } catch (caught) {
      setError(String(caught));
      setBusy(false);
    }
  };

  return (
    <section className="screen-context-panel">
      <header>
        <div><span>LIVE SCREEN CONTEXT</span><h2>See and control the active view</h2><p>Accessibility comes first. Pixels are captured only for this request and are not persisted.</p></div>
        <div>
          <label title={permissionDetail(status?.permissions.screen_capture)}><input type="checkbox" checked={capture} disabled={!canCapture} onChange={(event) => setCapture(event.target.checked)} /> include pixels</label>
          <button className="primary" title={canObserve ? "Read the active application and accessibility tree" : permissionDetail(status?.permissions.accessibility)} disabled={busy || !canObserve} onClick={() => void observe()}>{busy ? "Reading…" : "Read active view"}</button>
        </div>
      </header>
      {error && <p className="error-banner">{error}</p>}
      {status && (
        <div className="screen-capabilities">
          <article className={status.connected ? "supported" : "unsupported"}><header><strong>{status.connected ? "Bridge connected" : "Bridge unavailable"}</strong><b>{status.connected ? "connected" : "setup"}</b></header><small>{status.connection_detail}</small></article>
          {status.plan.capabilities.map((capability) => (
            <article key={capability.id} className={capability.status.state}>
              <header><strong>{capability.id.replaceAll("desktop.", "").replaceAll("_", " ")}</strong><b>{capability.status.state}</b></header>
              <span>{capability.backend}</span>
              <small>{capabilityDetail(capability.status)}</small>
            </article>
          ))}
        </div>
      )}
      {status && (
        <div className="screen-permission-list" aria-label="Desktop permissions">
          {(["accessibility", "screen_capture", "input_control"] as const).map((permission) => (
            <span key={permission} className={status.permissions[permission]?.state ?? "unavailable"}>
              <b>{permission.replaceAll("_", " ")}</b>
              <small>{permissionDetail(status.permissions[permission])}</small>
            </span>
          ))}
        </div>
      )}
      {portalStatus && (
        <section className={`portal-session-panel ${portalStatus.consent}`} aria-label="Wayland portal session">
          <div>
            <span>WAYLAND PORTAL</span>
            <strong>{portalStatus.phase === "active" ? "User-selected session active" : "Private system selection"}</strong>
            <small>{portalStatus.detail}</small>
            {portalStatus.streams.length > 0 && <small>{portalStatus.streams.length} selected stream{portalStatus.streams.length === 1 ? "" : "s"} · PipeWire frames {portalStatus.pipewire_transport ? "connected" : "not connected"}</small>}
          </div>
          <div>
            {(["creating", "selecting", "awaiting_consent", "cancelling"] as string[]).includes(portalStatus.phase) ? (
              <button onClick={() => void cancelPortal()}>Cancel portal request</button>
            ) : portalStatus.phase === "active" ? (
              <button disabled={portalBusy} onClick={() => void disconnectPortal()}>Disconnect portal</button>
            ) : (
              <>
                <button disabled={portalBusy || !portalStatus.interfaces.screencast_version} onClick={() => void connectPortal(false)}>Share screen via portal</button>
                <button title={portalStatus.interfaces.remote_desktop_version ? "Request pointer control through the system portal" : "RemoteDesktop is not exposed by this portal backend"} disabled={portalBusy || !portalStatus.interfaces.remote_desktop_version} onClick={() => void connectPortal(true)}>Grant screen control</button>
              </>
            )}
          </div>
        </section>
      )}
      {context && (
        <div className="screen-observation">
          <div>
            <span>{context.snapshot.view.application_name}</span>
            <h3>{context.snapshot.view.title || "Untitled window"}</h3>
            <small>{context.snapshot.view.application_id} · generation {context.snapshot.generation.epoch}:{context.snapshot.generation.sequence}</small>
            <div className="context-node-list">
              {context.snapshot.nodes.map((node) => (
                <article key={node.handle.opaque_id}>
                  <div><b>{node.role}</b><strong>{node.name || "Unnamed control"}</strong><small>{node.states.join(" · ")}</small></div>
                  <div>
                    {node.actions.includes("press") && <button title={canControl ? "Press this semantic control" : permissionDetail(status?.permissions.input_control)} disabled={busy || !canControl} onClick={() => void act({ action: "click", target: node.handle, button: "primary", click_count: 1 }, "interact", [{ postcondition: "generation_advanced" }])}>Press</button>}
                    {node.actions.includes("focus") && <button title={canControl ? "Focus this semantic control" : permissionDetail(status?.permissions.input_control)} disabled={busy || !canControl} onClick={() => void act({ action: "focus", target: node.handle }, "navigate", [{ postcondition: "condition", condition: "window_exists", window_id: node.handle.window_id }, { postcondition: "generation_advanced" }])}>Focus</button>}
                    {(node.actions.includes("set_value") || node.actions.includes("replace_selection")) && <button title={canControl ? "Type into this editable semantic control" : permissionDetail(status?.permissions.input_control)} disabled={busy || !canControl} onClick={() => { const text = window.prompt("Text to type"); if (text) void act({ action: "type_text", target: node.handle, text, replace_selection: node.actions.includes("replace_selection") }, "write_text", [{ postcondition: "condition", condition: "node_value_contains", target: { selector: "semantic", window_id: node.handle.window_id, role: node.role, name: node.name }, text }, { postcondition: "generation_advanced" }]); }}>Type</button>}
                  </div>
                </article>
              ))}
            </div>
          </div>
          {context.frame_png_base64 && <img alt="Ephemeral active-window capture" src={`data:image/png;base64,${context.frame_png_base64}`} />}
        </div>
      )}
    </section>
  );
}
