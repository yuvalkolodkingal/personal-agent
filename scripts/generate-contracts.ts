import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const proto = readFileSync(resolve(root, "contracts/proto/events.proto"), "utf8");
const hash = createHash("sha256").update(proto).digest("hex");
const destination = resolve(root, "packages/sdk/src/contracts.generated.ts");
const output = `// Generated from contracts/proto/events.proto. Do not edit.\n` +
`export const CONTRACT_SOURCE_SHA256 = "${hash}" as const;\n` +
`export const EVENT_SCHEMA_VERSION = 1 as const;\n\n` +
`export interface EventEnvelope {\n` +
`  schemaVersion: number; eventId: string; wallClockTimestamp: string;\n` +
`  monotonicSequence: bigint; origin: string; profileId: string;\n` +
`  sessionId?: string; goalId?: string; taskId?: string; agentId?: string;\n` +
`  type: string; payloadJson: Uint8Array;\n` +
`}\n\n` +
`export interface ControlRequest { requestId: string; command: string; argumentsJson: Uint8Array; }\n` +
`export interface ControlResponse { requestId: string; ok: boolean; resultJson: Uint8Array; errorCode?: string; errorMessage?: string; }\n`;

if (process.argv.includes("--check")) {
  const current = readFileSync(destination, "utf8");
  if (current !== output) {
    console.error("generated TypeScript contracts are out of date; run bun run contracts:generate");
    process.exit(1);
  }
} else {
  writeFileSync(destination, output);
}
