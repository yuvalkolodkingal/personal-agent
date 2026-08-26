// Generated from contracts/proto/events.proto. Do not edit.
export const CONTRACT_SOURCE_SHA256 = "9e95299c9f54ae7121013324f3e0f8e3801e57801397fa5c3b850c2a5ec0c1b2" as const;
export const EVENT_SCHEMA_VERSION = 1 as const;

export interface EventEnvelope {
  schemaVersion: number; eventId: string; wallClockTimestamp: string;
  monotonicSequence: bigint; origin: string; profileId: string;
  sessionId?: string; goalId?: string; taskId?: string; agentId?: string;
  type: string; payloadJson: Uint8Array;
}

export interface ControlRequest { requestId: string; command: string; argumentsJson: Uint8Array; }
export interface ControlResponse { requestId: string; ok: boolean; resultJson: Uint8Array; errorCode?: string; errorMessage?: string; }
