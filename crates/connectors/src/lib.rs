//! Least-privilege connectors for coding and office workflows.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

/// Built-in services with reviewed endpoint and OAuth defaults.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorKind {
    GitHub,
    Gmail,
    GoogleCalendar,
    Slack,
    MicrosoftGraph,
    CustomRest,
}

/// An individual capability. Read and write are always separate grants.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ConnectorGrant {
    pub resource: String,
    pub action: ConnectorAction,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorAction {
    Read,
    Create,
    Update,
    Delete,
    Send,
}

/// Authentication references a keychain alias; serialized state never contains credentials.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnectorAuth {
    OAuth2 {
        keychain_alias: String,
        account_label: String,
        expires_at: Option<DateTime<Utc>>,
    },
    BearerToken {
        keychain_alias: String,
    },
    None,
}

/// Installable connector configuration stored in encrypted application state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConnectorConfig {
    pub id: Uuid,
    pub display_name: String,
    pub kind: ConnectorKind,
    pub base_url: Url,
    pub auth: ConnectorAuth,
    pub grants: BTreeSet<ConnectorGrant>,
    pub enabled: bool,
}

impl ConnectorConfig {
    /// Construct one of the reviewed built-in service profiles.
    #[must_use]
    pub fn built_in(kind: ConnectorKind, display_name: impl Into<String>) -> Self {
        let (base_url, grants) = built_in_defaults(kind);
        Self {
            id: Uuid::now_v7(),
            display_name: display_name.into(),
            kind,
            base_url,
            auth: ConnectorAuth::None,
            grants,
            enabled: false,
        }
    }

