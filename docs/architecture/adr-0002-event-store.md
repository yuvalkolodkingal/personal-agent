# ADR-0002: Append-only encrypted events and rebuildable projections

- Status: accepted
- Date: 2026-08-26

## Decision

Every durable state transition is first represented as a versioned event.
SQLCipher stores the canonical ordered log; tables for goals, tasks, memory,
automations, usage, and similar views are transactional projections or
repositories that can be reconciled to events.

The event ID deduplicates retries; monotonic sequence orders local delivery;
origin and optional entity IDs support audit and projection. Payloads remain
typed by event name and contract version.

## Consequences

- Recovery and UI reconstruction are deterministic.
- Background work survives process restarts without guessing state.
- Audit and migration provenance share one substrate.
- Schema evolution must remain additive within a major version.
- Sensitive payload retention is still governed by privacy settings; an event
  log is not permission to store secrets or every transcript forever.
