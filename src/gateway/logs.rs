use super::AppState;
use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

const DEFAULT_LOG_LIMIT: usize = 200;
const MAX_LOG_LIMIT: usize = 1_000;

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    limit: Option<usize>,
}
#[derive(Debug, Serialize)]
pub struct RuntimeLogsResponse {
    pub entries: Vec<crate::runtime_logs::RuntimeLogEntry>,
}

pub async fn handle_api_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LogsQuery>,
) -> impl IntoResponse {
    if let Some(response) = unauthorized_response(&state, &headers) {
        return response;
    }

    let limit = query
        .limit
        .unwrap_or(DEFAULT_LOG_LIMIT)
        .clamp(1, MAX_LOG_LIMIT);
    let entries = crate::runtime_logs::global_runtime_log_store().tail(limit);

    Json(RuntimeLogsResponse { entries }).into_response()
}

pub async fn handle_log_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = unauthorized_response(&state, &headers) {
        return response;
    }

    let rx = crate::runtime_logs::global_runtime_log_store().subscribe();
    let stream = BroadcastStream::new(rx).filter_map(
        |result: Result<
            crate::runtime_logs::RuntimeLogEntry,
            tokio_stream::wrappers::errors::BroadcastStreamRecvError,
        >| match result {
            Ok(entry) => Some(Ok::<_, Infallible>(
                Event::default().data(
                    serde_json::json!({
                        "type": "runtime_log",
                        "id": entry.id,
                        "timestamp": entry.timestamp,
                        "line": entry.line,
                    })
                    .to_string(),
                ),
            )),
            Err(_) => None,
        },
    );

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn unauthorized_response(
    state: &AppState,
    headers: &HeaderMap,
) -> Option<axum::response::Response> {
    if !state.pairing.require_pairing() {
        return None;
    }

    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
        .unwrap_or("");

    if state.pairing.is_authenticated(token) {
        None
    } else {
        Some(
            (
                StatusCode::UNAUTHORIZED,
                "Unauthorized — provide Authorization: Bearer <token>",
            )
                .into_response(),
        )
    }
}
