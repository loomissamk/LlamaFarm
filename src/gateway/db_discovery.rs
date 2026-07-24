//! Local-network database discovery.
//!
//! Scans the node's own subnets for reachable database servers on their
//! standard ports so the Database page can reconcile them into connections
//! without making the operator hand-write DSNs.
//!
//! This is a TCP-connect probe only: it never authenticates, never sends
//! payloads, and never reads data. It is confined to the node's own private
//! (RFC1918) subnets — it will not scan the public internet.

use crate::config::{DbConnectionConfig, DbDriver};
use serde::Serialize;
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_millis(300);
const MAX_CONCURRENCY: usize = 128;

/// Well-known database ports worth probing.
const DB_PORTS: &[(u16, &str)] = &[
    (5432, "postgres"),
    (3306, "mysql"),
    (27017, "mongodb"),
    (6333, "qdrant"),
    (6379, "redis"),
    (11434, "ollama"),
];

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredDb {
    pub host: String,
    pub port: u16,
    pub driver: String,
}

/// Internal-only connection URI for a passwordless probe. Discovery responses
/// must never serialize this value.
fn passwordless_uri(driver: &str, host: &str, port: u16) -> Option<String> {
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    match driver {
        "postgres" => Some(format!("postgresql://postgres@{authority}/postgres")),
        "mongodb" => Some(format!("mongodb://{authority}")),
        _ => None,
    }
}

fn config_driver(driver: &str) -> Option<DbDriver> {
    match driver {
        "postgres" => Some(DbDriver::Postgres),
        "mongodb" => Some(DbDriver::Mongodb),
        _ => None,
    }
}

fn default_port(driver: &DbDriver) -> Option<u16> {
    match driver {
        DbDriver::Postgres => Some(5432),
        DbDriver::Mysql => Some(3306),
        DbDriver::Mongodb => Some(27017),
        DbDriver::Sqlite => None,
    }
}

fn normalized_host(host: &str) -> String {
    host.trim_matches(['[', ']']).to_ascii_lowercase()
}

/// Parse only the authority needed for endpoint reconciliation. This is kept
/// deliberately small and internal so configured credentials are never copied
/// into discovery results or errors.
fn uri_hosts(uri: &str, driver: &DbDriver) -> Vec<(String, u16)> {
    let Some((_, remainder)) = uri.split_once("://") else {
        return Vec::new();
    };
    let authority = remainder
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit_once('@')
        .map(|(_, hosts)| hosts)
        .unwrap_or_else(|| remainder.split(['/', '?', '#']).next().unwrap_or_default());
    let Some(fallback_port) = default_port(driver) else {
        return Vec::new();
    };

    authority
        .split(',')
        .filter_map(|raw| {
            let raw = raw.trim();
            if let Some(rest) = raw.strip_prefix('[') {
                let (host, suffix) = rest.split_once(']')?;
                let port = suffix
                    .strip_prefix(':')
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(fallback_port);
                return Some((normalized_host(host), port));
            }
            match raw.rsplit_once(':') {
                Some((host, port)) if !host.contains(':') => {
                    Some((normalized_host(host), port.parse().ok()?))
                }
                _ => Some((normalized_host(raw), fallback_port)),
            }
        })
        .collect()
}

fn connection_matches(
    connection: &DbConnectionConfig,
    discovered: &DiscoveredDb,
    driver: &DbDriver,
) -> bool {
    connection.driver == *driver
        && uri_hosts(&connection.uri, driver)
            .iter()
            .any(|(host, port)| {
                host == &normalized_host(&discovered.host) && *port == discovered.port
            })
}

fn discovered_connection_name(discovered: &DiscoveredDb) -> String {
    let host = discovered
        .host
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!(
        "discovered-{}-{}-{}",
        discovered.driver, host, discovered.port
    )
}

