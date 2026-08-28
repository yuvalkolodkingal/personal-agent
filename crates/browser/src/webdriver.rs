//! Cross-platform W3C `WebDriver` implementation of the browser boundary.

use crate::{
    BrowserEngine, BrowserError, BrowserPolicy, BrowserProfile, NodeHandle, PageSnapshot,
    validate_handle,
};
use async_trait::async_trait;
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::{Instant, sleep};
use url::Url;
use uuid::Uuid;

const ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";
const INTERACTIVE_SELECTOR: &str =
    "a,button,input,textarea,select,[role='button'],[role='link'],[contenteditable='true']";
type DriverCandidate = (&'static str, fn(u16) -> Vec<String>);

/// Connection and policy settings for a locally managed `WebDriver`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WebDriverConfig {
    /// Base URL of geckodriver, chromedriver, Edge `WebDriver`, or `SafariDriver`.
    pub endpoint: Url,
    /// W3C browser name (`firefox`, `chrome`, `MicrosoftEdge`, or `safari`).
    pub browser_name: String,
    /// Additional capabilities merged into `alwaysMatch`.
    #[serde(default)]
    pub capabilities: BTreeMap<String, Value>,
    /// Top-level network policy applied before navigation.
    pub policy: BrowserPolicy,
}

impl Default for WebDriverConfig {
    fn default() -> Self {
        Self {
            endpoint: Url::parse("http://127.0.0.1:4444").expect("constant WebDriver URL"),
            browser_name: "firefox".into(),
            capabilities: BTreeMap::new(),
            policy: BrowserPolicy {
                allowed_domains: BTreeSet::new(),
                blocked_domains: BTreeSet::new(),
                allow_third_party_subresources: false,
            },
        }
    }
}

/// Browser engine backed by the standardized W3C `WebDriver` HTTP protocol.
///
/// It works with the platform browser driver rather than depending on a single
/// browser binary. Every DOM handle is remapped to an opaque, generation-bound
/// identifier so stale or page-supplied identifiers cannot be replayed.
pub struct WebDriverBrowser {
    config: WebDriverConfig,
    client: Client,
    profile: Option<BrowserProfile>,
    session_id: Option<String>,
    page_id: String,
    generation: u64,
    current: Option<PageSnapshot>,
    elements: BTreeMap<String, String>,
    takeover: bool,
}

/// Managed local browser-driver process. If a compatible driver was already
/// listening, `child` remains empty and closing this guard leaves it untouched.
pub struct WebDriverProcess {
    child: Option<Child>,
    pub executable: String,
    pub endpoint: Url,
}

impl WebDriverProcess {
    /// Start a matching installed driver or attach to an existing healthy one.
    ///
    /// # Errors
    ///
    /// Returns a [`BrowserError`] when the endpoint is invalid or no compatible
    /// local driver can start.
    pub async fn start(browser_name: &str, endpoint: Url) -> Result<Self, BrowserError> {
        if driver_is_healthy(&endpoint).await {
            return Ok(Self {
                child: None,
                executable: "external".into(),
                endpoint,
            });
        }
        let port = endpoint
            .port_or_known_default()
            .ok_or_else(|| BrowserError::Operation("WebDriver endpoint has no port".into()))?;
        let candidates: &[DriverCandidate] = match browser_name.to_ascii_lowercase().as_str() {
            "firefox" => &[("geckodriver", gecko_args)],
            "safari" => &[("safaridriver", safari_args)],
            "microsoftedge" | "edge" => &[("msedgedriver", chromium_args)],
            _ => &[
                ("chromedriver", chromium_args),
                ("msedgedriver", chromium_args),
            ],
        };
        let mut failures = Vec::new();
        for (executable, arguments) in candidates {
            let child = Command::new(executable)
                .args(arguments(port))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn();
            let Ok(mut child) = child else {
                failures.push(format!("{executable} is not installed or could not start"));
                continue;
            };
            let deadline = Instant::now() + Duration::from_secs(8);
            while Instant::now() < deadline {
                if driver_is_healthy(&endpoint).await {
                    return Ok(Self {
                        child: Some(child),
                        executable: (*executable).into(),
                        endpoint,
                    });
                }
                if child.try_wait().ok().flatten().is_some() {
                    break;
                }
                sleep(Duration::from_millis(100)).await;
            }
            let _ = child.kill().await;
            failures.push(format!("{executable} did not become healthy"));
        }
        Err(BrowserError::Unavailable(format!(
            "no compatible local WebDriver: {}",
            failures.join("; ")
        )))
    }

