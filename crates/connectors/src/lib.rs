//! Least-privilege connectors for coding and office workflows.

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use reqwest::{Client, Method};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
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

/// Reviewed OAuth providers supported by native connector onboarding.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthProvider {
    GitHub,
    Google,
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
    #[serde(rename = "oauth2")]
    OAuth2 {
        keychain_alias: String,
        #[serde(default)]
        refresh_keychain_alias: Option<String>,
        #[serde(default)]
        client_id: String,
        #[serde(default)]
        provider: Option<OAuthProvider>,
        #[serde(default)]
        scopes: BTreeSet<String>,
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

/// Provider-neutral OAuth failure that never includes token response bodies.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum OAuthError {
    #[error("OAuth configuration is invalid: {0}")]
    Invalid(String),
    #[error("OAuth provider rejected the request with HTTP {0}")]
    Rejected(u16),
    #[error("OAuth transport failed")]
    Transport,
    #[error("OAuth token response was invalid")]
    InvalidTokenResponse,
    #[error("this provider does not support secret-free refresh")]
    RefreshUnsupported,
    #[error("this provider does not expose secret-free remote revocation")]
    RevokeUnsupported,
}

/// Non-secret client registration. Public desktop clients use PKCE and never
/// require a client secret in this boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OAuthClientRegistration {
    pub client_id: String,
}

impl OAuthClientRegistration {
    /// Validate a public OAuth client identifier.
    ///
    /// # Errors
    ///
    /// Returns an error for blank, oversized, or control-character IDs.
    pub fn validate(&self) -> Result<(), OAuthError> {
        if self.client_id.trim().is_empty()
            || self.client_id.len() > 512
            || self.client_id.chars().any(char::is_control)
        {
            return Err(OAuthError::Invalid(
                "a valid public client ID is required".into(),
            ));
        }
        Ok(())
    }
}

/// Reviewed provider endpoints and behavior. Alternate loopback endpoints are
/// accepted only through [`OAuthProviderMetadata::new`] for local testing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OAuthProviderMetadata {
    pub provider: OAuthProvider,
    pub authorization_endpoint: Url,
    pub token_endpoint: Url,
    pub revocation_endpoint: Option<Url>,
    pub supports_refresh_without_secret: bool,
}

impl OAuthProviderMetadata {
    /// Return the production endpoint set reviewed for a provider.
    ///
    /// # Panics
    ///
    /// Panics only if a compile-time constant provider URL is malformed.
    #[must_use]
    pub fn reviewed(provider: OAuthProvider) -> Self {
        match provider {
            OAuthProvider::GitHub => Self {
                provider,
                authorization_endpoint: Url::parse("https://github.com/login/oauth/authorize")
                    .expect("reviewed GitHub authorization URL"),
                token_endpoint: Url::parse("https://github.com/login/oauth/access_token")
                    .expect("reviewed GitHub token URL"),
                // GitHub application-token deletion requires confidential app
                // credentials, so native revocation remains local-only.
                revocation_endpoint: None,
                supports_refresh_without_secret: false,
            },
            OAuthProvider::Google => Self {
                provider,
                authorization_endpoint: Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
                    .expect("reviewed Google authorization URL"),
                token_endpoint: Url::parse("https://oauth2.googleapis.com/token")
                    .expect("reviewed Google token URL"),
                revocation_endpoint: Some(
                    Url::parse("https://oauth2.googleapis.com/revoke")
                        .expect("reviewed Google revocation URL"),
                ),
                supports_refresh_without_secret: true,
            },
        }
    }

    /// Construct validated metadata for loopback mock servers.
    ///
    /// # Errors
    ///
    /// Rejects non-HTTPS endpoints except on loopback.
    pub fn new(
        provider: OAuthProvider,
        authorization_endpoint: Url,
        token_endpoint: Url,
        revocation_endpoint: Option<Url>,
        supports_refresh_without_secret: bool,
    ) -> Result<Self, OAuthError> {
        for endpoint in [&authorization_endpoint, &token_endpoint]
            .into_iter()
            .chain(revocation_endpoint.as_ref())
        {
            let loopback = matches!(endpoint.host_str(), Some("127.0.0.1" | "localhost" | "::1"));
            if endpoint.scheme() != "https" && !(endpoint.scheme() == "http" && loopback) {
                return Err(OAuthError::Invalid(
                    "OAuth endpoints must use HTTPS or loopback HTTP".into(),
                ));
            }
        }
        Ok(Self {
            provider,
            authorization_endpoint,
            token_endpoint,
            revocation_endpoint,
            supports_refresh_without_secret,
        })
    }
}

