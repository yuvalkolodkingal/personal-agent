//! HTTP attempt loop: request construction, retry/backoff, SSE reading, abort.
//!
//! Exactly one attempt is in flight at a time. An attempt that fails *before*
//! producing any event may be retried; an attempt that already streamed output
//! never is, because re-running it would duplicate assistant text in the
//! caller's transcript. Mid-stream failures are reported as a terminal
//! `response.failed` event instead.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use secrecy::SecretString;
use serde_json::Value;
use sse_stream::SseStream;
use tokio::sync::mpsc;

use crate::abort::AbortHandle;
use crate::error::LlmError;
use crate::event::LlmEvent;
use crate::message::ChatRequest;
use crate::provider::{ProviderConfig, ProviderKind};
use crate::redact::scrub;
use crate::secret::ApiKey;
use crate::turn::TurnState;
use crate::{anthropic, openai};

/// Copy provider-configured static headers onto a request.
pub(crate) fn apply_extra_headers(
    headers: &mut HeaderMap,
    provider: &ProviderConfig,
) -> Result<(), LlmError> {
    for (name, value) in &provider.extra_headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| LlmError::Request(format!("header name {name} is invalid")))?;
        let value = HeaderValue::from_str(value)
            .map_err(|_| LlmError::Request("header value is invalid".to_owned()))?;
        headers.insert(name, value);
    }
    Ok(())
}

/// Wire dialect decoder selected by [`ProviderKind`].
#[derive(Debug)]
enum Decoder {
    Anthropic(Box<anthropic::Decoder>),
    OpenAi(Box<openai::Decoder>),
}

impl Decoder {
    fn new(provider: &ProviderConfig, model: &str) -> Self {
        match provider.kind {
            ProviderKind::Anthropic => {
                Self::Anthropic(Box::new(anthropic::Decoder::new(&provider.id, model)))
            }
            ProviderKind::OpenAiCompatible => {
                Self::OpenAi(Box::new(openai::Decoder::new(&provider.id, model)))
            }
        }
    }

    fn push(&mut self, data: &str) -> Result<Vec<LlmEvent>, LlmError> {
        match self {
            Self::Anthropic(decoder) => decoder.push(data),
            Self::OpenAi(decoder) => decoder.push(data),
        }
    }

    fn state_mut(&mut self) -> &mut TurnState {
        match self {
            Self::Anthropic(decoder) => decoder.state_mut(),
            Self::OpenAi(decoder) => decoder.state_mut(),
        }
    }
}

/// One streamed turn, retried according to the provider's policy.
pub(crate) struct TurnDriver {
    pub(crate) http: reqwest::Client,
    pub(crate) provider: Arc<ProviderConfig>,
    pub(crate) key: Option<ApiKey>,
    pub(crate) request: ChatRequest,
    pub(crate) abort: AbortHandle,
    pub(crate) events: mpsc::Sender<LlmEvent>,
}

/// Outcome of one attempt.
enum Attempt {
    /// A terminal event was already delivered; do not retry.
    Terminal,
    /// The attempt failed before producing output and may be retried.
    Failed(LlmError),
}

impl TurnDriver {
    pub(crate) async fn run(mut self) {
        let attempts = self.provider.retry.max_attempts.max(1);
        for attempt in 1..=attempts {
            if self.abort.is_aborted() {
                self.emit_failure(&LlmError::Aborted).await;
                return;
            }
            let error = match self.attempt().await {
                Attempt::Terminal => return,
                Attempt::Failed(error) => error,
            };
            let retryable = error.is_retryable() && attempt < attempts;
            if !retryable {
                self.emit_failure(&error).await;
                return;
            }
            let delay = self
                .provider
                .retry
                .delay_after(attempt, retry_after(&error));
            let reason = error.to_string();
            tracing::warn!(
                provider = %self.provider.id,
                attempt,
                delay_ms = delay.as_millis(),
                reason = %reason,
                "retrying provider turn"
            );
            let delivered = self
                .emit(vec![LlmEvent::ResponseRetrying {
                    attempt,
                    delay_ms: u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                    reason,
                }])
                .await;
            if !delivered || !self.sleep(delay).await {
                self.emit_failure(&LlmError::Aborted).await;
                return;
            }
        }
    }

    /// Wait out a backoff delay, returning `false` if the turn was aborted.
    async fn sleep(&self, delay: Duration) -> bool {
        tokio::select! {
            biased;
            () = self.abort.cancelled() => false,
            () = tokio::time::sleep(delay) => true,
        }
    }

    async fn emit(&self, events: Vec<LlmEvent>) -> bool {
        for event in events {
            if self.events.send(event).await.is_err() {
                return false;
            }
        }
        true
    }

    async fn emit_failure(&self, error: &LlmError) {
        self.emit(vec![LlmEvent::ResponseFailed {
            error: error.to_string(),
            usage: crate::event::Usage::default(),
        }])
        .await;
    }

    fn key_material(&self) -> Option<&SecretString> {
        self.key.as_ref().map(ApiKey::secret)
    }

