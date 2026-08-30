import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { DiagnosticsView } from "./LazyDestinations";

const voiceStates = {
  idle: {
    glyph: "○",
    label: "Idle",
    hint: "",
    color: "#fff",
    stoppable: false,
  },
};

function renderDiagnostics(capabilities: unknown[]) {
  render(
    <DiagnosticsView
      diagnostic={
        {
          product: "Personal Agent",
          version: "0.1.0",
          platform: "linux",
          arch: "x86_64",
          opencode: { pinned: "1.18.23", topology: "sidecar" },
          capabilities,
        } as never
      }
      catalog={{} as never}
      projection={{ runtime_healthy: true } as never}
      voice={{ stt_ready: true, tts_ready: true } as never}
      voiceStates={voiceStates as never}
    />,
  );
  return screen.getByLabelText("Implementation audit");
}

afterEach(cleanup);

describe("implementation audit", () => {
  it("renders one row per live capability instead of a hardcoded table", () => {
    const audit = renderDiagnostics([
      { id: "screen.capture", backend: "portal", status: { state: "supported" } },
      {
        id: "desktop.input",
        backend: "libei",
        status: {
          state: "degraded",
          reason: "No compositor session.",
          remediation: "Grant the portal permission.",
        },
      },
      {
        id: "vision.ocr",
        backend: "none",
        status: { state: "unsupported", reason: "No OCR engine installed." },
      },
    ]);

    const rows = within(audit).getAllByRole("article");
    expect(rows).toHaveLength(3);
    // Indexing is checked, so bind the asserted length to named rows.
    const [supported, degraded, unsupported] = rows as [
      HTMLElement,
      HTMLElement,
      HTMLElement,
    ];

    expect(within(supported).getByText("screen.capture")).toBeInTheDocument();
    expect(within(supported).getByLabelText("implemented")).toHaveTextContent("✓");
    // A supported capability has no reason, so the backend is the detail.
    expect(within(supported).getByText("portal")).toBeInTheDocument();

    expect(within(degraded).getByLabelText("partial")).toHaveTextContent("◐");
    expect(
      within(degraded).getByText(
        "No compositor session. Grant the portal permission.",
      ),
    ).toBeInTheDocument();

    expect(within(unsupported).getByLabelText("not wired")).toHaveTextContent("○");
    expect(
      within(unsupported).getByText("No OCR engine installed."),
    ).toBeInTheDocument();
  });

  it("reflects a backend change rather than a stale narrative", () => {
    const audit = renderDiagnostics([
      { id: "voice.tts", backend: "kokoro", status: { state: "supported" } },
    ]);
    expect(within(audit).getByText("kokoro")).toBeInTheDocument();
    // The removed hardcoded table asserted Qwen3-TTS unconditionally.
    expect(within(audit).queryByText(/Qwen3-TTS/)).toBeNull();
  });

  it("tolerates a bare string status", () => {
    const audit = renderDiagnostics([
      { id: "legacy.probe", backend: "shim", status: "supported" },
    ]);
    expect(within(audit).getByLabelText("implemented")).toBeInTheDocument();
  });
});
