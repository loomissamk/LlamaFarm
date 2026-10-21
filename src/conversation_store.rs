//! Durable ordered conversation-event persistence for V3.
//!
//! This module is additive: existing V2 chat/session endpoints keep their wire
//! format until a later migration. Store append owns sequence allocation inside
//! a SQLite IMMEDIATE transaction; no caller may guess or reuse a sequence.
//! Repeated event IDs are idempotent only when the full original draft matches.
//! Synchronous operations are intended for blocking threads (spawn_blocking
//! from Tokio), not directly in an Axum async handler.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::Value;
use thiserror::Error;

use crate::conversation_events::{
    redact_tool_arguments, validate_replay_page, ConversationEvent, EventError, EventType,
    SCHEMA_VERSION,
};

pub const STORE_SCHEMA_VERSION: i64 = 1;
pub const MAX_REPLAY_PAGE: usize = 500;

#[derive(Debug, Clone, PartialEq)]
pub struct EventDraft {
    /// Stable idempotency identifier supplied by the event producer.
    pub event_id: String,
    pub conversation_id: String,
    pub turn_id: Option<String>,
    pub run_id: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub event_type: EventType,
    pub data: Value,
    pub trace_id: Option<String>,
    pub source_node: Option<String>,
}

impl EventDraft {
    fn with_sequence(&self, sequence: u64) -> ConversationEvent {
        ConversationEvent {
            schema_version: SCHEMA_VERSION,
            event_id: self.event_id.clone(),
            conversation_id: self.conversation_id.clone(),
            sequence,
            turn_id: self.turn_id.clone(),
            run_id: self.run_id.clone(),
            timestamp: self.timestamp.clone(),
            event_type: self.event_type,
            data: self.data.clone(),
            trace_id: self.trace_id.clone(),
            source_node: self.source_node.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppendResult {
    Inserted(ConversationEvent),
    AlreadyPresent(ConversationEvent),
}

impl AppendResult {
    pub fn event(&self) -> &ConversationEvent {
        match self {
            Self::Inserted(event) | Self::AlreadyPresent(event) => event,
        }
    }
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("SQLite error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("event contract error: {0}")]
    Event(#[from] EventError),
    #[error("unsupported event-store database version {0}")]
    DatabaseVersion(i64),
    #[error("event ID already exists with different content: {0}")]
    EventIdConflict(String),
    #[error("conversation has exhausted its sequence range")]
    SequenceOverflow,
    #[error("replay cursor exceeds the database integer range")]
    InvalidCursor,
    #[error("replay page limit must be between 1 and {MAX_REPLAY_PAGE}")]
    InvalidLimit,
}

/// Thread-safe handle: every operation opens a short-lived SQLite connection.
/// No Rust mutex serializes writers; SQLite's IMMEDIATE transaction does.
#[derive(Debug, Clone)]
pub struct ConversationEventStore {
    path: PathBuf,
}

impl ConversationEventStore {
    /// Create or validate a durable event-store database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let store = Self { path };
        let conn = store.connect()?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => {
                conn.execute_batch(
                    "BEGIN IMMEDIATE;
                     CREATE TABLE IF NOT EXISTS conversation_events (
                       conversation_id TEXT NOT NULL,
                       sequence INTEGER NOT NULL CHECK (sequence > 0),
                       event_id TEXT NOT NULL UNIQUE,
                       event_json TEXT NOT NULL,
                       PRIMARY KEY (conversation_id, sequence)
                     );
                     PRAGMA user_version = 1;
                     COMMIT;",
                )?;
            }
            STORE_SCHEMA_VERSION => {
                // Never mutate an unknown schema silently, and never assume
                // a user_version value alone means the table still exists.
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='conversation_events'",
                    [],
                    |row| row.get(0),
                )?;
                if count != 1 {
                    return Err(StoreError::DatabaseVersion(version));
                }
            }
            other => return Err(StoreError::DatabaseVersion(other)),
        }
        Ok(store)
    }

    fn connect(&self) -> Result<Connection, StoreError> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        Ok(conn)
    }

    /// Atomically allocate a sequence and append one validated event.
    /// Retries with an identical event ID + body return the previous record.
    pub fn append(&self, draft: &EventDraft) -> Result<AppendResult, StoreError> {
        // Never place known credential-bearing JSON keys in the persisted event.
        // This is NOT a substitute for scrubbing unstructured command output.
        let mut safe_draft = draft.clone();
        safe_draft.data = redact_tool_arguments(&draft.data);
        // Validate identifiers/data/size without making a sequence public.
        safe_draft.with_sequence(1).validate()?;
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT event_json FROM conversation_events WHERE event_id=?1",
                params![safe_draft.event_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(json) = previous {
            let saved = ConversationEvent::parse_checked(json.as_bytes())?;
            if saved == safe_draft.with_sequence(saved.sequence) {
                tx.commit()?;
                return Ok(AppendResult::AlreadyPresent(saved));
            }
            return Err(StoreError::EventIdConflict(safe_draft.event_id.clone()));
        }
        let last: Option<i64> = tx.query_row(
            "SELECT MAX(sequence) FROM conversation_events WHERE conversation_id=?1",
            params![safe_draft.conversation_id],
            |row| row.get(0),
        )?;
        let next = last
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(StoreError::SequenceOverflow)?;
        let event = safe_draft
            .with_sequence(u64::try_from(next).map_err(|_| StoreError::SequenceOverflow)?);
        event.validate()?;
        let json = serde_json::to_string(&event)
            .map_err(|err| StoreError::Event(EventError::Json(err.to_string())))?;
        tx.execute(
            "INSERT INTO conversation_events (conversation_id, sequence, event_id, event_json) \
             VALUES (?1, ?2, ?3, ?4)",
            params![event.conversation_id, next, event.event_id, json],
        )?;
        tx.commit()?;
        Ok(AppendResult::Inserted(event))
    }

    /// Fetch a contiguous, ordered page strictly after `cursor`.
    /// A cursor of zero means "from the beginning".
    pub fn replay_after(
        &self,
        conversation_id: &str,
        cursor: u64,
        limit: usize,
    ) -> Result<Vec<ConversationEvent>, StoreError> {
        if conversation_id.trim().is_empty() || conversation_id.len() > 128 {
            return Err(StoreError::Event(EventError::Identifier("conversation_id")));
        }
        if !(1..=MAX_REPLAY_PAGE).contains(&limit) {
            return Err(StoreError::InvalidLimit);
        }
        let cursor_sql: i64 = cursor.try_into().map_err(|_| StoreError::InvalidCursor)?;
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT event_json FROM conversation_events \
             WHERE conversation_id=?1 AND sequence>?2 \
             ORDER BY sequence ASC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![conversation_id, cursor_sql, limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut events = Vec::new();
        for row in rows {
            events.push(ConversationEvent::parse_checked(row?.as_bytes())?);
        }
        if !events.is_empty() {
            let first = cursor.checked_add(1).ok_or(StoreError::InvalidCursor)?;
            validate_replay_page(&events, first)?;
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    fn draft(name: &str, conversation: &str) -> EventDraft {
        EventDraft {
            event_id: name.to_string(),
            conversation_id: conversation.to_string(),
            turn_id: Some("turn-1".into()),
            run_id: None,
            timestamp: Utc::now(),
            event_type: EventType::MessageAccepted,
            data: json!({"content": "hello"}),
            trace_id: None,
            source_node: None,
        }
    }

    #[test]
    fn append_is_ordered_and_survives_reopening() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("events.sqlite");
        let store = ConversationEventStore::open(&path).unwrap();
        let first = store.append(&draft("ev-a", "chat-1")).unwrap();
        let second = store.append(&draft("ev-b", "chat-1")).unwrap();
        let other = store.append(&draft("ev-c", "chat-2")).unwrap();
        assert_eq!(first.event().sequence, 1);
        assert_eq!(second.event().sequence, 2);
        assert_eq!(other.event().sequence, 1);
        drop(store);
        let reopened = ConversationEventStore::open(&path).unwrap();
        let page = reopened.replay_after("chat-1", 0, 10).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].event_id, "ev-a");
        assert_eq!(reopened.replay_after("chat-1", 1, 10).unwrap().len(), 1);
        assert!(reopened.replay_after("chat-1", 2, 10).unwrap().is_empty());
    }

    #[test]
    fn retry_is_idempotent_and_conflicting_ids_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConversationEventStore::open(dir.path().join("events.sqlite")).unwrap();
        let request = draft("same-id", "chat-1");
        assert!(matches!(
            store.append(&request).unwrap(),
            AppendResult::Inserted(_)
        ));
        assert!(matches!(
            store.append(&request).unwrap(),
            AppendResult::AlreadyPresent(_)
        ));
        let mut conflict = request.clone();
        conflict.data = json!({"content": "different"});
        assert!(matches!(
            store.append(&conflict),
            Err(StoreError::EventIdConflict(_))
        ));
        assert_eq!(store.replay_after("chat-1", 0, 10).unwrap().len(), 1);
    }

    #[test]
    fn concurrent_writers_obtain_unique_contiguous_sequences() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Arc::new(ConversationEventStore::open(dir.path().join("events.sqlite")).unwrap());
        let handles: Vec<_> = (0..12)
            .map(|index| {
                let store = Arc::clone(&store);
                std::thread::spawn(move || {
                    store.append(&draft(&format!("event-{index}"), "chat-1"))
                })
            })
            .collect();
        for handle in handles {
            assert!(matches!(
                handle.join().unwrap(),
                Ok(AppendResult::Inserted(_))
            ));
        }
        let events = store.replay_after("chat-1", 0, 12).unwrap();
        let observed: Vec<u64> = events.iter().map(|event| event.sequence).collect();
        assert_eq!(observed, (1..=12).collect::<Vec<_>>());
    }

    #[test]
    fn nested_known_secret_fields_are_redacted_before_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConversationEventStore::open(dir.path().join("events.sqlite")).unwrap();
        let mut request = draft("redacted", "chat-1");
        request.event_type = EventType::ToolStarted;
        request.data = json!({"arguments":{"api_key":"DO-NOT-PERSIST", "command":"echo hi"}});
        store.append(&request).unwrap();
        let saved = store.replay_after("chat-1", 0, 10).unwrap();
        assert_eq!(saved[0].data["arguments"]["api_key"], "[REDACTED]");
        let raw = std::fs::read(dir.path().join("events.sqlite")).unwrap();
        assert!(!raw
            .windows(b"DO-NOT-PERSIST".len())
            .any(|w| w == b"DO-NOT-PERSIST"));
    }

    #[test]
    fn replay_rejects_invalid_page_sizes_and_cursors() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConversationEventStore::open(dir.path().join("events.sqlite")).unwrap();
        assert!(matches!(
            store.replay_after("chat-1", 0, 0),
            Err(StoreError::InvalidLimit)
        ));
        assert!(matches!(
            store.replay_after("chat-1", u64::MAX, 10),
            Err(StoreError::InvalidCursor)
        ));
    }
}
