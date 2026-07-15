use super::{
    AppState, RATE_LIMIT_WINDOW_SECS, WebhookBody, client_key_from_request, hash_webhook_secret,
    run_gateway_chat_simple, sanitize_gateway_response, webhook_memory_key,
};
use crate::memory::MemoryCategory;
use crate::providers;
use crate::security::pairing::constant_time_eq;
use axum::{
    Json,
    extract::rejection::JsonRejection,
    http::{HeaderMap, StatusCode, header},
};
use serde_json::Value;
use std::net::SocketAddr;
use std::time::Instant;

struct WebhookTelemetry {
    provider_label: String,
    model_label: String,
    started_at: Instant,
}

impl WebhookTelemetry {
    fn start(state: &AppState) -> Self {
        let runtime = state.runtime_snapshot();
        let provider_label = state
            .config
            .lock()
            .default_provider
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let model_label = runtime.model.clone();
        let started_at = Instant::now();

        state
            .observer
            .record_event(&crate::observability::ObserverEvent::AgentStart {
                provider: provider_label.clone(),
                model: model_label.clone(),
            });
        state
            .observer
            .record_event(&crate::observability::ObserverEvent::LlmRequest {
                provider: provider_label.clone(),
                model: model_label.clone(),
                messages_count: 1,
            });

        Self {
            provider_label,
            model_label,
            started_at,
        }
    }

    fn finish_success(self, state: &AppState) {
        let duration = self.started_at.elapsed();

        state
            .observer
            .record_event(&crate::observability::ObserverEvent::LlmResponse {
                provider: self.provider_label.clone(),
                model: self.model_label.clone(),
                duration,
                success: true,
                error_message: None,
                input_tokens: None,
                output_tokens: None,
            });
        state
            .observer
            .record_metric(&crate::observability::traits::ObserverMetric::RequestLatency(duration));
        state
            .observer
            .record_event(&crate::observability::ObserverEvent::AgentEnd {
                provider: self.provider_label,
                model: self.model_label,
                duration,
                tokens_used: None,
                cost_usd: None,
            });
    }

    fn finish_error(self, state: &AppState, error_message: &str) {
        let duration = self.started_at.elapsed();
        let sanitized = error_message.to_string();

        state
            .observer
            .record_event(&crate::observability::ObserverEvent::LlmResponse {
                provider: self.provider_label.clone(),
                model: self.model_label.clone(),
                duration,
                success: false,
                error_message: Some(sanitized.clone()),
                input_tokens: None,
                output_tokens: None,
            });
        state
            .observer
            .record_metric(&crate::observability::traits::ObserverMetric::RequestLatency(duration));
        state
            .observer
            .record_event(&crate::observability::ObserverEvent::Error {
                component: "gateway".to_string(),
                message: sanitized.clone(),
            });
        state
            .observer
            .record_event(&crate::observability::ObserverEvent::AgentEnd {
                provider: self.provider_label,
                model: self.model_label,
                duration,
                tokens_used: None,
                cost_usd: None,
            });
    }
}

pub(super) async fn handle_webhook_inner(
    state: AppState,
    peer_addr: SocketAddr,
    headers: HeaderMap,
    body: Result<Json<WebhookBody>, JsonRejection>,
) -> (StatusCode, Json<Value>) {
    if let Some(response) = enforce_rate_limit(&state, peer_addr, &headers) {
        return response;
    }

    if let Some(response) = authorize_webhook_request(&state, peer_addr, &headers) {
        return response;
    }

    let webhook_body = match parse_webhook_body(body) {
        Ok(webhook_body) => webhook_body,
        Err(response) => return response,
    };

    if let Some(response) = enforce_idempotency(&state, &headers) {
        return response;
    }

    maybe_persist_inbound_message(&state, &webhook_body.message).await;

    let telemetry = WebhookTelemetry::start(&state);
    let runtime = state.runtime_snapshot();

    match run_gateway_chat_simple(&state, &webhook_body.message).await {
        Ok(response) => {
            let safe_response =
                sanitize_gateway_response(&response, runtime.tools_registry_exec.as_ref());
            telemetry.finish_success(&state);
            let body = serde_json::json!({"response": safe_response, "model": runtime.model});
            (StatusCode::OK, Json(body))
        }
        Err(error) => {
            let sanitized = providers::sanitize_api_error(&error.to_string());
            telemetry.finish_error(&state, &sanitized);
            tracing::error!("Webhook provider error: {}", sanitized);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "LLM request failed"})),
            )
        }
    }
}

