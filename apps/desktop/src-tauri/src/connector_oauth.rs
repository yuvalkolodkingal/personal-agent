//! Native OAuth lifecycle for reviewed app connectors.

#![allow(clippy::needless_pass_by_value)] // Tauri deserializes and owns IPC arguments.

use crate::capabilities::CapabilityState;
use personal_agent_connectors::{
    ConnectorAuth, ConnectorConfig, OAuthClient, OAuthClientRegistration, OAuthProvider,
    OAuthProviderMetadata, OAuthTokens, default_oauth_scopes, oauth_provider,
};
use personal_agent_platform::{OsSecretStore, SecretReference, SecretStore, SecretStoreError};
use secrecy::SecretString;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use url::Url;
use uuid::Uuid;

const AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(300);
const CALLBACK_LIMIT: usize = 16 * 1024;
const OAUTH_SERVICE: &str = "personal-agent-connector-oauth";

#[derive(Default)]
pub(crate) struct ConnectorOAuthState {
    pending: Mutex<BTreeMap<Uuid, watch::Sender<bool>>>,
}

#[derive(Serialize)]
pub(crate) struct ConnectorOAuthResult {
    connector: ConnectorConfig,
    message: String,
    remote_revoked: Option<bool>,
}

impl ConnectorOAuthState {
    fn reserve(&self, id: Uuid) -> Result<Option<watch::Receiver<bool>>, String> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| "connector OAuth state lock is poisoned".to_owned())?;
        if pending.contains_key(&id) {
            return Ok(None);
        }
        let (sender, receiver) = watch::channel(false);
        pending.insert(id, sender);
        Ok(Some(receiver))
    }

    fn clear(&self, id: Uuid) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        } else {
            tracing::error!(connector_id = %id, "connector OAuth state lock is poisoned");
        }
    }

    fn cancel(&self, id: Uuid) -> Result<bool, String> {
        let pending = self
            .pending
            .lock()
            .map_err(|_| "connector OAuth state lock is poisoned".to_owned())?;
        Ok(pending
            .get(&id)
            .is_some_and(|sender| sender.send(true).is_ok()))
    }
}

struct AttemptGuard<'a> {
    state: &'a ConnectorOAuthState,
    id: Uuid,
}

impl Drop for AttemptGuard<'_> {
    fn drop(&mut self) {
        self.state.clear(self.id);
    }
}

#[tauri::command]
pub(crate) async fn connector_oauth_authorize(
    id: String,
    client_id: String,
    scopes: Option<Vec<String>>,
    connectors: tauri::State<'_, CapabilityState>,
    oauth: tauri::State<'_, ConnectorOAuthState>,
) -> Result<ConnectorOAuthResult, String> {
    let id = parse_id(&id)?;
    let config = connectors.connector(id)?;
    let provider = oauth_provider(config.kind)
        .ok_or_else(|| "this connector does not support native OAuth yet".to_owned())?;
    let requested_scopes = validated_scopes(config.kind, scopes)?;
    let Some(mut cancellation) = oauth.reserve(id)? else {
        return Err("authorization is already in progress for this connector".into());
    };
    let _guard = AttemptGuard { state: &oauth, id };
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|_| "could not reserve the private OAuth callback port".to_owned())?;
    let redirect_uri = Url::parse(&format!(
        "http://127.0.0.1:{}/oauth/callback",
        listener
            .local_addr()
            .map_err(|_| "OAuth callback port is unavailable".to_owned())?
            .port()
    ))
    .map_err(|_| "OAuth callback URL is invalid".to_owned())?;
    let registration = OAuthClientRegistration { client_id };
    let client = OAuthClient::new(OAuthProviderMetadata::reviewed(provider));
    let authorization = client
        .authorize(&registration, &redirect_uri, &requested_scopes)
        .map_err(|error| error.to_string())?;
    open_browser(&authorization.authorization_url)?;
    let code = await_callback(&listener, &authorization.state, &mut cancellation).await?;
    let tokens = client
        .exchange_code(
            &registration,
            &code,
            authorization.code_verifier(),
            &redirect_uri,
            &requested_scopes,
        )
        .await
        .map_err(|error| error.to_string())?;
    let connected = store_tokens_and_activate(
        &connectors,
        config.id,
        provider,
        registration.client_id,
        tokens,
    )?;
    Ok(ConnectorOAuthResult {
        connector: connected,
        message: "Authorization completed. Tokens are stored only in the OS keychain.".into(),
        remote_revoked: None,
    })
}

