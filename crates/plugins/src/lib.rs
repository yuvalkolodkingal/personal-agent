//! Plugin and skill installation policy. Renderer code is never loadable.

use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

/// Permitted plugin execution boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginRuntime {
    Declarative,
    Wasi,
    Process,
}

/// Signed manifest shown in installation preview.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub runtime: PluginRuntime,
    pub entrypoint: Option<String>,
    pub scopes: BTreeSet<String>,
    pub signed: bool,
    pub renderer_code: bool,
}

/// Install gate failure.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum PluginError {
    #[error("unsigned plugins are disabled by default")]
    Unsigned,
    #[error("plugins may contribute only declarative UI")]
    RendererCode,
    #[error("plugin requests forbidden core-policy scope: {0}")]
    PolicyRewrite(String),
    #[error("pack manifest is invalid: {0}")]
    InvalidPack(String),
    #[error("official pack capability is not declared in the product registry: {0}")]
    UnknownCapability(String),
    #[error("pack is already installed: {0}")]
    AlreadyInstalled(String),
    #[error("official connector does not exist: {0}")]
    MissingConnector(String),
    #[error("official connector is disabled or revoked: {0}")]
    ConnectorDisabled(String),
    #[error("connector scope was not declared: {0}")]
    ConnectorScope(String),
    #[error("connector authorization challenge is invalid or already used")]
    ConnectorAuthorization,
    #[error("connector credentials are unavailable from the OS keychain")]
    ConnectorCredential,
    #[error("connector action requires explicit scoped consent")]
    ConnectorConsent,
    #[error("untrusted content cannot directly invoke a connector")]
    ConnectorUntrusted,
    #[error("connector adapter failed without returning private response content")]
    ConnectorAdapter,
    #[error("pairing request is invalid, expired, or already used")]
    InvalidPairing,
    #[error("pairing requested a capability not offered by the server: {0}")]
    PairingCapability(String),
    #[error("paired client has been revoked")]
    PairingRevoked,
    #[error("skill manifest is invalid: {0}")]
    InvalidSkill(String),
    #[error("skill does not exist: {0}")]
    MissingSkill(String),
    #[error("skill requirements are not met: {0}")]
    SkillRequirements(String),
    #[error("agent-authored skill requires explicit user approval")]
    SkillApprovalRequired,
    #[error("skill body could not be read safely: {0}")]
    SkillRead(String),
}

/// Static security preview before an installer can ask for user consent.
///
/// # Errors
///
/// Rejects unsigned manifests (unless explicitly allowed), renderer code, and
/// any scope that attempts to rewrite core policy.
pub fn inspect(manifest: &PluginManifest, allow_unsigned: bool) -> Result<(), PluginError> {
    if !manifest.signed && !allow_unsigned {
        return Err(PluginError::Unsigned);
    }
    if manifest.renderer_code {
        return Err(PluginError::RendererCode);
    }
    if let Some(scope) = manifest
        .scopes
        .iter()
        .find(|scope| scope.starts_with("core.policy."))
    {
        return Err(PluginError::PolicyRewrite(scope.clone()));
    }
    Ok(())
}

/// Supported MCP transports behind the plugin host.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpTransport {
    Stdio,
    Http,
    Sse,
    StreamableHttp,
}

/// Requirement declared by a progressively disclosed skill.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SkillRequirement {
    OperatingSystem(String),
    Binary(String),
    Environment(String),
    Configuration(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillState {
    Proposed,
    Disabled,
    Enabled,
}

/// Metadata indexed without loading the skill body.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub id: String,
    pub name: String,
    pub relative_path: PathBuf,
    pub triggers: BTreeSet<String>,
    pub requirements: BTreeSet<SkillRequirement>,
    pub scopes: BTreeSet<String>,
    pub authored_by_agent: bool,
    pub state: SkillState,
}

/// Presence-only runtime context. Environment values never enter this structure.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RequirementContext {
    pub operating_system: String,
    pub binaries: BTreeSet<String>,
    pub environment_names: BTreeSet<String>,
    pub configuration_keys: BTreeSet<String>,
}

/// Loaded skill body. It is deliberately not `Debug` so logs cannot dump content.
pub struct SkillBody {
    pub id: String,
    body: String,
}

impl SkillBody {
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}

/// Agent Skills-compatible index with proposal review and progressive body loading.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SkillRegistry {
    skills: BTreeMap<String, SkillMetadata>,
}

