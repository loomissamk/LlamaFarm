use super::peer_registry::{FederationCapabilities, FederationPeerRegistry, FederationPeerTarget};
use crate::agent::loop_::{format_inference_metrics_summary, InferenceMetricsDelta};
use crate::tools::ToolResult;
use chrono::Utc;
use futures_util::StreamExt;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const TASK_EVENT_HISTORY_LIMIT: usize = 256;
/// Bounds only connection/control-plane handshakes (task start/cancel/capability
/// calls). The task result stream itself is bounded separately by
/// `FederationRemoteSubagentAdapter::task_timeout` (config:
/// `federation.task_timeout_seconds`) — an overloaded or stuck peer used to be
/// able to hang the calling agent's turn indefinitely; it can't anymore.
const FEDERATION_CONTROL_TIMEOUT_SECS: u64 = 10;
pub const FEDERATION_AUTH_HEADER: &str = "x-llamafarm-federation-token";
pub const FEDERATION_TASK_ID_HEADER: &str = "x-llamafarm-task-id";

fn build_federation_http_client(connect_timeout: Duration) -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .build()?)
}

fn configured_federation_token() -> Option<String> {
    std::env::var("LLAMAFARM_FEDERATION_TOKEN")
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

/// Attach the optional shared node token to peer-to-peer federation traffic.
/// An unset token preserves standalone/local federation behavior, while the
/// deployed two-node profiles require one at their gateway boundary.
fn with_federation_auth(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    if let Some(token) = configured_federation_token() {
        request.header(FEDERATION_AUTH_HEADER, token)
    } else {
        request
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationTaskRequest {
    pub prompt: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub requester_node_id: Option<String>,
    #[serde(default)]
    pub requester_name: Option<String>,
    #[serde(default)]
    pub agentic: bool,
    #[serde(default)]
    pub max_iterations: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationTaskAccepted {
    pub task_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationTaskEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub task_id: String,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Provider-measured inference timing/token telemetry for this segment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<serde_json::Value>,
}

impl FederationTaskEvent {
    pub fn status(task_id: &str, message: impl Into<String>) -> Self {
        Self {
            event_type: "status".to_string(),
            task_id: task_id.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            content: None,
            full_response: None,
            name: None,
            args: None,
            success: None,
            duration_secs: None,
            output: None,
            message: Some(message.into()),
            metrics: None,
        }
    }

    pub fn done(task_id: &str, full_response: impl Into<String>) -> Self {
        Self {
            event_type: "done".to_string(),
            task_id: task_id.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            content: None,
            full_response: Some(full_response.into()),
            name: None,
            args: None,
            success: Some(true),
            duration_secs: None,
            output: None,
            message: None,
            metrics: None,
        }
    }

    pub fn error(task_id: &str, message: impl Into<String>) -> Self {
        Self {
            event_type: "error".to_string(),
            task_id: task_id.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            content: None,
            full_response: None,
            name: None,
            args: None,
            success: Some(false),
            duration_secs: None,
            output: None,
            message: Some(message.into()),
            metrics: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationChatEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub session_id: String,
    pub peer_id: String,
    pub peer_name: String,
    pub delegate_agent: String,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<serde_json::Value>,
}

#[derive(Clone)]
pub struct FederationChatContext {
    pub session_id: String,
    pub selected_peer_ids: Vec<String>,
    pub event_tx: Option<mpsc::UnboundedSender<FederationChatEvent>>,
    /// Cancellation propagated from the originating interactive chat turn.
    /// This lets a browser Stop request also cancel a remote federation task
    /// after it has been accepted by the worker.
    pub cancellation: Option<CancellationToken>,
}

tokio::task_local! {
    static FEDERATION_CHAT_CONTEXT: Option<FederationChatContext>;
}

pub async fn with_chat_context<F>(context: Option<FederationChatContext>, future: F) -> F::Output
where
    F: std::future::Future,
{
    FEDERATION_CHAT_CONTEXT.scope(context, future).await
}

pub fn current_chat_context() -> Option<FederationChatContext> {
    FEDERATION_CHAT_CONTEXT
        .try_with(Clone::clone)
        .ok()
        .flatten()
}

#[derive(Clone)]
pub struct FederationRemoteSubagentAdapter {
    registry: Arc<FederationPeerRegistry>,
    client: reqwest::Client,
    local_node_id: Arc<RwLock<String>>,
    local_node_name: Arc<RwLock<String>>,
    /// Max time to wait for a delegated task's result. `None` means unlimited
    /// (operator opted out via `task_timeout_seconds = 0`).
    task_timeout: Option<Duration>,
}

impl FederationRemoteSubagentAdapter {
    pub fn new(
        registry: Arc<FederationPeerRegistry>,
        local_node_id: Arc<RwLock<String>>,
        local_node_name: Arc<RwLock<String>>,
        task_timeout_seconds: u64,
    ) -> anyhow::Result<Self> {
        let client =
            build_federation_http_client(Duration::from_secs(FEDERATION_CONTROL_TIMEOUT_SECS))?;
        let task_timeout =
            (task_timeout_seconds > 0).then(|| Duration::from_secs(task_timeout_seconds));

        Ok(Self {
            registry,
            client,
            local_node_id,
            task_timeout,
            local_node_name,
        })
    }

    pub fn http_client(&self) -> reqwest::Client {
        self.client.clone()
    }

    pub fn federation_auth_token(&self) -> Option<String> {
        configured_federation_token()
    }

    pub fn available_remote_agents(&self) -> Vec<String> {
        current_chat_context()
            .map(|context| {
                self.registry
                    .available_remote_agent_names(&context.selected_peer_ids)
            })
            .unwrap_or_default()
    }

    pub fn available_remote_agents_info(
        &self,
    ) -> Vec<crate::federation::peer_registry::RemoteAgentInfo> {
        current_chat_context()
            .map(|context| {
                self.registry
                    .available_remote_agent_infos(&context.selected_peer_ids)
            })
            .unwrap_or_default()
    }

    pub fn resolve_remote_agent(&self, agent_name: &str) -> Option<FederationPeerTarget> {
        let context = current_chat_context()?;
        self.registry
            .find_peer_by_agent_name(&context.selected_peer_ids, agent_name)
    }

    pub async fn execute_delegate(
        &self,
        agent_name: &str,
        prompt: &str,
        context: &str,
    ) -> anyhow::Result<ToolResult> {
        let peer = self
            .resolve_remote_agent(agent_name)
            .ok_or_else(|| anyhow::anyhow!("Unknown remote peer '{agent_name}'"))?;

        let request = FederationTaskRequest {
            prompt: prompt.to_string(),
            context: (!context.trim().is_empty()).then(|| context.to_string()),
            session_id: current_chat_context().map(|value| value.session_id),
            requester_node_id: Some(self.local_node_id.read().clone()),
            requester_name: Some(self.local_node_name.read().clone()),
            agentic: true,
            max_iterations: 0,
        };

        let accepted = self.start_remote_task(&peer, &request).await?;
        self.emit_chat_event(
            &peer,
            &accepted.task_id,
            FederationTaskEvent::status(
                &accepted.task_id,
                format!("Delegated to {}", peer.display_name),
            ),
        );

        let cancellation = current_chat_context().and_then(|context| context.cancellation);
        if cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            self.cancel_remote_task(&peer, &accepted.task_id).await;
            anyhow::bail!("Federated subtask cancelled by caller");
        }

        if let Some(cancellation) = cancellation {
            // `execute_one_tool` also observes the turn token and may drop this
            // future as soon as Stop is pressed. Keep a small forwarder alive so
            // that race cannot strand a remote task after the local tool future is
            // dropped. An explicit completion signal distinguishes normal return
            // from a dropped/cancelled caller future.
            let (finished_tx, mut finished_rx) = tokio::sync::oneshot::channel::<()>();
            let cancellation_forwarder = self.clone();
            let peer_for_cancellation = peer.clone();
            let task_id_for_cancellation = accepted.task_id.clone();
            let cancellation_for_forwarder = cancellation.clone();
            tokio::spawn(async move {
                tokio::select! {
                    () = cancellation_for_forwarder.cancelled() => {
                        cancellation_forwarder
                            .cancel_remote_task(&peer_for_cancellation, &task_id_for_cancellation)
                            .await;
                    }
                    completed = &mut finished_rx => {
                        // Sender dropped means the caller was interrupted before it
                        // could confirm normal completion; clean up defensively.
                        if completed.is_err() {
                            cancellation_forwarder
                                .cancel_remote_task(&peer_for_cancellation, &task_id_for_cancellation)
                                .await;
                        }
                    }
                }
            });

            let result = tokio::select! {
                result = self.consume_remote_task(&peer, &accepted.task_id) => result,
                () = cancellation.cancelled() => {
                    // The remote node owns the actual inference/tool task. Tell it to
                    // stop before returning locally so Stop does not merely hide a
                    // still-running federated workload.
                    self.cancel_remote_task(&peer, &accepted.task_id).await;
                    anyhow::bail!("Federated subtask cancelled by caller");
                }
            };
            let _ = finished_tx.send(());
            result
        } else {
            self.consume_remote_task(&peer, &accepted.task_id).await
        }
    }

    pub async fn start_remote_task(
        &self,
        peer: &FederationPeerTarget,
        request: &FederationTaskRequest,
    ) -> anyhow::Result<FederationTaskAccepted> {
        let response = with_federation_auth(
            self.client
                .post(format!("{}/federation/tasks", peer.base_url)),
        )
        .timeout(Duration::from_secs(FEDERATION_CONTROL_TIMEOUT_SECS))
        .json(request)
        .send()
        .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Remote worker '{}' rejected task ({}): {}",
                peer.display_name,
                status,
                body
            );
        }

        if let Some(task_id) = response
            .headers()
            .get(FEDERATION_TASK_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(FederationTaskAccepted {
                task_id: task_id.to_string(),
                status: "accepted".to_string(),
            });
        }

        // Compatibility with peers predating the acceptance header.
        Ok(response.json::<FederationTaskAccepted>().await?)
    }

    pub async fn fetch_capabilities(
        &self,
        base_url: &str,
    ) -> anyhow::Result<FederationCapabilities> {
        fetch_capabilities(&self.client, base_url).await
    }

    pub async fn cancel_remote_task(&self, peer: &FederationPeerTarget, task_id: &str) {
        let _ = with_federation_auth(self.client.post(format!(
            "{}/federation/tasks/{task_id}/cancel",
            peer.base_url
        )))
        .timeout(Duration::from_secs(FEDERATION_CONTROL_TIMEOUT_SECS))
        .send()
        .await;
    }

    pub async fn consume_remote_task(
        &self,
        peer: &FederationPeerTarget,
        task_id: &str,
    ) -> anyhow::Result<ToolResult> {
        let stream_url = format!("{}/federation/tasks/{task_id}/stream", peer.base_url);
        let mut final_response = None;
        let mut failure_message = None;
        let mut measured_metrics = None;

        let stream_future = self.stream_remote_task_events(&stream_url, |event| {
            self.emit_chat_event(peer, task_id, event.clone());

            match event.event_type.as_str() {
                "done" => {
                    final_response = event.full_response.clone().or(event.content.clone());
                }
                "error" => {
                    failure_message = event.message.clone().or(event.output.clone());
                }
                "metrics" => {
                    measured_metrics = event.metrics.clone().and_then(|value| {
                        serde_json::from_value::<InferenceMetricsDelta>(value).ok()
                    });
                }
                _ => {}
            }
        });

        match self.task_timeout {
            Some(limit) => match tokio::time::timeout(limit, stream_future).await {
                Ok(result) => result?,
                Err(_) => {
                    // The peer didn't finish in time. Ask it to stop rather than
                    // leaving an unbounded task running unmonitored — the caller
                    // gave up waiting, so nothing is watching for its completion.
                    self.cancel_remote_task(peer, task_id).await;
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Remote worker '{}' did not respond within {}s — it may be \
                             overloaded or stuck. The task was cancelled on that peer.",
                            peer.display_name,
                            limit.as_secs()
                        )),
                    });
                }
            },
            None => stream_future.await?,
        }

        if let Some(message) = failure_message {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Remote worker '{}' failed: {message}",
                    peer.display_name
                )),
            });
        }

        let rendered = final_response.unwrap_or_else(|| "[Empty response]".to_string());
        let telemetry = measured_metrics
            .as_ref()
            .and_then(format_inference_metrics_summary)
            .map(|summary| format!(" · {summary}"))
            .unwrap_or_default();
        Ok(ToolResult {
            success: true,
            output: format!(
                "[Remote worker '{}' ({}){telemetry}]\n{rendered}",
                peer.display_name, peer.base_url,
            ),
            error: None,
        })
    }

    fn emit_chat_event(
        &self,
        peer: &FederationPeerTarget,
        task_id: &str,
        event: FederationTaskEvent,
    ) {
        let Some(context) = current_chat_context() else {
            return;
        };
        let Some(tx) = context.event_tx else {
            return;
        };

        let payload = FederationChatEvent {
            event_type: format!("federation_{}", event.event_type),
            session_id: context.session_id,
            peer_id: peer.peer_id.clone(),
            peer_name: peer.display_name.clone(),
            delegate_agent: peer.delegate_agent.clone(),
            task_id: task_id.to_string(),
            content: event.content,
            name: event.name,
            args: event.args,
            success: event.success,
            duration_secs: event.duration_secs,
            output: event.output,
            message: event
                .message
                .or(event.full_response)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            metrics: event.metrics,
        };

        let _ = tx.send(payload);
    }

    async fn stream_remote_task_events<F>(&self, url: &str, mut on_event: F) -> anyhow::Result<()>
    where
        F: FnMut(FederationTaskEvent),
    {
        let response = with_federation_auth(self.client.get(url)).send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Remote task stream failed ({}): {body}", status);
        }

        let mut data_buffer = String::new();
        let mut line_buffer = String::new();
        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            line_buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(position) = line_buffer.find('\n') {
                let line = line_buffer[..position].trim_end_matches('\r').to_string();
                line_buffer.drain(..=position);

                if line.is_empty() {
                    if !data_buffer.trim().is_empty() {
                        let event =
                            serde_json::from_str::<FederationTaskEvent>(data_buffer.trim())?;
                        on_event(event);
                        data_buffer.clear();
                    }
                    continue;
                }

                if let Some(data) = line.strip_prefix("data:") {
                    data_buffer.push_str(data.trim_start());
                    data_buffer.push('\n');
                }
            }
        }

        if !data_buffer.trim().is_empty() {
            let event = serde_json::from_str::<FederationTaskEvent>(data_buffer.trim())?;
            on_event(event);
        }

        Ok(())
    }
}

