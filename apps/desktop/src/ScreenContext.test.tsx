import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { ScreenContext } from "./ScreenContext";

const status = {
  connected: true,
  connection_detail: "Linux active-window command bridge connected",
  plan: {
    platform: "linux",
    session: "wayland",
    screen_capture_backend: "XDG ScreenCast portal / PipeWire",
    accessibility_backend: "AT-SPI over D-Bus",
    input_backend: "AT-SPI / XDG RemoteDesktop portal",
    capabilities: [
      {
        id: "desktop.active_view",
        backend: "AT-SPI / desktop portal",
        status: {
          state: "degraded",
          reason: "semantic child bridge is not connected",
          remediation: "enable AT-SPI",
        },
      },
      {
        id: "desktop.screen_capture",
        backend: "XDG ScreenCast portal / PipeWire",
        status: { state: "supported" },
      },
      {
        id: "desktop.input_control",
        backend: "AT-SPI / XDG RemoteDesktop portal",
        status: {
          state: "degraded",
          reason: "portal session is not granted",
        },
      },
    ],
  },
  permissions: {
    accessibility: { state: "granted" },
    screen_capture: {
      state: "not_determined",
      guidance: "choose a screen in the portal prompt",
    },
    input_control: {
      state: "unavailable",
      reason: "no input-control bridge",
    },
  },
};

const handle = {
  window_id: "window-1",
  generation: { epoch: 1, sequence: 1 },
  opaque_id: "editor",
};

const context = {
  snapshot: {
    generation: { epoch: 1, sequence: 1 },
    observed_at_unix_ms: 1,
    view: {
      application_id: "org.example.Editor",
      application_name: "Editor",
      title: "Document",
      secure_surface: false,
    },
    nodes: [
      {
        handle,
        role: "text_field",
        name: "Document body",
        value: "",
        states: ["enabled", "editable"],
        actions: ["focus", "set_value"],
      },
    ],
    backend: "AT-SPI",
    degraded_reasons: [],
  },
};

describe("screen context capability surface", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation((command: string) => {
      if (command === "desktop_status") return Promise.resolve(status);
      if (command === "desktop_snapshot") return Promise.resolve(context);
      if (command === "desktop_execute") return Promise.resolve({});
      return Promise.resolve(undefined);
    });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("shows degraded reasons and disables only unavailable capabilities", async () => {
    render(<ScreenContext />);

    expect(await screen.findByText("semantic child bridge is not connected · enable AT-SPI")).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "include pixels" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Read active view" })).toBeEnabled();
    expect(screen.getByText("choose a screen in the portal prompt")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Read active view" }));
    expect(await screen.findByText("Document body")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Focus" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Type" })).toBeDisabled();
  });

  it("sends semantic postconditions for approved text control", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "desktop_status")
        return Promise.resolve({
          ...status,
          permissions: {
            accessibility: { state: "granted" },
            screen_capture: { state: "granted" },
            input_control: { state: "granted" },
          },
        });
      if (command === "desktop_snapshot") return Promise.resolve(context);
      if (command === "desktop_execute") return Promise.resolve({});
      return Promise.resolve(undefined);
    });
    vi.spyOn(window, "prompt").mockReturnValue("Hello world");
    render(<ScreenContext />);

    fireEvent.click(await screen.findByRole("button", { name: "Read active view" }));
    fireEvent.click(await screen.findByRole("button", { name: "Type" }));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "desktop_execute",
        expect.objectContaining({
          request: expect.objectContaining({
            authorization: expect.objectContaining({
              approved_effects: ["write_text"],
              sensitive_text_approved: true,
            }),
            postconditions: expect.arrayContaining([
              expect.objectContaining({
                postcondition: "condition",
                condition: "node_value_contains",
                text: "Hello world",
              }),
            ]),
          }),
        }),
      ),
    );
  });
});
