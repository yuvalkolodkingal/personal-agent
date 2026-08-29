import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { McpManager } from "./McpManager";
import type {
  McpCatalogEntry,
  McpManagedServer,
  McpManagerAction,
  McpManagerActionResult,
  McpManagerController,
  McpManagerSnapshot,
} from "./McpManager.types";

const connectedServer: McpManagedServer = {
  definition: {
    id: "5ed8eead-7ec3-4e32-a25d-f34e1311bb52",
    name: "GitHub",
    namespace: "github",
    description: "Issues, pull requests, and repository search",
    source: { kind: "catalog", catalog_id: "github", publisher: "Model Context Protocol" },
    transport: {
      kind: "streamable_http",
      endpoint: "https://mcp.github.example/v1",
      stateless: true,
      headers: [],
      oauth: null,
    },
    supported_protocols: ["2026-07-28", "2025-06-18"],
    preferred_protocol: "2026-07-28",
    install: null,
    project_scopes: [],
    agent_scopes: [],
    tags: ["development", "github"],
  },
  state: "connected",
  enabled: true,
  negotiated_protocol: "2026-07-28",
  health: {
    healthy: true,
    checked_at: "2026-08-28T10:00:00Z",
    latency_ms: 18,
    error_rate: 0,
    consecutive_failures: 0,
    message: "Healthy",
  },
  catalog: {
    tools: [
      {
        name: "search_issues",
        title: "Search issues",
        description: "Search repository issues",
        input_schema: {
          type: "object",
          properties: {
            query: { type: "string", title: "Query", description: "GitHub search query" },
            limit: { type: "integer", title: "Limit", default: 10 },
          },
          required: ["query"],
          additionalProperties: false,
        },
        output_schema: null,
        annotations: { read_only: true, destructive: false, idempotent: true, open_world: false },
        resolved_name: "github.search_issues",
      },
    ],
    resources: [
      { uri: "github://repo/issues", name: "Repository issues", description: "Visible issues", mime_type: "application/json" },
    ],
    prompts: [{ name: "triage", description: "Triage an issue", arguments_schema: null }],
    supports_logging: true,
    supports_completions: false,
    supports_resource_subscriptions: false,
  },
  permissions: [
    {
      tool: "github.search_issues",
      scope: { kind: "global" },
      decision: "ask",
      execution_zone: "mcp-restricted",
      max_calls_per_minute: 30,
      timeout_ms: 30_000,
      max_output_bytes: 1_048_576,
    },
  ],
  current_release: null,
  release_history: [],
  pending_update: null,
  logs: [{ timestamp: "2026-08-28T10:00:00Z", level: "info", message: "MCP initialization completed" }],
  last_connected_at: "2026-08-28T10:00:00Z",
};

const snapshot: McpManagerSnapshot = {
  servers: [connectedServer],
  audit_events: [],
  protocol_version: "2026-07-28",
};

