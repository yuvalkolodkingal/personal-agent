//! End-to-end Anthropic Messages API behaviour against a scripted HTTP stub:
//! request shape (tools, prompt-cache markers, `context-1m` passthrough),
//! streamed decoding, usage extraction, and retry with backoff.

mod common;

use std::time::Duration;

use common::{FIXTURE_ALIAS, FIXTURE_KEY, FixtureKeychain, StubResponse, StubServer, drain};
use personal_agent_llm::{
    ANTHROPIC_CONTEXT_1M_BETA, CLAUDE_OPUS_5, CacheTtl, ChatRequest, FinishReason, LlmClient,
    LlmEvent, Message, ProviderConfig, RetryPolicy, TextBlock, ToolDefinition,
};
use serde_json::{Value, json};

fn turn_frames() -> Vec<String> {
    [
        r#"{"type":"message_start","message":{"id":"msg_stub","model":"claude-opus-5","usage":{"input_tokens":120,"cache_read_input_tokens":900,"cache_creation_input_tokens":40,"output_tokens":1}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"writing the file"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_stub","name":"write"}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"filePath\":"}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"notes.md\"}"}}"#,
        r#"{"type":"content_block_stop","index":1}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":57}}"#,
        r#"{"type":"message_stop"}"#,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn request() -> ChatRequest {
    let mut request = ChatRequest::new(CLAUDE_OPUS_5, 4096)
        .with_message(Message::user("write my notes"))
        .with_tool(ToolDefinition::new(
            "write",
            "Write a file",
            json!({"type": "object", "properties": {"filePath": {"type": "string"}}}),
        ));
    request.system = vec![TextBlock::cached("large stable operator preamble")];
    request.tools[0].cache = true;
    request
}

fn client(base_url: &str, retry: RetryPolicy) -> LlmClient {
    let provider = ProviderConfig::anthropic(FIXTURE_ALIAS)
        .expect("provider configuration")
        .with_base_url(base_url)
        .expect("stub base URL")
        .with_context_1m()
        .with_prompt_caching(Some(CacheTtl::FiveMinutes))
        .with_retry(retry);
    LlmClient::connect(provider, &FixtureKeychain::default()).expect("client")
}

#[tokio::test]
async fn streams_a_messages_api_tool_turn_with_caching_and_beta_passthrough() {
    let stub = StubServer::start(vec![StubResponse::Sse(turn_frames())]).await;
    let client = client(&stub.base_url(), RetryPolicy::none());
    let mut turn = client.stream(request()).expect("turn starts");
    let events = drain(&mut turn, Duration::from_secs(10)).await;

    let types: Vec<&str> = events.iter().map(LlmEvent::event_type).collect();
    assert_eq!(
        types,
        [
            "response.started",
            "response.delta",
            "tool.started",
            "tool.progress",
            "tool.progress",
            "tool.progress",
            "response.step_completed",
            "response.completed",
        ],
        "unexpected event sequence: {events:#?}"
    );

    let Some(LlmEvent::ResponseCompleted {
        finish_reason,
        usage,
        message,
    }) = events.last()
    else {
        panic!("turn did not complete: {events:#?}");
    };
    assert_eq!(*finish_reason, FinishReason::ToolCalls);
    assert_eq!(message.text, "writing the file");
    assert_eq!(message.tool_calls[0].call_id, "toolu_stub");
    assert_eq!(
        message.tool_calls[0].arguments,
        json!({"filePath": "notes.md"})
    );
    assert_eq!(usage.input_tokens, 120);
    assert_eq!(usage.cache_read_input_tokens, 900);
    assert_eq!(usage.cache_creation_input_tokens, 40);
    assert_eq!(usage.output_tokens, 57);
    assert_eq!(usage.total_tokens(), 1117);
    assert_eq!(usage.cost_usd, None, "Anthropic reports no cost");

    let requests = stub.requests();
    assert_eq!(requests.len(), 1);
    let recorded = &requests[0];
    assert_eq!(recorded.method, "POST");
    assert_eq!(recorded.path, "/v1/messages");
    assert_eq!(
        recorded.headers.get("x-api-key").map(String::as_str),
        Some(FIXTURE_KEY)
    );
    assert_eq!(
        recorded
            .headers
            .get("anthropic-version")
            .map(String::as_str),
        Some("2023-06-01")
    );
    assert_eq!(
        recorded.headers.get("anthropic-beta").map(String::as_str),
        Some(ANTHROPIC_CONTEXT_1M_BETA)
    );
    assert_eq!(
        recorded.headers.get("accept").map(String::as_str),
        Some("text/event-stream")
    );

    let body: Value = serde_json::from_str(&recorded.body).expect("request body is JSON");
    assert_eq!(body["model"], json!(CLAUDE_OPUS_5));
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["max_tokens"], json!(4096));
    assert_eq!(
        body["system"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(
        body["tools"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(body["tools"][0]["name"], json!("write"));
    assert_eq!(body["tool_choice"], json!({"type": "auto"}));
}

#[tokio::test]
async fn retries_a_retryable_status_then_succeeds() {
    let stub = StubServer::start(vec![
        StubResponse::Status {
            code: 529,
            body: r#"{"type":"error","error":{"type":"overloaded_error","message":"overloaded"}}"#
                .to_owned(),
            retry_after: Some(1),
        },
        StubResponse::Sse(turn_frames()),
    ])
    .await;
    let retry = RetryPolicy {
        max_attempts: 3,
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(50),
        multiplier: 2,
        honor_retry_after: false,
    };
    let client = client(&stub.base_url(), retry);
    let mut turn = client.stream(request()).expect("turn starts");
    let events = drain(&mut turn, Duration::from_secs(10)).await;

    let retrying = events
        .iter()
        .find_map(|event| match event {
            LlmEvent::ResponseRetrying {
                attempt,
                delay_ms,
                reason,
            } => Some((*attempt, *delay_ms, reason.clone())),
            _ => None,
        })
        .expect("the retryable status produced a response.retrying event");
    assert_eq!(retrying.0, 1);
    assert_eq!(retrying.1, 10);
    assert!(retrying.2.contains("529"), "reason: {}", retrying.2);
    assert!(
        events.last().is_some_and(LlmEvent::is_terminal),
        "the retried turn still terminated"
    );
    assert_eq!(
        events.last().expect("terminal").event_type(),
        "response.completed"
    );
    assert_eq!(stub.requests().len(), 2, "exactly one retry was issued");
}

#[tokio::test]
async fn a_truncated_stream_fails_instead_of_reporting_a_complete_turn() {
    // The provider closes the body after partial text and never states a stop
    // reason: the turn must surface as a failure, not as a short answer.
    let truncated = turn_frames().into_iter().take(3).collect();
    let stub = StubServer::start(vec![StubResponse::Sse(truncated)]).await;
    let client = client(&stub.base_url(), RetryPolicy::none());
    let mut turn = client.stream(request()).expect("turn starts");
    let events = drain(&mut turn, Duration::from_secs(10)).await;

    let types: Vec<&str> = events.iter().map(LlmEvent::event_type).collect();
    assert_eq!(
        types,
        ["response.started", "response.delta", "response.failed"],
        "unexpected event sequence: {events:#?}"
    );
    let Some(LlmEvent::ResponseFailed { error, .. }) = events.last() else {
        panic!("expected a terminal failure: {events:#?}");
    };
    assert!(
        error.contains("closed the stream before finishing"),
        "error: {error}"
    );
}

#[tokio::test]
async fn a_non_retryable_status_fails_the_turn_once() {
    let stub = StubServer::start(vec![StubResponse::Status {
        code: 400,
        body: r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens is too large"}}"#
            .to_owned(),
        retry_after: None,
    }])
    .await;
    let client = client(&stub.base_url(), RetryPolicy::default());
    let mut turn = client.stream(request()).expect("turn starts");
    let events = drain(&mut turn, Duration::from_secs(10)).await;

    assert_eq!(events.len(), 1, "no retry, no partial output: {events:#?}");
    let Some(LlmEvent::ResponseFailed { error, .. }) = events.last() else {
        panic!("expected a terminal failure: {events:#?}");
    };
    assert!(error.contains("400"), "error: {error}");
    assert!(error.contains("max_tokens is too large"), "error: {error}");
    assert_eq!(stub.requests().len(), 1);
}