impl SkillRegistry {
    /// Index metadata only. Agent-authored entries are forced into proposal state.
    ///
    /// # Errors
    ///
    /// Rejects unsafe paths, blank identity/triggers, or policy rewrite scopes.
    pub fn index(&mut self, mut metadata: SkillMetadata) -> Result<(), PluginError> {
        let safe_path = metadata.relative_path.components().all(|component| {
            matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        });
        if metadata.id.trim().is_empty()
            || metadata.name.trim().is_empty()
            || metadata.triggers.is_empty()
            || !safe_path
            || metadata
                .relative_path
                .file_name()
                .and_then(|name| name.to_str())
                != Some("SKILL.md")
            || metadata
                .scopes
                .iter()
                .any(|scope| scope.starts_with("core.policy."))
        {
            return Err(PluginError::InvalidSkill(metadata.id));
        }
        if metadata.authored_by_agent {
            metadata.state = SkillState::Proposed;
        }
        self.skills.insert(metadata.id.clone(), metadata);
        Ok(())
    }

    /// Enable a reviewed skill; agent-authored proposals require explicit confirmation.
    ///
    /// # Errors
    ///
    /// Returns missing-skill or approval-required errors.
    pub fn enable(&mut self, id: &str, user_confirmed: bool) -> Result<(), PluginError> {
        let skill = self
            .skills
            .get_mut(id)
            .ok_or_else(|| PluginError::MissingSkill(id.into()))?;
        if skill.authored_by_agent && !user_confirmed {
            return Err(PluginError::SkillApprovalRequired);
        }
        skill.state = SkillState::Enabled;
        Ok(())
    }

    /// Load the body only after trigger selection, enablement, and requirement checks.
    ///
    /// # Errors
    ///
    /// Rejects disabled/missing skills, unmet requirements, path escapes, non-files,
    /// non-UTF-8 bodies, or files over 256 KiB.
    pub fn load_for_trigger(
        &self,
        root: &Path,
        id: &str,
        trigger: &str,
        context: &RequirementContext,
    ) -> Result<SkillBody, PluginError> {
        let skill = self
            .skills
            .get(id)
            .ok_or_else(|| PluginError::MissingSkill(id.into()))?;
        if skill.state != SkillState::Enabled || !skill.triggers.contains(trigger) {
            return Err(PluginError::InvalidSkill(
                "skill is disabled or trigger did not match".into(),
            ));
        }
        if let Some(requirement) = skill
            .requirements
            .iter()
            .find(|requirement| !requirement_met(requirement, context))
        {
            return Err(PluginError::SkillRequirements(format!("{requirement:?}")));
        }
        let root =
            fs::canonicalize(root).map_err(|error| PluginError::SkillRead(error.to_string()))?;
        let path = fs::canonicalize(root.join(&skill.relative_path))
            .map_err(|error| PluginError::SkillRead(error.to_string()))?;
        if !path.starts_with(&root) || !path.is_file() {
            return Err(PluginError::SkillRead("path left the skill root".into()));
        }
        let metadata =
            fs::metadata(&path).map_err(|error| PluginError::SkillRead(error.to_string()))?;
        if metadata.len() > 256 * 1024 {
            return Err(PluginError::SkillRead("body exceeds 256 KiB".into()));
        }
        let body =
            fs::read_to_string(path).map_err(|error| PluginError::SkillRead(error.to_string()))?;
        Ok(SkillBody {
            id: id.into(),
            body,
        })
    }

    #[must_use]
    pub fn metadata(&self, id: &str) -> Option<&SkillMetadata> {
        self.skills.get(id)
    }
}

fn requirement_met(requirement: &SkillRequirement, context: &RequirementContext) -> bool {
    match requirement {
        SkillRequirement::OperatingSystem(value) => value == &context.operating_system,
        SkillRequirement::Binary(value) => context.binaries.contains(value),
        SkillRequirement::Environment(value) => context.environment_names.contains(value),
        SkillRequirement::Configuration(value) => context.configuration_keys.contains(value),
    }
}

/// Official capability-pack category.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackCategory {
    Productivity,
    Development,
    Communications,
    SmartHome,
    Media,
    Research,
    Dictation,
    Creative,
    Browser,
    Remote,
}

/// Connector entry in an official pack. Credentials are referenced by alias only.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConnectorDeclaration {
    pub id: String,
    pub transport: String,
    pub authorization: String,
    pub credential_aliases: BTreeSet<String>,
    pub scopes: BTreeSet<String>,
    pub enabled_by_default: bool,
}

/// Installable official pack manifest and its deterministic evaluation cases.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PackManifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub category: PackCategory,
    pub publisher: String,
    pub official: bool,
    pub capabilities: BTreeSet<String>,
    pub connectors: Vec<ConnectorDeclaration>,
    pub evaluation_ids: BTreeSet<String>,
    pub install_disabled: bool,
}

