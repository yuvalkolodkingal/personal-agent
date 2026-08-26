import { describe, expect, it } from "bun:test";
import { StatusPill } from "./status-pill";

describe("StatusPill", () => {
  it("is an exported component", () => expect(typeof StatusPill).toBe("function"));
});
