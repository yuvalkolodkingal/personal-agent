import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { UsageEgress, type UsageSnapshot } from "./UsageEgress";

const aggregate = {
  provider_steps: 75,
  tokens: {
    input: 75,
    output: 75,
    reasoning: 0,
    cache_read: 0,
    cache_write: 0,
    total: 150,
  },
  reported_cost_microusd: 75,
  unknown_cost_steps: 0,
  tool_calls: 0,
  egress_events: 0,
  known_egress_bytes: 0,
  unknown_egress_sizes: 0,
  providers: ["test-provider"],
  models: ["test-model"],
};

function page(offset: number, total = 75, provider = "test-provider"): UsageSnapshot {
  const pageNumber = offset / 50 + 1;
  return {
    records: [{
      id: `usage-${pageNumber}`,
      at: "2026-08-30T10:00:00Z",
      day_utc: "2026-08-30",
      session_id: "session-1",
      turn_id: `turn-${pageNumber}`,
      scope_key: "session:session-1",
      provider_id: provider,
      model_id: `model-page-${pageNumber}`,
      tokens: {
        input: 1,
        output: 1,
        reasoning: 0,
        cache_read: 0,
        cache_write: 0,
        total: 2,
      },
      cost: { microusd: 1, status: "provider_reported" },
    }],
    egress: [],
    turns: {},
    sessions: {},
    days: { "2026-08-30": aggregate },
    scopes: {},
    usage_total: total,
    egress_total: 0,
    limit: 50,
    offset,
    pricing_policy: "Provider reported only.",
  };
}

describe("usage and egress pagination", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation((command: string, args?: { offset?: number; provider?: string | null }) => {
      if (command === "usage_snapshot") {
        return Promise.resolve(args?.provider
          ? page(args.offset ?? 0, 1, args.provider)
          : page(args?.offset ?? 0));
      }
      return Promise.resolve({});
    });
  });

  afterEach(() => cleanup());

  it("requests bounded pages and advances to the next provider-usage page", async () => {
    render(<UsageEgress />);

    expect(await screen.findByText("model-page-1")).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("usage_snapshot", {
      limit: 50,
      offset: 0,
      from: null,
      to: null,
      provider: null,
      model: null,
      session: null,
      source: null,
    });
    expect(screen.getByText("1–50 of 75")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Next usage page" }));

    expect(await screen.findByText("model-page-2")).toBeInTheDocument();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("usage_snapshot", {
      limit: 50,
      offset: 50,
      from: null,
      to: null,
      provider: null,
      model: null,
      session: null,
      source: null,
    }));
    expect(screen.getByText("51–75 of 75")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Next usage page" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Previous usage page" })).toBeEnabled();

    fireEvent.change(screen.getByPlaceholderText("All providers"), {
      target: { value: "needle-provider" },
    });

    await waitFor(() => expect(invoke).toHaveBeenCalledWith("usage_snapshot", {
      limit: 50,
      offset: 0,
      from: null,
      to: null,
      provider: "needle-provider",
      model: null,
      session: null,
      source: null,
    }));
    expect(await screen.findByText("needle-provider")).toBeInTheDocument();
    expect(screen.getByText("1–1 of 1")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Previous usage page" })).toBeDisabled();
  });
});
