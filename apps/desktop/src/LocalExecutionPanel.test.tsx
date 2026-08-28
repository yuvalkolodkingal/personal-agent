import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { LocalExecutionPanel } from "./LocalExecutionPanel";

describe("local execution panel", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue({
      operation_id: "run-1",
      started_at: "2026-08-28T00:00:00Z",
      finished_at: "2026-08-28T00:00:01Z",
      exit_code: 0,
      stdout: "clean\n",
      stderr: "",
      truncated: false,
      timed_out: false,
      pty: "unavailable",
    });
  });

  afterEach(() => cleanup());

  it("sends a structured argv request without a shell command string", async () => {
    render(<LocalExecutionPanel workingDirectory="/workspace/project" />);
    fireEvent.click(screen.getByRole("button", { name: "Git status" }));
    fireEvent.click(screen.getByRole("button", { name: "Run process" }));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("local_execute", {
        confirmed: false,
        spec: expect.objectContaining({
          program: "git",
          args: ["status", "--short"],
          cwd: "/workspace/project",
          mode: "captured",
        }),
      }),
    );
    expect(await screen.findByText("Exit 0")).toBeInTheDocument();
    expect(screen.getByText(/clean/)).toBeInTheDocument();
  });

  it("uses hardened Docker defaults through the native request boundary", async () => {
    render(<LocalExecutionPanel workingDirectory="/workspace/project" />);
    fireEvent.click(screen.getByRole("button", { name: "Docker" }));
    fireEvent.click(screen.getByRole("checkbox", { name: /Mount workspace/ }));
    fireEvent.click(screen.getByRole("button", { name: "Run docker" }));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "docker_execute",
        expect.objectContaining({
          confirmed: false,
          request: expect.objectContaining({
            image: "alpine:3.22",
            network_requested: false,
            mounts: [
              {
                host: "/workspace/project",
                container: "/workspace",
                writable: false,
              },
            ],
          }),
        }),
      ),
    );
  });
});
