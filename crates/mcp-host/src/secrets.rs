//! Credential lookup boundary.
//!
//! The host resolves [`KeychainReference`] values immediately before a process
//! spawn or HTTP request and never stores the resolved value.

use std::collections::BTreeMap;
use std::fmt::Debug;

use personal_agent_mcp_manager::{AdapterError, BindingValue, KeychainReference};

/// Resolves keychain references into secret values.
pub trait SecretResolver: Debug + Send + Sync {
    /// Returns the secret behind `reference`.
    ///
    /// # Errors
    ///
    /// Returns an [`AdapterError`] carrying an explicit reason and remediation
    /// when the secret is missing or the platform store is unavailable.
    fn resolve(&self, reference: &KeychainReference) -> Result<String, AdapterError>;
}

/// Default resolver used until the keychain setup wizard exists.
///
/// It refuses every lookup with an explicit remediation rather than silently
/// starting a server without its credential.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnconfiguredSecrets;

impl SecretResolver for UnconfiguredSecrets {
    fn resolve(&self, reference: &KeychainReference) -> Result<String, AdapterError> {
        Err(AdapterError {
            code: "keychain_unavailable".into(),
            message: format!(
                "No keychain value is configured for {}. Add the secret with keychain setup, then reconnect.",
                reference.reference_id
            ),
            authentication_required: true,
        })
    }
}

/// In-memory resolver for tests and for callers that already hold the values.
#[derive(Clone, Debug, Default)]
pub struct InMemorySecrets {
    values: BTreeMap<String, String>,
}

impl InMemorySecrets {
    /// Stores `value` under `reference_id`.
    #[must_use]
    pub fn with(mut self, reference_id: impl Into<String>, value: impl Into<String>) -> Self {
        self.values.insert(reference_id.into(), value.into());
        self
    }
}

impl SecretResolver for InMemorySecrets {
    fn resolve(&self, reference: &KeychainReference) -> Result<String, AdapterError> {
        self.values
            .get(&reference.reference_id)
            .cloned()
            .ok_or_else(|| AdapterError {
                code: "keychain_missing".into(),
                message: format!(
                    "No keychain value is configured for {}. Add the secret with keychain setup, then reconnect.",
                    reference.reference_id
                ),
                authentication_required: true,
            })
    }
}

/// Resolves a binding to its literal value, consulting `secrets` when needed.
///
/// # Errors
///
/// Propagates the resolver's error for unavailable keychain references.
pub fn resolve_binding(
    secrets: &dyn SecretResolver,
    value: &BindingValue,
) -> Result<String, AdapterError> {
    match value {
        BindingValue::NonSecret { value } => Ok(value.clone()),
        BindingValue::Keychain { reference } => secrets.resolve(reference),
    }
}

#[cfg(test)]
mod tests {
    use super::{InMemorySecrets, SecretResolver, UnconfiguredSecrets, resolve_binding};
    use personal_agent_mcp_manager::{BindingValue, KeychainReference};

    fn reference() -> KeychainReference {
        KeychainReference {
            reference_id: "github-token".into(),
            service: "personal-agent".into(),
            account_hint: "mcp".into(),
        }
    }

    #[test]
    fn unconfigured_resolver_asks_for_authentication() {
        let error = UnconfiguredSecrets.resolve(&reference()).unwrap_err();
        assert!(error.authentication_required);
        assert!(error.message.contains("keychain setup"));
    }

    #[test]
    fn in_memory_resolver_returns_configured_values() {
        let secrets = InMemorySecrets::default().with("github-token", "value");
        let binding = BindingValue::Keychain {
            reference: reference(),
        };
        assert_eq!(resolve_binding(&secrets, &binding).unwrap(), "value");
    }

    #[test]
    fn non_secret_bindings_bypass_the_resolver() {
        let binding = BindingValue::NonSecret {
            value: "plain".into(),
        };
        assert_eq!(
            resolve_binding(&UnconfiguredSecrets, &binding).unwrap(),
            "plain"
        );
    }
}
