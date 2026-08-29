# Codex implementation prompt — Personal Agent SPEC-V2

Copy everything below the line into Codex as the opening prompt.

---

You are implementing `SPEC-V2.md` in the repository at
`/home/yuval/Documents/GitHub/personal-agent`. Work continuously and autonomously,
using parallel subagents, until every task is done or genuinely blocked.

## Mission

`SPEC-V2.md` contains 87 numbered tasks across nine workstreams. Each task has
**Files**, **Do**, and **Done when**. Implement them in the dependency order given in
SPEC-V2 §12. Your success metric is *tasks actually completed and verified*, not tasks
attempted. A task you cannot finish is reported as BLOCKED with a reason — never as done.

## Before you start

1. **Read, in this order:** `SPEC-V2.md` §0 (execution rules), §1 (invariants), §2
   (current-state map), §12 (execution order), §14 (verification commands). Then
   `SPEC.md` §6 (security/autonomy), `docs/architecture/adr-0001-opencode-sidecar.md`,
   `docs/architecture/adr-0002-standalone-runtime.md`, `rejections.yaml`.
2. **Baseline the working tree.** It is currently dirty with substantial uncommitted work
   (modified crates, untracked `src-tauri/src/*.rs` files that SPEC-V2 treats as existing).
   Run `git status`, then commit the current state on a branch named `pre-spec-v2-baseline`
   so your changes are separable. Then create and work on branch `spec-v2`.
3. **Confirm the toolchain.** `rustc --version` must be 1.98.0; `bun --version` 1.4.0.
   For task A-1 you need `rustup target add x86_64-pc-windows-msvc`. Note which of these
   succeed — network-dependent steps may fail in your sandbox and that is a BLOCKED
   condition, not a reason to skip the task's code changes.
4. **Establish a green baseline** before changing anything: run the full gate from §14 and
   record which commands already fail. You are not responsible for pre-existing failures,
   but you must not add new ones, and you must report the baseline in your final summary.

## Non-negotiable rules

- **Never break an invariant in SPEC-V2 §1.** If a task appears to require it, stop and
  report instead of proceeding.
- **Never fabricate completion.** A task is DONE only if you actually ran its "Done when"
  command and it passed. Paste the real command output in your report.
- **Never weaken a test to make it pass.** If an existing test is genuinely wrong, fix it
  and say so explicitly with justification. Do not delete, skip, `#[ignore]`, or loosen an
  assertion to get green.
- **Never narrow scope silently.** If you implement part of a task, report it as PARTIAL
  with exactly what is missing.
- **Do not `git push`, open PRs, or touch anything outside this repository.**
- **Decisions in SPEC-V2 are locked.** Do not re-open them, substitute libraries, or
  survey alternatives. If a locked choice is impossible (crate does not exist, API
  changed), report BLOCKED with the evidence rather than improvising a different design.
- **Pin every new dependency to an exact version** (`=x.y.z`), matching the existing
  workspace convention. `unsafe_code = "forbid"` stays workspace-wide.

## Subagent protocol

Run one subagent per task. The orchestrator (you) assigns tasks, enforces file ownership,
runs the wave gate, and commits. The orchestrator does not edit files itself while
subagents are live.

**Task packet — give every subagent exactly this:**

```
Repository: /home/yuval/Documents/GitHub/personal-agent   Branch: spec-v2
Task: <TASK-ID> from SPEC-V2.md

1. Read SPEC-V2.md §1 (invariants) and the full text of task <TASK-ID>.
2. Read every file listed in that task's "Files" line, at the cited line anchors.
3. Implement the "Do" section exactly. Locked decisions may not be substituted.
4. Satisfy the "Done when" condition and run it. Paste real output.
5. You own ONLY these files: <explicit list>. Do not edit any other file.
   If the task cannot be completed without editing a file you do not own, stop and
   report that as a blocker.
6. Pin any new dependency to an exact version.
7. Do not weaken or skip tests. Do not commit — the orchestrator commits.

Report back: DONE | PARTIAL | BLOCKED, files changed, the "Done when" command and its
verbatim output, and anything you discovered that contradicts SPEC-V2.
```

**File ownership.** Two subagents must never own the same file in the same wave. These
seven files are contended by many tasks — serialize any wave that would touch one twice:

- `apps/desktop/src-tauri/src/api.rs` (2,527 lines; PERF-4/10/11/12, STT-1/3, TTS-1/2/3)
- `apps/desktop/src/App.tsx` (3,683 lines; A-5, PERF-4/13/14, TTS-3/5, FIX-10…17)
- `crates/runtime/src/lib.rs` (3,299 lines; A-3, PERF-10, RUN-0/2)
- `crates/storage/src/lib.rs` (PERF-2/6/7, FIX-8/9)
- `crates/core/src/lib.rs` (PERF-2/7)
- `apps/desktop/src-tauri/src/main.rs` (A-6, PERF-1/3)
- `scripts/voice-runtime.py` (STT-1/2/3, TTS-1/4, FIX-7)

