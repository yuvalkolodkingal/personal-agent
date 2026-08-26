//! Canonical, strict, human-editable application configuration.

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use thiserror::Error;

/// JSON Schema used by editors and external tooling.
pub const CONFIG_SCHEMA: &str = include_str!("../../../contracts/schema/config.schema.json");

fn schema_version() -> u32 {
    1
}

fn persona_style() -> String {
    "Composed, concise, and quietly witty.".to_owned()
}

fn parallelism() -> u8 {
    3
}

fn delegation_depth() -> u8 {
    3
}

fn opencode_version() -> String {
    personal_agent_runtime::OPENCODE_VERSION.to_owned()
}

fn startup_timeout_ms() -> u64 {
    30_000
}

fn transcript_recording() -> bool {
    true
}

/// Top-level v1 configuration. Unknown fields fail closed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalAgentConfig {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    pub persona: PersonaConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub privacy: PrivacyConfig,
    #[serde(default)]
    pub secret_aliases: Vec<KeychainAlias>,
    #[serde(default)]
    pub risk_acknowledgements: Vec<RiskAcknowledgement>,
}

impl Default for PersonalAgentConfig {
    fn default() -> Self {
        Self {
            schema_version: schema_version(),
            persona: PersonaConfig::default(),
            agent: AgentConfig::default(),
            runtime: RuntimeConfig::default(),
            privacy: PrivacyConfig::default(),
            secret_aliases: Vec::new(),
            risk_acknowledgements: Vec::new(),
        }
    }
}

/// Configurable assistant identity. It describes behavior and never sentience.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaConfig {
    pub name: String,
    #[serde(default = "persona_style")]
    pub style: String,
}

impl Default for PersonaConfig {
    fn default() -> Self {
        Self {
            name: "JARVIS".to_owned(),
            style: persona_style(),
        }
    }
}

/// Bounded agent concurrency controls.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    #[serde(default = "parallelism")]
    pub default_parallelism: u8,
    #[serde(default = "delegation_depth")]
    pub max_delegation_depth: u8,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            default_parallelism: parallelism(),
            max_delegation_depth: delegation_depth(),
        }
    }
}

/// Runtime ownership and startup controls.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    #[serde(default = "opencode_version")]
    pub opencode_version: String,
    #[serde(default = "startup_timeout_ms")]
    pub startup_timeout_ms: u64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            opencode_version: opencode_version(),
            startup_timeout_ms: startup_timeout_ms(),
        }
    }
}

/// Local audit-retention choices.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrivacyConfig {
    #[serde(default = "transcript_recording")]
    pub record_transcripts: bool,
    #[serde(default)]
    pub record_tool_arguments: bool,
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        Self {
            record_transcripts: transcript_recording(),
            record_tool_arguments: false,
        }
    }
}

/// A reference to an operating-system keychain entry, never a secret value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeychainAlias(pub String);

/// Risk levels that require a durable, explicit acknowledgement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RiskLevel {
    Consequential,
    Irreversible,
}

/// Acknowledgement is configuration, not a blanket permission grant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskAcknowledgement {
    pub scope: String,
    pub risk: RiskLevel,
    pub acknowledged: bool,
}

/// Parsed configuration plus safe defaults that were materialized.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigLoad {
    pub config: PersonalAgentConfig,
    pub repaired_fields: Vec<&'static str>,
}

impl ConfigLoad {
    /// Serialize the validated and default-filled configuration.
    ///
    /// # Errors
    ///
    /// Returns an error only if TOML serialization fails.
    pub fn repaired_toml(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(&self.config).map_err(ConfigError::Serialize)
    }
}

/// Configuration parsing or semantic validation failure.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration is not valid TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("configuration cannot be serialized: {0}")]
    Serialize(toml::ser::Error),
    #[error("unsupported configuration schema version {0}")]
    SchemaVersion(u32),
    #[error("persona.name must not be blank")]
    BlankPersona,
    #[error("agent.default_parallelism must be between 1 and 8")]
    Parallelism,
    #[error("agent.max_delegation_depth must be at most 8")]
    DelegationDepth,
    #[error("runtime.opencode_version must match bundled version {expected}")]
    RuntimeVersion { expected: &'static str },
    #[error("runtime.startup_timeout_ms must be between 1000 and 120000")]
    StartupTimeout,
    #[error("secret alias must use keychain:// and contain a non-empty service/account path")]
    SecretAlias,
    #[error("risk acknowledgement scope must not be blank")]
    BlankRiskScope,
    #[error("risk acknowledgement must set acknowledged = true")]
    RiskNotAcknowledged,
}

/// Canonical configuration file failure.
#[derive(Debug, Error)]
pub enum ConfigFileError {
    #[error("configuration file cannot be accessed: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Config(#[from] ConfigError),
}

/// Render the safe default configuration used for a fresh profile.
///
/// # Errors
///
/// Returns an error only if TOML serialization fails.
pub fn default_config_toml() -> Result<String, ConfigError> {
    toml::to_string_pretty(&PersonalAgentConfig::default()).map_err(ConfigError::Serialize)
}

