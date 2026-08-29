# OpenCode v1.18.23 fork patch series

Apply the reviewable commits, in order, to upstream tag `v1.18.23`
(`ef2880f`):

```sh
git checkout -b personal-agent-v1.18.23 v1.18.23
git am /path/to/0001-granular-permission-names.patch
git am /path/to/0003-per-session-environment.patch
```

Each commit message links the related upstream issue. The first patch preserves
the `write` and `apply_patch` permission names. The third accepts a private
per-session environment on session creation and applies it only to that
session's shell processes.

Patch 2 is not carried. See
`0002-before-hook-short-circuit-blocked.md`: pristine v1.18.23 already turns
both synchronous hook throws and asynchronous rejections into a failing Effect,
and every relevant executor yields that Effect before invoking the tool. The
proposed source rewrite is behaviorally equivalent, so this part of RUN-0 is
`BLOCKED-CONTRADICTION` rather than a no-op fork patch.

Patch 4 is intentionally not carried. See
`0004-pty-reattach-skipped.md`: the v1.18.23 PTY and its process are both owned
by an in-memory service and destroyed on server teardown, so persisting only a
reattach token would falsely advertise recovery for a process that no longer
exists. SPEC-V2 explicitly permits skipping this impractical patch because
native PTY work in RUN-3 supersedes it.

## Fork release handoff

The fork source stays fail-closed until its release exists. A maintainer with a
working GitHub credential must:

1. Fork `anomalyco/opencode` to `yuvalkolodkingal/opencode`.
2. Apply the two source patches above at tag `v1.18.23` and push the branch/tag.
3. Run the fork's publish workflow for Linux x64/arm64, macOS x64/arm64, and
   Windows x64/arm64; require every build and focused test to pass.
4. Publish release `v1.18.23` with the six archives and a SHA-256 manifest.
5. Copy those six real hashes into
   `docs/operations/opencode-1.18.23.json`, verify the authenticated `/doc`
   fingerprint against the recorded fork value, then set `fork.ready` to
   `true`.
6. Run `PERSONAL_AGENT_OPENCODE_SOURCE=fork bun run sidecar:fetch` for every
   target in CI, followed by `cargo test --workspace --locked`.

Do not set `fork.ready` before the release archives and hashes exist.
