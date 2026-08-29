import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./usage-egress.css";

type Tokens = {
  input: number;
  output: number;
  reasoning: number;
  cache_read: number;
  cache_write: number;
  total: number;
};

type UsageRecord = {
  id: string;
  at: string;
  day_utc: string;
  session_id: string;
  turn_id: string;
  scope_key: string;
  provider_id?: string | null;
  model_id?: string | null;
  tokens: Tokens;
  cost: { microusd?: number | null; status: "provider_reported" | "unknown" };
};

type EgressRecord = {
  id: string;
  at: string;
  source: "web" | "mcp" | "connector";
  destination: string;
  operation: string;
  data_class: string;
  size_bytes?: number | null;
  purpose: string;
  session_id?: string | null;
  scope_key?: string | null;
};

type Aggregate = {
  provider_steps: number;
  tokens: Tokens;
  reported_cost_microusd: number;
  unknown_cost_steps: number;
  tool_calls: number;
  egress_events: number;
  known_egress_bytes: number;
  unknown_egress_sizes: number;
  providers: string[];
  models: string[];
};

export type UsageSnapshot = {
  records: UsageRecord[];
  egress: EgressRecord[];
  turns: Record<string, Aggregate>;
  sessions: Record<string, Aggregate>;
  days: Record<string, Aggregate>;
  scopes: Record<string, Aggregate>;
  pricing_policy: string;
};

type Filter = {
  from_day: string;
  to_day: string;
  provider: string;
  model: string;
  session: string;
  source: string;
};

const empty: UsageSnapshot = {
  records: [],
  egress: [],
  turns: {},
  sessions: {},
  days: {},
  scopes: {},
  pricing_policy: "Only provider-reported cost is totaled.",
};

const emptyFilter: Filter = {
  from_day: "",
  to_day: "",
  provider: "",
  model: "",
  session: "",
  source: "",
};

export function UsageEgress() {
  const [snapshot, setSnapshot] = useState(empty);
  const [filter, setFilter] = useState(emptyFilter);
  const [tab, setTab] = useState<"usage" | "egress">("usage");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void invoke<UsageSnapshot>("usage_snapshot")
      .then((value) => setSnapshot({ ...empty, ...value }))
      .catch((caught) => setError(String(caught)));
  }, []);

  const usage = useMemo(
    () => snapshot.records.filter((record) => usageMatches(record, filter)),
    [snapshot.records, filter],
  );
  const egress = useMemo(
    () => snapshot.egress.filter((record) => egressMatches(record, filter)),
    [snapshot.egress, filter],
  );
  const metrics = useMemo(() => {
    const tokens = usage.reduce((total, record) => total + record.tokens.total, 0);
    const cost = usage.reduce(
      (total, record) => total + (record.cost.microusd ?? 0),
      0,
    );
    const unknownCost = usage.filter((record) => record.cost.status === "unknown").length;
    const knownBytes = egress.reduce(
      (total, record) => total + (record.size_bytes ?? 0),
      0,
    );
    const unknownSizes = egress.filter((record) => record.size_bytes == null).length;
    return { tokens, cost, unknownCost, knownBytes, unknownSizes };
  }, [usage, egress]);

  const update = (key: keyof Filter, value: string) =>
    setFilter((current) => ({ ...current, [key]: value }));

  const exportFiltered = async () => {
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const result = await invoke<{
        path: string;
        usage_records: number;
        egress_records: number;
      }>("usage_export", { filter });
      setNotice(
        `Exported ${result.usage_records} usage and ${result.egress_records} egress records to ${result.path}`,
      );
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="usage-workspace">
      <header className="usage-hero">
        <div>
          <span className="eyebrow">LOCAL ACCOUNTING</span>
          <h2>Usage &amp; egress</h2>
          <p>Encrypted provider counters and content-free outbound transfer records.</p>
        </div>
        <button className="primary" disabled={busy} onClick={() => void exportFiltered()}>
          {busy ? "Exporting…" : "Export filtered JSON"}
        </button>
      </header>

      <div className="usage-policy"><b>Pricing truth</b><span>{snapshot.pricing_policy}</span></div>
      {error && <div className="usage-alert error">{error}</div>}
      {notice && <div className="usage-alert success">{notice}</div>}

      <div className="usage-metrics">
        <Metric label="provider tokens" value={formatNumber(metrics.tokens)} />
        <Metric
          label="reported cost"
          value={formatCost(metrics.cost, metrics.unknownCost)}
          detail={metrics.unknownCost ? `${metrics.unknownCost} step(s) unknown` : "complete"}
        />
        <Metric label="egress events" value={formatNumber(egress.length)} />
        <Metric
          label="known outbound size"
          value={formatBytes(metrics.knownBytes)}
          detail={metrics.unknownSizes ? `${metrics.unknownSizes} unknown size` : "all sizes known"}
        />
      </div>

      <div className="usage-filters" aria-label="Usage filters">
        <label>From<input type="date" value={filter.from_day} onChange={(event) => update("from_day", event.target.value)} /></label>
        <label>To<input type="date" value={filter.to_day} onChange={(event) => update("to_day", event.target.value)} /></label>
        <label>Provider<input placeholder="All providers" value={filter.provider} onChange={(event) => update("provider", event.target.value)} /></label>
        <label>Model<input placeholder="All models" value={filter.model} onChange={(event) => update("model", event.target.value)} /></label>
        <label>Session<input placeholder="All sessions" value={filter.session} onChange={(event) => update("session", event.target.value)} /></label>
        <label>Source<select value={filter.source} onChange={(event) => update("source", event.target.value)}><option value="">All sources</option><option value="web">Web</option><option value="mcp">MCP</option><option value="connector">Connectors</option></select></label>
        <button onClick={() => setFilter(emptyFilter)}>Clear</button>
      </div>

      <nav className="usage-tabs" aria-label="Accounting view">
        <button className={tab === "usage" ? "active" : ""} onClick={() => setTab("usage")}>Provider usage <b>{usage.length}</b></button>
        <button className={tab === "egress" ? "active" : ""} onClick={() => setTab("egress")}>Outbound data <b>{egress.length}</b></button>
      </nav>

      {tab === "usage" ? <UsageTable records={usage} /> : <EgressTable records={egress} />}
    </section>
  );
}

