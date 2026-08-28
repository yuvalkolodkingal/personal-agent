import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { McpManager } from "./McpManager";
import type {
  McpManagerAction,
  McpManagerActionResult,
  McpManagerSnapshot,
} from "./McpManager.types";

const empty: McpManagerSnapshot = {
  servers: [],
  audit_events: [],
  protocol_version: "2026-07-28",
};

export function McpManagerHost() {
  const [snapshot, setSnapshot] = useState<McpManagerSnapshot>(empty);
  const [error, setError] = useState("");
  useEffect(() => {
    void invoke<McpManagerSnapshot>("mcp_manager_snapshot")
      .then(setSnapshot)
      .catch((caught) => setError(String(caught)));
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<McpManagerSnapshot>("mcp-manager://changed", (event) => {
      if (!disposed) {
        setSnapshot(event.payload);
        setError("");
      }
    })
      .then((dispose) => {
        if (disposed) dispose();
        else unlisten = dispose;
      })
      .catch((caught) => {
        if (!disposed) setError(String(caught));
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
  const controller = useMemo(
    () => ({
      execute: (action: McpManagerAction) =>
        invoke<McpManagerActionResult>("mcp_manager_execute", { action }),
    }),
    [],
  );
  return (
    <>
      {error && <p className="error-banner">{error}</p>}
      <McpManager
        snapshot={snapshot}
        controller={controller}
        onSnapshot={setSnapshot}
      />
    </>
  );
}
