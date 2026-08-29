import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AppConfig } from "./types";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { SkillsAgents } from "./SkillsAgents";

const managedAgent = {
  kind: "agent",
  name: "reviewer",
  content: "---\ndescription: Reviews work\nmode: subagent\n---\nReview this work.\n",
  digest: "agent-digest",
  enabled: true,
  path_hint: "agents/reviewer.md",
};
const snapshot = {
  agents: {
    available: true,
    data: [
      {
        name: "reviewer",
        description: "Reviews work",
        mode: "all",
        model: { providerID: "openai", modelID: "gpt-5.6" },
        tools: { bash: false, read: true },
        permission: { edit: "ask", bash: "deny" },
      },
      { name: "build", description: "Built-in builder", mode: "primary", builtIn: true },
    ],
  },
  commands: {
    available: true,
    data: { test: { description: "Run tests", agent: "build", template: "Run $ARGUMENTS" } },
  },
  skills: {
    available: true,
    data: [{ name: "agent-authored-skill", description: "Untrusted discovered skill" }],
  },
  managed_documents: [managedAgent],
  default_agent: "build",
};
const config = {
  runtime: { default_agent: "build" },
} as unknown as AppConfig;

describe("Skills & Agents workspace", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "skills_agents_snapshot") return snapshot;
      if (command === "save_config") return { config: args?.config };
      return { message: "Saved." };
    });
  });
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("shows runtime model, tools, permissions and persists the selected agent", async () => {
    const onConfig = vi.fn();
    render(<SkillsAgents config={config} onConfig={onConfig} />);
    expect(await screen.findByText("openai/gpt-5.6")).toBeInTheDocument();
    expect(screen.getByText("read: true")).toBeInTheDocument();
    expect(screen.getByText("edit: ask")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Use as default" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("save_config", {
        config: expect.objectContaining({ runtime: { default_agent: "reviewer" } }),
      }),
    );
    expect(onConfig).toHaveBeenCalled();
  });

  it("requires exact explicit confirmation before creating a user document", async () => {
    render(<SkillsAgents config={config} onConfig={vi.fn()} />);
    await screen.findByText("reviewer");
    fireEvent.click(screen.getByRole("button", { name: "+ Agent" }));
    fireEvent.change(screen.getByLabelText("Document name"), { target: { value: "security-review" } });
    const template = (screen.getByLabelText("Document Markdown") as HTMLTextAreaElement).value;
    expect(template).toContain("edit: true\n  bash: true");
    expect(template).toContain("edit: allow\n  bash: ask");
    expect(screen.getByRole("button", { name: "Save agent" })).toBeDisabled();
    expect(invoke).not.toHaveBeenCalledWith("skills_agents_write", expect.anything());
    fireEvent.click(screen.getByLabelText(/I reviewed this exact Markdown/));
    expect(screen.getByRole("button", { name: "Save agent" })).toBeEnabled();
    fireEvent.change(screen.getByLabelText("Document Markdown"), {
      target: { value: managedAgent.content },
    });
    expect(screen.getByRole("button", { name: "Save agent" })).toBeDisabled();
    fireEvent.click(screen.getByLabelText(/I reviewed this exact Markdown/));
    fireEvent.click(screen.getByRole("button", { name: "Save agent" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "skills_agents_write",
        expect.objectContaining({
          kind: "agent",
          name: "security-review",
          mode: "create",
          expectedDigest: null,
          confirmed: true,
        }),
      ),
    );
  });

  it("keeps a rejected save visible inside the open editor", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "skills_agents_snapshot") return snapshot;
      if (command === "skills_agents_write") throw new Error("document validation failed");
      return { message: "Saved." };
    });
    render(<SkillsAgents config={config} onConfig={vi.fn()} />);
    await screen.findByText("reviewer");
    fireEvent.click(screen.getByRole("button", { name: "+ Agent" }));
    fireEvent.change(screen.getByLabelText("Document name"), {
      target: { value: "security-review" },
    });
    fireEvent.click(screen.getByLabelText(/I reviewed this exact Markdown/));
    fireEvent.click(screen.getByRole("button", { name: "Save agent" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("document validation failed");
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });

  it("shows managed documents when the runtime catalog is unavailable", async () => {
    invoke.mockResolvedValueOnce({
      ...snapshot,
      agents: { available: false, reason: "runtime is starting" },
      commands: { available: false, reason: "runtime is starting" },
      skills: { available: false, reason: "runtime is starting" },
    });
    render(<SkillsAgents config={config} onConfig={vi.fn()} />);
    expect(await screen.findByText("reviewer")).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /agents 1/i })).toBeInTheDocument();
    expect(screen.getByText("Reviews work")).toBeInTheDocument();
  });

  it("imports renderer-read Markdown only after review and confirmation", async () => {
    render(<SkillsAgents config={config} onConfig={vi.fn()} />);
    await screen.findByText("reviewer");
    const file = new File(
      ["---\ndescription: Imported command\nagent: build\n---\nDo $ARGUMENTS\n"],
      "release-notes.md",
      { type: "text/markdown" },
    );
    fireEvent.change(screen.getByLabelText("Import Markdown", { selector: "input" }), {
      target: { files: [file] },
    });
    expect(await screen.findByDisplayValue("release-notes")).toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalledWith("skills_agents_write", expect.anything());
    fireEvent.click(screen.getByLabelText(/I reviewed this exact Markdown/));
    fireEvent.click(screen.getByRole("button", { name: "Save agent" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "skills_agents_write",
        expect.objectContaining({ mode: "import", confirmed: true }),
      ),
    );
  });

  it("keeps discovered skills read-only with no installation action", async () => {
    render(<SkillsAgents config={config} onConfig={vi.fn()} />);
    fireEvent.click(await screen.findByRole("tab", { name: /skills/i }));
    expect(await screen.findByText("agent-authored-skill")).toBeInTheDocument();
    expect(screen.getByRole("note")).toHaveTextContent("never installed automatically");
    expect(screen.queryByRole("button", { name: /install/i })).not.toBeInTheDocument();
  });
});