impl PackManifest {
    /// Validate manifest safety and coverage before installation preview.
    ///
    /// # Errors
    ///
    /// Rejects malformed IDs, non-official publishers, plaintext credential
    /// references, enabled connectors, or packs without capabilities/evaluations.
    pub fn validate(&self, known_capabilities: &BTreeSet<String>) -> Result<(), PluginError> {
        if self.schema_version != 1
            || self.id.trim().is_empty()
            || self.name.trim().is_empty()
            || self.version.trim().is_empty()
            || self.publisher != "Personal Agent"
            || !self.official
            || !self.install_disabled
            || self.capabilities.is_empty()
            || self.evaluation_ids.is_empty()
        {
            return Err(PluginError::InvalidPack(
                "identity, publisher, capabilities, evaluations, and disabled install are required"
                    .into(),
            ));
        }
        if let Some(capability) = self
            .capabilities
            .iter()
            .find(|capability| !known_capabilities.contains(*capability))
        {
            return Err(PluginError::UnknownCapability(capability.clone()));
        }
        for connector in &self.connectors {
            if connector.enabled_by_default
                || connector.id.trim().is_empty()
                || connector.scopes.is_empty()
                || connector
                    .credential_aliases
                    .iter()
                    .any(|alias| !alias.starts_with("keychain://") || alias.split('/').count() != 4)
            {
                return Err(PluginError::InvalidPack(format!(
                    "connector {} must be disabled and use keychain aliases",
                    connector.id
                )));
            }
        }
        Ok(())
    }
}

/// Installed pack registry. Installation never authorizes connectors.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PackRegistry {
    installed: BTreeMap<String, PackManifest>,
}

impl PackRegistry {
    /// Install a reviewed official pack in disabled state.
    ///
    /// # Errors
    ///
    /// Returns manifest validation or duplicate-install errors.
    pub fn install(
        &mut self,
        manifest: PackManifest,
        known_capabilities: &BTreeSet<String>,
    ) -> Result<(), PluginError> {
        manifest.validate(known_capabilities)?;
        if self.installed.contains_key(&manifest.id) {
            return Err(PluginError::AlreadyInstalled(manifest.id));
        }
        self.installed.insert(manifest.id.clone(), manifest);
        Ok(())
    }

    #[must_use]
    pub fn installed(&self) -> &BTreeMap<String, PackManifest> {
        &self.installed
    }
}

/// Runtime state for an installed official connector.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorState {
    Disabled,
    AuthorizationPending,
    Enabled,
    Revoked,
}

/// Effect class used to preserve always-confirm boundaries across connectors.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorEffect {
    Read,
    Write,
    Communication,
    Commerce,
    Security,
    Power,
}

impl ConnectorEffect {
    fn always_confirms(self) -> bool {
        matches!(
            self,
            Self::Communication | Self::Commerce | Self::Security | Self::Power
        )
    }
}

/// One-time authorization challenge. It contains no token or credential value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConnectorAuthorization {
    pub connector_id: String,
    pub state: Uuid,
    pub requested_scopes: BTreeSet<String>,
}

/// Content-free request handed to a service-specific adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectorDispatch<'a> {
    pub connector_id: &'a str,
    pub scope: &'a str,
    pub effect: ConnectorEffect,
    pub target: &'a str,
}

/// Complete invocation context entering the official-pack policy boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectorInvocation<'a> {
    pub connector_id: &'a str,
    pub scope: &'a str,
    pub effect: ConnectorEffect,
    pub target: &'a str,
    pub from_untrusted_content: bool,
    pub scoped_consent: bool,
}

/// Opaque adapter failure that cannot carry private provider response content.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("connector adapter failed")]
pub struct ConnectorAdapterError;

/// Service-specific connector boundary. Adapters own token exchange and must
/// resolve credentials from keychain aliases outside this API.
pub trait ConnectorAdapter {
    /// Execute one already-authorized operation without returning private body data.
    ///
    /// # Errors
    ///
    /// Returns an opaque failure; private remote response bodies must be retained
    /// by the adapter and never attached to the error.
    fn execute(&mut self, dispatch: ConnectorDispatch<'_>) -> Result<(), ConnectorAdapterError>;
}

/// Auditable content-free connector result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConnectorReceipt {
    pub sequence: u64,
    pub connector_id: String,
    pub scope: String,
    pub effect: ConnectorEffect,
    pub target_sha256: String,
    pub egress: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct ConnectorBinding {
    declaration: ConnectorDeclaration,
    state: ConnectorState,
    pending_state: Option<Uuid>,
}

/// Installed official-pack runtime. Installation and authorization are separate,
/// connector scopes cannot widen at runtime, and untrusted page content cannot
/// directly drive a connector.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct OfficialPackRuntime {
    registry: PackRegistry,
    connectors: BTreeMap<String, ConnectorBinding>,
    sequence: u64,
}

