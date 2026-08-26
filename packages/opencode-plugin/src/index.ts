type OpenCodeConfig = {
  autoupdate?: boolean; share?: string;
  permission?: Record<string, unknown>;
  agent?: Record<string, { tools?: Record<string, boolean> }>;
};
type Permission = { id?: string; sessionID?: string; permission?: string; patterns?: string[]; metadata?: unknown };
type ToolContext = { sessionID: string; directory: string };
type ToolDefinition = {
  description: string;
  args: Record<string, never>;
  execute(args: Record<string, never>, context: ToolContext): Promise<string>;
};

const disabledBuiltinEffects = [
  "apply_patch", "bash", "edit", "execute", "external_directory", "glob", "grep",
  "patch", "read", "skill", "task", "webfetch", "websearch", "write",
] as const;

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

/** Stable-sidecar plugin. All effectful built-ins are disabled and replaced by Personal Agent MCP tools. */
export default async function PersonalAgentPlugin() {
  return {
    config: async (config: OpenCodeConfig) => {
      config.autoupdate = false;
      config.share = "disabled";
      config.permission ??= {};
      for (const name of disabledBuiltinEffects) config.permission[name] = { "*": "deny" };
      config.agent ??= {};
      for (const agent of Object.values(config.agent)) {
        agent.tools ??= {};
        for (const name of disabledBuiltinEffects) agent.tools[name] = false;
      }
    },
    tool: { personal_agent_gateway_status: gatewayStatus },
    "permission.ask": async (_input: Permission, output: { status: "ask" | "deny" | "allow" }) => {
      // Unknown permission paths fail closed. Native Personal Agent tools ask through
      // the authenticated MCP/control channel and never depend on this callback.
      output.status = "deny";
    },
  };
}
