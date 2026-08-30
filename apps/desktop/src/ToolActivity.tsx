import { memo } from "react";
import type { EventEnvelope } from "./types";
import { eventPayload } from "./types";

/**
 * Tool cards for the live turn.
 *
 * The pinned sidecar boundary only forwards `{assistantMessageID, callID, tool,
 * status, provider}` for `tool.*` events — raw arguments and results are
 * discarded there on purpose. So this surface never fabricates them: when
 * `ui.show_tool_details` is on and the boundary supplied nothing, it says so.
 * A native gateway path that forwards redacted `arguments`/`result` fields will
 * light the same detail rows up without further changes here.
 */

export type ToolCallStatus = "running" | "completed" | "failed";

export type ToolCall = {
  id: string;
  name: string;
  status: ToolCallStatus;
  startedAt: number | null;
  endedAt: number | null;
  provider: string;
  /** Only ever populated when the event boundary actually carried them. */
  argumentsText: string | null;
  resultText: string | null;
  detailSource: "gateway" | "none";
};

const MAX_TOOL_CALLS = 50;
const DETAIL_LIMIT = 4_000;

function timestamp(event: EventEnvelope): number | null {
  const parsed = Date.parse(event.wall_clock_timestamp);
  return Number.isFinite(parsed) ? parsed : null;
}

function firstString(payload: Record<string, unknown>, keys: string[]) {
  for (const key of keys) {
    const value = payload[key];
    if (typeof value === "string" && value.trim()) return value.trim();
    if (value && typeof value === "object") {
      try {
        return JSON.stringify(value, null, 2);
      } catch {
        return null;
      }
    }
  }
  return null;
}

function clamp(value: string | null) {
  if (value === null) return null;
  return value.length > DETAIL_LIMIT
    ? `${value.slice(0, DETAIL_LIMIT)}\n… truncated`
    : value;
}

export function describeToolDuration(call: ToolCall): string {
  if (call.startedAt === null || call.endedAt === null)
    return call.status === "running" ? "in progress" : "duration unknown";
  const elapsed = Math.max(0, call.endedAt - call.startedAt);
  return elapsed < 1_000
    ? `${elapsed} ms`
    : `${(elapsed / 1_000).toFixed(elapsed < 10_000 ? 2 : 1)} s`;
}

/**
 * Fold one `tool.*` runtime event into the current card list. Exported so the
 * reducer is testable without a rendered transcript.
 */
export function reduceToolEvent(
  current: ToolCall[],
  event: EventEnvelope,
): ToolCall[] {
  if (!event.type.startsWith("tool.")) return current;
  const payload = eventPayload(event);
  const id = String(
    payload.callID ?? payload.call_id ?? payload.id ?? event.event_id,
  );
  const name = String(payload.tool ?? payload.name ?? "unnamed tool");
  const provider = String(payload.provider ?? "");
  const argumentsText = clamp(
    firstString(payload, ["arguments", "redacted_arguments", "input"]),
  );
  const resultText = clamp(
    firstString(payload, ["result", "redacted_result", "output"]),
  );
  const at = timestamp(event);
  const status: ToolCallStatus =
    event.type === "tool.completed"
      ? "completed"
      : event.type === "tool.failed"
        ? "failed"
        : "running";
  const index = current.findIndex((call) => call.id === id);
  const existing = index >= 0 ? current[index]! : null;
  const merged: ToolCall = {
    id,
    name: existing && name === "unnamed tool" ? existing.name : name,
    status: event.type === "tool.progress" ? (existing?.status ?? "running") : status,
    startedAt: existing?.startedAt ?? at,
    endedAt: status === "running" ? (existing?.endedAt ?? null) : at,
    provider: provider || (existing?.provider ?? ""),
    argumentsText: argumentsText ?? existing?.argumentsText ?? null,
    resultText: resultText ?? existing?.resultText ?? null,
    detailSource: "none",
  };
  merged.detailSource =
    merged.argumentsText !== null || merged.resultText !== null
      ? "gateway"
      : "none";
  if (existing) {
    const next = [...current];
    next[index] = merged;
    return next;
  }
  return [...current, merged].slice(-MAX_TOOL_CALLS);
}

function ToolCard({
  call,
  showDetails,
}: {
  call: ToolCall;
  showDetails: boolean;
}) {
  return (
    <details className={`tool-card tool-${call.status}`} data-tool-call={call.id}>
      <summary>
        <strong>{call.name}</strong>
        <span className={`tool-status tool-status-${call.status}`}>
          {call.status}
        </span>
        <small>{describeToolDuration(call)}</small>
      </summary>
      <dl>
        <div>
          <dt>Call</dt>
          <dd>{call.id}</dd>
        </div>
        {call.provider && (
          <div>
            <dt>Provider</dt>
            <dd>{call.provider}</dd>
          </div>
        )}
      </dl>
      {showDetails ? (
        call.detailSource === "gateway" ? (
          <div className="tool-detail">
            {call.argumentsText !== null && (
              <>
                <h4>Arguments</h4>
                <pre>{call.argumentsText}</pre>
              </>
            )}
            {call.resultText !== null && (
              <>
                <h4>Result</h4>
                <pre>{call.resultText}</pre>
              </>
            )}
          </div>
        ) : (
          <p className="tool-detail-missing">
            Arguments and results are discarded at the runtime boundary, so this
            path shows name and status only.
          </p>
        )
      ) : null}
    </details>
  );
}

export const ToolActivity = memo(function ToolActivity({
  calls,
  showDetails,
  reasoningAvailable,
  showReasoning,
}: {
  calls: ToolCall[];
  showDetails: boolean;
  reasoningAvailable: boolean;
  showReasoning: boolean;
}) {
  if (calls.length === 0 && !(showReasoning && reasoningAvailable)) return null;
  return (
    <section className="tool-activity" aria-label="Tool activity">
      <header>
        <span className="eyebrow">TOOL ACTIVITY</span>
        {showReasoning && (
          <small className="reasoning-availability">
            {reasoningAvailable
              ? "Reasoning available · content is not exposed by the runtime"
              : "No reasoning reported for this turn"}
          </small>
        )}
      </header>
      {calls.map((call) => (
        <ToolCard key={call.id} call={call} showDetails={showDetails} />
      ))}
    </section>
  );
});
