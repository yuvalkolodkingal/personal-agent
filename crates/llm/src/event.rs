//! Typed provider events mirroring the normalized `EventEnvelope` taxonomy.
//!
//! The sidecar adapter normalizes upstream provider events into a fixed set of
//! envelope types (`crates/runtime/src/lib.rs`): `response.started`,
//! `response.delta`, `response.retrying`, `response.step_completed`,
//! `response.completed`, `response.failed`, `reasoning.available`,
//! `reasoning.completed`, `tool.started`, `tool.progress`, `tool.completed`,
//! and `tool.failed`. The native provider layer emits the same vocabulary so a
//! native turn and a sidecar turn are indistinguishable to the event store and
//! the renderer.
//!
//! One mapping deserves an explicit note. This layer never *executes* a tool,
//! so it never produces `tool.completed`: that envelope stays reserved for the
//! gateway, exactly as the sidecar mapping reserves it for
//! `session.next.tool.success`. A fully streamed tool call arrives here as
//! [`LlmEvent::ToolCallReady`], which maps to `tool.progress` with
//! `status = "input_complete"` — the same envelope the sidecar mapping uses
//! for `session.next.tool.input.ended`. `tool.failed` is emitted only for a
//! call whose arguments the model did not close as valid JSON.

use personal_agent_contracts::proto::EventEnvelope;
use serde_json::{Value, json};

use crate::error::LlmError;

/// Token and cost accounting for one turn.
///
/// Cost is populated only when the provider reports it (`OpenRouter`'s
/// `usage.cost`); it is never estimated locally from a price table.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// Uncached prompt tokens billed at full input price.
    pub input_tokens: u64,
    /// Generated tokens, including reasoning tokens where the provider bills them as output.
    pub output_tokens: u64,
    /// Prompt tokens served from the provider's cache.
    pub cache_read_input_tokens: u64,
    /// Prompt tokens written into the provider's cache this turn.
    pub cache_creation_input_tokens: u64,
    /// Reasoning tokens, when the provider reports them separately.
    pub reasoning_tokens: u64,
    /// Provider-reported cost in US dollars.
    pub cost_usd: Option<f64>,
}

impl Usage {
    /// Every prompt token processed this turn, cached or not.
    #[must_use]
    pub fn total_input_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_input_tokens)
            .saturating_add(self.cache_creation_input_tokens)
    }

    /// Prompt plus generated tokens.
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens().saturating_add(self.output_tokens)
    }

    fn to_payload(self) -> Value {
        json!({
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "cache_read_input_tokens": self.cache_read_input_tokens,
            "cache_creation_input_tokens": self.cache_creation_input_tokens,
            "reasoning_tokens": self.reasoning_tokens,
            "total_tokens": self.total_tokens(),
            "cost_usd": self.cost_usd,
        })
    }
}

/// Why the provider stopped generating.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FinishReason {
    /// The model ended its turn on its own.
    Stop,
    /// The model wants one or more tools executed.
    ToolCalls,
    /// The output token ceiling was reached.
    MaxTokens,
    /// A safety classifier declined the request.
    Refusal,
    /// The caller aborted the turn.
    Aborted,
    /// A provider-specific reason preserved verbatim.
    Other(String),
}

impl FinishReason {
    /// The wire spelling of this reason.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Stop => "stop",
            Self::ToolCalls => "tool_calls",
            Self::MaxTokens => "max_tokens",
            Self::Refusal => "refusal",
            Self::Aborted => "aborted",
            Self::Other(reason) => reason.as_str(),
        }
    }
}

/// One fully streamed tool call requested by the model.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    /// Provider-assigned call identifier echoed back with the result.
    pub call_id: String,
    /// Tool name as advertised in the request.
    pub tool: String,
    /// Parsed arguments object.
    pub arguments: Value,
}

/// The assistant turn assembled from the stream.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssistantMessage {
    /// Concatenated visible text.
    pub text: String,
    /// Concatenated reasoning summary text, when the provider returns one.
    pub reasoning: String,
    /// Tool calls in the order the model emitted them.
    pub tool_calls: Vec<ToolCall>,
}

