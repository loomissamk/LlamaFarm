pub mod peer_registry;
pub mod remote_subagent;

use self::peer_registry::{
    FederationDiscoveredPeer, FederationLocalNodeSummary, FederationPeerRegistry,
    FederationPeerSource, FederationPeersResponse, FederationToolCapability,
};
use self::remote_subagent::FederationRemoteSubagentAdapter;
use crate::config::{FederationConfig, FederationDiscoveryMode, FederationRole};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Clone)]
struct FederationLocalNodeState {
    node_id: String,
    display_name: String,
    api_port: u16,
    role: FederationRole,
    allow_remote_subagents: bool,
    discovery_mode: FederationDiscoveryMode,
    service_name: String,
    default_model: String,
    tool_names: Vec<String>,
}

pub struct FederationService {
    config: FederationConfig,
    registry: Arc<FederationPeerRegistry>,
    remote_adapter: Arc<FederationRemoteSubagentAdapter>,
    local_state: Arc<RwLock<FederationLocalNodeState>>,
    mdns_daemon: Mutex<Option<ServiceDaemon>>,
    started: AtomicBool,
}

impl FederationService {
    pub fn new(
        config: &FederationConfig,
        gateway_port: u16,
        config_dir: Option<&std::path::Path>,
    ) -> anyhow::Result<Option<Arc<Self>>> {
        if !config.enabled {
            return Ok(None);
        }

        let api_port = config.api_port.unwrap_or(gateway_port);
        let machine_uuid = load_or_create_machine_uuid(config_dir);
        let node_id = build_node_id(&config.node_name, api_port, &machine_uuid);
        let registry = Arc::new(FederationPeerRegistry::new(
            Duration::from_secs(config.peer_timeout_seconds.max(1)),
            config.default_role,
        ));
        let local_node_id = Arc::new(RwLock::new(node_id.clone()));
        let local_node_name = Arc::new(RwLock::new(config.node_name.clone()));
        let remote_adapter = Arc::new(FederationRemoteSubagentAdapter::new(
            registry.clone(),
            local_node_id.clone(),
            local_node_name.clone(),
        )?);

        let service = Arc::new(Self {
            config: config.clone(),
            registry,
            remote_adapter,
            local_state: Arc::new(RwLock::new(FederationLocalNodeState {
                node_id,
                display_name: config.node_name.clone(),
                api_port,
                role: config.default_role,
                allow_remote_subagents: config.allow_remote_subagents,
                discovery_mode: config.discovery_mode,
                service_name: config.service_name.clone(),
                default_model: String::new(),
                tool_names: Vec::new(),
            })),
            mdns_daemon: Mutex::new(None),
            started: AtomicBool::new(false),
        });

        for peer in &config.manual_peers {
            if let Some((base_url, host, port)) = normalize_peer_endpoint(peer) {
                service
                    .registry
                    .seed_manual_peer(base_url.clone(), base_url, host, port);
            }
        }

        Ok(Some(service))
    }

    pub fn remote_adapter(&self) -> Arc<FederationRemoteSubagentAdapter> {
        self.remote_adapter.clone()
    }

    pub fn registry(&self) -> Arc<FederationPeerRegistry> {
        self.registry.clone()
    }

    pub fn local_node_summary(&self) -> FederationLocalNodeSummary {
        let local = self.local_state.read();
        FederationLocalNodeSummary {
            node_id: local.node_id.clone(),
            display_name: local.display_name.clone(),
            api_port: local.api_port,
            role: local.role,
            allow_remote_subagents: local.allow_remote_subagents,
            discovery_mode: local.discovery_mode,
            service_name: local.service_name.clone(),
            gateway_host: String::new(),
        }
    }

    pub fn peers_response(&self) -> FederationPeersResponse {
        self.registry
            .peers_response(self.config.enabled, self.local_node_summary())
    }

    pub fn set_assigned_role(
        &self,
        peer_id: &str,
        role: FederationRole,
    ) -> Option<peer_registry::FederationPeerSummary> {
        self.registry.set_assigned_role(peer_id, role)
    }

    pub fn set_peer_hints(
        &self,
        peer_id: &str,
        specialization: String,
        priority: i32,
    ) -> Option<peer_registry::FederationPeerSummary> {
        self.registry.set_peer_hints(peer_id, specialization, priority)
    }