#[derive(Debug, Clone)]
pub struct ReconciledDb {
    pub discovered: DiscoveredDb,
    /// Present for Explorer-supported drivers. This may be an existing
    /// operator-updated connection (including stored credentials) or a newly
    /// created passwordless candidate.
    pub connection: Option<DbConnectionConfig>,
    pub newly_added: bool,
}

/// Reconcile scan results with configured Explorer connections.
///
/// Supported candidates are added read-only so the caller can probe them and
/// so an authentication/routing failure has a real saved connection for the
/// dashboard's Update/Retry flow. Unsupported services remain visible but are
/// never falsely marked connected.
pub fn reconcile_connections(
    discovered: Vec<DiscoveredDb>,
    connections: &mut Vec<DbConnectionConfig>,
) -> Vec<ReconciledDb> {
    discovered
        .into_iter()
        .map(|item| {
            let Some(driver) = config_driver(&item.driver) else {
                return ReconciledDb {
                    discovered: item,
                    connection: None,
                    newly_added: false,
                };
            };

            if let Some(existing) = connections
                .iter()
                .find(|connection| connection_matches(connection, &item, &driver))
                .cloned()
            {
                return ReconciledDb {
                    discovered: item,
                    connection: Some(existing),
                    newly_added: false,
                };
            }

            let uri = passwordless_uri(&item.driver, &item.host, item.port)
                .expect("supported discovery drivers have a passwordless URI");
            let base_name = discovered_connection_name(&item);
            let mut name = base_name.clone();
            let mut suffix = 2usize;
            while connections.iter().any(|connection| connection.name == name) {
                name = format!("{base_name}-{suffix}");
                suffix += 1;
            }
            let connection = DbConnectionConfig {
                name: name.clone(),
                driver,
                uri,
                database: None,
                read_only: true,
                max_rows: 500,
                label: Some(format!(
                    "Discovered {} {}:{}",
                    item.driver, item.host, item.port
                )),
            };
            connections.push(connection.clone());
            ReconciledDb {
                discovered: item,
                connection: Some(connection),
                newly_added: true,
            }
        })
        .collect()
}

/// Private IPv4 /24 subnets this node is attached to (RFC1918 only).
///
/// Uses the standard "connect a UDP socket and read back the local address"
/// trick — it sends no packets and needs no extra dependency.
///
/// Running inside a container, this only ever sees the container's own
/// bridge network (e.g. 172.18.0.0/24) — other containers, not the
/// operator's actual WiFi/LAN, which sits on the far side of the host's NAT
/// and is invisible to the container's own routing table. `host_live_subnet`
/// fills that gap by asking a throwaway `--network host` helper container
/// (the bundle already mounts the Docker socket) for the *host's* current
/// default route — re-derived on every scan, so it tracks whatever WiFi/LAN
/// the node is actually on right now, not just whatever it was on at deploy
/// time. `parse_lan_subnets` (from `LLAMAFARM_LAN_SUBNETS`) is a manual
/// fallback/addition for deployments without Docker-socket access.
async fn local_subnets() -> Vec<[u8; 3]> {
    let mut nets: HashSet<[u8; 3]> = HashSet::new();
    for probe_target in ["8.8.8.8:80", "1.1.1.1:80"] {
        let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") else {
            continue;
        };
        if sock.connect(probe_target).is_ok() {
            if let Ok(SocketAddr::V4(local)) = sock.local_addr() {
                let v4 = *local.ip();
                if v4.is_private() && !v4.is_loopback() {
                    let o = v4.octets();
                    nets.insert([o[0], o[1], o[2]]);
                }
            }
        }
    }
    if let Some(net) = host_live_subnet().await {
        nets.insert(net);
    }
    nets.extend(parse_lan_subnets(
        std::env::var("LLAMAFARM_LAN_SUBNETS").ok().as_deref(),
    ));
    nets.into_iter().collect()
}

