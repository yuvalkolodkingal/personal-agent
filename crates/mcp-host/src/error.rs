//! Sanitized adapter failures.
//!
//! Transport and process text is untrusted and may quote credentials or local
//! paths, so it is written to the per-server log ring and to `tracing` while the
//! value returned to the manager carries only a stable code and a fixed,
//! user-facing message.

use personal_agent_mcp_manager::AdapterError;
use rmcp::service::ServiceError;

use crate::logs::LogLevel;
use crate::session::{SharedLog, record};

/// Builds a sanitized adapter failure.
#[must_use]
pub(crate) fn adapter_error(code: &str, message: &str) -> AdapterError {
    AdapterError {
        code: code.into(),
        message: message.into(),
        authentication_required: false,
    }
}

/// Builds a failure that asks the GUI to start a sign-in flow.
#[must_use]
pub(crate) fn authentication_error(message: &str) -> AdapterError {
    AdapterError {
        code: "authentication_required".into(),
        message: message.into(),
        authentication_required: true,
    }
}

/// Maps an `rmcp` initialization failure, keeping the detail in the log ring.
pub(crate) fn initialize_error(
    log: &SharedLog,
    error: &rmcp::service::ClientInitializeError,
) -> AdapterError {
    let detail = error.to_string();
    if detail.to_ascii_lowercase().contains("unauthorized")
        || detail.to_ascii_lowercase().contains("auth required")
    {
        record(log, LogLevel::Error, format!("initialize: {detail}"));
        return authentication_error("This MCP server requires sign-in before it will connect.");
    }
    record(log, LogLevel::Error, format!("initialize: {detail}"));
    adapter_error(
        "initialize_failed",
        "The MCP server did not complete initialization.",
    )
}

/// Maps a post-initialization request failure.
#[must_use]
pub(crate) fn service_error(error: ServiceError) -> AdapterError {
    match error {
        ServiceError::McpError(data) => AdapterError {
            code: "server_error".into(),
            message: data.message.to_string(),
            authentication_required: false,
        },
        ServiceError::Timeout { .. } => {
            adapter_error("timeout", "The MCP server did not answer in time.")
        }
        ServiceError::Cancelled { .. } => {
            adapter_error("cancelled", "The MCP request was cancelled.")
        }
        other => {
            tracing::warn!(error = %other, "MCP request failed");
            adapter_error("transport_failed", "The MCP session is no longer usable.")
        }
    }
}

/// Records a transport-level failure and returns the sanitized value.
pub(crate) fn transport_error(log: &SharedLog, detail: &str) -> AdapterError {
    record(log, LogLevel::Error, detail.to_owned());
    adapter_error("transport_failed", "The MCP session is no longer usable.")
}

/// Marker used when a caller asks for a server that is not connected.
#[must_use]
pub(crate) fn not_connected() -> AdapterError {
    adapter_error("not_connected", "This MCP server is not connected.")
}
