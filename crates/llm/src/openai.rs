//! OpenAI-compatible chat completions.
//!
//! One dialect covers `OpenRouter`, Ollama, LM Studio, vLLM, and the
//! repository's synthetic fixture provider: `POST {base}/chat/completions`
//! with `stream: true`, function tools, and `stream_options.include_usage` so
//! the terminal chunk carries token accounting (and, on `OpenRouter`, a
//! provider-reported `usage.cost`).

use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Map, Value, json};

use crate::error::LlmError;
use crate::event::{FinishReason, LlmEvent, Usage};
use crate::message::{ChatRequest, Content, Message, Role, ToolChoice};
use crate::provider::ProviderConfig;
use crate::turn::TurnState;

/// Route appended to the provider's base URL.
pub(crate) const ROUTE: &str = "chat/completions";

/// Build the streamed chat-completions body.
pub(crate) fn body(request: &ChatRequest) -> Value {
    let mut body = Map::new();
    body.insert("model".to_owned(), json!(request.model));
    body.insert("stream".to_owned(), json!(true));
    body.insert("stream_options".to_owned(), json!({"include_usage": true}));
    body.insert("max_tokens".to_owned(), json!(request.max_output_tokens));
    body.insert("messages".to_owned(), json!(messages(request)));
    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema,
                    },
                })
            })
            .collect();
        body.insert("tools".to_owned(), Value::Array(tools));
        body.insert("tool_choice".to_owned(), tool_choice(&request.tool_choice));
    }
    Value::Object(body)
}

fn tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::Any => json!("required"),
        ToolChoice::None => json!("none"),
        ToolChoice::Named(name) => json!({"type": "function", "function": {"name": name}}),
    }
}

fn messages(request: &ChatRequest) -> Vec<Value> {
    let mut messages = Vec::new();
    let system = request
        .system
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if !system.is_empty() {
        messages.push(json!({"role": "system", "content": system}));
    }
    for message in &request.messages {
        render_message(message, &mut messages);
    }
    messages
}

/// Render one neutral message as one or more chat-completions messages.
///
/// Tool results are their own `role: "tool"` messages in this dialect, so a
/// user turn that carries results expands into several wire messages.
fn render_message(message: &Message, out: &mut Vec<Value>) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for block in &message.content {
        match block {
            Content::Text(block) => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&block.text);
            }
            Content::ToolUse {
                call_id,
                tool,
                arguments,
            } => tool_calls.push(json!({
                "id": call_id,
                "type": "function",
                "function": {"name": tool, "arguments": arguments.to_string()},
            })),
            Content::ToolResult {
                call_id,
                content,
                is_error,
            } => out.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": if *is_error { format!("ERROR: {content}") } else { content.clone() },
            })),
        }
    }
    if text.is_empty() && tool_calls.is_empty() {
        return;
    }
    let role = match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let mut rendered = Map::new();
    rendered.insert("role".to_owned(), json!(role));
    rendered.insert("content".to_owned(), json!(text));
    if !tool_calls.is_empty() {
        rendered.insert("tool_calls".to_owned(), Value::Array(tool_calls));
    }
    out.push(Value::Object(rendered));
}

/// Build request headers, marking the credential header as sensitive so it is
/// never rendered by `reqwest`'s own `Debug` output.
pub(crate) fn headers(
    provider: &ProviderConfig,
    key: Option<&SecretString>,
) -> Result<HeaderMap, LlmError> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    if let Some(key) = key {
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {}", key.expose_secret()))
                .map_err(|_| LlmError::Request("credential is not a valid header".to_owned()))?;
        authorization.set_sensitive(true);
        headers.insert(AUTHORIZATION, authorization);
    }
    crate::transport::apply_extra_headers(&mut headers, provider)?;
    Ok(headers)
}

/// Incremental decoder for `chat.completion.chunk` frames.
#[derive(Debug)]
pub(crate) struct Decoder {
    state: TurnState,
}

impl Decoder {
    pub(crate) fn new(provider: &str, model: &str) -> Self {
        Self {
            state: TurnState::new(provider, model),
        }
    }

    pub(crate) fn state_mut(&mut self) -> &mut TurnState {
        &mut self.state
    }

