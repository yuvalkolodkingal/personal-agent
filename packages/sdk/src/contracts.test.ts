import { describe, expect, it } from "bun:test";
import { CONTRACT_SOURCE_SHA256, EVENT_SCHEMA_VERSION } from "./contracts.generated";

describe("generated contracts", () => {
  it("carry a source fingerprint and version", () => {
    expect(CONTRACT_SOURCE_SHA256).toHaveLength(64);
    expect(EVENT_SCHEMA_VERSION).toBe(1);
  });
});
