//! Versioned domain and IPC contracts.

use chrono::{DateTime, Utc};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

/// Types generated from the versioned protobuf source.
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/personal_agent.v1.rs"));
}

/// Current event-envelope schema version.
pub const EVENT_SCHEMA_VERSION: u32 = 1;

/// Failure to construct or inspect a domain event.
#[derive(Debug, Error)]
pub enum EventError {
    /// The payload could not be encoded or decoded as JSON.
    #[error("invalid event payload: {0}")]
    Json(#[from] serde_json::Error),
    /// A timestamp is not RFC 3339.
    #[error("invalid event timestamp: {0}")]
    Timestamp(#[from] chrono::ParseError),
}

impl proto::EventEnvelope {
    /// Construct an event with stable metadata and a JSON payload.
    ///
    /// # Errors
    ///
    /// Returns an error when the JSON payload cannot be encoded.
    pub fn new(
        sequence: u64,
        origin: impl Into<String>,
        profile_id: impl Into<String>,
        event_type: impl Into<String>,
        payload: &Value,
    ) -> Result<Self, EventError> {
        Ok(Self {
            schema_version: EVENT_SCHEMA_VERSION,
            event_id: Uuid::now_v7().to_string(),
            wall_clock_timestamp: Utc::now().to_rfc3339(),
            monotonic_sequence: sequence,
            origin: origin.into(),
            profile_id: profile_id.into(),
            session_id: None,
            goal_id: None,
            task_id: None,
            agent_id: None,
            r#type: event_type.into(),
            payload_json: serde_json::to_vec(payload)?,
        })
    }

    /// Decode the untrusted payload without interpreting the event type.
    ///
    /// # Errors
    ///
    /// Returns an error when stored payload bytes are not valid JSON.
    pub fn payload(&self) -> Result<Value, EventError> {
        Ok(serde_json::from_slice(&self.payload_json)?)
    }

    /// Parse the wall-clock timestamp for ordering and display.
    ///
    /// # Errors
    ///
    /// Returns an error when the timestamp is not RFC 3339.
    pub fn timestamp(&self) -> Result<DateTime<Utc>, EventError> {
        Ok(DateTime::parse_from_rfc3339(&self.wall_clock_timestamp)?.with_timezone(&Utc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_round_trips_json_and_metadata() {
        let event =
            proto::EventEnvelope::new(7, "ui", "default", "chat.user", &json!({"text":"hi"}))
                .expect("event");
        assert_eq!(event.schema_version, EVENT_SCHEMA_VERSION);
        assert_eq!(event.monotonic_sequence, 7);
        assert_eq!(event.payload().expect("payload"), json!({"text":"hi"}));
        assert!(event.timestamp().is_ok());
    }
}
