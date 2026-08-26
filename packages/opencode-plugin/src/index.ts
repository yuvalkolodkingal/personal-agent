type OpenCodeConfig = {
  autoupdate?: boolean; share?: string;
  permission?: Record<string, unknown>;
  agent?: Record<string, { tools?: Record<string, boolean> }>;
};
type Permission = { id?: string; sessionID?: string; permission?: string; patterns?: string[]; metadata?: unknown };

const builtinEffects = ["bash", "edit", "write", "task", "external_directory", "skill"];

/** Stable-sidecar plugin. All effectful built-ins are disabled and replaced by Personal Agent MCP tools. */
export default async function PersonalAgentPlugin() {
  return {
    config: async (config: OpenCodeConfig) => {
      config.autoupdate = false;
      config.share = "disabled";
      config.permission ??= {};
      for (const name of builtinEffects) config.permission[name] = { "*": "deny" };
      config.agent ??= {};
      for (const agent of Object.values(config.agent)) {
        agent.tools ??= {};
        for (const name of builtinEffects) agent.tools[name] = false;
      }
    },
    "permission.ask": async (input: Permission, output: { status: "ask" | "deny" | "allow" }) => {
      // Unknown permission paths fail closed. Native Personal Agent tools ask through
      // the authenticated MCP/control channel and never depend on this callback.
      output.status = "deny";
      const endpoint = process.env.PERSONAL_AGENT_PERMISSION_ENDPOINT;
      const token = process.env.PERSONAL_AGENT_PERMISSION_TOKEN;
      if (!endpoint || !token) return;
      try {
        const response = await fetch(endpoint, {
          method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
          body: JSON.stringify(input), signal: AbortSignal.timeout(5_000),
        });
        if (!response.ok) return;
        const decision = await response.json() as { decision?: string };
        output.status = decision.decision === "allow" ? "allow" : decision.decision === "ask" ? "ask" : "deny";
      } catch { output.status = "deny"; }
    },
  };
}
