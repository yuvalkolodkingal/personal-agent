# Patch 2 blocked: upstream already short-circuits before-hook failures

Status: `BLOCKED-CONTRADICTION` against OpenCode tag `v1.18.23`
(`ef2880f`).

Related upstream reports:

- https://github.com/anomalyco/opencode/issues/42409
- https://github.com/anomalyco/opencode/issues/32565

RUN-0 asks for a patch making a synchronous throw or asynchronous rejection
from `tool.execute.before` stop the underlying tool executor. The pinned source
already invokes every hook as:

```ts
yield* Effect.promise(async () => fn(input, output))
```

An `async` function converts a synchronous throw from `fn` into a rejected
promise. `Effect.promise` converts that rejection into a failing effect. The
tool paths in `packages/opencode/src/session/tools.ts` and
`packages/opencode/src/tool/code-mode.ts` yield the trigger effect before
calling the tool implementation, so execution cannot continue after either
failure form.

The existing pristine test
`packages/opencode/test/tool/code-mode.test.ts` also asserts this ordering in
`a failing before hook fails only that child call as a catchable in-program
error`: its `called` array proves the blocked `a_tool` implementation was not
reached.

The behavior was reproduced with the exact Effect version pinned by v1.18.23,
`effect@4.0.0-beta.83`, using the pristine expression and an executor side
effect immediately after it:

```text
sync throw: exit=Failure executions=0
async rejection: exit=Failure executions=0
```

Replacing the source with
`Effect.tryPromise({ try: () => Promise.resolve().then(() => fn(...)), catch:
errorMessage })` would make the error typed instead of defective, but would not
change short-circuit behavior. A regression for the requested behavior passes
without that change, so carrying such a patch would violate the requirement
that fork patches be minimal and evidence-backed.

The full pristine OpenCode test could not be rerun after the crash recovery:
the disposable dependency tree was gone and `bun install --frozen-lockfile`
failed with `DNSResolveFailed`. This does not alter the source-level
contradiction or the pinned-Effect execution result above.
