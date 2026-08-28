use super::*;
use serde_json::json;

fn protocols() -> BTreeSet<ProtocolVersion> {
    BTreeSet::from([
        ProtocolVersion::new("2024-11-05").unwrap(),
        ProtocolVersion::new("2025-06-18").unwrap(),
        ProtocolVersion::current(),
    ])
}

fn definition(name: &str, namespace: &str, with_install: bool) -> ServerDefinition {
    ServerDefinition {
        id: Uuid::new_v4(),
        name: name.into(),
        namespace: namespace.into(),
        description: "Test server".into(),
        source: ServerSource::Manual,
        transport: TransportDefinition::Stdio {
            executable: "mcp-test".into(),
            arguments: vec!["--stdio".into()],
            working_directory: None,
            environment: Vec::new(),
        },
        supported_protocols: protocols(),
        preferred_protocol: ProtocolVersion::current(),
        install: with_install.then(|| InstallRecipe {
            program: "npm".into(),
            arguments: vec![
                "install".into(),
                "--global".into(),
                "@test/mcp@1.0.0".into(),
            ],
            expected_artifact_sha256: None,
            source_url: Some("https://registry.npmjs.org/@test/mcp".into()),
        }),
        project_scopes: BTreeSet::new(),
        agent_scopes: BTreeSet::new(),
        tags: BTreeSet::new(),
    }
}

fn catalog(tool_name: &str, destructive: bool) -> CapabilityCatalog {
    CapabilityCatalog {
        tools: vec![ToolDescriptor {
            name: tool_name.into(),
            title: Some("Search".into()),
            description: "Untrusted remote description".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"],
                "additionalProperties": false
            }),
            output_schema: None,
            annotations: ToolAnnotations {
                read_only: !destructive,
                destructive,
                idempotent: !destructive,
                open_world: false,
            },
            resolved_name: String::new(),
        }],
        resources: vec![ResourceDescriptor {
            uri: "test://resource".into(),
            name: "Resource".into(),
            description: "Test".into(),
            mime_type: Some("text/plain".into()),
        }],
        prompts: vec![PromptDescriptor {
            name: "summarize".into(),
            description: "Test".into(),
            arguments_schema: None,
        }],
        supports_logging: true,
        supports_completions: true,
        supports_resource_subscriptions: false,
    }
}

struct RuntimeMock {
    handshake: Result<RuntimeHandshake, AdapterError>,
    health: Result<u64, AdapterError>,
    disconnected: usize,
}

impl RuntimeMock {
    fn healthy(tool: &str) -> Self {
        Self {
            handshake: Ok(RuntimeHandshake {
                server_protocols: protocols(),
                catalog: catalog(tool, false),
                latency_ms: 12,
            }),
            health: Ok(8),
            disconnected: 0,
        }
    }
}

impl RuntimeAdapter for RuntimeMock {
    fn connect(
        &mut self,
        _definition: &ServerDefinition,
    ) -> Result<RuntimeHandshake, AdapterError> {
        self.handshake.clone()
    }

    fn health(&mut self, _definition: &ServerDefinition) -> Result<u64, AdapterError> {
        self.health.clone()
    }

    fn disconnect(&mut self, _definition: &ServerDefinition) -> Result<(), AdapterError> {
        self.disconnected += 1;
        Ok(())
    }
}

#[derive(Default)]
struct PackageMock {
    installs: usize,
    uninstalls: usize,
}

impl PackageAdapter for PackageMock {
    fn install(&mut self, recipe: &InstallRecipe) -> Result<InstalledRelease, AdapterError> {
        self.installs += 1;
        Ok(InstalledRelease {
            version: format!("release-{}", self.installs),
            installed_at: Utc::now(),
            artifact_sha256: recipe.expected_artifact_sha256.clone(),
            recipe: Some(recipe.clone()),
        })
    }

    fn uninstall(&mut self, _definition: &ServerDefinition) -> Result<(), AdapterError> {
        self.uninstalls += 1;
        Ok(())
    }
}

