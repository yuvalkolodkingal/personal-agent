import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    fit() {}
  },
}));
vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    loadAddon() {}
    open() {}
    dispose() {}
    reset() {}
    write() {}
    onData() {
      return { dispose() {} };
    }
    onResize() {
      return { dispose() {} };
    }
  },
}));

import { PersistentTerminal } from "./PersistentTerminal";

const session = {
  id: "pty_safe",
  title: "Personal Agent terminal",
  command: "/bin/bash",
  args: [],
  cwd: "/workspace",
  status: "running",
  pid: 42,
  attached: true,
  connection: "connected",
  cursor: 0,
  revision: 0,
  scrollback_bytes: 0,
  scrollback_limit_bytes: 1024 * 1024,
};

describe("PersistentTerminal", () => {
  beforeEach(() => {
    vi.stubGlobal(
      "ResizeObserver",
      class {
        observe() {}
        disconnect() {}
      },
    );
    vi.stubGlobal("confirm", vi.fn(() => true));
    invoke.mockReset();
    invoke.mockImplementation((command: string) => {
      if (command === "pty_capability") {
        return Promise.resolve({
          available: true,
          backend: "opencode-pinned-pty",
          platform: "linux",
          native_verified: true,
          persistence: "runtime-lifetime",
          reconnect: "absolute-cursor",
          detail: "verified",
        });
      }
      if (command === "pty_list") return Promise.resolve([]);
      if (command === "pty_create") return Promise.resolve(session);
      if (command === "pty_reconnect") return Promise.resolve(session);
      if (command === "pty_read") {
        return Promise.resolve({
          id: session.id,
          data: "",
          reset: false,
          revision: 0,
          cursor: 0,
          connection: "connected",
        });
      }
      return Promise.resolve(null);
    });
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it("creates a workspace-bound PTY with structured program and arguments", async () => {
    render(
      <PersistentTerminal workingDirectory="/workspace" shell="/bin/bash" />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "+ New" }));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("pty_create", {
        request: {
          directory: "/workspace",
          command: "/bin/bash",
          args: [],
          cwd: "/workspace",
          title: "Personal Agent terminal",
          env: { TERM: "xterm-256color", COLORTERM: "truecolor" },
          confirmed: false,
        },
      }),
    );
  });

  it("requires a user confirmation before terminating a process", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "pty_capability") return Promise.resolve({ backend: "pty" });
      if (command === "pty_list") return Promise.resolve([session]);
      if (command === "pty_reconnect") return Promise.resolve(session);
      if (command === "pty_read") {
        return Promise.resolve({
          id: session.id,
          data: "",
          reset: false,
          revision: 0,
          cursor: 0,
          connection: "connected",
        });
      }
      return Promise.resolve(null);
    });
    render(
      <PersistentTerminal workingDirectory="/workspace" shell="/bin/bash" />,
    );
    const terminate = await screen.findByRole("button", { name: "Terminate" });
    await waitFor(() => expect(terminate).toBeEnabled());
    fireEvent.click(terminate);
    await waitFor(() => expect(window.confirm).toHaveBeenCalled());
    expect(invoke).toHaveBeenCalledWith("pty_terminate", {
      id: session.id,
      directory: "/workspace",
      confirmed: true,
    });
  });
});
