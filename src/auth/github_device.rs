//! GitHub OAuth device flow.
//!
//! Powers the Copilot-style "Connect GitHub" button: the operator clicks
//! connect, the UI shows a short user code plus a link to
//! `https://github.com/login/device`, and the gateway polls until the
//! authorization completes. No token pasting.
//!
//! The resulting token is stored owner-only (0600) in the node's persistent
//! config directory and is brokered to git operations — it is never exposed
//! in model context, tool output, traces, memory, or browser storage.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// GitHub's public OAuth app for the device flow. Operators may override with
/// their own app via `LLAMAFARM_GITHUB_CLIENT_ID`.
const DEFAULT_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const USER_URL: &str = "https://api.github.com/user";
const DEFAULT_SCOPE: &str = "repo read:user";

pub fn client_id() -> String {
    std::env::var("LLAMAFARM_GITHUB_CLIENT_ID").unwrap_or_else(|_| DEFAULT_CLIENT_ID.to_string())
}

/// What the UI needs to show the operator to authorize this node.
#[derive(Debug, Clone, Serialize)]
pub struct GithubDeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Connection state surfaced to the settings UI. Never contains the token.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GithubConnection {
    pub login: String,
    pub scopes: String,
    pub connected_at: String,
}

/// Owner-only credential file: `<config dir>/github-token.json`.
fn token_path(config_dir: &Path) -> PathBuf {
    config_dir.join("github-token.json")
}

#[derive(Serialize, Deserialize)]
struct StoredToken {
    token: String,
    #[serde(flatten)]
    connection: GithubConnection,
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("llamafarm")
        .build()
        .unwrap_or_default()
}

/// Step 1: ask GitHub for a device + user code.
pub async fn start(scope: Option<&str>) -> Result<GithubDeviceStart> {
    let resp = http_client()
        .post(DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .form(&[
            ("client_id", client_id()),
            ("scope", scope.unwrap_or(DEFAULT_SCOPE).to_string()),
        ])
        .send()
        .await
        .context("requesting GitHub device code")?
        .error_for_status()?
        .json::<DeviceCodeResponse>()
        .await
        .context("parsing GitHub device code response")?;

    Ok(GithubDeviceStart {
        device_code: resp.device_code,
        user_code: resp.user_code,
        verification_uri: resp.verification_uri,
        expires_in: resp.expires_in,
        interval: resp.interval.unwrap_or(5).max(1),
    })
}

/// Outcome of one poll attempt — the UI polls until Connected or an error.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PollOutcome {
    /// Operator has not finished authorizing yet; keep polling.
    Pending,
    /// Poll interval too fast; back off by `interval` seconds.
    SlowDown { interval: u64 },
    /// Authorized and stored.
    Connected { connection: GithubConnection },
    /// Terminal failure (expired, denied, etc.).
    Failed { error: String },
}

/// Step 2: exchange the device code for a token (one poll attempt).
/// On success the token is persisted owner-only and the account is resolved.
pub async fn poll_once(device_code: &str, config_dir: &Path) -> Result<PollOutcome> {
    let client = http_client();
    let resp = client
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .form(&[
            ("client_id", client_id()),
            ("device_code", device_code.to_string()),
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:device_code".to_string(),
            ),
        ])
        .send()
        .await
        .context("polling GitHub for device token")?
        .json::<TokenResponse>()
        .await
        .context("parsing GitHub token response")?;

    if let Some(error) = resp.error.as_deref() {
        return Ok(match error {
            "authorization_pending" => PollOutcome::Pending,
            "slow_down" => PollOutcome::SlowDown { interval: 10 },
            other => PollOutcome::Failed {
                error: other.to_string(),
            },
        });
    }

    let Some(token) = resp.access_token else {
        return Ok(PollOutcome::Failed {
            error: "no access_token in response".into(),
        });
    };

    // Resolve the account so the UI can show who is connected. A failure here
    // must not lose a valid token, so it degrades to "unknown".
    let login = fetch_login(&client, &token).await.unwrap_or_else(|| "unknown".into());

    let connection = GithubConnection {
        login,
        scopes: resp.scope.unwrap_or_default(),
        connected_at: chrono::Utc::now().to_rfc3339(),
    };
    store_token(config_dir, &token, &connection)?;
    Ok(PollOutcome::Connected { connection })
}

/// Look up the authenticated account's login name.
async fn fetch_login(client: &reqwest::Client, token: &str) -> Option<String> {
    let value = client
        .get(USER_URL)
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json::<serde_json::Value>()
        .await
        .ok()?;
    value
        .get("login")
        .and_then(|l| l.as_str())
        .map(str::to_string)
}

/// Persist the token owner-only (0600).
fn store_token(config_dir: &Path, token: &str, connection: &GithubConnection) -> Result<()> {
    std::fs::create_dir_all(config_dir)?;
    let path = token_path(config_dir);
    let stored = StoredToken {
        token: token.to_string(),
        connection: connection.clone(),
    };
    let json = serde_json::to_string(&stored)?;
    std::fs::write(&path, json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Connection state for the settings UI (no token).
pub fn connection_status(config_dir: &Path) -> Option<GithubConnection> {
    let raw = std::fs::read_to_string(token_path(config_dir)).ok()?;
    serde_json::from_str::<StoredToken>(&raw)
        .ok()
        .map(|s| s.connection)
}

/// Broker the token to git operations only. Not exposed over the API.
pub fn brokered_token(config_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(token_path(config_dir)).ok()?;
    serde_json::from_str::<StoredToken>(&raw).ok().map(|s| s.token)
}

/// Disconnect: remove the stored credential.
pub fn disconnect(config_dir: &Path) -> bool {
    std::fs::remove_file(token_path(config_dir)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn stores_token_owner_only_and_hides_it_from_status() {
        let tmp = TempDir::new().unwrap();
        let conn = GithubConnection {
            login: "octocat".into(),
            scopes: "repo".into(),
            connected_at: "2026-07-16T00:00:00Z".into(),
        };
        store_token(tmp.path(), "ghs_secret_value", &conn).unwrap();

        // Status exposes the account but never the token.
        let status = connection_status(tmp.path()).expect("connected");
        assert_eq!(status.login, "octocat");
        let status_json = serde_json::to_string(&status).unwrap();
        assert!(!status_json.contains("ghs_secret_value"));

        // The token is only reachable through the broker.
        assert_eq!(
            brokered_token(tmp.path()).as_deref(),
            Some("ghs_secret_value")
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(token_path(tmp.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "credential must be owner-only");
        }

        assert!(disconnect(tmp.path()));
        assert!(connection_status(tmp.path()).is_none());
    }

    #[test]
    fn status_is_none_when_never_connected() {
        let tmp = TempDir::new().unwrap();
        assert!(connection_status(tmp.path()).is_none());
        assert!(brokered_token(tmp.path()).is_none());
    }
}