    pub fn update_local_runtime(&self, gateway_port: u16, default_model: &str, tool_names: &[String]) {
        let mut local = self.local_state.write();
        local.api_port = self.config.api_port.unwrap_or(gateway_port);
        local.default_model = default_model.to_string();
        local.tool_names = tool_names.to_vec();
    }

    pub fn start(&self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }

        self.spawn_refresh_loop();
        if matches!(self.config.discovery_mode, FederationDiscoveryMode::Mdns) {
            if let Err(error) = self.start_mdns() {
                tracing::warn!("federation mdns startup failed: {error}");
            }
        }
    }

    fn spawn_refresh_loop(&self) {
        let registry = self.registry.clone();
        let adapter = self.remote_adapter.clone();
        let interval_secs = (self.config.peer_timeout_seconds / 3).clamp(5, 15);

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
            loop {
                interval.tick().await;
                registry.mark_stale_offline();

                for target in registry.refresh_targets() {
                    match adapter.fetch_capabilities(&target.base_url).await {
                        Ok(capabilities) => {
                            registry.apply_capabilities(
                                &target.base_url,
                                target.source,
                                capabilities,
                            );
                        }
                        Err(error) => {
                            tracing::debug!(
                                peer_id = %target.peer_id,
                                base_url = %target.base_url,
                                error = %error,
                                "federation peer refresh failed"
                            );
                        }
                    }
                }
            }
        });
    }

    fn start_mdns(&self) -> anyhow::Result<()> {
        let daemon = ServiceDaemon::new()?;
        let service_type = mdns_service_type(&self.config.service_name);
        let local = self.local_state.read().clone();
        let hostname = format!("{}.local.", sanitize_dns_label(&local.node_id));
        let tool_summary = summarize_tool_names(&local.tool_names);
        let properties = HashMap::from([
            ("node_id".to_string(), local.node_id.clone()),
            ("display_name".to_string(), local.display_name.clone()),
            ("api_port".to_string(), local.api_port.to_string()),
            ("app_version".to_string(), env!("CARGO_PKG_VERSION").to_string()),
            (
                "role".to_string(),
                format!("{:?}", local.role).to_ascii_lowercase(),
            ),
            ("model".to_string(), local.default_model.clone()),
            ("tools".to_string(), tool_summary),
            (
                "allow_remote_subagents".to_string(),
                local.allow_remote_subagents.to_string(),
            ),
        ]);

        let service = ServiceInfo::new(
            &service_type,
            &local.display_name,
            &hostname,
            "",
            local.api_port,
            properties,
        )?
        .enable_addr_auto();

        daemon.register(service)?;
        let browse = daemon.browse(&service_type)?;
        let local_node_id = local.node_id.clone();
        let registry = self.registry.clone();

        std::thread::Builder::new()
            .name("llamafarm-federation-mdns".to_string())
            .spawn(move || {
                while let Ok(event) = browse.recv() {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            if let Some(peer) =
                                discovered_peer_from_service(info.as_ref(), &local_node_id)
                            {
                                registry.upsert_discovered_peer(peer);
                            }
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            registry.mark_mdns_removed(&fullname);
                        }
                        _ => {}
                    }
                }
            })?;

        *self.mdns_daemon.lock() = Some(daemon);
        Ok(())
    }
}

pub fn delegate_agent_name(display_name: &str, peer_id: &str) -> String {
    let sanitized = sanitize_token(display_name);
    let suffix = short_hash(peer_id);
    format!("peer_{sanitized}_{suffix}")
}

fn build_node_id(node_name: &str, api_port: u16, machine_uuid: &str) -> String {
    format!("{}-{}-{}", sanitize_token(node_name), api_port, &machine_uuid[..8])
}

fn load_or_create_machine_uuid(config_dir: Option<&std::path::Path>) -> String {
    let Some(dir) = config_dir else {
        return Uuid::new_v4().to_string();
    };
    let path = dir.join("federation_node_id");
    if let Ok(contents) = std::fs::read_to_string(&path) {
        let trimmed = contents.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }
    let id = Uuid::new_v4().to_string();
    let _ = std::fs::write(&path, &id);
    id
}

fn sanitize_token(raw: &str) -> String {
    let mut value = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    value = value.trim_matches('_').to_string();
    if value.is_empty() {
        "worker".to_string()
    } else {
        value
    }
}

fn sanitize_dns_label(raw: &str) -> String {
    let mut value = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    value = value.trim_matches('-').to_string();
    if value.is_empty() {
        "llamafarm".to_string()
    } else {
        value
    }
}

