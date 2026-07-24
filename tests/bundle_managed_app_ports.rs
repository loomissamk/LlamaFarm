use std::fs;
use std::path::Path;

fn read_repo_file(path: &str) -> String {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(repo_root.join(path))
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
}

#[test]
fn bundle_publishes_full_managed_range_and_keeps_stock_port_exclusive() {
    let compose = read_repo_file("docker-compose.bundle.yml");

    assert!(compose.contains("LLAMAFARM_RESERVED_APP_PORTS=${LLAMAFARM_RESERVED_APP_PORTS:-5000}"));
    assert!(
        compose.contains("LLAMAFARM_MANAGED_APP_PORTS=${LLAMAFARM_MANAGED_APP_PORTS:-8501-8599}")
    );
    assert!(compose.contains("${DEV_APP_PORT:-5000}:5000"));
    assert!(compose.contains(
        "${LLAMAFARM_MANAGED_APP_PORTS:-8501-8599}:${LLAMAFARM_MANAGED_APP_PORTS:-8501-8599}"
    ));
    assert!(
        !compose.contains("8501-8510"),
        "the old ten-port managed range must not remain in bundle Compose"
    );
}

#[test]
fn launcher_status_and_operator_docs_match_the_published_range() {
    let launcher = read_repo_file("scripts/docker/up-bundle.sh");
    let node_profiles = read_repo_file("deploy/node-profiles/README.md");

    assert!(launcher.contains(
        "LlamaFarm managed app ports: ${LLAMAFARM_MANAGED_APP_PORTS:-8501-8599} (${LLAMAFARM_RESERVED_APP_PORTS:-5000} reserved)"
    ));
    assert!(node_profiles.contains("ports 42617, 5000, and 8501-8599"));
    assert!(!launcher.contains("8501-8510"));
    assert!(!node_profiles.contains("8501-8510"));
}
