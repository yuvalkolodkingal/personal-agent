import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { StatusPill } from "@personal-agent/ui";

const navigation = [
  "Chat", "Goals & tasks", "Browser", "Projects & terminal", "Artifacts", "History",
  "Memory", "Automations", "Integrations", "Skills & agents", "Usage & egress", "Diagnostics", "Settings",
] as const;

type Diagnostic = {
  product: string; version: string; platform: string; arch: string;
  opencode: { pinned: string; topology: string };
  capabilities: Array<{ id: string; backend: string; status: { state: string } | string }>;
};

const fallback: Diagnostic = {
  product: "Personal Agent", version: "0.1.0", platform: "development", arch: "local",
  opencode: { pinned: "1.18.23", topology: "authenticated-loopback-sidecar" }, capabilities: [],
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

export function App() {
  const [active, setActive] = useState<(typeof navigation)[number]>("Chat");
  const [diagnostic, setDiagnostic] = useState<Diagnostic>(fallback);
  const [listening, setListening] = useState(false);
  const [message, setMessage] = useState("");

  useEffect(() => { invoke<Diagnostic>("diagnostics").then(setDiagnostic).catch(() => setDiagnostic(fallback)); }, []);

  return <div className="app-shell">
    <aside className="sidebar" aria-label="Workspace navigation">
      <div className="brand">
        <div className="brand-mark"><span /><i /></div>
        <div><strong>PERSONAL AGENT</strong><small>JARVIS · BOUNDED</small></div>
      </div>
      <nav>
        {navigation.map((item) => <button key={item} className={active === item ? "active" : ""} onClick={() => setActive(item)} aria-current={active === item ? "page" : undefined}>
          <Icon name={item} /><span>{item}</span>{item === "Goals & tasks" && <b>3</b>}{item === "Integrations" && <em />}
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
          <button className="command"><kbd>⌘</kbd><kbd>K</kbd></button>
        </div>
      </header>

      <section className="content" aria-live="polite">
        <div className="hero-grid">
          <article className={`reactor-card ${listening ? "is-listening" : ""}`}>
            <div className="card-label"><span>VOICE LINK</span><StatusPill tone={listening ? "good" : "neutral"}>{listening ? "LISTENING" : "WAKE-ONLY"}</StatusPill></div>
            <div className="reactor" role="img" aria-label={listening ? "Microphone is listening" : "Microphone is in wake-only mode"}>
              <div className="orbit orbit-a" /><div className="orbit orbit-b" /><div className="orbit orbit-c" />
              <div className="core-ring"><div className="core"><span /></div></div>
              <i className="tick t1"/><i className="tick t2"/><i className="tick t3"/><i className="tick t4"/>
            </div>
            <div className="voice-copy"><h2>{listening ? "I'm listening." : "Say “Hey JARVIS”"}</h2><p>{listening ? "Your microphone is active. Press stop when you're done." : "or use the global hotkey to start a private voice turn."}</p></div>
            <button className="listen-button" aria-label={listening ? "Stop listening" : "Push to talk"} onClick={() => setListening((value) => !value)}>{listening ? "Stop listening" : "Push to talk"}<kbd>Space</kbd></button>
          </article>

          <div className="right-stack">
            <article className="status-card">
              <div className="card-label"><span>SYSTEM STATUS</span><StatusPill tone="good">ALL SYSTEMS NOMINAL</StatusPill></div>
              <div className="metrics">
                <div><span className="metric-icon">◈</span><p><strong>Agent runtime</strong><small>Authenticated sidecar</small></p><b>READY</b></div>
                <div><span className="metric-icon">⌁</span><p><strong>Local voice</strong><small>Offline path available</small></p><b>LOCAL</b></div>
                <div><span className="metric-icon">⬡</span><p><strong>Policy gateway</strong><small>Bounded autonomy</small></p><b>ENFORCED</b></div>
              </div>
            </article>
            <article className="goal-card">
              <div className="card-label"><span>ACTIVE GOAL</span><button>VIEW GRAPH →</button></div>
              <h3>Prepare the weekly project briefing</h3>
              <div className="progress-track"><span /></div>
              <div className="goal-meta"><span>3 of 5 tasks verified</span><span>2 agents running</span><strong>60%</strong></div>
              <div className="agent-row"><i>PL</i><i>EX</i><i>RV</i><p><strong>Reviewer</strong><small>Checking sources and claims…</small></p><button aria-label="Pause goal">Ⅱ</button><button aria-label="Stop goal">■</button></div>
            </article>
          </div>
        </div>

        <div className="lower-grid">
          <article className="conversation-card">
            <div className="card-label"><span>CONVERSATION</span><button>HISTORY</button></div>
            <div className="message assistant"><div className="avatar">J</div><div><small>JARVIS · 09:41</small><p>Morning. The runtime is healthy, your approval queue is clear, and the briefing goal is moving. What shall we tackle?</p></div></div>
            <div className="suggestions"><button>Show task progress</button><button>Open diagnostics</button><button>Start a new goal</button></div>
            <form onSubmit={(event) => { event.preventDefault(); setMessage(""); }}>
              <button type="button" aria-label="Attach artifact">＋</button><input value={message} onChange={(event) => setMessage(event.target.value)} placeholder="Message JARVIS…" aria-label="Message JARVIS"/><span>Silent typed turn</span><button className="send" aria-label="Send message">↑</button>
            </form>
          </article>

          <article className="activity-card">
            <div className="card-label"><span>LIVE ACTIVITY</span><StatusPill tone="good">2 AGENTS</StatusPill></div>
            <ol>
              <li className="done"><i>✓</i><div><strong>Sources collected</strong><span>Research agent · 12 sources</span></div><time>09:39</time></li>
              <li className="running"><i>↻</i><div><strong>Comparing project changes</strong><span>Executor · local workspace</span><div className="mini-progress"><span /></div></div><time>NOW</time></li>
              <li><i>◇</i><div><strong>Verify briefing</strong><span>Reviewer · waiting on task 2</span></div><time>QUEUED</time></li>
            </ol>
            <button className="activity-footer">Open goals and tasks <span>→</span></button>
          </article>
        </div>
      </section>
      <footer><span><i className="online" /> CORE ONLINE</span><span>MICROPHONE · {listening ? "ACTIVE" : "WAKE-ONLY"}</span><span>PRIVATE MODE</span><span className="footer-right">{diagnostic.platform.toUpperCase()} · {diagnostic.arch.toUpperCase()} <b>v{diagnostic.version}</b></span></footer>
    </main>
  </div>;
}