/// One in-memory PKCE authorization attempt. The verifier is deliberately
/// non-serializable and never leaves the native process.
pub struct OAuthAuthorization {
    pub authorization_url: Url,
    pub state: String,
    code_verifier: SecretString,
}

impl OAuthAuthorization {
    #[must_use]
    pub fn code_verifier(&self) -> &SecretString {
        &self.code_verifier
    }
}

/// Token material returned by an OAuth server. Callers must move these values
/// directly into an OS secret store and persist only their aliases.
pub struct OAuthTokens {
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
    pub expires_at: Option<DateTime<Utc>>,
    pub scopes: BTreeSet<String>,
}

#[derive(Deserialize)]
struct OAuthTokenWire {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
}

/// PKCE authorization-code client shared by GitHub and Google adapters.
#[derive(Clone)]
pub struct OAuthClient {
    metadata: OAuthProviderMetadata,
    client: Client,
}

impl OAuthClient {
    #[must_use]
    pub fn new(metadata: OAuthProviderMetadata) -> Self {
        Self {
            metadata,
            client: Client::new(),
        }
    }

    /// Create a fresh PKCE/state-bound authorization URL.
    ///
    /// # Errors
    ///
    /// Returns an error when registration, redirect, or scopes are invalid.
    pub fn authorize(
        &self,
        registration: &OAuthClientRegistration,
        redirect_uri: &Url,
        scopes: &BTreeSet<String>,
    ) -> Result<OAuthAuthorization, OAuthError> {
        registration.validate()?;
        if redirect_uri.scheme() != "http"
            || !matches!(
                redirect_uri.host_str(),
                Some("127.0.0.1" | "localhost" | "::1")
            )
        {
            return Err(OAuthError::Invalid(
                "desktop OAuth redirect must use loopback HTTP".into(),
            ));
        }
        if scopes.is_empty()
            || scopes
                .iter()
                .any(|scope| scope.trim().is_empty() || scope.chars().any(char::is_control))
        {
            return Err(OAuthError::Invalid(
                "at least one valid OAuth scope is required".into(),
            ));
        }
        let state = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let verifier = format!(
            "{}{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        );
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut authorization_url = self.metadata.authorization_endpoint.clone();
        {
            let mut query = authorization_url.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", registration.client_id.trim())
                .append_pair("redirect_uri", redirect_uri.as_str())
                .append_pair(
                    "scope",
                    &scopes.iter().cloned().collect::<Vec<_>>().join(" "),
                )
                .append_pair("state", &state)
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256");
            if self.metadata.provider == OAuthProvider::Google {
                query
                    .append_pair("access_type", "offline")
                    .append_pair("include_granted_scopes", "true")
                    .append_pair("prompt", "consent");
            }
        }
        Ok(OAuthAuthorization {
            authorization_url,
            state,
            code_verifier: SecretString::from(verifier),
        })
    }

    /// Exchange an authorization code with PKCE and no client secret.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for transport, rejection, or invalid response.
    pub async fn exchange_code(
        &self,
        registration: &OAuthClientRegistration,
        code: &SecretString,
        code_verifier: &SecretString,
        redirect_uri: &Url,
        requested_scopes: &BTreeSet<String>,
    ) -> Result<OAuthTokens, OAuthError> {
        registration.validate()?;
        let response = self
            .client
            .post(self.metadata.token_endpoint.clone())
            .header(reqwest::header::ACCEPT, "application/json")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .timeout(std::time::Duration::from_secs(30))
            .body(form_body(&[
                ("grant_type", "authorization_code"),
                ("client_id", registration.client_id.trim()),
                ("code", code.expose_secret()),
                ("code_verifier", code_verifier.expose_secret()),
                ("redirect_uri", redirect_uri.as_str()),
            ]))
            .send()
            .await
            .map_err(|_| OAuthError::Transport)?;
        self.parse_token_response(response, requested_scopes).await
    }

    /// Refresh a Google public-client grant without a client secret.
    ///
    /// # Errors
    ///
    /// Returns [`OAuthError::RefreshUnsupported`] for providers that require a
    /// confidential client credential.
    pub async fn refresh(
        &self,
        registration: &OAuthClientRegistration,
        refresh_token: &SecretString,
        scopes: &BTreeSet<String>,
    ) -> Result<OAuthTokens, OAuthError> {
        registration.validate()?;
        if !self.metadata.supports_refresh_without_secret {
            return Err(OAuthError::RefreshUnsupported);
        }
        let response = self
            .client
            .post(self.metadata.token_endpoint.clone())
            .header(reqwest::header::ACCEPT, "application/json")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .timeout(std::time::Duration::from_secs(30))
            .body(form_body(&[
                ("grant_type", "refresh_token"),
                ("client_id", registration.client_id.trim()),
                ("refresh_token", refresh_token.expose_secret()),
            ]))
            .send()
            .await
            .map_err(|_| OAuthError::Transport)?;
        self.parse_token_response(response, scopes).await
    }

    /// Revoke a token when the provider exposes a public-client endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`OAuthError::RevokeUnsupported`] for GitHub, whose application
    /// token deletion API requires confidential app credentials.
    pub async fn revoke(&self, token: &SecretString) -> Result<(), OAuthError> {
        let endpoint = self
            .metadata
            .revocation_endpoint
            .clone()
            .ok_or(OAuthError::RevokeUnsupported)?;
        let response = self
            .client
            .post(endpoint)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .timeout(std::time::Duration::from_secs(30))
            .body(form_body(&[("token", token.expose_secret())]))
            .send()
            .await
            .map_err(|_| OAuthError::Transport)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(OAuthError::Rejected(response.status().as_u16()))
        }
    }

