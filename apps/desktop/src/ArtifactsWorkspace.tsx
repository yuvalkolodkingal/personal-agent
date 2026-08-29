import { useEffect, useMemo, useState, type FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";

type SourceLink = { label: string; uri: string; content_hash?: string | null };
type ArtifactVersion = {
  version: number;
  content_sha256: string;
  media_type: string;
  byte_length: number;
  source_links: SourceLink[];
};
type Artifact = {
  id: string;
  title: string;
  kind: string;
  versions: ArtifactVersion[];
};
type WhiteboardCard = { id: string; artifact_id: string; pinned: boolean };
type Snapshot = {
  artifacts: Artifact[];
  cards: WhiteboardCard[];
  order: string[];
  focused?: string | null;
};
type ArtifactContent = {
  artifact_id: string;
  title: string;
  kind: string;
  version: number;
  media_type: string;
  byte_length: number;
  content_base64: string;
  text?: string | null;
  terminal_safe_text?: string | null;
  source_links: SourceLink[];
};

const empty: Snapshot = { artifacts: [], cards: [], order: [], focused: null };
const artifactKinds = [
  "text",
  "code",
  "diff",
  "table",
  "chart",
  "diagram",
  "html_report",
  "image",
  "audio",
  "video",
  "pdf",
  "document",
  "spreadsheet",
  "presentation",
];
const textKinds = new Set(["text", "code", "diff", "table", "chart", "diagram", "html_report"]);

async function fileBase64(file: File): Promise<string> {
  if (file.size > 8 * 1024 * 1024) throw new Error("Artifact files are limited to 8 MiB.");
  const result = await new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error("The selected artifact file could not be read."));
    reader.onload = () => resolve(String(reader.result ?? ""));
    reader.readAsDataURL(file);
  });
  const separator = result.indexOf(",");
  if (separator < 0) throw new Error("The selected artifact file encoding is invalid.");
  return result.slice(separator + 1);
}

function parseSources(value: string): SourceLink[] {
  return value
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const [label, uri, hash] = line.split("|").map((part) => part.trim());
      return { label: label || "Source", uri: uri || "", content_hash: hash || null };
    });
}

function latest(artifact?: Artifact): number | undefined {
  return artifact?.versions.at(-1)?.version;
}

function dataUrl(content: ArtifactContent): string {
  return `data:${content.media_type};base64,${content.content_base64}`;
}

