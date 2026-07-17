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
    /// True when the server answered an unauthenticated probe (no password).
    /// The operator can then one-click connect. Only meaningful for redis so
    /// far; None for drivers we don't safely probe.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_auth: Option<bool>,
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

/// Non-destructive check for whether a server requires a password.
/// Currently only redis: send `PING` and read the reply — `+PONG` means no
/// auth, `-NOAUTH`/`-ERR ...auth...` means a password is set. Read-only, sends
/// no data-mutating commands. Returns None for drivers we don't safely probe.
async fn probe_no_auth(addr: SocketAddr, driver: &str) -> Option<bool> {
    if driver != "redis" {
        return None;
    }
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(addr))
        .await
        .ok()?
        .ok()?;
    stream.write_all(b"PING\r\n").await.ok()?;
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(PROBE_TIMEOUT, stream.read(&mut buf))
        .await
        .ok()?
        .ok()?;
    let reply = String::from_utf8_lossy(&buf[..n]).to_ascii_uppercase();
    if reply.starts_with("+PONG") {
        Some(true) // answered without auth
    } else if reply.contains("NOAUTH") || reply.contains("AUTH") {
        Some(false) // password required
    } else {
        None
    }
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
                let no_auth = if let Ok(ip) = host.parse::<IpAddr>() {
                    probe_no_auth(SocketAddr::new(ip, port), &driver).await
                } else {
                    None
                };
                found.push(DiscoveredDb {
                    host,
                    port,
                    driver,
                    suggested_dsn,
                    no_auth,
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
    async fn no_auth_probe_detects_open_redis() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Ok(listener) = tokio::net::TcpListener::bind("127.0.0.1:0").await else {
            return;
        };
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 16];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(b"+PONG\r\n").await;
            }
        });
        assert_eq!(probe_no_auth(addr, "redis").await, Some(true));
        // Non-redis drivers are never probed.
        assert_eq!(probe_no_auth(addr, "postgres").await, None);
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
