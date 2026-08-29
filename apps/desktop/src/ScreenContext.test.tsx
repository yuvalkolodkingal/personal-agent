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
      {
        handle: { ...handle, opaque_id: "save" },
        role: "button",
        name: "Save",
        states: ["enabled"],
        actions: ["press"],
      },
    ],
    backend: "AT-SPI",
    degraded_reasons: [],
  },
};

const portalStatus = {
  interfaces: {
    screencast_version: 6,
    remote_desktop_version: undefined,
    available_source_types: 7,
    available_cursor_modes: 3,
  },
  phase: "idle",
  consent: "required",
  kind: undefined,
  streams: [],
  pipewire_transport: false,
  detail: "ScreenCast v6 is available; RemoteDesktop control is not exposed by this portal backend",
};

describe("screen context capability surface", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation((command: string) => {
      if (command === "desktop_status") return Promise.resolve(status);
      if (command === "desktop_snapshot") return Promise.resolve(context);
      if (command === "desktop_execute") return Promise.resolve({});
      if (command === "portal_status") return Promise.resolve(portalStatus);
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
    expect(screen.getByRole("button", { name: "Press" })).toBeDisabled();
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

  it("dispatches an approved semantic press action", async () => {
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
    render(<ScreenContext />);

    fireEvent.click(await screen.findByRole("button", { name: "Read active view" }));
    fireEvent.click(await screen.findByRole("button", { name: "Press" }));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("desktop_execute", {
        request: expect.objectContaining({
          action: expect.objectContaining({
            action: "click",
            button: "primary",
            click_count: 1,
          }),
          authorization: expect.objectContaining({ approved_effects: ["interact"] }),
          postconditions: [{ postcondition: "generation_advanced" }],
        }),
      }),
    );
  });

  it("exposes consent-bound portal capture without pretending control exists", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "desktop_status") return Promise.resolve(status);
      if (command === "portal_status") return Promise.resolve(portalStatus);
      if (command === "portal_connect")
        return Promise.resolve({
          ...portalStatus,
          phase: "active",
          consent: "granted",
          kind: "screen_cast",
          streams: [{ node_id: 42, size: [1920, 1080] }],
          detail: "Portal selection is active. PipeWire frame transport is not connected.",
        });
      return Promise.resolve(undefined);
    });
    render(<ScreenContext />);

    expect(await screen.findByRole("button", { name: "Share screen via portal" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Grant screen control" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Share screen via portal" }));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("portal_connect", {
        requestControl: false,
        parentWindow: "",
      }),
    );
    expect(await screen.findByText("User-selected session active")).toBeInTheDocument();
    expect(screen.getByText(/PipeWire frames not connected/)).toBeInTheDocument();
  });
});
