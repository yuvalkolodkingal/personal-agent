//! Anthropic Messages API.
//!
//! `POST {base}/v1/messages` with `stream: true`. Tool use, prompt-cache
//! markers, and beta passthrough (notably `context-1m-2025-08-07`) are all
//! request-level concerns handled here; the streamed event vocabulary is
//! translated in [`Decoder`].
//!
//! Prompt caching is generally available and needs no beta token: marking a
//! block with `cache_control` is the whole opt-in. The `anthropic-beta` header
//! is therefore a pure passthrough of whatever the provider configuration
//! lists, which keeps this module from having to track the beta calendar.

use std::collections::BTreeMap;

use reqwest::header::{ACCEPT, HeaderMap, HeaderName, HeaderValue};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Map, Value, json};

use crate::error::LlmError;
use crate::event::{FinishReason, LlmEvent};
use crate::message::{ChatRequest, Content, Message, Role, ToolChoice};
use crate::provider::{ANTHROPIC_VERSION, CacheTtl, ProviderConfig};
use crate::turn::TurnState;

/// Route appended to the provider's base URL.
pub(crate) const ROUTE: &str = "v1/messages";

const API_KEY_HEADER: &str = "x-api-key";
const VERSION_HEADER: &str = "anthropic-version";
const BETA_HEADER: &str = "anthropic-beta";

fn cache_control(caching: Option<CacheTtl>) -> Option<Value> {
    let ttl = caching?;
    Some(match ttl.wire_value() {
        Some(ttl) => json!({"type": "ephemeral", "ttl": ttl}),
        None => json!({"type": "ephemeral"}),
    })
}

/// Build the streamed Messages API body.
pub(crate) fn body(provider: &ProviderConfig, request: &ChatRequest) -> Value {
    let caching = provider.prompt_caching;
    let mut body = Map::new();
    body.insert("model".to_owned(), json!(request.model));
    body.insert("stream".to_owned(), json!(true));
    body.insert("max_tokens".to_owned(), json!(request.max_output_tokens));
    if !request.system.is_empty() {
        let system: Vec<Value> = request
            .system
            .iter()
            .map(|block| {
                let mut rendered = json!({"type": "text", "text": block.text});
                mark_cacheable(&mut rendered, block.cache, caching);
                rendered
            })
            .collect();
        body.insert("system".to_owned(), Value::Array(system));
    }
    body.insert(
        "messages".to_owned(),
        Value::Array(request.messages.iter().map(render_message).collect()),
    );
    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                let mut rendered = json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.input_schema,
                });
                mark_cacheable(&mut rendered, tool.cache, caching);
                rendered
            })
            .collect();
        body.insert("tools".to_owned(), Value::Array(tools));
        body.insert("tool_choice".to_owned(), tool_choice(&request.tool_choice));
    }
    Value::Object(body)
}

fn mark_cacheable(block: &mut Value, requested: bool, caching: Option<CacheTtl>) {
    if !requested {
        return;
    }
    let (Some(control), Some(object)) = (cache_control(caching), block.as_object_mut()) else {
        return;
    };
    object.insert("cache_control".to_owned(), control);
}

fn tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!({"type": "auto"}),
        ToolChoice::Any => json!({"type": "any"}),
        ToolChoice::None => json!({"type": "none"}),
        ToolChoice::Named(name) => json!({"type": "tool", "name": name}),
    }
}

fn render_message(message: &Message) -> Value {
    let content: Vec<Value> = message
        .content
        .iter()
        .map(|block| match block {
            Content::Text(text) => json!({"type": "text", "text": text.text}),
            Content::ToolUse {
                call_id,
                tool,
                arguments,
            } => json!({"type": "tool_use", "id": call_id, "name": tool, "input": arguments}),
            Content::ToolResult {
                call_id,
                content,
                is_error,
            } => json!({
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": content,
                "is_error": is_error,
            }),
        })
        .collect();
    let role = match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    json!({"role": role, "content": content})
}

/// Build request headers, marking the credential header as sensitive so it is
/// never rendered by `reqwest`'s own `Debug` output.
pub(crate) fn headers(
    provider: &ProviderConfig,
    key: Option<&SecretString>,
) -> Result<HeaderMap, LlmError> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    headers.insert(
        HeaderName::from_static(VERSION_HEADER),
        HeaderValue::from_static(ANTHROPIC_VERSION),
    );
    if let Some(key) = key {
        let mut credential = HeaderValue::from_str(key.expose_secret())
            .map_err(|_| LlmError::Request("credential is not a valid header".to_owned()))?;
        credential.set_sensitive(true);
        headers.insert(HeaderName::from_static(API_KEY_HEADER), credential);
    }
    if let Some(betas) = provider.beta_header() {
        let value = HeaderValue::from_str(&betas)
            .map_err(|_| LlmError::Request("beta header is not a valid header".to_owned()))?;
        headers.insert(HeaderName::from_static(BETA_HEADER), value);
    }
    crate::transport::apply_extra_headers(&mut headers, provider)?;
    Ok(headers)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockKind {
    Text,
    Thinking,
    ToolUse,
    Other,
}

