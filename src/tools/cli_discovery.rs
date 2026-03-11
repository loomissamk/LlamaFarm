//! CLI tool auto-discovery — scans PATH for known CLI tools.
//! Zero external dependencies (uses `std::process::Command` + `std::env`).

use std::path::PathBuf;

/// Category of a discovered CLI tool.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum CliCategory {
    VersionControl,
    Language,
    PackageManager,
    Container,
    Build,
    Cloud,
    Shell,
    Browser,
    Utility,
    AiRuntime,
}

impl std::fmt::Display for CliCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VersionControl => write!(f, "Version Control"),
            Self::Language => write!(f, "Language"),
            Self::PackageManager => write!(f, "Package Manager"),
            Self::Container => write!(f, "Container"),
            Self::Build => write!(f, "Build"),
            Self::Cloud => write!(f, "Cloud"),
            Self::Shell => write!(f, "Shell"),
            Self::Browser => write!(f, "Browser"),
            Self::Utility => write!(f, "Utility"),
            Self::AiRuntime => write!(f, "AI Runtime"),
        }
    }
}

/// A discovered CLI tool with metadata.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiscoveredCli {
    pub name: String,
    pub path: PathBuf,
    pub version: Option<String>,
    pub category: CliCategory,
}

/// Known CLI tools to scan for.
struct KnownCli {
    name: &'static str,
    version_args: &'static [&'static str],
    category: CliCategory,
}

const KNOWN_CLIS: &[KnownCli] = &[
    KnownCli {
        name: "git",
        version_args: &["--version"],
        category: CliCategory::VersionControl,
    },
    KnownCli {
        name: "bash",
        version_args: &["--version"],
        category: CliCategory::Shell,
    },
    KnownCli {
        name: "sh",
        version_args: &["--version"],
        category: CliCategory::Shell,
    },
    KnownCli {
        name: "python",
        version_args: &["--version"],
        category: CliCategory::Language,
    },
    KnownCli {
        name: "python3",
        version_args: &["--version"],
        category: CliCategory::Language,
    },
    KnownCli {
        name: "node",
        version_args: &["--version"],
        category: CliCategory::Language,
    },
    KnownCli {
        name: "npm",
        version_args: &["--version"],
        category: CliCategory::PackageManager,
    },
    KnownCli {
        name: "pip",
        version_args: &["--version"],
        category: CliCategory::PackageManager,
    },
    KnownCli {
        name: "pip3",
        version_args: &["--version"],
        category: CliCategory::PackageManager,
    },
    KnownCli {
        name: "docker",
        version_args: &["--version"],
        category: CliCategory::Container,
    },
    KnownCli {
        name: "docker-compose",
        version_args: &["--version"],
        category: CliCategory::Container,
    },
    KnownCli {
        name: "podman",
        version_args: &["--version"],
        category: CliCategory::Container,
    },
    KnownCli {
        name: "cargo",
        version_args: &["--version"],
        category: CliCategory::Build,
    },
    KnownCli {
        name: "rg",
        version_args: &["--version"],
        category: CliCategory::Utility,
    },
    KnownCli {
        name: "curl",
        version_args: &["--version"],
        category: CliCategory::Utility,
    },
    KnownCli {
        name: "jq",
        version_args: &["--version"],
        category: CliCategory::Utility,
    },
    KnownCli {
        name: "grep",
        version_args: &["--version"],
        category: CliCategory::Utility,
    },
    KnownCli {
        name: "sed",
        version_args: &["--version"],
        category: CliCategory::Utility,
    },
    KnownCli {
        name: "find",
        version_args: &["--version"],
        category: CliCategory::Utility,
    },
    KnownCli {
        name: "awk",
        version_args: &["--version"],
        category: CliCategory::Utility,
    },
    KnownCli {
        name: "tar",
        version_args: &["--version"],
        category: CliCategory::Utility,
    },
    KnownCli {
        name: "zip",
        version_args: &["--version"],
        category: CliCategory::Utility,
    },
    KnownCli {
        name: "unzip",
        version_args: &["--version"],
        category: CliCategory::Utility,
    },
    KnownCli {
        name: "lsusb",
        version_args: &["--version"],
        category: CliCategory::Utility,
    },
    KnownCli {
        name: "lsblk",
        version_args: &["--version"],
        category: CliCategory::Utility,
    },
    KnownCli {
        name: "lspci",
        version_args: &["--version"],
        category: CliCategory::Utility,
    },
    KnownCli {
        name: "sqlite3",
        version_args: &["--version"],
        category: CliCategory::Utility,
    },
    KnownCli {
        name: "pdftotext",
        version_args: &["-v"],
        category: CliCategory::Utility,
    },
    KnownCli {
        name: "ffmpeg",
        version_args: &["-version"],
        category: CliCategory::Utility,
    },
    KnownCli {
        name: "ollama",
        version_args: &["--version"],
        category: CliCategory::AiRuntime,
    },
    KnownCli {
        name: "chromium",
        version_args: &["--version"],
        category: CliCategory::Browser,
    },
    KnownCli {
        name: "chromium-browser",
        version_args: &["--version"],
        category: CliCategory::Browser,
    },
    KnownCli {
        name: "google-chrome",
        version_args: &["--version"],
        category: CliCategory::Browser,
    },
    KnownCli {
        name: "google-chrome-stable",
        version_args: &["--version"],
        category: CliCategory::Browser,
    },
    KnownCli {
        name: "make",
        version_args: &["--version"],
        category: CliCategory::Build,
    },
    KnownCli {
        name: "go",
        version_args: &["version"],
        category: CliCategory::Language,
    },
    KnownCli {
        name: "java",
        version_args: &["-version"],
        category: CliCategory::Language,
    },
    KnownCli {
        name: "javac",
        version_args: &["-version"],
        category: CliCategory::Language,
    },
    KnownCli {
        name: "ruby",
        version_args: &["--version"],
        category: CliCategory::Language,
    },
    KnownCli {
        name: "perl",
        version_args: &["--version"],
        category: CliCategory::Language,
    },
    KnownCli {
        name: "php",
        version_args: &["--version"],
        category: CliCategory::Language,
    },
    KnownCli {
        name: "pnpm",
        version_args: &["--version"],
        category: CliCategory::PackageManager,
    },
    KnownCli {
        name: "yarn",
        version_args: &["--version"],
        category: CliCategory::PackageManager,
    },
    KnownCli {
        name: "bun",
        version_args: &["--version"],
        category: CliCategory::PackageManager,
    },
    KnownCli {
        name: "kubectl",
        version_args: &["version", "--client", "--short"],
        category: CliCategory::Cloud,
    },
    KnownCli {
        name: "rustc",
        version_args: &["--version"],
        category: CliCategory::Language,
    },
];

