# Patch 4 skipped: PTY reattach across server restart

Status: skipped under RUN-0's explicit “if impractical” clause.

Related upstream reports:

- https://github.com/anomalyco/opencode/issues/30597
- https://github.com/anomalyco/opencode/issues/6004

In v1.18.23, `packages/core/src/pty.ts` keeps every `Active` PTY in a
process-local `Map`. Its layer finalizer calls `teardown` for every entry,
killing each still-running native process and clearing the map. Reattach tickets
in `packages/core/src/pty/ticket.ts` are also process-local, single-use cache
entries, but preserving a ticket alone cannot preserve or recover the killed
PTY process, its file descriptors, or its output buffer.

A truthful implementation therefore needs an external PTY owner plus a durable
authenticated rendezvous protocol, not a minimal token patch. That is a new
subsystem and conflicts with RUN-0's “each minimal” patch rule. RUN-3 owns the
native PTY replacement and supersedes this temporary fork gap.
