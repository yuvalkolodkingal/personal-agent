//! Keychain-only credential resolution.
//!
//! Provider keys are never read from configuration files, environment
//! variables, or request parameters. A provider names a `keychain://` alias and
//! the OS secret store resolves it, so the only place key material exists is
//! the platform keychain and the process memory of one request.

use personal_agent_platform::{SecretReference, SecretStore};
use secrecy::SecretString;
use std::fmt;

use crate::error::LlmError;
use crate::redact::REDACTED;

/// A resolved provider credential that cannot be printed or serialized.
///
/// `Debug` and `Display` both render `[redacted]`; the value is reachable only
/// through [`ApiKey::secret`], which is crate-internal.
#[derive(Clone)]
pub struct ApiKey(SecretString);

impl ApiKey {
    /// Resolve one `keychain://service/account` alias through the OS store.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::InvalidKeyAlias`] when the alias is not a keychain
    /// alias (a literal key pasted into configuration is rejected here), or
    /// [`LlmError::Secret`] when the store has no entry or is unavailable.
    pub fn resolve(alias: &str, store: &dyn SecretStore) -> Result<Self, LlmError> {
        let reference = SecretReference::parse(alias).map_err(|_| LlmError::InvalidKeyAlias)?;
        Ok(Self(store.get(&reference)?))
    }

    pub(crate) fn secret(&self) -> &SecretString {
        &self.0
    }

    #[cfg(test)]
    pub(crate) fn for_test(value: &str) -> Self {
        Self(SecretString::from(value))
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(REDACTED)
    }
}

impl fmt::Display for ApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(REDACTED)
    }
}

#[cfg(test)]
mod tests {
    use super::ApiKey;
    use crate::error::LlmError;
    use personal_agent_platform::{SecretReference, SecretStore, SecretStoreError};
    use secrecy::SecretString;

    struct FixedStore(&'static str);

    impl SecretStore for FixedStore {
        fn put(
            &self,
            _reference: &SecretReference,
            _value: &SecretString,
        ) -> Result<(), SecretStoreError> {
            Err(SecretStoreError::Unavailable("read-only".into()))
        }

        fn get(&self, _reference: &SecretReference) -> Result<SecretString, SecretStoreError> {
            Ok(SecretString::from(self.0))
        }

        fn delete(&self, _reference: &SecretReference) -> Result<(), SecretStoreError> {
            Err(SecretStoreError::Unavailable("read-only".into()))
        }
    }

    #[test]
    fn only_keychain_aliases_resolve() {
        let store = FixedStore("fixture-provider-token-1234");
        assert!(ApiKey::resolve("keychain://anthropic/default", &store).is_ok());
        for rejected in [
            "fixture-provider-token-1234",
            "env://ANTHROPIC_API_KEY",
            "keychain://anthropic",
            "keychain://anthropic/default/extra",
        ] {
            assert!(matches!(
                ApiKey::resolve(rejected, &store),
                Err(LlmError::InvalidKeyAlias)
            ));
        }
    }

    #[test]
    fn debug_and_display_never_disclose_the_key() {
        let key = ApiKey::for_test("fixture-provider-token-1234");
        assert_eq!(format!("{key:?}"), "[redacted]");
        assert_eq!(format!("{key}"), "[redacted]");
    }
}