pub async fn fetch_capabilities(
    client: &reqwest::Client,
    base_url: &str,
) -> anyhow::Result<FederationCapabilities> {
    let response = with_federation_auth(client.get(format!("{base_url}/federation/capabilities")))
        .timeout(Duration::from_secs(FEDERATION_CONTROL_TIMEOUT_SECS))
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(
            "Federation capabilities request failed ({}): {body}",
            status
        );
    }

    Ok(response.json::<FederationCapabilities>().await?)
}

struct FederationRunningTask {
    handle: JoinHandle<()>,
    cancellation: CancellationToken,
}

struct FederationTaskState {
    events: Vec<FederationTaskEvent>,
    sender: broadcast::Sender<FederationTaskEvent>,
    running: Option<FederationRunningTask>,
}

#[derive(Clone)]
pub struct FederationTaskManager {
    tasks: Arc<RwLock<HashMap<String, FederationTaskState>>>,
}

impl FederationTaskManager {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn create_task(&self, task_id: &str) {
        let mut tasks = self.tasks.write();
        tasks.entry(task_id.to_string()).or_insert_with(|| {
            let (sender, _) = broadcast::channel(128);
            FederationTaskState {
                events: Vec::new(),
                sender,
                running: None,
            }
        });
    }

    pub fn publish(&self, task_id: &str, event: FederationTaskEvent) {
        let mut tasks = self.tasks.write();
        let entry = tasks.entry(task_id.to_string()).or_insert_with(|| {
            let (sender, _) = broadcast::channel(128);
            FederationTaskState {
                events: Vec::new(),
                sender,
                running: None,
            }
        });

        entry.events.push(event.clone());
        if entry.events.len() > TASK_EVENT_HISTORY_LIMIT {
            let drain = entry.events.len() - TASK_EVENT_HISTORY_LIMIT;
            entry.events.drain(..drain);
        }
        let _ = entry.sender.send(event);
    }