fn consent_for(recipe: &InstallRecipe) -> OperationConsent {
    OperationConsent {
        operation_digest: recipe.digest(),
        displayed_text: recipe.display_command(),
        accepted_at: Utc::now(),
        user_confirmed: true,
    }
}

#[test]
fn protocol_negotiation_chooses_newest_shared_revision() {
    let client = protocols();
    let server = BTreeSet::from([
        ProtocolVersion::new("2025-03-26").unwrap(),
        ProtocolVersion::new("2025-06-18").unwrap(),
    ]);
    assert_eq!(
        negotiate_protocol(&client, &server).unwrap().as_str(),
        "2025-06-18"
    );
    assert!(ProtocolVersion::new("next").is_err());
}

#[test]
fn protocol_mismatch_preserves_desired_enablement_for_restart_retry() {
    let mut manager = McpManager::default();
    let server = definition("Future", "future", false);
    let id = server.id;
    manager.add_server(server).unwrap();
    let mut runtime = RuntimeMock {
        handshake: Ok(RuntimeHandshake {
            server_protocols: BTreeSet::from([ProtocolVersion::new("2099-01-01").unwrap()]),
            catalog: CapabilityCatalog::default(),
            latency_ms: 1,
        }),
        health: Ok(1),
        disconnected: 0,
    };

    assert_eq!(
        manager.connect(id, &mut runtime),
        Err(ManagerError::ProtocolMismatch)
    );
    let persisted = manager.server(id).unwrap();
    assert!(persisted.enabled);
    assert_eq!(persisted.state, LifecycleState::Crashed);
    assert_eq!(manager.enabled_server_ids(), vec![id]);
}

#[test]
fn rejects_literal_credentials_and_insecure_remote_http() {
    let mut server = definition("Secrets", "secrets", false);
    server.transport = TransportDefinition::Stdio {
        executable: "server".into(),
        arguments: Vec::new(),
        working_directory: None,
        environment: vec![EnvironmentBinding {
            name: "GITHUB_TOKEN".into(),
            value: BindingValue::NonSecret {
                value: "not-allowed".into(),
            },
        }],
    };
    assert!(matches!(
        validate_definition(&server),
        Err(ManagerError::InvalidTransport(_))
    ));

    server.transport = TransportDefinition::StreamableHttp {
        endpoint: "http://example.com/mcp".into(),
        stateless: true,
        headers: Vec::new(),
        oauth: None,
    };
    assert!(validate_definition(&server).is_err());
    if let TransportDefinition::StreamableHttp { endpoint, .. } = &mut server.transport {
        *endpoint = "http://127.0.0.1:9999/mcp".into();
    }
    assert!(validate_definition(&server).is_ok());
}

#[test]
fn import_discards_raw_credentials_but_preserves_references() {
    let raw_secret = ["gh", "p_this_value_must_never_survive_import"].concat();
    let input = r#"{
      "mcpServers": {
        "GitHub": {
          "command": "npx",
          "args": ["-y", "@modelcontextprotocol/server-github"],
          "env": {
            "GITHUB_TOKEN": "$RAW_SECRET",
            "GITHUB_TOKEN_ALIAS": "${github_token}",
            "LOG_LEVEL": "info"
          }
        }
      }
    }"#
    .replace("$RAW_SECRET", &raw_secret);
    let preview = import_server_json(&input, ImportSource::ClaudeDesktop).unwrap();
    assert_eq!(preview.definitions.len(), 1);
    assert_eq!(preview.issues.len(), 1);
    assert_eq!(preview.issues[0].code, "secret_omitted");
    let encoded = serde_json::to_string(&preview).unwrap();
    assert!(!encoded.contains("must_never_survive_import"));
    assert!(encoded.contains("github_token"));
    assert!(encoded.contains("LOG_LEVEL"));
}