    async fn parse_token_response(
        &self,
        response: reqwest::Response,
        requested_scopes: &BTreeSet<String>,
    ) -> Result<OAuthTokens, OAuthError> {
        if !response.status().is_success() {
            return Err(OAuthError::Rejected(response.status().as_u16()));
        }
        let wire = response
            .json::<OAuthTokenWire>()
            .await
            .map_err(|_| OAuthError::InvalidTokenResponse)?;
        if wire.access_token.trim().is_empty() {
            return Err(OAuthError::InvalidTokenResponse);
        }
        let expires_at = wire
            .expires_in
            .filter(|seconds| *seconds > 0)
            .map(|seconds| Utc::now() + chrono::Duration::seconds(seconds));
        let scopes = wire.scope.map_or_else(
            || requested_scopes.clone(),
            |scope| scope.split_whitespace().map(str::to_owned).collect(),
        );
        Ok(OAuthTokens {
            access_token: SecretString::from(wire.access_token),
            refresh_token: wire.refresh_token.map(SecretString::from),
            expires_at,
            scopes,
        })
    }
}

fn form_body(fields: &[(&str, &str)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.extend_pairs(fields.iter().copied());
    serializer.finish()
}

/// Safe default OAuth scopes. Built-ins begin with no external-write scope.
#[must_use]
pub fn default_oauth_scopes(kind: ConnectorKind) -> BTreeSet<String> {
    match kind {
        ConnectorKind::GitHub => ["read:user", "user:email"],
        ConnectorKind::Gmail => ["https://www.googleapis.com/auth/gmail.readonly", ""],
        ConnectorKind::GoogleCalendar => ["https://www.googleapis.com/auth/calendar.readonly", ""],
        _ => ["", ""],
    }
    .into_iter()
    .filter(|scope| !scope.is_empty())
    .map(str::to_owned)
    .collect()
}

/// OAuth provider used by a reviewed connector kind.
#[must_use]
pub fn oauth_provider(kind: ConnectorKind) -> Option<OAuthProvider> {
    match kind {
        ConnectorKind::GitHub => Some(OAuthProvider::GitHub),
        ConnectorKind::Gmail | ConnectorKind::GoogleCalendar => Some(OAuthProvider::Google),
        _ => None,
    }
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
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

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

    #[test]
    fn pkce_authorization_has_fresh_state_and_read_only_defaults() {
        let client = OAuthClient::new(OAuthProviderMetadata::reviewed(OAuthProvider::Google));
        let registration = OAuthClientRegistration {
            client_id: "public-desktop-client.apps.googleusercontent.com".into(),
        };
        let redirect = Url::parse("http://127.0.0.1:43123/oauth/callback").unwrap();
        let scopes = default_oauth_scopes(ConnectorKind::Gmail);
        let first = client.authorize(&registration, &redirect, &scopes).unwrap();
        let second = client.authorize(&registration, &redirect, &scopes).unwrap();
        assert_ne!(first.state, second.state);
        assert_ne!(
            first.code_verifier().expose_secret(),
            second.code_verifier().expose_secret()
        );
        let query = first
            .authorization_url
            .query_pairs()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            query
                .get("code_challenge_method")
                .map(std::convert::AsRef::as_ref),
            Some("S256")
        );
        assert_eq!(
            query.get("state").map(std::convert::AsRef::as_ref),
            Some(first.state.as_str())
        );
        assert!(query.contains_key("code_challenge"));
        assert!(!first.authorization_url.as_str().contains("client_secret"));
        assert!(scopes.iter().all(|scope| scope.ends_with(".readonly")));
    }

    #[tokio::test]
    async fn mock_server_covers_exchange_refresh_and_revoke_without_client_secret() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured = Arc::clone(&requests);
        let server = tokio::spawn(async move {
            for index in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = vec![0_u8; 8192];
                let read = stream.read(&mut bytes).await.unwrap();
                let request = String::from_utf8_lossy(&bytes[..read]).into_owned();
                captured.lock().unwrap().push(request.clone());
                let body = if index == 0 {
                    r#"{"access_token":"access-one","refresh_token":"refresh-one","expires_in":3600,"scope":"scope.read"}"#
                } else if index == 1 {
                    r#"{"access_token":"access-two","expires_in":3600,"scope":"scope.read"}"#
                } else {
                    "{}"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let root = Url::parse(&format!("http://{address}/")).unwrap();
        let metadata = OAuthProviderMetadata::new(
            OAuthProvider::Google,
            root.join("authorize").unwrap(),
            root.join("token").unwrap(),
            Some(root.join("revoke").unwrap()),
            true,
        )
        .unwrap();
        let client = OAuthClient::new(metadata);
        let registration = OAuthClientRegistration {
            client_id: "public-client-id".into(),
        };
        let scopes = BTreeSet::from(["scope.read".to_owned()]);
        let redirect = Url::parse("http://127.0.0.1:44444/oauth/callback").unwrap();
        let verifier = SecretString::from("v".repeat(64));
        let exchanged = client
            .exchange_code(
                &registration,
                &SecretString::from("authorization-code"),
                &verifier,
                &redirect,
                &scopes,
            )
            .await
            .unwrap();
        assert_eq!(exchanged.access_token.expose_secret(), "access-one");
        let refresh = exchanged.refresh_token.as_ref().unwrap();
        let refreshed = client
            .refresh(&registration, refresh, &scopes)
            .await
            .unwrap();
        assert_eq!(refreshed.access_token.expose_secret(), "access-two");
        client.revoke(refresh).await.unwrap();
        server.await.unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let joined = requests.join("\n");
        assert!(!joined.contains("client_secret"));
        assert!(requests[0].starts_with("POST /token "));
        assert!(requests[0].contains("grant_type=authorization_code"));
        assert!(requests[0].contains("code_verifier="));
        assert!(requests[1].contains("grant_type=refresh_token"));
        assert!(requests[2].starts_with("POST /revoke "));
    }
}