export function ArtifactsWorkspace() {
  const [snapshot, setSnapshot] = useState<Snapshot>(empty);
  const [selectedId, setSelectedId] = useState("");
  const [selectedVersion, setSelectedVersion] = useState<number>();
  const [content, setContent] = useState<ArtifactContent | null>(null);
  const [newTitle, setNewTitle] = useState("");
  const [newKind, setNewKind] = useState("text");
  const [newBody, setNewBody] = useState("");
  const [newSources, setNewSources] = useState("");
  const [newPin, setNewPin] = useState(false);
  const [newBinary, setNewBinary] = useState("");
  const [newMediaType, setNewMediaType] = useState("");
  const [newFileName, setNewFileName] = useState("");
  const [body, setBody] = useState("");
  const [sources, setSources] = useState("");
  const [versionBinary, setVersionBinary] = useState("");
  const [versionMediaType, setVersionMediaType] = useState("");
  const [versionFileName, setVersionFileName] = useState("");
  const [exportPath, setExportPath] = useState("");
  const [exportConfirmed, setExportConfirmed] = useState(false);
  const [deleteConfirmed, setDeleteConfirmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  const artifacts = useMemo(
    () => new Map(snapshot.artifacts.map((artifact) => [artifact.id, artifact])),
    [snapshot.artifacts],
  );
  const selected = artifacts.get(selectedId);

  async function refresh(preferredId?: string) {
    const next = await invoke<Snapshot>("artifact_snapshot");
    setSnapshot(next);
    const id = preferredId || selectedId || next.artifacts[0]?.id || "";
    setSelectedId(id);
    if (!id) setContent(null);
  }

  useEffect(() => {
    void refresh().catch((caught) => setError(String(caught)));
  }, []);

  useEffect(() => {
    if (!selectedId) return;
    const version = selectedVersion ?? latest(artifacts.get(selectedId));
    if (!version) return;
    setBusy(true);
    void invoke<ArtifactContent>("artifact_content", {
      artifactId: selectedId,
      version,
    })
      .then((next) => {
        setContent(next);
        setSelectedVersion(next.version);
        setBody(next.text ?? "");
        setSources(
          next.source_links
            .map((source) =>
              [source.label, source.uri, source.content_hash || ""].join(" | "),
            )
            .join("\n"),
        );
      })
      .catch((caught) => setError(String(caught)))
      .finally(() => setBusy(false));
  }, [artifacts, selectedId, selectedVersion]);

  async function create(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError("");
    setNotice("");
    const existingIds = new Set(snapshot.artifacts.map((artifact) => artifact.id));
    try {
      const next = await invoke<Snapshot>("artifact_create", {
        title: newTitle,
        kind: newKind,
        mediaType: newMediaType || null,
        content: newBody,
        contentBase64: newBinary || null,
        sourceLinks: parseSources(newSources),
        pin: newPin,
      });
      setSnapshot(next);
      const created = next.artifacts.find((artifact) => !existingIds.has(artifact.id));
      setSelectedId(created?.id ?? "");
      setSelectedVersion(latest(created));
      setNewTitle("");
      setNewBody("");
      setNewSources("");
      setNewPin(false);
      setNewBinary("");
      setNewMediaType("");
      setNewFileName("");
      setNotice("Artifact created in encrypted storage.");
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy(false);
    }
  }

  async function addVersion() {
    if (!selected) return;
    setBusy(true);
    setError("");
    try {
      const next = await invoke<Snapshot>("artifact_add_version", {
        artifactId: selected.id,
        mediaType: versionMediaType || content?.media_type || null,
        content: body,
        contentBase64: versionBinary || null,
        sourceLinks: parseSources(sources),
      });
      setSnapshot(next);
      const version = latest(next.artifacts.find((item) => item.id === selected.id));
      setSelectedVersion(version);
      setVersionBinary("");
      setVersionMediaType("");
      setVersionFileName("");
      setNotice(`Saved immutable version ${version}.`);
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy(false);
    }
  }

  async function restoreVersion() {
    if (!selected || selectedVersion == null) return;
    setBusy(true);
    setError("");
    try {
      const next = await invoke<Snapshot>("artifact_restore_version", {
        artifactId: selected.id,
        version: selectedVersion,
      });
      setSnapshot(next);
      const restored = latest(next.artifacts.find((item) => item.id === selected.id));
      setSelectedVersion(restored);
      setNotice(`Restored version ${selectedVersion} as new version ${restored}.`);
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy(false);
    }
  }

  async function action(
    actionName: string,
    values: Record<string, unknown> = {},
  ) {
    setBusy(true);
    setError("");
    try {
      const next = await invoke<Snapshot>("artifact_action", {
        action: actionName,
        artifactId: null,
        cardId: null,
        title: null,
        pinned: null,
        order: null,
        confirmed: null,
        ...values,
      });
      setSnapshot(next);
      if (!next.artifacts.some((artifact) => artifact.id === selectedId)) {
        setSelectedId(next.artifacts[0]?.id ?? "");
        setSelectedVersion(undefined);
        setContent(null);
      }
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy(false);
    }
  }

  async function exportArtifact() {
    if (!selected) return;
    setBusy(true);
    setError("");
    try {
      const destination = await invoke<string>("artifact_export", {
        artifactId: selected.id,
        version: selectedVersion ?? null,
        path: exportPath,
        confirmed: exportConfirmed,
      });
      setNotice(`Exported privately to ${destination}`);
      setExportConfirmed(false);
    } catch (caught) {
      setError(String(caught));
    } finally {
      setBusy(false);
    }
  }

  function moveCard(cardId: string, delta: -1 | 1) {
    const order = [...snapshot.order];
    const index = order.indexOf(cardId);
    const target = index + delta;
    if (index < 0 || target < 0 || target >= order.length) return;
    const current = order[index];
    const replacement = order[target];
    if (current == null || replacement == null) return;
    order[index] = replacement;
    order[target] = current;
    void action("reorder", { order });
  }

  return (
    <section className="artifact-workspace" aria-label="Artifact workspace">
      <header className="artifact-heading">
        <div>
          <small>ARTIFACTS / WHITEBOARD</small>
          <h2>Encrypted, versioned work products</h2>
          <p>Every revision is immutable and source-linked. Restoring creates a new version.</p>
        </div>
        <span>{snapshot.artifacts.length} artifacts · {snapshot.cards.length} cards</span>
      </header>
      {error && <p className="error-banner" role="alert">{error}</p>}
      {notice && <p className="artifact-notice" role="status">{notice}</p>}
      <div className="artifact-layout">
        <aside className="artifact-library">
          <form onSubmit={create}>
            <h3>New artifact</h3>
            <label>Title<input value={newTitle} onChange={(event) => setNewTitle(event.target.value)} required /></label>
            <label>Kind<select value={newKind} onChange={(event) => {
              setNewKind(event.target.value);
              setNewBinary("");
              setNewMediaType("");
              setNewFileName("");
            }}>{artifactKinds.map((item) => <option key={item} value={item}>{item.replaceAll("_", " ")}</option>)}</select></label>
            {textKinds.has(newKind) ? <label>Content<textarea value={newBody} onChange={(event) => setNewBody(event.target.value)} required /></label> : <label>Artifact file<input type="file" aria-label="Artifact file" onChange={(event) => {
              const file = event.target.files?.[0];
              if (!file) return;
              setBusy(true);
              void fileBase64(file).then((encoded) => {
                setNewBinary(encoded);
                setNewMediaType(file.type);
                setNewFileName(file.name);
                setError("");
              }).catch((caught) => setError(String(caught))).finally(() => setBusy(false));
            }} /><small>{newFileName || "Select a file up to 8 MiB"}</small></label>}
            <label>Sources <small>label | URL | optional SHA-256</small><textarea value={newSources} onChange={(event) => setNewSources(event.target.value)} /></label>
            <label className="artifact-check"><input type="checkbox" checked={newPin} onChange={(event) => setNewPin(event.target.checked)} /> Pin on whiteboard</label>
            <button className="primary" disabled={busy}>Create artifact</button>
          </form>
          <div className="artifact-list" aria-label="Artifact library">
            {snapshot.artifacts.map((artifact) => (
              <button
                key={artifact.id}
                className={artifact.id === selectedId ? "active" : ""}
                onClick={() => {
                  setSelectedId(artifact.id);
                  setSelectedVersion(latest(artifact));
                  setVersionBinary("");
                  setVersionMediaType("");
                  setVersionFileName("");
                }}
              >
                <strong>{artifact.title}</strong>
                <small>{artifact.kind.replaceAll("_", " ")} · {artifact.versions.length} versions</small>
              </button>
            ))}
            {!snapshot.artifacts.length && <p>No artifacts yet.</p>}
          </div>
        </aside>
        <main className="artifact-editor">
          {!selected ? <div className="artifact-empty"><h3>Select or create an artifact</h3><p>Text, code, reports and binary media share the same encrypted version history.</p></div> : <>
            <header>
              <div><small>{selected.kind.replaceAll("_", " ")}</small><h3>{selected.title}</h3></div>
              <label>Version<select value={selectedVersion ?? ""} onChange={(event) => setSelectedVersion(Number(event.target.value))}>{selected.versions.map((version) => <option key={version.version} value={version.version}>v{version.version} · {version.byte_length} bytes</option>)}</select></label>
            </header>
            <div className="artifact-preview">
              {content?.media_type.startsWith("image/") && content.media_type !== "image/svg+xml" ? <img src={dataUrl(content)} alt={content.title} /> : null}
              {content?.media_type.startsWith("audio/") ? <audio controls src={dataUrl(content)} /> : null}
              {content?.media_type.startsWith("video/") ? <video controls src={dataUrl(content)} /> : null}
              {content?.text != null ? <pre>{content.terminal_safe_text}</pre> : null}
              {content && content.text == null && !/^(image|audio|video)\//.test(content.media_type) ? <p>Binary preview is unavailable. Export this version to open it in its native application.</p> : null}
            </div>
            {textKinds.has(selected.kind) ? <label>Editor<textarea aria-label="Artifact editor" value={body} onChange={(event) => setBody(event.target.value)} /></label> : <label>New binary version<input type="file" aria-label="New binary version" onChange={(event) => {
              const file = event.target.files?.[0];
              if (!file) return;
              setBusy(true);
              void fileBase64(file).then((encoded) => {
                setVersionBinary(encoded);
                setVersionMediaType(file.type);
                setVersionFileName(file.name);
                setError("");
              }).catch((caught) => setError(String(caught))).finally(() => setBusy(false));
            }} /><small>{versionFileName || "Choose a replacement file to create a new immutable version"}</small></label>}
            <label>Version sources<textarea value={sources} onChange={(event) => setSources(event.target.value)} /></label>
            <div className="artifact-actions">
              <button className="primary" onClick={() => void addVersion()} disabled={busy || (!textKinds.has(selected.kind) && !versionBinary)}>Save new version</button>
              <button onClick={() => void restoreVersion()} disabled={busy || selectedVersion === latest(selected)}>Restore as new version</button>
              <button onClick={() => void action("add_to_board", { artifactId: selected.id })}>Add card</button>
            </div>
            <section className="artifact-export">
              <label>Absolute export path<input value={exportPath} onChange={(event) => setExportPath(event.target.value)} placeholder="/home/you/Documents/report.txt" /></label>
              <label className="artifact-check"><input type="checkbox" checked={exportConfirmed} onChange={(event) => setExportConfirmed(event.target.checked)} /> I approve writing this exact path without overwrite.</label>
              <button onClick={() => void exportArtifact()} disabled={!exportConfirmed || !exportPath}>Export version</button>
            </section>
            <section className="artifact-danger">
              <label className="artifact-check"><input type="checkbox" checked={deleteConfirmed} onChange={(event) => setDeleteConfirmed(event.target.checked)} /> Delete artifact metadata and every board card. Immutable blobs remain deduplicated.</label>
              <button className="danger" disabled={!deleteConfirmed} onClick={() => void action("delete", { artifactId: selected.id, confirmed: true })}>Delete artifact</button>
            </section>
          </>}
        </main>
        <aside className="whiteboard" aria-label="Whiteboard">
          <header><h3>Whiteboard</h3>{snapshot.focused && <button onClick={() => void action("clear_focus")}>Clear focus</button>}</header>
          {snapshot.cards.map((card, index) => {
            const artifact = artifacts.get(card.artifact_id);
            return <article key={card.id} className={snapshot.focused === card.id ? "focused" : ""}>
              <header><strong>{artifact?.title ?? "Missing artifact"}</strong>{card.pinned && <span>PINNED</span>}</header>
              <small>{artifact?.kind.replaceAll("_", " ")} · card {index + 1}</small>
              <div>
                <button aria-label={`Move ${artifact?.title} up`} onClick={() => moveCard(card.id, -1)} disabled={index === 0}>↑</button>
                <button aria-label={`Move ${artifact?.title} down`} onClick={() => moveCard(card.id, 1)} disabled={index === snapshot.cards.length - 1}>↓</button>
                <button onClick={() => void action("focus", { cardId: card.id })}>Focus</button>
                <button onClick={() => void action("pin", { cardId: card.id, pinned: !card.pinned })}>{card.pinned ? "Unpin" : "Pin"}</button>
                <button onClick={() => void action("copy_card", { cardId: card.id })}>Copy</button>
                <button onClick={() => void action("remove_card", { cardId: card.id })}>Remove</button>
              </div>
            </article>;
          })}
          {!snapshot.cards.length && <p>No cards. Add an artifact to begin.</p>}
        </aside>
      </div>
    </section>
  );
}
