export type McpLifecycleState =
  | "draft"
  | "install_consent_required"
  | "installing"
  | "disabled"
  | "connecting"
  | "connected"
  | "degraded"
  | "authentication_required"
  | "crashed"
  | "update_available"
  | "updating"
  | "rollback_available"
  | "uninstalling"
  | "uninstalled";

export type McpProtocolVersion = string;

export type McpKeychainReference = {
  reference_id: string;
  service: string;
  account_hint: string;
};

export type McpOAuthReference = {
  grant_id: string;
  issuer: string;
  client_id: string;
  scopes: string[];
  credential: McpKeychainReference;
  expires_at?: string | null;
};

export type McpBindingValue =
  | { kind: "non_secret"; value: string }
  | { kind: "keychain"; reference: McpKeychainReference };

export type McpTransport =
  | {
      kind: "stdio";
      executable: string;
      arguments: string[];
      working_directory?: string | null;
      environment: Array<{ name: string; value: McpBindingValue }>;
    }
  | {
      kind: "streamable_http";
      endpoint: string;
      stateless: boolean;
      headers: Array<{ name: string; value: McpBindingValue }>;
      oauth?: McpOAuthReference | null;
    }
  | {
      kind: "legacy_sse";
      endpoint: string;
      headers: Array<{ name: string; value: McpBindingValue }>;
      oauth?: McpOAuthReference | null;
    };

export type McpServerSource =
  | { kind: "catalog"; catalog_id: string; publisher: string }
  | { kind: "manual" }
  | { kind: "imported"; application: string }
  | { kind: "local_package"; package: string }
  | { kind: "remote"; origin: string };

export type McpInstallRecipe = {
  program: string;
  arguments: string[];
  expected_artifact_sha256?: string | null;
  source_url?: string | null;
};

export type McpServerDefinition = {
  id: string;
  name: string;
  namespace: string;
  description: string;
  source: McpServerSource;
  transport: McpTransport;
  supported_protocols: McpProtocolVersion[];
  preferred_protocol: McpProtocolVersion;
  install?: McpInstallRecipe | null;
  project_scopes: string[];
  agent_scopes: string[];
  tags: string[];
};

export type McpToolAnnotations = {
  read_only: boolean;
  destructive: boolean;
  idempotent: boolean;
  open_world: boolean;
};

export type JsonSchemaProperty = {
  type?: "string" | "number" | "integer" | "boolean" | "array" | "object";
  title?: string;
  description?: string;
  default?: unknown;
  enum?: unknown[];
  minimum?: number;
  maximum?: number;
};

export type McpJsonSchema = {
  type?: string;
  title?: string;
  description?: string;
  properties?: Record<string, JsonSchemaProperty>;
  required?: string[];
  additionalProperties?: boolean;
  [key: string]: unknown;
};

export type McpToolDescriptor = {
  name: string;
  title?: string | null;
  description: string;
  input_schema: McpJsonSchema;
  output_schema?: McpJsonSchema | null;
  annotations: McpToolAnnotations;
  resolved_name: string;
};

export type McpResourceDescriptor = {
  uri: string;
  name: string;
  description: string;
  mime_type?: string | null;
};

export type McpPromptDescriptor = {
  name: string;
  description: string;
  arguments_schema?: McpJsonSchema | null;
};

export type McpCapabilityCatalog = {
  tools: McpToolDescriptor[];
  resources: McpResourceDescriptor[];
  prompts: McpPromptDescriptor[];
  supports_logging: boolean;
  supports_completions: boolean;
  supports_resource_subscriptions: boolean;
};

export type McpPermissionDecision = "allow" | "ask" | "deny";
export type McpPermissionScope =
  | { kind: "global" }
  | { kind: "profile"; id: string }
  | { kind: "workspace"; id: string }
  | { kind: "agent"; id: string };

export type McpToolPermissionRule = {
  tool: string;
  scope: McpPermissionScope;
  decision: McpPermissionDecision;
  execution_zone: string;
  max_calls_per_minute: number;
  timeout_ms: number;
  max_output_bytes: number;
};