    /// Stop the managed driver while leaving attached external drivers running.
    ///
    /// # Errors
    ///
    /// Returns a [`BrowserError`] when the managed child cannot be terminated.
    pub async fn stop(&mut self) -> Result<(), BrowserError> {
        if let Some(child) = &mut self.child {
            child
                .kill()
                .await
                .map_err(|error| BrowserError::Operation(error.to_string()))?;
            let _ = child.wait().await;
        }
        self.child = None;
        Ok(())
    }
}

impl Drop for WebDriverProcess {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.start_kill();
        }
    }
}

async fn driver_is_healthy(endpoint: &Url) -> bool {
    let Ok(status) = endpoint.join("status") else {
        return false;
    };
    Client::new()
        .get(status)
        .timeout(Duration::from_millis(500))
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

fn gecko_args(port: u16) -> Vec<String> {
    vec!["--port".into(), port.to_string()]
}

fn safari_args(port: u16) -> Vec<String> {
    vec!["-p".into(), port.to_string()]
}

fn chromium_args(port: u16) -> Vec<String> {
    vec![format!("--port={port}")]
}

impl WebDriverBrowser {
    #[must_use]
    pub fn new(config: WebDriverConfig) -> Self {
        Self {
            config,
            client: Client::new(),
            profile: None,
            session_id: None,
            page_id: Uuid::new_v4().to_string(),
            generation: 0,
            current: None,
            elements: BTreeMap::new(),
            takeover: false,
        }
    }

    fn session(&self) -> Result<&str, BrowserError> {
        self.session_id
            .as_deref()
            .ok_or_else(|| BrowserError::Unavailable("WebDriver session is not open".into()))
    }

    fn endpoint(&self, path: &str) -> Result<Url, BrowserError> {
        self.config
            .endpoint
            .join(path.trim_start_matches('/'))
            .map_err(|error| BrowserError::Operation(error.to_string()))
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, BrowserError> {
        let url = self.endpoint(path)?;
        let mut request = self.client.request(method, url);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|error| {
            BrowserError::Unavailable(format!("cannot reach WebDriver: {error}"))
        })?;
        let status = response.status();
        let envelope = response.json::<Value>().await.map_err(|error| {
            BrowserError::Operation(format!("invalid WebDriver response: {error}"))
        })?;
        let value = envelope.get("value").cloned().unwrap_or(Value::Null);
        if !status.is_success() || value.get("error").is_some() {
            let code = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("webdriver_error");
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("WebDriver request failed");
            return Err(BrowserError::Operation(format!("{code}: {message}")));
        }
        Ok(value)
    }

    async fn read_string(&self, path: &str) -> Result<String, BrowserError> {
        self.request(Method::GET, path, None)
            .await?
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| BrowserError::Operation("WebDriver returned a non-string value".into()))
    }

    async fn find_elements(&self, session: &str) -> Result<Vec<String>, BrowserError> {
        let value = self
            .request(
                Method::POST,
                &format!("session/{session}/elements"),
                Some(json!({"using": "css selector", "value": INTERACTIVE_SELECTOR})),
            )
            .await?;
        Ok(value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|element| element.get(ELEMENT_KEY).and_then(Value::as_str))
            .map(str::to_owned)
            .collect())
    }

    async fn body_text(&self, session: &str) -> Result<String, BrowserError> {
        let body = self
            .request(
                Method::POST,
                &format!("session/{session}/element"),
                Some(json!({"using": "css selector", "value": "body"})),
            )
            .await?;
        let element = body
            .get(ELEMENT_KEY)
            .and_then(Value::as_str)
            .ok_or_else(|| BrowserError::Operation("document body is unavailable".into()))?;
        self.read_string(&format!("session/{session}/element/{element}/text"))
            .await
    }

    fn driver_element(&self, handle: &NodeHandle) -> Result<&str, BrowserError> {
        let snapshot = self.current.as_ref().ok_or(BrowserError::StaleHandle)?;
        validate_handle(snapshot, handle)?;
        self.elements
            .get(&handle.opaque_id)
            .map(String::as_str)
            .ok_or(BrowserError::StaleHandle)
    }
}