function Metric({ label, value, detail }: { label: string; value: string; detail?: string }) {
  return <article><span>{label}</span><b>{value}</b>{detail && <small>{detail}</small>}</article>;
}

function UsageTable({ records }: { records: UsageRecord[] }) {
  if (!records.length) return <Empty text="No provider usage matches these filters." />;
  return <div className="usage-table" role="table" aria-label="Provider usage">
    <header role="row"><span>Time / scope</span><span>Provider / model</span><span>Tokens</span><span>Cost</span></header>
    {[...records].reverse().map((record) => <article role="row" key={record.id}>
      <span><b>{new Date(record.at).toLocaleString()}</b><small>{record.scope_key} · {shortId(record.session_id)}</small></span>
      <span><b>{record.provider_id ?? "Unknown provider"}</b><small>{record.model_id ?? "Model not reported"}</small></span>
      <span><b>{formatNumber(record.tokens.total)}</b><small>in {record.tokens.input} · out {record.tokens.output} · reasoning {record.tokens.reasoning}</small></span>
      <span className={record.cost.status === "unknown" ? "unknown" : "known"}><b>{record.cost.microusd == null ? "Unknown" : formatUsd(record.cost.microusd)}</b><small>{record.cost.status.replaceAll("_", " ")}</small></span>
    </article>)}
  </div>;
}

function EgressTable({ records }: { records: EgressRecord[] }) {
  if (!records.length) return <Empty text="No outbound transfers match these filters." />;
  return <div className="usage-table egress-table" role="table" aria-label="Outbound data">
    <header role="row"><span>Time / source</span><span>Destination</span><span>Operation / purpose</span><span>Size</span></header>
    {[...records].reverse().map((record) => <article role="row" key={record.id}>
      <span><b>{new Date(record.at).toLocaleString()}</b><small>{record.source}</small></span>
      <span><b>{record.destination}</b><small>{record.data_class}</small></span>
      <span><b>{record.operation}</b><small>{record.purpose}</small></span>
      <span className={record.size_bytes == null ? "unknown" : "known"}><b>{record.size_bytes == null ? "Unknown" : formatBytes(record.size_bytes)}</b><small>content not stored</small></span>
    </article>)}
  </div>;
}

function Empty({ text }: { text: string }) {
  return <div className="usage-empty"><b>No records</b><span>{text}</span></div>;
}

function usageMatches(record: UsageRecord, filter: Filter) {
  return dayMatches(record.day_utc, filter) && includes(record.provider_id, filter.provider)
    && includes(record.model_id, filter.model) && includes(record.session_id, filter.session);
}

function egressMatches(record: EgressRecord, filter: Filter) {
  const day = record.at.slice(0, 10);
  return dayMatches(day, filter) && includes(record.session_id, filter.session)
    && (!filter.source || record.source === filter.source);
}

function dayMatches(day: string, filter: Filter) {
  return (!filter.from_day || day >= filter.from_day) && (!filter.to_day || day <= filter.to_day);
}

function includes(value: string | null | undefined, needle: string) {
  return !needle.trim() || (value ?? "").toLowerCase().includes(needle.trim().toLowerCase());
}

function formatNumber(value: number) { return new Intl.NumberFormat().format(value); }
function formatUsd(microusd: number) { return `$${(microusd / 1_000_000).toFixed(6)}`; }
function formatCost(microusd: number, unknown: number) {
  if (!microusd && unknown) return "Unknown";
  return `${formatUsd(microusd)}${unknown ? " + unknown" : ""}`;
}
function formatBytes(value: number) {
  if (value < 1024) return `${value} B`;
  if (value < 1024 ** 2) return `${(value / 1024).toFixed(1)} KiB`;
  return `${(value / 1024 ** 2).toFixed(1)} MiB`;
}
function shortId(value: string) { return value.length > 20 ? `${value.slice(0, 10)}…${value.slice(-6)}` : value; }