    /// Consume one `data:` payload.
    pub(crate) fn push(&mut self, data: &str) -> Result<Vec<LlmEvent>, LlmError> {
        if data.trim() == "[DONE]" {
            return Ok(self.state.complete());
        }
        let chunk: Value = serde_json::from_str(data)
            .map_err(|error| LlmError::Protocol(format!("chunk is not JSON: {error}")))?;
        if let Some(error) = chunk.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("provider reported a stream error");
            return Ok(self.state.fail(message.to_owned()));
        }
        let mut events = self.state.start(
            chunk.get("model").and_then(Value::as_str),
            chunk.get("id").and_then(Value::as_str).map(str::to_owned),
        );
        if let Some(usage) = chunk.get("usage").filter(|usage| usage.is_object()) {
            merge_usage(self.state.usage_mut(), usage);
        }
        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return Ok(events);
        };
        if let Some(delta) = choice.get("delta") {
            events.extend(self.delta(delta));
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.state.set_finish(finish_reason(reason));
        }
        Ok(events)
    }

    fn delta(&mut self, delta: &Value) -> Vec<LlmEvent> {
        let mut events = Vec::new();
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            events.extend(self.state.text(text));
        }
        for field in ["reasoning", "reasoning_content"] {
            if let Some(text) = delta.get(field).and_then(Value::as_str) {
                events.extend(self.state.reasoning(text));
            }
        }
        for (position, call) in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            events.extend(self.tool_call(position, call));
        }
        events
    }

    fn tool_call(&mut self, position: usize, call: &Value) -> Vec<LlmEvent> {
        let index = call
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())
            .unwrap_or(position);
        let function = call.get("function");
        let mut events = self.state.announce(
            index,
            call.get("id").and_then(Value::as_str),
            function
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str),
        );
        if let Some(arguments) = function
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
        {
            events.extend(self.state.arguments(index, arguments));
        }
        events
    }
}

fn finish_reason(reason: &str) -> FinishReason {
    match reason {
        "stop" | "end_turn" => FinishReason::Stop,
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "length" | "max_tokens" => FinishReason::MaxTokens,
        "content_filter" | "refusal" => FinishReason::Refusal,
        other => FinishReason::Other(other.to_owned()),
    }
}

/// Merge an OpenAI-shaped `usage` object, including the cache and reasoning
/// extensions and `OpenRouter`'s provider-reported cost.
fn merge_usage(usage: &mut Usage, reported: &Value) {
    let number = |path: &[&str]| -> Option<u64> {
        let mut cursor = reported;
        for key in path {
            cursor = cursor.get(*key)?;
        }
        cursor.as_u64()
    };
    if let Some(value) = number(&["prompt_tokens"]) {
        let cached = number(&["prompt_tokens_details", "cached_tokens"]).unwrap_or(0);
        usage.input_tokens = value.saturating_sub(cached);
        usage.cache_read_input_tokens = cached;
    }
    if let Some(value) = number(&["completion_tokens"]) {
        usage.output_tokens = value;
    }
    if let Some(value) = number(&["completion_tokens_details", "reasoning_tokens"]) {
        usage.reasoning_tokens = value;
    }
    if let Some(cost) = reported
        .get("cost")
        .and_then(Value::as_f64)
        .filter(|cost| cost.is_finite())
    {
        usage.cost_usd = Some(cost);
    }
}

#[cfg(test)]
mod tests {
    use super::{Decoder, body, headers, merge_usage};
    use crate::event::{LlmEvent, Usage};
    use crate::message::{
        ChatRequest, Content, Message, Role, TextBlock, ToolChoice, ToolDefinition,
    };
    use crate::provider::ProviderConfig;
    use secrecy::SecretString;
    use serde_json::json;

    fn fixture_request() -> ChatRequest {
        ChatRequest {
            tool_choice: ToolChoice::Any,
            ..ChatRequest::new("deterministic", 512)
                .with_system("be terse")
                .with_message(Message::user("write the file"))
                .with_tool(ToolDefinition::new(
                    "write",
                    "write a file",
                    json!({"type": "object"}),
                ))
        }
    }