/// Incremental decoder for Messages API stream events.
#[derive(Debug)]
pub(crate) struct Decoder {
    state: TurnState,
    blocks: BTreeMap<usize, BlockKind>,
}

impl Decoder {
    pub(crate) fn new(provider: &str, model: &str) -> Self {
        Self {
            state: TurnState::new(provider, model),
            blocks: BTreeMap::new(),
        }
    }

    pub(crate) fn state_mut(&mut self) -> &mut TurnState {
        &mut self.state
    }

    /// Consume one SSE frame.
    ///
    /// The `event:` name is advisory: the payload repeats it in `type`, which
    /// is what this decoder switches on, so a proxy that drops event names
    /// still decodes correctly.
    pub(crate) fn push(&mut self, data: &str) -> Result<Vec<LlmEvent>, LlmError> {
        let frame: Value = serde_json::from_str(data)
            .map_err(|error| LlmError::Protocol(format!("event is not JSON: {error}")))?;
        let index = frame
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())
            .unwrap_or(0);
        match frame
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "message_start" => Ok(self.message_start(&frame)),
            "content_block_start" => Ok(self.block_start(index, &frame)),
            "content_block_delta" => Ok(self.block_delta(index, &frame)),
            "content_block_stop" => Ok(self.block_stop(index)),
            "message_delta" => Ok(self.message_delta(&frame)),
            "message_stop" => Ok(self.state.complete()),
            "error" => Ok(self.state.fail(stream_error(&frame))),
            _ => Ok(Vec::new()),
        }
    }

    fn message_start(&mut self, frame: &Value) -> Vec<LlmEvent> {
        let message = frame.get("message");
        let events = self.state.start(
            message
                .and_then(|message| message.get("model"))
                .and_then(Value::as_str),
            message
                .and_then(|message| message.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned),
        );
        if let Some(usage) = message.and_then(|message| message.get("usage")) {
            let target = self.state.usage_mut();
            let number = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
            target.input_tokens = number("input_tokens");
            target.cache_read_input_tokens = number("cache_read_input_tokens");
            target.cache_creation_input_tokens = number("cache_creation_input_tokens");
            target.output_tokens = number("output_tokens");
        }
        events
    }

    fn block_start(&mut self, index: usize, frame: &Value) -> Vec<LlmEvent> {
        let block = frame.get("content_block");
        let kind = match block
            .and_then(|block| block.get("type"))
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "text" => BlockKind::Text,
            "thinking" | "redacted_thinking" => BlockKind::Thinking,
            "tool_use" => BlockKind::ToolUse,
            _ => BlockKind::Other,
        };
        self.blocks.insert(index, kind);
        if kind != BlockKind::ToolUse {
            return Vec::new();
        }
        self.state.announce(
            index,
            block
                .and_then(|block| block.get("id"))
                .and_then(Value::as_str),
            block
                .and_then(|block| block.get("name"))
                .and_then(Value::as_str),
        )
    }

    fn block_delta(&mut self, index: usize, frame: &Value) -> Vec<LlmEvent> {
        let Some(delta) = frame.get("delta") else {
            return Vec::new();
        };
        let text = |key: &str| delta.get(key).and_then(Value::as_str).unwrap_or_default();
        match delta
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "text_delta" => self.state.text(text("text")),
            "thinking_delta" => self.state.reasoning(text("thinking")),
            "input_json_delta" => self.state.arguments(index, text("partial_json")),
            _ => Vec::new(),
        }
    }

    fn block_stop(&mut self, index: usize) -> Vec<LlmEvent> {
        match self.blocks.get(&index) {
            Some(BlockKind::ToolUse) => self.state.settle(index),
            Some(BlockKind::Thinking) => vec![LlmEvent::ReasoningCompleted],
            _ => Vec::new(),
        }
    }

    fn message_delta(&mut self, frame: &Value) -> Vec<LlmEvent> {
        if let Some(reason) = frame
            .get("delta")
            .and_then(|delta| delta.get("stop_reason"))
            .and_then(Value::as_str)
        {
            self.state.set_finish(finish_reason(reason));
        }
        if let Some(usage) = frame.get("usage") {
            let target = self.state.usage_mut();
            if let Some(output) = usage.get("output_tokens").and_then(Value::as_u64) {
                target.output_tokens = output;
            }
            if let Some(input) = usage.get("input_tokens").and_then(Value::as_u64) {
                target.input_tokens = input;
            }
        }
        Vec::new()
    }
}

fn stream_error(frame: &Value) -> String {
    frame
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("provider reported a stream error")
        .to_owned()
}