function controller(
  execute: McpManagerController["execute"] = vi.fn(async () => ({})),
): McpManagerController {
  return { execute };
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("MCP Manager", () => {
  it("renders health, catalogs, scopes, and credential-safe controls", () => {
    render(<McpManager snapshot={snapshot} controller={controller()} />);
    expect(screen.getByRole("heading", { name: "MCP Manager" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open GitHub" })).toBeInTheDocument();
    expect(screen.getByText("18 ms")).toBeInTheDocument();
    expect(screen.getByText("MCP 2026-07-28")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Connect OAuth" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add key securely" })).toBeInTheDocument();
    expect(screen.queryByLabelText(/API key value/i)).not.toBeInTheDocument();
  });

  it("keeps OAuth authorization single-flight across all sign-in controls", async () => {
    let finishAuthorization: ((value: McpManagerActionResult) => void) | undefined;
    const pending = new Promise<McpManagerActionResult>((resolve) => {
      finishAuthorization = resolve;
    });
    const execute = vi.fn(() => pending);
    const authenticationRequired = {
      ...connectedServer,
      state: "authentication_required" as const,
      negotiated_protocol: null,
    };
    render(
      <McpManager
        snapshot={{ ...snapshot, servers: [authenticationRequired] }}
        controller={controller(execute)}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect(screen.getAllByRole("button", { name: "Signing in…" })).toHaveLength(2);
    fireEvent.click(screen.getAllByRole("button", { name: "Signing in…" })[1]!);
    expect(execute).toHaveBeenCalledTimes(1);
    expect(execute).toHaveBeenCalledWith({
      type: "start_oauth",
      server_id: connectedServer.definition.id,
    });

    finishAuthorization?.({});
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Sign in" })).toBeEnabled(),
    );
  });

  it("generates a typed test form and routes the request through the controller", async () => {
    const execute = vi.fn(async (action: McpManagerAction) =>
      action.type === "test_tool"
        ? {
            test_output: {
              tool: action.tool,
              duration_ms: 14,
              content: { issues: 3 },
              truncated: false,
            },
          }
        : {},
    );
    render(<McpManager snapshot={snapshot} controller={controller(execute)} />);
    fireEvent.click(screen.getByRole("button", { name: /Tools/ }));
    fireEvent.change(screen.getByLabelText("Query *"), { target: { value: "is:open label:bug" } });
    fireEvent.change(screen.getByLabelText("Limit"), { target: { value: "5" } });
    fireEvent.click(screen.getByRole("button", { name: "Review & run" }));
    await waitFor(() => {
      expect(execute).toHaveBeenCalledWith({
        type: "test_tool",
        server_id: connectedServer.definition.id,
        tool: "github.search_issues",
        arguments: { query: "is:open label:bug", limit: 5 },
      });
    });
    expect(screen.getByText(/ToolGateway and may require approval/)).toBeInTheDocument();
    expect(
      await screen.findByText(
        (_, element) =>
          element?.tagName === "PRE" &&
          Boolean(element.textContent?.includes('"issues": 3')),
      ),
    ).toBeInTheDocument();
  });

  it("binds ask-mode tool execution to an in-app approval digest", async () => {
    const execute = vi.fn(async (action: McpManagerAction) => {
      if (action.type === "test_tool" && !action.approval_digest) {
        return {
          operation_preview: {
            digest: "tool-bound-digest",
            display_text: 'MCP tool: github.search_issues\nArguments:\n{"query":"security"}',
          },
        };
      }
      return {};
    });
    render(<McpManager snapshot={snapshot} controller={controller(execute)} />);
    fireEvent.click(screen.getByRole("button", { name: /Tools/ }));
    fireEvent.change(screen.getByLabelText("Query *"), { target: { value: "security" } });
    fireEvent.click(screen.getByRole("button", { name: "Review & run" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(
      within(dialog).getByText(/github\.search_issues/, { selector: "code" }),
    ).toBeInTheDocument();
    fireEvent.click(
      within(dialog).getByLabelText(
        "I reviewed the exact operation above and authorize it.",
      ),
    );
    fireEvent.click(within(dialog).getByRole("button", { name: "Run tool" }));
    await waitFor(() =>
      expect(execute).toHaveBeenCalledWith({
        type: "test_tool",
        server_id: connectedServer.definition.id,
        tool: "github.search_issues",
        arguments: { query: "security", limit: 10 },
        approval_digest: "tool-bound-digest",
      }),
    );
  });

  it("changes global tool policy without implying that gateway approval is bypassed", async () => {
    const execute = vi.fn(async () => ({}));
    render(<McpManager snapshot={snapshot} controller={controller(execute)} />);
    fireEvent.click(screen.getByRole("button", { name: /Permissions/ }));
    fireEvent.change(screen.getByLabelText("Permission for github.search_issues"), { target: { value: "allow" } });
    await waitFor(() => {
      expect(execute).toHaveBeenCalledWith(expect.objectContaining({
        type: "set_permission",
        server_id: connectedServer.definition.id,
        rule: expect.objectContaining({ tool: "github.search_issues", decision: "allow" }),
      }));
    });
    expect(screen.getByText("ToolGateway is always in control")).toBeInTheDocument();
  });

  it("adds a manual stdio definition without shell parsing or secret fields", async () => {
    const execute = vi.fn(async () => ({}));
    render(<McpManager snapshot={{ ...snapshot, servers: [] }} controller={controller(execute)} />);
    fireEvent.click(screen.getByRole("button", { name: "+ Add MCP server" }));
    fireEvent.click(screen.getByRole("button", { name: /Connect manually/ }));
    fireEvent.change(screen.getByLabelText("Name *"), { target: { value: "Local Files" } });
    fireEvent.change(screen.getByLabelText("Namespace *"), { target: { value: "local_files" } });
    fireEvent.change(screen.getByLabelText("Executable *"), { target: { value: "npx" } });
    fireEvent.change(screen.getByLabelText(/Arguments/), { target: { value: "-y\n@mcp/server-files\n/home/yuval/Documents" } });
    fireEvent.click(screen.getByRole("button", { name: "Validate & add" }));
    await waitFor(() => {
      expect(execute).toHaveBeenCalledWith(expect.objectContaining({
        type: "add_manual",
        definition: expect.objectContaining({
          name: "Local Files",
          namespace: "local_files",
          transport: {
            kind: "stdio",
            executable: "npx",
            arguments: ["-y", "@mcp/server-files", "/home/yuval/Documents"],
            working_directory: null,
            environment: [],
          },
        }),
      }));
    });
  });

  it("imports via a redacted preview before accepting definitions", async () => {
    const imported = { ...connectedServer.definition, id: "67ea5e69-ef7f-4f80-b787-af425d9c8fd0", source: { kind: "imported" as const, application: "OpenCode" } };
    const execute = vi.fn(async (action: McpManagerAction) => {
      if (action.type === "preview_import") {
        return {
          import_preview: {
            definitions: [imported],
            issues: [{ server_name: "GitHub", field: "env.GITHUB_TOKEN", code: "secret_omitted", message: "Credential value was discarded. Reconnect it through the OS keychain." }],
          },
        };
      }
      return {};
    });
    render(<McpManager snapshot={{ ...snapshot, servers: [] }} controller={controller(execute)} />);
    fireEvent.click(screen.getByRole("button", { name: "+ Add MCP server" }));
    fireEvent.click(screen.getByRole("button", { name: /Import configuration/ }));
    fireEvent.change(screen.getByLabelText("Configuration JSON"), { target: { value: '{"mcp":{"servers":{}}}' } });
    fireEvent.click(screen.getByRole("button", { name: "Inspect import" }));
    expect(await screen.findByText("Credential value was discarded. Reconnect it through the OS keychain.")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Import 1 server" }));
    await waitFor(() => expect(execute).toHaveBeenCalledWith({ type: "accept_import", definitions: [imported] }));
  });

  it("requires in-app exact-operation consent before an update", async () => {
    const updateServer: McpManagedServer = {
      ...connectedServer,
      state: "update_available",
      pending_update: {
        target_version: "2.0.0",
        release_notes_url: null,
        recipe: { program: "npm", arguments: ["install", "@mcp/github@2"], expected_artifact_sha256: null, source_url: null },
      },
    };
    const execute = vi.fn(async (action: McpManagerAction) => {
      if (action.type === "update_preview") {
        return { operation_preview: { digest: "bound-digest", display_text: "npm install @mcp/github@2" } };
      }
      return {};
    });
    render(<McpManager snapshot={{ ...snapshot, servers: [updateServer] }} controller={controller(execute)} />);
    fireEvent.click(screen.getByRole("button", { name: "Update to 2.0.0" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(within(dialog).getByText("npm install @mcp/github@2")).toBeInTheDocument();
    const updateButton = within(dialog).getByRole("button", { name: "Update" });
    expect(updateButton).toBeDisabled();
    fireEvent.click(within(dialog).getByLabelText("I reviewed the exact operation above and authorize it."));
    fireEvent.click(updateButton);
    await waitFor(() => expect(execute).toHaveBeenCalledWith({ type: "update", server_id: updateServer.definition.id, operation_digest: "bound-digest" }));
  });

  it("catalog installation exposes the exact command and requires consent", async () => {
    const entry: McpCatalogEntry = {
      id: "filesystem",
      name: "Filesystem",
      publisher: "MCP",
      description: "Scoped file access",
      tags: ["files"],
      verified: true,
      transport: "stdio",
      install_command: "npm install -g @mcp/filesystem@1.0.0",
      install_digest: "catalog-bound-digest",
      requested_environment: [],
      requested_network_origins: [],
    };
    const execute = vi.fn(async () => ({}));
    render(<McpManager snapshot={{ ...snapshot, servers: [] }} controller={controller(execute)} catalog={[entry]} />);
    fireEvent.click(screen.getByRole("button", { name: "+ Add MCP server" }));
    fireEvent.click(screen.getByRole("button", { name: /Browse catalog/ }));
    fireEvent.click(screen.getByRole("button", { name: /Filesystem/ }));
    expect(screen.getByText(entry.install_command!)).toBeInTheDocument();
    const add = screen.getByRole("button", { name: "Add server" });
    expect(add).toBeDisabled();
    fireEvent.click(screen.getByLabelText(/I reviewed this exact command/));
    fireEvent.click(add);
    await waitFor(() => expect(execute).toHaveBeenCalledWith({ type: "add_catalog", catalog_id: "filesystem", install_digest: "catalog-bound-digest" }));
  });

  it("exposes consented uninstall and tombstone purge actions", async () => {
    const execute = vi.fn(async (action: McpManagerAction) => {
      if (action.type === "uninstall_preview") {
        return {
          operation_preview: {
            digest: "uninstall-digest",
            display_text: "Uninstall MCP server GitHub",
          },
        };
      }
      return {};
    });
    const { rerender } = render(
      <McpManager snapshot={snapshot} controller={controller(execute)} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Uninstall server" }));
    const dialog = await screen.findByRole("alertdialog");
    fireEvent.click(
      within(dialog).getByLabelText(
        "I reviewed the exact operation above and authorize it.",
      ),
    );
    fireEvent.click(within(dialog).getByRole("button", { name: "Uninstall" }));
    await waitFor(() =>
      expect(execute).toHaveBeenCalledWith({
        type: "uninstall",
        server_id: connectedServer.definition.id,
        operation_digest: "uninstall-digest",
      }),
    );

    rerender(
      <McpManager
        snapshot={{
          ...snapshot,
          servers: [
            { ...connectedServer, state: "uninstalled", enabled: false },
          ],
        }}
        controller={controller(execute)}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Purge tombstone" }));
    await waitFor(() =>
      expect(execute).toHaveBeenCalledWith({
        type: "purge",
        server_id: connectedServer.definition.id,
      }),
    );
  });
});
