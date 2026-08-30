import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import {
  describeToolDuration,
  reduceToolEvent,
  ToolActivity,
  type ToolCall,
} from "./ToolActivity";
import type { EventEnvelope } from "./types";

const event = (
  type: string,
  payload: Record<string, unknown>,
  at: string,
): EventEnvelope => ({
  schema_version: 1,
  event_id: `${type}-${at}`,
  wall_clock_timestamp: at,
  monotonic_sequence: 1,
  origin: "fixture",
  profile_id: "default",
  type,
  payload_json: Array.from(new TextEncoder().encode(JSON.stringify(payload))),
});

afterEach(() => cleanup());

describe("tool activity", () => {
  it("pairs a started and completed event into one card with a measured duration", () => {
    let calls: ToolCall[] = [];
    calls = reduceToolEvent(
      calls,
      event(
        "tool.started",
        { callID: "c1", tool: "bash", status: "running" },
        "2026-08-30T10:00:00.000Z",
      ),
    );
    expect(calls).toHaveLength(1);
    expect(describeToolDuration(calls[0]!)).toBe("in progress");
    calls = reduceToolEvent(
      calls,
      event(
        "tool.completed",
        { callID: "c1", status: "completed" },
        "2026-08-30T10:00:00.420Z",
      ),
    );
    expect(calls).toHaveLength(1);
    expect(calls[0]).toMatchObject({
      id: "c1",
      name: "bash",
      status: "completed",
      detailSource: "none",
    });
    expect(describeToolDuration(calls[0]!)).toBe("420 ms");
  });

  it("marks a failed call and keeps the tool name reported at start", () => {
    let calls = reduceToolEvent(
      [],
      event(
        "tool.started",
        { callID: "c2", tool: "webfetch" },
        "2026-08-30T10:00:00.000Z",
      ),
    );
    calls = reduceToolEvent(
      calls,
      event("tool.failed", { callID: "c2" }, "2026-08-30T10:00:12.000Z"),
    );
    expect(calls[0]).toMatchObject({ name: "webfetch", status: "failed" });
    expect(describeToolDuration(calls[0]!)).toBe("12.0 s");
  });

  it("shows gateway-supplied arguments and results when the boundary carries them", () => {
    const calls = reduceToolEvent(
      [],
      event(
        "tool.completed",
        {
          callID: "c3",
          tool: "read",
          arguments: { path: "/etc/hosts" },
          result: "127.0.0.1 localhost",
        },
        "2026-08-30T10:00:00.000Z",
      ),
    );
    expect(calls[0]?.detailSource).toBe("gateway");
    render(
      <ToolActivity
        calls={calls}
        showDetails
        reasoningAvailable={false}
        showReasoning={false}
      />,
    );
    expect(screen.getByText(/"path": "\/etc\/hosts"/)).toBeInTheDocument();
    expect(screen.getByText("127.0.0.1 localhost")).toBeInTheDocument();
    expect(
      screen.queryByText(/discarded at the runtime boundary/),
    ).toBeNull();
  });

  it("renders nothing when there is no tool activity and no reasoning signal", () => {
    const { container } = render(
      <ToolActivity
        calls={[]}
        showDetails
        reasoningAvailable={false}
        showReasoning
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("ignores events that are not tool events", () => {
    expect(
      reduceToolEvent(
        [],
        event("response.delta", { delta: "x" }, "2026-08-30T10:00:00.000Z"),
      ),
    ).toEqual([]);
  });
});