When a wave contends on one of these, split it: run the file's owner alone, then fan out
the rest.

## Wave plan

Follow SPEC-V2 §12 exactly. Waves 1–3 are spelled out; derive the rest from §12 using the
ownership rule above.

**Wave 1 — 8 subagents in parallel.** A-1 … A-8 touch eight disjoint files:
`portal_stub.rs`+CI · `opencode-plugin/src/index.ts` · `crates/runtime/src/lib.rs` ·
`crates/tools/src/lib.rs` · `App.tsx` · `main.rs` · `capabilities.rs` ·
`connector_oauth.rs`. This wave unblocks non-Linux CI — do it first.

**Wave 2 — 2 subagents.** PERF-1 (measurement; needs A-6's `main.rs` merged first) and
RUN-0 (fork + supply chain). RUN-0 needs a GitHub account, a fork, and fork CI — you
almost certainly cannot complete it. Do the part you can: prepare the four patches as a
reviewable series in `patches/opencode/`, write the `fetch-opencode.ts` change behind a
config switch, and report BLOCKED-EXTERNAL with precise handoff steps.

**Wave 3 — 4 subagents.** PERF-2 (storage+core+goals_host) · PERF-3 (main/native_desktop/
capabilities/mcp_host) · PERF-4 (api.rs+App.tsx) · FIX-27 (policy/tools tests).

**Waves 4+ —** continue down §12. Before each wave, list its tasks, map each to its file
set, and split on contention. Announce the wave plan before dispatching.

## Per-task loop

For each task: read anchors → implement → run the task's "Done when" → run the fast gate →
report. The orchestrator then commits with `<type>: <TASK-ID> <summary>` (e.g.
`fix: A-3 tokenize destructive command matching`), one commit per task, and moves on.

**Fast gate** (per task — targeted, seconds not minutes):
- Rust: `cargo check -p <crate>` then `cargo test -p <crate> <filter>`
- TypeScript: `bun run --cwd apps/desktop test <file>` and `bun run check`

**Wave gate** (after every wave — the full §14 set):
```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
bun run check && bun run test && bun run --cwd apps/desktop build
python scripts/verify-registries.py && python scripts/verify-packs.py
python scripts/verify-performance.py && python scripts/verify-fuzz.py
python scripts/security-gate.py
python scripts/generate-release-metadata.py --check
```
A wave is not complete until this passes (minus pre-existing baseline failures you
recorded at the start). If a wave gate fails, fix it before starting the next wave.

## Blocked-task protocol

Four legitimate blockers. Mark the task BLOCKED, do everything you *can* do, commit that,
and record the exact command a human must run:

1. **BLOCKED-EXTERNAL** — needs an account, credential, or third-party service (RUN-0's
   fork; signing; Microsoft/Slack app registration in INT-5).
2. **BLOCKED-NETWORK** — needs a download (`bun run sidecar:fetch`, model weights for
   STT-3/TTS-4/FIX-7, Chromium for BROW-1). Implement the code and pin the hashes; gate
   the test behind the existing env-var pattern.
3. **BLOCKED-HARDWARE** — needs display, audio device, or GPU (DESK-1 portal frames,
   DESK-2/3 live AT-SPI and input, voice smoke tests). SPEC-V2 §14 lists the env-gated
   commands; write the test, gate it, report the command.
4. **BLOCKED-CONTRADICTION** — SPEC-V2 is wrong about the code. Report the anchor, what
   you actually found, and your recommendation. Do not guess.

Never convert a blocker into a silent skip or a weakened assertion.

## Reporting

Keep a running log at `docs/operations/spec-v2-progress.md`, updated after every wave, and
end your session with this table:

| Task | Status | Evidence | Commit |
|---|---|---|---|
| A-1 | DONE | `cargo check --target x86_64-pc-windows-msvc` → 0 errors | `abc1234` |
| RUN-0 | BLOCKED-EXTERNAL | needs GitHub fork; patches staged in `patches/opencode/` | `def5678` |

Then state plainly: how many tasks are DONE / PARTIAL / BLOCKED, whether the full gate is
green, which pre-existing failures you inherited, and the single next task to pick up.

If you run out of context or time, **stop at a wave boundary** with everything committed
and the gate green, and report exactly where you stopped. Do not leave the tree broken and
do not claim more progress than the evidence supports.

## If you cannot spawn parallel subagents

Execute the identical wave order sequentially, task by task, with the same per-task loop,
gates, commit discipline, and reporting. The ordering in §12 is what protects correctness;
parallelism is only a speed optimization.
