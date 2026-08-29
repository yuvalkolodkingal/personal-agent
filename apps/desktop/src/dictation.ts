import { invoke } from "@tauri-apps/api/core";

export type DictationMode = "natural" | "literal" | "code";
export type TokenStability = "stable" | "unstable";
export type EditStrategy =
  | "session_state"
  | "direct_text_api"
  | "accessibility"
  | "clipboard_paste"
  | "keystrokes"
  | "unsupported";

export type DictationToken = {
  text: string;
  stability: TokenStability;
};

export type PartialTranscript = {
  text: string;
  final_result: boolean;
  audio_end_ms?: number;
  received_at_ms: number;
};

export type Formatting =
  | { kind: "bold" }
  | { kind: "heading"; level: number }
  | { kind: "bulleted_list" }
  | { kind: "numbered_list" };

export type EditOperation =
  | {
      kind: "replace_provisional_tail";
      transaction_id: number;
      retain_utf16: number;
      delete_utf16: number;
      insert: string;
      stable_prefix_utf16: number;
    }
  | { kind: "commit_transaction"; transaction_id: number }
  | {
      kind: "delete_last_utterance";
      expected: string;
      utf16_len: number;
    }
  | { kind: "undo" }
  | {
      kind: "replace_literal";
      find: string;
      replacement: string;
      occurrence: "first" | "last" | "all";
    }
  | {
      kind: "insert_relative";
      anchor: string;
      text: string;
      before: boolean;
      occurrence: "first" | "last" | "all";
    }
  | { kind: "insert_text"; text: string }
  | {
      kind: "format_last_utterance";
      expected?: string | null;
      formatting: Formatting;
    }
  | { kind: "change_indent"; levels: number }
  | { kind: "set_mode"; mode: DictationMode };

export type DictationUpdate = {
  transaction_id: number;
  mode: DictationMode;
  tokens: DictationToken[];
  rendered_text: string;
  final_result: boolean;
  operations: EditOperation[];
};

export type EditReceipt = {
  transaction_id?: number;
  applied_at_ms: number;
  strategy: EditStrategy;
  verified: boolean;
};

export type LatencyDistribution = {
  p50_ms: number;
  p95_ms: number;
  maximum_ms: number;
  sample_count: number;
};

export type LatencyReport = {
  first_partial: LatencyDistribution;
  partial_updates: LatencyDistribution;
  finalization: LatencyDistribution;
  target_apply: LatencyDistribution;
};

export type DeterministicCommand =
  | { kind: "launch_application"; name: string }
  | { kind: "focus_application"; name: string }
  | { kind: "stop" }
  | { kind: "mute" }
  | { kind: "unmute" }
  | { kind: "sleep" }
  | { kind: "wake" }
  | { kind: "start_dictation" }
  | { kind: "stop_dictation" }
  | { kind: "set_dictation_mode"; mode: DictationMode };

export type VoiceRoute =
  | { route: "commands"; commands: DeterministicCommand[] }
  | { route: "agent_goal"; prompt: string }
  | { route: "dictation"; text: string };

export type RouteContext = "auto" | "command" | "dictation";

export type DictationTransport = (
  command: string,
  payload?: Record<string, unknown>,
) => Promise<unknown>;

export type ApplyResult = {
  receipts: EditReceipt[];
  rejected: Array<{ operation: EditOperation; reason: string }>;
};

export type NativeDictationAvailability =
  | "degraded"
  | "permission_required"
  | "unavailable";

export type NativeInputContract = {
  platform: string;
  session: string;
  adapter: string;
  availability: NativeDictationAvailability;
  review_before_insert: boolean;
  supports_text_insertion: boolean;
  supports_live_revisions: boolean;
  supports_verified_edits: boolean;
  detail: string;
  remediation?: string | null;
};

export type NativeDictationTarget = {
  application_id: string;
  title: string;
  window_id: string;
  secure: boolean;
};

export type NativeDictationPending = {
  transaction_id: number;
  text: string;
  final_result: boolean;
  kind: "insert" | "replace_last" | "undo_last";
  warning?: string | null;
  preview_latency_ms: number;
};

export type NativeDictationStatus = {
  contract: NativeInputContract;
  armed_target?: NativeDictationTarget | null;
  pending?: NativeDictationPending | null;
  undo_available: boolean;
  metrics: {
    last_apply_ms?: number | null;
    p95_apply_ms?: number | null;
    apply_samples: number;
  };
};

