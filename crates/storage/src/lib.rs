//! SQLCipher-backed append-only event store and transactional schema migrations.

use personal_agent_contracts::proto::EventEnvelope;
use prost::Message;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use secrecy::{ExposeSecret, SecretString};
use std::path::Path;
use thiserror::Error;

const SCHEMA_VERSION: i64 = 1;

/// Storage failure with enough context for diagnostics and recovery.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("stored event is not valid protobuf: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("SQLCipher support is unavailable in this build")]
    SqlCipherUnavailable,
    #[error("event schema version {0} is not supported")]
    UnsupportedEventSchema(u32),
    #[error("event sequence is outside SQLite's signed integer range: {0}")]
    SequenceOutOfRange(u64),
}

/// Owned encrypted store. Database access never crosses the native-core boundary.
pub struct EventStore {
    connection: Connection,
}

impl EventStore {
    /// Open, key, verify, and migrate a database atomically.
    ///
    /// # Errors
    ///
    /// Returns a database, cipher-availability, or migration error.
    pub fn open(path: &Path, key: &SecretString) -> Result<Self, StorageError> {
        let connection = Connection::open(path)?;
        Self::configure(&connection, key, !path.as_os_str().is_empty())?;
        Self::migrate(&connection)?;
        Ok(Self { connection })
    }

    /// In-memory encrypted store for tests and ephemeral guest sessions.
    ///
    /// # Errors
    ///
    /// Returns a database, cipher-availability, or migration error.
    pub fn open_in_memory(key: &SecretString) -> Result<Self, StorageError> {
        let connection = Connection::open_in_memory()?;
        Self::configure(&connection, key, false)?;
        Self::migrate(&connection)?;
        Ok(Self { connection })
    }

    fn configure(
        connection: &Connection,
        key: &SecretString,
        file_backed: bool,
    ) -> Result<(), StorageError> {
        connection.pragma_update(None, "key", key.expose_secret())?;
        let cipher: Option<String> = connection
            .query_row("PRAGMA cipher_version", [], |row| row.get(0))
            .optional()?;
        if cipher.as_deref().is_none_or(str::is_empty) {
            return Err(StorageError::SqlCipherUnavailable);
        }
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "busy_timeout", 5000_i64)?;
        if file_backed {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        Ok(())
    }

