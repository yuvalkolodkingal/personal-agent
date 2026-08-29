import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { ArtifactsWorkspace } from "./ArtifactsWorkspace";

const artifact = {
  id: "0198f0ec-75a8-7000-8000-000000000001",
  title: "Architecture report",
  kind: "text",
  versions: [
    {
      version: 1,
      content_sha256: "a".repeat(64),
      media_type: "text/plain",
      byte_length: 11,
      source_links: [],
    },
  ],
};
const card = {
  id: "0198f0ec-75a8-7000-8000-000000000002",
  artifact_id: artifact.id,
  pinned: false,
};
const populated = {
  artifacts: [artifact],
  cards: [card],
  order: [card.id],
  focused: null,
};

describe("artifact workspace", () => {
  beforeEach(() => {
    invoke.mockReset().mockImplementation((command: string) => {
      if (command === "artifact_snapshot") return Promise.resolve({ artifacts: [], cards: [], order: [], focused: null });
      if (command === "artifact_create" || command === "artifact_action") return Promise.resolve(populated);
      if (command === "artifact_content") return Promise.resolve({
        artifact_id: artifact.id,
        title: artifact.title,
        kind: artifact.kind,
        version: 1,
        media_type: "text/plain",
        byte_length: 11,
        content_base64: "aGVsbG8gd29ybGQ=",
        text: "hello world",
        terminal_safe_text: "hello world",
        source_links: [],
      });
      return Promise.resolve(populated);
    });
  });

  afterEach(() => cleanup());

  it("creates source-linked content through the encrypted native boundary", async () => {
    render(<ArtifactsWorkspace />);
    const form = screen.getByRole("heading", { name: "New artifact" }).closest("form");
    expect(form).not.toBeNull();
    const controls = within(form!);
    fireEvent.change(controls.getByLabelText("Title"), { target: { value: "Architecture report" } });
    fireEvent.change(controls.getByLabelText("Content"), { target: { value: "hello world" } });
    fireEvent.change(controls.getByLabelText(/Sources/), { target: { value: "Design | https://example.test/design" } });
    fireEvent.click(controls.getByRole("checkbox", { name: /Pin on whiteboard/ }));
    fireEvent.click(controls.getByRole("button", { name: "Create artifact" }));

    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifact_create", {
      title: "Architecture report",
      kind: "text",
      mediaType: null,
      content: "hello world",
      contentBase64: null,
      sourceLinks: [{ label: "Design", uri: "https://example.test/design", content_hash: null }],
      pin: true,
    }));
    expect(await screen.findByText("Artifact created in encrypted storage.")).toBeInTheDocument();
  });

  it("requires exact confirmations for export and deletion", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "artifact_snapshot") return Promise.resolve(populated);
      if (command === "artifact_content") return Promise.resolve({
        artifact_id: artifact.id,
        title: artifact.title,
        kind: artifact.kind,
        version: 1,
        media_type: "text/plain",
        byte_length: 11,
        content_base64: "aGVsbG8gd29ybGQ=",
        text: "hello world",
        terminal_safe_text: "hello world",
        source_links: [],
      });
      if (command === "artifact_export") return Promise.resolve("/tmp/report.txt");
      return Promise.resolve({ artifacts: [], cards: [], order: [], focused: null });
    });
    render(<ArtifactsWorkspace />);
    expect((await screen.findAllByText("Architecture report")).length).toBeGreaterThan(0);
    const exportButton = screen.getByRole("button", { name: "Export version" });
    const deleteButton = screen.getByRole("button", { name: "Delete artifact" });
    expect(exportButton).toBeDisabled();
    expect(deleteButton).toBeDisabled();

    fireEvent.change(screen.getByLabelText("Absolute export path"), { target: { value: "/tmp/report.txt" } });
    fireEvent.click(screen.getByRole("checkbox", { name: /approve writing this exact path/ }));
    fireEvent.click(exportButton);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifact_export", {
      artifactId: artifact.id,
      version: 1,
      path: "/tmp/report.txt",
      confirmed: true,
    }));

    fireEvent.click(screen.getByRole("checkbox", { name: /Delete artifact metadata/ }));
    fireEvent.click(deleteButton);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("artifact_action", expect.objectContaining({
      action: "delete",
      artifactId: artifact.id,
      confirmed: true,
    })));
  });
});