#[tauri::command]
pub(crate) fn connector_oauth_cancel(
    id: String,
    oauth: tauri::State<'_, ConnectorOAuthState>,
) -> Result<bool, String> {
    oauth.cancel(parse_id(&id)?)
}

#[tauri::command]
pub(crate) async fn connector_oauth_refresh(
    id: String,
    connectors: tauri::State<'_, CapabilityState>,
) -> Result<ConnectorOAuthResult, String> {
    let id = parse_id(&id)?;
    let connector = refresh_connector(id, &connectors, true).await?;
    Ok(ConnectorOAuthResult {
        connector,
        message: "OAuth access was refreshed through the OS keychain.".into(),
        remote_revoked: None,
    })
}

#[tauri::command]
pub(crate) async fn connector_oauth_revoke(
    id: String,
    confirmed: bool,
    connectors: tauri::State<'_, CapabilityState>,
) -> Result<ConnectorOAuthResult, String> {
    if !confirmed {
        return Err("revoking an app connection requires confirmation".into());
    }
    let id = parse_id(&id)?;
    let config = connectors.connector(id)?;
    let OAuthDetails {
        provider,
        access_reference,
        refresh_reference,
        ..
    } = oauth_details(&config)?;
    let remote_revoked = if provider == OAuthProvider::Google {
        let token = refresh_reference
            .as_ref()
            .and_then(|reference| OsSecretStore.get(reference).ok())
            .or_else(|| OsSecretStore.get(&access_reference).ok())
            .ok_or_else(|| "OAuth credentials are missing from the OS keychain".to_owned())?;
        OAuthClient::new(OAuthProviderMetadata::reviewed(provider))
            .revoke(&token)
            .await
            .map_err(|error| error.to_string())?;
        true
    } else {
        // GitHub's remote application-token deletion endpoint requires a
        // confidential client secret. The desktop app intentionally has none.
        false
    };
    delete_token(&access_reference)?;
    if let Some(reference) = &refresh_reference {
        delete_token(reference)?;
    }
    let connector = connectors.mutate_connectors(|items| {
        let connector = items
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| "connector does not exist".to_owned())?;
        connector.auth = ConnectorAuth::None;
        connector.enabled = false;
        Ok(connector.clone())
    })?;
    let message = if remote_revoked {
        "Google authorization was revoked remotely and removed from the OS keychain."
    } else {
        "GitHub credentials were removed locally. Revoke Personal Agent under GitHub Settings → Applications to invalidate the remote grant."
    };
    Ok(ConnectorOAuthResult {
        connector,
        message: message.into(),
        remote_revoked: Some(remote_revoked),
    })
}

pub(crate) async fn refresh_if_needed(
    id: Uuid,
    connectors: &CapabilityState,
) -> Result<(), String> {
    let config = connectors.connector(id)?;
    let should_refresh = match &config.auth {
        ConnectorAuth::OAuth2 {
            expires_at: Some(expires_at),
            ..
        } => *expires_at <= chrono::Utc::now() + chrono::Duration::minutes(1),
        _ => false,
    };
    if should_refresh {
        refresh_connector(id, connectors, false).await?;
    }
    Ok(())
}

