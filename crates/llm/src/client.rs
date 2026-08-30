//! The provider client.

use std::fmt;
use std::sync::Arc;

use personal_agent_platform::SecretStore;
use tokio::sync::mpsc;

use crate::abort::AbortHandle;
use crate::error::LlmError;
use crate::message::ChatRequest;
use crate::provider::ProviderConfig;
use crate::secret::ApiKey;
use crate::stream::TurnStream;
use crate::transport::TurnDriver;

/// Depth of the event channel between the HTTP task and the caller.
const EVENT_BUFFER: usize = 32;

/// A configured provider client.
///
/// One client owns one `reqwest` client (connection pool included) and one
/// resolved credential. It is cheap to clone and safe to share: every turn
/// gets its own task, channel, and abort handle.
#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    provider: Arc<ProviderConfig>,
    key: Option<ApiKey>,
}

impl LlmClient {
    /// Build a client, resolving the provider's keychain alias if it has one.
    ///
    /// A provider with no alias (a local Ollama or LM Studio server) is sent
    /// no credential at all rather than an empty one.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::InvalidKeyAlias`] when the configured alias is not
    /// a `keychain://` alias, [`LlmError::Secret`] when the OS store cannot
    /// produce it, and [`LlmError::Transport`] when the HTTP client cannot be
    /// constructed.
    pub fn connect(provider: ProviderConfig, secrets: &dyn SecretStore) -> Result<Self, LlmError> {
        let key = provider
            .key_alias
            .as_deref()
            .map(|alias| ApiKey::resolve(alias, secrets))
            .transpose()?;
        Self::with_key(provider, key)
    }

    /// Build a client from an already resolved credential.
    ///
    /// [`ApiKey`] values can only be produced by [`ApiKey::resolve`], so this
    /// entry point cannot be used to smuggle in a literal key.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::Transport`] when the HTTP client cannot be built.
    pub fn with_key(provider: ProviderConfig, key: Option<ApiKey>) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .connect_timeout(provider.connect_timeout)
            .timeout(provider.request_timeout)
            .build()
            .map_err(|error| LlmError::Transport(error.to_string()))?;
        Ok(Self {
            http,
            provider: Arc::new(provider),
            key,
        })
    }

    /// The provider this client talks to.
    #[must_use]
    pub fn provider(&self) -> &ProviderConfig {
        &self.provider
    }

    /// Start a streamed turn.
    ///
    /// The request is validated before any network use, the HTTP work runs on
    /// a spawned task, and events arrive on the returned [`TurnStream`].
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::Request`] when the request could not succeed as
    /// written (no model, no messages, or a zero output ceiling).
    ///
    /// # Panics
    ///
    /// Panics if called outside a Tokio runtime, because the turn is driven by
    /// a spawned task.
    pub fn stream(&self, request: ChatRequest) -> Result<TurnStream, LlmError> {
        request.validate()?;
        let (sender, receiver) = mpsc::channel(EVENT_BUFFER);
        let abort = AbortHandle::new();
        let driver = TurnDriver {
            http: self.http.clone(),
            provider: Arc::clone(&self.provider),
            key: self.key.clone(),
            request,
            abort: abort.clone(),
            events: sender,
        };
        let task = tokio::spawn(driver.run());
        Ok(TurnStream::new(receiver, abort, task))
    }
}

impl fmt::Debug for LlmClient {
    /// Renders configuration only. The credential is never included, and the
    /// inner `reqwest` client is omitted because its own `Debug` prints the
    /// default header map.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LlmClient")
            .field("provider", &self.provider.id)
            .field("kind", &self.provider.kind)
            .field("base_url", &self.provider.base_url.as_str())
            .field("key_alias", &self.provider.key_alias)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}
