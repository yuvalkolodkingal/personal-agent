//! SQLCipher-backed append-only event store and transactional schema migrations.

use personal_agent_contracts::proto::EventEnvelope;
use prost::Message;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 4;

/// Storage failure with enough context for diagnostics and recovery.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("stored event is not valid protobuf: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("stored migration payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("SQLCipher support is unavailable in this build")]
    SqlCipherUnavailable,
    #[error("event schema version {0} is not supported")]
    UnsupportedEventSchema(u32),
    #[error("event sequence is outside SQLite's signed integer range: {0}")]
    SequenceOutOfRange(u64),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("export or backup destination already exists: {0}")]
    DestinationExists(PathBuf),
    #[error("content-addressed blob does not exist: {0}")]
    BlobMissing(String),
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
             CREATE TABLE IF NOT EXISTS blobs(content_hash TEXT PRIMARY KEY, byte_length INTEGER NOT NULL, body BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS connectors(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, state TEXT NOT NULL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS provider_usage(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, amount_usd REAL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS egress(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, destination TEXT NOT NULL, size_bytes INTEGER NOT NULL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS settings(profile_id TEXT NOT NULL, key TEXT NOT NULL, value_json TEXT NOT NULL, PRIMARY KEY(profile_id, key));
             CREATE TABLE IF NOT EXISTS migration_runs(id TEXT PRIMARY KEY, source_fingerprint TEXT NOT NULL, state TEXT NOT NULL, body_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS migration_items(
               id TEXT PRIMARY KEY,
               kind TEXT NOT NULL,
               source_locator TEXT NOT NULL,
               source_modified_at TEXT,
               content_sha256 TEXT NOT NULL,
               destination TEXT NOT NULL,
               enabled INTEGER NOT NULL,
               contains_personal_data INTEGER NOT NULL,
               payload BLOB NOT NULL,
               imported_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS runtime_snapshots(
               kind TEXT NOT NULL,
               id TEXT NOT NULL,
               body_json TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               PRIMARY KEY(kind,id)
             );"
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

    /// Persist a content-free migration run report inside the encrypted store.
    ///
    /// # Errors
    ///
    /// Returns a serialization or database transaction error.
    pub fn record_migration_report(
        &mut self,
        report: &personal_agent_migration::MigrationReport,
    ) -> Result<(), StorageError> {
        let body = report.to_json_pretty()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO migration_runs(id,source_fingerprint,state,body_json) VALUES (?1,?2,'completed',?3)",
            params![report.run_id, report.source_fingerprint, body],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Store bytes once under their SHA-256 address inside `SQLCipher`.
    ///
    /// # Errors
    ///
    /// Returns a database error or a length-conversion error.
    pub fn store_blob(&mut self, bytes: &[u8]) -> Result<String, StorageError> {
        let hash = hex(&Sha256::digest(bytes));
        let length = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO blobs(content_hash,byte_length,body) VALUES (?1,?2,?3)",
            params![hash, length, bytes],
        )?;
        tx.commit()?;
        Ok(hash)
    }

    /// Retrieve an exact content-addressed blob.
    ///
    /// # Errors
    ///
    /// Returns `BlobMissing` or a database error.
    pub fn blob(&self, hash: &str) -> Result<Vec<u8>, StorageError> {
        self.connection
            .query_row(
                "SELECT body FROM blobs WHERE content_hash = ?1",
                [hash],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StorageError::BlobMissing(hash.into()))
    }

    /// Create a consistent encrypted database backup before update/migration.
    /// The destination must not already exist.
    ///
    /// # Errors
    ///
    /// Returns destination, I/O, `SQLCipher`, or backup errors.
    pub fn backup_to(&self, path: &Path, key: &SecretString) -> Result<(), StorageError> {
        reject_existing_destination(path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut destination = Connection::open(path)?;
        Self::configure(&destination, key, true)?;
        {
            let backup = rusqlite::backup::Backup::new(&self.connection, &mut destination)?;
            backup.run_to_completion(128, Duration::from_millis(1), None)?;
        }
        destination.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
        Ok(())
    }

    /// Export all event envelopes to a private, atomic JSON file. This is an
    /// explicit user-data export and therefore includes event payloads.
    ///
    /// # Errors
    ///
    /// Returns destination, decode, serialization, or I/O errors.
    pub fn export_events_json(&self, path: &Path) -> Result<(), StorageError> {
        reject_existing_destination(path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let events = self.after(0, usize::MAX)?;
        let events = events
            .into_iter()
            .map(|event| {
                Ok(serde_json::json!({
                    "schema_version": event.schema_version,
                    "event_id": event.event_id,
                    "wall_clock_timestamp": event.wall_clock_timestamp,
                    "monotonic_sequence": event.monotonic_sequence,
                    "origin": event.origin,
                    "profile_id": event.profile_id,
                    "session_id": event.session_id,
                    "goal_id": event.goal_id,
                    "task_id": event.task_id,
                    "agent_id": event.agent_id,
                    "type": event.r#type,
                    "payload": serde_json::from_slice::<serde_json::Value>(&event.payload_json)?,
                }))
            })
            .collect::<Result<Vec<_>, serde_json::Error>>()?;
        let body = serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "product": "Personal Agent",
            "events": events,
        }))?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("export");
        let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        if let Err(error) = (|| -> Result<(), std::io::Error> {
            file.write_all(&body)?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            Ok(())
        })() {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        Ok(())
    }

    /// Atomically persist a durable task supervisor snapshot.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn save_supervisor_snapshot(
        &mut self,
        snapshot: &personal_agent_agent::SupervisorSnapshot,
    ) -> Result<(), StorageError> {
        save_snapshot(
            &mut self.connection,
            "agent-supervisor",
            &snapshot.graph.goal_id.to_string(),
            snapshot,
        )
    }

    /// Load a task supervisor snapshot; absence is `Ok(None)`.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn supervisor_snapshot(
        &self,
        goal_id: uuid::Uuid,
    ) -> Result<Option<personal_agent_agent::SupervisorSnapshot>, StorageError> {
        load_snapshot(&self.connection, "agent-supervisor", &goal_id.to_string())
    }

    /// Atomically persist the complete scheduler snapshot.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn save_scheduler_snapshot(
        &mut self,
        profile_id: &str,
        snapshot: &personal_agent_automation::SchedulerSnapshot,
    ) -> Result<(), StorageError> {
        save_snapshot(
            &mut self.connection,
            "automation-scheduler",
            profile_id,
            snapshot,
        )
    }

    /// Load the scheduler snapshot for a profile; absence is `Ok(None)`.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn scheduler_snapshot(
        &self,
        profile_id: &str,
    ) -> Result<Option<personal_agent_automation::SchedulerSnapshot>, StorageError> {
        load_snapshot(&self.connection, "automation-scheduler", profile_id)
    }

    /// Atomically persist the provenance-first memory index and vectors.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn save_memory_snapshot(
        &mut self,
        profile_id: &str,
        snapshot: &personal_agent_memory::MemoryStore,
    ) -> Result<(), StorageError> {
        save_snapshot(&mut self.connection, "memory-index", profile_id, snapshot)
    }

    /// Load the memory snapshot for a profile; absence is `Ok(None)`.
    ///
    /// # Errors
    /// Returns JSON or database errors.
    pub fn memory_snapshot(
        &self,
        profile_id: &str,
    ) -> Result<Option<personal_agent_memory::MemoryStore>, StorageError> {
        load_snapshot(&self.connection, "memory-index", profile_id)
    }
}