/// A single normalized provider event.
#[derive(Clone, Debug, PartialEq)]
pub enum LlmEvent {
    /// The provider accepted the request and began streaming.
    ResponseStarted {
        /// Configured provider identifier.
        provider: String,
        /// Model that is actually serving the turn.
        model: String,
        /// Provider-assigned response identifier, when supplied.
        response_id: Option<String>,
    },
    /// A retryable failure occurred and another attempt is scheduled.
    ResponseRetrying {
        /// One-based number of the attempt that just failed.
        attempt: u32,
        /// Delay before the next attempt.
        delay_ms: u64,
        /// Redacted reason for the retry.
        reason: String,
    },
    /// Visible assistant text.
    ResponseDelta {
        /// Text fragment.
        text: String,
    },
    /// Reasoning summary text, where the provider discloses one.
    ReasoningAvailable {
        /// Reasoning fragment.
        text: String,
    },
    /// The reasoning block closed.
    ReasoningCompleted,
    /// The model named a tool and opened its argument stream.
    ToolCallStarted {
        /// Position of the call within the turn.
        index: u32,
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name.
        tool: String,
    },
    /// A fragment of a tool call's argument JSON.
    ToolCallProgress {
        /// Position of the call within the turn.
        index: u32,
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name.
        tool: String,
        /// Raw argument fragment as sent by the provider.
        arguments_delta: String,
    },
    /// A tool call whose arguments are complete and parsed.
    ToolCallReady {
        /// Position of the call within the turn.
        index: u32,
        /// The assembled call.
        call: ToolCall,
    },
    /// A tool call whose arguments never became valid JSON.
    ToolCallFailed {
        /// Position of the call within the turn.
        index: u32,
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name.
        tool: String,
        /// Redacted parse failure.
        error: String,
    },
    /// The provider reported a stop reason and final accounting for the step.
    ResponseStepCompleted {
        /// Why generation stopped.
        finish_reason: FinishReason,
        /// Token and cost accounting.
        usage: Usage,
    },
    /// Terminal success: the turn is complete.
    ResponseCompleted {
        /// Why generation stopped.
        finish_reason: FinishReason,
        /// Token and cost accounting.
        usage: Usage,
        /// The assembled assistant turn.
        message: Box<AssistantMessage>,
    },
    /// Terminal failure: the turn ended without a usable response.
    ResponseFailed {
        /// Redacted failure description.
        error: String,
        /// Accounting for whatever was billed before the failure.
        usage: Usage,
    },
}

