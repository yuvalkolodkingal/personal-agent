# MCP Manager integration contract

The implementation is split deliberately:

- `crates/mcp-manager` owns serializable server definitions, secret-free import
  and export, lifecycle transitions, health, catalogs, scoped permissions,
  consent digests, audit events, and MCP-to-`ToolGateway` normalization.
- The Tauri host owns processes, HTTP, OAuth browser flows, OS-keychain access,
  package installation, persistence, and the existing native `ToolGateway`.
- `apps/desktop/src/McpManager.tsx` is a complete controller-driven surface. It
  has no direct Tauri dependency and is independently testable.

## Workspace wiring

Add `"crates/mcp-manager"` to the root workspace members, then add:

```toml
personal-agent-mcp-manager = { path = "../../../crates/mcp-manager" }
```

to `apps/desktop/src-tauri/Cargo.toml`.

The desktop host is implemented in `apps/desktop/src-tauri/src/mcp_host.rs`.
It persists the manager at `mcp/manager.json` below the private application data
directory using an atomic temporary-file replacement and mode `0600` on Unix.
The serialized state contains keychain/OAuth references, never credential
values. `McpManager::recover_after_restart` conservatively repairs interrupted
package operations and returns every desired-enabled server for synchronization
into the newly-created OpenCode runtime. A failed restore remains enabled and
degraded so a later runtime restart can retry it.

## Native adapters

Implement these crate traits at the Tauri boundary:

```rust
pub trait RuntimeAdapter {
    fn connect(&mut self, definition: &ServerDefinition)
        -> Result<RuntimeHandshake, AdapterError>;
    fn health(&mut self, definition: &ServerDefinition)
        -> Result<u64, AdapterError>;
    fn disconnect(&mut self, definition: &ServerDefinition)
        -> Result<(), AdapterError>;
}

pub trait PackageAdapter {
    fn install(&mut self, recipe: &InstallRecipe)
        -> Result<InstalledRelease, AdapterError>;
    fn uninstall(&mut self, definition: &ServerDefinition)
        -> Result<(), AdapterError>;
}
```

`RuntimeAdapter` must select native stdio, modern Streamable HTTP, or legacy SSE
from `ServerDefinition.transport`; retrieve binding values from the OS keychain;
perform MCP initialize; return all advertised protocol revisions and the
tool/resource/prompt catalog; and retain process/session handles outside the
serializable manager. Adapter error messages must be redacted before returning.

`PackageAdapter` must invoke `program` with the exact `arguments` array directly,
never through a shell. Verify the optional artifact SHA-256 before making a
release current.

## Tauri command contract

The reusable UI expects one typed dispatcher:

```ts
const controller: McpManagerController = {
  execute: (action) => invoke("mcp_manager_execute", { action }),
};
```

Its action/result unions are authoritative in
`apps/desktop/src/McpManager.types.ts`. The backend dispatcher maps actions as
follows:

| Action | Backend operation |
| --- | --- |
| `refresh` | `manager.snapshot()`, plus optional health refresh |
| `add_catalog` | Resolve signed catalog definition, verify install digest, then `add_server` |
| `add_manual` | `add_server(definition)` |
| `preview_import` | `import_server_json(document, source)`; do not log the document |
| `accept_import` | Validate and call `add_server` for each previewed definition |
| `connect` | `manager.connect(id, runtime_adapter)` |
| `start_oauth` | Run native OAuth, store tokens in keychain, persist only `OAuthReference`, reconnect |
| `open_keychain_setup` | Open native secure credential UI; persist only `KeychainReference` |
| `disable` | `manager.disable(id, runtime_adapter)` |
| `restart` | `manager.restart(id, runtime_adapter)` |
| `health` | `manager.check_health(id, runtime_adapter)` |
| `install_preview` | `manager.install_consent_preview(id)` |
| `install` | Re-read preview, compare digest, construct confirmed `OperationConsent`, call `install` |
| `update_preview` | `manager.update_consent_preview(id)` |
| `update` | Re-read preview, compare digest, construct consent, call `apply_update` |
| `rollback_preview` | `manager.rollback_consent_preview(id)` |
| `rollback` | Re-read preview, compare digest, construct consent, call `rollback` |
| `uninstall_preview` | `manager.uninstall_consent_preview(id)` |
| `uninstall` | Re-read preview, compare digest, construct consent, call `uninstall` |
| `purge` | `manager.purge_tombstone(id)` |
| `set_scopes` | Validate IDs then update definition scope sets and audit |
| `set_permission` | `manager.set_permission(id, rule)` |
| `test_tool` | `prepare_tool_call`, obtain approval if required, then send request to `ToolGateway` |
| `export` | Pretty-serialize `manager.export_secret_free()`; never add resolved secrets |