#[test]
fn install_requires_consent_bound_to_exact_displayed_command() {
    let mut manager = McpManager::default();
    let server = definition("Local", "local", true);
    let id = server.id;
    let recipe = server.install.clone().unwrap();
    manager.add_server(server).unwrap();
    let mut packages = PackageMock::default();
    let wrong = OperationConsent {
        operation_digest: "wrong".into(),
        displayed_text: recipe.display_command(),
        accepted_at: Utc::now(),
        user_confirmed: true,
    };
    assert_eq!(
        manager.install(id, &wrong, &mut packages),
        Err(ManagerError::ConsentMismatch)
    );
    manager
        .install(id, &consent_for(&recipe), &mut packages)
        .unwrap();
    assert_eq!(packages.installs, 1);
    assert_eq!(manager.server(id).unwrap().state, LifecycleState::Disabled);
}

#[test]
fn connects_normalizes_names_and_seeds_ask_permissions() {
    let mut manager = McpManager::default();
    let server = definition("GitHub", "github", false);
    let id = server.id;
    manager.add_server(server).unwrap();
    let mut runtime = RuntimeMock::healthy("search issues");
    manager.connect(id, &mut runtime).unwrap();
    let server_state = manager.server(id).unwrap();
    assert_eq!(server_state.state, LifecycleState::Connected);
    assert_eq!(
        server_state.negotiated_protocol.as_ref().unwrap().as_str(),
        CURRENT_PROTOCOL_VERSION
    );
    assert_eq!(
        server_state.catalog.tools[0].resolved_name,
        "github.search_issues"
    );
    assert_eq!(
        server_state.permissions[0].decision,
        PermissionDecision::Ask
    );
    assert!(server_state.health.as_ref().unwrap().healthy);
}

#[test]
fn tool_calls_are_validated_and_only_prepared_for_gateway() {
    let mut manager = McpManager::default();
    let server = definition("GitHub", "github", false);
    let id = server.id;
    manager.add_server(server).unwrap();
    manager
        .connect(id, &mut RuntimeMock::healthy("search"))
        .unwrap();

    let missing = manager.prepare_tool_call(
        id,
        "github.search",
        json!({}),
        &InvocationContext::default(),
    );
    assert!(matches!(missing, Err(ManagerError::InvalidArguments(_))));

    let route = manager
        .prepare_tool_call(
            id,
            "github.search",
            json!({ "query": "safe" }),
            &InvocationContext::default(),
        )
        .unwrap();
    assert!(matches!(route, ToolRoute::ApprovalRequired(_)));

    manager
        .set_permission(
            id,
            ToolPermissionRule {
                tool: "github.search".into(),
                scope: PermissionScope::Workspace("repo".into()),
                decision: PermissionDecision::Allow,
                execution_zone: "network-read".into(),
                max_calls_per_minute: 10,
                timeout_ms: 4_000,
                max_output_bytes: 128_000,
            },
        )
        .unwrap();
    let route = manager
        .prepare_tool_call(
            id,
            "github.search",
            json!({ "query": "safe" }),
            &InvocationContext {
                workspace_id: Some("repo".into()),
                ..InvocationContext::default()
            },
        )
        .unwrap();
    let ToolRoute::Ready(request) = route else {
        panic!("workspace allow rule should prepare a ready gateway request");
    };
    assert_eq!(request.execution_zone, "network-read");
    assert_eq!(request.max_output_bytes, 128_000);
}

#[test]
fn explicit_deny_prevents_gateway_request() {
    let mut manager = McpManager::default();
    let server = definition("GitHub", "github", false);
    let id = server.id;
    manager.add_server(server).unwrap();
    manager
        .connect(id, &mut RuntimeMock::healthy("delete_issue"))
        .unwrap();
    manager
        .set_permission(
            id,
            ToolPermissionRule {
                tool: "github.delete_issue".into(),
                scope: PermissionScope::Global,
                decision: PermissionDecision::Deny,
                execution_zone: "mcp-restricted".into(),
                max_calls_per_minute: 1,
                timeout_ms: 1_000,
                max_output_bytes: 1_000,
            },
        )
        .unwrap();
    assert!(matches!(
        manager.prepare_tool_call(
            id,
            "github.delete_issue",
            json!({ "query": "42" }),
            &InvocationContext::default(),
        ),
        Err(ManagerError::ToolDenied(tool)) if tool == "github.delete_issue"
    ));
}