    #[test]
    fn body_carries_tools_usage_options_and_streaming() {
        let body = body(&fixture_request());
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["stream_options"]["include_usage"], json!(true));
        assert_eq!(body["max_tokens"], json!(512));
        assert_eq!(body["tool_choice"], json!("required"));
        assert_eq!(body["tools"][0]["function"]["name"], json!("write"));
        assert_eq!(body["messages"][0]["role"], json!("system"));
        assert_eq!(body["messages"][1]["content"], json!("write the file"));
    }

    #[test]
    fn tool_results_become_their_own_messages() {
        let request = ChatRequest::new("deterministic", 64)
            .with_message(Message::user("go"))
            .with_message(Message {
                role: Role::Assistant,
                content: vec![
                    Content::Text(TextBlock::new("calling")),
                    Content::ToolUse {
                        call_id: "call_1".to_owned(),
                        tool: "write".to_owned(),
                        arguments: json!({"path": "a.txt"}),
                    },
                ],
            })
            .with_message(Message {
                role: Role::User,
                content: vec![Content::ToolResult {
                    call_id: "call_1".to_owned(),
                    content: "denied".to_owned(),
                    is_error: true,
                }],
            });
        let body = body(&request);
        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["tool_calls"][0]["id"], json!("call_1"));
        assert_eq!(
            messages[1]["tool_calls"][0]["function"]["arguments"],
            json!("{\"path\":\"a.txt\"}")
        );
        assert_eq!(messages[2]["role"], json!("tool"));
        assert_eq!(messages[2]["content"], json!("ERROR: denied"));
    }

    #[test]
    fn credential_header_is_marked_sensitive() {
        let provider = ProviderConfig::openai_compatible("fixture", "http://127.0.0.1:1/v1", None)
            .expect("provider");
        let key = SecretString::from("fixture-provider-token-1234");
        let headers = headers(&provider, Some(&key)).expect("headers");
        let authorization = headers.get(reqwest::header::AUTHORIZATION).expect("header");
        assert!(authorization.is_sensitive());
        assert!(!format!("{headers:?}").contains("fixture-provider-token-1234"));
    }

    #[test]
    fn decodes_a_streamed_tool_call_turn() {
        let mut decoder = Decoder::new("fixture", "deterministic");
        let frames = [
            r#"{"id":"chatcmpl_1","model":"deterministic","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"write","arguments":"{\"path\":"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.txt\"}"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":40,"completion_tokens":9,"cost":0.0004}}"#,
            "[DONE]",
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
                "tool.started",
                "tool.progress",
                "tool.progress",
                "tool.progress",
                "response.step_completed",
                "response.completed",
            ]
        );
        let LlmEvent::ResponseCompleted { usage, message, .. } =
            events.last().expect("terminal event")
        else {
            panic!("expected a terminal completion");
        };
        assert_eq!(usage.output_tokens, 9);
        assert_eq!(usage.cost_usd, Some(0.0004));
        assert_eq!(message.tool_calls[0].arguments, json!({"path": "a.txt"}));
    }

    #[test]
    fn decodes_text_reasoning_and_stream_errors() {
        let mut decoder = Decoder::new("fixture", "deterministic");
        let mut events = decoder
            .push(r#"{"model":"deterministic","choices":[{"delta":{"content":"hi","reasoning":"pondering"}}]}"#)
            .expect("frame decodes");
        events.extend(
            decoder
                .push(r#"{"error":{"message":"upstream exploded"}}"#)
                .expect("frame decodes"),
        );
        let types: Vec<&str> = events.iter().map(LlmEvent::event_type).collect();
        assert_eq!(
            types,
            [
                "response.started",
                "response.delta",
                "reasoning.available",
                "response.failed"
            ]
        );
        assert!(decoder.push("not json").is_err());
    }

    #[test]
    fn usage_merge_splits_cached_prompt_tokens() {
        let mut usage = Usage::default();
        merge_usage(
            &mut usage,
            &json!({
                "prompt_tokens": 100,
                "prompt_tokens_details": {"cached_tokens": 60},
                "completion_tokens": 20,
                "completion_tokens_details": {"reasoning_tokens": 5},
                "cost": 0.01,
            }),
        );
        assert_eq!(usage.input_tokens, 40);
        assert_eq!(usage.cache_read_input_tokens, 60);
        assert_eq!(usage.reasoning_tokens, 5);
        assert_eq!(usage.total_tokens(), 120);
    }
}