#[async_trait]
impl BrowserEngine for WebDriverBrowser {
    async fn open_isolated_profile(&mut self, profile_id: &str) -> Result<(), BrowserError> {
        if self.session_id.is_some() {
            return Err(BrowserError::Operation(
                "browser session is already open".into(),
            ));
        }
        let profile = BrowserProfile {
            id: profile_id.into(),
            isolated: true,
            personal_profile_opt_in: false,
        };
        profile.validate()?;

        let mut always_match = serde_json::Map::new();
        always_match.insert("browserName".into(), json!(self.config.browser_name));
        for (key, value) in &self.config.capabilities {
            always_match.insert(key.clone(), value.clone());
        }
        let value = self
            .request(
                Method::POST,
                "session",
                Some(json!({"capabilities": {"alwaysMatch": always_match}})),
            )
            .await?;
        let session_id = value
            .get("sessionId")
            .and_then(Value::as_str)
            .or_else(|| {
                value
                    .as_object()
                    .and_then(|_| value.get("session_id"))
                    .and_then(Value::as_str)
            })
            .ok_or_else(|| BrowserError::Operation("WebDriver omitted sessionId".into()))?;
        self.session_id = Some(session_id.into());
        self.profile = Some(profile);
        self.page_id = Uuid::new_v4().to_string();
        self.generation = 0;
        Ok(())
    }

    async fn navigate(&mut self, url: &Url) -> Result<PageSnapshot, BrowserError> {
        self.config.policy.allow_navigation(url)?;
        let session = self.session()?.to_owned();
        self.request(
            Method::POST,
            &format!("session/{session}/url"),
            Some(json!({"url": url.as_str()})),
        )
        .await?;
        self.snapshot().await
    }

    async fn snapshot(&mut self) -> Result<PageSnapshot, BrowserError> {
        let session = self.session()?.to_owned();
        let url = Url::parse(&self.read_string(&format!("session/{session}/url")).await?)
            .map_err(|error| BrowserError::Operation(format!("invalid page URL: {error}")))?;
        let title = self
            .read_string(&format!("session/{session}/title"))
            .await?;
        let text = self.body_text(&session).await.unwrap_or_default();
        let driver_elements = self.find_elements(&session).await?;

        self.generation = self.generation.saturating_add(1);
        self.elements.clear();
        let handles = driver_elements
            .into_iter()
            .map(|driver_id| {
                let opaque_id = Uuid::new_v4().to_string();
                self.elements.insert(opaque_id.clone(), driver_id);
                NodeHandle {
                    page_id: self.page_id.clone(),
                    generation: self.generation,
                    opaque_id,
                }
            })
            .collect();
        let snapshot = PageSnapshot {
            page_id: self.page_id.clone(),
            generation: self.generation,
            url,
            title,
            text,
            handles,
        };
        self.current = Some(snapshot.clone());
        Ok(snapshot)
    }

    async fn click(&mut self, handle: &NodeHandle) -> Result<PageSnapshot, BrowserError> {
        let session = self.session()?.to_owned();
        let element = self.driver_element(handle)?.to_owned();
        self.request(
            Method::POST,
            &format!("session/{session}/element/{element}/click"),
            Some(json!({})),
        )
        .await?;
        self.snapshot().await
    }

    async fn type_text(
        &mut self,
        handle: &NodeHandle,
        text: &str,
    ) -> Result<PageSnapshot, BrowserError> {
        let session = self.session()?.to_owned();
        let element = self.driver_element(handle)?.to_owned();
        let characters = text
            .chars()
            .map(|character| character.to_string())
            .collect::<Vec<_>>();
        self.request(
            Method::POST,
            &format!("session/{session}/element/{element}/value"),
            Some(json!({"text": text, "value": characters})),
        )
        .await?;
        self.snapshot().await
    }

    async fn takeover(&mut self) -> Result<(), BrowserError> {
        self.session()?;
        self.takeover = true;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), BrowserError> {
        if let Some(session) = self.session_id.take() {
            self.request(Method::DELETE, &format!("session/{session}"), None)
                .await?;
        }
        self.current = None;
        self.elements.clear();
        self.takeover = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_driver_is_local_and_policy_is_fail_closed_for_schemes() {
        let config = WebDriverConfig::default();
        assert_eq!(config.endpoint.as_str(), "http://127.0.0.1:4444/");
        assert!(
            config
                .policy
                .allow_navigation(&Url::parse("file:///etc/passwd").expect("fixture"))
                .is_err()
        );
    }

    #[test]
    fn opaque_handles_never_expose_driver_ids() {
        let mut browser = WebDriverBrowser::new(WebDriverConfig::default());
        browser
            .elements
            .insert("opaque".into(), "driver-secret".into());
        let serialized = serde_json::to_string(&NodeHandle {
            page_id: "page".into(),
            generation: 1,
            opaque_id: "opaque".into(),
        })
        .expect("serialize");
        assert!(!serialized.contains("driver-secret"));
    }
}
