use super::peer_registry::{
    FederationCapabilities, FederationPeerRegistry, FederationPeerTarget,
};
use crate::tools::ToolResult;
use chrono::Utc;
use futures_util::StreamExt;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const TASK_EVENT_HISTORY_LIMIT: usize = 256;
const REMOTE_TASK_START_TIMEOUT_SECS: u64 = 10;

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
}

#[derive(Clone)]
pub struct FederationChatContext {
    pub session_id: String,
    pub selected_peer_ids: Vec<String>,
    pub event_tx: Option<mpsc::UnboundedSender<FederationChatEvent>>,
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
    FEDERATION_CHAT_CONTEXT.try_with(Clone::clone).ok().flatten()
}

#[derive(Clone)]
pub struct FederationRemoteSubagentAdapter {
    registry: Arc<FederationPeerRegistry>,
    client: reqwest::Client,
    local_node_id: Arc<RwLock<String>>,
    local_node_name: Arc<RwLock<String>>,
}

impl FederationRemoteSubagentAdapter {
    pub fn new(
        registry: Arc<FederationPeerRegistry>,
        local_node_id: Arc<RwLock<String>>,
        local_node_name: Arc<RwLock<String>>,
    ) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REMOTE_TASK_START_TIMEOUT_SECS))
            .build()?;

        Ok(Self {
            registry,
            client,
            local_node_id,
            local_node_name,
        })
    }

    pub fn http_client(&self) -> reqwest::Client {
        self.client.clone()
    }

    pub fn available_remote_agents(&self) -> Vec<String> {
        current_chat_context()
            .map(|context| {
                self.registry
                    .available_remote_agent_names(&context.selected_peer_ids)
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
            max_iterations: 12,
        };

        let accepted = self.start_remote_task(&peer, &request).await?;
        self.emit_chat_event(&peer, &accepted.task_id, FederationTaskEvent::status(&accepted.task_id, format!("Delegated to {}", peer.display_name)));

        self.consume_remote_task(&peer, &accepted.task_id).await
    }

    pub async fn start_remote_task(
        &self,
        peer: &FederationPeerTarget,
        request: &FederationTaskRequest,
    ) -> anyhow::Result<FederationTaskAccepted> {
        let response = self
            .client
            .post(format!("{}/federation/tasks", peer.base_url))
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

        Ok(response.json::<FederationTaskAccepted>().await?)
    }

    pub async fn fetch_capabilities(&self, base_url: &str) -> anyhow::Result<FederationCapabilities> {
        fetch_capabilities(&self.client, base_url).await
    }

    pub async fn cancel_remote_task(&self, peer: &FederationPeerTarget, task_id: &str) {
        let _ = self
            .client
            .post(format!("{}/federation/tasks/{task_id}/cancel", peer.base_url))
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

        self.stream_remote_task_events(&stream_url, |event| {
            self.emit_chat_event(peer, task_id, event.clone());

            match event.event_type.as_str() {
                "done" => {
                    final_response = event.full_response.clone().or(event.content.clone());
                }
                "error" => {
                    failure_message = event.message.clone().or(event.output.clone());
                }
                _ => {}
            }
        })
        .await?;

        if let Some(message) = failure_message {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Remote worker '{}' failed: {message}", peer.display_name)),
            });
        }

        let rendered = final_response.unwrap_or_else(|| "[Empty response]".to_string());
        Ok(ToolResult {
            success: true,
            output: format!("[Remote worker '{}' ({})]\n{rendered}", peer.display_name, peer.base_url),
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
        };

        let _ = tx.send(payload);
    }

    async fn stream_remote_task_events<F>(&self, url: &str, mut on_event: F) -> anyhow::Result<()>
    where
        F: FnMut(FederationTaskEvent),
    {
        let response = self.client.get(url).send().await?;
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
                        let event = serde_json::from_str::<FederationTaskEvent>(data_buffer.trim())?;
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
    let response = client
        .get(format!("{base_url}/federation/capabilities"))
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Federation capabilities request failed ({}): {body}", status);
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
            entry.running = Some(FederationRunningTask { handle, cancellation });
        }
    }

    pub fn stream(
        &self,
        task_id: &str,
    ) -> Option<(Vec<FederationTaskEvent>, broadcast::Receiver<FederationTaskEvent>)> {
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