    fn migrate(connection: &Connection) -> Result<(), StorageError> {
        let tx = connection.unchecked_transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS events(
               monotonic_sequence INTEGER PRIMARY KEY,
               event_id TEXT NOT NULL UNIQUE,
               profile_id TEXT NOT NULL,
               event_type TEXT NOT NULL,
               wall_clock_timestamp TEXT NOT NULL,
               envelope BLOB NOT NULL
             );
             CREATE TABLE IF NOT EXISTS profiles(id TEXT PRIMARY KEY, created_at TEXT NOT NULL, mode TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, status TEXT NOT NULL, updated_at TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS goals(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, status TEXT NOT NULL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY, goal_id TEXT NOT NULL, status TEXT NOT NULL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS task_edges(goal_id TEXT NOT NULL, dependency_id TEXT NOT NULL, dependent_id TEXT NOT NULL, PRIMARY KEY(goal_id, dependency_id, dependent_id));
             CREATE TABLE IF NOT EXISTS agent_runs(id TEXT PRIMARY KEY, task_id TEXT NOT NULL, state TEXT NOT NULL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS tool_runs(id TEXT PRIMARY KEY, task_id TEXT, tool_id TEXT NOT NULL, state TEXT NOT NULL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS permission_requests(id TEXT PRIMARY KEY, task_id TEXT, state TEXT NOT NULL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS consent_grants(id TEXT PRIMARY KEY, goal_id TEXT NOT NULL, revoked INTEGER NOT NULL DEFAULT 0, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS checkpoints(id TEXT PRIMARY KEY, task_id TEXT NOT NULL, coverage TEXT NOT NULL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS memories(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, trust TEXT NOT NULL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS memory_links(memory_id TEXT NOT NULL, source_event_id TEXT NOT NULL, PRIMARY KEY(memory_id, source_event_id));
             CREATE TABLE IF NOT EXISTS automations(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, state TEXT NOT NULL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS automation_runs(id TEXT PRIMARY KEY, automation_id TEXT NOT NULL, state TEXT NOT NULL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS artifacts(id TEXT PRIMARY KEY, content_hash TEXT NOT NULL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS connectors(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, state TEXT NOT NULL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS provider_usage(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, amount_usd REAL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS egress(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, destination TEXT NOT NULL, size_bytes INTEGER NOT NULL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS settings(profile_id TEXT NOT NULL, key TEXT NOT NULL, value_json TEXT NOT NULL, PRIMARY KEY(profile_id, key));
             CREATE TABLE IF NOT EXISTS migration_runs(id TEXT PRIMARY KEY, source_fingerprint TEXT NOT NULL, state TEXT NOT NULL, body_json TEXT NOT NULL);"
        )?;
        tx.execute("INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ','now'))", [SCHEMA_VERSION])?;
        tx.commit()?;
        Ok(())
    }

    /// Atomically append an event. Sequence and event ID uniqueness make retries safe.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported/out-of-range events or failed storage.
    pub fn append(&mut self, event: &EventEnvelope) -> Result<(), StorageError> {
        if event.schema_version != personal_agent_contracts::EVENT_SCHEMA_VERSION {
            return Err(StorageError::UnsupportedEventSchema(event.schema_version));
        }
        let sequence = i64::try_from(event.monotonic_sequence)
            .map_err(|_| StorageError::SequenceOutOfRange(event.monotonic_sequence))?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO events(monotonic_sequence,event_id,profile_id,event_type,wall_clock_timestamp,envelope) VALUES (?1,?2,?3,?4,?5,?6)",
            params![sequence, event.event_id, event.profile_id, event.r#type, event.wall_clock_timestamp, event.encode_to_vec()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Resume a bounded event subscription after a known sequence number.
    ///
    /// # Errors
    ///
    /// Returns an error for out-of-range sequences, database reads, or corrupt
    /// protobuf envelopes.
    pub fn after(&self, sequence: u64, limit: usize) -> Result<Vec<EventEnvelope>, StorageError> {
        let sequence =
            i64::try_from(sequence).map_err(|_| StorageError::SequenceOutOfRange(sequence))?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = self.connection.prepare("SELECT envelope FROM events WHERE monotonic_sequence > ?1 ORDER BY monotonic_sequence LIMIT ?2")?;
        let blobs =
            statement.query_map(params![sequence, limit], |row| row.get::<_, Vec<u8>>(0))?;
        blobs
            .map(|blob| Ok(EventEnvelope::decode(blob?.as_slice())?))
            .collect()
    }

    /// Highest committed sequence for projection recovery.
    ///
    /// # Errors
    ///
    /// Returns a database read error.
    pub fn last_sequence(&self) -> Result<u64, StorageError> {
        let value: i64 = self.connection.query_row(
            "SELECT COALESCE(MAX(monotonic_sequence), 0) FROM events",
            [],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(value).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn append_and_resume_events() {
        let mut store = EventStore::open_in_memory(&SecretString::from("test-only-key".to_owned()))
            .expect("store");
        let first = EventEnvelope::new(1, "test", "default", "goal.created", &json!({"id":1}))
            .expect("event");
        let second = EventEnvelope::new(2, "test", "default", "task.created", &json!({"id":2}))
            .expect("event");
        store.append(&first).expect("append");
        store.append(&second).expect("append");
        let resumed = store.after(1, 10).expect("resume");
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].event_id, second.event_id);
        assert_eq!(store.last_sequence().expect("sequence"), 2);
    }

    #[test]
    fn duplicate_sequence_is_rejected() {
        let mut store = EventStore::open_in_memory(&SecretString::from("test-only-key".to_owned()))
            .expect("store");
        let event =
            EventEnvelope::new(1, "test", "default", "goal.created", &json!({})).expect("event");
        store.append(&event).expect("first");
        assert!(store.append(&event).is_err());
    }
}