fn save_snapshot<T: serde::Serialize>(
    connection: &mut Connection,
    kind: &str,
    id: &str,
    snapshot: &T,
) -> Result<(), StorageError> {
    let body = serde_json::to_string(snapshot)?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO runtime_snapshots(kind,id,body_json,updated_at)
         VALUES (?1,?2,?3,strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         ON CONFLICT(kind,id) DO UPDATE SET body_json=excluded.body_json, updated_at=excluded.updated_at",
        params![kind, id, body],
    )?;
    tx.commit()?;
    Ok(())
}

fn load_snapshot<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    kind: &str,
    id: &str,
) -> Result<Option<T>, StorageError> {
    let body: Option<String> = connection
        .query_row(
            "SELECT body_json FROM runtime_snapshots WHERE kind=?1 AND id=?2",
            params![kind, id],
            |row| row.get(0),
        )
        .optional()?;
    body.map(|body| serde_json::from_str(&body))
        .transpose()
        .map_err(Into::into)
}

fn reject_existing_destination(path: &Path) -> Result<(), StorageError> {
    if path.exists() {
        Err(StorageError::DestinationExists(path.into()))
    } else {
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

impl personal_agent_migration::MigrationSink for EventStore {
    type Error = StorageError;

    fn contains(&mut self, record_id: &str) -> Result<bool, Self::Error> {
        let present = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM migration_items WHERE id = ?1)",
            [record_id],
            |row| row.get::<_, bool>(0),
        )?;
        Ok(present)
    }

    fn store(
        &mut self,
        record: &personal_agent_migration::PreparedRecord,
    ) -> Result<(), Self::Error> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO migration_items(
               id,kind,source_locator,source_modified_at,content_sha256,destination,
               enabled,contains_personal_data,payload,imported_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            params![
                record.id,
                record.kind,
                record.source_locator,
                record.source_modified_at,
                record.content_sha256,
                record.destination,
                record.enabled,
                record.contains_personal_data,
                record.payload(),
            ],
        )?;
        materialize_migration_record(&tx, record)?;
        tx.commit()?;
        Ok(())
    }
}

