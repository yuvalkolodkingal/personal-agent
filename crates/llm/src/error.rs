//! Provider errors that are safe to log verbatim.
//!
//! Every variant carrying provider-controlled text is constructed through
//! [`crate::redact::scrub`], so `Debug` and `Display` output can be written to
//! the application log or surfaced in the UI without a second review step.

use thiserror::Error;

/// Everything the provider layer can fail with.
#[derive(Debug, Error)]
pub enum LlmError {
    /// A credential was named by something other than a keychain alias.
    #[error("provider key must be a keychain://service/account alias")]
    InvalidKeyAlias,
    /// The OS secret store could not produce the credential.
    #[error("provider credential is unavailable: {0}")]
    Secret(#[from] personal_agent_platform::SecretStoreError),
    /// The configured base URL cannot address the provider route.
    #[error("provider base URL is not a usable HTTP endpoint")]
    InvalidBaseUrl,
    /// The request could not be constructed (bad header value, bad JSON body).
    #[error("provider request is invalid: {0}")]
    Request(String),
    /// The connection failed, timed out, or dropped mid-body.
    #[error("provider transport failed: {0}")]
    Transport(String),
    /// The provider answered with a non-success status.
    #[error("provider returned HTTP {status}: {message}")]
    Status {
        /// HTTP status code as reported by the provider.
        status: u16,
        /// Redacted, length-bounded response body.
        message: String,
    },
    /// The stream did not follow the provider's documented event shape.
    #[error("provider stream is malformed: {0}")]
    Protocol(String),
    /// The caller aborted the turn.
    #[error("turn aborted before completion")]
    Aborted,
}

impl LlmError {
    /// Whether another attempt could plausibly succeed.
    ///
    /// Transport failures and the retry-shaped status codes qualify. An abort
    /// never does: it is an explicit instruction from the caller.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::Status { status, .. } => is_retryable_status(*status),
            _ => false,
        }
    }
}

/// Status codes worth a second attempt: request timeout, conflict, too-early,
/// rate limit, and every server-side failure including 529 `overloaded`.
#[must_use]
pub(crate) fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 425 | 429) || status >= 500
}

#[cfg(test)]
mod tests {
    use super::{LlmError, is_retryable_status};

    #[test]
    fn retryable_classification_matches_provider_guidance() {
        for status in [408, 409, 425, 429, 500, 502, 503, 529] {
            assert!(is_retryable_status(status), "{status} should retry");
        }
        for status in [200, 400, 401, 403, 404, 413, 422] {
            assert!(!is_retryable_status(status), "{status} must not retry");
        }
    }

    #[test]
    fn aborts_and_client_errors_are_terminal() {
        assert!(!LlmError::Aborted.is_retryable());
        assert!(
            !LlmError::Status {
                status: 401,
                message: "unauthorized".to_owned(),
            }
            .is_retryable()
        );
        assert!(LlmError::Transport("connection reset".to_owned()).is_retryable());
    }
}