fn short_hash(raw: &str) -> String {
    let mut hash = 0_u64;
    for byte in raw.bytes() {
        hash = hash.wrapping_mul(131).wrapping_add(u64::from(byte));
    }
    format!("{:06x}", hash & 0x00ff_ffff)
}

fn mdns_service_type(service_name: &str) -> String {
    let trimmed = service_name.trim().trim_end_matches('.');
    if trimmed.ends_with(".local") {
        format!("{trimmed}.")
    } else {
        format!("{trimmed}.local.")
    }
}

fn summarize_tool_names(tool_names: &[String]) -> String {
    if tool_names.is_empty() {
        return String::new();
    }

    let mut names = tool_names.iter().map(String::as_str).collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();

    if names.len() <= 6 {
        names.join(", ")
    } else {
        format!("{}, +{} more", names[..6].join(", "), names.len() - 6)
    }
}

fn discovered_peer_from_service(
    service: &mdns_sd::ResolvedService,
    local_node_id: &str,
) -> Option<FederationDiscoveredPeer> {
    let node_id = service
        .txt_properties
        .get_property_val_str("node_id")
        .unwrap_or(service.get_fullname())
        .trim()
        .to_string();
    if node_id == local_node_id {
        return None;
    }

    let display_name = service
        .txt_properties
        .get_property_val_str("display_name")
        .unwrap_or(service.get_fullname())
        .trim()
        .to_string();
    let api_port = service
        .txt_properties
        .get_property_val_str("api_port")
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(service.get_port());
    let host_ip = service
        .get_addresses()
        .iter()
        .find_map(|address| (!address.is_loopback()).then(|| address.to_ip_addr().to_string()))
        .or_else(|| {
            service
                .get_addresses()
                .iter()
                .next()
                .map(|address| address.to_ip_addr().to_string())
        })?;

    let role_support = match service
        .txt_properties
        .get_property_val_str("role")
        .unwrap_or("both")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "master" => FederationRole::Master,
        "worker" => FederationRole::Worker,
        "disabled" => FederationRole::Disabled,
        _ => FederationRole::Both,
    };

    let allow_remote_subagents = service
        .txt_properties
        .get_property_val_str("allow_remote_subagents")
        .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(true);
    let model_summary = service
        .txt_properties
        .get_property_val_str("model")
        .unwrap_or_default()
        .trim()
        .to_string();
    let tool_summary = service
        .txt_properties
        .get_property_val_str("tools")
        .unwrap_or_default()
        .trim()
        .to_string();

    Some(FederationDiscoveredPeer {
        peer_id: node_id.clone(),
        node_id: node_id.clone(),
        display_name: display_name.clone(),
        delegate_agent: delegate_agent_name(&display_name, &node_id),
        mdns_fullname: Some(service.get_fullname().to_string()),
        source: FederationPeerSource::Mdns,
        base_url: format!("http://{host_ip}:{api_port}"),
        host: host_ip,
        api_port,
        app_version: service
            .txt_properties
            .get_property_val_str("app_version")
            .unwrap_or_default()
            .trim()
            .to_string(),
        role_support,
        allow_remote_subagents,
        model_summary,
        tool_summary,
    })
}

pub fn local_only_tool(name: &str) -> bool {
    matches!(
        name,
        "shell"
            | "process"
            | "file_read"
            | "file_write"
            | "file_edit"
            | "apply_patch"
            | "git_operations"
            | "docker"
            | "service_control"
            | "browser"
            | "browser_open"
            | "package_manager"
            | "hardware_board_info"
            | "hardware_memory_map"
            | "hardware_memory_read"
    )
}

pub fn normalize_peer_endpoint(raw: &str) -> Option<(String, String, u16)> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    let normalized = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };

    let without_scheme = normalized
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .to_string();
    let authority = without_scheme.split('/').next().unwrap_or_default();
    let (host, port) = authority.rsplit_once(':')?;
    let port = port.parse::<u16>().ok()?;
    Some((normalized, host.to_string(), port))
}

pub fn tools_to_capabilities(tools: &[crate::tools::ToolSpec]) -> Vec<FederationToolCapability> {
    tools.iter()
        .map(|tool| FederationToolCapability {
            name: tool.name.clone(),
            description: tool.description.clone(),
            local_only: local_only_tool(&tool.name),
        })
        .collect()
}
