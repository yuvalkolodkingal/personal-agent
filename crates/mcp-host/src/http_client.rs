//! `rmcp` HTTP client bindings for the workspace `reqwest` version.
//!
//! `rmcp` ships convenience implementations of [`StreamableHttpClient`] and
//! [`SseClient`] for its own pinned `reqwest` major version. Using them would
//! link a second `reqwest` (and a second TLS stack) into the desktop binary, so
//! this module implements the same two SDK extension traits over the single
//! `reqwest` the workspace already pins. The MCP framing, session handling, and
//! reconnect logic all stay inside `rmcp`; only request construction lives here.

use std::sync::Arc;

use futures_util::StreamExt as _;
use futures_util::stream::BoxStream;
use http::Uri;
use http::header::WWW_AUTHENTICATE;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::common::http_header::{
    EVENT_STREAM_MIME_TYPE, HEADER_LAST_EVENT_ID, HEADER_SESSION_ID, JSON_MIME_TYPE,
};
use rmcp::transport::sse_client::{SseClient, SseTransportError};
use rmcp::transport::streamable_http_client::{
    AuthRequiredError, StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};
use sse_stream::{Error as SseError, Sse, SseStream};

/// A `reqwest` client wired for MCP HTTP transports.
///
/// The wrapper exists so the SDK traits can be implemented locally; header
/// bindings are baked into the inner client as default headers when the session
/// is built.
#[derive(Clone, Debug)]
pub struct HttpClient {
    inner: reqwest::Client,
}

impl HttpClient {
    /// Wraps an already configured `reqwest` client.
    #[must_use]
    pub fn new(inner: reqwest::Client) -> Self {
        Self { inner }
    }
}

type SseResult<T> = Result<T, SseTransportError<reqwest::Error>>;
type HttpResult<T> = Result<T, StreamableHttpError<reqwest::Error>>;
type SseByteStream = BoxStream<'static, Result<Sse, SseError>>;

fn accept_stream_or_json() -> String {
    [EVENT_STREAM_MIME_TYPE, JSON_MIME_TYPE].join(", ")
}

fn content_type(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get(CONTENT_TYPE)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
}

fn session_header(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get(HEADER_SESSION_ID)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn sse_body(response: reqwest::Response) -> SseByteStream {
    SseStream::from_bytes_stream(response.bytes_stream()).boxed()
}

impl StreamableHttpClient for HttpClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
    ) -> HttpResult<StreamableHttpPostResponse> {
        let mut request = self
            .inner
            .post(uri.as_ref())
            .header(ACCEPT, accept_stream_or_json());
        if let Some(auth_header) = auth_header {
            request = request.bearer_auth(auth_header);
        }
        if let Some(session_id) = session_id {
            request = request.header(HEADER_SESSION_ID, session_id.as_ref());
        }
        let response = request
            .json(&message)
            .send()
            .await
            .map_err(StreamableHttpError::Client)?;
        post_response(response).await
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
    ) -> HttpResult<()> {
        let mut request = self
            .inner
            .delete(uri.as_ref())
            .header(HEADER_SESSION_ID, session_id.as_ref());
        if let Some(auth_header) = auth_header {
            request = request.bearer_auth(auth_header);
        }
        let response = request.send().await.map_err(StreamableHttpError::Client)?;
        if response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
            // Stateless servers legitimately refuse explicit session deletion.
            return Ok(());
        }
        response
            .error_for_status()
            .map(drop)
            .map_err(StreamableHttpError::Client)
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
    ) -> HttpResult<SseByteStream> {
        let mut request = self
            .inner
            .get(uri.as_ref())
            .header(ACCEPT, accept_stream_or_json())
            .header(HEADER_SESSION_ID, session_id.as_ref());
        if let Some(last_event_id) = last_event_id {
            request = request.header(HEADER_LAST_EVENT_ID, last_event_id);
        }
        if let Some(auth_header) = auth_header {
            request = request.bearer_auth(auth_header);
        }
        let response = request.send().await.map_err(StreamableHttpError::Client)?;
        if response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        let response = response
            .error_for_status()
            .map_err(StreamableHttpError::Client)?;
        let mime = content_type(&response);
        if !accepts_stream_or_json(mime.as_deref()) {
            return Err(StreamableHttpError::UnexpectedContentType(mime));
        }
        Ok(sse_body(response))
    }
}

fn accepts_stream_or_json(mime: Option<&str>) -> bool {
    mime.is_some_and(|mime| {
        mime.starts_with(EVENT_STREAM_MIME_TYPE) || mime.starts_with(JSON_MIME_TYPE)
    })
}

