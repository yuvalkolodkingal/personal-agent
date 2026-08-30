//! Shared harness for the provider integration tests.
//!
//! Two servers are used. The repository's `scripts/fixtures/openai-compatible.ts`
//! is the OpenAI-compatible provider the SPEC-V2 "Done when" names; a tiny
//! scripted HTTP stub covers the Anthropic dialect, retry, and redaction paths
//! that need control over status codes and response bodies.

#![allow(dead_code)] // Each integration test binary uses a subset of the harness.

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use personal_agent_llm::{LlmEvent, TurnStream};
use personal_agent_platform::{SecretReference, SecretStore, SecretStoreError};
use secrecy::SecretString;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};

/// Credential the tests place in their fake keychain.
pub const FIXTURE_KEY: &str = "fixture-provider-token-6f2a91c4";

/// Alias the tests resolve that credential through.
pub const FIXTURE_ALIAS: &str = "keychain://dev.personal-agent.llm/fixture";

/// A keychain stand-in that holds exactly one credential.
pub struct FixtureKeychain(pub String);

impl Default for FixtureKeychain {
    fn default() -> Self {
        Self(FIXTURE_KEY.to_owned())
    }
}

impl SecretStore for FixtureKeychain {
    fn put(
        &self,
        _reference: &SecretReference,
        _value: &SecretString,
    ) -> Result<(), SecretStoreError> {
        Err(SecretStoreError::Unavailable("fixture is read-only".into()))
    }

    fn get(&self, _reference: &SecretReference) -> Result<SecretString, SecretStoreError> {
        Ok(SecretString::from(self.0.clone()))
    }

    fn delete(&self, _reference: &SecretReference) -> Result<(), SecretStoreError> {
        Err(SecretStoreError::Unavailable("fixture is read-only".into()))
    }
}

/// Reserve a loopback port the way the runtime tests do.
pub fn reserve_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("bind loopback")
        .local_addr()
        .expect("local address")
        .port()
}

/// Collect events until the stream ends, failing the test on timeout.
pub async fn drain(stream: &mut TurnStream, timeout: Duration) -> Vec<LlmEvent> {
    let mut events = Vec::new();
    loop {
        match tokio::time::timeout(timeout, stream.recv()).await {
            Ok(Some(event)) => {
                let terminal = event.is_terminal();
                events.push(event);
                if terminal {
                    return events;
                }
            }
            Ok(None) => return events,
            Err(elapsed) => {
                panic!("turn did not terminate ({elapsed}) within {timeout:?}: {events:#?}")
            }
        }
    }
}

/// The bun fixture provider from `scripts/fixtures/openai-compatible.ts`.
pub struct FixtureProvider {
    child: Child,
    pub port: u16,
}

impl FixtureProvider {
    /// Spawn the fixture and wait for it to accept connections.
    pub async fn start(metadata_path: &std::path::Path, write_path: &std::path::Path) -> Self {
        let port = reserve_port();
        let script = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/fixtures/openai-compatible.ts");
        let mut child = Command::new("bun")
            .arg(script)
            .arg(format!("--port={port}"))
            .arg(format!("--metadata-path={}", metadata_path.display()))
            .arg(format!("--write-path={}", write_path.display()))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("start the synthetic provider (is `bun` installed?)");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while std::net::TcpStream::connect_timeout(
            &SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_millis(100),
        )
        .is_err()
        {
            assert!(
                child.try_wait().expect("provider status").is_none(),
                "synthetic provider exited during startup"
            );
            assert!(
                tokio::time::Instant::now() < deadline,
                "synthetic provider startup timeout"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Self { child, port }
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    pub async fn stop(mut self) {
        let _ = self.child.kill().await;
    }
}

/// One request the stub server observed.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

/// A scripted response.
#[derive(Clone, Debug)]
pub enum StubResponse {
    /// A non-streaming status response with a body.
    Status {
        code: u16,
        body: String,
        retry_after: Option<u64>,
    },
    /// A `text/event-stream` response made of pre-rendered `data:` payloads.
    Sse(Vec<String>),
}

/// A minimal scripted HTTP/1.1 server.
///
/// Responses are consumed in order; the last one repeats. The server closes
/// each connection after answering, which also terminates the SSE body.
pub struct StubServer {
    pub port: u16,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

impl StubServer {
    pub async fn start(responses: Vec<StubResponse>) -> Self {
        let port = reserve_port();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .expect("bind stub server");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            let mut served = 0usize;
            while let Ok((socket, _)) = listener.accept().await {
                let response = responses
                    .get(served)
                    .or_else(|| responses.last())
                    .cloned()
                    .unwrap_or(StubResponse::Sse(Vec::new()));
                served += 1;
                if let Some(request) = serve(socket, &response).await {
                    recorded.lock().expect("recorded requests").push(request);
                }
            }
        });
        Self {
            port,
            requests,
            task,
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("recorded requests").clone()
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(mut socket: TcpStream, response: &StubResponse) -> Option<RecordedRequest> {
    let request = read_request(&mut socket).await?;
    let rendered = match response {
        StubResponse::Status {
            code,
            body,
            retry_after,
        } => {
            let retry = retry_after
                .map(|seconds| format!("retry-after: {seconds}\r\n"))
                .unwrap_or_default();
            format!(
                "HTTP/1.1 {code} STUB\r\ncontent-type: application/json\r\ncontent-length: {}\r\n{retry}connection: close\r\n\r\n{body}",
                body.len()
            )
        }
        StubResponse::Sse(frames) => {
            let mut body = String::new();
            for frame in frames {
                body.push_str("data: ");
                body.push_str(frame);
                body.push_str("\n\n");
            }
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n{body}"
            )
        }
    };
    let _ = socket.write_all(rendered.as_bytes()).await;
    let _ = socket.flush().await;
    let _ = socket.shutdown().await;
    Some(request)
}

async fn read_request(socket: &mut TcpStream) -> Option<RecordedRequest> {
    let mut raw = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = socket.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        raw.extend_from_slice(&buffer[..read]);
        if let Some(position) = find_header_end(&raw) {
            break position;
        }
    };
    let head = String::from_utf8_lossy(&raw[..header_end]).into_owned();
    let mut lines = head.lines();
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_owned();
    let path = request_line.next()?.to_owned();
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = raw[header_end + 4..].to_vec();
    while body.len() < length {
        let read = socket.read(&mut buffer).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buffer[..read]);
    }
    Some(RecordedRequest {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn find_header_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|window| window == b"\r\n\r\n")
}
