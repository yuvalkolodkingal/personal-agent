import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { StatusPill, direction, translate, type Locale } from "@personal-agent/ui";

const navigation = [
  "Chat", "Goals & tasks", "Browser", "Projects & terminal", "Artifacts", "History",
  "Memory", "Automations", "Integrations", "Skills & agents", "Usage & egress", "Diagnostics", "Settings",
] as const;

type Diagnostic = {
  product: string; version: string; platform: string; arch: string;
  opencode: { pinned: string; topology: string };
  capabilities: Array<{ id: string; backend: string; status: { state: string } | string }>;
};

type Projection = {
  last_sequence: number; active_profile: string; active_session: string | null;
  goals_total: number; tasks_running: number; approvals_waiting: number;
  microphone_active: boolean; runtime_healthy: boolean;
  unclean_shutdowns: number; recovered_unclean_run: boolean;
  recent_events?: Array<{ sequence: number; event_type: string; origin: string }>;
};

type MigrationInput = {
  kind: string; path: string; bytes: number; entries: number;
  contains_possible_secrets: boolean; action: string;
};

type MigrationPlan = {
  source_fingerprint: string; inputs: MigrationInput[]; requires_confirmation: boolean;
  remote_devices_require_repairing: boolean; plaintext_secrets_will_be_skipped: boolean;
};

type MigrationReview = { review_token: string; plan: MigrationPlan };
type MigrationSummary = { imported: number; already_present: number; skipped: number; invalid: number; secrets_skipped: number };
type MigrationImportResult = {
  report: { run_id: string; summary: MigrationSummary };
  projection: Projection; json_report_path: string; markdown_report_path: string;
};

const fallback: Diagnostic = {
  product: "Personal Agent", version: "0.1.0", platform: "development", arch: "local",
  opencode: { pinned: "1.18.23", topology: "authenticated-loopback-sidecar" }, capabilities: [],
};

const emptyProjection: Projection = {
  last_sequence: 0, active_profile: "default", active_session: null, goals_total: 0,
  tasks_running: 0, approvals_waiting: 0, microphone_active: false, runtime_healthy: false,
  unclean_shutdowns: 0, recovered_unclean_run: false,
};

const Icon = ({ name }: { name: string }) => {
  const path: Record<string, string> = {
    "Chat": "M4 5h16v11H8l-4 4V5Z", "Goals & tasks": "m5 12 4 4L19 6", Browser: "M3 5h18v14H3V5Zm0 4h18",
    "Projects & terminal": "M4 5h7l2 2h7v12H4V5Zm3 6 2 2-2 2m4 0h4", Artifacts: "M5 3h10l4 4v14H5V3Zm10 0v5h5",
    History: "M4 12a8 8 0 1 0 2-5.3L4 9m0-5v5h5m3-3v6l4 2", Memory: "M8 5a4 4 0 0 1 8 0v1a4 4 0 0 1 2 7.5V17a3 3 0 0 1-3 3H9a3 3 0 0 1-3-3v-3.5A4 4 0 0 1 8 6V5Z",
    Automations: "M12 3v3m0 12v3M3 12h3m12 0h3M5.6 5.6l2.1 2.1m8.6 8.6 2.1 2.1m0-12.8-2.1 2.1m-8.6 8.6-2.1 2.1M12 9a3 3 0 1 1 0 6 3 3 0 0 1 0-6Z",
  };
  return <svg viewBox="0 0 24 24" aria-hidden="true"><path d={path[name] ?? "M5 5h14v14H5z"} /></svg>;
};