impl OfficialPackRuntime {
    /// Install a reviewed pack with every connector disabled.
    ///
    /// # Errors
    ///
    /// Returns manifest, duplicate-pack, or duplicate-connector errors.
    pub fn install(
        &mut self,
        manifest: &PackManifest,
        known_capabilities: &BTreeSet<String>,
    ) -> Result<(), PluginError> {
        manifest.validate(known_capabilities)?;
        if let Some(connector) = manifest
            .connectors
            .iter()
            .find(|connector| self.connectors.contains_key(&connector.id))
        {
            return Err(PluginError::InvalidPack(format!(
                "duplicate connector id {}",
                connector.id
            )));
        }
        for connector in &manifest.connectors {
            self.connectors.insert(
                connector.id.clone(),
                ConnectorBinding {
                    declaration: connector.clone(),
                    state: ConnectorState::Disabled,
                    pending_state: None,
                },
            );
        }
        if let Err(error) = self.registry.install(manifest.clone(), known_capabilities) {
            for connector in &manifest.connectors {
                self.connectors.remove(&connector.id);
            }
            return Err(error);
        }
        Ok(())
    }

    /// Begin a one-time OAuth/credential authorization with an exact scope subset.
    ///
    /// # Errors
    ///
    /// Rejects unknown connectors and undeclared scopes.
    pub fn begin_authorization(
        &mut self,
        connector_id: &str,
        requested_scopes: BTreeSet<String>,
    ) -> Result<ConnectorAuthorization, PluginError> {
        let binding = self
            .connectors
            .get_mut(connector_id)
            .ok_or_else(|| PluginError::MissingConnector(connector_id.into()))?;
        if requested_scopes.is_empty() || !requested_scopes.is_subset(&binding.declaration.scopes) {
            return Err(PluginError::ConnectorScope(connector_id.into()));
        }
        let state = Uuid::new_v4();
        binding.pending_state = Some(state);
        binding.state = ConnectorState::AuthorizationPending;
        Ok(ConnectorAuthorization {
            connector_id: connector_id.into(),
            state,
            requested_scopes,
        })
    }

    /// Complete an authorization only after explicit confirmation and keychain
    /// presence checks. Credential values never enter the runtime.
    ///
    /// # Errors
    ///
    /// Rejects replayed challenges, absent aliases, and missing user confirmation.
    pub fn complete_authorization(
        &mut self,
        challenge: &ConnectorAuthorization,
        present_keychain_aliases: &BTreeSet<String>,
        user_confirmed: bool,
    ) -> Result<(), PluginError> {
        if !user_confirmed {
            return Err(PluginError::ConnectorConsent);
        }
        let binding = self
            .connectors
            .get_mut(&challenge.connector_id)
            .ok_or_else(|| PluginError::MissingConnector(challenge.connector_id.clone()))?;
        if binding.state != ConnectorState::AuthorizationPending
            || binding.pending_state != Some(challenge.state)
            || !challenge
                .requested_scopes
                .is_subset(&binding.declaration.scopes)
        {
            return Err(PluginError::ConnectorAuthorization);
        }
        if !binding
            .declaration
            .credential_aliases
            .is_subset(present_keychain_aliases)
        {
            return Err(PluginError::ConnectorCredential);
        }
        binding.pending_state = None;
        binding
            .declaration
            .scopes
            .clone_from(&challenge.requested_scopes);
        binding.state = ConnectorState::Enabled;
        Ok(())
    }

    /// Revoke a connector locally; subsequent calls fail closed.
    ///
    /// # Errors
    ///
    /// Returns a missing-connector error for unknown IDs.
    pub fn revoke(&mut self, connector_id: &str) -> Result<(), PluginError> {
        let binding = self
            .connectors
            .get_mut(connector_id)
            .ok_or_else(|| PluginError::MissingConnector(connector_id.into()))?;
        binding.pending_state = None;
        binding.state = ConnectorState::Revoked;
        Ok(())
    }

    /// Invoke a connector after state, zone, scope, and consent checks.
    ///
    /// # Errors
    ///
    /// Fails closed for untrusted input, disabled state, undeclared scopes,
    /// always-confirm effects without consent, and adapter failures.
    pub fn invoke<A: ConnectorAdapter>(
        &mut self,
        invocation: ConnectorInvocation<'_>,
        adapter: &mut A,
    ) -> Result<ConnectorReceipt, PluginError> {
        if invocation.from_untrusted_content {
            return Err(PluginError::ConnectorUntrusted);
        }
        let binding = self
            .connectors
            .get(invocation.connector_id)
            .ok_or_else(|| PluginError::MissingConnector(invocation.connector_id.into()))?;
        if binding.state != ConnectorState::Enabled {
            return Err(PluginError::ConnectorDisabled(
                invocation.connector_id.into(),
            ));
        }
        if !binding.declaration.scopes.contains(invocation.scope) {
            return Err(PluginError::ConnectorScope(invocation.scope.into()));
        }
        if invocation.effect.always_confirms() && !invocation.scoped_consent {
            return Err(PluginError::ConnectorConsent);
        }
        adapter
            .execute(ConnectorDispatch {
                connector_id: invocation.connector_id,
                scope: invocation.scope,
                effect: invocation.effect,
                target: invocation.target,
            })
            .map_err(|_| PluginError::ConnectorAdapter)?;
        self.sequence = self.sequence.saturating_add(1);
        Ok(ConnectorReceipt {
            sequence: self.sequence,
            connector_id: invocation.connector_id.into(),
            scope: invocation.scope.into(),
            effect: invocation.effect,
            target_sha256: hex(&Sha256::digest(invocation.target.as_bytes())),
            egress: true,
        })
    }

