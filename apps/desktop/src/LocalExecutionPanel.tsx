import { useState, type FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";

type ExecutionResult = {
  operation_id: string;
  started_at: string;
  finished_at: string;
  exit_code?: number | null;
  stdout: string;
  stderr: string;
  truncated: boolean;
  timed_out: boolean;
  pty: "supported" | "degraded" | "unavailable";
};

type Props = { workingDirectory: string };
type Runner = "process" | "docker";

function lines(value: string) {
  return value
    .split("\n")
    .map((item) => item.trim())
    .filter(Boolean);
}

const presets = [
  { name: "Git status", program: "git", arguments: "status\n--short" },
  { name: "Run tests", program: "cargo", arguments: "test\n--workspace" },
  { name: "Frontend check", program: "bun", arguments: "run\ncheck" },
  { name: "List files", program: "rg", arguments: "--files" },
] as const;

export function LocalExecutionPanel({ workingDirectory }: Props) {
  const [runner, setRunner] = useState<Runner>("process");
  const [program, setProgram] = useState("git");
  const [argumentsText, setArgumentsText] = useState("status\n--short");
  const [image, setImage] = useState("alpine:3.22");
  const [mountWorkspace, setMountWorkspace] = useState(false);
  const [network, setNetwork] = useState(false);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<ExecutionResult | null>(null);
  const [error, setError] = useState("");

  const execute = async (confirmed: boolean) => {
    if (runner === "process") {
      return invoke<ExecutionResult>("local_execute", {
        confirmed,
        spec: {
          program: program.trim(),
          args: lines(argumentsText),
          cwd: workingDirectory,
          environment: {},
          mode: "captured",
          timeout_ms: 120_000,
          max_output_bytes: 1_048_576,
          network_requested: network,
        },
      });
    }
    return invoke<ExecutionResult>("docker_execute", {
      confirmed,
      request: {
        image: image.trim(),
        command: lines(argumentsText),
        cwd: workingDirectory,
        mounts: mountWorkspace
          ? [
              {
                host: workingDirectory,
                container: "/workspace",
                writable: false,
              },
            ]
          : [],
        network_requested: network,
        timeout_ms: 120_000,
        max_output_bytes: 1_048_576,
      },
    });
  };

  const run = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError("");
    setResult(null);
    try {
      setResult(await execute(false));
    } catch (caught) {
      const message = String(caught);
      if (
        message.toLocaleLowerCase().includes("requires approval") &&
        window.confirm(
          `Approve this exact local operation?\n\n${runner === "process" ? program : `docker ${image}`}\n${lines(argumentsText).join(" ")}`,
        )
      ) {
        try {
          setResult(await execute(true));
        } catch (approvedError) {
          setError(String(approvedError));
        }
      } else {
        setError(message);
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="local-execution-panel">
      <header>
        <div>
          <span>PRIVATE LOCAL EXECUTION</span>
          <h3>Run structured commands and hardened containers</h3>
          <p>
            Arguments are passed directly—never through a shell. Output and run
            time are bounded, and destructive operations require confirmation.
          </p>
        </div>
        <div className="runner-switch" role="group" aria-label="Execution type">
          <button className={runner === "process" ? "active" : ""} onClick={() => setRunner("process")}>Process</button>
          <button className={runner === "docker" ? "active" : ""} onClick={() => setRunner("docker")}>Docker</button>
        </div>
      </header>
      <div className="execution-presets" aria-label="Command presets">
        {presets.map((preset) => (
          <button
            key={preset.name}
            onClick={() => {
              setRunner("process");
              setProgram(preset.program);
              setArgumentsText(preset.arguments);
            }}
          >
            {preset.name}
          </button>
        ))}
      </div>
      <form onSubmit={(event) => void run(event)}>
        {runner === "process" ? (
          <label>
            <span>Program</span>
            <input required value={program} onChange={(event) => setProgram(event.target.value)} placeholder="git" />
          </label>
        ) : (
          <label>
            <span>Container image</span>
            <input required value={image} onChange={(event) => setImage(event.target.value)} placeholder="alpine:3.22" />
          </label>
        )}
        <label className="execution-arguments">
          <span>{runner === "process" ? "Arguments" : "Container command"} · one per line</span>
          <textarea value={argumentsText} onChange={(event) => setArgumentsText(event.target.value)} rows={4} />
        </label>
        <div className="execution-options">
          {runner === "docker" && (
            <label><input type="checkbox" checked={mountWorkspace} onChange={(event) => setMountWorkspace(event.target.checked)} /> Mount workspace read-only at /workspace</label>
          )}
          <label><input type="checkbox" checked={network} onChange={(event) => setNetwork(event.target.checked)} /> Request network access</label>
          <code>{workingDirectory}</code>
        </div>
        <button className="primary" disabled={busy} type="submit">{busy ? "Running…" : `Run ${runner}`}</button>
      </form>
      {error && <p className="error-banner">{error}</p>}
      {result && (
        <article className={`execution-result ${result.exit_code === 0 && !result.timed_out ? "success" : "failed"}`}>
          <header>
            <strong>{result.timed_out ? "Timed out" : `Exit ${result.exit_code ?? "signal"}`}</strong>
            <span>{result.operation_id}</span>
            {result.truncated && <b>output truncated</b>}
          </header>
          {result.stdout && <pre>{result.stdout}</pre>}
          {result.stderr && <pre className="stderr">{result.stderr}</pre>}
        </article>
      )}
    </section>
  );
}