    pub fn set_running_handle(
        &self,
        task_id: &str,
        handle: JoinHandle<()>,
        cancellation: CancellationToken,
    ) {
        let mut tasks = self.tasks.write();
        if let Some(entry) = tasks.get_mut(task_id) {
            entry.running = Some(FederationRunningTask {
                handle,
                cancellation,
            });
        }
    }

    pub fn stream(
        &self,
        task_id: &str,
    ) -> Option<(
        Vec<FederationTaskEvent>,
        broadcast::Receiver<FederationTaskEvent>,
    )> {
        let tasks = self.tasks.read();
        let entry = tasks.get(task_id)?;
        Some((entry.events.clone(), entry.sender.subscribe()))
    }

    pub fn cancel(&self, task_id: &str) -> bool {
        let mut tasks = self.tasks.write();
        let Some(entry) = tasks.get_mut(task_id) else {
            return false;
        };

        if let Some(running) = entry.running.take() {
            running.cancellation.cancel();
            running.handle.abort();
            entry.events.push(FederationTaskEvent::error(
                task_id,
                "Task cancelled by remote controller",
            ));
            let _ = entry.sender.send(FederationTaskEvent::error(
                task_id,
                "Task cancelled by remote controller",
            ));
            true
        } else {
            false
        }
    }
}