    #[must_use]
    pub fn connector_state(&self, connector_id: &str) -> Option<ConnectorState> {
        self.connectors
            .get(connector_id)
            .map(|binding| binding.state)
    }

    #[must_use]
    pub fn registry(&self) -> &PackRegistry {
        &self.registry
    }
}

/// Public pairing offer; the one-time code is displayed out of band and is not serialized.
pub struct PairingOffer {
    pub id: Uuid,
    pub server_nonce: [u8; 32],
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub offered_capabilities: BTreeSet<String>,
    code: secrecy::SecretString,
}

impl std::fmt::Debug for PairingOffer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairingOffer")
            .field("id", &self.id)
            .field("server_nonce", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .field("offered_capabilities", &self.offered_capabilities)
            .field("code", &"[REDACTED]")
            .finish()
    }
}

impl PairingOffer {
    /// Return the one-time code for explicit user-mediated transfer.
    #[must_use]
    pub fn code(&self) -> &secrecy::SecretString {
        &self.code
    }
}

/// Client proof for the versioned remote-control protocol.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PairingProof {
    pub protocol_version: u32,
    pub offer_id: Uuid,
    pub client_id: String,
    pub client_nonce: [u8; 32],
    pub requested_capabilities: BTreeSet<String>,
    pub proof: [u8; 32],
}

/// Paired client metadata. Session key material is never serializable or printable.
pub struct PairedClient {
    pub client_id: String,
    pub capabilities: BTreeSet<String>,
    pub paired_at: chrono::DateTime<chrono::Utc>,
    session_key: secrecy::SecretString,
    revoked: bool,
}

impl std::fmt::Debug for PairedClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairedClient")
            .field("client_id", &self.client_id)
            .field("capabilities", &self.capabilities)
            .field("paired_at", &self.paired_at)
            .field("session_key", &"[REDACTED]")
            .field("revoked", &self.revoked)
            .finish()
    }
}

impl PairedClient {
    /// Authorize only an exactly negotiated remote capability.
    #[must_use]
    pub fn allows(&self, capability: &str) -> bool {
        !self.revoked && self.capabilities.contains(capability)
    }

    /// Produce a request authenticator without exposing the session key.
    ///
    /// # Errors
    ///
    /// Returns `PairingRevoked` after explicit revocation.
    pub fn authenticate(&self, payload: &[u8]) -> Result<[u8; 32], PluginError> {
        if self.revoked {
            return Err(PluginError::PairingRevoked);
        }
        Ok(hmac_sha256(
            self.session_key.expose_secret().as_bytes(),
            payload,
        ))
    }

    /// Revoke the negotiated session without retaining reusable pairing material.
    pub fn revoke(&mut self) {
        self.revoked = true;
    }
}

/// One-time pairing server. Offers are consumed on success and never imported from legacy state.
#[derive(Default)]
pub struct PairingServer {
    pending: BTreeMap<Uuid, PairingOffer>,
}