fn authorization_challenge(response: &reqwest::Response) -> HttpResult<()> {
    if response.status() != reqwest::StatusCode::UNAUTHORIZED {
        return Ok(());
    }
    let Some(header) = response.headers().get(WWW_AUTHENTICATE) else {
        return Ok(());
    };
    let header = header.to_str().map_err(|_| {
        StreamableHttpError::UnexpectedServerResponse("invalid www-authenticate header".into())
    })?;
    Err(StreamableHttpError::AuthRequired(AuthRequiredError {
        www_authenticate_header: header.to_owned(),
    }))
}

async fn post_response(response: reqwest::Response) -> HttpResult<StreamableHttpPostResponse> {
    authorization_challenge(&response)?;
    let status = response.status();
    let response = response
        .error_for_status()
        .map_err(StreamableHttpError::Client)?;
    if matches!(
        status,
        reqwest::StatusCode::ACCEPTED | reqwest::StatusCode::NO_CONTENT
    ) {
        return Ok(StreamableHttpPostResponse::Accepted);
    }
    let mime = content_type(&response);
    let session_id = session_header(&response);
    match mime.as_deref() {
        Some(mime) if mime.starts_with(EVENT_STREAM_MIME_TYPE) => Ok(
            StreamableHttpPostResponse::Sse(sse_body(response), session_id),
        ),
        Some(mime) if mime.starts_with(JSON_MIME_TYPE) => {
            let message: ServerJsonRpcMessage =
                response.json().await.map_err(StreamableHttpError::Client)?;
            Ok(StreamableHttpPostResponse::Json(message, session_id))
        }
        _ => Err(StreamableHttpError::UnexpectedContentType(mime)),
    }
}

impl SseClient for HttpClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Uri,
        message: ClientJsonRpcMessage,
        auth_token: Option<String>,
    ) -> SseResult<()> {
        let mut request = self.inner.post(uri.to_string()).json(&message);
        if let Some(auth_token) = auth_token {
            request = request.bearer_auth(auth_token);
        }
        request
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map(drop)
            .map_err(SseTransportError::Client)
    }

    async fn get_stream(
        &self,
        uri: Uri,
        last_event_id: Option<String>,
        auth_token: Option<String>,
    ) -> SseResult<SseByteStream> {
        let mut request = self
            .inner
            .get(uri.to_string())
            .header(ACCEPT, EVENT_STREAM_MIME_TYPE);
        if let Some(last_event_id) = last_event_id {
            request = request.header(HEADER_LAST_EVENT_ID, last_event_id);
        }
        if let Some(auth_token) = auth_token {
            request = request.bearer_auth(auth_token);
        }
        let response = request
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(SseTransportError::Client)?;
        let mime = content_type(&response);
        if !mime
            .as_deref()
            .is_some_and(|mime| mime.starts_with(EVENT_STREAM_MIME_TYPE))
        {
            return Err(SseTransportError::UnexpectedContentType(mime));
        }
        Ok(sse_body(response))
    }
}

#[cfg(test)]
mod tests {
    use super::{accept_stream_or_json, accepts_stream_or_json};

    #[test]
    fn accept_header_offers_both_mcp_media_types() {
        assert_eq!(
            accept_stream_or_json(),
            "text/event-stream, application/json"
        );
    }

    #[test]
    fn unexpected_media_types_are_rejected() {
        assert!(accepts_stream_or_json(Some("text/event-stream")));
        assert!(accepts_stream_or_json(Some(
            "application/json; charset=utf-8"
        )));
        assert!(!accepts_stream_or_json(Some("text/html")));
        assert!(!accepts_stream_or_json(None));
    }

    /// Remote MCP servers are reached over `https`, which only works when a TLS
    /// backend is compiled in. Without one `reqwest` rejects the scheme outright
    /// rather than attempting a connection, so a refused local port proves the
    /// backend is present without needing the network.
    #[tokio::test]
    async fn https_requests_reach_the_transport_instead_of_failing_on_a_missing_tls_backend() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve a local port");
        let port = listener.local_addr().expect("local address").port();
        drop(listener);

        let error = reqwest::Client::new()
            .get(format!("https://127.0.0.1:{port}/"))
            .send()
            .await
            .expect_err("a closed port cannot answer");

        assert!(
            error.is_connect(),
            "expected a connect failure, got {error:?}"
        );
        assert!(
            !error.is_builder(),
            "https was rejected before connecting, which means no TLS backend is linked: {error:?}"
        );
    }
}
