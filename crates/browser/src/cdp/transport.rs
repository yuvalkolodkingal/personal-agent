//! JSON-over-pipe Chrome `DevTools` Protocol transport.
//!
//! The browser is launched with `--remote-debugging-pipe`, so the protocol
//! travels over inherited file descriptors 3 (browser reads) and 4 (browser
//! writes) instead of a localhost TCP port. Nothing on the machine can reach the
//! automation channel, which removes the whole "guess the debugging port" attack
//! surface that a fixed port such as 4444 creates.

use crate::BrowserError;
use chromiumoxide_types::{Command, MethodType};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc, oneshot};

/// Upper bound on a single protocol round trip.
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Backlog of protocol events kept for late subscribers.
const EVENT_BUFFER: usize = 512;

/// A protocol notification with its payload left as raw JSON so each caller can
/// deserialize only the event shapes it cares about.
#[derive(Clone, Debug)]
pub(crate) struct CdpEvent {
    pub method: String,
    pub session_id: Option<String>,
    pub params: Value,
}

impl CdpEvent {
    /// Deserialize the payload into a generated CDP event type.
    pub(crate) fn parse<T: serde::de::DeserializeOwned>(&self) -> Result<T, BrowserError> {
        serde_json::from_value(self.params.clone()).map_err(|error| {
            BrowserError::Operation(format!("malformed {} event: {error}", self.method))
        })
    }
}

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, BrowserError>>>>>;

/// Multiplexes typed CDP commands and events over one duplex byte channel.
#[derive(Clone)]
pub(crate) struct CdpClient {
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    pending: PendingMap,
    events: broadcast::Sender<CdpEvent>,
    next_id: Arc<AtomicU64>,
}

impl CdpClient {
    /// Start the reader and writer pumps over an already-connected pipe pair.
    pub(crate) fn start<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (outgoing, outbox) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        tokio::spawn(write_pump(writer, outbox));
        tokio::spawn(read_pump(reader, Arc::clone(&pending), events.clone()));
        Self {
            outgoing,
            pending,
            events,
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Subscribe to every protocol event from this point forward.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.events.subscribe()
    }

    /// Send a typed command and await its typed response.
    ///
    /// `session` selects a flattened target session; `None` addresses the
    /// browser itself.
    pub(crate) async fn send<C>(
        &self,
        session: Option<&str>,
        params: &C,
    ) -> Result<C::Response, BrowserError>
    where
        C: Command + MethodType,
    {
        let method = C::method_id();
        let value = self.call(session, method.as_ref(), params).await?;
        C::response_from_value(value).map_err(|error| {
            BrowserError::Operation(format!("malformed {method} response: {error}"))
        })
    }

    async fn call<P: serde::Serialize>(
        &self,
        session: Option<&str>,
        method: &str,
        params: &P,
    ) -> Result<Value, BrowserError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let params = serde_json::to_value(params)
            .map_err(|error| BrowserError::Operation(format!("cannot encode {method}: {error}")))?;
        let mut message = json!({"id": id, "method": method, "params": params});
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        let mut frame = serde_json::to_vec(&message)
            .map_err(|error| BrowserError::Operation(format!("cannot encode {method}: {error}")))?;
        frame.push(0);

        let (reply, wait) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, reply);
        if self.outgoing.send(frame).is_err() {
            self.pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&id);
            return Err(BrowserError::Unavailable(
                "the browser devtools pipe is closed".into(),
            ));
        }
        match tokio::time::timeout(COMMAND_TIMEOUT, wait).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(BrowserError::Unavailable(
                "the browser devtools pipe is closed".into(),
            )),
            Err(_) => {
                self.pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&id);
                Err(BrowserError::Operation(format!("{method} timed out")))
            }
        }
    }
}

async fn write_pump<W>(mut writer: W, mut outbox: mpsc::UnboundedReceiver<Vec<u8>>)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    while let Some(frame) = outbox.recv().await {
        if writer.write_all(&frame).await.is_err() || writer.flush().await.is_err() {
            break;
        }
    }
}