const panelCopy: Partial<Record<(typeof navigation)[number], { summary: string; facts: string[]; action: string }>> = {
  "Goals & tasks": { summary: "Durable plans, task dependencies, approvals, checkpoints, and verified completion.", facts: ["Task graphs are acyclic", "Consequential retries require an idempotency key", "User work preempts background agents"], action: "Create goal" },
  Browser: { summary: "Isolated profiles, structured page handles, download quarantine, and visible takeover.", facts: ["Personal profiles require opt-in", "Page instructions are untrusted", "Sensitive submissions always confirm"], action: "Open isolated browser" },
  "Projects & terminal": { summary: "Registered workspace boundaries preserve each project's conversation, terminal, and memory context.", facts: ["Unknown roots start isolated", "Project switching preserves general chat", "Desktop access is never inherited"], action: "Register project" },
  Artifacts: { summary: "Versioned reports, code, media, documents, and whiteboard cards retain their source links.", facts: ["Large content is content-addressed", "HTML is sanitized", "Exports remain terminal-safe"], action: "Create artifact" },
  History: { summary: "Append-only activity reconstructed from the encrypted event stream.", facts: ["Arguments and transcripts follow privacy settings", "Unknown additive events remain visible", "Resume uses monotonic sequence numbers"], action: "Export history" },
  Memory: { summary: "Trusted facts, reviewable inference, conflicts, provenance, expiry, and hybrid local retrieval.", facts: ["Explicit remember requests are trusted", "Inference waits for review", "Recalled text is not re-extracted"], action: "Review proposals" },
  Automations: { summary: "Schedules and monitors survive restart while normal approval and data-zone policy stays in force.", facts: ["Missed runs follow explicit policy", "Approvals suspend rather than fail", "Repeated failures pause the automation"], action: "New automation" },
  Integrations: { summary: "Official connector packs install disabled and use OS-keychain aliases for authorization.", facts: ["OAuth scopes are previewed", "Revocation is explicit", "No connector is enabled by installation"], action: "Browse official packs" },
  "Skills & agents": { summary: "Scoped experts, progressive-disclosure skills, MCP transports, and signed plugin manifests.", facts: ["Agent-written skills enter a proposal queue", "Plugins cannot rewrite core policy", "Renderer code is rejected"], action: "Inspect registry" },
  "Usage & egress": { summary: "Provider usage, cost, connector access, and outbound data are attributed to goals and reasons.", facts: ["Secret values are never recorded", "Budgets apply independently of providers", "Egress includes destination, kind, and size"], action: "Export usage" },
  Diagnostics: { summary: "Platform support and health are reported explicitly; unavailable capabilities never silently no-op.", facts: ["OpenCode version coherence", "Database and migration health", "Audio, browser, provider, and permission guidance"], action: "Create support bundle" },
};

function WorkspacePanel({ active, projection, diagnostic }: { active: (typeof navigation)[number]; projection: Projection; diagnostic: Diagnostic }) {
  const copy = panelCopy[active];
  if (!copy) return null;
  const events = projection.recent_events ?? [];
  return <section className="workspace-panel" aria-labelledby="workspace-panel-title">
    <article className="workspace-overview">
      <div className="card-label"><span>{active.toUpperCase()}</span><StatusPill tone="neutral">BOUNDED</StatusPill></div>
      <div className="workspace-copy"><span className="eyebrow">ENCRYPTED PROFILE · {projection.active_profile.toUpperCase()}</span><h2 id="workspace-panel-title">{active}</h2><p>{copy.summary}</p>
        <ul>{copy.facts.map((fact) => <li key={fact}><span aria-hidden="true">◇</span>{fact}</li>)}</ul>
        <button type="button" disabled title="Available when the corresponding native service is connected">{copy.action}</button>
      </div>
    </article>
    <article className="workspace-detail">
      <div className="card-label"><span>{active === "History" ? "EVENT STREAM" : "CURRENT STATE"}</span><strong>EVENT {projection.last_sequence}</strong></div>
      {active === "History" ? <ol className="event-stream" aria-label="Projected event stream">
        {events.length === 0 && <li className="empty-row"><strong>No events yet</strong><span>New event types will render here by name and origin.</span></li>}
        {events.map((event) => <li key={`${event.sequence}:${event.event_type}`}><b>{event.sequence}</b><div><strong>{event.event_type}</strong><span>{event.origin}</span></div></li>)}
      </ol> : active === "Diagnostics" ? <ul className="capability-list" aria-label="Platform capability matrix">
        <li><strong>Runtime topology</strong><span>{diagnostic.opencode.topology}</span><b>{diagnostic.opencode.pinned}</b></li>
        {diagnostic.capabilities.map((capability) => <li key={capability.id}><strong>{capability.id}</strong><span>{capability.backend}</span><b>{typeof capability.status === "string" ? capability.status : capability.status.state}</b></li>)}
      </ul> : <div className="empty-state"><span aria-hidden="true">◇</span><h3>No active records</h3><p>This screen reports real encrypted state. It will populate when its native service creates records.</p></div>}
    </article>
  </section>;
}