Every mutation returns `{ snapshot, message? }`. Preview actions return
`{ operation_preview: { digest, display_text } }`. Import preview and tool tests
use the exact result types declared in the TypeScript union. Emit a
`mcp-manager://changed` event with the new snapshot when another native event
(crash, OAuth completion, catalog refresh, or update discovery) changes state.

Do not expose a Tauri command that sends MCP requests directly. Both normal calls
and the GUI test form must call `McpManager::prepare_tool_call`, then route the
returned `GatewayToolRequest` through the native `ToolGateway`. Tool arguments
must not enter lifecycle logs or manager audit metadata.

`test_tool` is a two-step action for consequential tools. The first request
omits `approval_digest` and returns an exact, argument-bound operation preview.
After confirmation, resend the same action with that digest. The host registers
a dynamic MCP `ToolImplementation` in `personal_agent_tools::ToolGateway`; the
gateway applies native policy, a one-call scoped consent grant, timeout, secret
redaction, output limits, verification, and audit handling before the value is
returned to the UI.

## React integration

Keep the snapshot in the parent desktop view and update it from both command
results and the native changed event:

```tsx
<McpManager
  snapshot={mcpSnapshot}
  catalog={mcpCatalog}
  controller={controller}
  onSnapshot={setMcpSnapshot}
/>
```

Render this component for the Integrations/MCP destination. The component owns
its wizard, server cards, filters, details, tool form, permission editor, health,
logs, responsive layout, and in-app consent dialogs. Its CSS is fully `mcp-`
prefixed. `McpManager.test.tsx` supplies a complete mocked controller example.

`McpManagerHost.tsx` is the ready-to-use Tauri wrapper. It loads the initial
snapshot, dispatches typed actions, and subscribes to
`mcp-manager://changed`, including the post-startup OpenCode restore event.

## Current host boundaries

- Manual/imported servers, persistence, reconnect/disable/restart, health,
  scopes, permissions, tests, export, exact-command package operations,
  uninstall, rollback, and tombstone purge are wired.
- Secret-bearing bindings are deliberately refused at the OpenCode config
  boundary instead of materializing keychain values into JSON. Remote MCP
  servers use OpenCode's native OAuth registration and callback flow: Connect
  opens the validated authorization URL in the system browser when sign-in is
  required. API-key bindings still require the platform-owned keychain setup
  flow before they can connect.
- `add_catalog` is intentionally unavailable until a signed catalog and digest
  verifier are bundled. Manual and secret-free import flows remain functional.
- Legacy SSE definitions are supported and persisted, but the GUI test form
  delegates legacy session execution to the connected OpenCode runtime rather
  than opening a second SSE session.

## Required acceptance checks

1. Import a Claude/OpenCode file containing a real-looking token and prove the
   token is absent from the preview, persisted JSON, audit log, and export.
2. Connect one stdio, one stateless Streamable HTTP, one session HTTP, and one
   legacy SSE fixture; negotiate both current and older protocol revisions.
3. Crash/restart every transport and revoke OAuth/keychain credentials while the
   UI is open; state and logs must update without leaking adapter output.
4. Attempt each tool permission at global/profile/workspace/agent scope and prove
   the most-specific rule wins while destructive/open-world calls remain gated.
5. Alter one character of an install/update/rollback/uninstall preview and prove
   consent fails.
6. Update, rollback, disable, uninstall, and purge a fixture while verifying
   state persistence after application restart.
7. Run generated test forms for required, typed, enum, array, and object inputs;
   perform full JSON Schema validation again inside `ToolGateway`.
