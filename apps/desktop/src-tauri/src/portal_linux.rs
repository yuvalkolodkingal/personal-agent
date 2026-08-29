//! Permission-aware XDG Desktop Portal session management for Wayland.
//!
//! The portal owns target selection and consent. Session and request handles are
//! never persisted, every request is bounded/cancellable, and shutdown closes
//! any live session. Pixel transport remains a separate `PipeWire` concern.

use async_trait::async_trait;
use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;
use zbus::zvariant::{DynamicType, OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

const DESKTOP_DESTINATION: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
const SCREENCAST: &str = "org.freedesktop.portal.ScreenCast";
const REMOTE_DESKTOP: &str = "org.freedesktop.portal.RemoteDesktop";
const REQUEST: &str = "org.freedesktop.portal.Request";
const SESSION: &str = "org.freedesktop.portal.Session";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PortalSessionKind {
    ScreenCast,
    RemoteDesktop,
}

impl PortalSessionKind {
    fn interface(self) -> &'static str {
        match self {
            Self::ScreenCast => SCREENCAST,
            Self::RemoteDesktop => REMOTE_DESKTOP,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PortalSessionPhase {
    Idle,
    Probing,
    Creating,
    Selecting,
    AwaitingConsent,
    Active,
    Cancelling,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PortalConsentState {
    Required,
    Requesting,
    Granted,
    Cancelled,
    Denied,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PortalInterfaces {
    pub screencast_version: Option<u32>,
    pub remote_desktop_version: Option<u32>,
    pub available_source_types: u32,
    pub available_cursor_modes: u32,
}

impl PortalInterfaces {
    fn unavailable() -> Self {
        Self {
            screencast_version: None,
            remote_desktop_version: None,
            available_source_types: 0,
            available_cursor_modes: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PortalStream {
    pub node_id: u32,
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PortalStatus {
    pub interfaces: PortalInterfaces,
    pub phase: PortalSessionPhase,
    pub consent: PortalConsentState,
    pub kind: Option<PortalSessionKind>,
    pub streams: Vec<PortalStream>,
    pub pipewire_transport: bool,
    pub detail: String,
}

impl Default for PortalStatus {
    fn default() -> Self {
        Self {
            interfaces: PortalInterfaces::unavailable(),
            phase: PortalSessionPhase::Idle,
            consent: PortalConsentState::Required,
            kind: None,
            streams: Vec::new(),
            pipewire_transport: false,
            detail: "Portal capability has not been probed".into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PortalSession {
    handle: String,
    kind: PortalSessionKind,
    streams: Vec<PortalStream>,
}

#[derive(Clone, Default)]
struct CancellationSignal {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl CancellationSignal {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        self.notify.notified().await;
    }
}

#[async_trait]
trait PortalBoundary: Send + Sync {
    async fn probe(&self) -> Result<PortalInterfaces, String>;
    async fn create_session(
        &self,
        kind: PortalSessionKind,
        cancel: &CancellationSignal,
    ) -> Result<String, String>;
    async fn select(
        &self,
        kind: PortalSessionKind,
        session: &str,
        cancel: &CancellationSignal,
    ) -> Result<(), String>;
    async fn start(
        &self,
        kind: PortalSessionKind,
        session: &str,
        parent_window: &str,
        cancel: &CancellationSignal,
    ) -> Result<Vec<PortalStream>, String>;
    async fn close_session(&self, session: &str) -> Result<(), String>;
    async fn notify_pointer_axis(
        &self,
        session: &str,
        delta_x: f64,
        delta_y: f64,
    ) -> Result<(), String>;
}

pub(crate) struct WaylandPortalManager {
    boundary: Arc<dyn PortalBoundary>,
    status: RwLock<PortalStatus>,
    operation: Mutex<()>,
    cancellation: Mutex<Option<CancellationSignal>>,
    session: Mutex<Option<PortalSession>>,
}

impl WaylandPortalManager {
    #[must_use]
    pub(crate) fn live() -> Arc<Self> {
        Arc::new(Self::new(Arc::new(LivePortalBoundary::default())))
    }

    fn new(boundary: Arc<dyn PortalBoundary>) -> Self {
        Self {
            boundary,
            status: RwLock::new(PortalStatus::default()),
            operation: Mutex::new(()),
            cancellation: Mutex::new(None),
            session: Mutex::new(None),
        }
    }

    #[must_use]
    pub(crate) fn status(&self) -> PortalStatus {
        self.status
            .read()
            .map_or_else(|_| PortalStatus::default(), |status| status.clone())
    }

    fn update_status(&self, update: impl FnOnce(&mut PortalStatus)) {
        if let Ok(mut status) = self.status.write() {
            update(&mut status);
        }
    }

    pub(crate) async fn probe(&self) -> PortalStatus {
        self.update_status(|status| {
            status.phase = PortalSessionPhase::Probing;
            status.detail = "Checking XDG Desktop Portal interfaces".into();
        });
        match self.boundary.probe().await {
            Ok(interfaces) => self.update_status(|status| {
                status.interfaces = interfaces;
                if status.phase == PortalSessionPhase::Probing {
                    status.phase = PortalSessionPhase::Idle;
                }
                status.consent = if status.interfaces.screencast_version.is_some() {
                    PortalConsentState::Required
                } else {
                    PortalConsentState::Unavailable
                };
                status.detail = portal_probe_detail(&status.interfaces);
            }),
            Err(error) => self.update_status(|status| {
                status.interfaces = PortalInterfaces::unavailable();
                status.phase = PortalSessionPhase::Failed;
                status.consent = PortalConsentState::Unavailable;
                status.detail = bounded_detail(&error);
            }),
        }
        self.status()
    }

    pub(crate) async fn connect(
        &self,
        request_control: bool,
        parent_window: &str,
    ) -> Result<PortalStatus, String> {
        let _operation = self.operation.lock().await;
        self.close_current_session().await;
        let interfaces = self.boundary.probe().await?;
        let kind = if request_control {
            if interfaces.remote_desktop_version.is_none() {
                self.update_status(|status| {
                    status.interfaces = interfaces.clone();
                    status.phase = PortalSessionPhase::Failed;
                    status.consent = PortalConsentState::Unavailable;
                    status.detail =
                        "This portal backend does not expose RemoteDesktop control".into();
                });
                return Err("XDG RemoteDesktop is unavailable on this desktop portal".into());
            }
            PortalSessionKind::RemoteDesktop
        } else {
            if interfaces.screencast_version.is_none() {
                return Err("XDG ScreenCast is unavailable on this desktop portal".into());
            }
            PortalSessionKind::ScreenCast
        };
        let cancellation = CancellationSignal::default();
        *self.cancellation.lock().await = Some(cancellation.clone());
        self.update_status(|status| {
            status.interfaces = interfaces;
            status.phase = PortalSessionPhase::Creating;
            status.consent = PortalConsentState::Requesting;
            status.kind = Some(kind);
            status.streams.clear();
            status.pipewire_transport = false;
            status.detail = "Creating an ephemeral portal session".into();
        });

        let result = self.connect_inner(kind, parent_window, &cancellation).await;
        *self.cancellation.lock().await = None;
        match result {
            Ok(session) => {
                let streams = session.streams.clone();
                *self.session.lock().await = Some(session);
                self.update_status(|status| {
                    status.phase = PortalSessionPhase::Active;
                    status.consent = PortalConsentState::Granted;
                    status.streams = streams;
                    status.pipewire_transport = false;
                    status.detail = "Portal selection is active. PipeWire frame transport is not connected; capture continues through the audited fallback.".into();
                });
                Ok(self.status())
            }
            Err(error) => {
                let cancelled = cancellation.is_cancelled() || error.starts_with("cancelled:");
                self.update_status(|status| {
                    status.phase = if cancelled {
                        PortalSessionPhase::Cancelled
                    } else {
                        PortalSessionPhase::Failed
                    };
                    status.consent = if cancelled {
                        PortalConsentState::Cancelled
                    } else if error.starts_with("denied:") {
                        PortalConsentState::Denied
                    } else {
                        PortalConsentState::Required
                    };
                    status.detail = bounded_detail(&error);
                    status.streams.clear();
                });
                Err(error)
            }
        }
    }

    async fn connect_inner(
        &self,
        kind: PortalSessionKind,
        parent_window: &str,
        cancellation: &CancellationSignal,
    ) -> Result<PortalSession, String> {
        let handle = self.boundary.create_session(kind, cancellation).await?;
        self.update_status(|status| {
            status.phase = PortalSessionPhase::Selecting;
            status.detail = "Requesting a user-selected source".into();
        });
        if let Err(error) = self.boundary.select(kind, &handle, cancellation).await {
            let _ = self.boundary.close_session(&handle).await;
            return Err(error);
        }
        self.update_status(|status| {
            status.phase = PortalSessionPhase::AwaitingConsent;
            status.detail = "Waiting for the system portal selection".into();
        });
        let streams = match self
            .boundary
            .start(kind, &handle, parent_window, cancellation)
            .await
        {
            Ok(streams) if !streams.is_empty() => streams,
            Ok(_) => {
                let _ = self.boundary.close_session(&handle).await;
                return Err("denied: portal returned no selected streams".into());
            }
            Err(error) => {
                let _ = self.boundary.close_session(&handle).await;
                return Err(error);
            }
        };
        Ok(PortalSession {
            handle,
            kind,
            streams,
        })
    }

    pub(crate) async fn cancel(&self) -> PortalStatus {
        self.update_status(|status| {
            status.phase = PortalSessionPhase::Cancelling;
            status.detail = "Cancelling the portal request".into();
        });
        if let Some(cancellation) = self.cancellation.lock().await.as_ref() {
            cancellation.cancel();
        } else {
            self.close_current_session().await;
            self.update_status(|status| {
                status.phase = PortalSessionPhase::Cancelled;
                status.consent = PortalConsentState::Cancelled;
                status.streams.clear();
                status.detail = "Portal session closed".into();
            });
        }
        self.status()
    }

    pub(crate) async fn disconnect(&self) -> PortalStatus {
        if let Some(cancellation) = self.cancellation.lock().await.as_ref() {
            cancellation.cancel();
        }
        let _operation = self.operation.lock().await;
        self.close_current_session().await;
        self.update_status(|status| {
            status.phase = PortalSessionPhase::Idle;
            status.consent = if status.interfaces.screencast_version.is_some() {
                PortalConsentState::Required
            } else {
                PortalConsentState::Unavailable
            };
            status.kind = None;
            status.streams.clear();
            status.pipewire_transport = false;
            status.detail = portal_probe_detail(&status.interfaces);
        });
        self.status()
    }

    async fn close_current_session(&self) {
        if let Some(session) = self.session.lock().await.take() {
            let _ = self.boundary.close_session(&session.handle).await;
        }
    }

    pub(crate) async fn notify_pointer_axis(
        &self,
        delta_x: f64,
        delta_y: f64,
    ) -> Result<(), String> {
        let session =
            self.session.lock().await.clone().ok_or_else(|| {
                "no user-granted RemoteDesktop portal session is active".to_owned()
            })?;
        if session.kind != PortalSessionKind::RemoteDesktop {
            return Err("active portal session does not grant input control".into());
        }
        self.boundary
            .notify_pointer_axis(&session.handle, delta_x, delta_y)
            .await
    }
}

fn portal_probe_detail(interfaces: &PortalInterfaces) -> String {
    match (
        interfaces.screencast_version,
        interfaces.remote_desktop_version,
    ) {
        (Some(screen), Some(remote)) => {
            format!(
                "ScreenCast v{screen} and RemoteDesktop v{remote} are available; consent is required"
            )
        }
        (Some(screen), None) => format!(
            "ScreenCast v{screen} is available; RemoteDesktop control is not exposed by this portal backend"
        ),
        _ => "The XDG ScreenCast portal is unavailable".into(),
    }
}

fn bounded_detail(detail: &str) -> String {
    detail.chars().take(512).collect()
}

#[derive(Default)]
struct LivePortalBoundary {
    connection: Mutex<Option<Connection>>,
}

impl LivePortalBoundary {
    async fn connection(&self) -> Result<Connection, String> {
        let mut connection = self.connection.lock().await;
        if let Some(connection) = connection.as_ref() {
            return Ok(connection.clone());
        }
        let connected = Connection::session()
            .await
            .map_err(|error| format!("portal session bus connection failed: {error}"))?;
        *connection = Some(connected.clone());
        Ok(connected)
    }

    async fn interface_version(connection: &Connection, interface: &str) -> Option<u32> {
        let proxy = Proxy::new(connection, DESKTOP_DESTINATION, DESKTOP_PATH, interface)
            .await
            .ok()?;
        proxy.get_property("version").await.ok()
    }

    async fn screencast_property(connection: &Connection, property: &str) -> u32 {
        let Ok(proxy) = Proxy::new(connection, DESKTOP_DESTINATION, DESKTOP_PATH, SCREENCAST).await
        else {
            return 0;
        };
        proxy.get_property(property).await.unwrap_or_default()
    }

    async fn remote_desktop_property(connection: &Connection, property: &str) -> u32 {
        let Ok(proxy) = Proxy::new(
            connection,
            DESKTOP_DESTINATION,
            DESKTOP_PATH,
            REMOTE_DESKTOP,
        )
        .await
        else {
            return 0;
        };
        proxy.get_property(property).await.unwrap_or_default()
    }

    fn token(prefix: &str) -> String {
        format!("{prefix}_{}", Uuid::new_v4().simple())
    }

    fn request_path(connection: &Connection, token: &str) -> Result<OwnedObjectPath, String> {
        let sender = connection
            .unique_name()
            .ok_or_else(|| "portal D-Bus connection has no unique name".to_owned())?
            .as_str()
            .trim_start_matches(':')
            .replace('.', "_");
        OwnedObjectPath::try_from(format!(
            "/org/freedesktop/portal/desktop/request/{sender}/{token}"
        ))
        .map_err(|error| format!("portal request path is invalid: {error}"))
    }

    async fn request<T>(
        connection: &Connection,
        interface: &str,
        method: &str,
        token: &str,
        body: &T,
        cancel: &CancellationSignal,
    ) -> Result<HashMap<String, OwnedValue>, String>
    where
        T: Serialize + DynamicType + Sync,
    {
        let expected = Self::request_path(connection, token)?;
        let request = Proxy::new(connection, DESKTOP_DESTINATION, expected.as_str(), REQUEST)
            .await
            .map_err(|error| format!("portal request proxy failed: {error}"))?;
        let mut responses = request
            .receive_signal("Response")
            .await
            .map_err(|error| format!("portal response subscription failed: {error}"))?;
        let desktop = Proxy::new(connection, DESKTOP_DESTINATION, DESKTOP_PATH, interface)
            .await
            .map_err(|error| format!("portal interface {interface} is unavailable: {error}"))?;
        let returned: OwnedObjectPath = desktop
            .call(method, body)
            .await
            .map_err(|error| format!("portal {method} request failed: {error}"))?;
        if returned != expected {
            let _ = Self::close_request(connection, returned.as_str()).await;
            return Err("portal returned an unexpected request handle".into());
        }
        tokio::select! {
            response = responses.next() => {
                let message = response.ok_or_else(|| "portal response stream ended".to_owned())?;
                let (code, results): (u32, HashMap<String, OwnedValue>) = message
                    .body()
                    .deserialize()
                    .map_err(|error| format!("portal response was invalid: {error}"))?;
                match code {
                    0 => Ok(results),
                    1 => Err("cancelled: system portal request was cancelled".into()),
                    _ => Err("denied: system portal rejected the request".into()),
                }
            }
            () = cancel.cancelled() => {
                let _ = Self::close_request(connection, expected.as_str()).await;
                Err("cancelled: portal request cancelled by the user".into())
            }
            () = tokio::time::sleep(REQUEST_TIMEOUT) => {
                let _ = Self::close_request(connection, expected.as_str()).await;
                Err("portal request timed out after 120 seconds".into())
            }
        }
    }

    async fn close_request(connection: &Connection, path: &str) -> Result<(), String> {
        let proxy = Proxy::new(connection, DESKTOP_DESTINATION, path, REQUEST)
            .await
            .map_err(|error| format!("portal request close proxy failed: {error}"))?;
        proxy
            .call::<_, _, ()>("Close", &())
            .await
            .map_err(|error| format!("portal request close failed: {error}"))
    }

    fn session_path(results: &mut HashMap<String, OwnedValue>) -> Result<String, String> {
        let value = results
            .remove("session_handle")
            .ok_or_else(|| "portal response omitted the session handle".to_owned())?;
        let path = OwnedObjectPath::try_from(value)
            .map_err(|error| format!("portal session handle was invalid: {error}"))?;
        Ok(path.to_string())
    }

    fn streams(results: &mut HashMap<String, OwnedValue>) -> Result<Vec<PortalStream>, String> {
        let value = results
            .remove("streams")
            .ok_or_else(|| "portal response omitted selected streams".to_owned())?;
        let streams = Vec::<(u32, HashMap<String, OwnedValue>)>::try_from(value)
            .map_err(|error| format!("portal stream metadata was invalid: {error}"))?;
        Ok(streams
            .into_iter()
            .map(|(node_id, mut properties)| PortalStream {
                node_id,
                position: properties
                    .remove("position")
                    .and_then(|value| <(i32, i32)>::try_from(value).ok()),
                size: properties
                    .remove("size")
                    .and_then(|value| <(i32, i32)>::try_from(value).ok()),
            })
            .collect())
    }
}

#[async_trait]
impl PortalBoundary for LivePortalBoundary {
    async fn probe(&self) -> Result<PortalInterfaces, String> {
        let connection = self.connection().await?;
        let screencast_version = Self::interface_version(&connection, SCREENCAST).await;
        let remote_desktop_version = Self::interface_version(&connection, REMOTE_DESKTOP).await;
        Ok(PortalInterfaces {
            screencast_version,
            remote_desktop_version,
            available_source_types: Self::screencast_property(&connection, "AvailableSourceTypes")
                .await,
            available_cursor_modes: Self::screencast_property(&connection, "AvailableCursorModes")
                .await,
        })
    }

    async fn create_session(
        &self,
        kind: PortalSessionKind,
        cancel: &CancellationSignal,
    ) -> Result<String, String> {
        let connection = self.connection().await?;
        let handle_token = Self::token("request");
        let session_token = Self::token("session");
        let options = HashMap::from([
            ("handle_token", Value::new(handle_token.as_str())),
            ("session_handle_token", Value::new(session_token.as_str())),
        ]);
        let mut results = Self::request(
            &connection,
            kind.interface(),
            "CreateSession",
            &handle_token,
            &(options,),
            cancel,
        )
        .await?;
        Self::session_path(&mut results)
    }

    async fn select(
        &self,
        kind: PortalSessionKind,
        session: &str,
        cancel: &CancellationSignal,
    ) -> Result<(), String> {
        let connection = self.connection().await?;
        let session = OwnedObjectPath::try_from(session)
            .map_err(|error| format!("portal session path is invalid: {error}"))?;
        if kind == PortalSessionKind::RemoteDesktop {
            let handle_token = Self::token("request");
            let available_devices =
                Self::remote_desktop_property(&connection, "AvailableDeviceTypes").await;
            let pointer = available_devices & 2;
            if pointer == 0 {
                return Err("RemoteDesktop portal does not expose pointer control".into());
            }
            let options = HashMap::from([
                ("handle_token", Value::new(handle_token.as_str())),
                ("types", Value::new(pointer)),
            ]);
            Self::request(
                &connection,
                REMOTE_DESKTOP,
                "SelectDevices",
                &handle_token,
                &(session.clone(), options),
                cancel,
            )
            .await?;
        }
        let handle_token = Self::token("request");
        let available_sources =
            Self::screencast_property(&connection, "AvailableSourceTypes").await & 3;
        if available_sources == 0 {
            return Err("ScreenCast portal exposes no monitor or window sources".into());
        }
        let cursor_modes = Self::screencast_property(&connection, "AvailableCursorModes").await;
        let cursor_mode = if cursor_modes & 2 != 0 { 2_u32 } else { 1_u32 };
        let options = HashMap::from([
            ("handle_token", Value::new(handle_token.as_str())),
            ("types", Value::new(available_sources)),
            ("multiple", Value::new(false)),
            ("cursor_mode", Value::new(cursor_mode)),
        ]);
        Self::request(
            &connection,
            SCREENCAST,
            "SelectSources",
            &handle_token,
            &(session, options),
            cancel,
        )
        .await?;
        Ok(())
    }

    async fn start(
        &self,
        kind: PortalSessionKind,
        session: &str,
        parent_window: &str,
        cancel: &CancellationSignal,
    ) -> Result<Vec<PortalStream>, String> {
        let connection = self.connection().await?;
        let session = OwnedObjectPath::try_from(session)
            .map_err(|error| format!("portal session path is invalid: {error}"))?;
        let handle_token = Self::token("request");
        let options = HashMap::from([("handle_token", Value::new(handle_token.as_str()))]);
        let mut results = Self::request(
            &connection,
            kind.interface(),
            "Start",
            &handle_token,
            &(session, parent_window, options),
            cancel,
        )
        .await?;
        Self::streams(&mut results)
    }

    async fn close_session(&self, session: &str) -> Result<(), String> {
        let connection = self.connection().await?;
        let proxy = Proxy::new(&connection, DESKTOP_DESTINATION, session, SESSION)
            .await
            .map_err(|error| format!("portal session close proxy failed: {error}"))?;
        proxy
            .call::<_, _, ()>("Close", &())
            .await
            .map_err(|error| format!("portal session close failed: {error}"))
    }

    async fn notify_pointer_axis(
        &self,
        session: &str,
        delta_x: f64,
        delta_y: f64,
    ) -> Result<(), String> {
        let connection = self.connection().await?;
        let session = OwnedObjectPath::try_from(session)
            .map_err(|error| format!("portal session path is invalid: {error}"))?;
        let proxy = Proxy::new(
            &connection,
            DESKTOP_DESTINATION,
            DESKTOP_PATH,
            REMOTE_DESKTOP,
        )
        .await
        .map_err(|error| format!("RemoteDesktop portal is unavailable: {error}"))?;
        let options: HashMap<&str, Value<'_>> = HashMap::new();
        proxy
            .call::<_, _, ()>("NotifyPointerAxis", &(session, options, delta_x, delta_y))
            .await
            .map_err(|error| format!("portal pointer-axis action failed: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct MockPortal {
        remote_available: bool,
        block_select: AtomicBool,
        select_started: Notify,
        closed: StdMutex<Vec<String>>,
        calls: StdMutex<Vec<&'static str>>,
    }

    impl MockPortal {
        fn record(&self, call: &'static str) {
            self.calls.lock().expect("calls").push(call);
        }
    }

    #[async_trait]
    impl PortalBoundary for MockPortal {
        async fn probe(&self) -> Result<PortalInterfaces, String> {
            self.record("probe");
            Ok(PortalInterfaces {
                screencast_version: Some(5),
                remote_desktop_version: self.remote_available.then_some(2),
                available_source_types: 3,
                available_cursor_modes: 3,
            })
        }

        async fn create_session(
            &self,
            _: PortalSessionKind,
            _: &CancellationSignal,
        ) -> Result<String, String> {
            self.record("create");
            Ok("/org/freedesktop/portal/desktop/session/mock/1".into())
        }

        async fn select(
            &self,
            _: PortalSessionKind,
            _: &str,
            cancel: &CancellationSignal,
        ) -> Result<(), String> {
            self.record("select");
            self.select_started.notify_one();
            if self.block_select.load(Ordering::SeqCst) {
                cancel.cancelled().await;
                return Err("cancelled: mock selection".into());
            }
            Ok(())
        }

        async fn start(
            &self,
            _: PortalSessionKind,
            _: &str,
            _: &str,
            _: &CancellationSignal,
        ) -> Result<Vec<PortalStream>, String> {
            self.record("start");
            Ok(vec![PortalStream {
                node_id: 42,
                position: Some((10, 20)),
                size: Some((1920, 1080)),
            }])
        }

        async fn close_session(&self, session: &str) -> Result<(), String> {
            self.record("close");
            self.closed.lock().expect("closed").push(session.into());
            Ok(())
        }

        async fn notify_pointer_axis(&self, _: &str, _: f64, _: f64) -> Result<(), String> {
            self.record("axis");
            Ok(())
        }
    }

    #[tokio::test]
    async fn screencast_lifecycle_requires_consent_and_closes_session() {
        let boundary = Arc::new(MockPortal::default());
        let manager = WaylandPortalManager::new(boundary.clone());
        let probed = manager.probe().await;
        assert_eq!(probed.consent, PortalConsentState::Required);
        let active = manager.connect(false, "").await.unwrap();
        assert_eq!(active.phase, PortalSessionPhase::Active);
        assert_eq!(active.consent, PortalConsentState::Granted);
        assert_eq!(active.streams[0].node_id, 42);
        assert!(!active.pipewire_transport);
        let idle = manager.disconnect().await;
        assert_eq!(idle.phase, PortalSessionPhase::Idle);
        assert_eq!(boundary.closed.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unavailable_remote_desktop_fails_before_session_creation() {
        let boundary = Arc::new(MockPortal::default());
        let manager = WaylandPortalManager::new(boundary.clone());
        assert!(manager.connect(true, "").await.is_err());
        assert_eq!(manager.status().consent, PortalConsentState::Unavailable);
        assert!(!boundary.calls.lock().unwrap().contains(&"create"));
    }

    #[tokio::test]
    async fn cancellation_interrupts_selection_and_cleans_up_session() {
        let boundary = Arc::new(MockPortal {
            block_select: AtomicBool::new(true),
            ..MockPortal::default()
        });
        let manager = Arc::new(WaylandPortalManager::new(boundary.clone()));
        let task = tokio::spawn({
            let manager = manager.clone();
            async move { manager.connect(false, "").await }
        });
        boundary.select_started.notified().await;
        manager.cancel().await;
        assert!(task.await.unwrap().is_err());
        assert_eq!(manager.status().phase, PortalSessionPhase::Cancelled);
        assert_eq!(boundary.closed.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn remote_axis_requires_a_user_granted_remote_session() {
        let boundary = Arc::new(MockPortal {
            remote_available: true,
            ..MockPortal::default()
        });
        let manager = WaylandPortalManager::new(boundary.clone());
        assert!(manager.notify_pointer_axis(0.0, 1.0).await.is_err());
        manager.connect(true, "").await.unwrap();
        manager.notify_pointer_axis(0.0, 1.0).await.unwrap();
        assert!(boundary.calls.lock().unwrap().contains(&"axis"));
    }

    #[tokio::test]
    async fn live_probe_when_explicitly_requested() {
        if std::env::var("PERSONAL_AGENT_PORTAL_LIVE_TEST").as_deref() != Ok("1") {
            return;
        }
        let interfaces = LivePortalBoundary::default()
            .probe()
            .await
            .expect("live portal probe");
        assert!(interfaces.screencast_version.is_some());
        assert_ne!(interfaces.available_source_types, 0);
    }
}