async fn refresh_connector(
    id: Uuid,
    connectors: &CapabilityState,
    force: bool,
) -> Result<ConnectorConfig, String> {
    let config = connectors.connector(id)?;
    let details = oauth_details(&config)?;
    if !force
        && details
            .expires_at
            .is_none_or(|expires_at| expires_at > chrono::Utc::now() + chrono::Duration::minutes(1))
    {
        return Ok(config);
    }
    let refresh_reference = details
        .refresh_reference
        .as_ref()
        .ok_or_else(|| "this authorization has no refresh token; retry Connect OAuth".to_owned())?;
    let refresh_token = OsSecretStore
        .get(refresh_reference)
        .map_err(|_| "OAuth refresh token is missing from the OS keychain".to_owned())?;
    let client = OAuthClient::new(OAuthProviderMetadata::reviewed(details.provider));
    let tokens = client
        .refresh(
            &OAuthClientRegistration {
                client_id: details.client_id.clone(),
            },
            &refresh_token,
            &details.scopes,
        )
        .await
        .map_err(|error| error.to_string())?;
    OsSecretStore
        .put(&details.access_reference, &tokens.access_token)
        .map_err(|error| error.to_string())?;
    if let Some(new_refresh_token) = &tokens.refresh_token {
        OsSecretStore
            .put(refresh_reference, new_refresh_token)
            .map_err(|error| error.to_string())?;
    }
    connectors.mutate_connectors(|items| {
        let connector = items
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| "connector does not exist".to_owned())?;
        let ConnectorAuth::OAuth2 {
            expires_at, scopes, ..
        } = &mut connector.auth
        else {
            return Err("connector authorization changed while refreshing".into());
        };
        *expires_at = tokens.expires_at;
        *scopes = tokens.scopes;
        Ok(connector.clone())
    })
}

struct OAuthDetails {
    provider: OAuthProvider,
    client_id: String,
    scopes: BTreeSet<String>,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    access_reference: SecretReference,
    refresh_reference: Option<SecretReference>,
}

fn oauth_details(config: &ConnectorConfig) -> Result<OAuthDetails, String> {
    let ConnectorAuth::OAuth2 {
        keychain_alias,
        refresh_keychain_alias,
        client_id,
        provider,
        scopes,
        expires_at,
        ..
    } = &config.auth
    else {
        return Err("connector is not authorized with OAuth".into());
    };
    Ok(OAuthDetails {
        provider: provider.ok_or_else(|| "legacy OAuth grant must be reconnected".to_owned())?,
        client_id: client_id.clone(),
        scopes: scopes.clone(),
        expires_at: *expires_at,
        access_reference: SecretReference::parse(keychain_alias)
            .map_err(|error| error.to_string())?,
        refresh_reference: refresh_keychain_alias
            .as_deref()
            .map(SecretReference::parse)
            .transpose()
            .map_err(|error| error.to_string())?,
    })
}

fn validated_scopes(
    kind: personal_agent_connectors::ConnectorKind,
    requested: Option<Vec<String>>,
) -> Result<BTreeSet<String>, String> {
    let defaults = default_oauth_scopes(kind);
    let requested = requested.map_or_else(|| defaults.clone(), BTreeSet::from_iter);
    if requested.is_empty() || !requested.is_subset(&defaults) {
        return Err(
            "OAuth scopes must be a non-empty subset of this connector's reviewed read-only defaults"
                .into(),
        );
    }
    Ok(requested)
}

fn store_tokens_and_activate(
    connectors: &CapabilityState,
    connector_id: Uuid,
    provider: OAuthProvider,
    client_id: String,
    tokens: OAuthTokens,
) -> Result<ConnectorConfig, String> {
    let access_reference = token_reference(connector_id, "access");
    let refresh_reference = tokens
        .refresh_token
        .as_ref()
        .map(|_| token_reference(connector_id, "refresh"));
    OsSecretStore
        .put(&access_reference, &tokens.access_token)
        .map_err(|error| error.to_string())?;
    if let (Some(reference), Some(refresh_token)) = (&refresh_reference, &tokens.refresh_token)
        && let Err(error) = OsSecretStore.put(reference, refresh_token)
    {
        delete_if_present(&access_reference);
        return Err(error.to_string());
    }
    let result = connectors.mutate_connectors(|items| {
        let connector = items
            .iter_mut()
            .find(|item| item.id == connector_id)
            .ok_or_else(|| "connector does not exist".to_owned())?;
        connector.auth = ConnectorAuth::OAuth2 {
            keychain_alias: access_reference.alias(),
            refresh_keychain_alias: refresh_reference.as_ref().map(SecretReference::alias),
            client_id,
            provider: Some(provider),
            scopes: tokens.scopes,
            account_label: match provider {
                OAuthProvider::GitHub => "GitHub OAuth".into(),
                OAuthProvider::Google => "Google OAuth".into(),
            },
            expires_at: tokens.expires_at,
        };
        connector.enabled = true;
        connector.validate().map_err(|error| error.to_string())?;
        Ok(connector.clone())
    });
    if result.is_err() {
        delete_if_present(&access_reference);
        if let Some(reference) = &refresh_reference {
            delete_if_present(reference);
        }
    }
    result
}