export type NativeDictationApplyResult = {
  submitted: boolean;
  verified: boolean;
  adapter: string;
  elapsed_ms: number;
  detail: string;
  status: NativeDictationStatus;
};

/**
 * Typed frontend boundary for the native dictation engine and focused-target adapter.
 * The module is deliberately independent of chat and wake capture; callers feed the partials
 * they already receive from Moonshine and decide whether the routed result is text or a goal.
 */
export class DictationClient {
  readonly #transport: DictationTransport;

  constructor(
    transport: DictationTransport = (command, payload) =>
      invoke(command, payload),
  ) {
    this.#transport = transport;
  }

  ingest(event: PartialTranscript): Promise<DictationUpdate> {
    return this.#transport("dictation_ingest", { event }) as Promise<DictationUpdate>;
  }

  async ingestAndApply(event: PartialTranscript): Promise<{
    update: DictationUpdate;
    application: ApplyResult;
  }> {
    const update = await this.ingest(event);
    const application = update.operations.length
      ? await this.apply(update.operations)
      : { receipts: [], rejected: [] };
    return { update, application };
  }

  apply(operations: EditOperation[]): Promise<ApplyResult> {
    return this.#transport("dictation_apply", { operations }) as Promise<ApplyResult>;
  }

  route(transcript: string, context: RouteContext): Promise<VoiceRoute> {
    return this.#transport("voice_route", {
      transcript,
      context,
    }) as Promise<VoiceRoute>;
  }

  latencyReport(): Promise<LatencyReport> {
    return this.#transport("dictation_latency_report") as Promise<LatencyReport>;
  }

  reset(mode: DictationMode = "natural"): Promise<void> {
    return this.#transport("dictation_reset", { mode }) as Promise<void>;
  }

  nativeStatus(): Promise<NativeDictationStatus> {
    return this.#transport(
      "native_dictation_status",
    ) as Promise<NativeDictationStatus>;
  }

  armNative(delayMs = 2_500): Promise<NativeDictationStatus> {
    return this.#transport("native_dictation_arm", {
      delayMs,
    }) as Promise<NativeDictationStatus>;
  }

  disarmNative(): Promise<NativeDictationStatus> {
    return this.#transport(
      "native_dictation_disarm",
    ) as Promise<NativeDictationStatus>;
  }

  stageNative(update: DictationUpdate): Promise<NativeDictationStatus> {
    return this.#transport("native_dictation_stage", {
      update,
    }) as Promise<NativeDictationStatus>;
  }

  discardNative(): Promise<NativeDictationStatus> {
    return this.#transport(
      "native_dictation_discard",
    ) as Promise<NativeDictationStatus>;
  }

  confirmNative(delayMs = 2_500): Promise<NativeDictationApplyResult> {
    return this.#transport("native_dictation_confirm", {
      confirmed: true,
      delayMs,
    }) as Promise<NativeDictationApplyResult>;
  }

  undoNative(delayMs = 2_500): Promise<NativeDictationApplyResult> {
    return this.#transport("native_dictation_undo", {
      confirmed: true,
      delayMs,
    }) as Promise<NativeDictationApplyResult>;
  }
}

type ProvisionalTransaction = {
  start: number;
  text: string;
  undoSnapshot: string;
};

/**
 * Verified in-app fallback target for the chat composer.
 *
 * JavaScript string offsets are UTF-16 units, matching the native edit protocol. Native desktop
 * adapters can replace this target later without changing recognition or correction semantics.
 */
export class InAppDictationBuffer {
  #value: string;
  readonly #transactions = new Map<number, ProvisionalTransaction>();
  readonly #undo: string[] = [];

  constructor(initialValue = "") {
    this.#value = initialValue;
  }

  get value() {
    return this.#value;
  }

