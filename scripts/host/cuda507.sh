#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/host/cuda-profile-common.sh
source "$SCRIPT_DIR/cuda-profile-common.sh"

ACTION="${1:---apply}"
OPEN_PACKAGE="${LLAMAFARM_NVIDIA_OPEN_PACKAGE:-nvidia-driver-${CUDA_PROFILE_DRIVER_BRANCH}-open}"
RTX5070_DEVICE_ID="${LLAMAFARM_5070_DEVICE_ID:-2c05}"
RTX5070_BDF=""
RTX5070_GPU_UUID=""

usage() {
  cat <<'USAGE'
Usage: sudo ./scripts/host/cuda507.sh [--preflight|--apply|--verify|--reboot]

Selects the RTX 5070 Ti as the host's only LlamaFarm CUDA GPU. The command
installs NVIDIA R580 Open when needed, disables LlamaFarm's V100/mixed-binding
services, preserves Docker volumes and private configuration, and deploys the
rtx5070ti-16gb node profile once the live driver verifies.

--preflight  Prove the configured 5070 Ti PCI function is present; no changes.
--apply      Stage/verify the driver and deploy. Default. Exit 10 means reboot.
--verify     Verify the live driver, display node, CUDA identity, and node.
--reboot     Run --apply and reboot only when activation is actually required.
USAGE
}

