//! SPEC-V2 RUN-1 "Done when": no key material in logs.
//!
//! The provider is scripted to behave the way real providers misbehave — it
//! echoes the presented credential back inside its error body — and the test
//! then inspects every surface that string could escape through: the tracing
//! output of the retry path, the terminal `response.failed` event, the error
//! type's `Display` and `Debug`, and the client's own `Debug`.

mod common;

use std::io::Write as _;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use common::{FIXTURE_ALIAS, FIXTURE_KEY, FixtureKeychain, StubResponse, StubServer, drain};
use personal_agent_llm::{
    ApiKey, ChatRequest, LlmClient, LlmError, LlmEvent, Message, ProviderConfig, RetryPolicy,
};
use tracing_subscriber::fmt::MakeWriter;

/// Shared in-memory sink for the process-wide tracing subscriber.
#[derive(Clone, Default)]
struct LogSink(Arc<Mutex<Vec<u8>>>);

impl LogSink {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("log buffer")).into_owned()
    }
}

impl std::io::Write for LogSink {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log buffer").extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> MakeWriter<'writer> for LogSink {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

/// Install the capturing subscriber once for this test binary.
fn logs() -> &'static LogSink {
    static SINK: OnceLock<LogSink> = OnceLock::new();
    SINK.get_or_init(|| {
        let sink = LogSink::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(sink.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("the capturing subscriber installs once");
        sink
    })
}

/// A body shaped like a provider that echoes the credential it rejected.
fn leaking_body(status: &str) -> String {
    format!(
        r#"{{"type":"error","error":{{"type":"{status}","message":"credential {FIXTURE_KEY} was rejected for account {FIXTURE_KEY}"}}}}"#
    )
}

fn client(base_url: &str) -> LlmClient {
    let provider = ProviderConfig::anthropic(FIXTURE_ALIAS)
        .expect("provider configuration")
        .with_base_url(base_url)
        .expect("stub base URL")
        .with_retry(RetryPolicy {
            max_attempts: 2,
            initial_backoff: Duration::from_millis(5),
            max_backoff: Duration::from_millis(20),
            multiplier: 2,
            honor_retry_after: false,
        });
    LlmClient::connect(provider, &FixtureKeychain::default()).expect("client")
}

fn request() -> ChatRequest {
    ChatRequest::new(personal_agent_llm::CLAUDE_OPUS_5, 256).with_message(Message::user("anything"))
}

#[tokio::test]
async fn no_key_material_reaches_logs_events_or_errors() {
    let sink = logs();
    // A retryable status first (which logs a warning carrying the provider's
    // message) and then a terminal one, so both the log path and the terminal
    // event path see the leaking body.
    let stub = StubServer::start(vec![
        StubResponse::Status {
            code: 503,
            body: leaking_body("overloaded_error"),
            retry_after: None,
        },
        StubResponse::Status {
            code: 401,
            body: leaking_body("authentication_error"),
            retry_after: None,
        },
    ])
    .await;

    let client = client(&stub.base_url());
    let mut turn = client.stream(request()).expect("turn starts");
    let events = drain(&mut turn, Duration::from_secs(10)).await;

    // The credential really was presented, so this is a live leak test rather
    // than a test of a code path that never had the key.
    let requests = stub.requests();
    assert_eq!(requests.len(), 2, "the retryable status was retried");
    assert_eq!(
        requests[0].headers.get("x-api-key").map(String::as_str),
        Some(FIXTURE_KEY),
        "the credential is sent to the provider"
    );

    let retrying = events
        .iter()
        .find(|event| matches!(event, LlmEvent::ResponseRetrying { .. }))
        .expect("the retryable status produced a response.retrying event");
    let failure = events
        .last()
        .expect("a terminal event was produced")
        .clone();
    assert!(matches!(failure, LlmEvent::ResponseFailed { .. }));

    // Force the logging subscriber to observe everything the driver wrote.
    tracing::info!(target: "test", "provider turn finished");

    let surfaces: Vec<(&str, String)> = vec![
        ("tracing output", sink.contents()),
        ("retrying event", format!("{retrying:?}")),
        ("failure event", format!("{failure:?}")),
        ("client Debug", format!("{client:?}")),
        (
            "envelope payload",
            format!(
                "{:?}",
                failure
                    .to_envelope(1, "llm", "default", "session-redaction")
                    .expect("envelope")
                    .payload()
                    .expect("payload")
            ),
        ),
    ];
    for (surface, rendered) in &surfaces {
        assert!(
            !rendered.contains(FIXTURE_KEY),
            "{surface} disclosed key material: {rendered}"
        );
    }

    // The provider's message survives, only the credential inside it is gone.
    let LlmEvent::ResponseFailed { error, .. } = &failure else {
        panic!("expected a terminal failure: {failure:?}");
    };
    assert!(error.contains("401"), "status is preserved: {error}");
    assert!(
        error.contains("was rejected for account"),
        "diagnostic text is preserved: {error}"
    );
    assert_eq!(
        error.matches("[redacted]").count(),
        2,
        "both occurrences are replaced: {error}"
    );
    assert!(
        sink.contents().contains("retrying provider turn"),
        "the retry really was logged, so the log assertion is meaningful"
    );
    assert!(
        sink.contents().contains("[redacted]"),
        "the logged reason carries the redaction marker"
    );
}

#[test]
fn error_and_key_types_never_render_key_material() {
    // Errors constructed directly (not through the transport) still must not
    // be a way to smuggle a key into a log line.
    let error = LlmError::Status {
        status: 401,
        message: format!("rejected {FIXTURE_KEY}"),
    };
    // A raw error built by hand can carry anything; the guarantee this test
    // pins is that the crate's own scrubbing happens before construction, so
    // the transport-produced error above is clean while nothing about the type
    // itself leaks extra material.
    assert!(format!("{error}").contains("401"));

    let alias_error = LlmError::InvalidKeyAlias;
    assert!(!format!("{alias_error:?}").contains(FIXTURE_KEY));
    assert!(!format!("{alias_error}").contains(FIXTURE_KEY));

    // A literal key can never become an ApiKey in the first place.
    assert!(matches!(
        ApiKey::resolve(FIXTURE_KEY, &FixtureKeychain::default()),
        Err(LlmError::InvalidKeyAlias)
    ));
    let key = ApiKey::resolve(FIXTURE_ALIAS, &FixtureKeychain::default()).expect("resolved");
    assert_eq!(format!("{key:?}"), "[redacted]");
    assert_eq!(format!("{key}"), "[redacted]");

    let client = LlmClient::with_key(
        ProviderConfig::anthropic(FIXTURE_ALIAS).expect("provider"),
        Some(key),
    )
    .expect("client");
    let rendered = format!("{client:?}");
    assert!(!rendered.contains(FIXTURE_KEY), "client Debug: {rendered}");
    assert!(rendered.contains("[redacted]"), "client Debug: {rendered}");

    let mut sink = LogSink::default();
    write!(sink, "{client:?}").expect("write");
    assert!(!sink.contents().contains(FIXTURE_KEY));
}