export function App() {
  const [active, setActive] = useState<(typeof navigation)[number]>("Chat");
  const [diagnostic, setDiagnostic] = useState<Diagnostic>(fallback);
  const [projection, setProjection] = useState<Projection>(emptyProjection);
  const [message, setMessage] = useState("");
  const [submitError, setSubmitError] = useState("");
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [autostartError, setAutostartError] = useState("");
  const [migrationConfigRoot, setMigrationConfigRoot] = useState("");
  const [migrationDataRoot, setMigrationDataRoot] = useState("");
  const [migrationAuthPath, setMigrationAuthPath] = useState("");
  const [migrationReview, setMigrationReview] = useState<MigrationReview | null>(null);
  const [migrationConfirmed, setMigrationConfirmed] = useState(false);
  const [migrationBusy, setMigrationBusy] = useState(false);
  const [migrationError, setMigrationError] = useState("");
  const [migrationResult, setMigrationResult] = useState<MigrationImportResult | null>(null);
  const [commandPalette, setCommandPalette] = useState(false);
  const [compactHud, setCompactHud] = useState(false);
  const [theme, setTheme] = useState<"cyan" | "amber" | "mono">("cyan");
  const [locale, setLocale] = useState<Locale>("en-US");

  useEffect(() => {
    invoke<Diagnostic>("diagnostics").then(setDiagnostic).catch(() => setDiagnostic(fallback));
    invoke<boolean>("autostart_status").then(setAutostart).catch((error: unknown) => setAutostartError(String(error)));
    const refresh = () => invoke<Projection>("projection").then(setProjection).catch(() => setProjection(emptyProjection));
    void refresh();
    const interval = window.setInterval(refresh, 1000);
    return () => window.clearInterval(interval);
  }, []);

  useEffect(() => {
    document.documentElement.lang = locale;
    document.documentElement.dir = direction(locale);
  }, [locale]);

  useEffect(() => {
    const handleShortcut = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setCommandPalette((open) => !open);
      }
      if (event.key === "Escape") setCommandPalette(false);
    };
    window.addEventListener("keydown", handleShortcut);
    return () => window.removeEventListener("keydown", handleShortcut);
  }, []);

  const submitMessage = () => {
    const text = message.trim();
    if (!text) return;
    setSubmitError("");
    void invoke<Projection>("submit_message", { text })
      .then((next) => { setProjection(next); setMessage(""); })
      .catch((error: unknown) => setSubmitError(String(error)));
  };

  const toggleAutostart = () => {
    if (autostart === null) return;
    setAutostartError("");
    void invoke<boolean>("set_autostart", { enabled: !autostart })
      .then(setAutostart)
      .catch((error: unknown) => setAutostartError(String(error)));
  };

  const invalidateMigrationReview = () => {
    setMigrationReview(null);
    setMigrationConfirmed(false);
    setMigrationResult(null);
  };

  const reviewMigration = () => {
    if (!migrationConfigRoot.trim() || !migrationDataRoot.trim()) return;
    setMigrationBusy(true);
    setMigrationError("");
    setMigrationResult(null);
    void invoke<MigrationReview>("migration_dry_run", {
      configRoot: migrationConfigRoot.trim(), dataRoot: migrationDataRoot.trim(),
      opencodeAuth: migrationAuthPath.trim() || null,
    }).then((review) => {
      setMigrationReview(review);
      setMigrationConfirmed(false);
    }).catch((error: unknown) => setMigrationError(String(error)))
      .finally(() => setMigrationBusy(false));
  };

  const importMigration = () => {
    if (!migrationReview || !migrationConfirmed) return;
    setMigrationBusy(true);
    setMigrationError("");
    void invoke<MigrationImportResult>("migration_import", {
      reviewToken: migrationReview.review_token, confirmed: true, adoptOpencodeAuth: false,
    }).then((result) => {
      setMigrationResult(result);
      setProjection(result.projection);
      setMigrationReview(null);
      setMigrationConfirmed(false);
    }).catch((error: unknown) => setMigrationError(String(error)))
      .finally(() => setMigrationBusy(false));
  };

  return <div className={`app-shell theme-${theme} ${compactHud ? "compact-hud" : ""}`}>
    <aside className="sidebar" aria-label="Workspace navigation">
      <div className="brand">
        <div className="brand-mark"><span /><i /></div>
        <div><strong>PERSONAL AGENT</strong><small>JARVIS · BOUNDED</small></div>
      </div>
      <nav>
        {navigation.map((item) => <button key={item} className={active === item ? "active" : ""} onClick={() => setActive(item)} aria-current={active === item ? "page" : undefined}>
          <Icon name={item} /><span>{translate(locale, `nav.${item}` as Parameters<typeof translate>[1])}</span>
        </button>)}
      </nav>
      <div className="sidebar-foot">
        <div className="profile-dot">Y</div><div><strong>Default profile</strong><span>Private · Local state</span></div><button aria-label="Profile menu">•••</button>
      </div>
    </aside>

    <main>
      <header className="topbar">
        <div><span className="eyebrow">WORKSPACE / {active.toUpperCase()}</span><h1>{active === "Chat" ? "Good morning, Yuval." : active}</h1></div>
        <div className="selectors">
          <button><span className="provider-mark">O</span> OpenCode <small>{diagnostic.opencode.pinned}</small>⌄</button>
          <button>JARVIS <small>persona</small>⌄</button>
          <button type="button" onClick={() => setCompactHud((compact) => !compact)} aria-pressed={compactHud} aria-label="Toggle compact HUD">HUD</button>
          <button type="button" onClick={() => setTheme((current) => current === "cyan" ? "amber" : current === "amber" ? "mono" : "cyan")} aria-label={`Change theme; current theme ${theme}`}>{theme.toUpperCase()}</button>
          <button type="button" onClick={() => setLocale((current) => current === "en-US" ? "he-IL" : "en-US")} aria-label={translate(locale, "action.language")}>{locale === "en-US" ? "EN" : "עב"}</button>
          <button type="button" className="command" onClick={() => setCommandPalette(true)} aria-label="Open command palette"><kbd>⌘</kbd><kbd>K</kbd></button>
        </div>
      </header>

      {commandPalette && <div className="palette-backdrop" role="presentation" onMouseDown={() => setCommandPalette(false)}><section className="command-palette" role="dialog" aria-modal="true" aria-labelledby="command-palette-title" onMouseDown={(event) => event.stopPropagation()}>
        <div className="card-label"><span id="command-palette-title">COMMAND PALETTE</span><button type="button" onClick={() => setCommandPalette(false)} aria-label="Close command palette">ESC</button></div>
        <p>Navigate without changing permissions or runtime state.</p>
        <div>{navigation.map((item) => <button type="button" key={item} onClick={() => { setActive(item); setCommandPalette(false); }}><Icon name={item}/><span>{translate(locale, `nav.${item}` as Parameters<typeof translate>[1])}</span></button>)}</div>
      </section></div>}

      <section className="content" aria-live="polite">
        {active !== "Chat" && active !== "Settings" && (
          <WorkspacePanel active={active} projection={projection} diagnostic={diagnostic}/>
        )}
        {active === "Settings" && <article className="migration-card" aria-labelledby="migration-title">
          <div className="card-label"><span>LEGACY MIGRATION</span><StatusPill tone={migrationResult ? "good" : "neutral"}>{migrationResult ? "IMPORTED" : "REVIEW REQUIRED"}</StatusPill></div>
          <div className="migration-layout">
            <div className="migration-copy">
              <span className="eyebrow">READ-ONLY SOURCE</span>
              <h2 id="migration-title">Import a Jarvis profile safely</h2>
              <p>A dry run reads file metadata only. Personal content is opened only after you review the plan and confirm. The old profile is never changed.</p>
              <label>Legacy configuration root<input aria-label="Legacy configuration root" value={migrationConfigRoot} placeholder="~/.config/jarvis" onChange={(event) => { setMigrationConfigRoot(event.target.value); invalidateMigrationReview(); }}/></label>
              <label>Legacy data root<input aria-label="Legacy data root" value={migrationDataRoot} placeholder="~/.local/share/jarvis" onChange={(event) => { setMigrationDataRoot(event.target.value); invalidateMigrationReview(); }}/></label>
              <label>OpenCode auth file <small>optional; never copied as normal data</small><input aria-label="OpenCode auth file" value={migrationAuthPath} placeholder="Leave blank" onChange={(event) => { setMigrationAuthPath(event.target.value); invalidateMigrationReview(); }}/></label>
              <button className="review-button" type="button" onClick={reviewMigration} disabled={migrationBusy || !migrationConfigRoot.trim() || !migrationDataRoot.trim()}>{migrationBusy ? "WORKING…" : "RUN METADATA-ONLY DRY RUN"}</button>
            </div>
            <div className="migration-review">
              {!migrationReview && !migrationResult && <div className="migration-empty"><span>◇</span><strong>No reviewed plan</strong><p>Choose the two legacy roots to see exactly what would import, quarantine, or be skipped.</p></div>}
              {migrationReview && <>
                <div className="migration-summary"><strong>{migrationReview.plan.inputs.length} source groups found</strong><span>Fingerprint {migrationReview.plan.source_fingerprint.slice(0, 12)}…</span></div>
                <ul aria-label="Legacy migration plan">{migrationReview.plan.inputs.map((input) => <li key={`${input.kind}:${input.path}`}><div><strong>{input.kind}</strong><span>{input.entries} item{input.entries === 1 ? "" : "s"} · {input.bytes.toLocaleString()} bytes</span></div><b>{input.action.replaceAll("-", " ")}</b>{input.contains_possible_secrets && <em>SECRET-BEARING</em>}</li>)}</ul>
                <p className="migration-safety">Secrets and traces will be skipped. Skills, experts, connectors, schedules, projects, themes, and remote-device metadata stay disabled; devices must pair again.</p>
                <label className="migration-confirm"><input type="checkbox" checked={migrationConfirmed} onChange={(event) => setMigrationConfirmed(event.target.checked)}/><span>I reviewed this plan and consent to copying the listed personal data into my encrypted profile.</span></label>
                <button className="import-button" type="button" onClick={importMigration} disabled={migrationBusy || !migrationConfirmed}>IMPORT REVIEWED DATA</button>
              </>}
              {migrationResult && <div className="migration-complete" role="status"><span>✓</span><h3>Migration run complete</h3><p>{migrationResult.report.summary.imported} imported · {migrationResult.report.summary.already_present} already present · {migrationResult.report.summary.skipped} skipped · {migrationResult.report.summary.invalid} invalid</p><small>Machine report: {migrationResult.json_report_path}</small><small>Human report: {migrationResult.markdown_report_path}</small></div>}
              {migrationError && <p className="migration-error" role="alert">Migration stopped: {migrationError}</p>}
            </div>
          </div>
        </article>}
        {active === "Chat" && <><div className="hero-grid">
          <article className={`reactor-card ${projection.microphone_active ? "is-listening" : ""}`}>
            <div className="card-label"><span>VOICE LINK</span><StatusPill tone={projection.microphone_active ? "good" : "neutral"}>{projection.microphone_active ? "LISTENING" : "NOT CONNECTED"}</StatusPill></div>
            <div className="reactor" role="img" aria-label={projection.microphone_active ? "Microphone is listening" : "Microphone capture is not connected"}>
              <div className="orbit orbit-a" /><div className="orbit orbit-b" /><div className="orbit orbit-c" />
              <div className="core-ring"><div className="core"><span /></div></div>
              <i className="tick t1"/><i className="tick t2"/><i className="tick t3"/><i className="tick t4"/>
            </div>
            <div className="voice-copy"><h2>{projection.microphone_active ? "I'm listening." : "Voice capture is offline."}</h2><p>{projection.microphone_active ? "Your microphone is active." : "No native audio backend has claimed the microphone."}</p></div>
            <button className="listen-button" aria-label="Voice capture unavailable" disabled>Voice unavailable<kbd>Space</kbd></button>
          </article>

          <div className="right-stack">
            <article className="status-card">
              <div className="card-label"><span>SYSTEM STATUS</span><button type="button" onClick={toggleAutostart} disabled={autostart === null} aria-label="Toggle start at login">START AT LOGIN · {autostart === null ? "UNKNOWN" : autostart ? "ON" : "OFF"}</button></div>
              <div className="metrics">
                <div><span className="metric-icon">◈</span><p><strong>Agent runtime</strong><small>Authenticated sidecar</small></p><b>{projection.runtime_healthy ? "READY" : "NOT STARTED"}</b></div>
                <div><span className="metric-icon">⌁</span><p><strong>Local voice</strong><small>Native backend pending</small></p><b>UNAVAILABLE</b></div>
                <div><span className="metric-icon">⬡</span><p><strong>Policy gateway</strong><small>Bounded autonomy</small></p><b>ENFORCED</b></div>
              </div>
              {autostartError && <p className="status-error" role="status">Autostart unavailable: {autostartError}</p>}
            </article>
            <article className="goal-card">
              <div className="card-label"><span>ACTIVE GOAL</span><button>VIEW GRAPH →</button></div>
              <h3>{projection.goals_total ? `${projection.goals_total} recorded goal${projection.goals_total === 1 ? "" : "s"}` : "No active goal"}</h3>
              <div className="progress-track"><span style={{ width: projection.tasks_running ? "18%" : "0%" }} /></div>
              <div className="goal-meta"><span>{projection.tasks_running} tasks running</span><span>{projection.approvals_waiting} approvals waiting</span><strong>EVENT {projection.last_sequence}</strong></div>
              <div className="agent-row"><i>—</i><p><strong>No agent assigned</strong><small>Create a goal after the runtime is connected.</small></p><button aria-label="Pause goal" disabled>Ⅱ</button><button aria-label="Stop goal" disabled>■</button></div>
            </article>
          </div>
        </div>

        <div className="lower-grid">
          <article className="conversation-card">
            <div className="card-label"><span>CONVERSATION</span><button>HISTORY</button></div>
            <div className="message assistant"><div className="avatar">J</div><div><small>JARVIS · LOCAL CORE</small><p>Your encrypted profile is open. Typed messages are persisted as events; model responses remain unavailable until the authenticated runtime completes startup.</p></div></div>
            <div className="suggestions"><button>Show task progress</button><button>Open diagnostics</button><button>Start a new goal</button></div>
            <form onSubmit={(event) => { event.preventDefault(); submitMessage(); }}>
              <button type="button" aria-label="Attach artifact">＋</button><input value={message} onChange={(event) => setMessage(event.target.value)} placeholder="Message JARVIS…" aria-label="Message JARVIS"/><span>Silent typed turn</span><button className="send" aria-label="Send message">↑</button>
            </form>
            {submitError && <p className="inline-error" role="alert">Message was not persisted: {submitError}</p>}
          </article>

          <article className="activity-card">
              <div className="card-label"><span>LIVE ACTIVITY</span><StatusPill tone="neutral">{projection.tasks_running} AGENTS</StatusPill></div>
            <ol>
              <li className="done"><i>✓</i><div><strong>{projection.recovered_unclean_run ? "Recovered after an unclean exit" : "Encrypted profile recovered"}</strong><span>Projection rebuilt through event {projection.last_sequence}; {projection.unclean_shutdowns} unclean exits recorded</span></div><time>LOCAL</time></li>
              <li className={projection.runtime_healthy ? "done" : ""}><i>{projection.runtime_healthy ? "✓" : "◇"}</i><div><strong>Runtime connection</strong><span>{projection.runtime_healthy ? "Authenticated bundled sidecar is healthy" : "Waiting for bundled sidecar bootstrap"}</span></div><time>{projection.runtime_healthy ? "READY" : "WAIT"}</time></li>
            </ol>
            <button className="activity-footer">Open goals and tasks <span>→</span></button>
          </article>
        </div></>}
      </section>
      <footer><span><i className="online" /> {translate(locale, "status.coreOnline").toUpperCase()}</span><span>{translate(locale, "status.microphone", { state: projection.microphone_active ? "ACTIVE" : "OFFLINE" }).toUpperCase()}</span><span>{translate(locale, "status.privateMode").toUpperCase()}</span><span className="footer-right">{diagnostic.platform.toUpperCase()} · {diagnostic.arch.toUpperCase()} <b>v{diagnostic.version}</b></span></footer>
    </main>
  </div>;
}
