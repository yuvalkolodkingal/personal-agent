//! Native provider layer for the standalone runtime (SPEC-V2 RUN-1).
//!
//! Two async streaming clients live here:
//!
//! * **Anthropic Messages API** — tool use, streamed SSE, prompt-cache
//!   markers, and `anthropic-beta` passthrough including `context-1m`.
//! * **OpenAI-compatible chat completions** — one dialect covering
//!   `OpenRouter`, Ollama, LM Studio, vLLM, and the repository's synthetic
//!   fixture provider.
//!
//! The `OpenAI` *Responses* API is deliberately absent; SPEC-V2 marks it
//! optional and later.
//!
//! Both clients emit the same typed event stream ([`LlmEvent`]), which mirrors
//! the normalized `EventEnvelope` taxonomy the sidecar adapter already
//! produces, so a native turn and a sidecar turn look identical to the event
//! store and the renderer. [`LlmEvent::to_envelope`] performs that conversion.
//!
//! Three invariants shape the public surface:
//!
//! 1. **Keys resolve from keychain aliases only.** [`ApiKey`] can be built
//!    exclusively by [`ApiKey::resolve`] from a `keychain://service/account`
//!    alias; a literal key in configuration is a typed error, not a fallback.
//! 2. **No key material escapes.** Credential headers are marked sensitive,
//!    [`LlmClient`] has a hand-written `Debug`, and every provider-controlled
//!    string is scrubbed of the resolved key and length-bounded before it
//!    reaches an error, a log line, or the UI.
//! 3. **Every turn terminates.** The stream ends with `response.completed` or
//!    `response.failed` — including on abort, where the failure is the abort
//!    itself.
//!
//! ```no_run
//! # async fn run(store: &dyn personal_agent_platform::SecretStore)
//! # -> Result<(), personal_agent_llm::LlmError> {
//! use personal_agent_llm::{ChatRequest, LlmClient, LlmEvent, Message, ProviderConfig};
//!
//! let provider = ProviderConfig::anthropic("keychain://anthropic/default")?.with_context_1m();
//! let client = LlmClient::connect(provider, store)?;
//! let mut turn = client.stream(
//!     ChatRequest::new(personal_agent_llm::CLAUDE_OPUS_5, 4096)
//!         .with_message(Message::user("summarize today's calendar")),
//! )?;
//! while let Some(event) = turn.recv().await {
//!     if let LlmEvent::ResponseDelta { text } = &event {
//!         print!("{text}");
//!     }
//! }
//! # Ok(())
//! # }
//! ```

mod abort;
mod anthropic;
mod client;
mod error;
mod event;
mod message;
mod openai;
mod provider;
mod redact;
mod retry;
mod secret;
mod stream;
mod transport;
mod turn;

pub use abort::AbortHandle;
pub use client::LlmClient;
pub use error::LlmError;
pub use event::{AssistantMessage, FinishReason, LlmEvent, ToolCall, Usage};
pub use message::{ChatRequest, Content, Message, Role, TextBlock, ToolChoice, ToolDefinition};
pub use provider::{
    ANTHROPIC_BASE_URL, ANTHROPIC_CONTEXT_1M_BETA, ANTHROPIC_VERSION, CLAUDE_HAIKU_4_5,
    CLAUDE_OPUS_5, CLAUDE_SONNET_5, CacheTtl, ProviderConfig, ProviderKind,
};
pub use retry::RetryPolicy;
pub use secret::ApiKey;
pub use stream::TurnStream;