/// Asks a short-lived `docker run --network host` helper for the host's
/// *current* default-route source address, so discovery follows whichever
/// WiFi/LAN the node is presently connected to. Returns `None` (never errors
/// out the caller) whenever Docker isn't reachable, there's no default
/// route, or the reported source address isn't a private RFC1918 address.
async fn host_live_subnet() -> Option<[u8; 3]> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("docker")
            .args([
                "run",
                "--rm",
                "--network",
                "host",
                "alpine:latest",
                "ip",
                "-o",
                "route",
                "get",
                "1.1.1.1",
            ])
            .output(),
    )
    .await
    .ok()?
    .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let src_ip = stdout
        .split_whitespace()
        .zip(stdout.split_whitespace().skip(1))
        .find(|(token, _)| *token == "src")
        .map(|(_, ip)| ip)?;
    let v4: Ipv4Addr = src_ip.parse().ok()?;
    let o = v4.octets();
    v4.is_private().then_some([o[0], o[1], o[2]])
}

/// Parses extra /24 prefixes from a `LLAMAFARM_LAN_SUBNETS`-shaped value: a
/// comma-separated list like `192.168.1,10.0.0`, each entry optionally ending
/// in `.0` or `.0/24`. Pure function (no env access) so it's directly
/// testable without mutating shared process state.
fn parse_lan_subnets(raw: Option<&str>) -> Vec<[u8; 3]> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    raw.split(',')
        .filter_map(|entry| {
            let trimmed = entry.trim();
            let trimmed = trimmed.strip_suffix("/24").unwrap_or(trimmed);
            // `strip_suffix` (unlike `trim_end_matches`) removes at most one
            // trailing ".0" so "10.0.0.0" -> "10.0.0" rather than collapsing
            // every repeated ".0" down to a bare "10".
            let trimmed = trimmed.strip_suffix(".0").unwrap_or(trimmed);
            let octets: Vec<&str> = trimmed.split('.').collect();
            let [a, b, c] = octets[..] else { return None };
            let a: u8 = a.parse().ok()?;
            let b: u8 = b.parse().ok()?;
            let c: u8 = c.parse().ok()?;
            let v4 = Ipv4Addr::new(a, b, c, 1);
            v4.is_private().then_some([a, b, c])
        })
        .collect()
}

