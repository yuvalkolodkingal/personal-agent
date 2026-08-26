# Legacy Jarvis migration

Personal Agent treats a legacy profile as a read-only source. The Jarvis source
repository is a behavioral reference, not a user profile and not a migration
target. Normal profile roots are `~/.config/jarvis` for configuration and
`~/.local/share/jarvis` for data, unless the legacy installation used explicit
overrides.

## Review before import

Settings → Legacy migration accepts the configuration root, data root, and an
optional OpenCode auth path. **Run metadata-only dry run** reads filesystem
metadata without opening personal payloads and shows every discovered group,
size, item count, secret warning, and planned action. Changing a path invalidates
the review.

Import remains locked until the user checks the explicit consent box. Native
code keeps a one-time review token and re-discovers the roots immediately before
import. A changed source fingerprint stops the run and requires a new review.
The renderer cannot provide or rewrite a migration plan.

The compatibility CLI exposes only the read-only stage:

```sh
personal-agent migration dry-run ~/.config/jarvis ~/.local/share/jarvis
jarvisctl migration dry-run ~/.config/jarvis ~/.local/share/jarvis
```

An optional third path identifies existing OpenCode auth for the plan. Auth is
never emitted as an ordinary migration record. Even with separate adoption
consent, it remains outside the importer until an interactive OS-keychain
onboarding flow handles it.

## Conversion and quarantine rules

- Persona, transcript privacy, trace-argument privacy, and registered project
  paths use an explicit version-1 allowlist. Unknown and credential-like config
  fields are omitted.
- Valid conversation JSONL entries become `conversation.legacy-imported`
  append-only events. Tool arguments and results are not history.
- Memory Markdown keeps its original locator, modified time, and content hash as
  provenance.
- Schedules and MCP server names import disabled. MCP arguments, environment,
  URLs, headers, and auth are omitted.
- Skills and experts pass conservative manifest/name/folder validation. Their
  files are stored as disabled quarantine artifacts; nothing executes during
  migration.
- Remote device names/platform metadata may be listed, but pairing keys and old
  grants are omitted and every device must pair again.
- Environment files and traces are skipped in full.
- Symlinks and special files are not followed. Individual inputs and walks have
  hard size/count limits.

The SQLCipher store commits each deterministic migration record and its domain
materialization in one transaction. The record ID combines the logical source
locator and content hash, so reruns report `already-present` instead of
duplicating consequential state.

## Reports and source integrity

Each confirmed run writes private mode-0600 JSON and Markdown reports beneath
the application-data `migration-reports` directory. Reports contain counts,
hashes, provenance, destinations, enablement state, and errors; they never
contain imported payloads or secret values. The old profile is never written.

CI uses only the synthetic co-located and anonymized split-root fixtures in
`fixtures/legacy`. Tests hash every source byte before and after import, scan all
prepared/encrypted payloads for canary credentials, rerun the import, and verify
history, memory, and disabled automation materialization. Real transcripts,
memory, credentials, auth stores, and device keys must never become fixtures.