  /** Synchronize explicit keyboard edits and invalidate stale provisional ranges. */
  sync(value: string) {
    if (value === this.#value) return;
    this.#value = value;
    this.#transactions.clear();
    this.#undo.length = 0;
  }

  apply(operations: EditOperation[]) {
    for (const operation of operations) this.#applyOne(operation);
    return this.#value;
  }

  /** Remove an uncommitted command phrase when routing changes away from dictation. */
  cancelProvisional() {
    const transaction = [...this.#transactions.values()].at(-1);
    if (transaction) this.#value = transaction.undoSnapshot;
    this.#transactions.clear();
    return this.#value;
  }

  #applyOne(operation: EditOperation) {
    switch (operation.kind) {
      case "replace_provisional_tail": {
        const transaction = this.#transactions.get(operation.transaction_id) ?? {
          start: this.#value.length,
          text: "",
          undoSnapshot: this.#value,
        };
        const retained = transaction.text.slice(0, operation.retain_utf16);
        const afterDeleted = transaction.text.slice(
          operation.retain_utf16 + operation.delete_utf16,
        );
        const revised = retained + operation.insert + afterDeleted;
        this.#value =
          this.#value.slice(0, transaction.start) +
          revised +
          this.#value.slice(transaction.start + transaction.text.length);
        transaction.text = revised;
        this.#transactions.set(operation.transaction_id, transaction);
        break;
      }
      case "commit_transaction": {
        const transaction = this.#transactions.get(operation.transaction_id);
        if (transaction && transaction.undoSnapshot !== this.#value)
          this.#undo.push(transaction.undoSnapshot);
        this.#transactions.delete(operation.transaction_id);
        break;
      }
      case "delete_last_utterance": {
        this.#rememberUndo();
        const start = this.#value.lastIndexOf(operation.expected);
        if (start >= 0)
          this.#value =
            this.#value.slice(0, start) +
            this.#value.slice(start + operation.utf16_len);
        break;
      }
      case "undo": {
        const prior = this.#undo.pop();
        if (prior !== undefined) this.#value = prior;
        this.#transactions.clear();
        break;
      }
      case "replace_literal": {
        this.#rememberUndo();
        this.#value = replaceOccurrence(
          this.#value,
          operation.find,
          operation.replacement,
          operation.occurrence,
        );
        break;
      }
      case "insert_relative": {
        this.#rememberUndo();
        const addition = operation.before
          ? `${operation.text} ${operation.anchor}`
          : `${operation.anchor} ${operation.text}`;
        this.#value = replaceOccurrence(
          this.#value,
          operation.anchor,
          addition,
          operation.occurrence,
        );
        break;
      }
      case "insert_text":
        this.#rememberUndo();
        this.#value += operation.text;
        break;
      case "format_last_utterance": {
        const expected = operation.expected ?? "";
        if (!expected) break;
        this.#rememberUndo();
        this.#value = replaceOccurrence(
          this.#value,
          expected,
          markdownFormat(expected, operation.formatting),
          "last",
        );
        break;
      }
      case "change_indent": {
        this.#rememberUndo();
        const lineStart = this.#value.lastIndexOf("\n") + 1;
        const line = this.#value.slice(lineStart);
        if (operation.levels > 0)
          this.#value =
            this.#value.slice(0, lineStart) +
            "    ".repeat(operation.levels) +
            line;
        else {
          const remove = Math.min(
            line.length - line.trimStart().length,
            Math.abs(operation.levels) * 4,
          );
          this.#value = this.#value.slice(0, lineStart) + line.slice(remove);
        }
        break;
      }
      case "set_mode":
        break;
    }
  }

  #rememberUndo() {
    if (this.#undo.at(-1) !== this.#value) this.#undo.push(this.#value);
  }
}

function replaceOccurrence(
  value: string,
  find: string,
  replacement: string,
  occurrence: "first" | "last" | "all",
) {
  if (!find) return value;
  if (occurrence === "all") return value.split(find).join(replacement);
  const start =
    occurrence === "first" ? value.indexOf(find) : value.lastIndexOf(find);
  return start < 0
    ? value
    : value.slice(0, start) + replacement + value.slice(start + find.length);
}

function markdownFormat(value: string, formatting: Formatting) {
  switch (formatting.kind) {
    case "bold":
      return `**${value}**`;
    case "heading":
      return `${"#".repeat(Math.max(1, Math.min(6, formatting.level)))} ${value.trimStart()}`;
    case "bulleted_list":
      return value
        .split("\n")
        .map((line) => `- ${line}`)
        .join("\n");
    case "numbered_list":
      return value
        .split("\n")
        .map((line, index) => `${index + 1}. ${line}`)
        .join("\n");
  }
}

/** Monotonic transcript event helper shared by push-to-talk and continuous dictation. */
export function transcriptEvent(
  text: string,
  finalResult: boolean,
  audioEndMs?: number,
  now: () => number = () => performance.now(),
): PartialTranscript {
  return {
    text,
    final_result: finalResult,
    audio_end_ms: audioEndMs,
    received_at_ms: Math.round(now()),
  };
}