    /// Validate network scope, credential references, and non-empty identity.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectorError`] when the configuration violates transport,
    /// identity, or credential-reference requirements.
    pub fn validate(&self) -> Result<(), ConnectorError> {
        if self.display_name.trim().is_empty() {
            return Err(ConnectorError::Invalid(
                "display name cannot be blank".into(),
            ));
        }
        if self.base_url.scheme() != "https"
            && !matches!(self.base_url.host_str(), Some("127.0.0.1" | "localhost"))
        {
            return Err(ConnectorError::InsecureTransport);
        }
        let alias = match &self.auth {
            ConnectorAuth::OAuth2 { keychain_alias, .. }
            | ConnectorAuth::BearerToken { keychain_alias } => Some(keychain_alias),
            ConnectorAuth::None => None,
        };
        if alias.is_some_and(|alias| alias.trim().is_empty()) {
            return Err(ConnectorError::Invalid(
                "keychain alias cannot be blank".into(),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn permits(&self, grant: &ConnectorGrant) -> bool {
        self.enabled && self.grants.contains(grant)
    }
}

/// Cursor persisted after every successful incremental read.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncCursor {
    pub connector_id: Uuid,
    pub opaque_cursor: String,
    pub observed_at: DateTime<Utc>,
}

/// A request emitted by a planner only after the connector grant is checked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConnectorRequest {
    pub operation_id: Uuid,
    pub grant: ConnectorGrant,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    pub body: Option<Value>,
    pub idempotency_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConnectorResponse {
    pub status: u16,
    pub body: Value,
    pub next_cursor: Option<String>,
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ConnectorError {
    #[error("connector is disabled")]
    Disabled,
    #[error("connector grant was not approved: {0:?}")]
    GrantDenied(ConnectorGrant),
    #[error("connector credentials are unavailable")]
    CredentialsUnavailable,
    #[error("connector transport must use HTTPS except on loopback")]
    InsecureTransport,
    #[error("connector request path must stay under the configured origin")]
    CrossOrigin,
    #[error("connector configuration is invalid: {0}")]
    Invalid(String),
    #[error("connector transport failed: {0}")]
    Transport(String),
}

/// Credential provider implemented by the platform keychain adapter.
#[async_trait]
pub trait CredentialProvider: Send + Sync {
    async fn bearer_token(&self, keychain_alias: &str) -> Result<String, ConnectorError>;
}

/// Auditable connector execution boundary.
#[async_trait]
pub trait Connector: Send + Sync {
    fn config(&self) -> &ConnectorConfig;
    async fn execute(&self, request: ConnectorRequest)
    -> Result<ConnectorResponse, ConnectorError>;
}

/// Generic REST connector used by reviewed built-ins and custom services.
pub struct RestConnector<C> {
    config: ConnectorConfig,
    client: Client,
    credentials: C,
}

impl<C> RestConnector<C> {
    #[must_use]
    pub fn new(config: ConnectorConfig, credentials: C) -> Self {
        Self {
            config,
            client: Client::new(),
            credentials,
        }
    }

    fn request_url(&self, request: &ConnectorRequest) -> Result<Url, ConnectorError> {
        let path = request.path.trim_start_matches('/');
        let mut url = self
            .config
            .base_url
            .join(path)
            .map_err(|error| ConnectorError::Invalid(error.to_string()))?;
        if url.origin() != self.config.base_url.origin() {
            return Err(ConnectorError::CrossOrigin);
        }
        url.query_pairs_mut().extend_pairs(&request.query);
        Ok(url)
    }
}

#[async_trait]
impl<C: CredentialProvider> Connector for RestConnector<C> {
    fn config(&self) -> &ConnectorConfig {
        &self.config
    }

    async fn execute(
        &self,
        request: ConnectorRequest,
    ) -> Result<ConnectorResponse, ConnectorError> {
        self.config.validate()?;
        if !self.config.enabled {
            return Err(ConnectorError::Disabled);
        }
        if !self.config.permits(&request.grant) {
            return Err(ConnectorError::GrantDenied(request.grant));
        }
        let url = self.request_url(&request)?;
        let method = Method::from_bytes(request.method.as_bytes())
            .map_err(|error| ConnectorError::Invalid(error.to_string()))?;
        let mut outgoing = self.client.request(method, url);
        if let Some(key) = &request.idempotency_key {
            outgoing = outgoing.header("Idempotency-Key", key);
        }
        match &self.config.auth {
            ConnectorAuth::OAuth2 { keychain_alias, .. }
            | ConnectorAuth::BearerToken { keychain_alias } => {
                let token = self.credentials.bearer_token(keychain_alias).await?;
                outgoing = outgoing.bearer_auth(token);
            }
            ConnectorAuth::None => {}
        }
        if let Some(body) = &request.body {
            outgoing = outgoing.json(body);
        }
        let response = outgoing
            .send()
            .await
            .map_err(|error| ConnectorError::Transport(error.to_string()))?;
        let status = response.status().as_u16();
        let request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let next_cursor = response
            .headers()
            .get("x-next-cursor")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response
            .json::<Value>()
            .await
            .map_err(|error| ConnectorError::Transport(error.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(ConnectorError::Transport(format!("HTTP {status}: {body}")));
        }
        Ok(ConnectorResponse {
            status,
            body,
            next_cursor,
            request_id,
        })
    }
}

fn built_in_defaults(kind: ConnectorKind) -> (Url, BTreeSet<ConnectorGrant>) {
    let (base, resources): (&str, &[&str]) = match kind {
        ConnectorKind::GitHub => (
            "https://api.github.com/",
            &["repositories", "issues", "pull_requests"],
        ),
        ConnectorKind::Gmail => ("https://gmail.googleapis.com/", &["messages", "labels"]),
        ConnectorKind::GoogleCalendar => (
            "https://www.googleapis.com/calendar/v3/",
            &["calendars", "events"],
        ),
        ConnectorKind::Slack => ("https://slack.com/api/", &["channels", "messages"]),
        ConnectorKind::MicrosoftGraph => (
            "https://graph.microsoft.com/v1.0/",
            &["mail", "calendar", "files", "teams"],
        ),
        ConnectorKind::CustomRest => ("https://localhost/", &[]),
    };
    let grants = resources
        .iter()
        .map(|resource| ConnectorGrant {
            resource: (*resource).into(),
            action: ConnectorAction::Read,
        })
        .collect();
    (Url::parse(base).expect("reviewed built-in URL"), grants)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MissingCredentials;

    #[async_trait]
    impl CredentialProvider for MissingCredentials {
        async fn bearer_token(&self, _keychain_alias: &str) -> Result<String, ConnectorError> {
            Err(ConnectorError::CredentialsUnavailable)
        }
    }

    #[test]
    fn built_ins_begin_disabled_and_read_only() {
        let github = ConnectorConfig::built_in(ConnectorKind::GitHub, "Work GitHub");
        assert!(!github.enabled);
        assert!(
            github
                .grants
                .iter()
                .all(|grant| grant.action == ConnectorAction::Read)
        );
        assert!(github.validate().is_ok());
    }

    #[test]
    fn secrets_are_references_not_serialized_tokens() {
        let mut config = ConnectorConfig::built_in(ConnectorKind::Slack, "Slack");
        config.auth = ConnectorAuth::BearerToken {
            keychain_alias: "connector/slack/work".into(),
        };
        let serialized = serde_json::to_string(&config).expect("serialize");
        assert!(serialized.contains("connector/slack/work"));
        assert!(!serialized.contains("xoxb-"));
    }

    #[test]
    fn cross_origin_paths_are_rejected() {
        let config = ConnectorConfig::built_in(ConnectorKind::GitHub, "GitHub");
        let connector = RestConnector::new(config, MissingCredentials);
        let request = ConnectorRequest {
            operation_id: Uuid::now_v7(),
            grant: ConnectorGrant {
                resource: "repositories".into(),
                action: ConnectorAction::Read,
            },
            method: "GET".into(),
            path: "https://attacker.test/steal".into(),
            query: BTreeMap::new(),
            body: None,
            idempotency_key: None,
        };
        assert_eq!(
            connector.request_url(&request),
            Err(ConnectorError::CrossOrigin)
        );
    }
}