async fn read_pump<R>(reader: R, pending: PendingMap, events: broadcast::Sender<CdpEvent>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut reader = BufReader::new(reader);
    let mut frame = Vec::new();
    loop {
        frame.clear();
        match reader.read_until(0, &mut frame).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if frame.last() == Some(&0) {
            frame.pop();
        }
        let Ok(message) = serde_json::from_slice::<Value>(&frame) else {
            continue;
        };
        dispatch(message, &pending, &events);
    }
    let mut pending = pending.lock().unwrap_or_else(PoisonError::into_inner);
    for (_, reply) in pending.drain() {
        let _ = reply.send(Err(BrowserError::Unavailable(
            "the browser exited before answering".into(),
        )));
    }
}

fn dispatch(mut message: Value, pending: &PendingMap, events: &broadcast::Sender<CdpEvent>) {
    if let Some(id) = message.get("id").and_then(Value::as_u64) {
        let Some(reply) = pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id)
        else {
            return;
        };
        let outcome = if let Some(error) = message.get("error") {
            Err(BrowserError::Operation(protocol_error(error)))
        } else {
            Ok(message
                .get_mut("result")
                .map_or_else(|| json!({}), Value::take))
        };
        let _ = reply.send(outcome);
        return;
    }
    let Some(method) = message
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    let session_id = message
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let params = message
        .get_mut("params")
        .map_or_else(|| json!({}), Value::take);
    let _ = events.send(CdpEvent {
        method,
        session_id,
        params,
    });
}

fn protocol_error(error: &Value) -> String {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("devtools protocol error");
    match error.get("data").and_then(Value::as_str) {
        Some(data) => format!("{message}: {data}"),
        None => message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chromiumoxide_cdp::cdp::browser_protocol::browser::GetVersionParams;
    use tokio::io::duplex;

    #[tokio::test]
    async fn commands_are_nul_framed_and_responses_are_typed() {
        let (ours, theirs) = duplex(4096);
        let (browser_read, browser_write) = tokio::io::split(theirs);
        let (client_read, client_write) = tokio::io::split(ours);
        let client = CdpClient::start(client_read, client_write);

        let fake = tokio::spawn(async move {
            let mut reader = BufReader::new(browser_read);
            let mut frame = Vec::new();
            reader.read_until(0, &mut frame).await.expect("read frame");
            let mut writer = browser_write;
            frame.pop();
            let request: Value = serde_json::from_slice(&frame).expect("valid json frame");
            assert_eq!(request["method"], "Browser.getVersion");
            let mut response = serde_json::to_vec(&json!({
                "id": request["id"],
                "result": {
                    "protocolVersion": "1.3",
                    "product": "Chrome/145.0.0.0",
                    "revision": "@0",
                    "userAgent": "test",
                    "jsVersion": "14.5",
                },
            }))
            .expect("encode response");
            response.push(0);
            writer.write_all(&response).await.expect("write response");
            writer.flush().await.expect("flush");
        });

        let version = client
            .send(None, &GetVersionParams::default())
            .await
            .expect("typed response");
        assert_eq!(version.protocol_version, "1.3");
        fake.await.expect("fake browser");
    }

    #[tokio::test]
    async fn a_closed_pipe_fails_pending_commands_instead_of_hanging() {
        let (ours, theirs) = duplex(64);
        let (client_read, client_write) = tokio::io::split(ours);
        let client = CdpClient::start(client_read, client_write);
        drop(theirs);
        let error = client
            .send(None, &GetVersionParams::default())
            .await
            .expect_err("closed pipe");
        assert!(matches!(error, BrowserError::Unavailable(_)), "{error:?}");
    }

    #[tokio::test]
    async fn events_are_broadcast_with_their_session() {
        let (ours, theirs) = duplex(4096);
        let (client_read, client_write) = tokio::io::split(ours);
        let client = CdpClient::start(client_read, client_write);
        let mut events = client.subscribe();
        let (_, mut browser_write) = tokio::io::split(theirs);
        let mut frame = serde_json::to_vec(
            &json!({"method": "Page.loadEventFired", "sessionId": "S1", "params": {"timestamp": 1.0}}),
        )
        .expect("encode event");
        frame.push(0);
        browser_write.write_all(&frame).await.expect("write event");
        let event = events.recv().await.expect("event");
        assert_eq!(event.method, "Page.loadEventFired");
        assert_eq!(event.session_id.as_deref(), Some("S1"));
    }
}
