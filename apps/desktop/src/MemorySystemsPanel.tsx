import { useState, type FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";

type Item = Record<string, unknown>;
type ProjectData = { nodes?: Item[]; relations?: Item[] };

export function MemorySystemsPanel({
  memories,
  styles,
  projects,
  onChanged,
}: {
  memories: Item[];
  styles: Item[];
  projects: ProjectData;
  onChanged: () => Promise<void>;
}) {
  const [tab, setTab] = useState<"style" | "projects" | "conflicts">("style");
  const [description, setDescription] = useState("");
  const [examples, setExamples] = useState("");
  const [project, setProject] = useState("default");
  const [kind, setKind] = useState("repository");
  const [name, setName] = useState("");
  const [attributes, setAttributes] = useState("");
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [relation, setRelation] = useState("uses");
  const [left, setLeft] = useState("");
  const [right, setRight] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const nodes = projects.nodes ?? [];
  const relations = projects.relations ?? [];

  const perform = async (action: string, payload: Item) => {
    setBusy(true);
    setError("");
    try {
      await invoke("domain_action", { domain: "memory", action, payload });
      await onChanged();
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy(false);
    }
  };

  const saveStyle = async (event: FormEvent) => {
    event.preventDefault();
    await perform("style_create", {
      description,
      examples: examples.split("\n").map((value) => value.trim()).filter(Boolean),
    });
    setDescription("");
    setExamples("");
  };

  const saveNode = async (event: FormEvent) => {
    event.preventDefault();
    let parsed: Item = {};
    try {
      if (attributes.trim()) parsed = JSON.parse(attributes) as Item;
    } catch {
      setError("Attributes must be a JSON object.");
      return;
    }
    await perform("project_node_create", {
      project,
      kind,
      name,
      attributes: parsed,
    });
    setName("");
    setAttributes("");
  };

  return (
    <section className="memory-systems-panel">
      <header>
        <div><span>LONG-TERM CONTEXT</span><h3>Style, project graph and conflicts</h3></div>
        <nav aria-label="Memory system sections">
          <button className={tab === "style" ? "active" : ""} onClick={() => setTab("style")}>Writing style</button>
          <button className={tab === "projects" ? "active" : ""} onClick={() => setTab("projects")}>Projects</button>
          <button className={tab === "conflicts" ? "active" : ""} onClick={() => setTab("conflicts")}>Conflicts</button>
        </nav>
      </header>
      {error && <p className="field-error">{error}</p>}
      {tab === "style" && (
        <div className="memory-system-layout">
          <form onSubmit={(event) => void saveStyle(event)}>
            <label>Preference<input required value={description} onChange={(event) => setDescription(event.target.value)} placeholder="Use concise headings and direct language" /></label>
            <label>Examples · one per line<textarea value={examples} onChange={(event) => setExamples(event.target.value)} rows={3} /></label>
            <button className="primary" disabled={busy}>Save reviewed preference</button>
          </form>
          <div className="memory-system-records">
            {styles.map((style) => <article key={String(style.id)}><strong>{String(style.description)}</strong><small>{style.reviewed ? "reviewed" : "awaiting review"} · confidence {Number(style.confidence ?? 0).toFixed(2)}</small></article>)}
            {!styles.length && <p>No writing-style preferences saved.</p>}
          </div>
        </div>
      )}
      {tab === "projects" && (
        <div className="memory-system-layout">
          <form onSubmit={(event) => void saveNode(event)}>
            <label>Project scope<input required value={project} onChange={(event) => setProject(event.target.value)} /></label>
            <label>Node kind<select value={kind} onChange={(event) => setKind(event.target.value)}><option>repository</option><option>service</option><option>person</option><option>document</option><option>tool</option></select></label>
            <label>Name<input required value={name} onChange={(event) => setName(event.target.value)} /></label>
            <label>Attributes · JSON<textarea value={attributes} onChange={(event) => setAttributes(event.target.value)} rows={3} placeholder='{"path":"/workspace/project"}' /></label>
            <button className="primary" disabled={busy}>Add project node</button>
          </form>
          <div className="memory-system-records project-graph-list">
            {nodes.map((node) => <article key={String(node.id)}><strong>{String(node.name)}</strong><small>{String(node.kind)} · {String((node.namespace as Item | undefined)?.id ?? "project")}</small><code>{String(node.id)}</code></article>)}
            {!nodes.length && <p>No project graph nodes saved.</p>}
            {nodes.length >= 2 && (
              <form className="relation-form" onSubmit={(event) => { event.preventDefault(); void perform("project_relation_create", { from, relation, to }); }}>
                <select required value={from} onChange={(event) => setFrom(event.target.value)}><option value="">From…</option>{nodes.map((node) => <option key={String(node.id)} value={String(node.id)}>{String(node.name)}</option>)}</select>
                <input required value={relation} onChange={(event) => setRelation(event.target.value)} />
                <select required value={to} onChange={(event) => setTo(event.target.value)}><option value="">To…</option>{nodes.map((node) => <option key={String(node.id)} value={String(node.id)}>{String(node.name)}</option>)}</select>
                <button disabled={busy}>Link</button>
              </form>
            )}
            {relations.map((item, index) => <small key={`${String(item.from)}-${index}`}>{String(item.from)} —{String(item.relation)}→ {String(item.to)}</small>)}
          </div>
        </div>
      )}
      {tab === "conflicts" && (
        <div className="memory-conflict-editor">
          <p>Link facts that disagree. Both remain visible until you resolve them.</p>
          <select value={left} onChange={(event) => setLeft(event.target.value)}><option value="">First fact…</option>{memories.map((memory) => <option key={String(memory.id)} value={String(memory.id)}>{String(memory.content)}</option>)}</select>
          <select value={right} onChange={(event) => setRight(event.target.value)}><option value="">Conflicting fact…</option>{memories.map((memory) => <option key={String(memory.id)} value={String(memory.id)}>{String(memory.content)}</option>)}</select>
          <button className="primary" disabled={busy || !left || !right || left === right} onClick={() => void perform("link_conflict", { left, right })}>Link conflict</button>
        </div>
      )}
    </section>
  );
}