#[test]
fn update_rollback_and_uninstall_have_consented_state_transitions() {
    let mut manager = McpManager::default();
    let server = definition("Package", "package", true);
    let id = server.id;
    let install_recipe = server.install.clone().unwrap();
    manager.add_server(server).unwrap();
    let mut packages = PackageMock::default();
    manager
        .install(id, &consent_for(&install_recipe), &mut packages)
        .unwrap();
    let update_recipe = InstallRecipe {
        arguments: vec!["install".into(), "@test/mcp@2".into()],
        ..install_recipe
    };
    manager
        .offer_update(
            id,
            UpdatePlan {
                target_version: "2.0.0".into(),
                release_notes_url: Some("https://example.com/releases/2".into()),
                recipe: update_recipe.clone(),
            },
        )
        .unwrap();
    manager
        .apply_update(id, &consent_for(&update_recipe), &mut packages)
        .unwrap();
    assert_eq!(
        manager.server(id).unwrap().state,
        LifecycleState::RollbackAvailable
    );
    assert_eq!(manager.server(id).unwrap().release_history.len(), 1);

    let (digest, text) = manager.rollback_consent_preview(id).unwrap();
    manager
        .rollback(
            id,
            &OperationConsent {
                operation_digest: digest,
                displayed_text: text,
                accepted_at: Utc::now(),
                user_confirmed: true,
            },
            &mut packages,
        )
        .unwrap();
    assert_eq!(manager.server(id).unwrap().state, LifecycleState::Disabled);

    let (digest, text) = manager.uninstall_consent_preview(id).unwrap();
    manager
        .uninstall(
            id,
            &OperationConsent {
                operation_digest: digest,
                displayed_text: text,
                accepted_at: Utc::now(),
                user_confirmed: true,
            },
            &mut packages,
        )
        .unwrap();
    assert_eq!(
        manager.server(id).unwrap().state,
        LifecycleState::Uninstalled
    );
    assert_eq!(packages.uninstalls, 1);
    manager.purge_tombstone(id).unwrap();
    assert!(matches!(
        manager.server(id),
        Err(ManagerError::MissingServer(_))
    ));
}

#[test]
fn health_failure_degrades_without_exposing_adapter_secret() {
    let mut manager = McpManager::default();
    let server = definition("Health", "health", false);
    let id = server.id;
    manager.add_server(server).unwrap();
    let mut runtime = RuntimeMock::healthy("read");
    manager.connect(id, &mut runtime).unwrap();
    runtime.health = Err(AdapterError {
        code: "timeout".into(),
        message: "request failed token=super-sensitive-value after timeout".into(),
        authentication_required: false,
    });
    let health = manager.check_health(id, &mut runtime).unwrap();
    assert!(!health.healthy);
    assert_eq!(manager.server(id).unwrap().state, LifecycleState::Degraded);
    let encoded = serde_json::to_string(&manager.snapshot()).unwrap();
    assert!(!encoded.contains("super-sensitive-value"));
    assert!(encoded.contains("REDACTED"));
}

#[test]
fn exports_references_and_permissions_but_no_keychain_values_exist() {
    let mut manager = McpManager::default();
    let mut server = definition("Remote", "remote", false);
    server.transport = TransportDefinition::StreamableHttp {
        endpoint: "https://mcp.example.com/v1".into(),
        stateless: true,
        headers: vec![HeaderBinding {
            name: "Authorization".into(),
            value: BindingValue::Keychain {
                reference: KeychainReference {
                    reference_id: "remote_auth".into(),
                    service: "personal-agent-mcp".into(),
                    account_hint: "remote".into(),
                },
            },
        }],
        oauth: None,
    };
    manager.add_server(server).unwrap();
    let encoded = serde_json::to_string_pretty(&manager.export_secret_free()).unwrap();
    assert!(encoded.contains("remote_auth"));
    assert!(!encoded.contains("access_token"));
    assert!(!encoded.contains("refresh_token"));
    assert!(!encoded.contains("client_secret"));
}

