import { describe, expect, it } from "bun:test";
import plugin from "./index";

const disabledBuiltinEffects = [
  "apply_patch", "bash", "edit", "execute", "external_directory", "glob", "grep",
  "patch", "read", "skill", "task", "webfetch", "websearch", "write",
] as const;

describe("OpenCode safety bridge", () => {
  it("exposes only the default plugin function to the legacy loader", async () => {
    const module = await import("./index");
    expect(Object.keys(module)).toEqual(["default"]);
    expect(typeof module.default).toBe("function");
  });

  it("disables every effectful built-in", async () => {
    const hooks = await plugin(); const config: any = { agent: { jarvis: {} } };
    await hooks.config(config);
    for (const name of disabledBuiltinEffects) {
      expect(config.permission[name]["*"]).toBe("deny"); expect(config.agent.jarvis.tools[name]).toBe(false);
    }
  });

  it("fails every upstream permission callback closed", async () => {
    const hooks = await plugin();
    const output: { status: "ask" | "deny" | "allow" } = { status: "allow" };
    await hooks["permission.ask"]({}, output);
    expect(output.status).toBe("deny");
  });

  it("registers only the native authenticated gateway status slice", async () => {
    const hooks = await plugin();
    expect(Object.keys(hooks.tool)).toEqual(["personal_agent_gateway_status"]);
    expect(hooks.tool.personal_agent_gateway_status.args).toEqual({});
  });
});
