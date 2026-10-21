//! V3 typed conversation event contract.
//!
//! Additive to V2: no existing WebSocket format changes. This module defines
//! the durable envelope and a deliberately narrow adapter for legacy frames.
//! Event persistence, correlation, and transport fan-out are later PRs.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

pub const SCHEMA_VERSION: u16 = 1;
pub const MAX_EVENT_BYTES: usize = 65_536;
pub const MAX_DATA_PROPERTIES: usize = 40;
pub const MAX_ID_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventType {
    #[serde(rename = "message.accepted")]
    MessageAccepted,
    #[serde(rename = "assistant.delta")]
    AssistantDelta,
    #[serde(rename = "assistant.completed")]
    AssistantCompleted,
    #[serde(rename = "run.created")]
    RunCreated,
    #[serde(rename = "run.status")]
    RunStatus,
    #[serde(rename = "step.status")]
    StepStatus,
    #[serde(rename = "tool.started")]
    ToolStarted,
    #[serde(rename = "tool.completed")]
    ToolCompleted,
    #[serde(rename = "artifact.created")]
    ArtifactCreated,
    #[serde(rename = "approval.requested")]
    ApprovalRequested,
    #[serde(rename = "approval.resolved")]
    ApprovalResolved,
    #[serde(rename = "model.metrics")]
    ModelMetrics,
    #[serde(rename = "worker.status")]
    WorkerStatus,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "session.snapshot")]
    SessionSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationEvent {
    pub schema_version: u16,
    pub event_id: String,
    pub conversation_id: String,
    pub sequence: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub timestamp: DateTime<Utc>,
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub data: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_node: Option<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EventError {
    #[error("unsupported schema version {0}")]
    SchemaVersion(u16),
    #[error("invalid identifier field: {0}")]
    Identifier(&'static str),
    #[error("event sequence must be >= 1")]
    Sequence,
    #[error("event data must be an object with <= 40 keys")]
    DataShape,
    #[error("event exceeds 64 KiB serialized limit")]
    TooLarge,
    #[error("event serialization or JSON parsing failed: {0}")]
    Json(String),
    #[error("out-of-order, duplicated, or missing replay event at sequence {0}")]
    Replay(u64),
    #[error("replay mixes distinct conversations")]
    MixedConversations,
    #[error("unsupported V2 WebSocket event kind")]
    UnsupportedLegacyKind,
}

fn valid_id(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= MAX_ID_BYTES && !s.chars().any(char::is_control)
}

impl ConversationEvent {
    /// Every externally supplied event must pass this check before persistence.
    pub fn validate(&self) -> Result<(), EventError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(EventError::SchemaVersion(self.schema_version));
        }
        for (field, id) in [
            ("event_id", Some(&self.event_id)),
            ("conversation_id", Some(&self.conversation_id)),
            ("turn_id", self.turn_id.as_ref()),
            ("run_id", self.run_id.as_ref()),
            ("trace_id", self.trace_id.as_ref()),
            ("source_node", self.source_node.as_ref()),
        ] {
            if id.is_some_and(|value| !valid_id(value)) {
                return Err(EventError::Identifier(field));
            }
        }
        if self.sequence == 0 {
            return Err(EventError::Sequence);
        }
        let Some(object) = self.data.as_object() else {
            return Err(EventError::DataShape);
        };
        if object.len() > MAX_DATA_PROPERTIES {
            return Err(EventError::DataShape);
        }
        let bytes = serde_json::to_vec(self).map_err(|err| EventError::Json(err.to_string()))?;
        if bytes.len() > MAX_EVENT_BYTES {
            return Err(EventError::TooLarge);
        }
        Ok(())
    }

    pub fn parse_checked(bytes: &[u8]) -> Result<Self, EventError> {
        if bytes.len() > MAX_EVENT_BYTES {
            return Err(EventError::TooLarge);
        }
        let event: Self =
            serde_json::from_slice(bytes).map_err(|err| EventError::Json(err.to_string()))?;
        event.validate()?;
        Ok(event)
    }

    pub fn json_checked(&self) -> Result<Vec<u8>, EventError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|err| EventError::Json(err.to_string()))
    }
}

