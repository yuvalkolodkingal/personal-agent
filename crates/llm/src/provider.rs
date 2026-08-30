//! Provider configuration.
//!
//! A provider is described by a wire dialect, a base URL, and a keychain alias.
//! No configuration field can hold key material: the alias is resolved through
//! [`crate::ApiKey`] at client construction and the resolved value never
//! re-enters a configuration value, a log line, or a `Debug` rendering.

use std::collections::BTreeMap;
use std::time::Duration;
use url::Url;

use crate::error::LlmError;
use crate::retry::RetryPolicy;

/// Latest Claude Opus model identifier.
pub const CLAUDE_OPUS_5: &str = "claude-opus-5";
/// Latest Claude Sonnet model identifier.
pub const CLAUDE_SONNET_5: &str = "claude-sonnet-5";
/// Latest Claude Haiku model identifier.
pub const CLAUDE_HAIKU_4_5: &str = "claude-haiku-4-5-20251001";

/// Value of the `anthropic-version` header this client speaks.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Beta token that opts a request into the 1M-token context window.
pub const ANTHROPIC_CONTEXT_1M_BETA: &str = "context-1m-2025-08-07";
/// Default Anthropic API root.
pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";

/// Wire dialect spoken by a provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderKind {
    /// Anthropic Messages API (`POST /v1/messages`).
    Anthropic,
    /// OpenAI-compatible chat completions (`POST /v1/chat/completions`).
    ///
    /// Covers `OpenRouter`, Ollama, LM Studio, vLLM, and the repository's
    /// synthetic fixture provider.
    OpenAiCompatible,
}

/// How long the provider should retain a cached prompt prefix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheTtl {
    /// Default five-minute ephemeral cache.
    FiveMinutes,
    /// Extended one-hour ephemeral cache.
    OneHour,
}

impl CacheTtl {
    pub(crate) fn wire_value(self) -> Option<&'static str> {
        match self {
            Self::FiveMinutes => None,
            Self::OneHour => Some("1h"),
        }
    }
}

/// Parse an API root, rejecting every scheme that is not HTTP or HTTPS.
fn parse_http_url(base_url: &str) -> Result<Url, LlmError> {
    let parsed = Url::parse(base_url).map_err(|_| LlmError::InvalidBaseUrl)?;
    if matches!(parsed.scheme(), "http" | "https") {
        Ok(parsed)
    } else {
        Err(LlmError::InvalidBaseUrl)
    }
}

/// Everything needed to reach one provider.
#[derive(Clone, Debug)]
pub struct ProviderConfig {
    /// Stable identifier used in events and logs.
    pub id: String,
    /// Wire dialect.
    pub kind: ProviderKind,
    /// API root; the route is appended by the wire module.
    pub base_url: Url,
    /// `keychain://service/account` alias, or `None` for a keyless local server.
    pub key_alias: Option<String>,
    /// Retry and backoff behaviour.
    pub retry: RetryPolicy,
    /// Prompt-cache marking, or `None` to send no `cache_control` markers.
    pub prompt_caching: Option<CacheTtl>,
    /// Beta tokens passed through as `anthropic-beta`.
    pub betas: Vec<String>,
    /// Additional static headers (`OpenRouter` attribution, gateway routing).
    pub extra_headers: BTreeMap<String, String>,
    /// Ceiling on one attempt, including the streamed body.
    pub request_timeout: Duration,
    /// Ceiling on establishing the connection.
    pub connect_timeout: Duration,
}

impl ProviderConfig {
    /// Anthropic Messages API with prompt caching on and the default API root.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::InvalidBaseUrl`] only if the compiled-in default
    /// root ever stops parsing as a URL.
    pub fn anthropic(key_alias: impl Into<String>) -> Result<Self, LlmError> {
        let base_url = Url::parse(ANTHROPIC_BASE_URL).map_err(|_| LlmError::InvalidBaseUrl)?;
        Ok(Self {
            id: "anthropic".to_owned(),
            kind: ProviderKind::Anthropic,
            base_url,
            key_alias: Some(key_alias.into()),
            retry: RetryPolicy::default(),
            prompt_caching: Some(CacheTtl::FiveMinutes),
            betas: Vec::new(),
            extra_headers: BTreeMap::new(),
            request_timeout: Duration::from_secs(600),
            connect_timeout: Duration::from_secs(10),
        })
    }