export type McpHealthStatus = {
  healthy: boolean;
  checked_at: string;
  latency_ms?: number | null;
  error_rate: number;
  consecutive_failures: number;
  message: string;
};

export type McpServerLog = {
  timestamp: string;
  level: "debug" | "info" | "warn" | "error";
  message: string;
};

export type McpInstalledRelease = {
  version: string;
  installed_at: string;
  artifact_sha256?: string | null;
  recipe?: McpInstallRecipe | null;
};

export type McpUpdatePlan = {
  target_version: string;
  release_notes_url?: string | null;
  recipe: McpInstallRecipe;
};

export type McpManagedServer = {
  definition: McpServerDefinition;
  state: McpLifecycleState;
  enabled: boolean;
  negotiated_protocol?: McpProtocolVersion | null;
  health?: McpHealthStatus | null;
  catalog: McpCapabilityCatalog;
  permissions: McpToolPermissionRule[];
  current_release?: McpInstalledRelease | null;
  release_history: McpInstalledRelease[];
  pending_update?: McpUpdatePlan | null;
  logs: McpServerLog[];
  last_connected_at?: string | null;
};

export type McpAuditEvent = {
  id: string;
  timestamp: string;
  server_id?: string | null;
  event_type: string;
  outcome: string;
  metadata: Record<string, string>;
};

export type McpManagerSnapshot = {
  servers: McpManagedServer[];
  audit_events: McpAuditEvent[];
  protocol_version: string;
};

export type McpCatalogEntry = {
  id: string;
  name: string;
  publisher: string;
  description: string;
  icon?: string;
  tags: string[];
  verified: boolean;
  transport: "stdio" | "streamable_http";
  install_command?: string | null;
  install_digest?: string | null;
  requested_environment: string[];
  requested_network_origins: string[];
};

export type McpImportIssue = {
  server_name: string;
  field: string;
  code: string;
  message: string;
};

export type McpImportPreview = {
  definitions: McpServerDefinition[];
  issues: McpImportIssue[];
};

export type McpTestOutput = {
  tool: string;
  duration_ms: number;
  content: unknown;
  truncated: boolean;
};

export type McpManagerAction =
  | { type: "refresh" }
  | { type: "add_catalog"; catalog_id: string; install_digest?: string }
  | { type: "add_manual"; definition: McpServerDefinition }
  | { type: "preview_import"; source: "claude_desktop" | "opencode" | "generic"; document: string }
  | { type: "accept_import"; definitions: McpServerDefinition[] }
  | { type: "connect"; server_id: string }
  | { type: "start_oauth"; server_id: string }
  | { type: "open_keychain_setup"; server_id: string; binding_name?: string }
  | { type: "disable"; server_id: string }
  | { type: "restart"; server_id: string }
  | { type: "health"; server_id: string }
  | { type: "install_preview"; server_id: string }
  | { type: "install"; server_id: string; operation_digest: string }
  | { type: "update_preview"; server_id: string }
  | { type: "update"; server_id: string; operation_digest: string }
  | { type: "rollback_preview"; server_id: string }
  | { type: "rollback"; server_id: string; operation_digest: string }
  | { type: "uninstall_preview"; server_id: string }
  | { type: "uninstall"; server_id: string; operation_digest: string }
  | { type: "purge"; server_id: string }
  | { type: "set_scopes"; server_id: string; project_scopes: string[]; agent_scopes: string[] }
  | { type: "set_permission"; server_id: string; rule: McpToolPermissionRule }
  | {
      type: "test_tool";
      server_id: string;
      tool: string;
      arguments: Record<string, unknown>;
      approval_digest?: string;
    }
  | { type: "export"; server_ids?: string[] };

export type McpManagerActionResult = {
  snapshot?: McpManagerSnapshot;
  import_preview?: McpImportPreview;
  test_output?: McpTestOutput;
  operation_preview?: { digest: string; display_text: string };
  export_json?: string;
  message?: string;
};

export type McpManagerController = {
  execute(action: McpManagerAction): Promise<McpManagerActionResult>;
};