fn token_reference(id: Uuid, kind: &str) -> SecretReference {
    SecretReference {
        service: OAUTH_SERVICE.into(),
        account: format!("{id}-{kind}"),
    }
}

fn delete_if_present(reference: &SecretReference) {
    let _ = OsSecretStore.delete(reference);
}

fn delete_token(reference: &SecretReference) -> Result<(), String> {
    match OsSecretStore.delete(reference) {
        Ok(()) | Err(SecretStoreError::Missing) => Ok(()),
        Err(error) => Err(format!(
            "authorization could not be fully removed from the OS keychain: {error}"
        )),
    }
}

fn parse_id(id: &str) -> Result<Uuid, String> {
    Uuid::parse_str(id).map_err(|_| "connector ID is invalid".to_owned())
}

fn open_browser(url: &Url) -> Result<(), String> {
    if url.scheme() != "https"
        && !(url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1")))
    {
        return Err("authorization URL is not a reviewed HTTPS or loopback URL".into());
    }
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.arg(url.as_str());
        command
    } else if cfg!(target_os = "windows") {
        let mut command = Command::new("rundll32");
        command.args(["url.dll,FileProtocolHandler", url.as_str()]);
        command
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(url.as_str());
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| "the system browser could not be opened".to_owned())
}

async fn await_callback(
    listener: &TcpListener,
    expected_state: &str,
    cancellation: &mut watch::Receiver<bool>,
) -> Result<SecretString, String> {
    tokio::time::timeout(AUTHORIZATION_TIMEOUT, async {
        loop {
            tokio::select! {
                changed = cancellation.changed() => {
                    if changed.is_err() || *cancellation.borrow() {
                        return Err("authorization was cancelled; close the browser page and retry when ready".into());
                    }
                }
                accepted = listener.accept() => {
                    let (mut stream, _) = accepted.map_err(|_| "OAuth callback listener failed".to_owned())?;
                    match read_callback(&mut stream, expected_state).await {
                        Ok(code) => {
                            write_callback_page(&mut stream, 200, "Connected", "Authorization completed. You can return to Personal Agent.").await;
                            return Ok(code);
                        }
                        Err(error) => {
                            write_callback_page(&mut stream, 400, "Not connected", &error).await;
                            if error.starts_with("authorization provider returned") {
                                return Err(error);
                            }
                        }
                    }
                }
            }
        }
    })
    .await
    .map_err(|_| "authorization expired after five minutes; close the browser page and retry".to_owned())?
}

async fn read_callback(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
    expected_state: &str,
) -> Result<SecretString, String> {
    let mut bytes = vec![0_u8; CALLBACK_LIMIT];
    let read = tokio::time::timeout(Duration::from_secs(5), async {
        let mut read = 0;
        loop {
            if bytes[..read].windows(4).any(|window| window == b"\r\n\r\n") {
                return Ok(read);
            }
            if read == CALLBACK_LIMIT {
                return Err("OAuth callback request exceeded the 16 KiB limit".to_owned());
            }
            let received = stream
                .read(&mut bytes[read..])
                .await
                .map_err(|_| "OAuth callback request could not be read".to_owned())?;
            if received == 0 {
                return Err(
                    "OAuth callback request ended before its headers were complete".to_owned(),
                );
            }
            read += received;
        }
    })
    .await
    .map_err(|_| "OAuth callback request timed out".to_owned())??;
    let request = std::str::from_utf8(&bytes[..read])
        .map_err(|_| "OAuth callback request was invalid".to_owned())?;
    let target = request
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|line| line.strip_suffix(" HTTP/1.1"))
        .ok_or_else(|| "OAuth callback must be an HTTP GET".to_owned())?;
    let callback = Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|_| "OAuth callback URL was invalid".to_owned())?;
    if callback.path() != "/oauth/callback" {
        return Err("OAuth callback path did not match".into());
    }
    let query = callback.query_pairs().collect::<BTreeMap<_, _>>();
    if let Some(error) = query.get("error") {
        return Err(format!(
            "authorization provider returned {}",
            bounded_label(error)
        ));
    }
    if query.get("state").map(std::convert::AsRef::as_ref) != Some(expected_state) {
        return Err("OAuth state did not match; the callback was rejected".into());
    }
    query
        .get("code")
        .filter(|code| !code.is_empty() && code.len() <= 4096)
        .map(|code| SecretString::from(code.to_string()))
        .ok_or_else(|| "OAuth callback did not contain an authorization code".to_owned())
}

