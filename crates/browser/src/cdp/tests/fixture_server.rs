//! A loopback HTTP/1.1 server for the CDP engine tests.
//!
//! It is deliberately tiny and dependency free. Two host names resolve to it,
//! `127.0.0.1` and `localhost`, which gives the policy tests a genuine
//! first-party and third-party origin pair on one listener.

use std::collections::BTreeMap;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use url::Url;

/// Cookie the isolation test sets in one profile and looks for in another.
pub(crate) const ISOLATION_COOKIE: &str = "pa_isolation_probe";

/// Body served at `/download.txt`.
pub(crate) const DOWNLOAD_BODY: &[u8] = b"quarantined fixture payload\n";

pub(crate) struct FixtureServer {
    pub(crate) port: u16,
    handle: JoinHandle<()>,
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl FixtureServer {
    pub(crate) async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture server");
        let port = listener.local_addr().expect("fixture address").port();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let _ = handle_connection(stream, port).await;
                });
            }
        });
        Self { port, handle }
    }

    /// Build a fixture URL for a specific host name.
    pub(crate) fn url(&self, host: &str, path: &str) -> Url {
        Url::parse(&format!("http://{host}:{}{path}", self.port)).expect("fixture url")
    }

    /// The first-party origin the policy allows.
    pub(crate) fn first_party(&self, path: &str) -> Url {
        self.url("127.0.0.1", path)
    }
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/fixtures/browser")
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

async fn handle_connection(mut stream: TcpStream, port: u16) -> std::io::Result<()> {
    let mut chunk = vec![0_u8; 16384];
    let mut raw = Vec::new();
    let header_end = loop {
        if let Some(position) = find(&raw, b"\r\n\r\n") {
            break position + 4;
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        raw.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&raw[..header_end]).into_owned();
    let expected = content_length(&head);
    while raw.len() < header_end + expected {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
    }
    let body = raw[header_end..].to_vec();
    let response = route(&head, &body, port);
    stream.write_all(&response).await?;
    stream.flush().await
}

fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0)
}

fn request_target(head: &str) -> (String, String) {
    let mut parts = head.lines().next().unwrap_or_default().split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or("/").to_owned();
    (method, target)
}

fn route(head: &str, body: &[u8], port: u16) -> Vec<u8> {
    let (method, target) = request_target(head);
    let path = target.split('?').next().unwrap_or("/").to_owned();
    match (method.as_str(), path.as_str()) {
        ("GET", "/form.html") => {
            let page = std::fs::read_to_string(fixtures_dir().join("form.html"))
                .expect("form.html fixture")
                .replace(
                    "{{THIRD_PARTY_SCRIPT}}",
                    &format!("http://localhost:{port}/tracker.js"),
                );
            respond("200 OK", "text/html; charset=utf-8", &[], page.as_bytes())
        }
        ("GET", "/second.html") => {
            let page =
                std::fs::read_to_string(fixtures_dir().join("second.html")).expect("second.html");
            respond("200 OK", "text/html; charset=utf-8", &[], page.as_bytes())
        }
        ("GET", "/tracker.js") => respond(
            "200 OK",
            "application/javascript",
            &[],
            b"/* third-party tracker that policy must refuse */",
        ),
        ("GET", "/set-cookie") => respond(
            "200 OK",
            "text/html; charset=utf-8",
            &[(
                "Set-Cookie",
                &format!("{ISOLATION_COOKIE}=task-a; Path=/; Max-Age=3600"),
            )],
            b"<!doctype html><title>cookie set</title><h1>Cookie set</h1>",
        ),
        ("GET", "/download.txt") => respond(
            "200 OK",
            "text/plain; charset=utf-8",
            &[(
                "Content-Disposition",
                "attachment; filename=\"fixture-download.txt\"",
            )],
            DOWNLOAD_BODY,
        ),
        ("POST", "/submit") => {
            let page = echo_page(&parse_multipart(head, body));
            respond("200 OK", "text/html; charset=utf-8", &[], page.as_bytes())
        }
        _ => respond(
            "404 Not Found",
            "text/plain; charset=utf-8",
            &[],
            b"not found",
        ),
    }
}

fn respond(status: &str, content_type: &str, extra: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra {
        use std::fmt::Write as _;
        let _ = write!(head, "{name}: {value}\r\n");
    }
    head.push_str("\r\n");
    let mut response = head.into_bytes();
    response.extend_from_slice(body);
    response
}

