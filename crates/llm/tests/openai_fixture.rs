//! SPEC-V2 RUN-1 "Done when": a streamed tool-call turn, an abort mid-stream,
//! and captured usage, all against `scripts/fixtures/openai-compatible.ts`.

mod common;

use std::time::{Duration, Instant};

use common::{FIXTURE_ALIAS, FixtureKeychain, FixtureProvider, drain};
use personal_agent_llm::{
    ChatRequest, FinishReason, LlmClient, LlmEvent, Message, ProviderConfig, ToolDefinition,
};
use serde_json::{Value, json};

fn tool_request(model: &str, write_path: &std::path::Path) -> ChatRequest {
    ChatRequest::new(model, 512)
        .with_system("You are the native provider layer test harness.")
        .with_message(Message::user(format!(
            "Write a file at {}",
            write_path.display()
        )))
        .with_tool(ToolDefinition::new(
            "write",
            "Write a file to the workspace",
            json!({
                "type": "object",
                "properties": {"filePath": {"type": "string"}, "content": {"type": "string"}},
                "required": ["filePath", "content"],
                "additionalProperties": false,
            }),
        ))
}

fn client(base_url: &str) -> LlmClient {
    let provider =
        ProviderConfig::openai_compatible("fixture", base_url, Some(FIXTURE_ALIAS.to_owned()))
            .expect("provider configuration");
    LlmClient::connect(provider, &FixtureKeychain::default()).expect("client")
}

#[tokio::test]
async fn streams_a_tool_call_turn_from_the_fixture_provider() {
    let temp = tempfile::tempdir().expect("temp directory");
    let metadata_path = temp.path().join("provider-requests.json");
    let write_path = temp.path().join("provider-proof.txt");
    let provider = FixtureProvider::start(&metadata_path, &write_path).await;

    let client = client(&provider.base_url());
    let mut turn = client
        .stream(tool_request("deterministic", &write_path))
        .expect("turn starts");
    let events = drain(&mut turn, Duration::from_secs(20)).await;

    let types: Vec<&str> = events.iter().map(LlmEvent::event_type).collect();
    assert_eq!(
        types,
        [
            "response.started",
            "tool.started",
            "tool.progress",
            "tool.progress",
            "response.step_completed",
            "response.completed",
        ],
        "unexpected event sequence: {events:#?}"
    );

    let Some(LlmEvent::ResponseCompleted {
        finish_reason,
        message,
        ..
    }) = events.last()
    else {
        panic!("turn did not end with response.completed: {events:#?}");
    };
    assert_eq!(*finish_reason, FinishReason::ToolCalls);
    assert_eq!(message.tool_calls.len(), 1);
    let call = &message.tool_calls[0];
    assert_eq!(call.tool, "write");
    assert_eq!(call.call_id, "call_fixture_write");
    assert_eq!(
        call.arguments["filePath"],
        json!(write_path.display().to_string())
    );
    assert_eq!(
        call.arguments["content"],
        json!("Personal Agent coding tools are active.\n")
    );

    // The same call is observable incrementally, which is what the engine's
    // parallel dispatch will hang off.
    let announced = events.iter().find_map(|event| match event {
        LlmEvent::ToolCallStarted { tool, call_id, .. } => Some((tool.clone(), call_id.clone())),
        _ => None,
    });
    assert_eq!(
        announced,
        Some(("write".to_owned(), "call_fixture_write".to_owned()))
    );

    let recorded: Value = serde_json::from_slice(
        &std::fs::read(&metadata_path).expect("fixture recorded the request"),
    )
    .expect("request metadata is JSON");
    let body_keys = recorded[0]["bodyKeys"]
        .as_array()
        .expect("body keys")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(
        body_keys.contains(&"stream") && body_keys.contains(&"stream_options"),
        "usage reporting must be requested: {body_keys:?}"
    );
    assert!(body_keys.contains(&"tools"), "tools must be advertised");
    assert_eq!(recorded[0]["toolNames"][0], json!("write"));

    provider.stop().await;
}

#[tokio::test]
async fn captures_provider_reported_usage_and_cost() {
    let temp = tempfile::tempdir().expect("temp directory");
    let metadata_path = temp.path().join("provider-requests.json");
    let write_path = temp.path().join("provider-proof.txt");
    let provider = FixtureProvider::start(&metadata_path, &write_path).await;

    let client = client(&provider.base_url());
    let mut turn = client
        .stream(tool_request("deterministic-usage", &write_path))
        .expect("turn starts");
    let events = drain(&mut turn, Duration::from_secs(20)).await;

    let Some(LlmEvent::ResponseCompleted {
        usage,
        finish_reason,
        message,
    }) = events.last()
    else {
        panic!("turn did not end with response.completed: {events:#?}");
    };
    assert_eq!(*finish_reason, FinishReason::Stop);
    assert_eq!(message.text, "usage probe complete");
    assert_eq!(usage.input_tokens, 30, "cached tokens are reported apart");
    assert_eq!(usage.cache_read_input_tokens, 11);
    assert_eq!(usage.output_tokens, 7);
    assert_eq!(usage.total_tokens(), 48);
    assert_eq!(usage.cost_usd, Some(0.000_42));

    let step = events
        .iter()
        .find_map(|event| match event {
            LlmEvent::ResponseStepCompleted { usage, .. } => Some(*usage),
            _ => None,
        })
        .expect("a step completion carries usage");
    assert_eq!(step, *usage);

    let envelope = events
        .last()
        .expect("terminal event")
        .to_envelope(1, "llm", "default", "session-usage")
        .expect("envelope");
    let payload = envelope.payload().expect("payload");
    assert_eq!(payload["tokens"]["total_tokens"], json!(48));
    assert_eq!(payload["tokens"]["cost_usd"], json!(0.000_42));

    provider.stop().await;
}

#[tokio::test]
async fn aborts_mid_stream_and_terminates_the_turn() {
    let temp = tempfile::tempdir().expect("temp directory");
    let metadata_path = temp.path().join("provider-requests.json");
    let write_path = temp.path().join("provider-proof.txt");
    let provider = FixtureProvider::start(&metadata_path, &write_path).await;

    let client = client(&provider.base_url());
    let mut turn = client
        .stream(tool_request("deterministic-abort", &write_path))
        .expect("turn starts");
    let abort = turn.abort_handle();

    // Wait for real streamed output, then abort while the provider is still
    // producing: the fixture keeps this stream open for ~20 s.
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), turn.recv())
            .await
            .expect("first delta arrives")
            .expect("stream stays open");
        let is_delta = matches!(event, LlmEvent::ResponseDelta { .. });
        seen.push(event);
        if is_delta {
            break;
        }
    }

    let aborted_at = Instant::now();
    abort.abort();
    let rest = drain(&mut turn, Duration::from_secs(5)).await;
    let elapsed = aborted_at.elapsed();
    assert!(
        elapsed < Duration::from_secs(3),
        "abort must stop the turn promptly, took {elapsed:?}"
    );

    let Some(LlmEvent::ResponseFailed { error, .. }) = rest.last() else {
        panic!("abort must end the stream with response.failed: {rest:#?}");
    };
    assert!(error.contains("aborted"), "unexpected failure: {error}");
    assert!(abort.is_aborted());
    assert!(
        turn.recv().await.is_none(),
        "the stream must close after the terminal event"
    );
    assert!(
        seen.iter()
            .any(|event| matches!(event, LlmEvent::ResponseStarted { .. })),
        "the aborted turn still reported its start: {seen:#?}"
    );

    provider.stop().await;
}