/// Validate an ordered contiguous replay page for a *single* conversation.
/// Start sequence comes from the persisted checkpoint / replay cursor.
/// Empty pages are valid. Gaps must trigger another fetch, not silent success.
pub fn validate_replay_page(
    events: &[ConversationEvent],
    starting_sequence: u64,
) -> Result<(), EventError> {
    if starting_sequence == 0 {
        return Err(EventError::Sequence);
    }
    let mut seen = HashSet::new();
    let mut expected = starting_sequence;
    let mut conversation: Option<&str> = None;
    for event in events {
        event.validate()?;
        if let Some(id) = conversation {
            if event.conversation_id != id {
                return Err(EventError::MixedConversations);
            }
        } else {
            conversation = Some(&event.conversation_id);
        }
        if event.sequence != expected || !seen.insert(event.event_id.as_str()) {
            return Err(EventError::Replay(expected));
        }
        expected = expected
            .checked_add(1)
            .ok_or(EventError::Replay(expected))?;
    }
    Ok(())
}

fn sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase().replace('-', "_");
    matches!(
        k.as_str(),
        "authorization"
            | "cookie"
            | "set_cookie"
            | "password"
            | "passwd"
            | "secret"
            | "api_key"
            | "apikey"
            | "private_key"
            | "client_secret"
            | "access_token"
            | "refresh_token"
            | "auth_token"
            | "bearer_token"
            | "ssh_key"
            | "credential"
            | "credentials"
    ) || k.ends_with("_password")
        || k.ends_with("_secret")
        || k.ends_with("_api_key")
}

/// Redact known credential-bearing argument *keys*, including nested arrays.
/// This does not claim to scrub secrets embedded in arbitrary freeform stdout.
pub fn redact_tool_arguments(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut result = Map::with_capacity(map.len());
            for (key, v) in map {
                result.insert(
                    key.clone(),
                    if sensitive_key(key) {
                        Value::String("[REDACTED]".into())
                    } else {
                        redact_tool_arguments(v)
                    },
                );
            }
            Value::Object(result)
        }
        Value::Array(values) => Value::Array(values.iter().map(redact_tool_arguments).collect()),
        _ => value.clone(),
    }
}

