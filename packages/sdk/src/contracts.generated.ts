// Generated from contracts/proto/events.proto. Do not edit.
export const CONTRACT_SOURCE_SHA256 = "8821049676b65923f276505ab437c326abf1919801308393e1c1b7e8803460b4" as const;
export const EVENT_SCHEMA_VERSION = 1 as const;

export interface EventEnvelope {
  schemaVersion: number; eventId: string; wallClockTimestamp: string;
  monotonicSequence: bigint; origin: string; profileId: string;
  sessionId?: string; goalId?: string; taskId?: string; agentId?: string;
  type: string; payloadJson: Uint8Array;
}

export interface ControlRequest { requestId: string; command: string; argumentsJson: Uint8Array; }
export interface ControlResponse { requestId: string; ok: boolean; resultJson: Uint8Array; errorCode?: string; errorMessage?: string; }
