import { describe, expect, it } from "bun:test";
import plugin from "./index";

const codingTools = [
  "apply_patch", "bash", "edit", "execute", "glob", "grep", "list", "lsp", "patch",
  "read", "skill", "task", "todowrite", "todoread", "webfetch", "websearch", "write",
] as const;

describe("OpenCode safety bridge", () => {
  it("exposes only the default plugin function to the legacy loader", async () => {
    const module = await import("./index");
    expect(Object.keys(module)).toEqual(["default"]);
    expect(typeof module.default).toBe("function");
  });

  it("enables workspace coding tools with granular safety policy", async () => {
    const hooks = await plugin(); const config: any = { agent: { jarvis: {} } };
    await hooks.config(config);
    for (const name of codingTools) expect(config.agent.jarvis.tools[name]).toBe(true);
    expect(config.permission.edit).toBe("allow");
    expect(config.permission.read["*"]).toBe("allow");
    expect(config.permission.read["**/.env"]).toBe("deny");
    expect(config.permission.bash["*"]).toBe("ask");
    expect(config.permission.bash["cargo test*"]).toBe("allow");
    expect(config.permission.bash["git push*"]).toBe("ask");
    expect(config.permission.bash["git reset --hard*"]).toBe("deny");
    expect(config.permission.external_directory).toBe("ask");
    expect(config.permission.webfetch).toBe("ask");
  });

  it("keeps reviewed approvals pending and fails unknown permissions closed", async () => {
    const hooks = await plugin();
    const reviewed: { status: "ask" | "deny" | "allow" } = { status: "ask" };
    await hooks["permission.ask"]({ permission: "bash" }, reviewed);
    expect(reviewed.status).toBe("ask");
    const unknown: { status: "ask" | "deny" | "allow" } = { status: "allow" };
    await hooks["permission.ask"]({ permission: "unreviewed-native-effect" }, unknown);
    expect(unknown.status).toBe("deny");
  });

  it("registers only the native authenticated gateway status slice", async () => {
    const hooks = await plugin();
    expect(Object.keys(hooks.tool)).toEqual(["personal_agent_gateway_status"]);
    expect(hooks.tool.personal_agent_gateway_status.args).toEqual({});
  });
});