impl Default for FederationTaskManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FederationRole;
    use crate::federation::peer_registry::FederationPeerTarget;
    use parking_lot::RwLock;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn test_adapter(connect_timeout: Duration) -> FederationRemoteSubagentAdapter {
        test_adapter_with_task_timeout(connect_timeout, None)
    }

    fn test_adapter_with_task_timeout(
        connect_timeout: Duration,
        task_timeout: Option<Duration>,
    ) -> FederationRemoteSubagentAdapter {
        FederationRemoteSubagentAdapter {
            registry: Arc::new(FederationPeerRegistry::new(
                Duration::from_secs(30),
                FederationRole::Worker,
            )),
            client: build_federation_http_client(connect_timeout).expect("build client"),
            local_node_id: Arc::new(RwLock::new("node-a".to_string())),
            local_node_name: Arc::new(RwLock::new("Node A".to_string())),
            task_timeout,
        }
    }

    #[tokio::test]
    async fn federation_task_start_uses_header_without_waiting_for_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock federation worker");
        let address = listener.local_addr().expect("mock worker address");

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept request");
            let mut request = vec![0_u8; 4096];
            let _ = socket.read(&mut request).await.expect("read request");
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\n\
                      content-type: application/json\r\n\
                      x-llamafarm-task-id: task-from-header\r\n\
                      content-length: 1024\r\n\
                      connection: close\r\n\r\n{",
                )
                .await
                .expect("write response headers and partial body");
            tokio::time::sleep(Duration::from_millis(250)).await;
        });

        let adapter = test_adapter(Duration::from_secs(1));
        let peer = FederationPeerTarget {
            peer_id: "peer-1".to_string(),
            node_id: "node-b".to_string(),
            display_name: "Node B".to_string(),
            delegate_agent: "peer_node_b".to_string(),
            base_url: format!("http://{address}"),
            online: true,
            role_support: FederationRole::Worker,
            assigned_role: FederationRole::Worker,
            allow_remote_subagents: true,
        };
        let request = FederationTaskRequest {
            prompt: "respond".to_string(),
            context: None,
            session_id: None,
            requester_node_id: None,
            requester_name: None,
            agentic: false,
            max_iterations: 1,
        };

        let accepted = tokio::time::timeout(
            Duration::from_millis(100),
            adapter.start_remote_task(&peer, &request),
        )
        .await
        .expect("task start must not wait for the response body")
        .expect("task accepted");

        assert_eq!(accepted.task_id, "task-from-header");
        assert_eq!(accepted.status, "accepted");
        server.await.expect("mock worker task");
    }

    #[tokio::test]
    async fn federation_stream_can_outlive_connect_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock federation worker");
        let address = listener.local_addr().expect("mock worker address");

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept request");
            let mut request = vec![0_u8; 4096];
            let _ = socket.read(&mut request).await.expect("read request");
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\n\
                      content-type: text/event-stream\r\n\
                      connection: close\r\n\r\n",
                )
                .await
                .expect("write response headers");

            tokio::time::sleep(Duration::from_millis(100)).await;
            let event = FederationTaskEvent::done("task-1", "stream completed");
            let payload = serde_json::to_string(&event).expect("serialize event");
            socket
                .write_all(format!("data: {payload}\n\n").as_bytes())
                .await
                .expect("write delayed event");
        });

        let adapter = test_adapter(Duration::from_millis(25));
        let mut events = Vec::new();

        adapter
            .stream_remote_task_events(
                &format!("http://{address}/federation/tasks/task-1/stream"),
                |event| events.push(event),
            )
            .await
            .expect("stream must not inherit the shorter connect timeout");
        server.await.expect("mock worker task");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "done");
        assert_eq!(events[0].full_response.as_deref(), Some("stream completed"));
    }

    #[tokio::test]
    async fn consume_remote_task_times_out_and_cancels_stuck_peer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock federation worker");
        let address = listener.local_addr().expect("mock worker address");
        let cancel_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_called_server = cancel_called.clone();

        let server = tokio::spawn(async move {
            // First connection: the stream. Send headers, then never send a
            // terminal event — this simulates an overloaded/stuck peer.
            let (mut stream_socket, _) = listener.accept().await.expect("accept stream request");
            let mut request = vec![0_u8; 4096];
            let _ = stream_socket
                .read(&mut request)
                .await
                .expect("read stream request");
            stream_socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\n\
                      content-type: text/event-stream\r\n\
                      connection: close\r\n\r\n",
                )
                .await
                .expect("write stream response headers");

            // Second connection: the cancel call the timeout path must make.
            let (mut cancel_socket, _) = listener.accept().await.expect("accept cancel request");
            let mut cancel_request = vec![0_u8; 4096];
            let _ = cancel_socket
                .read(&mut cancel_request)
                .await
                .expect("read cancel request");
            cancel_called_server.store(true, std::sync::atomic::Ordering::SeqCst);
            cancel_socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
                )
                .await
                .expect("write cancel response");

            // Keep the stream connection alive past the test's timeout so the
            // hang is real, not just a fast connection close.
            tokio::time::sleep(Duration::from_secs(2)).await;
            drop(stream_socket);
        });

        let adapter = test_adapter_with_task_timeout(
            Duration::from_secs(1),
            Some(Duration::from_millis(150)),
        );
        let peer = FederationPeerTarget {
            peer_id: "peer-1".to_string(),
            node_id: "node-b".to_string(),
            display_name: "Node B".to_string(),
            delegate_agent: "peer_node_b".to_string(),
            base_url: format!("http://{address}"),
            online: true,
            role_support: FederationRole::Worker,
            assigned_role: FederationRole::Worker,
            allow_remote_subagents: true,
        };

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            adapter.consume_remote_task(&peer, "task-1"),
        )
        .await
        .expect("consume_remote_task must not hang past its own configured timeout")
        .expect("timeout path returns a ToolResult, not an Err");

        assert!(!result.success);
        let error = result.error.expect("timeout must report an error");
        assert!(error.contains("did not respond within"), "{error}");
        assert!(
            cancel_called.load(std::sync::atomic::Ordering::SeqCst),
            "timing out must cancel the stuck task on the peer"
        );

        server.await.expect("mock worker task");
    }
}
