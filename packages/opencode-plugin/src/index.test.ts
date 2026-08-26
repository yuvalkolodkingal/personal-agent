import { describe, expect, it } from "bun:test";
import plugin from "./index";

describe("OpenCode safety bridge", () => {
  it("disables every effectful built-in", async () => {
    const hooks = await plugin(); const config: any = { agent: { jarvis: {} } };
    await hooks.config(config);
    for (const name of ["bash", "edit", "write", "task", "external_directory", "skill"]) {
      expect(config.permission[name]["*"]).toBe("deny"); expect(config.agent.jarvis.tools[name]).toBe(false);
    }
  });
});