    fn build(&self) -> Result<(reqwest::RequestBuilder, Value), LlmError> {
        let (route, headers, body) = match self.provider.kind {
            ProviderKind::Anthropic => (
                anthropic::ROUTE,
                anthropic::headers(&self.provider, self.key_material())?,
                anthropic::body(&self.provider, &self.request),
            ),
            ProviderKind::OpenAiCompatible => (
                openai::ROUTE,
                openai::headers(&self.provider, self.key_material())?,
                openai::body(&self.request),
            ),
        };
        let url = self.provider.route(route)?;
        Ok((self.http.post(url).headers(headers), body))
    }

    async fn attempt(&mut self) -> Attempt {
        let (builder, body) = match self.build() {
            Ok(built) => built,
            Err(error) => return Attempt::Failed(error),
        };
        let response = match builder.json(&body).send().await {
            Ok(response) => response,
            Err(error) => {
                return Attempt::Failed(LlmError::Transport(self.clean(&error.to_string())));
            }
        };
        let status = response.status();
        if !status.is_success() {
            return Attempt::Failed(self.status_error(response).await);
        }
        self.read_stream(response).await
    }

    async fn status_error(&self, response: reqwest::Response) -> LlmError {
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let message = response.text().await.unwrap_or_default();
        let mut message = self.clean(&message);
        if let Some(retry_after) = retry_after {
            // Encoded in the message so the retry scheduler can recover it
            // without widening the error type.
            let _ = write!(message, " [retry-after: {}s]", retry_after.as_secs());
        }
        LlmError::Status { status, message }
    }

    fn clean(&self, text: &str) -> String {
        scrub(text, self.key_material())
    }

    async fn read_stream(&mut self, response: reqwest::Response) -> Attempt {
        let mut decoder = Decoder::new(&self.provider, &self.request.model);
        let mut frames = SseStream::from_bytes_stream(response.bytes_stream());
        loop {
            let frame = tokio::select! {
                biased;
                () = self.abort.cancelled() => {
                    let events = decoder.state_mut().fail(LlmError::Aborted.to_string());
                    self.emit(events).await;
                    return Attempt::Terminal;
                }
                frame = frames.next() => frame,
            };
            let (events, exhausted) = match frame {
                Some(Ok(frame)) => match frame.data {
                    Some(data) => match decoder.push(&data) {
                        Ok(events) => (events, false),
                        Err(error) => (
                            decoder.state_mut().fail(self.clean(&error.to_string())),
                            false,
                        ),
                    },
                    None => (Vec::new(), false),
                },
                Some(Err(error)) => (
                    decoder
                        .state_mut()
                        .fail(self.clean(&format!("stream ended early: {error}"))),
                    false,
                ),
                // The body ended. A stream that already reported a stop reason
                // is complete even without a closing frame (minimal local
                // servers omit `[DONE]`); one that did not was truncated, and
                // reporting that as success would hand the engine a silently
                // half-written turn.
                None => {
                    let state = decoder.state_mut();
                    let events = if state.has_finish() {
                        state.complete()
                    } else {
                        state
                            .fail("provider closed the stream before finishing the turn".to_owned())
                    };
                    (events, true)
                }
            };
            let terminal = events.iter().any(LlmEvent::is_terminal);
            if !self.emit(events).await || terminal || exhausted {
                return Attempt::Terminal;
            }
        }
    }
}

/// Recover a `retry-after` hint from a status error message.
fn retry_after(error: &LlmError) -> Option<Duration> {
    let LlmError::Status { message, .. } = error else {
        return None;
    };
    let marker = message.rfind("[retry-after: ")?;
    let tail = &message[marker + "[retry-after: ".len()..];
    let seconds = tail.split('s').next()?;
    seconds.parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::{apply_extra_headers, retry_after};
    use crate::error::LlmError;
    use crate::provider::ProviderConfig;
    use reqwest::header::HeaderMap;
    use std::time::Duration;

    #[test]
    fn extra_headers_are_validated() {
        let mut provider =
            ProviderConfig::openai_compatible("openrouter", "https://openrouter.ai/api/v1", None)
                .expect("provider");
        provider
            .extra_headers
            .insert("x-title".to_owned(), "Personal Agent".to_owned());
        let mut headers = HeaderMap::new();
        apply_extra_headers(&mut headers, &provider).expect("valid headers");
        assert_eq!(headers.get("x-title").expect("header"), "Personal Agent");

        provider
            .extra_headers
            .insert("bad header".to_owned(), "value".to_owned());
        assert!(matches!(
            apply_extra_headers(&mut HeaderMap::new(), &provider),
            Err(LlmError::Request(_))
        ));
    }

    #[test]
    fn retry_after_round_trips_through_the_status_message() {
        let error = LlmError::Status {
            status: 429,
            message: "slow down [retry-after: 3s]".to_owned(),
        };
        assert_eq!(retry_after(&error), Some(Duration::from_secs(3)));
        assert_eq!(
            retry_after(&LlmError::Status {
                status: 429,
                message: "slow down".to_owned(),
            }),
            None
        );
        assert_eq!(retry_after(&LlmError::Aborted), None);
    }
}