fn materialize_migration_record(
    tx: &Transaction<'_>,
    record: &personal_agent_migration::PreparedRecord,
) -> Result<(), StorageError> {
    let payload = String::from_utf8_lossy(record.payload());
    match record.kind.as_str() {
        "history-event" => {
            let last: i64 = tx.query_row(
                "SELECT COALESCE(MAX(monotonic_sequence), 0) FROM events",
                [],
                |row| row.get(0),
            )?;
            let sequence = last
                .checked_add(1)
                .ok_or(StorageError::SequenceOutOfRange(u64::MAX))?;
            let timestamp = record
                .source_modified_at
                .clone()
                .unwrap_or_else(|| "1970-01-01T00:00:00+00:00".to_owned());
            let event = EventEnvelope {
                schema_version: personal_agent_contracts::EVENT_SCHEMA_VERSION,
                event_id: record.id.clone(),
                wall_clock_timestamp: timestamp.clone(),
                monotonic_sequence: u64::try_from(sequence)
                    .map_err(|_| StorageError::SequenceOutOfRange(u64::MAX))?,
                origin: "legacy-migration".to_owned(),
                profile_id: "default".to_owned(),
                session_id: None,
                goal_id: None,
                task_id: None,
                agent_id: None,
                r#type: "conversation.legacy-imported".to_owned(),
                payload_json: record.payload().to_vec(),
            };
            tx.execute(
                "INSERT INTO events(monotonic_sequence,event_id,profile_id,event_type,wall_clock_timestamp,envelope) VALUES (?1,?2,?3,?4,?5,?6)",
                params![sequence, event.event_id, event.profile_id, event.r#type, timestamp, event.encode_to_vec()],
            )?;
        }
        "memory" => {
            let body = serde_json::to_string(&serde_json::json!({
                "origin": "legacy-migration",
                "source_locator": record.source_locator,
                "source_modified_at": record.source_modified_at,
                "content_sha256": record.content_sha256,
                "markdown": payload,
            }))?;
            tx.execute(
                "INSERT INTO memories(id,profile_id,trust,body_json) VALUES (?1,'default','legacy-imported',?2)",
                params![record.id, body],
            )?;
        }
        "automation" => {
            tx.execute(
                "INSERT INTO automations(id,profile_id,state,body_json) VALUES (?1,'default','disabled',?2)",
                params![record.id, payload.as_ref()],
            )?;
        }
        "connector-metadata" => {
            tx.execute(
                "INSERT INTO connectors(id,profile_id,state,body_json) VALUES (?1,'default','disabled',?2)",
                params![record.id, payload.as_ref()],
            )?;
        }
        "settings" | "conversation-state" | "projects" | "remote-device-metadata" => {
            tx.execute(
                "INSERT INTO settings(profile_id,key,value_json) VALUES ('default',?1,?2)",
                params![
                    format!("migration:{}:{}", record.destination, record.id),
                    payload.as_ref()
                ],
            )?;
        }
        "skill-file" | "expert-file" | "theme" => {
            let body = serde_json::to_string(&serde_json::json!({
                "origin": "legacy-migration",
                "migration_item_id": record.id,
                "source_locator": record.source_locator,
                "destination": record.destination,
                "enabled": false,
            }))?;
            tx.execute(
                "INSERT INTO artifacts(id,content_hash,body_json) VALUES (?1,?2,?3)",
                params![record.id, record.content_sha256, body],
            )?;
        }
        _ => {}
    }
    Ok(())
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

    #[test]
    fn blobs_deduplicate_and_backup_reopens_with_the_same_key() {
        let temp = tempfile::tempdir().expect("temp");
        let source = temp.path().join("profile.db");
        let backup = temp.path().join("profile.backup.db");
        let key = SecretString::from("test-only-key".to_owned());
        let mut store = EventStore::open(&source, &key).expect("store");
        let event = EventEnvelope::new(1, "test", "default", "artifact.created", &json!({}))
            .expect("event");
        store.append(&event).expect("append");
        let first = store.store_blob(b"large encrypted bytes").expect("blob");
        let second = store.store_blob(b"large encrypted bytes").expect("dedup");
        assert_eq!(first, second);
        assert_eq!(store.blob(&first).expect("read"), b"large encrypted bytes");
        store.backup_to(&backup, &key).expect("backup");
        let reopened = EventStore::open(&backup, &key).expect("reopen backup");
        assert_eq!(reopened.last_sequence().expect("sequence"), 1);
        assert_eq!(
            reopened.blob(&first).expect("backup blob"),
            b"large encrypted bytes"
        );
    }

    #[test]
    fn event_export_is_private_atomic_and_never_overwrites() {
        let temp = tempfile::tempdir().expect("temp");
        let export = temp.path().join("profile-export.json");
        let mut store = EventStore::open_in_memory(&SecretString::from("test-only-key".to_owned()))
            .expect("store");
        store
            .append(
                &EventEnvelope::new(
                    1,
                    "test",
                    "default",
                    "message.user",
                    &json!({"text":"owned data"}),
                )
                .expect("event"),
            )
            .expect("append");
        store.export_events_json(&export).expect("export");
        let body = fs::read_to_string(&export).expect("read");
        assert!(body.contains("owned data"));
        assert!(matches!(
            store.export_events_json(&export),
            Err(StorageError::DestinationExists(_))
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&export).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn durable_runtime_snapshots_survive_database_restart() {
        use personal_agent_agent::{DurableSupervisor, ExecutionZone, Task, TaskGraph, WorkStatus};
        use std::collections::{BTreeMap, BTreeSet};

        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("durable.db");
        let key = SecretString::from("test-only-key".to_owned());
        let goal_id = Uuid::now_v7();
        let task = Task {
            id: Uuid::now_v7(),
            goal_id,
            parent_task_id: None,
            title: "durable task".into(),
            assigned_agent: "executor".into(),
            workspace: None,
            browser_profile: None,
            tool_scopes: BTreeSet::new(),
            risk: "read".into(),
            execution_zone: ExecutionZone::Isolated,
            max_attempts: 3,
            attempt: 0,
            idempotency_key: None,
            checkpoint_id: None,
            status: WorkStatus::Queued,
            progress: 0,
            output: None,
        };
        let graph = TaskGraph {
            goal_id,
            revision: 1,
            tasks: BTreeMap::from([(task.id, task)]),
            edges: vec![],
        };
        let supervisor = DurableSupervisor::new(graph, 3, 3).expect("supervisor");
        {
            let mut store = EventStore::open(&path, &key).expect("store");
            store
                .save_supervisor_snapshot(supervisor.snapshot())
                .expect("save agent");
            store
                .save_scheduler_snapshot(
                    "default",
                    &personal_agent_automation::SchedulerSnapshot::default(),
                )
                .expect("save scheduler");
            store
                .save_memory_snapshot("default", &personal_agent_memory::MemoryStore::default())
                .expect("save memory");
        }
        let store = EventStore::open(&path, &key).expect("reopen");
        let agent = store
            .supervisor_snapshot(goal_id)
            .expect("load agent")
            .expect("agent exists");
        assert_eq!(agent.graph.goal_id, goal_id);
        assert_eq!(
            store
                .scheduler_snapshot("default")
                .expect("scheduler")
                .expect("scheduler exists"),
            personal_agent_automation::SchedulerSnapshot::default()
        );
        assert!(
            store
                .memory_snapshot("default")
                .expect("memory")
                .expect("memory exists")
                .export()
                .is_empty()
        );
    }

    #[test]
    fn migration_sink_is_encrypted_idempotent_and_excludes_secret_payloads() {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/legacy/synthetic-v1");
        let plan = personal_agent_migration::discover(&fixture).expect("plan");
        let consent = personal_agent_migration::MigrationConsent {
            copy_personal_data: true,
            adopt_opencode_auth: false,
        };
        let mut store = EventStore::open_in_memory(&SecretString::from("test-only-key".to_owned()))
            .expect("store");

        let first =
            personal_agent_migration::migrate(&plan, consent, &mut store).expect("first migration");
        store
            .record_migration_report(&first)
            .expect("migration report");
        let second = personal_agent_migration::migrate(&plan, consent, &mut store)
            .expect("second migration");

        assert!(first.summary.imported > 5);
        assert_eq!(second.summary.imported, 0);
        assert_eq!(second.summary.already_present, first.summary.imported);
        let leaked: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM migration_items WHERE instr(CAST(payload AS TEXT), 'fixture-provider-value-must-never-migrate') > 0",
                [],
                |row| row.get(0),
            )
            .expect("secret check");
        assert_eq!(leaked, 0);
        let history_events: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE event_type = 'conversation.legacy-imported'",
                [],
                |row| row.get(0),
            )
            .expect("history events");
        let memories: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE trust = 'legacy-imported'",
                [],
                |row| row.get(0),
            )
            .expect("memories");
        let disabled_automations: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM automations WHERE state = 'disabled'",
                [],
                |row| row.get(0),
            )
            .expect("automations");
        assert_eq!(history_events, 2);
        assert_eq!(memories, 1);
        assert_eq!(disabled_automations, 1);
        let migration_runs: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM migration_runs", [], |row| row.get(0))
            .expect("migration runs");
        assert_eq!(migration_runs, 1);
    }
}