discover_5070_bdf() {
  local candidate="${LLAMAFARM_5070_PCI_BDF:-}"
  local prior_host_config="${LLAMAFARM_V100_HOST_CONFIG:-/etc/llamafarm/v100-cuda-5070-nvk.env}"
  local bus name

  if [ -z "$candidate" ] && command -v nvidia-smi >/dev/null 2>&1; then
    while IFS=, read -r bus name; do
      name="${name#${name%%[![:space:]]*}}"
      if [[ "${name,,}" == *"rtx 5070 ti"* ]]; then
        candidate="$(cuda_profile_normalize_bdf "${bus//[[:space:]]/}")"
        break
      fi
    done < <(nvidia-smi --query-gpu=pci.bus_id,name --format=csv,noheader,nounits 2>/dev/null || true)
  fi

  if [ -z "$candidate" ] && command -v lspci >/dev/null 2>&1; then
    candidate="$(lspci -Dnn 2>/dev/null | awk 'tolower($0) ~ /rtx 5070 ti/ {print $1; exit}')"
  fi

  # A stale pci.ids database may label a very new Blackwell board as only
  # "Device 2c05". Reuse the prior hardware-only mixed profile as a fallback.
  if [ -z "$candidate" ] && [ -r "$prior_host_config" ]; then
    candidate="$(awk -F= '$1 == "DISPLAY_GPU_PCI_BDF" {print $2; exit}' "$prior_host_config")"
  fi

  [ -n "$candidate" ] \
    || cuda_profile_die "RTX 5070 Ti not found; set LLAMAFARM_5070_PCI_BDF only after checking lspci -Dnn"
  RTX5070_BDF="$(cuda_profile_normalize_bdf "$candidate")"
  [[ "$RTX5070_BDF" =~ ^[[:xdigit:]]{4}:[[:xdigit:]]{2}:[[:xdigit:]]{2}\.[0-7]$ ]] \
    || cuda_profile_die "invalid RTX 5070 Ti PCI BDF: $RTX5070_BDF"
}

preflight_5070() {
  discover_5070_bdf
  cuda_profile_validate_display_function "RTX 5070 Ti" "$RTX5070_BDF" "$RTX5070_DEVICE_ID"
  cuda_profile_log "Preflight passed: RTX 5070 Ti is present at $RTX5070_BDF."
}

clear_5070_driver_override() {
  local override
  override="$(cuda_profile_pci_path "$RTX5070_BDF")/driver_override"
  [ -e "$override" ] || cuda_profile_die "driver_override is missing for RTX 5070 Ti $RTX5070_BDF"
  [ -w "$override" ] || cuda_profile_die "driver_override is not writable for RTX 5070 Ti $RTX5070_BDF"
  printf '\n' >"$override"
}

verify_5070_host() {
  local driver found=0 reported=0 bus name uuid card_count
  preflight_5070 >/dev/null
  cuda_profile_verify_open_module
  driver="$(cuda_profile_pci_driver "$RTX5070_BDF")"
  [ "$driver" = nvidia ] \
    || cuda_profile_die "RTX 5070 Ti $RTX5070_BDF is bound to ${driver:-no driver}, not nvidia"
  cuda_profile_require_command nvidia-smi

  while IFS=, read -r bus name uuid; do
    reported=$((reported + 1))
    bus="${bus//[[:space:]]/}"
    name="${name#${name%%[![:space:]]*}}"
    uuid="${uuid//[[:space:]]/}"
    if [ "$(cuda_profile_normalize_bdf "$bus")" = "$RTX5070_BDF" ]; then
      [[ "${name,,}" == *"rtx 5070 ti"* ]] \
        || cuda_profile_die "CUDA device at $RTX5070_BDF is not reported as an RTX 5070 Ti"
      [[ "$uuid" == GPU-* ]] || cuda_profile_die "CUDA did not report a GPU UUID for the RTX 5070 Ti"
      RTX5070_GPU_UUID="$uuid"
      found=$((found + 1))
    fi
  done < <(nvidia-smi --query-gpu=pci.bus_id,name,uuid --format=csv,noheader,nounits)
  [ "$found" -eq 1 ] \
    || cuda_profile_die "nvidia-smi did not report exactly one RTX 5070 Ti at $RTX5070_BDF"
  [ "$reported" -eq 1 ] \
    || cuda_profile_die "5070 Ti mode requires it to be the sole CUDA GPU; nvidia-smi reported $reported devices"

  card_count="$(find "$(cuda_profile_pci_path "$RTX5070_BDF")/drm" -maxdepth 1 -name 'card*' -print 2>/dev/null | wc -l)"
  [ "$card_count" -ge 1 ] \
    || cuda_profile_die "RTX 5070 Ti has no DRM card node; display mode is not ready"
  cuda_profile_log "Verified host: RTX 5070 Ti uses NVIDIA Open R${CUDA_PROFILE_DRIVER_BRANCH} for CUDA and display."
}

verify_5070_mode() {
  verify_5070_host
  cuda_profile_assert_node_env 5070 "$RTX5070_GPU_UUID"
  cuda_profile_verify_node rtx5070ti-16gb
  cuda_profile_verify_container_gpu_uuid "$RTX5070_GPU_UUID"
  cuda_profile_log "Verified LlamaFarm profile: rtx5070ti-16gb."
}

apply_5070_mode() {
  cuda_profile_require_root
  preflight_5070
  cuda_profile_disable_v100_services
  clear_5070_driver_override
  cuda_profile_install_driver_package "$OPEN_PACKAGE"

  if ! (verify_5070_host); then
    cuda_profile_log "NVIDIA Open is staged, but the live kernel/display stack is not ready."
    cuda_profile_log "Run: sudo $0 --reboot"
    return "$CUDA_PROFILE_REBOOT_REQUIRED"
  fi

  # Re-run outside the readiness subshell so its exact UUID is retained.
  verify_5070_host
  cuda_profile_persist_node_env 5070 "$RTX5070_GPU_UUID"
  cuda_profile_deploy_node rtx5070ti-16gb
  cuda_profile_verify_node rtx5070ti-16gb
  cuda_profile_verify_container_gpu_uuid "$RTX5070_GPU_UUID"
  cuda_profile_commit_node_env
  cuda_profile_log "RTX 5070 Ti CUDA mode is active; named model/config/data volumes were preserved."
}

case "$ACTION" in
  --preflight|preflight)
    preflight_5070
    ;;
  --apply|apply)
    apply_5070_mode
    ;;
  --verify|verify)
    verify_5070_mode
    ;;
  --reboot|reboot)
    set +e
    "$SCRIPT_DIR/cuda507.sh" --apply
    status=$?
    set -e
    if [ "$status" -eq 0 ]; then
      cuda_profile_log "The requested profile is already live; reboot was not needed."
    elif [ "$status" -eq "$CUDA_PROFILE_REBOOT_REQUIRED" ]; then
      cuda_profile_request_reboot
    else
      exit "$status"
    fi
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
