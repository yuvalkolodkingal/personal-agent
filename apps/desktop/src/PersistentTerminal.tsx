import { invoke } from "@tauri-apps/api/core";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";

type PtySnapshot = {
  id: string;
  title: string;
  command: string;
  args: string[];
  cwd: string;
  status: string;
  pid: number;
  exit_code?: number | null;
  attached: boolean;
  connection: string;
  cursor: number;
  revision: number;
  scrollback_bytes: number;
  scrollback_limit_bytes: number;
  error?: string | null;
};

type PtyRead = {
  id: string;
  data: string;
  reset: boolean;
  revision: number;
  cursor: number;
  connection: string;
  error?: string | null;
};

type PtyCapability = {
  available: boolean;
  backend: string;
  platform: string;
  native_verified: boolean;
  persistence: string;
  reconnect: string;
  detail: string;
};

const detail = (caught: unknown) =>
  caught instanceof Error ? caught.message : String(caught);

export function PersistentTerminal({
  workingDirectory,
  shell,
}: {
  workingDirectory: string;
  shell: string;
}) {
  const terminalHost = useRef<HTMLDivElement>(null);
  const terminal = useRef<Terminal | null>(null);
  const fit = useRef<FitAddon | null>(null);
  const selectedRef = useRef("");
  const revision = useRef<number | undefined>(undefined);
  const reconnectGeneration = useRef(0);
  const resizeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [sessions, setSessions] = useState<PtySnapshot[]>([]);
  const [selected, setSelected] = useState("");
  const [attachedId, setAttachedId] = useState("");
  const [connection, setConnection] = useState("detached");
  const [capability, setCapability] = useState<PtyCapability | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    const next = await invoke<PtySnapshot[]>("pty_list", {
      directory: workingDirectory,
    });
    setSessions(next);
    setSelected((current) =>
      current && next.some((session) => session.id === current)
        ? current
        : (next[0]?.id ?? ""),
    );
  }, [workingDirectory]);

  useEffect(() => {
    void Promise.all([
      invoke<PtyCapability>("pty_capability").then(setCapability),
      refresh(),
    ]).catch((caught) => setError(detail(caught)));
  }, [refresh]);

  useEffect(() => {
    const host = terminalHost.current;
    if (!host) return;
    const next = new Terminal({
      allowTransparency: false,
      convertEol: false,
      cursorBlink: true,
      cursorStyle: "bar",
      fontFamily: "JetBrains Mono, SFMono-Regular, Consolas, monospace",
      fontSize: 12,
      scrollback: 10_000,
      theme: {
        background: "#05090e",
        foreground: "#c1d7de",
        cursor: "#45d7eb",
        selectionBackground: "#174b5a",
        black: "#071018",
        brightBlack: "#58717b",
        cyan: "#45d7eb",
        brightCyan: "#8eeeff",
        red: "#ef756b",
        green: "#52d6a2",
        yellow: "#e3bd69",
      },
    });
    const nextFit = new FitAddon();
    next.loadAddon(nextFit);
    next.open(host);
    nextFit.fit();
    terminal.current = next;
    fit.current = nextFit;

    const data = next.onData((value) => {
      const id = selectedRef.current;
      if (!id) return;
      void invoke("pty_input", { request: { id, data: value } }).catch(
        (caught) => setError(detail(caught)),
      );
    });
    const resize = next.onResize(({ rows, cols }) => {
      const id = selectedRef.current;
      if (!id) return;
      if (resizeTimer.current) clearTimeout(resizeTimer.current);
      resizeTimer.current = setTimeout(() => {
        void invoke("pty_resize", {
          request: { id, directory: workingDirectory, rows, cols },
        }).catch((caught) => setError(detail(caught)));
      }, 120);
    });
    const observer = new ResizeObserver(() => nextFit.fit());
    observer.observe(host);
    return () => {
      if (resizeTimer.current) clearTimeout(resizeTimer.current);
      observer.disconnect();
      data.dispose();
      resize.dispose();
      next.dispose();
      terminal.current = null;
      fit.current = null;
    };
  }, [workingDirectory]);

  const reconnect = useCallback(
    async (id: string) => {
      if (!id) return;
      const generation = reconnectGeneration.current + 1;
      reconnectGeneration.current = generation;
      setAttachedId("");
      setBusy(true);
      setError("");
      setConnection("connecting");
      try {
        const next = await invoke<PtySnapshot>("pty_reconnect", {
          id,
          directory: workingDirectory,
        });
        setSessions((current) =>
          current.map((item) => (item.id === next.id ? next : item)),
        );
        if (
          reconnectGeneration.current === generation &&
          selectedRef.current === id
        ) {
          setAttachedId(next.id);
          setConnection(next.connection);
        }
      } catch (caught) {
        if (reconnectGeneration.current === generation) {
          setConnection("degraded");
          setError(detail(caught));
        }
      } finally {
        if (reconnectGeneration.current === generation) setBusy(false);
      }
    },
    [workingDirectory],
  );

  useEffect(() => {
    selectedRef.current = selected;
    setAttachedId("");
    revision.current = undefined;
    terminal.current?.reset();
    setConnection(selected ? "connecting" : "detached");
    if (selected) void reconnect(selected);
  }, [reconnect, selected]);

  useEffect(() => {
    if (!selected || attachedId !== selected) return;
    let active = true;
    let reading = false;
    const read = async () => {
      if (reading) return;
      reading = true;
      try {
        const next = await invoke<PtyRead>("pty_read", {
          id: selected,
          afterRevision: revision.current ?? null,
        });
        if (!active || selectedRef.current !== selected) return;
        if (next.reset) terminal.current?.reset();
        if (next.data) terminal.current?.write(next.data);
        revision.current = next.revision;
        setConnection(next.connection);
        if (next.error) setError(next.error);
      } catch (caught) {
        if (active) setError(detail(caught));
      } finally {
        reading = false;
      }
    };
    void read();
    const timer = window.setInterval(() => void read(), 180);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, [attachedId, selected]);

  const create = async (confirmed = false) => {
    setBusy(true);
    setError("");
    try {
      const next = await invoke<PtySnapshot>("pty_create", {
        request: {
          directory: workingDirectory,
          command: shell,
          args: [],
          cwd: workingDirectory,
          title: "Personal Agent terminal",
          env: { TERM: "xterm-256color", COLORTERM: "truecolor" },
          confirmed,
        },
      });
      await refresh();
      setSelected(next.id);
    } catch (caught) {
      const message = detail(caught);
      if (
        !confirmed &&
        message.includes("confirmation required") &&
        window.confirm(`${message}\n\nStart this terminal?`)
      ) {
        await create(true);
        return;
      }
      setError(message);
    } finally {
      setBusy(false);
    }
  };

  const terminate = async () => {
    if (!selected) return;
    const item = sessions.find((session) => session.id === selected);
    if (
      !window.confirm(
        `Terminate ${item?.title ?? "this terminal"} and its running process?`,
      )
    ) {
      return;
    }
    reconnectGeneration.current += 1;
    setBusy(true);
    setError("");
    try {
      await invoke("pty_terminate", {
        id: selected,
        directory: workingDirectory,
        confirmed: true,
      });
      terminal.current?.reset();
      revision.current = undefined;
      setAttachedId("");
      setSelected("");
      await refresh();
    } catch (caught) {
      setError(detail(caught));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="persistent-terminal" aria-label="Workspace terminals">
      <header className="persistent-terminal__toolbar">
        <div>
          <strong>Workspace terminal</strong>
          <span className={`terminal-connection ${connection}`}>
            {connection}
          </span>
        </div>
        <div className="persistent-terminal__actions">
          <button disabled={busy} onClick={() => void create()}>
            + New
          </button>
          <button
            disabled={!selected || busy}
            onClick={() => void reconnect(selected)}
          >
            Reconnect
          </button>
          <button
            className="danger"
            disabled={!selected || busy}
            onClick={() => void terminate()}
          >
            Terminate
          </button>
        </div>
      </header>
      <div className="persistent-terminal__layout">
        <aside aria-label="Terminal sessions">
          {sessions.length ? (
            sessions.map((session) => (
              <button
                className={session.id === selected ? "active" : ""}
                key={session.id}
                onClick={() => setSelected(session.id)}
              >
                <span>{session.title || "Terminal"}</span>
                <small>
                  PID {session.pid} · {session.status}
                </small>
              </button>
            ))
          ) : (
            <p>No terminal sessions. Create one to start coding.</p>
          )}
        </aside>
        <div className="persistent-terminal__surface">
          <div ref={terminalHost} className="persistent-terminal__xterm" />
          {!selected && (
            <div className="persistent-terminal__empty">
              <strong>Native workspace shell</strong>
              <span>Create a terminal to run interactive tools.</span>
            </div>
          )}
        </div>
      </div>
      {error && (
        <p className="persistent-terminal__error" role="alert">
          {error}
        </p>
      )}
      <footer>
        <span>{workingDirectory}</span>
        <span>
          {capability?.backend ?? "Checking PTY backend"} ·{" "}
          {capability?.native_verified ? "live verified" : "build supported"}
        </span>
      </footer>
    </section>
  );
}