async fn probe(addr: SocketAddr) -> bool {
    matches!(
        tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

/// Scan the node's private subnets for reachable database ports.
///
/// `hosts` optionally restricts the scan to specific addresses; when empty the
/// node's own /24 subnets are swept.
pub async fn scan(hosts: &[String]) -> Vec<DiscoveredDb> {
    let mut targets: Vec<IpAddr> = Vec::new();
    if hosts.is_empty() {
        for net in local_subnets().await {
            for last in 1u8..=254 {
                targets.push(IpAddr::V4(Ipv4Addr::new(net[0], net[1], net[2], last)));
            }
        }
    } else {
        for host in hosts {
            if let Ok(ip) = host.parse::<IpAddr>() {
                // Only private addresses are permitted, even when explicit.
                let allowed = match ip {
                    IpAddr::V4(v4) => v4.is_private() || v4.is_loopback(),
                    IpAddr::V6(v6) => v6.is_loopback(),
                };
                if allowed {
                    targets.push(ip);
                }
            }
        }
    }

    let mut found = Vec::new();
    // Bounded concurrency keeps the sweep fast without exhausting sockets.
    for chunk in targets.chunks(MAX_CONCURRENCY / DB_PORTS.len().max(1)) {
        let mut set = tokio::task::JoinSet::new();
        for ip in chunk {
            for (port, driver) in DB_PORTS {
                let addr = SocketAddr::new(*ip, *port);
                let driver = driver.to_string();
                set.spawn(async move {
                    if probe(addr).await {
                        Some((addr.ip().to_string(), addr.port(), driver))
                    } else {
                        None
                    }
                });
            }
        }
        while let Some(res) = set.join_next().await {
            if let Ok(Some((host, port, driver))) = res {
                found.push(DiscoveredDb { host, port, driver });
            }
        }
    }

    found.sort_by(|a, b| (a.host.clone(), a.port).cmp(&(b.host.clone(), b.port)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lan_subnets_handles_comma_list_with_trailing_zero_or_cidr() {
        let mut got = parse_lan_subnets(Some("192.168.1.0/24, 10.0.0.0 ,172.20.5"));
        got.sort();
        assert_eq!(got, vec![[10, 0, 0], [172, 20, 5], [192, 168, 1]]);
    }

    #[test]
    fn parse_lan_subnets_rejects_non_private_ranges() {
        assert!(parse_lan_subnets(Some("8.8.8")).is_empty());
    }

    #[test]
    fn parse_lan_subnets_empty_when_unset() {
        assert!(parse_lan_subnets(None).is_empty());
    }

    #[test]
    fn discovery_serialization_never_exposes_a_connection_uri() {
        let discovered = DiscoveredDb {
            host: "10.0.0.5".to_string(),
            port: 27017,
            driver: "mongodb".to_string(),
        };

        let serialized = serde_json::to_string(&discovered).expect("discovery should serialize");

        assert!(!serialized.contains("mongodb://"));
        assert!(!serialized.contains("uri"));
        assert!(!serialized.contains("password"));
    }

    #[tokio::test]
    async fn scan_rejects_public_addresses() {
        // A public IP must never be probed, even when explicitly requested.
        let found = scan(&["8.8.8.8".to_string()]).await;
        assert!(found.is_empty(), "public addresses must be refused");
    }

    #[test]
    fn reconciliation_reuses_a_credentialed_connection_without_exposing_it() {
        let mut connections = vec![DbConnectionConfig {
            name: "research".to_string(),
            driver: DbDriver::Mongodb,
            uri: "mongodb://reader:private-value@10.0.0.5:27017/research".to_string(),
            database: Some("research".to_string()),
            read_only: true,
            max_rows: 100,
            label: None,
        }];
        let reconciled = reconcile_connections(
            vec![DiscoveredDb {
                host: "10.0.0.5".to_string(),
                port: 27017,
                driver: "mongodb".to_string(),
            }],
            &mut connections,
        );

        assert_eq!(connections.len(), 1);
        assert!(!reconciled[0].newly_added);
        assert_eq!(
            reconciled[0]
                .connection
                .as_ref()
                .map(|connection| connection.name.as_str()),
            Some("research")
        );
    }

    #[test]
    fn reconciliation_adds_supported_passwordless_candidates_once() {
        let discovered = DiscoveredDb {
            host: "192.168.1.154".to_string(),
            port: 27017,
            driver: "mongodb".to_string(),
        };
        let mut connections = Vec::new();

        let first = reconcile_connections(vec![discovered.clone()], &mut connections);
        let second = reconcile_connections(vec![discovered], &mut connections);

        assert!(first[0].newly_added);
        assert!(!second[0].newly_added);
        assert_eq!(connections.len(), 1);
        assert_eq!(
            connections[0].name,
            "discovered-mongodb-192-168-1-154-27017"
        );
        assert!(connections[0].read_only);
    }

    #[test]
    fn reconciliation_keeps_unsupported_services_visible_but_unconfigured() {
        let mut connections = Vec::new();
        let reconciled = reconcile_connections(
            vec![DiscoveredDb {
                host: "10.0.0.8".to_string(),
                port: 6379,
                driver: "redis".to_string(),
            }],
            &mut connections,
        );

        assert!(connections.is_empty());
        assert!(reconciled[0].connection.is_none());
        assert!(!reconciled[0].newly_added);
    }

    #[tokio::test]
    async fn scan_finds_a_listening_local_port() {
        // Bind a listener on a DB port we can actually claim; if the port is
        // busy on this host, skip rather than fail.
        let Ok(listener) = tokio::net::TcpListener::bind("127.0.0.1:6379").await else {
            return;
        };
        let _guard = tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let found = scan(&["127.0.0.1".to_string()]).await;
        assert!(
            found.iter().any(|d| d.port == 6379 && d.driver == "redis"),
            "expected the bound redis port to be discovered: {found:?}"
        );
    }
}
