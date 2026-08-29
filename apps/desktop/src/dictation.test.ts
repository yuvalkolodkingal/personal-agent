import { describe, expect, it, vi } from "vitest";
import {
  DictationClient,
  InAppDictationBuffer,
  transcriptEvent,
  type DictationTransport,
  type DictationUpdate,
} from "./dictation";

const update: DictationUpdate = {
  transaction_id: 3,
  mode: "natural",
  tokens: [
    { text: "hello", stability: "stable" },
    { text: "world", stability: "unstable" },
  ],
  rendered_text: "hello world",
  final_result: false,
  operations: [
    {
      kind: "replace_provisional_tail",
      transaction_id: 3,
      retain_utf16: 6,
      delete_utf16: 3,
      insert: "world",
      stable_prefix_utf16: 6,
    },
  ],
};

describe("DictationClient", () => {
  it("passes monotonic partial metadata to the native engine", async () => {
    const transport = vi.fn<DictationTransport>().mockResolvedValue(update);
    const client = new DictationClient(transport);
    const event = transcriptEvent("hello world", false, 840, () => 900.4);

    await expect(client.ingest(event)).resolves.toBe(update);
    expect(transport).toHaveBeenCalledWith("dictation_ingest", {
      event: {
        text: "hello world",
        final_result: false,
        audio_end_ms: 840,
        received_at_ms: 900,
      },
    });
  });

  it("applies every operation only after native ingestion succeeds", async () => {
    const transport = vi.fn<DictationTransport>(async (command) => {
      if (command === "dictation_ingest") return update as never;
      if (command === "dictation_apply") {
        return {
          receipts: [
            {
              transaction_id: 3,
              applied_at_ms: 920,
              strategy: "direct_text_api",
              verified: true,
            },
          ],
          rejected: [],
        } as never;
      }
      throw new Error(`unexpected ${command}`);
    });
    const result = await new DictationClient(transport).ingestAndApply(
      transcriptEvent("hello world", false, 840, () => 900),
    );

    expect(result.application.receipts[0]?.verified).toBe(true);
    expect(transport).toHaveBeenNthCalledWith(2, "dictation_apply", {
      operations: update.operations,
    });
  });

  it("does not invoke the focused target when a partial has no edit", async () => {
    const unchanged = { ...update, operations: [] };
    const transport = vi.fn<DictationTransport>().mockResolvedValue(unchanged);
    const result = await new DictationClient(transport).ingestAndApply(
      transcriptEvent("hello world", false, 840, () => 900),
    );

    expect(result.application).toEqual({ receipts: [], rejected: [] });
    expect(transport).toHaveBeenCalledTimes(1);
  });

  it("keeps deterministic commands separate from model-planned goals", async () => {
    const transport = vi
      .fn<DictationTransport>()
      .mockResolvedValueOnce({
        route: "commands",
        commands: [{ kind: "launch_application", name: "vscode" }],
      })
      .mockResolvedValueOnce({
        route: "agent_goal",
        prompt: "run the tests and explain the failure",
      });
    const client = new DictationClient(transport);

    await expect(client.route("launch vscode", "auto")).resolves.toMatchObject({
      route: "commands",
    });
    await expect(
      client.route("run the tests and explain the failure", "command"),
    ).resolves.toMatchObject({ route: "agent_goal" });
  });

  it("uses explicit confirmation and a focus-switch delay for native apply and undo", async () => {
    const nativeStatus = {
      contract: {
        platform: "linux",
        session: "wayland",
        adapter: "wtype",
        availability: "degraded",
        review_before_insert: true,
        supports_text_insertion: true,
        supports_live_revisions: true,
        supports_verified_edits: false,
        detail: "unverified keystrokes",
      },
      armed_target: null,
      pending: null,
      undo_available: false,
      metrics: { apply_samples: 0 },
    } as const;
    const transport = vi.fn<DictationTransport>().mockResolvedValue({
      submitted: true,
      verified: false,
      adapter: "wtype",
      elapsed_ms: 4,
      detail: "verify visually",
      status: nativeStatus,
    });
    const client = new DictationClient(transport);

    await client.confirmNative(3_000);
    await client.undoNative(1_500);

    expect(transport).toHaveBeenNthCalledWith(1, "native_dictation_confirm", {
      confirmed: true,
      delayMs: 3_000,
    });
    expect(transport).toHaveBeenNthCalledWith(2, "native_dictation_undo", {
      confirmed: true,
      delayMs: 1_500,
    });
  });
});

describe("InAppDictationBuffer", () => {
  it("revises a provisional UTF-16 suffix and commits one undoable turn", () => {
    const target = new InAppDictationBuffer("Draft: ");
    target.apply([
      {
        kind: "replace_provisional_tail",
        transaction_id: 1,
        retain_utf16: 0,
        delete_utf16: 0,
        insert: "hello wur",
        stable_prefix_utf16: 6,
      },
      {
        kind: "replace_provisional_tail",
        transaction_id: 1,
        retain_utf16: 7,
        delete_utf16: 2,
        insert: "orld",
        stable_prefix_utf16: 6,
      },
      { kind: "commit_transaction", transaction_id: 1 },
    ]);
    expect(target.value).toBe("Draft: hello world");
    target.apply([{ kind: "undo" }]);
    expect(target.value).toBe("Draft: ");
  });

  it("applies corrections, relative inserts, formatting and indentation", () => {
    const target = new InAppDictationBuffer("Project color\nimportant");
    expect(
      target.apply([
        {
          kind: "replace_literal",
          find: "color",
          replacement: "colour",
          occurrence: "last",
        },
        {
          kind: "insert_relative",
          anchor: "important",
          text: "very",
          before: true,
          occurrence: "last",
        },
        { kind: "change_indent", levels: 1 },
      ]),
    ).toBe("Project colour\n    very important");
    expect(
      target.apply([
        {
          kind: "format_last_utterance",
          expected: "very important",
          formatting: { kind: "bold" },
        },
      ]),
    ).toBe("Project colour\n    **very important**");
  });

  it("drops stale provisional ranges after an explicit keyboard edit", () => {
    const target = new InAppDictationBuffer("one");
    target.apply([
      {
        kind: "replace_provisional_tail",
        transaction_id: 1,
        retain_utf16: 0,
        delete_utf16: 0,
        insert: " two",
        stable_prefix_utf16: 4,
      },
    ]);
    target.sync("manually replaced");
    target.apply([
      {
        kind: "replace_provisional_tail",
        transaction_id: 2,
        retain_utf16: 0,
        delete_utf16: 0,
        insert: " safely",
        stable_prefix_utf16: 7,
      },
    ]);
    expect(target.value).toBe("manually replaced safely");
  });

  it("removes an uncommitted command phrase before switching modes", () => {
    const target = new InAppDictationBuffer("Keep this");
    target.apply([
      {
        kind: "replace_provisional_tail",
        transaction_id: 8,
        retain_utf16: 0,
        delete_utf16: 0,
        insert: " stop dictation",
        stable_prefix_utf16: 5,
      },
    ]);
    expect(target.cancelProvisional()).toBe("Keep this");
  });
});