/// Safe additive adapter: V2 frames are not changed by calling this helper.
/// The caller owns event IDs, conversation IDs, and sequence allocation.
/// Unsupported legacy kinds must be handled by a future explicit adapter,
/// never guessed or reclassified as a completed assistant answer.
pub fn from_v2_ws_frame(
    frame: &Value,
    event_id: impl Into<String>,
    conversation_id: impl Into<String>,
    sequence: u64,
    timestamp: DateTime<Utc>,
) -> Result<ConversationEvent, EventError> {
    let kind = frame.get("type").and_then(Value::as_str).unwrap_or("");
    let (event_type, data) = match kind {
        "chunk" => (
            EventType::AssistantDelta,
            serde_json::json!({"content": frame.get("content").cloned().unwrap_or(Value::Null)}),
        ),
        "done" => (
            EventType::AssistantCompleted,
            serde_json::json!({"content": frame.get("full_response").cloned().unwrap_or(Value::Null)}),
        ),
        "tool_call" => (
            EventType::ToolStarted,
            serde_json::json!({
                "tool_name": frame.get("name").cloned().unwrap_or(Value::Null),
                "arguments": redact_tool_arguments(frame.get("args").unwrap_or(&Value::Null)),
            }),
        ),
        "tool_result" => (
            EventType::ToolCompleted,
            serde_json::json!({
                "tool_name": frame.get("name").cloned().unwrap_or(Value::Null),
                "success": frame.get("success").cloned().unwrap_or(Value::Null),
                "duration_secs": frame.get("duration_secs").cloned().unwrap_or(Value::Null),
                "output": frame.get("output").cloned().unwrap_or(Value::Null),
            }),
        ),
        "error" => (
            EventType::Error,
            serde_json::json!({"message": frame.get("message").cloned().unwrap_or(Value::Null)}),
        ),
        "cancelled" => (
            EventType::RunStatus,
            serde_json::json!({"status":"cancelled","message":frame.get("message").cloned().unwrap_or(Value::Null)}),
        ),
        "metrics" => (
            EventType::ModelMetrics,
            serde_json::json!({"metrics":frame.get("metrics").cloned().unwrap_or(Value::Null)}),
        ),
        _ => return Err(EventError::UnsupportedLegacyKind),
    };
    let event = ConversationEvent {
        schema_version: SCHEMA_VERSION,
        event_id: event_id.into(),
        conversation_id: conversation_id.into(),
        sequence,
        turn_id: None,
        run_id: None,
        timestamp,
        event_type,
        data,
        trace_id: None,
        source_node: None,
    };
    event.validate()?;
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(sequence: u64) -> ConversationEvent {
        ConversationEvent {
            schema_version: SCHEMA_VERSION,
            event_id: format!("ev-{sequence}"),
            conversation_id: "conversation-1".into(),
            sequence,
            turn_id: None,
            run_id: None,
            timestamp: Utc::now(),
            event_type: EventType::MessageAccepted,
            data: json!({"content": "hello"}),
            trace_id: None,
            source_node: None,
        }
    }

    #[test]
    fn event_roundtrip() {
        let original = event(1);
        let bytes = original.json_checked().unwrap();
        let restored = ConversationEvent::parse_checked(&bytes).unwrap();
        assert_eq!(restored, original);
        assert_eq!(restored.event_type, EventType::MessageAccepted);
    }

    #[test]
    fn invalid_versions_unknown_types_and_keys_fail() {
        let mut invalid_version = event(1);
        invalid_version.schema_version = 99;
        assert_eq!(
            invalid_version.validate(),
            Err(EventError::SchemaVersion(99))
        );
        let bad_type = serde_json::to_vec(&event(1)).unwrap();
        let mut value: Value = serde_json::from_slice(&bad_type).unwrap();
        value["type"] = json!("unknown.event");
        assert!(ConversationEvent::parse_checked(&serde_json::to_vec(&value).unwrap()).is_err());
        value["type"] = json!("message.accepted");
        value["surprise"] = json!(true);
        assert!(ConversationEvent::parse_checked(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn identity_and_payload_bounds() {
        let mut e = event(0);
        assert_eq!(e.validate(), Err(EventError::Sequence));
        e.sequence = 1;
        e.event_id = " ".into();
        assert_eq!(e.validate(), Err(EventError::Identifier("event_id")));
        e.event_id = "ok".into();
        e.data = json!("not an object");
        assert_eq!(e.validate(), Err(EventError::DataShape));
        e.data = json!({"body": "a".repeat(MAX_EVENT_BYTES)});
        assert_eq!(e.validate(), Err(EventError::TooLarge));
    }

    #[test]
    fn replay_detects_gaps_duplicates_and_mixed_conversations() {
        assert!(validate_replay_page(&[event(3), event(4)], 3).is_ok());
        assert_eq!(
            validate_replay_page(&[event(3), event(5)], 3),
            Err(EventError::Replay(4))
        );
        assert_eq!(
            validate_replay_page(&[event(3), event(3)], 3),
            Err(EventError::Replay(4))
        );
        let mut other = event(4);
        other.conversation_id = "different".into();
        assert_eq!(
            validate_replay_page(&[event(3), other], 3),
            Err(EventError::MixedConversations)
        );
    }

    #[test]
    fn legacy_tool_arguments_are_redacted_recursively() {
        let frame = json!({
            "type": "tool_call", "name": "shell", "args": {
                "api_key": "never-log-this", "token_count": 45,
                "options": [{"password": "also-secret", "command": "echo hello"}]
            }
        });
        let e = from_v2_ws_frame(&frame, "event", "conv", 1, Utc::now()).unwrap();
        assert_eq!(e.event_type, EventType::ToolStarted);
        assert_eq!(e.data["arguments"]["api_key"], "[REDACTED]");
        assert_eq!(e.data["arguments"]["options"][0]["password"], "[REDACTED]");
        assert_eq!(e.data["arguments"]["token_count"], 45);
        assert_eq!(e.data["arguments"]["options"][0]["command"], "echo hello");
        assert!(!e
            .json_checked()
            .unwrap()
            .windows(14)
            .any(|w| w == b"never-log-this"));
    }

    #[test]
    fn unmapped_legacy_kinds_are_not_silently_converted() {
        let frame = json!({"type":"totally-unknown","content":"looks completed"});
        assert_eq!(
            from_v2_ws_frame(&frame, "event", "conv", 1, Utc::now()),
            Err(EventError::UnsupportedLegacyKind)
        );
    }
}
