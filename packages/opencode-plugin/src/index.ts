type OpenCodeConfig = {
  autoupdate?: boolean; share?: string;
  permission?: Record<string, unknown>;
  agent?: Record<string, { tools?: Record<string, boolean> }>;
};
type Permission = { id?: string; sessionID?: string; permission?: string; patterns?: string[]; metadata?: unknown };
type ToolContext = { sessionID: string; directory: string };
type PluginContext = { directory?: string };
type ToolHookInput = { tool?: string; sessionID?: string; callID?: string };
type ToolDefinition = {
  description: string;
  args: Record<string, never>;
  execute(args: Record<string, never>, context: ToolContext): Promise<string>;
};

const codingTools = [
  "apply_patch", "bash", "edit", "execute", "glob", "grep", "list", "lsp", "patch",
  "read", "skill", "task", "todowrite", "todoread", "webfetch", "websearch", "write",
] as const;

const reviewedPermissions = new Set([
  "apply_patch", "bash", "doom_loop", "edit", "execute", "external_directory", "glob",
  "grep", "list", "lsp", "patch", "question", "read", "skill", "task", "todoread",
  "todowrite", "webfetch", "websearch", "write",
]);

const readRules = {
  "*": "allow",
  "*.env": "deny",
  "*.env.*": "deny",
  "*.env.example": "allow",
  "**/.env": "deny",
  "**/.env.*": "deny",
  "**/.env.example": "allow",
};

// Workspace edits are the core product capability. Shells stay approval-gated
// except for common inspection/build/test commands, while known destructive or
// external mutations remain denied or explicitly ask through the native UI.
const bashRules = {
  "*": "ask",
  "pwd": "allow",
  "ls*": "allow",
  "find *": "allow",
  "rg *": "allow",
  "grep *": "allow",
  "git status*": "allow",
  "git diff*": "allow",
  "git log*": "allow",
  "git show*": "allow",
  "git branch*": "allow",
  "cargo check*": "allow",
  "cargo test*": "allow",
  "cargo clippy*": "allow",
  "cargo fmt*": "allow",
  "bun test*": "allow",
  "bun run check*": "allow",
  "bun run build*": "allow",
  "npm test*": "allow",
  "npm run test*": "allow",
  "npm run build*": "allow",
  "pnpm test*": "allow",
  "pnpm run build*": "allow",
  "pytest*": "allow",
  "python -m pytest*": "allow",
  "git commit*": "ask",
  "git push*": "ask",
  "git clean*": "deny",
  "git reset --hard*": "deny",
  "rm *": "deny",
  "rmdir *": "deny",
  "sudo *": "deny",
  "doas *": "deny",
  "dd *": "deny",
  "mkfs*": "deny",
  "shutdown*": "deny",
  "reboot*": "deny",
};

const gatewayStatus: ToolDefinition = {
  description: "Check the authenticated native Personal Agent tool gateway for this registered session.",
  args: {},
  async execute(_args, context) {
    const endpoint = process.env.PERSONAL_AGENT_TOOL_GATEWAY_URL;
    const token = process.env.PERSONAL_AGENT_TOOL_GATEWAY_TOKEN;
    if (!endpoint || !token) throw new Error("native tool gateway is unavailable");
    const response = await fetch(endpoint, {
      method: "POST",
      headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
      body: JSON.stringify({ session_id: context.sessionID, directory: context.directory }),
    });
    if (!response.ok) throw new Error("native tool gateway rejected the call");
    return JSON.stringify(await response.json());
  },
};

async function authorizeBuiltin(
  input: ToolHookInput,
  args: Record<string, unknown>,
  directory: string | undefined,
): Promise<void> {
  if (!input.tool || !codingTools.includes(input.tool as (typeof codingTools)[number])) return;
  const endpoint = process.env.PERSONAL_AGENT_TOOL_GATEWAY_URL;
  const token = process.env.PERSONAL_AGENT_TOOL_GATEWAY_TOKEN;
  if (!endpoint || !token || !input.sessionID || !directory) {
    throw new Error("native coding-tool gateway is unavailable");
  }
  const response = await fetch(endpoint, {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify({
      operation: "authorize",
      session_id: input.sessionID,
      directory,
      call_id: input.callID,
      tool: input.tool,
      arguments: args,
    }),
  });
  if (!response.ok) {
    const detail = await response.text().catch(() => "");
    throw new Error(detail || "native coding-tool gateway rejected the call");
  }
}

/** Stable-sidecar plugin with workspace coding tools and fail-closed permissions. */
export default async function PersonalAgentPlugin(context: PluginContext = {}) {
  return {
    config: async (config: OpenCodeConfig) => {
      config.autoupdate = false;
      config.share = "disabled";
      config.permission ??= {};
      config.permission.read = readRules;
      config.permission.glob = "allow";
      config.permission.grep = "allow";
      config.permission.list = "allow";
      config.permission.lsp = "allow";
      config.permission.edit = "allow";
      config.permission.bash = bashRules;
      config.permission.task = "allow";
      config.permission.skill = "allow";
      config.permission.todowrite = "allow";
      config.permission.question = "allow";
      config.permission.webfetch = "ask";
      config.permission.websearch = "ask";
      config.permission.external_directory = "ask";
      config.permission.doom_loop = "ask";
      config.permission.execute = "ask";
      config.agent ??= {};
      for (const agent of Object.values(config.agent)) {
        agent.tools ??= {};
        for (const name of codingTools) agent.tools[name] = true;
      }
    },
    tool: { personal_agent_gateway_status: gatewayStatus },
    "tool.execute.before": async (
      input: ToolHookInput,
      output: { args: Record<string, unknown> },
    ) => authorizeBuiltin(input, output.args, context.directory),
    "permission.ask": async (input: Permission, output: { status: "ask" | "deny" | "allow" }) => {
      // Reviewed OpenCode permissions remain pending so Personal Agent can show
      // them in its native approval panel. Unknown permission names fail closed.
      if (!input.permission || !reviewedPermissions.has(input.permission)) output.status = "deny";
    },
  };
}
