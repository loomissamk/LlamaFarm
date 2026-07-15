use crate::config::{FederationDiscoveryMode, FederationRole};
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Lightweight info about a ready remote agent, used to build system prompt guidance.
#[derive(Debug, Clone)]
pub struct RemoteAgentInfo {
    pub agent_name: String,
    pub specialization: String,
    pub priority: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FederationPeerSource {
    Mdns,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationToolCapability {
    pub name: String,
    pub description: String,
    pub local_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationCapabilities {
    pub node_id: String,
    pub display_name: String,
    pub app_version: String,
    pub provider: Option<String>,
    pub model: String,
    pub installed_models: Vec<String>,
    pub tools: Vec<FederationToolCapability>,
    pub role_support: FederationRole,
    pub allow_remote_subagents: bool,
    pub health: String,
    pub api_port: u16,
    pub last_seen: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationLocalNodeSummary {
    pub node_id: String,
    pub display_name: String,
    pub api_port: u16,
    pub role: FederationRole,
    pub allow_remote_subagents: bool,
    pub discovery_mode: FederationDiscoveryMode,
    pub service_name: String,
    /// The host the gateway is bound to (e.g. 127.0.0.1 or 0.0.0.0).
    #[serde(default)]
    pub gateway_host: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationPeerSummary {
    pub peer_id: String,
    pub node_id: String,
    pub display_name: String,
    pub delegate_agent: String,
    pub source: FederationPeerSource,
    pub base_url: String,
    pub host: String,
    pub api_port: u16,
    pub app_version: String,
    pub role_support: FederationRole,
    pub assigned_role: FederationRole,
    pub allow_remote_subagents: bool,
    pub online: bool,
    pub health: String,
    pub model_summary: String,
    pub tool_summary: String,
    pub installed_models: Vec<String>,
    pub tools: Vec<FederationToolCapability>,
    pub last_seen: Option<String>,
    pub specialization: String,
    pub priority: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationPeersResponse {
    pub enabled: bool,
    pub local_node: FederationLocalNodeSummary,
    pub peers: Vec<FederationPeerSummary>,
}

#[derive(Debug, Clone)]
pub struct FederationPeerTarget {
    pub peer_id: String,
    pub node_id: String,
    pub display_name: String,
    pub delegate_agent: String,
    pub base_url: String,
    pub online: bool,
    pub role_support: FederationRole,
    pub assigned_role: FederationRole,
    pub allow_remote_subagents: bool,
}

#[derive(Debug, Clone)]
pub struct FederationRefreshTarget {
    pub peer_id: String,
    pub base_url: String,
    pub source: FederationPeerSource,
}

#[derive(Debug, Clone)]
pub struct FederationDiscoveredPeer {
    pub peer_id: String,
    pub node_id: String,
    pub display_name: String,
    pub delegate_agent: String,
    pub mdns_fullname: Option<String>,
    pub source: FederationPeerSource,
    pub base_url: String,
    pub host: String,
    pub api_port: u16,
    pub app_version: String,
    pub role_support: FederationRole,
    pub allow_remote_subagents: bool,
    pub model_summary: String,
    pub tool_summary: String,
}

#[derive(Debug, Clone)]
struct FederationPeerRecord {
    peer_id: String,
    node_id: String,
    display_name: String,
    delegate_agent: String,
    mdns_fullname: Option<String>,
    source: FederationPeerSource,
    base_url: String,
    host: String,
    api_port: u16,
    app_version: String,
    role_support: FederationRole,
    assigned_role: FederationRole,
    allow_remote_subagents: bool,
    online: bool,
    health: String,
    model_summary: String,
    tool_summary: String,
    installed_models: Vec<String>,
    tools: Vec<FederationToolCapability>,
    last_seen: Option<DateTime<Utc>>,
    /// Free-text hint injected into the system prompt so the LLM knows what to send here.
    specialization: String,
    /// Higher priority peers are listed first in the system prompt (default 0).
    priority: i32,
}

#[derive(Clone)]
pub struct FederationPeerRegistry {
    peers: Arc<RwLock<HashMap<String, FederationPeerRecord>>>,
    peer_timeout: Duration,
    default_role: FederationRole,
}

impl FederationPeerRegistry {
    pub fn new(peer_timeout: Duration, default_role: FederationRole) -> Self {
        Self {
            peers: Arc::new(RwLock::new(HashMap::new())),
            peer_timeout,
            default_role,
        }
    }

    pub fn seed_manual_peer(&self, peer_id: String, base_url: String, host: String, api_port: u16) {
        let mut peers = self.peers.write();
        peers
            .entry(peer_id.clone())
            .or_insert(FederationPeerRecord {
                peer_id,
                node_id: String::new(),
                display_name: host.clone(),
                delegate_agent: super::delegate_agent_name(&host, &base_url),
                mdns_fullname: None,
                source: FederationPeerSource::Manual,
                base_url,
                host,
                api_port,
                app_version: String::new(),
                role_support: FederationRole::Both,
                assigned_role: self.default_role,
                allow_remote_subagents: true,
                online: false,
                health: "offline".to_string(),
                model_summary: String::new(),
                tool_summary: String::new(),
                installed_models: Vec::new(),
                tools: Vec::new(),
                last_seen: None,
                specialization: String::new(),
                priority: 0,
            });
    }

    pub fn upsert_discovered_peer(&self, discovered: FederationDiscoveredPeer) {
        let mut peers = self.peers.write();
        let record = peers.values_mut().find(|peer| {
            (!discovered.node_id.is_empty() && peer.node_id == discovered.node_id)
                || peer.base_url == discovered.base_url
                || peer.peer_id == discovered.peer_id
        });

        let now = Utc::now();
        if let Some(record) = record {
            record.node_id = discovered.node_id.clone();
            record.display_name = discovered.display_name.clone();
            record.delegate_agent = discovered.delegate_agent.clone();
            record.mdns_fullname = discovered.mdns_fullname.clone();
            record.source = discovered.source.clone();
            record.base_url = discovered.base_url.clone();
            record.host = discovered.host.clone();
            record.api_port = discovered.api_port;
            record.app_version = discovered.app_version.clone();
            record.role_support = discovered.role_support;
            record.allow_remote_subagents = discovered.allow_remote_subagents;
            record.model_summary = discovered.model_summary.clone();
            record.tool_summary = discovered.tool_summary.clone();
            record.online = true;
            record.health = "online".to_string();
            record.last_seen = Some(now);
            return;
        }

        peers.insert(
            discovered.peer_id.clone(),
            FederationPeerRecord {
                peer_id: discovered.peer_id,
                node_id: discovered.node_id,
                display_name: discovered.display_name,
                delegate_agent: discovered.delegate_agent,
                mdns_fullname: discovered.mdns_fullname,
                source: discovered.source,
                base_url: discovered.base_url,
                host: discovered.host,
                api_port: discovered.api_port,
                app_version: discovered.app_version,
                role_support: discovered.role_support,
                assigned_role: self.default_role,
                allow_remote_subagents: discovered.allow_remote_subagents,
                online: true,
                health: "online".to_string(),
                model_summary: discovered.model_summary,
                tool_summary: discovered.tool_summary,
                installed_models: Vec::new(),
                tools: Vec::new(),
                last_seen: Some(now),
                specialization: String::new(),
                priority: 0,
            },
        );
    }

    pub fn apply_capabilities(
        &self,
        base_url: &str,
        source: FederationPeerSource,
        capabilities: FederationCapabilities,
    ) {
        let mut peers = self.peers.write();
        let now = Utc::now();
        if let Some(record) = peers.values_mut().find(|peer| {
            (!capabilities.node_id.is_empty() && peer.node_id == capabilities.node_id)
                || peer.base_url == base_url
        }) {
            if !capabilities.node_id.trim().is_empty() {
                record.node_id = capabilities.node_id.clone();
            }
            record.display_name = capabilities.display_name.clone();
            record.delegate_agent = super::delegate_agent_name(
                &capabilities.display_name,
                if capabilities.node_id.trim().is_empty() {
                    base_url
                } else {
                    &capabilities.node_id
                },
            );
            record.source = source;
            record.app_version = capabilities.app_version.clone();
            record.role_support = capabilities.role_support;
            record.allow_remote_subagents = capabilities.allow_remote_subagents;
            record.model_summary = capabilities.model.clone();
            record.tool_summary = summarize_tool_names(&capabilities.tools);
            record.installed_models = capabilities.installed_models.clone();
            record.tools = capabilities.tools.clone();
            record.online = true;
            record.health = capabilities.health.clone();
            record.api_port = capabilities.api_port;
            record.last_seen = Some(now);
            return;
        }

        let peer_id = if capabilities.node_id.trim().is_empty() {
            base_url.to_string()
        } else {
            capabilities.node_id.clone()
        };
        peers.insert(
            peer_id.clone(),
            FederationPeerRecord {
                peer_id: peer_id.clone(),
                node_id: capabilities.node_id.clone(),
                display_name: capabilities.display_name.clone(),
                delegate_agent: super::delegate_agent_name(&capabilities.display_name, &peer_id),
                mdns_fullname: None,
                source,
                base_url: base_url.to_string(),
                host: base_url.to_string(),
                api_port: capabilities.api_port,
                app_version: capabilities.app_version,
                role_support: capabilities.role_support,
                assigned_role: self.default_role,
                allow_remote_subagents: capabilities.allow_remote_subagents,
                online: true,
                health: capabilities.health,
                model_summary: capabilities.model,
                tool_summary: summarize_tool_names(&capabilities.tools),
                installed_models: capabilities.installed_models,
                tools: capabilities.tools,
                last_seen: Some(now),
                specialization: String::new(),
                priority: 0,
            },
        );
    }

    pub fn mark_mdns_removed(&self, fullname: &str) {
        let mut peers = self.peers.write();
        for record in peers.values_mut() {
            if record.mdns_fullname.as_deref() == Some(fullname) {
                record.online = false;
                record.health = "offline".to_string();
            }
        }
    }

    pub fn mark_stale_offline(&self) {
        let mut peers = self.peers.write();
        let now = Utc::now();
        for record in peers.values_mut() {
            if let Some(last_seen) = record.last_seen {
                let elapsed = now.signed_duration_since(last_seen).num_seconds();
                if elapsed > self.peer_timeout.as_secs() as i64 {
                    record.online = false;
                    if record.health != "failed" {
                        record.health = "offline".to_string();
                    }
                }
            }
        }
    }

    pub fn refresh_targets(&self) -> Vec<FederationRefreshTarget> {
        self.peers
            .read()
            .values()
            .filter(|peer| !peer.base_url.trim().is_empty())
            .map(|peer| FederationRefreshTarget {
                peer_id: peer.peer_id.clone(),
                base_url: peer.base_url.clone(),
                source: peer.source.clone(),
            })
            .collect()
    }

    pub fn set_assigned_role(
        &self,
        peer_id: &str,
        role: FederationRole,
    ) -> Option<FederationPeerSummary> {
        let mut peers = self.peers.write();
        let record = peers.get_mut(peer_id)?;
        record.assigned_role = role;
        Some(record_to_summary(record))
    }

    pub fn set_peer_hints(
        &self,
        peer_id: &str,
        specialization: String,
        priority: i32,
    ) -> Option<FederationPeerSummary> {
        let mut peers = self.peers.write();
        let record = peers.get_mut(peer_id)?;
        record.specialization = specialization;
        record.priority = priority;
        Some(record_to_summary(record))
    }

    pub fn selected_worker_peers(&self, peer_ids: &[String]) -> Vec<FederationPeerTarget> {
        let wanted = peer_ids.iter().map(String::as_str).collect::<Vec<_>>();
        self.peers
            .read()
            .values()
            .filter(|peer| wanted.iter().any(|candidate| *candidate == peer.peer_id))
            .filter(|peer| peer.online)
            .filter(|peer| peer.assigned_role.allows_worker())
            .filter(|peer| peer.role_support.allows_worker())
            .filter(|peer| peer.allow_remote_subagents)
            .map(|peer| FederationPeerTarget {
                peer_id: peer.peer_id.clone(),
                node_id: peer.node_id.clone(),
                display_name: peer.display_name.clone(),
                delegate_agent: peer.delegate_agent.clone(),
                base_url: peer.base_url.clone(),
                online: peer.online,
                role_support: peer.role_support,
                assigned_role: peer.assigned_role,
                allow_remote_subagents: peer.allow_remote_subagents,
            })
            .collect()
    }

    pub fn available_remote_agent_names(&self, peer_ids: &[String]) -> Vec<String> {
        self.selected_worker_peers(peer_ids)
            .into_iter()
            .map(|peer| peer.delegate_agent)
            .collect()
    }

    /// Returns agent infos sorted by priority descending (higher priority listed first).
    pub fn available_remote_agent_infos(&self, peer_ids: &[String]) -> Vec<RemoteAgentInfo> {
        let wanted = peer_ids.iter().map(String::as_str).collect::<Vec<_>>();
        let peers = self.peers.read();
        let mut infos: Vec<RemoteAgentInfo> = peers
            .values()
            .filter(|peer| wanted.iter().any(|candidate| *candidate == peer.peer_id))
            .filter(|peer| peer.online)
            .filter(|peer| peer.assigned_role.allows_worker())
            .filter(|peer| peer.role_support.allows_worker())
            .filter(|peer| peer.allow_remote_subagents)
            .map(|peer| RemoteAgentInfo {
                agent_name: peer.delegate_agent.clone(),
                specialization: peer.specialization.clone(),
                priority: peer.priority,
            })
            .collect();
        infos.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.agent_name.cmp(&b.agent_name))
        });
        infos
    }

    pub fn find_peer_by_agent_name(
        &self,
        peer_ids: &[String],
        agent_name: &str,
    ) -> Option<FederationPeerTarget> {
        self.selected_worker_peers(peer_ids)
            .into_iter()
            .find(|peer| peer.delegate_agent == agent_name)
    }

    pub fn peers_response(
        &self,
        enabled: bool,
        local_node: FederationLocalNodeSummary,
    ) -> FederationPeersResponse {
        let mut peers = self
            .peers
            .read()
            .values()
            .map(record_to_summary)
            .collect::<Vec<_>>();

        peers.sort_by(|left, right| {
            right
                .online
                .cmp(&left.online)
                .then_with(|| left.display_name.cmp(&right.display_name))
        });

        FederationPeersResponse {
            enabled,
            local_node,
            peers,
        }
    }
}

fn record_to_summary(record: &FederationPeerRecord) -> FederationPeerSummary {
    FederationPeerSummary {
        peer_id: record.peer_id.clone(),
        node_id: record.node_id.clone(),
        display_name: record.display_name.clone(),
        delegate_agent: record.delegate_agent.clone(),
        source: record.source.clone(),
        base_url: record.base_url.clone(),
        host: record.host.clone(),
        api_port: record.api_port,
        app_version: record.app_version.clone(),
        role_support: record.role_support,
        assigned_role: record.assigned_role,
        allow_remote_subagents: record.allow_remote_subagents,
        online: record.online,
        health: record.health.clone(),
        model_summary: record.model_summary.clone(),
        tool_summary: record.tool_summary.clone(),
        installed_models: record.installed_models.clone(),
        tools: record.tools.clone(),
        last_seen: record.last_seen.map(|value| value.to_rfc3339()),
        specialization: record.specialization.clone(),
        priority: record.priority,
    }
}

fn summarize_tool_names(tools: &[FederationToolCapability]) -> String {
    if tools.is_empty() {
        return String::new();
    }

    let mut names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();

    if names.len() <= 6 {
        names.join(", ")
    } else {
        format!("{}, +{} more", names[..6].join(", "), names.len() - 6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

    fn discovered_peer(peer_id: &str, role_support: FederationRole) -> FederationDiscoveredPeer {
        FederationDiscoveredPeer {
            peer_id: peer_id.to_string(),
            node_id: format!("node-{peer_id}"),
            display_name: format!("Peer {peer_id}"),
            delegate_agent: format!("peer-{peer_id}"),
            mdns_fullname: Some(format!("{peer_id}._llamafarm._tcp.local.")),
            source: FederationPeerSource::Mdns,
            base_url: format!("http://{peer_id}.local:8080"),
            host: format!("{peer_id}.local"),
            api_port: 8080,
            app_version: "0.1.8".to_string(),
            role_support,
            allow_remote_subagents: true,
            model_summary: "llama3".to_string(),
            tool_summary: "shell, file_read".to_string(),
        }
    }

    #[test]
    fn selected_worker_peers_require_online_worker_capable_nodes() {
        let registry = FederationPeerRegistry::new(Duration::from_secs(30), FederationRole::Both);
        registry.upsert_discovered_peer(discovered_peer("alpha", FederationRole::Both));
        registry.upsert_discovered_peer(discovered_peer("beta", FederationRole::Master));
        registry.upsert_discovered_peer(discovered_peer("gamma", FederationRole::Worker));

        registry.set_assigned_role("beta", FederationRole::Worker);
        registry.set_assigned_role("gamma", FederationRole::Disabled);
        registry.mark_mdns_removed("gamma._llamafarm._tcp.local.");

        let selected = registry.selected_worker_peers(&[
            "alpha".to_string(),
            "beta".to_string(),
            "gamma".to_string(),
        ]);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].peer_id, "alpha");
        assert_eq!(selected[0].delegate_agent, "peer-alpha");
    }

    #[test]
    fn mark_stale_offline_marks_expired_peers_offline() {
        let registry = FederationPeerRegistry::new(Duration::from_secs(5), FederationRole::Both);
        registry.upsert_discovered_peer(discovered_peer("alpha", FederationRole::Both));

        {
            let mut peers = registry.peers.write();
            let record = peers.get_mut("alpha").expect("peer should exist");
            record.last_seen = Some(Utc::now() - ChronoDuration::seconds(30));
            record.health = "online".to_string();
        }

        registry.mark_stale_offline();

        let peers = registry.peers_response(
            true,
            FederationLocalNodeSummary {
                node_id: "local-node".to_string(),
                display_name: "Local".to_string(),
                api_port: 8080,
                role: FederationRole::Both,
                allow_remote_subagents: true,
                discovery_mode: FederationDiscoveryMode::Mdns,
                service_name: "_llamafarm._tcp".to_string(),
                gateway_host: "127.0.0.1".to_string(),
            },
        );

        assert_eq!(peers.peers.len(), 1);
        assert!(!peers.peers[0].online);
        assert_eq!(peers.peers[0].health, "offline");
    }
}