/// One `multipart/form-data` part: field name, optional filename, and content.
struct Part {
    filename: Option<String>,
    content: Vec<u8>,
}

fn boundary(head: &str) -> Option<String> {
    head.lines()
        .find(|line| line.to_ascii_lowercase().starts_with("content-type:"))?
        .split("boundary=")
        .nth(1)
        .map(|value| value.trim().trim_matches('"').to_owned())
}

fn parse_multipart(head: &str, body: &[u8]) -> BTreeMap<String, Part> {
    let mut fields = BTreeMap::new();
    let Some(boundary) = boundary(head) else {
        return fields;
    };
    let separator = format!("--{boundary}").into_bytes();
    let mut rest = body;
    while let Some(start) = find(rest, &separator) {
        rest = &rest[start + separator.len()..];
        let Some(header_end) = find(rest, b"\r\n\r\n") else {
            break;
        };
        let headers = String::from_utf8_lossy(&rest[..header_end]).into_owned();
        let content = &rest[header_end + 4..];
        let end = find(content, &separator).unwrap_or(content.len());
        let content = content[..end].to_vec();
        let content = content
            .strip_suffix(b"\r\n--")
            .or_else(|| content.strip_suffix(b"\r\n"))
            .unwrap_or(&content)
            .to_vec();
        if let Some(name) = disposition_value(&headers, "name") {
            fields.insert(
                name,
                Part {
                    filename: disposition_value(&headers, "filename"),
                    content,
                },
            );
        }
        rest = &rest[header_end + 4..];
    }
    fields
}

fn disposition_value(headers: &str, key: &str) -> Option<String> {
    headers
        .split(&format!("{key}=\""))
        .nth(1)?
        .split('"')
        .next()
        .map(str::to_owned)
}

fn echo_page(fields: &BTreeMap<String, Part>) -> String {
    let text = |name: &str| {
        fields
            .get(name)
            .map(|part| String::from_utf8_lossy(&part.content).into_owned())
            .unwrap_or_default()
    };
    let attachment = fields.get("attachment");
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Submitted</title>\
         </head><body><h1>Submitted</h1><p>full_name={}</p><p>plan={}</p>\
         <p>attachment={}</p><p>attachment_bytes={}</p></body></html>",
        text("full_name"),
        text("plan"),
        attachment
            .and_then(|part| part.filename.clone())
            .unwrap_or_default(),
        attachment.map_or(0, |part| part.content.len()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_parsing_recovers_field_values_and_file_names() {
        let head =
            "POST /submit HTTP/1.1\r\nContent-Type: multipart/form-data; boundary=XyZ\r\n\r\n";
        let mut body = Vec::new();
        body.extend_from_slice(
            b"--XyZ\r\nContent-Disposition: form-data; name=\"full_name\"\r\n\r\nAda Lovelace\r\n",
        );
        body.extend_from_slice(
            b"--XyZ\r\nContent-Disposition: form-data; name=\"plan\"\r\n\r\npro\r\n",
        );
        body.extend_from_slice(
            b"--XyZ\r\nContent-Disposition: form-data; name=\"attachment\"; filename=\"note.txt\"\r\n\
              Content-Type: text/plain\r\n\r\nhello upload\r\n--XyZ--\r\n",
        );
        let fields = parse_multipart(head, &body);
        assert_eq!(
            String::from_utf8_lossy(&fields["full_name"].content),
            "Ada Lovelace"
        );
        assert_eq!(String::from_utf8_lossy(&fields["plan"].content), "pro");
        assert_eq!(fields["attachment"].filename.as_deref(), Some("note.txt"));
        assert_eq!(
            String::from_utf8_lossy(&fields["attachment"].content),
            "hello upload"
        );
        let page = echo_page(&fields);
        assert!(page.contains("full_name=Ada Lovelace"), "{page}");
        assert!(page.contains("attachment_bytes=12"), "{page}");
    }

    #[test]
    fn content_length_is_read_case_insensitively() {
        assert_eq!(
            content_length("POST / HTTP/1.1\r\ncontent-length: 42\r\n"),
            42
        );
        assert_eq!(content_length("GET / HTTP/1.1\r\n"), 0);
    }
}
