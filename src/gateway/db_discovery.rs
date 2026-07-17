//! Local-network database discovery.
//!
//! Scans the node's own subnets for reachable database servers on their
//! standard ports so the Database page can offer them as one-click
//! connections instead of making the operator hand-write DSNs.
//!
//! This is a TCP-connect probe only: it never authenticates, never sends
//! payloads, and never reads data. It is confined to the node's own private
//! (RFC1918) subnets — it will not scan the public internet.

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
    /// Suggested connection string for the Add-connection form.
    pub suggested_dsn: String,
}

fn suggested_dsn(driver: &str, host: &str, port: u16) -> String {
    match driver {
        "postgres" => format!("postgres://USER:PASSWORD@{host}:{port}/postgres"),
        "mysql" => format!("mysql://USER:PASSWORD@{host}:{port}/mysql"),
        "mongodb" => format!("mongodb://{host}:{port}"),
        "qdrant" => format!("http://{host}:{port}"),
        "redis" => format!("redis://{host}:{port}"),
        "ollama" => format!("http://{host}:{port}"),
        _ => format!("{host}:{port}"),
    }
}

/// Private IPv4 /24 subnets this node is attached to (RFC1918 only).
///
/// Uses the standard "connect a UDP socket and read back the local address"
/// trick — it sends no packets and needs no extra dependency.
fn local_subnets() -> Vec<[u8; 3]> {
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
    nets.into_iter().collect()
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
        for net in local_subnets() {
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
                let suggested_dsn = suggested_dsn(&driver, &host, port);
                found.push(DiscoveredDb {
                    host,
                    port,
                    driver,
                    suggested_dsn,
                });
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
    fn dsn_suggestions_match_driver() {
        assert!(suggested_dsn("postgres", "10.0.0.5", 5432).starts_with("postgres://"));
        assert_eq!(suggested_dsn("mongodb", "10.0.0.5", 27017), "mongodb://10.0.0.5:27017");
        assert_eq!(suggested_dsn("qdrant", "10.0.0.5", 6333), "http://10.0.0.5:6333");
    }

    #[tokio::test]
    async fn scan_rejects_public_addresses() {
        // A public IP must never be probed, even when explicitly requested.
        let found = scan(&["8.8.8.8".to_string()]).await;
        assert!(found.is_empty(), "public addresses must be refused");
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