fn bounded_label(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .take(64)
        .collect()
}

async fn write_callback_page(stream: &mut TcpStream, status: u16, title: &str, detail: &str) {
    let detail = detail
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>{title}</title><style>body{{font:16px system-ui;background:#071019;color:#dcebf2;padding:3rem;max-width:42rem;margin:auto}}h1{{color:#42d9ef}}</style><h1>{title}</h1><p>{detail}</p>"
    );
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\nX-Content-Type-Options: nosniff\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret as _;

    #[test]
    fn reviewed_scopes_are_read_only_and_cannot_be_widened() {
        let gmail =
            validated_scopes(personal_agent_connectors::ConnectorKind::Gmail, None).unwrap();
        assert_eq!(gmail.len(), 1);
        assert!(gmail.iter().all(|scope| scope.ends_with(".readonly")));
        assert!(
            validated_scopes(
                personal_agent_connectors::ConnectorKind::Gmail,
                Some(vec!["https://mail.google.com/".into()])
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn loopback_callback_rejects_state_mismatch_and_accepts_exact_state() {
        async fn callback_request(target: &str) -> Result<SecretString, String> {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = listener.local_addr().unwrap();
            let target = target.to_owned();
            let client = tokio::spawn(async move {
                let mut stream = TcpStream::connect(address).await.unwrap();
                stream
                    .write_all(
                        format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes(),
                    )
                    .await
                    .unwrap();
            });
            let (mut stream, _) = listener.accept().await.unwrap();
            let result = read_callback(&mut stream, "expected-state").await;
            client.await.unwrap();
            result
        }

        assert!(
            callback_request("/oauth/callback?code=private-code&state=wrong")
                .await
                .unwrap_err()
                .contains("state did not match")
        );
        let code = callback_request("/oauth/callback?code=private-code&state=expected-state")
            .await
            .unwrap();
        assert_eq!(code.expose_secret(), "private-code");
    }

    #[tokio::test]
    async fn loopback_callback_accepts_request_in_one_byte_chunks() {
        let (mut client, mut server) = tokio::io::duplex(1);
        let request =
            b"GET /oauth/callback?code=private-code&state=expected-state HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        let writer = tokio::spawn(async move {
            for byte in request {
                client.write_all(std::slice::from_ref(byte)).await.unwrap();
            }
        });

        let code = read_callback(&mut server, "expected-state").await.unwrap();
        writer.await.unwrap();
        assert_eq!(code.expose_secret(), "private-code");
    }

    #[test]
    fn persisted_connector_auth_contains_only_keychain_references() {
        let mut connector =
            ConnectorConfig::built_in(personal_agent_connectors::ConnectorKind::GitHub, "GitHub");
        connector.auth = ConnectorAuth::OAuth2 {
            keychain_alias: token_reference(connector.id, "access").alias(),
            refresh_keychain_alias: None,
            client_id: "public-client-id".into(),
            provider: Some(OAuthProvider::GitHub),
            scopes: default_oauth_scopes(connector.kind),
            account_label: "GitHub OAuth".into(),
            expires_at: None,
        };
        let serialized = serde_json::to_string(&connector).unwrap();
        assert!(serialized.contains("keychain://"));
        assert!(!serialized.contains("private-code"));
        assert!(!serialized.contains("access_token"));
        assert!(!serialized.contains("refresh_token"));
    }
}
