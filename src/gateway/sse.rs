//! Server-Sent Events (SSE) stream for real-time event delivery.
//!
//! Wraps the broadcast channel in AppState to deliver events to web dashboard clients.

use super::AppState;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use std::convert::Infallible;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

/// GET /api/events — SSE event stream
pub async fn handle_sse_events(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Auth check
    if state.pairing.require_pairing() {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|auth| auth.strip_prefix("Bearer "))
            .unwrap_or("");

        if !state.pairing.is_authenticated(token) {
            return (
                StatusCode::UNAUTHORIZED,
                "Unauthorized — provide Authorization: Bearer <token>",
            )
                .into_response();
        }
    }

    let rx = state.event_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(
        |result: Result<
            serde_json::Value,
            tokio_stream::wrappers::errors::BroadcastStreamRecvError,
        >| {
            match result {
                Ok(value) => Some(Ok::<_, Infallible>(
                    Event::default().data(value.to_string()),
                )),
                Err(_) => None, // Skip lagged messages
            }
        },
    );

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Broadcast observer that forwards events to the SSE broadcast channel.
pub struct BroadcastObserver {
    inner: Box<dyn crate::observability::Observer>,
    tx: tokio::sync::broadcast::Sender<serde_json::Value>,
}

impl BroadcastObserver {
    pub fn new(
        inner: Box<dyn crate::observability::Observer>,
        tx: tokio::sync::broadcast::Sender<serde_json::Value>,
    ) -> Self {
        Self { inner, tx }
    }
}

impl crate::observability::Observer for BroadcastObserver {
    fn record_event(&self, event: &crate::observability::ObserverEvent) {
        // Forward to inner observer
        self.inner.record_event(event);

        // Broadcast to SSE subscribers
        let json = match event {
            crate::observability::ObserverEvent::LlmRequest {
                provider,
                model,
                messages_count,
            } => serde_json::json!({
                "type": "llm_request",
                "provider": provider,
                "model": model,
                "messages_count": messages_count,
                "message": format!("provider={provider} model={model} messages_count={messages_count}"),
                "log_line": format!("llm.request provider={provider} model={model} messages_count={messages_count}"),
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
            crate::observability::ObserverEvent::LlmResponse {
                provider,
                model,
                duration,
                success,
                error_message,
                input_tokens,
                output_tokens,
            } => serde_json::json!({
                "type": "llm_response",
                "provider": provider,
                "model": model,
                "duration_ms": duration.as_millis(),
                "success": success,
                "error": error_message,
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "message": format!(
                    "provider={provider} model={model} duration_ms={} success={} error={}",
                    duration.as_millis(),
                    success,
                    error_message.as_deref().unwrap_or("none")
                ),
                "log_line": format!(
                    "llm.response provider={provider} model={model} duration_ms={} success={} error={}",
                    duration.as_millis(),
                    success,
                    error_message.as_deref().unwrap_or("none")
                ),
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
            crate::observability::ObserverEvent::ToolCall {
                tool,
                duration,
                success,
            } => serde_json::json!({
                "type": "tool_call",
                "tool": tool,
                "duration_ms": duration.as_millis(),
                "success": success,
                "message": format!("tool={tool} duration_ms={} success={success}", duration.as_millis()),
                "log_line": format!("tool.call tool={tool} duration_ms={} success={success}", duration.as_millis()),
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
            crate::observability::ObserverEvent::ToolCallStart { tool } => serde_json::json!({
                "type": "tool_call_start",
                "tool": tool,
                "message": format!("tool={tool}"),
                "log_line": format!("tool.start tool={tool}"),
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
            crate::observability::ObserverEvent::Error { component, message } => {
                serde_json::json!({
                    "type": "error",
                    "component": component,
                    "message": message,
                    "log_line": format!("error component={component} message={message}"),
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                })
            }
            crate::observability::ObserverEvent::AgentStart { provider, model } => {
                serde_json::json!({
                    "type": "agent_start",
                    "provider": provider,
                    "model": model,
                    "message": format!("provider={provider} model={model}"),
                    "log_line": format!("agent.start provider={provider} model={model}"),
                "timestamp": chrono::Utc::now().to_rfc3339(),
                })
            }
            crate::observability::ObserverEvent::AgentEnd {
                provider,
                model,
                duration,
                tokens_used,
                cost_usd,
            } => serde_json::json!({
                "type": "agent_end",
                "provider": provider,
                "model": model,
                "duration_ms": duration.as_millis(),
                "tokens_used": tokens_used,
                "cost_usd": cost_usd,
                "message": format!(
                    "provider={provider} model={model} duration_ms={} tokens_used={} cost_usd={}",
                    duration.as_millis(),
                    tokens_used.map(|v| v.to_string()).unwrap_or_else(|| "none".to_string()),
                    cost_usd.map(|v| v.to_string()).unwrap_or_else(|| "none".to_string())
                ),
                "log_line": format!(
                    "agent.end provider={provider} model={model} duration_ms={} tokens_used={} cost_usd={}",
                    duration.as_millis(),
                    tokens_used.map(|v| v.to_string()).unwrap_or_else(|| "none".to_string()),
                    cost_usd.map(|v| v.to_string()).unwrap_or_else(|| "none".to_string())
                ),
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
            crate::observability::ObserverEvent::TurnComplete => serde_json::json!({
                "type": "turn_complete",
                "message": "turn.complete",
                "log_line": "turn.complete",
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
            _ => return, // Skip events we don't broadcast
        };

        let _ = self.tx.send(json);
    }

    fn record_metric(&self, metric: &crate::observability::traits::ObserverMetric) {
        self.inner.record_metric(metric);
    }

    fn flush(&self) {
        self.inner.flush();
    }

    fn name(&self) -> &str {
        "broadcast"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