/// Load canonical TOML, creating a private safe-default file only when absent.
///
/// Existing invalid configuration is never overwritten or silently repaired on
/// disk. The returned value identifies safe defaults that were materialized in
/// memory so the UI can explain them before an explicit save.
///
/// # Errors
///
/// Returns an I/O or strict configuration-validation error.
pub fn load_or_initialize_config(path: &Path) -> Result<ConfigLoad, ConfigFileError> {
    match fs::read_to_string(path) {
        Ok(input) => Ok(parse_config(&input)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let rendered = default_config_toml()?;
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(path) {
                Ok(mut file) => {
                    file.write_all(rendered.as_bytes())?;
                    file.sync_all()?;
                    Ok(parse_config(&rendered)?)
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    Ok(parse_config(&fs::read_to_string(path)?)?)
                }
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// Parse strict TOML, materialize safe defaults, then validate security invariants.
///
/// # Errors
///
/// Unknown fields, malformed TOML, unsupported values, raw-looking secret aliases,
/// and incomplete risk acknowledgements are rejected.
pub fn parse_config(input: &str) -> Result<ConfigLoad, ConfigError> {
    let shape: toml::Value = toml::from_str(input)?;
    let mut repaired_fields = Vec::new();
    for field in [
        "agent",
        "runtime",
        "privacy",
        "secret_aliases",
        "risk_acknowledgements",
    ] {
        if shape.get(field).is_none() {
            repaired_fields.push(field);
        }
    }
    if shape.get("schema_version").is_none() {
        repaired_fields.push("schema_version");
    }
    if shape
        .get("persona")
        .and_then(|persona| persona.get("style"))
        .is_none()
    {
        repaired_fields.push("persona.style");
    }

    let config: PersonalAgentConfig = toml::from_str(input)?;
    validate(&config)?;
    Ok(ConfigLoad {
        config,
        repaired_fields,
    })
}

fn validate(config: &PersonalAgentConfig) -> Result<(), ConfigError> {
    if config.schema_version != schema_version() {
        return Err(ConfigError::SchemaVersion(config.schema_version));
    }
    if config.persona.name.trim().is_empty() {
        return Err(ConfigError::BlankPersona);
    }
    if !(1..=8).contains(&config.agent.default_parallelism) {
        return Err(ConfigError::Parallelism);
    }
    if config.agent.max_delegation_depth > 8 {
        return Err(ConfigError::DelegationDepth);
    }
    if config.runtime.opencode_version != personal_agent_runtime::OPENCODE_VERSION {
        return Err(ConfigError::RuntimeVersion {
            expected: personal_agent_runtime::OPENCODE_VERSION,
        });
    }
    if !(1_000..=120_000).contains(&config.runtime.startup_timeout_ms) {
        return Err(ConfigError::StartupTimeout);
    }
    for alias in &config.secret_aliases {
        if personal_agent_platform::SecretReference::parse(&alias.0).is_err() {
            return Err(ConfigError::SecretAlias);
        }
    }
    for acknowledgement in &config.risk_acknowledgements {
        if acknowledgement.scope.trim().is_empty() {
            return Err(ConfigError::BlankRiskScope);
        }
        if !acknowledgement.acknowledged {
            return Err(ConfigError::RiskNotAcknowledged);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "schema_version = 1\n[persona]\nname = 'JARVIS'\n";

    #[test]
    fn config_materializes_safe_defaults_and_roundtrips() {
        let loaded = parse_config(MINIMAL).expect("minimal config");
        assert_eq!(loaded.config.agent.default_parallelism, 3);
        assert!(loaded.repaired_fields.contains(&"runtime"));
        assert!(loaded.repaired_fields.contains(&"persona.style"));
        let rendered = loaded.repaired_toml().expect("serialize");
        let second = parse_config(&rendered).expect("roundtrip");
        assert_eq!(loaded.config, second.config);
        assert!(second.repaired_fields.is_empty());
    }

    #[test]
    fn config_rejects_unknown_fields() {
        let input = format!("{MINIMAL}\nsecret = 'plaintext'\n");
        assert!(matches!(parse_config(&input), Err(ConfigError::Parse(_))));
    }

    #[test]
    fn config_rejects_invalid_risk_acknowledgement() {
        let input = format!(
            "{MINIMAL}\n[[risk_acknowledgements]]\nscope = 'external.purchase'\nrisk = 'irreversible'\nacknowledged = false\n"
        );
        assert!(matches!(
            parse_config(&input),
            Err(ConfigError::RiskNotAcknowledged)
        ));
    }

    #[test]
    fn config_accepts_only_keychain_aliases() {
        let raw = "schema_version = 1\nsecret_aliases = ['super-secret-value']\n[persona]\nname = 'JARVIS'\n";
        assert!(matches!(parse_config(raw), Err(ConfigError::SecretAlias)));
        let alias = "schema_version = 1\nsecret_aliases = ['keychain://openai/default']\n[persona]\nname = 'JARVIS'\n";
        assert!(parse_config(alias).is_ok());
    }

    #[test]
    fn config_schema_remains_draft_2020_12() {
        let schema: serde_json::Value = serde_json::from_str(CONFIG_SCHEMA).expect("schema JSON");
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert!(schema["properties"]["risk_acknowledgements"].is_object());
    }

    #[test]
    fn config_file_initializes_once_and_preserves_invalid_edits() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("config.toml");
        let initialized = load_or_initialize_config(&path).expect("initialize");
        assert_eq!(initialized.config.persona.name, "JARVIS");
        assert!(initialized.repaired_fields.is_empty());

        fs::write(&path, "invalid = true\n").expect("edit fixture");
        assert!(load_or_initialize_config(&path).is_err());
        assert_eq!(fs::read_to_string(&path).expect("read"), "invalid = true\n");
    }
}