/// Discover available CLI tools on the system.
/// Scans PATH for known tools and returns metadata for each found.
pub fn discover_cli_tools(additional: &[String], excluded: &[String]) -> Vec<DiscoveredCli> {
    let mut results = Vec::new();

    for known in KNOWN_CLIS {
        if excluded.iter().any(|e| e == known.name) {
            continue;
        }
        if let Some(cli) = probe_cli(known.name, known.version_args, known.category.clone()) {
            results.push(cli);
        }
    }

    // Probe additional user-specified tools
    for tool_name in additional {
        if excluded.iter().any(|e| e == tool_name) {
            continue;
        }
        // Skip if already discovered
        if results.iter().any(|r| r.name == *tool_name) {
            continue;
        }
        if let Some(cli) = probe_cli(tool_name, &["--version"], CliCategory::Build) {
            results.push(cli);
        }
    }

    results
}

/// Probe a command by name using the known-command table first, then a generic
/// fallback. This is used by the gateway to validate runtime shell detection.
pub fn probe_cli_command(name: &str) -> Option<DiscoveredCli> {
    let known = KNOWN_CLIS.iter().find(|known| known.name == name);
    match known {
        Some(known) => probe_cli(known.name, known.version_args, known.category.clone()),
        None => probe_cli(name, &["--version"], CliCategory::Utility),
    }
}

/// Probe a single CLI tool: check if it exists and get its version.
fn probe_cli(name: &str, version_args: &[&str], category: CliCategory) -> Option<DiscoveredCli> {
    // Try to find the tool using `which` (Unix) or `where` (Windows)
    let path = find_executable(name)?;

    // Try to get version
    let version = get_version(name, version_args);

    Some(DiscoveredCli {
        name: name.to_string(),
        path,
        version,
        category,
    })
}

/// Find an executable on PATH.
fn find_executable(name: &str) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let which_cmd = "where";
    #[cfg(not(target_os = "windows"))]
    let which_cmd = "which";

    let output = std::process::Command::new(which_cmd)
        .arg(name)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let path_str = String::from_utf8_lossy(&output.stdout);
    let first_line = path_str.lines().next()?.trim();
    if first_line.is_empty() {
        return None;
    }
    Some(PathBuf::from(first_line))
}

/// Get the version string of a CLI tool.
fn get_version(name: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(name)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Some tools print version to stderr (e.g., pip)
    let version_text = if stdout.trim().is_empty() {
        stderr.trim().to_string()
    } else {
        stdout.trim().to_string()
    };

    // Extract first line only
    let first_line = version_text.lines().next()?.trim().to_string();
    if first_line.is_empty() {
        None
    } else {
        Some(first_line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_returns_vec() {
        // Just verify it runs without panic
        let results = discover_cli_tools(&[], &[]);
        // We can't assert specific tools exist in CI, but structure is valid
        for cli in &results {
            assert!(!cli.name.is_empty());
        }
    }

    #[test]
    fn excluded_tools_are_skipped() {
        let results = discover_cli_tools(&[], &["git".to_string()]);
        assert!(!results.iter().any(|r| r.name == "git"));
    }

    #[test]
    fn category_display() {
        assert_eq!(CliCategory::VersionControl.to_string(), "Version Control");
        assert_eq!(CliCategory::Language.to_string(), "Language");
        assert_eq!(CliCategory::PackageManager.to_string(), "Package Manager");
        assert_eq!(CliCategory::Container.to_string(), "Container");
        assert_eq!(CliCategory::Build.to_string(), "Build");
        assert_eq!(CliCategory::Cloud.to_string(), "Cloud");
        assert_eq!(CliCategory::Shell.to_string(), "Shell");
        assert_eq!(CliCategory::Browser.to_string(), "Browser");
        assert_eq!(CliCategory::Utility.to_string(), "Utility");
        assert_eq!(CliCategory::AiRuntime.to_string(), "AI Runtime");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn failed_version_probe_returns_none() {
        let version = get_version("sh", &["-c", "echo nope >&2; exit 2"]);
        assert!(version.is_none());
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn successful_stderr_version_probe_is_preserved() {
        let version = get_version("sh", &["-c", "echo stderr-version >&2"]);
        assert_eq!(version.as_deref(), Some("stderr-version"));
    }
}
