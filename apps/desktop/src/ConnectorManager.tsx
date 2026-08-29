import { useEffect, useState, type FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";

type Connector = {
  id: string;
  display_name: string;
  kind: string;
  base_url: string;
  auth: {
    kind: string;
    keychain_alias?: string;
    account_label?: string;
    client_id?: string;
    scopes?: string[];
    expires_at?: string | null;
  };
  grants: Array<{ resource: string; action: string }>;
  enabled: boolean;
  health?: { state: string; detail: string; checked_at?: string };
};
type GrantAction = "read" | "create" | "update" | "delete" | "send";
type ConnectorResponse = {
  status: number;
  body: unknown;
  next_cursor?: string | null;
  request_id?: string | null;
};
const grantActions: GrantAction[] = ["read", "create", "update", "delete", "send"];

const templates = [
  ["github", "GitHub"],
  ["gmail", "Gmail"],
  ["google_calendar", "Google Calendar"],
  ["slack", "Slack"],
  ["microsoft_graph", "Microsoft 365"],
  ["custom_rest", "Custom REST API"],
] as const;

const oauthScopes: Record<string, string[]> = {
  github: ["read:user", "user:email"],
  gmail: ["https://www.googleapis.com/auth/gmail.readonly"],
  google_calendar: ["https://www.googleapis.com/auth/calendar.readonly"],
};

export function ConnectorManager() {
  const [connectors, setConnectors] = useState<Connector[]>([]);
  const [wizard, setWizard] = useState(false);
  const [kind, setKind] = useState("github");
  const [name, setName] = useState("My GitHub");
  const [baseUrl, setBaseUrl] = useState("");
  const [token, setToken] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  const [editing, setEditing] = useState<string | null>(null);
  const [requesting, setRequesting] = useState<string | null>(null);
  const [customResource, setCustomResource] = useState("");
  const [requestResource, setRequestResource] = useState("");
  const [requestAction, setRequestAction] = useState<GrantAction>("read");
  const [requestMethod, setRequestMethod] = useState("GET");
  const [requestPath, setRequestPath] = useState("");
  const [requestBody, setRequestBody] = useState("");
  const [requestResult, setRequestResult] = useState<ConnectorResponse | null>(null);
  const [oauthConnector, setOauthConnector] = useState<Connector | null>(null);
  const [oauthClientId, setOauthClientId] = useState("");
  const [oauthPending, setOauthPending] = useState(false);
  const [notice, setNotice] = useState("");

  const refresh = async () => {
    try {
      setConnectors(await invoke<Connector[]>("connector_list"));
      setError("");
    } catch (caught) {
      setError(String(caught));
    }
  };
  useEffect(() => void refresh(), []);

  const create = async (event: FormEvent) => {
    event.preventDefault();
    setBusy("create");
    try {
      await invoke("connector_create", {
        kind,
        displayName: name,
        baseUrl: baseUrl || null,
        credential: token || null,
      });
      setToken("");
      setWizard(false);
      await refresh();
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy("");
    }
  };

  const action = async (id: string, operation: string, confirmed = false) => {
    setBusy(id);
    try {
      await invoke("connector_action", { id, operation, confirmed });
      await refresh();
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy("");
    }
  };

  const authorizeOAuth = async () => {
    if (!oauthConnector || !oauthClientId.trim()) return;
    setOauthPending(true);
    setError("");
    setNotice("");
    try {
      const result = await invoke<{ message: string }>("connector_oauth_authorize", {
        id: oauthConnector.id,
        clientId: oauthClientId.trim(),
        scopes: oauthScopes[oauthConnector.kind] ?? [],
      });
      setNotice(result.message);
      setOauthConnector(null);
      await refresh();
    } catch (caught) {
      setError(String(caught));
    } finally {
      setOauthPending(false);
    }
  };

  const cancelOAuth = async () => {
    if (!oauthConnector) return;
    try {
      await invoke("connector_oauth_cancel", { id: oauthConnector.id });
    } catch (caught) {
      setError(String(caught));
    }
  };

  const refreshOAuth = async (connector: Connector) => {
    setBusy(connector.id);
    setError("");
    try {
      const result = await invoke<{ message: string }>("connector_oauth_refresh", {
        id: connector.id,
      });
      setNotice(result.message);
      await refresh();
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy("");
    }
  };

  const revokeOAuth = async (connector: Connector) => {
    if (!window.confirm(`Revoke OAuth access for ${connector.display_name}?`)) return;
    setBusy(connector.id);
    setError("");
    try {
      const result = await invoke<{ message: string }>("connector_oauth_revoke", {
        id: connector.id,
        confirmed: true,
      });
      setNotice(result.message);
      await refresh();
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy("");
    }
  };

  const setGrant = async (
    connector: Connector,
    resource: string,
    grantAction: GrantAction,
  ) => {
    const normalized = resource.trim();
    if (!normalized) return;
    const exists = connector.grants.some(
      (grant) => grant.resource === normalized && grant.action === grantAction,
    );
    if (
      !exists &&
      grantAction !== "read" &&
      !window.confirm(
        `Grant ${grantAction.toUpperCase()} access to ${normalized} for ${connector.display_name}?`,
      )
    )
      return;
    const grants = exists
      ? connector.grants.filter(
          (grant) =>
            grant.resource !== normalized || grant.action !== grantAction,
        )
      : [...connector.grants, { resource: normalized, action: grantAction }];
    setBusy(connector.id);
    try {
      await invoke("connector_set_grants", {
        id: connector.id,
        grants,
        confirmed: grants.some((grant) => grant.action !== "read"),
      });
      setCustomResource("");
      await refresh();
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy("");
    }
  };

  const runRequest = async (connector: Connector) => {
    const grant = connector.grants.find(
      (item) =>
        item.resource === requestResource && item.action === requestAction,
    );
    if (!grant) {
      setError("Choose a resource/action pair granted in Permissions first.");
      return;
    }
    if (
      requestAction !== "read" &&
      !window.confirm(
        `Run this ${requestAction.toUpperCase()} request through ${connector.display_name}?\n\n${requestMethod} ${requestPath}`,
      )
    )
      return;
    setBusy(connector.id);
    setError("");
    try {
      let body: unknown = null;
      if (requestBody.trim()) body = JSON.parse(requestBody);
      setRequestResult(
        await invoke<ConnectorResponse>("connector_execute", {
          id: connector.id,
          resource: requestResource,
          action: requestAction,
          method: requestMethod,
          path: requestPath,
          query: {},
          body,
          idempotencyKey:
            requestAction === "read" ? null : crypto.randomUUID(),
        }),
      );
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy("");
    }
  };

  return (
    <section className="connector-manager">
      <header>
        <div>
          <span>APP CONNECTIONS</span>
          <h2>Connect work apps without copying secrets into chat</h2>
          <p>Each connection starts disabled and read-only. Add write access only when a workflow needs it.</p>
        </div>
        <button className="primary" onClick={() => setWizard(true)}>+ Connect app</button>
      </header>
      {error && <p className="error-banner">{error}</p>}
      {notice && <p className="connector-notice" role="status">{notice}</p>}
      <div className="connector-grid">
        {connectors.map((connector) => (
          <article key={connector.id} className={connector.enabled ? "connected" : ""}>
            <header>
              <i>{connector.display_name.slice(0, 1).toUpperCase()}</i>
              <div><strong>{connector.display_name}</strong><small>{connector.kind.replaceAll("_", " ")}</small></div>
              <b>{connector.health?.state ?? (connector.enabled ? "enabled" : "disabled")}</b>
            </header>
            <p>{connector.health?.detail ?? connector.base_url}</p>
            {connector.auth.kind === "oauth2" && (
              <p className="connector-auth-state">
                <strong>{connector.auth.account_label || "OAuth connected"}</strong>
                <small>{connector.auth.scopes?.join(" · ") || "Reviewed read-only scopes"}</small>
              </p>
            )}
            <div className="scope-chips">
              {connector.grants.map((grant) => <span key={`${grant.resource}:${grant.action}`}>{grant.action} {grant.resource}</span>)}
            </div>
            <footer>
              <button disabled={busy === connector.id} onClick={() => void action(connector.id, "test")}>Test</button>
              <button disabled={busy === connector.id} onClick={() => setEditing(editing === connector.id ? null : connector.id)}>Permissions</button>
              <button disabled={busy === connector.id || !connector.enabled} onClick={() => { setRequestResult(null); setRequesting(requesting === connector.id ? null : connector.id); const first = connector.grants[0]; if (first) { setRequestResource(first.resource); setRequestAction(first.action as GrantAction); } }}>API request</button>
              <button disabled={busy === connector.id} onClick={() => void action(connector.id, connector.enabled ? "disable" : "enable")}>{connector.enabled ? "Disable" : "Enable"}</button>
              {oauthScopes[connector.kind] && connector.auth.kind !== "bearer_token" && (
                <button
                  disabled={busy === connector.id}
                  onClick={() => {
                    setOauthConnector(connector);
                    setOauthClientId(connector.auth.client_id ?? "");
                    setError("");
                  }}
                >
                  {connector.auth.kind === "oauth2" ? "Reconnect OAuth" : "Connect OAuth"}
                </button>
              )}
              {connector.auth.kind === "oauth2" && ["gmail", "google_calendar"].includes(connector.kind) && (
                <>
                  <button disabled={busy === connector.id} onClick={() => void refreshOAuth(connector)}>Refresh OAuth</button>
                </>
              )}
              {connector.auth.kind === "oauth2" && (
                <button className="danger" disabled={busy === connector.id} onClick={() => void revokeOAuth(connector)}>Revoke OAuth</button>
              )}
              <button className="danger" disabled={busy === connector.id} onClick={() => void action(connector.id, "delete", true)}>Remove</button>
            </footer>
            {editing === connector.id && (
              <section className="connector-permissions">
                <header><strong>Explicit capability grants</strong><small>Read is safe by default. Every write grant is confirmed separately.</small></header>
                {[...new Set(connector.grants.map((grant) => grant.resource))].map((resource) => (
                  <div key={resource}>
                    <b>{resource}</b>
                    <span>
                      {grantActions.map((grantAction) => (
                        <button
                          key={grantAction}
                          className={connector.grants.some((grant) => grant.resource === resource && grant.action === grantAction) ? "active" : ""}
                          onClick={() => void setGrant(connector, resource, grantAction)}
                        >
                          {grantAction}
                        </button>
                      ))}
                    </span>
                  </div>
                ))}
                <div className="connector-add-resource">
                  <input value={customResource} onChange={(event) => setCustomResource(event.target.value)} placeholder="Custom resource name" />
                  <button onClick={() => void setGrant(connector, customResource, "read")}>+ Add read grant</button>
                </div>
              </section>
            )}
            {requesting === connector.id && (
              <section className="connector-request-builder">
                <header><strong>Bounded API request</strong><small>Only an already granted resource/action can run.</small></header>
                <div>
                  <select value={`${requestAction}:${requestResource}`} onChange={(event) => { const [nextAction, ...parts] = event.target.value.split(":"); setRequestAction(nextAction as GrantAction); setRequestResource(parts.join(":")); setRequestMethod(nextAction === "read" ? "GET" : "POST"); }}>
                    {connector.grants.map((grant) => <option key={`${grant.action}:${grant.resource}`} value={`${grant.action}:${grant.resource}`}>{grant.action} · {grant.resource}</option>)}
                  </select>
                  <select value={requestMethod} onChange={(event) => setRequestMethod(event.target.value)}><option>GET</option><option>POST</option><option>PATCH</option><option>PUT</option><option>DELETE</option></select>
                  <input value={requestPath} onChange={(event) => setRequestPath(event.target.value)} placeholder="Relative path, e.g. repos/owner/name" />
                </div>
                <textarea value={requestBody} onChange={(event) => setRequestBody(event.target.value)} rows={3} placeholder='Optional JSON body, e.g. {"title":"…"}' />
                <button className="primary" disabled={busy === connector.id || !requestPath.trim()} onClick={() => void runRequest(connector)}>Review & run</button>
                {requestResult && <pre>{JSON.stringify(requestResult, null, 2)}</pre>}
              </section>
            )}
          </article>
        ))}
        {!connectors.length && <div className="empty-state"><strong>No apps connected</strong><p>Choose a reviewed template to get started.</p></div>}
      </div>
      {wizard && (
        <div className="modal-backdrop" role="presentation" onMouseDown={() => setWizard(false)}>
          <form className="connector-wizard" onSubmit={create} onMouseDown={(event) => event.stopPropagation()}>
            <header><div><span>NEW CONNECTION</span><h2>Connect an app</h2></div><button type="button" onClick={() => setWizard(false)}>×</button></header>
            <label>Service<select value={kind} onChange={(event) => { const next = event.target.value; setKind(next); setName(`My ${templates.find(([id]) => id === next)?.[1] ?? "app"}`); }}>{templates.map(([id, label]) => <option value={id} key={id}>{label}</option>)}</select></label>
            <label>Connection name<input required value={name} onChange={(event) => setName(event.target.value)} /></label>
            {kind === "custom_rest" && <label>HTTPS base URL<input required type="url" value={baseUrl} onChange={(event) => setBaseUrl(event.target.value)} placeholder="https://api.example.com/" /></label>}
            <label>Access token <small>Stored immediately in your OS keychain</small><input type="password" autoComplete="off" value={token} onChange={(event) => setToken(event.target.value)} placeholder="Leave blank to connect OAuth later" /></label>
            <aside><strong>Safe initial access</strong><p>Read-only service scopes. Sending, editing, deleting, and external writes require a separate grant and confirmation.</p></aside>
            <footer><button type="button" onClick={() => setWizard(false)}>Cancel</button><button className="primary" disabled={busy === "create"}>{busy === "create" ? "Connecting…" : "Create connection"}</button></footer>
          </form>
        </div>
      )}
      {oauthConnector && (
        <div className="modal-backdrop" role="presentation" onMouseDown={() => { if (!oauthPending) setOauthConnector(null); }}>
          <section className="connector-wizard connector-oauth-dialog" role="dialog" aria-modal="true" aria-labelledby="connector-oauth-title" onMouseDown={(event) => event.stopPropagation()}>
            <header>
              <div><span>SECURE OAUTH</span><h2 id="connector-oauth-title">Connect {oauthConnector.display_name}</h2></div>
              <button type="button" disabled={oauthPending} onClick={() => setOauthConnector(null)}>×</button>
            </header>
            <p>A fresh PKCE-protected browser authorization uses a private loopback callback. Access and refresh tokens go directly to your OS keychain.</p>
            <label>
              Public desktop client ID
              <small>This identifies your OAuth app; it is not a client secret.</small>
              <input autoComplete="off" value={oauthClientId} onChange={(event) => setOauthClientId(event.target.value)} placeholder={oauthConnector.kind === "github" ? "GitHub OAuth App client ID" : "Google Desktop client ID"} disabled={oauthPending} />
            </label>
            <aside>
              <strong>Reviewed read-only scopes</strong>
              <p>{(oauthScopes[oauthConnector.kind] ?? []).join(" · ")}</p>
            </aside>
            {oauthPending && <p role="status">Waiting for the browser authorization…</p>}
            <footer>
              <button type="button" onClick={() => { if (oauthPending) void cancelOAuth(); else setOauthConnector(null); }}>{oauthPending ? "Cancel authorization" : "Close"}</button>
              <button type="button" className="primary" disabled={oauthPending || !oauthClientId.trim()} onClick={() => void authorizeOAuth()}>{oauthPending ? "Waiting…" : "Open secure sign-in"}</button>
            </footer>
          </section>
        </div>
      )}
    </section>
  );
}