    /// An OpenAI-compatible endpoint.
    ///
    /// `base_url` is the API root (for example `http://127.0.0.1:11434/v1` or
    /// `https://openrouter.ai/api/v1`). Pass `None` for `key_alias` when the
    /// server is a keyless local runtime.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::InvalidBaseUrl`] when `base_url` is not a valid
    /// HTTP or HTTPS URL.
    pub fn openai_compatible(
        id: impl Into<String>,
        base_url: &str,
        key_alias: Option<String>,
    ) -> Result<Self, LlmError> {
        let base_url = parse_http_url(base_url)?;
        Ok(Self {
            id: id.into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url,
            key_alias,
            retry: RetryPolicy::default(),
            prompt_caching: None,
            betas: Vec::new(),
            extra_headers: BTreeMap::new(),
            request_timeout: Duration::from_secs(600),
            connect_timeout: Duration::from_secs(10),
        })
    }

    /// Point the provider at a different API root.
    ///
    /// Used for self-hosted gateways, regional endpoints, and the test stubs.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::InvalidBaseUrl`] when `base_url` is not a valid
    /// HTTP or HTTPS URL.
    pub fn with_base_url(mut self, base_url: &str) -> Result<Self, LlmError> {
        self.base_url = parse_http_url(base_url)?;
        Ok(self)
    }

    /// Pass a provider beta token through on every request.
    #[must_use]
    pub fn with_beta(mut self, beta: impl Into<String>) -> Self {
        let beta = beta.into();
        if !self.betas.contains(&beta) {
            self.betas.push(beta);
        }
        self
    }

    /// Request the 1M-token context window.
    #[must_use]
    pub fn with_context_1m(self) -> Self {
        self.with_beta(ANTHROPIC_CONTEXT_1M_BETA)
    }

    /// Replace the retry policy.
    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Replace the prompt-cache marking policy.
    #[must_use]
    pub fn with_prompt_caching(mut self, caching: Option<CacheTtl>) -> Self {
        self.prompt_caching = caching;
        self
    }

    /// Resolve a route against the configured base URL.
    ///
    /// The base path is preserved, so a root of `.../api/v1` and a route of
    /// `chat/completions` address `.../api/v1/chat/completions`.
    pub(crate) fn route(&self, route: &str) -> Result<Url, LlmError> {
        let mut base = self.base_url.clone();
        {
            let mut segments = base
                .path_segments_mut()
                .map_err(|()| LlmError::InvalidBaseUrl)?;
            segments.pop_if_empty();
            for segment in route.split('/') {
                segments.push(segment);
            }
        }
        Ok(base)
    }

    pub(crate) fn beta_header(&self) -> Option<String> {
        if self.betas.is_empty() {
            None
        } else {
            Some(self.betas.join(","))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ANTHROPIC_CONTEXT_1M_BETA, ProviderConfig, ProviderKind};
    use crate::error::LlmError;

    #[test]
    fn routes_preserve_a_base_path() {
        let provider =
            ProviderConfig::openai_compatible("openrouter", "https://openrouter.ai/api/v1", None)
                .expect("provider");
        assert_eq!(
            provider.route("chat/completions").expect("route").as_str(),
            "https://openrouter.ai/api/v1/chat/completions"
        );

        let trailing =
            ProviderConfig::openai_compatible("local", "http://127.0.0.1:1234/v1/", None)
                .expect("provider");
        assert_eq!(
            trailing.route("chat/completions").expect("route").as_str(),
            "http://127.0.0.1:1234/v1/chat/completions"
        );

        let anthropic = ProviderConfig::anthropic("keychain://anthropic/default").expect("config");
        assert_eq!(anthropic.kind, ProviderKind::Anthropic);
        assert_eq!(
            anthropic.route("v1/messages").expect("route").as_str(),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn non_http_base_urls_are_rejected() {
        assert!(matches!(
            ProviderConfig::openai_compatible("bad", "file:///etc/passwd", None),
            Err(LlmError::InvalidBaseUrl)
        ));
        assert!(matches!(
            ProviderConfig::openai_compatible("bad", "not a url", None),
            Err(LlmError::InvalidBaseUrl)
        ));
    }

    #[test]
    fn beta_tokens_are_deduplicated_and_joined() {
        let provider = ProviderConfig::anthropic("keychain://anthropic/default")
            .expect("config")
            .with_context_1m()
            .with_context_1m()
            .with_beta("fine-grained-tool-streaming-2025-05-14");
        assert_eq!(
            provider.beta_header().expect("beta header"),
            format!("{ANTHROPIC_CONTEXT_1M_BETA},fine-grained-tool-streaming-2025-05-14")
        );
        assert!(
            ProviderConfig::anthropic("keychain://anthropic/default")
                .expect("config")
                .beta_header()
                .is_none()
        );
    }
}