#[test]
fn namespace_and_tool_collisions_are_deterministic() {
    let mut manager = McpManager::default();
    let first = definition("One", "same", false);
    manager.add_server(first).unwrap();
    let second = definition("Two", "same", false);
    assert!(matches!(
        manager.add_server(second),
        Err(ManagerError::NamespaceCollision(namespace)) if namespace == "same"
    ));

    let normalized = normalize_catalog(
        "tools",
        CapabilityCatalog {
            tools: vec![
                ToolDescriptor {
                    name: "Search".into(),
                    title: None,
                    description: String::new(),
                    input_schema: json!({}),
                    output_schema: None,
                    annotations: ToolAnnotations::default(),
                    resolved_name: String::new(),
                },
                ToolDescriptor {
                    name: "search".into(),
                    title: None,
                    description: String::new(),
                    input_schema: json!({}),
                    output_schema: None,
                    annotations: ToolAnnotations::default(),
                    resolved_name: String::new(),
                },
            ],
            ..CapabilityCatalog::default()
        },
        BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(normalized.tools[0].resolved_name, "tools.search");
    assert_eq!(normalized.tools[1].resolved_name, "tools.search_2");
}

#[test]
fn command_display_is_exact_and_digest_changes_with_each_argument() {
    let recipe = InstallRecipe {
        program: "uvx".into(),
        arguments: vec!["package name".into(), "a'b".into()],
        expected_artifact_sha256: None,
        source_url: None,
    };
    assert_eq!(recipe.display_command(), "uvx 'package name' 'a'\\''b'");
    let digest = recipe.digest();
    let mut changed = recipe;
    changed.arguments.push("--latest".into());
    assert_ne!(digest, changed.digest());
}

#[test]
fn restart_recovery_preserves_desired_enablement_and_requests_runtime_sync() {
    let mut manager = McpManager::default();
    let server = definition("Persistent", "persistent", false);
    let id = server.id;
    manager.add_server(server).unwrap();
    manager
        .connect(id, &mut RuntimeMock::healthy("read"))
        .unwrap();

    let encoded = serde_json::to_vec(&manager).unwrap();
    let mut restored: McpManager = serde_json::from_slice(&encoded).unwrap();
    let reconnect = restored.recover_after_restart();

    assert_eq!(reconnect, vec![id]);
    let server = restored.server(id).unwrap();
    assert!(server.enabled);
    assert_eq!(server.state, LifecycleState::Connecting);
    assert!(server.negotiated_protocol.is_none());
    assert!(server.health.is_none());
    assert_eq!(restored.enabled_server_ids(), vec![id]);
}

#[test]
fn interrupted_package_operations_recover_conservatively() {
    let mut manager = McpManager::default();
    let server = definition("Interrupted", "interrupted", true);
    let id = server.id;
    manager.add_server(server).unwrap();
    {
        let server = manager.server_mut(id).unwrap();
        server.state = LifecycleState::Installing;
        server.enabled = true;
    }
    assert!(manager.recover_after_restart().is_empty());
    let server = manager.server(id).unwrap();
    assert_eq!(server.state, LifecycleState::InstallConsentRequired);
    assert!(!server.enabled);
}

#[test]
fn restore_failure_is_redacted_and_remains_enabled_for_retry() {
    let mut manager = McpManager::default();
    let server = definition("Retry", "retry", false);
    let id = server.id;
    manager.add_server(server).unwrap();
    manager
        .connect(id, &mut RuntimeMock::healthy("read"))
        .unwrap();
    manager.recover_after_restart();
    manager
        .record_restore_failure(id, "runtime failed token=do-not-persist-this")
        .unwrap();

    let server = manager.server(id).unwrap();
    assert!(server.enabled);
    assert_eq!(server.state, LifecycleState::Degraded);
    assert_eq!(
        server.health.as_ref().unwrap().message,
        "runtime failed token=[REDACTED]"
    );
    let encoded = serde_json::to_string(&manager).unwrap();
    assert!(!encoded.contains("do-not-persist-this"));
}