fn enforce_rate_limit(
    state: &AppState,
    peer_addr: SocketAddr,
    headers: &HeaderMap,
) -> Option<(StatusCode, Json<Value>)> {
    let rate_key = client_key_from_request(Some(peer_addr), headers, state.trust_forwarded_headers);
    if state.rate_limiter.allow_webhook(&rate_key) {
        return None;
    }

    tracing::warn!("/webhook rate limit exceeded");
    Some((
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({
            "error": "Too many webhook requests. Please retry later.",
            "retry_after": RATE_LIMIT_WINDOW_SECS,
        })),
    ))
}

fn authorize_webhook_request(
    state: &AppState,
    peer_addr: SocketAddr,
    headers: &HeaderMap,
) -> Option<(StatusCode, Json<Value>)> {
    if !state.pairing.require_pairing()
        && state.webhook_secret_hash.is_none()
        && !peer_addr.ip().is_loopback()
    {
        tracing::warn!(
            "Webhook: rejected unauthenticated non-loopback request (pairing disabled and no webhook secret configured)"
        );
        return Some((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "Unauthorized — configure pairing or X-Webhook-Secret for non-local webhook access"
            })),
        ));
    }

    if state.pairing.require_pairing() {
        let auth = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        let token = auth.strip_prefix("Bearer ").unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            tracing::warn!("Webhook: rejected — not paired / invalid bearer token");
            return Some((
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>"
                })),
            ));
        }
    }

    if let Some(ref secret_hash) = state.webhook_secret_hash {
        match extract_webhook_secret_header_hash(headers) {
            Some(header_hash) if constant_time_eq(&header_hash, secret_hash.as_ref()) => {}
            _ => {
                tracing::warn!("Webhook: rejected request — invalid or missing X-Webhook-Secret");
                return Some((
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({
                        "error": "Unauthorized — invalid or missing X-Webhook-Secret header"
                    })),
                ));
            }
        }
    }

    None
}

fn parse_webhook_body(
    body: Result<Json<WebhookBody>, JsonRejection>,
) -> Result<WebhookBody, (StatusCode, Json<Value>)> {
    match body {
        Ok(Json(webhook_body)) => Ok(webhook_body),
        Err(error) => {
            tracing::warn!("Webhook JSON parse error: {error}");
            Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Invalid JSON body. Expected: {\"message\": \"...\"}"
                })),
            ))
        }
    }
}

fn enforce_idempotency(state: &AppState, headers: &HeaderMap) -> Option<(StatusCode, Json<Value>)> {
    let idempotency_key = extract_idempotency_key(headers)?;

    if state.idempotency_store.record_if_new(idempotency_key) {
        return None;
    }

    tracing::info!("Webhook duplicate ignored (idempotency key: {idempotency_key})");
    Some((
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "duplicate",
            "idempotent": true,
            "message": "Request already processed for this idempotency key"
        })),
    ))
}

fn extract_idempotency_key(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("X-Idempotency-Key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn extract_webhook_secret_header_hash(headers: &HeaderMap) -> Option<String> {
    headers
        .get("X-Webhook-Secret")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(hash_webhook_secret)
}

async fn maybe_persist_inbound_message(state: &AppState, message: &str) {
    if !state.auto_save {
        return;
    }

    let key = webhook_memory_key();
    let _ = state
        .mem
        .store(&key, message, MemoryCategory::Conversation, None)
        .await;
}

#[cfg(test)]
mod tests {
    use super::extract_idempotency_key;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn extract_idempotency_key_ignores_empty_values() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Idempotency-Key", HeaderValue::from_static(""));
        assert!(extract_idempotency_key(&headers).is_none());
    }

    #[test]
    fn extract_idempotency_key_reads_present_value() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Idempotency-Key", HeaderValue::from_static("request-123"));
        assert_eq!(extract_idempotency_key(&headers), Some("request-123"));
    }
}