fn finish_reason(reason: &str) -> FinishReason {
    match reason {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "tool_use" => FinishReason::ToolCalls,
        "max_tokens" => FinishReason::MaxTokens,
        "refusal" => FinishReason::Refusal,
        other => FinishReason::Other(other.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::{Decoder, ROUTE, body, headers};
    use crate::event::{FinishReason, LlmEvent};
    use crate::message::{ChatRequest, Message, TextBlock, ToolChoice, ToolDefinition};
    use crate::provider::{ANTHROPIC_CONTEXT_1M_BETA, CacheTtl, ProviderConfig};
    use secrecy::SecretString;
    use serde_json::json;

    fn provider() -> ProviderConfig {
        ProviderConfig::anthropic("keychain://anthropic/default").expect("provider")
    }

    #[test]
    fn body_marks_cacheable_prefixes_and_tools() {
        let mut request = ChatRequest::new("claude-opus-5", 4096)
            .with_message(Message::user("hello"))
            .with_tool(ToolDefinition::new(
                "write",
                "write",
                json!({"type": "object"}),
            ));
        request.system = vec![TextBlock::cached("large stable preamble")];
        request.tools[0].cache = true;
        request.tool_choice = ToolChoice::Named("write".to_owned());

        let provider = provider().with_prompt_caching(Some(CacheTtl::OneHour));
        let rendered = body(&provider, &request);
        assert_eq!(rendered["model"], json!("claude-opus-5"));
        assert_eq!(rendered["stream"], json!(true));
        assert_eq!(
            rendered["system"][0]["cache_control"],
            json!({"type": "ephemeral", "ttl": "1h"})
        );
        assert_eq!(
            rendered["tools"][0]["cache_control"],
            json!({"type": "ephemeral", "ttl": "1h"})
        );
        assert_eq!(
            rendered["tool_choice"],
            json!({"type": "tool", "name": "write"})
        );

        let uncached = body(&provider.with_prompt_caching(None), &request);
        assert!(uncached["system"][0].get("cache_control").is_none());
        assert!(uncached["tools"][0].get("cache_control").is_none());
    }

    #[test]
    fn headers_pin_the_version_and_pass_betas_through() {
        let key = SecretString::from("fixture-provider-token-1234");
        let headers = headers(&provider().with_context_1m(), Some(&key)).expect("headers");
        assert_eq!(
            headers.get("anthropic-version").expect("version"),
            "2023-06-01"
        );
        assert_eq!(
            headers.get("anthropic-beta").expect("beta"),
            ANTHROPIC_CONTEXT_1M_BETA
        );
        let credential = headers.get("x-api-key").expect("credential");
        assert!(credential.is_sensitive());
        assert!(!format!("{headers:?}").contains("fixture-provider-token-1234"));
        assert_eq!(ROUTE, "v1/messages");
    }

    #[test]
    fn decodes_a_streamed_tool_use_turn_with_thinking() {
        let mut decoder = Decoder::new("anthropic", "claude-opus-5");
        let frames = [
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-opus-5","usage":{"input_tokens":30,"cache_read_input_tokens":12,"cache_creation_input_tokens":4,"output_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"weighing"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"writing"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"write"}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.txt\"}"}}"#,
            r#"{"type":"content_block_stop","index":2}"#,
            r#"{"type":"ping"}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}}"#,
            r#"{"type":"message_stop"}"#,
        ];
        let mut events = Vec::new();
        for frame in frames {
            events.extend(decoder.push(frame).expect("frame decodes"));
        }
        let types: Vec<&str> = events.iter().map(LlmEvent::event_type).collect();
        assert_eq!(
            types,
            [
                "response.started",
                "reasoning.available",
                "reasoning.completed",
                "response.delta",
                "tool.started",
                "tool.progress",
                "tool.progress",
                "response.step_completed",
                "response.completed",
            ]
        );
        let LlmEvent::ResponseCompleted {
            usage,
            message,
            finish_reason,
        } = events.last().expect("terminal event")
        else {
            panic!("expected a terminal completion");
        };
        assert_eq!(*finish_reason, FinishReason::ToolCalls);
        assert_eq!(usage.input_tokens, 30);
        assert_eq!(usage.cache_read_input_tokens, 12);
        assert_eq!(usage.cache_creation_input_tokens, 4);
        assert_eq!(usage.output_tokens, 42);
        assert_eq!(usage.total_tokens(), 88);
        assert_eq!(message.text, "writing");
        assert_eq!(message.reasoning, "weighing");
        assert_eq!(message.tool_calls[0].call_id, "toolu_1");
        assert_eq!(message.tool_calls[0].arguments, json!({"path": "a.txt"}));
    }

    #[test]
    fn stream_errors_terminate_the_turn() {
        let mut decoder = Decoder::new("anthropic", "claude-opus-5");
        let events = decoder
            .push(r#"{"type":"error","error":{"type":"overloaded_error","message":"overloaded"}}"#)
            .expect("frame decodes");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), "response.failed");
        assert!(decoder.push("{").is_err());
    }
}
