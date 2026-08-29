# ADR-0002: Standalone agent runtime (fork, then native engine)

- Status: proposed
- Date: 2026-08-29
- Supersedes: the "upstream OpenCode is not a fork" exclusion in SPEC.md §9
- Amends: ADR-0001 (pinned OpenCode sidecar behind `AgentRuntime`)

## Context

ADR-0001 chose a pinned upstream OpenCode 1.18.23 sidecar and SPEC.md §9 explicitly excluded
forking. A file-level audit on 2026-08-29 shows the consequences of that boundary:

- The sidecar owns model calls, the tool loop, sessions, MCP server processes, and PTYs. The
  native side has no LLM client at all; it is an orchestration and safety shell.
- PTY sessions cannot survive a sidecar restart (`apps/desktop/src-tauri/src/pty_host.rs`), so
  "persistent terminal" is bounded by a process we do not control.
- Structured plan submission and per-session environment variables are rejected by the adapter
  because the pinned API does not support them
  (`crates/runtime/src/lib.rs:1819-1823`, `:1672-1677`), which caps the planner at a linear
  one-task-per-criterion chain.
- The V1 `tool.execute.before` hook cannot reliably short-circuit a call (REJ-010), so reviewed
  coding tools rely on the pinned permission engine plus native preauthorization rather than the
  full checkpoint/postcondition/rollback pipeline every native tool gets.
- Session durability, compaction policy, and provider failover semantics are all defined by
  upstream release decisions.

These are not defects in OpenCode; they are the cost of delegating the runtime.

## Decision

Replace the runtime using a strangler pattern behind the existing `AgentRuntime` trait, which is
already the only seam the application depends on.

1. **Fork for supply-chain ownership.** Fork OpenCode at v1.18.23, build artifacts in the fork's
   CI, publish a SHA-256 manifest, and point `scripts/fetch-opencode.ts` at it. Carry only
   minimal, versioned, upstream-issue-linked patches (permission names, hook short-circuit,
   per-session environment).
2. **Build a native engine in parallel.** `crates/llm` (Anthropic Messages + OpenAI-compatible
   streaming clients), `crates/engine` (session store, agent loop, compaction, subagents),
   `crates/coding-tools` (native read/write/edit/patch/grep/bash/task with checkpoint and
   transactional rollback through the existing `ToolGateway`), and native PTYs via `portable-pty`.
3. **Select at runtime.** `config.runtime.engine = "sidecar" | "native"`, defaulting to `sidecar`
   until the parity gate passes.
4. **Cut over on evidence.** Flip the default only when the parity suite is green, replay
   performance is at or better than the sidecar baseline, the prompt-injection red-team suite
   passes against native tools, and a 14-day restart soak shows no data loss. The forked sidecar
   remains selectable for two releases afterward.

## Consequences

Positive: we own provider integration, tool-loop correctness, session durability across restarts,
and checkpoint/rollback for every consequential tool — which closes the threat-model residual that
the compatibility surface "does not yet provide the full checkpoint, postcondition and rollback
guarantees of a native Personal Agent tool". Structured plans and real delegation become possible.

Negative: we maintain provider clients and a tool loop, and must track provider API changes
ourselves. Two runtimes coexist during transition, so the acceptance matrix runs twice.

Mitigation: the `AgentRuntime` trait, `FakeRuntime`, the local OpenAI-compatible fixture provider,
and the parity acceptance suite make the change reversible per-user through configuration rather
than through a rebuild.

## Sources

- ADR-0001 (`docs/architecture/adr-0001-opencode-sidecar.md`)
- `rejections.yaml` REJ-010
- SPEC-V2.md workstream F (RUN-0 … RUN-5)