impl PairingServer {
    /// Create fresh nonce/code material and a short-lived offer.
    #[must_use]
    pub fn offer(
        &mut self,
        capabilities: BTreeSet<String>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> &PairingOffer {
        let id = Uuid::now_v7();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut nonce = [0_u8; 32];
        nonce[..16].copy_from_slice(first.as_bytes());
        nonce[16..].copy_from_slice(second.as_bytes());
        let code = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        match self.pending.entry(id) {
            std::collections::btree_map::Entry::Vacant(entry) => entry.insert(PairingOffer {
                id,
                server_nonce: nonce,
                expires_at: now + chrono::Duration::minutes(5),
                offered_capabilities: capabilities,
                code: secrecy::SecretString::from(code),
            }),
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
        }
    }

    /// Verify and consume a client proof. Unknown capability requests fail rather
    /// than being silently narrowed.
    ///
    /// # Errors
    ///
    /// Rejects expired/replayed/invalid proofs or capability widening.
    pub fn complete(
        &mut self,
        proof: &PairingProof,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<PairedClient, PluginError> {
        let offer = self
            .pending
            .get(&proof.offer_id)
            .ok_or(PluginError::InvalidPairing)?;
        if proof.protocol_version != 1
            || offer.expires_at <= now
            || proof.client_id.trim().is_empty()
        {
            return Err(PluginError::InvalidPairing);
        }
        if let Some(capability) = proof
            .requested_capabilities
            .iter()
            .find(|capability| !offer.offered_capabilities.contains(*capability))
        {
            return Err(PluginError::PairingCapability(capability.clone()));
        }
        let transcript = pairing_transcript(
            proof.offer_id,
            &proof.client_id,
            &offer.server_nonce,
            &proof.client_nonce,
            &proof.requested_capabilities,
        );
        let expected = hmac_sha256(offer.code.expose_secret().as_bytes(), &transcript);
        if !constant_time_eq(&expected, &proof.proof) {
            return Err(PluginError::InvalidPairing);
        }
        let mut session_transcript = b"session".to_vec();
        session_transcript.extend_from_slice(&transcript);
        let session_bytes = hmac_sha256(offer.code.expose_secret().as_bytes(), &session_transcript);
        let session_key = secrecy::SecretString::from(hex(&session_bytes));
        let client = PairedClient {
            client_id: proof.client_id.clone(),
            capabilities: proof.requested_capabilities.clone(),
            paired_at: now,
            session_key,
            revoked: false,
        };
        self.pending.remove(&proof.offer_id);
        Ok(client)
    }
}

/// Third-party client helper for protocol interoperability.
#[must_use]
pub fn create_pairing_proof(
    offer: &PairingOffer,
    client_id: &str,
    client_nonce: [u8; 32],
    requested_capabilities: BTreeSet<String>,
) -> PairingProof {
    let transcript = pairing_transcript(
        offer.id,
        client_id,
        &offer.server_nonce,
        &client_nonce,
        &requested_capabilities,
    );
    PairingProof {
        protocol_version: 1,
        offer_id: offer.id,
        client_id: client_id.into(),
        client_nonce,
        requested_capabilities,
        proof: hmac_sha256(offer.code.expose_secret().as_bytes(), &transcript),
    }
}

fn pairing_transcript(
    offer_id: Uuid,
    client_id: &str,
    server_nonce: &[u8; 32],
    client_nonce: &[u8; 32],
    capabilities: &BTreeSet<String>,
) -> Vec<u8> {
    let mut transcript =
        format!("personal-agent-remote-v1\n{offer_id}\n{client_id}\n").into_bytes();
    transcript.extend_from_slice(server_nonce);
    transcript.extend_from_slice(client_nonce);
    for capability in capabilities {
        transcript.extend_from_slice(b"\n");
        transcript.extend_from_slice(capability.as_bytes());
    }
    transcript
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut normalized = [0_u8; BLOCK];
    if key.len() > BLOCK {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner_key = [0x36_u8; BLOCK];
    let mut outer_key = [0x5c_u8; BLOCK];
    for index in 0..BLOCK {
        inner_key[index] ^= normalized[index];
        outer_key[index] ^= normalized[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_key);
    inner.update(message);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_key);
    outer.update(inner_hash);
    outer.finalize().into()
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingConnector {
        calls: Vec<(String, String, ConnectorEffect)>,
    }

    impl ConnectorAdapter for RecordingConnector {
        fn execute(
            &mut self,
            dispatch: ConnectorDispatch<'_>,
        ) -> Result<(), ConnectorAdapterError> {
            self.calls.push((
                dispatch.connector_id.into(),
                dispatch.scope.into(),
                dispatch.effect,
            ));
            Ok(())
        }
    }

    fn invocation<'a>(
        declaration: &'a ConnectorDeclaration,
        scope: &'a str,
        effect: ConnectorEffect,
        target: &'a str,
        from_untrusted_content: bool,
        scoped_consent: bool,
    ) -> ConnectorInvocation<'a> {
        ConnectorInvocation {
            connector_id: &declaration.id,
            scope,
            effect,
            target,
            from_untrusted_content,
            scoped_consent,
        }
    }

    fn effect_for_scope(scope: &str) -> ConnectorEffect {
        match scope.rsplit_once('.').map(|(_, suffix)| suffix) {
            Some("send") => ConnectorEffect::Communication,
            Some("request") if scope == "commerce.request" => ConnectorEffect::Commerce,
            Some("control") if scope == "home.control" => ConnectorEffect::Power,
            Some("write") => ConnectorEffect::Write,
            _ => ConnectorEffect::Read,
        }
    }

    fn evaluate_connector(
        runtime: &mut OfficialPackRuntime,
        declaration: &ConnectorDeclaration,
        adapter: &mut RecordingConnector,
    ) {
        let first_scope = declaration.scopes.first().expect("declared scope");
        assert!(matches!(
            runtime.invoke(
                invocation(
                    declaration,
                    first_scope,
                    ConnectorEffect::Read,
                    "fixture-target",
                    false,
                    true,
                ),
                adapter,
            ),
            Err(PluginError::ConnectorDisabled(_))
        ));
        assert!(matches!(
            runtime.begin_authorization(&declaration.id, ["undeclared.scope".into()].into()),
            Err(PluginError::ConnectorScope(_))
        ));
        let challenge = runtime
            .begin_authorization(&declaration.id, declaration.scopes.clone())
            .expect("authorization challenge");
        assert_eq!(
            runtime.complete_authorization(&challenge, &declaration.credential_aliases, false),
            Err(PluginError::ConnectorConsent)
        );
        assert_eq!(
            runtime.complete_authorization(&challenge, &BTreeSet::new(), true),
            Err(PluginError::ConnectorCredential)
        );
        runtime
            .complete_authorization(&challenge, &declaration.credential_aliases, true)
            .expect("explicit authorization");
        assert_eq!(
            runtime.complete_authorization(&challenge, &declaration.credential_aliases, true),
            Err(PluginError::ConnectorAuthorization)
        );
        assert!(matches!(
            runtime.invoke(
                invocation(
                    declaration,
                    first_scope,
                    ConnectorEffect::Read,
                    "injection-target",
                    true,
                    true,
                ),
                adapter,
            ),
            Err(PluginError::ConnectorUntrusted)
        ));
        let effect = effect_for_scope(first_scope);
        if effect.always_confirms() {
            assert_eq!(
                runtime.invoke(
                    invocation(
                        declaration,
                        first_scope,
                        effect,
                        "fixture-target",
                        false,
                        false,
                    ),
                    adapter,
                ),
                Err(PluginError::ConnectorConsent)
            );
        }
        let receipt = runtime
            .invoke(
                invocation(
                    declaration,
                    first_scope,
                    effect,
                    "fixture-target",
                    false,
                    true,
                ),
                adapter,
            )
            .expect("connector invocation");
        assert_eq!(receipt.target_sha256.len(), 64);
        assert!(!format!("{receipt:?}").contains("fixture-target"));
        runtime.revoke(&declaration.id).expect("revoke");
        assert!(matches!(
            runtime.invoke(
                invocation(
                    declaration,
                    first_scope,
                    effect,
                    "fixture-target",
                    false,
                    true,
                ),
                adapter,
            ),
            Err(PluginError::ConnectorDisabled(_))
        ));
    }

    #[test]
    fn plugin_cannot_rewrite_safety_policy() {
        let manifest = PluginManifest {
            id: "evil".into(),
            name: "Evil".into(),
            version: "1".into(),
            publisher: "test".into(),
            runtime: PluginRuntime::Wasi,
            entrypoint: Some("evil.wasm".into()),
            scopes: ["core.policy.disable".into()].into(),
            signed: true,
            renderer_code: false,
        };
        assert!(matches!(
            inspect(&manifest, false),
            Err(PluginError::PolicyRewrite(_))
        ));
    }

    #[test]
    fn pack_installation_is_disabled_and_credential_alias_only() {
        let known = ["CAP-CONNECTORS".into()].into();
        let manifest = PackManifest {
            schema_version: 1,
            id: "productivity".into(),
            name: "Productivity".into(),
            version: "1.0.0".into(),
            category: PackCategory::Productivity,
            publisher: "Personal Agent".into(),
            official: true,
            capabilities: ["CAP-CONNECTORS".into()].into(),
            connectors: vec![ConnectorDeclaration {
                id: "calendar".into(),
                transport: "https".into(),
                authorization: "oauth2-pkce".into(),
                credential_aliases: ["keychain://dev.personal-agent/calendar".into()].into(),
                scopes: ["calendar.read".into()].into(),
                enabled_by_default: false,
            }],
            evaluation_ids: ["EVAL-PRODUCTIVITY-001".into()].into(),
            install_disabled: true,
        };
        let mut registry = PackRegistry::default();
        registry.install(manifest, &known).expect("install");
        assert_eq!(registry.installed().len(), 1);
        assert!(!registry.installed()["productivity"].connectors[0].enabled_by_default);
    }

    #[test]
    fn official_pack_evaluations_enforce_scopes_consent_and_revocation() {
        let manifests = [
            include_str!("../../../packs/productivity/pack.json"),
            include_str!("../../../packs/development/pack.json"),
            include_str!("../../../packs/communications/pack.json"),
            include_str!("../../../packs/smart-home/pack.json"),
            include_str!("../../../packs/media/pack.json"),
            include_str!("../../../packs/research/pack.json"),
            include_str!("../../../packs/dictation/pack.json"),
            include_str!("../../../packs/creative/pack.json"),
            include_str!("../../../packs/browser/pack.json"),
            include_str!("../../../packs/remote/pack.json"),
        ];
        let known: BTreeSet<String> = [
            "CAP-AGENT",
            "CAP-ARTIFACTS",
            "CAP-AUTOMATION",
            "CAP-BROWSER",
            "CAP-CONNECTORS",
            "CAP-CONVERSATION",
            "CAP-EXTENSIONS",
            "CAP-MIGRATION",
            "CAP-PROJECTS",
            "CAP-REMOTE",
            "CAP-RESEARCH",
            "CAP-SAFETY",
            "CAP-VOICE",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let parsed: Vec<PackManifest> = manifests
            .into_iter()
            .map(|body| serde_json::from_str(body).expect("official manifest"))
            .collect();
        let connector_count: usize = parsed.iter().map(|pack| pack.connectors.len()).sum();
        let mut runtime = OfficialPackRuntime::default();
        for manifest in &parsed {
            runtime
                .install(manifest, &known)
                .expect("install official pack");
        }
        assert_eq!(runtime.registry().installed().len(), 10);

        let mut adapter = RecordingConnector::default();
        for declaration in parsed.iter().flat_map(|pack| &pack.connectors) {
            evaluate_connector(&mut runtime, declaration, &mut adapter);
        }
        assert_eq!(adapter.calls.len(), connector_count);
    }

    #[test]
    fn mutation_corpus_pack_manifests_never_widen_connector_authority() {
        let original = include_bytes!("../../../packs/productivity/pack.json");
        let known: BTreeSet<String> = ["CAP-AUTOMATION", "CAP-CONNECTORS"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut state = 0xd1b5_4a32_d192_ed03_u64;
        for _ in 0..2_048 {
            state = state
                .wrapping_mul(2_862_933_555_777_941_757)
                .wrapping_add(3_037_000_493);
            let mut mutated = original.to_vec();
            let index = usize::try_from(state).unwrap_or(0) % mutated.len();
            let delta = u8::try_from((state >> 40) % 255 + 1).unwrap_or(1);
            mutated[index] ^= delta;
            if let Ok(manifest) = serde_json::from_slice::<PackManifest>(&mutated)
                && manifest.validate(&known).is_ok()
            {
                assert!(manifest.install_disabled);
                assert!(manifest.connectors.iter().all(|connector| {
                    !connector.enabled_by_default
                        && connector
                            .credential_aliases
                            .iter()
                            .all(|alias| alias.starts_with("keychain://"))
                }));
            }
        }
    }

    #[test]
    fn remote_pairing_uses_fresh_keys_and_exact_negotiated_capabilities() {
        let now = chrono::Utc::now();
        let mut server = PairingServer::default();
        let offer = server.offer(["status.read".into(), "goal.pause".into()].into(), now);
        let first_offer_id = offer.id;
        let proof = create_pairing_proof(
            offer,
            "third-party-client",
            [7; 32],
            ["status.read".into()].into(),
        );
        let mut client = server.complete(&proof, now).expect("pair");
        assert!(client.allows("status.read"));
        assert!(!client.allows("goal.pause"));
        assert!(server.complete(&proof, now).is_err(), "proof is one-time");
        let next = server.offer(["status.read".into()].into(), now);
        assert_ne!(first_offer_id, next.id);
        assert_ne!(proof.offer_id, next.id);
        assert!(format!("{client:?}").contains("[REDACTED]"));
        assert!(client.authenticate(b"request").is_ok());
        client.revoke();
        assert!(!client.allows("status.read"));
        assert!(matches!(
            client.authenticate(b"request"),
            Err(PluginError::PairingRevoked)
        ));
    }

    #[test]
    fn remote_client_cannot_request_unoffered_capability() {
        let now = chrono::Utc::now();
        let mut server = PairingServer::default();
        let offer = server.offer(["status.read".into()].into(), now);
        let proof =
            create_pairing_proof(offer, "client", [9; 32], ["system.shutdown".into()].into());
        assert!(matches!(
            server.complete(&proof, now),
            Err(PluginError::PairingCapability(capability)) if capability == "system.shutdown"
        ));
    }

    #[test]
    fn agent_authored_skill_is_progressively_disclosed_only_after_review() {
        let temp = tempfile::tempdir().expect("temp");
        let directory = temp.path().join("skills/status");
        fs::create_dir_all(&directory).expect("directory");
        fs::write(
            directory.join("SKILL.md"),
            "# Status\nRun only when triggered.",
        )
        .expect("skill");
        let metadata = SkillMetadata {
            id: "status".into(),
            name: "Status".into(),
            relative_path: PathBuf::from("skills/status/SKILL.md"),
            triggers: ["show status".into()].into(),
            requirements: [SkillRequirement::OperatingSystem(
                std::env::consts::OS.into(),
            )]
            .into(),
            scopes: ["system.read".into()].into(),
            authored_by_agent: true,
            state: SkillState::Enabled,
        };
        let mut registry = SkillRegistry::default();
        registry.index(metadata).expect("index");
        assert_eq!(
            registry.metadata("status").unwrap().state,
            SkillState::Proposed
        );
        assert_eq!(
            registry.enable("status", false),
            Err(PluginError::SkillApprovalRequired)
        );
        registry.enable("status", true).expect("approve");
        let body = registry
            .load_for_trigger(
                temp.path(),
                "status",
                "show status",
                &RequirementContext {
                    operating_system: std::env::consts::OS.into(),
                    ..RequirementContext::default()
                },
            )
            .expect("load");
        assert!(body.body().contains("Run only when triggered"));
    }
}
