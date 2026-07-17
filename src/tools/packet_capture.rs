//! Bounded packet-capture tool (authorized lab use).
//!
//! Wraps `tshark`/`tcpdump` with hard bounds (packet count + wall-clock
//! duration) so the agent can do real network analysis on its disposable lab
//! without an unbounded capture. Writes a capture artifact to the workspace
//! and returns a text summary. Requires the lab toolkit image
//! (`--build-arg LLAMAFARM_LAB_TOOLS=1`); reports cleanly if unavailable.

use super::traits::{Tool, ToolResult};
use crate::security::{SecurityPolicy, policy::ToolOperation};
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const MAX_PACKETS: u32 = 5000;
const DEFAULT_PACKETS: u32 = 200;
const MAX_DURATION_SECS: u64 = 120;
const DEFAULT_DURATION_SECS: u64 = 15;

pub struct PacketCaptureTool {
    security: Arc<SecurityPolicy>,
    workspace_dir: PathBuf,
}

impl PacketCaptureTool {
    pub fn new(security: Arc<SecurityPolicy>, workspace_dir: PathBuf) -> Self {
        Self {
            security,
            workspace_dir,
        }
    }

    /// A BPF filter must be a simple, whitelisted expression — no shell
    /// metacharacters that could break out of the argv.
    fn filter_is_safe(filter: &str) -> bool {
        filter.len() <= 200
            && filter.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || matches!(c, ' ' | '.' | ':' | '/' | '-' | '_' | '(' | ')' | '=')
            })
    }
}

#[async_trait]
impl Tool for PacketCaptureTool {
    fn name(&self) -> &str {
        "packet_capture"
    }

    fn description(&self) -> &str {
        "Capture network packets on the lab node with hard bounds (packet \
         count + duration) and return a summary. Params: interface (default \
         'any'), filter (optional BPF, e.g. 'tcp port 443'), packets (default \
         200, max 5000), duration_secs (default 15, max 120). Requires the lab \
         toolkit image. For authorized analysis on networks you own or may test."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "interface": {"type": "string", "description": "Interface to capture on (default 'any')"},
                "filter": {"type": "string", "description": "Optional BPF filter, e.g. 'tcp port 80'"},
                "packets": {"type": "integer", "description": "Max packets (default 200, max 5000)"},
                "duration_secs": {"type": "integer", "description": "Max seconds (default 15, max 120)"}
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(msg) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "packet_capture")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(msg),
            });
        }

        let engine = if which::which("tshark").is_ok() {
            "tshark"
        } else if which::which("tcpdump").is_ok() {
            "tcpdump"
        } else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "No capture engine (tshark/tcpdump). Rebuild with \
                     --build-arg LLAMAFARM_LAB_TOOLS=1."
                        .into(),
                ),
            });
        };

        let interface = args
            .get("interface")
            .and_then(|v| v.as_str())
            .filter(|s| s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
            .unwrap_or("any")
            .to_string();
        let packets = args
            .get("packets")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(DEFAULT_PACKETS)
            .clamp(1, MAX_PACKETS);
        let duration = Duration::from_secs(
            args.get("duration_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(DEFAULT_DURATION_SECS)
                .clamp(1, MAX_DURATION_SECS),
        );
        let filter = args.get("filter").and_then(|v| v.as_str()).unwrap_or("");
        if !filter.is_empty() && !Self::filter_is_safe(filter) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Unsafe capture filter rejected".into()),
            });
        }

        std::fs::create_dir_all(self.workspace_dir.join("captures")).ok();
        let out_path = self
            .workspace_dir
            .join("captures")
            .join(format!("capture-{}.txt", uuid::Uuid::new_v4()));

        let mut cmd = tokio::process::Command::new(engine);
        cmd.arg("-i").arg(&interface).kill_on_drop(true);
        match engine {
            "tshark" => {
                cmd.arg("-c").arg(packets.to_string());
                cmd.arg("-a").arg(format!("duration:{}", duration.as_secs()));
                if !filter.is_empty() {
                    cmd.arg("-f").arg(filter);
                }
            }
            _ => {
                // tcpdump
                cmd.arg("-c").arg(packets.to_string()).arg("-n");
                if !filter.is_empty() {
                    cmd.arg(filter);
                }
            }
        }

        let run = tokio::time::timeout(
            duration + Duration::from_secs(5),
            cmd.output(),
        )
        .await;

        let output = match run {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "capture failed to run: {e} (packet capture usually needs \
                         CAP_NET_RAW / privileged container)"
                    )),
                });
            }
            Err(_) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("capture timed out".into()),
                });
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() && stdout.trim().is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("capture error: {}", stderr.trim())),
            });
        }
        let _ = std::fs::write(&out_path, stdout.as_bytes());
        let line_count = stdout.lines().count();
        let preview: String = stdout.lines().take(40).collect::<Vec<_>>().join("\n");
        Ok(ToolResult {
            success: true,
            output: format!(
                "Captured {line_count} lines with {engine} on {interface} \
                 (≤{packets} pkts, ≤{}s). Saved to {}.\n\n{preview}",
                duration.as_secs(),
                out_path.display(),
            ),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_filters_accepted_unsafe_rejected() {
        assert!(PacketCaptureTool::filter_is_safe("tcp port 443"));
        assert!(PacketCaptureTool::filter_is_safe("host 10.0.0.1 and udp"));
        assert!(!PacketCaptureTool::filter_is_safe("tcp; rm -rf /"));
        assert!(!PacketCaptureTool::filter_is_safe("$(whoami)"));
        assert!(!PacketCaptureTool::filter_is_safe("a | b"));
    }

    #[tokio::test]
    async fn reports_cleanly_when_engine_missing() {
        // On a host without tshark/tcpdump this returns a clean error, not a panic.
        if which::which("tshark").is_ok() || which::which("tcpdump").is_ok() {
            return;
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let mut policy = SecurityPolicy::default();
        policy.autonomy = crate::security::AutonomyLevel::Full;
        let tool = PacketCaptureTool::new(Arc::new(policy), tmp.path().to_path_buf());
        let r = tool.execute(json!({})).await.unwrap();
        assert!(!r.success);
        assert!(r.error.as_deref().unwrap_or("").contains("LLAMAFARM_LAB_TOOLS"));
    }
}