impl LlmEvent {
    /// Normalized envelope type this event maps to.
    #[must_use]
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::ResponseStarted { .. } => "response.started",
            Self::ResponseRetrying { .. } => "response.retrying",
            Self::ResponseDelta { .. } => "response.delta",
            Self::ReasoningAvailable { .. } => "reasoning.available",
            Self::ReasoningCompleted => "reasoning.completed",
            Self::ToolCallStarted { .. } => "tool.started",
            Self::ToolCallProgress { .. } | Self::ToolCallReady { .. } => "tool.progress",
            Self::ToolCallFailed { .. } => "tool.failed",
            Self::ResponseStepCompleted { .. } => "response.step_completed",
            Self::ResponseCompleted { .. } => "response.completed",
            Self::ResponseFailed { .. } => "response.failed",
        }
    }

    /// Whether this event ends the stream.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::ResponseCompleted { .. } | Self::ResponseFailed { .. }
        )
    }

    /// Convert into a persisted [`EventEnvelope`] for the event store.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::Protocol`] when the payload cannot be encoded, which
    /// only happens if a provider supplied a non-finite cost.
    pub fn to_envelope(
        &self,
        sequence: u64,
        origin: &str,
        profile_id: &str,
        session_id: &str,
    ) -> Result<EventEnvelope, LlmError> {
        let mut envelope = EventEnvelope::new(
            sequence,
            origin,
            profile_id,
            self.event_type(),
            &self.payload(),
        )
        .map_err(|error| LlmError::Protocol(error.to_string()))?;
        envelope.session_id = Some(session_id.to_owned());
        Ok(envelope)
    }

    fn payload(&self) -> Value {
        match self {
            Self::ResponseStarted {
                provider,
                model,
                response_id,
            } => json!({"provider": provider, "model": model, "responseID": response_id}),
            Self::ResponseRetrying {
                attempt,
                delay_ms,
                reason,
            } => json!({"attempt": attempt, "next": delay_ms, "message": reason}),
            Self::ResponseDelta { text } => json!({"delta": text}),
            Self::ReasoningAvailable { text } => json!({"delta": text, "reasoning": true}),
            Self::ReasoningCompleted => json!({}),
            Self::ToolCallStarted {
                index,
                call_id,
                tool,
            } => json!({"index": index, "callID": call_id, "tool": tool}),
            Self::ToolCallProgress {
                index,
                call_id,
                tool,
                arguments_delta,
            } => json!({
                "index": index,
                "callID": call_id,
                "tool": tool,
                "status": "input_delta",
                "bytes": arguments_delta.len(),
            }),
            Self::ToolCallReady { index, call } => json!({
                "index": index,
                "callID": call.call_id,
                "tool": call.tool,
                "status": "input_complete",
            }),
            Self::ToolCallFailed {
                index,
                call_id,
                tool,
                error,
            } => json!({"index": index, "callID": call_id, "tool": tool, "error": error}),
            Self::ResponseStepCompleted {
                finish_reason,
                usage,
            } => json!({"finish": finish_reason.as_str(), "tokens": usage.to_payload()}),
            Self::ResponseCompleted {
                finish_reason,
                usage,
                message,
            } => json!({
                "terminal": true,
                "finish": finish_reason.as_str(),
                "tokens": usage.to_payload(),
                "toolCalls": message.tool_calls.len(),
            }),
            Self::ResponseFailed { error, usage } => {
                json!({"terminal": true, "error": error, "tokens": usage.to_payload()})
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AssistantMessage, FinishReason, LlmEvent, ToolCall, Usage};
    use serde_json::json;

    #[test]
    fn envelope_types_stay_inside_the_normalized_taxonomy() {
        let normalized = [
            "response.started",
            "response.retrying",
            "response.delta",
            "reasoning.available",
            "reasoning.completed",
            "tool.started",
            "tool.progress",
            "tool.failed",
            "response.step_completed",
            "response.completed",
            "response.failed",
        ];
        let events = [
            LlmEvent::ResponseStarted {
                provider: "anthropic".to_owned(),
                model: "claude-opus-5".to_owned(),
                response_id: None,
            },
            LlmEvent::ResponseRetrying {
                attempt: 1,
                delay_ms: 250,
                reason: "HTTP 529".to_owned(),
            },
            LlmEvent::ResponseDelta {
                text: "hi".to_owned(),
            },
            LlmEvent::ReasoningAvailable {
                text: "thinking".to_owned(),
            },
            LlmEvent::ReasoningCompleted,
            LlmEvent::ToolCallStarted {
                index: 0,
                call_id: "call_1".to_owned(),
                tool: "write".to_owned(),
            },
            LlmEvent::ToolCallProgress {
                index: 0,
                call_id: "call_1".to_owned(),
                tool: "write".to_owned(),
                arguments_delta: "{}".to_owned(),
            },
            LlmEvent::ToolCallReady {
                index: 0,
                call: ToolCall {
                    call_id: "call_1".to_owned(),
                    tool: "write".to_owned(),
                    arguments: json!({}),
                },
            },
            LlmEvent::ToolCallFailed {
                index: 0,
                call_id: "call_1".to_owned(),
                tool: "write".to_owned(),
                error: "unterminated object".to_owned(),
            },
            LlmEvent::ResponseStepCompleted {
                finish_reason: FinishReason::ToolCalls,
                usage: Usage::default(),
            },
            LlmEvent::ResponseCompleted {
                finish_reason: FinishReason::Stop,
                usage: Usage::default(),
                message: Box::new(AssistantMessage::default()),
            },
            LlmEvent::ResponseFailed {
                error: "boom".to_owned(),
                usage: Usage::default(),
            },
        ];
        for event in &events {
            assert!(
                normalized.contains(&event.event_type()),
                "{} is outside the normalized taxonomy",
                event.event_type()
            );
        }
        assert!(events[10].is_terminal());
        assert!(events[11].is_terminal());
        assert!(!events[0].is_terminal());
    }

    #[test]
    fn envelopes_carry_session_and_usage() {
        let usage = Usage {
            input_tokens: 11,
            output_tokens: 7,
            cache_read_input_tokens: 3,
            cost_usd: Some(0.000_25),
            ..Usage::default()
        };
        let event = LlmEvent::ResponseCompleted {
            finish_reason: FinishReason::Stop,
            usage,
            message: Box::new(AssistantMessage::default()),
        };
        let envelope = event
            .to_envelope(4, "llm", "default", "session-7")
            .expect("envelope");
        assert_eq!(envelope.r#type, "response.completed");
        assert_eq!(envelope.session_id.as_deref(), Some("session-7"));
        assert_eq!(envelope.monotonic_sequence, 4);
        let payload = envelope.payload().expect("payload");
        assert_eq!(payload["tokens"]["total_tokens"], json!(21));
        assert_eq!(payload["tokens"]["cost_usd"], json!(0.000_25));
        assert_eq!(payload["terminal"], json!(true));
    }

    #[test]
    fn usage_totals_are_saturating() {
        let usage = Usage {
            input_tokens: u64::MAX,
            output_tokens: 5,
            cache_read_input_tokens: 5,
            ..Usage::default()
        };
        assert_eq!(usage.total_tokens(), u64::MAX);
    }
}
