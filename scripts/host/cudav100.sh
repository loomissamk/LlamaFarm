#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/host/cuda-profile-common.sh
source "$SCRIPT_DIR/cuda-profile-common.sh"

ACTION="${1:---apply}"
PROPRIETARY_PACKAGE="${LLAMAFARM_NVIDIA_PROPRIETARY_PACKAGE:-nvidia-driver-${CUDA_PROFILE_DRIVER_BRANCH}}"
HOST_CONFIG="${LLAMAFARM_V100_HOST_CONFIG:-/etc/llamafarm/v100-cuda-5070-nvk.env}"
MIXED_SERVICE=llamafarm-v100-cuda-5070-nvk.service
INSTALLED_HELPER="${LLAMAFARM_INSTALLED_V100_HELPER:-/usr/local/libexec/llamafarm/v100-cuda-5070-nvk}"
SYSTEMD_UNIT_DIR="${LLAMAFARM_SYSTEMD_UNIT_DIR:-/etc/systemd/system}"
V100_PCI_BDF=""
V100_GPU_UUID=""
DISPLAY_GPU_PCI_BDF=""

usage() {
  cat <<'USAGE'
Usage: sudo ./scripts/host/cudav100.sh [--preflight|--apply|--verify|--reboot]

Selects one configured Tesla V100 as LlamaFarm's only CUDA GPU. The command
requires the V100 to be physically present before changing anything, installs
NVIDIA proprietary R580 when needed, keeps the configured RTX 5070 Ti out of
CUDA through the guarded Nouveau binding, and deploys v100-32gb by exact UUID.

--preflight  Validate both PCI identities and the V100 UUID; no changes.
--apply      Stage/verify the driver and deploy. Default. Exit 10 means reboot.
--verify     Verify proprietary V100 CUDA, 5070 exclusion, and the node.
--reboot     Run --apply and reboot only when activation is actually required.
USAGE
}

read_v100_config() {
  local required
  [ -r "$HOST_CONFIG" ] \
    || cuda_profile_die "V100 host configuration is not readable: $HOST_CONFIG"
  # Root-owned hardware identifiers only. See deploy/host-profiles/README.md.
  # shellcheck disable=SC1090
  source "$HOST_CONFIG"
  for required in \
    V100_PCI_BDF V100_PCI_VENDOR_ID V100_PCI_DEVICE_ID V100_GPU_UUID \
    DISPLAY_GPU_PCI_BDF DISPLAY_GPU_PCI_VENDOR_ID DISPLAY_GPU_PCI_DEVICE_ID; do
    [ -n "${!required:-}" ] || cuda_profile_die "missing V100 host setting: $required"
    [[ "${!required}" != REPLACE_WITH_* ]] || cuda_profile_die "replace V100 host placeholder: $required"
  done
  V100_PCI_BDF="$(cuda_profile_normalize_bdf "$V100_PCI_BDF")"
  DISPLAY_GPU_PCI_BDF="$(cuda_profile_normalize_bdf "$DISPLAY_GPU_PCI_BDF")"
  [[ "$V100_GPU_UUID" =~ ^GPU-[[:xdigit:]]{8}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{12}$ ]] \
    || cuda_profile_die "V100_GPU_UUID must be one full GPU UUID"
}

preflight_v100() {
  read_v100_config
  cuda_profile_validate_display_function V100 "$V100_PCI_BDF" "$V100_PCI_DEVICE_ID"
  cuda_profile_validate_display_function "RTX 5070 Ti" "$DISPLAY_GPU_PCI_BDF" "$DISPLAY_GPU_PCI_DEVICE_ID"
  [ "$(cuda_profile_normalize_id "$V100_PCI_VENDOR_ID")" = 10de ] \
    || cuda_profile_die "configured V100 vendor is not NVIDIA"
  [ "$(cuda_profile_normalize_id "$DISPLAY_GPU_PCI_VENDOR_ID")" = 10de ] \
    || cuda_profile_die "configured display GPU vendor is not NVIDIA"
  [ "$V100_PCI_BDF" != "$DISPLAY_GPU_PCI_BDF" ] \
    || cuda_profile_die "V100 and 5070 Ti cannot use the same PCI function"
  cuda_profile_log "Preflight passed: configured V100 and RTX 5070 Ti are physically present."
}

install_v100_binding_service() {
  local source_helper="$CUDA_PROFILE_ROOT_DIR/scripts/host/v100-cuda-5070-nvk.sh"
  local source_unit="$CUDA_PROFILE_ROOT_DIR/deploy/systemd/$MIXED_SERVICE"
  [ -x "$source_helper" ] || cuda_profile_die "missing guarded binding helper: $source_helper"
  [ -r "$source_unit" ] || cuda_profile_die "missing guarded binding unit: $source_unit"
  cuda_profile_require_command install
  cuda_profile_require_command systemctl
  install -D -m 0755 "$source_helper" "$INSTALLED_HELPER"
  install -D -m 0644 "$source_unit" "$SYSTEMD_UNIT_DIR/$MIXED_SERVICE"
  systemctl daemon-reload
  systemctl enable "$MIXED_SERVICE"
}

verify_v100_host() {
  preflight_v100 >/dev/null
  cuda_profile_verify_proprietary_module
  [ -x "$INSTALLED_HELPER" ] \
    || cuda_profile_die "installed V100 binding helper is missing: $INSTALLED_HELPER"
  "$INSTALLED_HELPER" verify "$HOST_CONFIG"
  cuda_profile_log "Verified host: V100 $V100_GPU_UUID is the CUDA GPU; RTX 5070 Ti is excluded."
}

verify_v100_mode() {
  verify_v100_host
  cuda_profile_assert_node_env v100 "$V100_GPU_UUID"
  cuda_profile_verify_node v100-32gb
  cuda_profile_verify_container_gpu_uuid "$V100_GPU_UUID"
  cuda_profile_log "Verified LlamaFarm profile: v100-32gb pinned to $V100_GPU_UUID."
}

apply_v100_mode() {
  cuda_profile_require_root
  # Deliberately first: a removed V100 must cause zero apt/systemd/container mutations.
  preflight_v100
  install_v100_binding_service
  cuda_profile_install_driver_package "$PROPRIETARY_PACKAGE"

  if ! (cuda_profile_verify_proprietary_module); then
    cuda_profile_log "NVIDIA proprietary R${CUDA_PROFILE_DRIVER_BRANCH} is staged, but not live."
    cuda_profile_log "Run: sudo $0 --reboot"
    return "$CUDA_PROFILE_REBOOT_REQUIRED"
  fi

  systemctl restart "$MIXED_SERVICE"
  verify_v100_host
  cuda_profile_persist_node_env v100 "$V100_GPU_UUID"
  cuda_profile_deploy_node v100-32gb
  cuda_profile_verify_node v100-32gb
  cuda_profile_verify_container_gpu_uuid "$V100_GPU_UUID"
  cuda_profile_commit_node_env
  cuda_profile_log "V100 CUDA mode is active; named model/config/data volumes were preserved."
}

case "$ACTION" in
  --preflight|preflight)
    preflight_v100
    ;;
  --apply|apply)
    apply_v100_mode
    ;;
  --verify|verify)
    verify_v100_mode
    ;;
  --reboot|reboot)
    set +e
    "$SCRIPT_DIR/cudav100.sh" --apply
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
