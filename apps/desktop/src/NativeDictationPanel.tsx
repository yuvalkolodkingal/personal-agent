import type { NativeDictationStatus } from "./dictation";
import "./nativeDictation.css";

export function NativeDictationPanel({
  status,
  arming,
  onArm,
  onDisarm,
  onApply,
  onDiscard,
  onUndo,
}: {
  status: NativeDictationStatus | null;
  arming: boolean;
  onArm: () => void;
  onDisarm: () => void;
  onApply: () => void;
  onDiscard: () => void;
  onUndo: () => void;
}) {
  const contract = status?.contract;
  const target = status?.armed_target;
  const pending = status?.pending;
  const available = contract?.supports_text_insertion === true;

  return (
    <section className="native-dictation-panel" aria-label="Focused app dictation">
      <header>
        <div>
          <strong>Focused app</strong>
          <span className={`native-capability ${contract?.availability ?? "checking"}`}>
            {contract?.availability.replaceAll("_", " ") ?? "checking"}
          </span>
        </div>
        <small>
          {target
            ? `Armed: ${target.title} · ${target.application_id}`
            : "Review-first · text is never injected before Apply"}
        </small>
      </header>

      {!target && (
        <div className="native-arm-row">
          <p>
            {contract?.detail ?? "Checking the native input adapter…"}
            {contract?.remediation ? ` ${contract.remediation}` : ""}
          </p>
          <button type="button" disabled={!available || arming} onClick={onArm}>
            {arming ? "Switch to target…" : "Arm in 3 seconds"}
          </button>
        </div>
      )}

      {target && !pending && (
        <div className="native-arm-row">
          <p>
            Switch back to <strong>{target.title}</strong>, start the microphone, and
            dictate. Focus changes are blocked automatically.
          </p>
          <div className="native-actions">
            <button type="button" disabled={!status?.undo_available || arming} onClick={onUndo}>
              Undo last
            </button>
            <button type="button" disabled={arming} onClick={onDisarm}>Disarm</button>
          </div>
        </div>
      )}

      {target && pending && (
        <div className="native-review" role="status">
          <div>
            <span>{pending.final_result ? "READY TO APPLY" : "LIVE PREVIEW"}</span>
            <small>{pending.kind.replaceAll("_", " ")}</small>
          </div>
          <p>{pending.kind === "undo_last" ? "Undo the last dictated insertion" : pending.text}</p>
          {pending.warning && <em>{pending.warning}</em>}
          <div className="native-actions">
            <button type="button" disabled={arming} onClick={onDiscard}>Discard</button>
            <button
              type="button"
              className="primary"
              disabled={!pending.final_result || arming}
              onClick={onApply}
            >
              {arming ? "Switch back to target…" : `Apply in 3s · ${target.application_id}`}
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
