#!/usr/bin/env bash
# Shared implementation for cuda507.sh and cudav100.sh.

CUDA_PROFILE_ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CUDA_PROFILE_DRIVER_BRANCH="${LLAMAFARM_NVIDIA_DRIVER_BRANCH:-580}"
CUDA_PROFILE_PCI_ROOT="${LLAMAFARM_PCI_SYSFS_ROOT:-/sys/bus/pci}"
CUDA_PROFILE_MODULE_ROOT="${LLAMAFARM_MODULE_SYSFS_ROOT:-/sys/module}"
CUDA_PROFILE_DRI_ROOT="${LLAMAFARM_DRI_ROOT:-/dev/dri}"
CUDA_PROFILE_DEPLOY_NODE_ENV=""
CUDA_PROFILE_NODE_ENV_SOURCE=""
CUDA_PROFILE_NODE_ENV_BACKUP=""
CUDA_PROFILE_NODE_ENV_PENDING=0
CUDA_PROFILE_REBOOT_REQUIRED=10

cuda_profile_log() {
  printf '%s\n' "$*"
}

cuda_profile_die() {
  printf 'LlamaFarm CUDA profile: %s\n' "$*" >&2
  exit 1
}

cuda_profile_restore_node_env() {
  local restore_tmp
  [ "$CUDA_PROFILE_NODE_ENV_PENDING" -eq 1 ] || return 0
  [ -r "$CUDA_PROFILE_NODE_ENV_BACKUP" ] && [ -n "$CUDA_PROFILE_NODE_ENV_SOURCE" ] \
    || return 1
  restore_tmp="$(mktemp "$(dirname "$CUDA_PROFILE_NODE_ENV_SOURCE")/.llamafarm-node-restore.XXXXXX")" \
    || return 1
  if ! cp --preserve=mode,ownership,timestamps -- "$CUDA_PROFILE_NODE_ENV_BACKUP" "$restore_tmp" \
    || ! mv -f -- "$restore_tmp" "$CUDA_PROFILE_NODE_ENV_SOURCE"; then
    rm -f -- "$restore_tmp"
    return 1
  fi
  CUDA_PROFILE_NODE_ENV_PENDING=0
  cuda_profile_log "Restored private node configuration after the failed profile change."
}

cuda_profile_cleanup() {
  local original_status="$1"
  if [ "$CUDA_PROFILE_NODE_ENV_PENDING" -eq 1 ]; then
    set +e
    cuda_profile_restore_node_env
    restore_status=$?
    set -e
    if [ "$restore_status" -ne 0 ]; then
      printf 'LlamaFarm CUDA profile: automatic node.env rollback failed; restore %s from %s\n' \
        "$CUDA_PROFILE_NODE_ENV_SOURCE" "$CUDA_PROFILE_NODE_ENV_BACKUP" >&2
      [ "$original_status" -ne 0 ] || original_status=1
    fi
  fi
  exit "$original_status"
}

trap 'cuda_profile_cleanup $?' EXIT

cuda_profile_require_command() {
  command -v "$1" >/dev/null 2>&1 \
    || cuda_profile_die "required command not found: $1"
}

cuda_profile_require_root() {
  [ "$(id -u)" -eq 0 ] \
    || cuda_profile_die "this action changes the host driver and must run as root"
}

cuda_profile_normalize_id() {
  local value="${1,,}"
  printf '%s' "${value#0x}"
}

cuda_profile_normalize_bdf() {
  local value="${1,,}"
  local domain="${value%%:*}"
  local rest="${value#*:}"
  if [ "${#domain}" -gt 4 ]; then
    domain="${domain: -4}"
  fi
  printf '%04s:%s' "$domain" "$rest" | tr ' ' '0'
}

cuda_profile_pci_path() {
  printf '%s/devices/%s' "$CUDA_PROFILE_PCI_ROOT" "$1"
}

cuda_profile_pci_driver() {
  local driver_link
  driver_link="$(cuda_profile_pci_path "$1")/driver"
  if [ -L "$driver_link" ]; then
    basename "$(readlink "$driver_link")"
  fi
}

cuda_profile_validate_display_function() {
  local label="$1"
  local bdf="$2"
  local expected_device="${3:-}"
  local path vendor device class

  path="$(cuda_profile_pci_path "$bdf")"
  [ -d "$path" ] || cuda_profile_die "$label is not present at PCI function $bdf"
  [ -r "$path/vendor" ] && [ -r "$path/device" ] && [ -r "$path/class" ] \
    || cuda_profile_die "cannot read PCI identity for $label at $bdf"
  vendor="$(cuda_profile_normalize_id "$(<"$path/vendor")")"
  device="$(cuda_profile_normalize_id "$(<"$path/device")")"
  class="$(<"$path/class")"

  [ "$vendor" = 10de ] || cuda_profile_die "$label at $bdf is not an NVIDIA device"
  [[ "${class,,}" == 0x03* ]] \
    || cuda_profile_die "$label at $bdf is not a display-class PCI function"
  if [ -n "$expected_device" ] && [ "$device" != "$(cuda_profile_normalize_id "$expected_device")" ]; then
    cuda_profile_die "$label device ID mismatch at $bdf (found $device)"
  fi
}

cuda_profile_module_license() {
  modinfo -F license nvidia 2>/dev/null | head -n1
}

cuda_profile_loaded_taint() {
  local taint_file="$CUDA_PROFILE_MODULE_ROOT/nvidia/taint"
  [ -r "$taint_file" ] && cat "$taint_file"
}

cuda_profile_verify_open_module() {
  local license taint
  cuda_profile_require_command modinfo
  [ -d "$CUDA_PROFILE_MODULE_ROOT/nvidia" ] \
    || cuda_profile_die "the NVIDIA kernel module is not loaded"
  license="$(cuda_profile_module_license)"
  taint="$(cuda_profile_loaded_taint)"
  [[ "$license" == *MIT* || "$license" == *GPL* ]] \
    || cuda_profile_die "the loaded NVIDIA stack is not the Open R${CUDA_PROFILE_DRIVER_BRANCH} variant (license: ${license:-unknown})"
  [[ "$taint" != *P* ]] \
    || cuda_profile_die "the loaded NVIDIA module is proprietary; the 5070 Ti mode needs NVIDIA Open"
}

cuda_profile_verify_proprietary_module() {
  local license taint
  cuda_profile_require_command modinfo
  [ -d "$CUDA_PROFILE_MODULE_ROOT/nvidia" ] \
    || cuda_profile_die "the NVIDIA kernel module is not loaded"
  license="$(cuda_profile_module_license)"
  taint="$(cuda_profile_loaded_taint)"
  [ "$license" = NVIDIA ] \
    || cuda_profile_die "the loaded NVIDIA stack is not the proprietary R${CUDA_PROFILE_DRIVER_BRANCH} variant (license: ${license:-unknown})"
  [[ "$taint" == *P* ]] \
    || cuda_profile_die "the loaded NVIDIA module is not marked proprietary"
}

cuda_profile_package_installed() {
  dpkg-query -W -f='${db:Status-Abbrev}' "$1" 2>/dev/null | grep -q '^ii '
}

cuda_profile_install_driver_package() {
  local package="$1"
  cuda_profile_require_root
  cuda_profile_require_command apt-get
  cuda_profile_require_command dpkg-query

  if cuda_profile_package_installed "$package"; then
    cuda_profile_log "Driver package already installed: $package"
    return 0
  fi

  cuda_profile_log "Installing driver package: $package"
  DEBIAN_FRONTEND=noninteractive apt-get install --yes "$package"
  if command -v update-initramfs >/dev/null 2>&1; then
    update-initramfs -u -k all
  fi
}

cuda_profile_operator_home() {
  local operator="${SUDO_USER:-}"
  local home=""
  if [ -n "$operator" ] && [ "$operator" != root ] && command -v getent >/dev/null 2>&1; then
    home="$(getent passwd "$operator" | awk -F: 'NR == 1 {print $6}')"
  fi
  printf '%s' "${home:-$HOME}"
}

cuda_profile_private_node_env() {
  if [ -n "${LLAMAFARM_NODE_ENV:-}" ]; then
    printf '%s' "$LLAMAFARM_NODE_ENV"
  else
    printf '%s/.config/llamafarm/node.env' "$(cuda_profile_operator_home)"
  fi
}

cuda_profile_node_env_matches() {
  local mode="$1"
  local uuid="$2"
  local source_env
  source_env="${3:-$(cuda_profile_private_node_env)}"
  (
    unset LLAMAFARM_NVIDIA_VISIBLE_DEVICES LLAMAFARM_NVIDIA_REQUIRE_EXACT_UUID
    unset LLAMAFARM_NVIDIA_COMPUTE_ONLY LLAMAFARM_EXPOSE_DRI
    # shellcheck disable=SC1090
    source "$source_env"
    [ "${LLAMAFARM_NVIDIA_VISIBLE_DEVICES:-}" = "$uuid" ] || exit 1
    [ "${LLAMAFARM_NVIDIA_REQUIRE_EXACT_UUID:-}" = true ] || exit 1
    if [ "$mode" = v100 ]; then
      [ "${LLAMAFARM_NVIDIA_COMPUTE_ONLY:-}" = true ] || exit 1
      [ "${LLAMAFARM_EXPOSE_DRI:-}" = false ] || exit 1
    else
      [ "${LLAMAFARM_NVIDIA_COMPUTE_ONLY:-}" = false ] || exit 1
      [ "${LLAMAFARM_EXPOSE_DRI:-}" = true ] || exit 1
    fi
  )
}

cuda_profile_assert_node_env() {
  local mode="$1"
  local uuid="$2"
  local source_env
  source_env="$(cuda_profile_private_node_env)"
  [ -r "$source_env" ] \
    || cuda_profile_die "private node configuration is not readable: $source_env"
  cuda_profile_node_env_matches "$mode" "$uuid" "$source_env" \
    || cuda_profile_die "private node configuration does not durably select $mode GPU $uuid; run --apply"
  CUDA_PROFILE_DEPLOY_NODE_ENV="$source_env"
}

cuda_profile_persist_node_env() {
  local mode="$1"
  local uuid="$2"
  local source_env source_dir source_base backup_tmp new_tmp
  source_env="$(cuda_profile_private_node_env)"
  [ -r "$source_env" ] \
    || cuda_profile_die "private node configuration is not readable: $source_env"
  source_env="$(readlink -f "$source_env")"
  source_dir="$(dirname "$source_env")"
  source_base="$(basename "$source_env")"
  CUDA_PROFILE_DEPLOY_NODE_ENV="$source_env"

  if cuda_profile_node_env_matches "$mode" "$uuid" "$source_env"; then
    cuda_profile_log "Private node configuration already durably selects $mode GPU $uuid."
    return 0
  fi

  cuda_profile_require_root
  cuda_profile_require_command awk
  cuda_profile_require_command cp
  cuda_profile_require_command mktemp
  cuda_profile_require_command mv
  [ -w "$source_env" ] && [ -w "$source_dir" ] \
    || cuda_profile_die "private node configuration cannot be updated atomically: $source_env"

  CUDA_PROFILE_NODE_ENV_SOURCE="$source_env"
  CUDA_PROFILE_NODE_ENV_BACKUP="${LLAMAFARM_NODE_ENV_BACKUP:-${source_env}.llamafarm-gpu-backup}"
  backup_tmp="$(mktemp "$source_dir/.${source_base}.backup.XXXXXX")"
  cp --preserve=mode,ownership,timestamps -- "$source_env" "$backup_tmp"
  mv -f -- "$backup_tmp" "$CUDA_PROFILE_NODE_ENV_BACKUP"

  new_tmp="$(mktemp "$source_dir/.${source_base}.gpu-profile.XXXXXX")"
  awk '
    !/^[[:space:]]*(export[[:space:]]+)?LLAMAFARM_NVIDIA_VISIBLE_DEVICES=/ &&
    !/^[[:space:]]*(export[[:space:]]+)?LLAMAFARM_NVIDIA_REQUIRE_EXACT_UUID=/ &&
    !/^[[:space:]]*(export[[:space:]]+)?LLAMAFARM_NVIDIA_COMPUTE_ONLY=/ &&
    !/^[[:space:]]*(export[[:space:]]+)?LLAMAFARM_EXPOSE_DRI=/ &&
    !/^[[:space:]]*(export[[:space:]]+)?LLAMAFARM_V100_HOST_CONFIG=/ { print }
  ' "$source_env" >"$new_tmp"

  {
    printf '\n# Managed by LlamaFarm CUDA profile switchers.\n'
    printf 'LLAMAFARM_NVIDIA_VISIBLE_DEVICES=%s\n' "$uuid"
    printf 'LLAMAFARM_NVIDIA_REQUIRE_EXACT_UUID=true\n'
    if [ "$mode" = v100 ]; then
      printf 'LLAMAFARM_NVIDIA_COMPUTE_ONLY=true\n'
      printf 'LLAMAFARM_EXPOSE_DRI=false\n'
    else
      printf 'LLAMAFARM_NVIDIA_COMPUTE_ONLY=false\n'
      printf 'LLAMAFARM_EXPOSE_DRI=true\n'
    fi
  } >>"$new_tmp"
  chmod --reference="$source_env" "$new_tmp"
  chown --reference="$source_env" "$new_tmp"
  mv -f -- "$new_tmp" "$source_env"
  CUDA_PROFILE_NODE_ENV_PENDING=1
  cuda_profile_log "Updated $source_env atomically; prior GPU selection is backed up at $CUDA_PROFILE_NODE_ENV_BACKUP."
}

cuda_profile_commit_node_env() {
  CUDA_PROFILE_NODE_ENV_PENDING=0
}

cuda_profile_deploy_node() {
  local profile="$1"
  local up_node="${LLAMAFARM_UP_NODE_SCRIPT:-$CUDA_PROFILE_ROOT_DIR/scripts/docker/up-node.sh}"
  local home
  [ -x "$up_node" ] || cuda_profile_die "node deployment helper is not executable: $up_node"
  [ -n "$CUDA_PROFILE_DEPLOY_NODE_ENV" ] \
    || cuda_profile_die "internal error: deployment node environment was not selected"
  home="$(cuda_profile_operator_home)"
  cuda_profile_log "Deploying LlamaFarm node profile: $profile"
  env HOME="$home" LLAMAFARM_NODE_ENV="$CUDA_PROFILE_DEPLOY_NODE_ENV" \
    "$up_node" "$profile" up -d --build --force-recreate
}

cuda_profile_verify_node() {
  local profile="$1"
  local check_node="${LLAMAFARM_CHECK_NODE_SCRIPT:-$CUDA_PROFILE_ROOT_DIR/scripts/docker/check-node.sh}"
  local home
  [ -x "$check_node" ] || cuda_profile_die "node verification helper is not executable: $check_node"
  [ -n "$CUDA_PROFILE_DEPLOY_NODE_ENV" ] \
    || cuda_profile_die "internal error: verification node environment was not selected"
  home="$(cuda_profile_operator_home)"
  env HOME="$home" LLAMAFARM_NODE_ENV="$CUDA_PROFILE_DEPLOY_NODE_ENV" \
    "$check_node" "$profile"
}

cuda_profile_verify_container_gpu_uuid() {
  local expected_uuid="$1"
  local container_name="${LLAMAFARM_CONTAINER_NAME:-LlamaFarm}"
  local visible
  local -a uuids=()
  cuda_profile_require_command docker

  mapfile -t uuids < <(
    docker exec "$container_name" \
      nvidia-smi --query-gpu=uuid --format=csv,noheader,nounits
  )
  if [ "${#uuids[@]}" -ne 1 ] \
    || [ "${uuids[0]//[[:space:]]/}" != "$expected_uuid" ]; then
    cuda_profile_die "container does not expose exactly the selected GPU UUID $expected_uuid"
  fi

  visible="$(docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' "$container_name" \
    | awk -F= '$1 == "NVIDIA_VISIBLE_DEVICES" {sub(/^[^=]*=/, ""); print; exit}')"
  [ "$visible" = "$expected_uuid" ] \
    || cuda_profile_die "container NVIDIA_VISIBLE_DEVICES is not pinned to $expected_uuid"
}

cuda_profile_disable_v100_services() {
  local unit
  local units=(llamafarm-v100-cuda-5070-nvk.service llamafarm-v100-profile.service)
  local -A seen=()
  cuda_profile_require_root
  cuda_profile_require_command systemctl

  while read -r unit _; do
    [[ "$unit" == llamafarm*v100*.service ]] || continue
    units+=("$unit")
  done < <(systemctl list-unit-files --type=service --no-legend 'llamafarm*v100*.service' 2>/dev/null || true)

  for unit in "${units[@]}"; do
    [ -n "$unit" ] || continue
    [ -z "${seen[$unit]:-}" ] || continue
    seen[$unit]=1
    if systemctl list-unit-files --type=service --no-legend "$unit" 2>/dev/null \
      | awk -v wanted="$unit" '$1 == wanted { found=1 } END { exit !found }'; then
      systemctl disable --now "$unit"
      if systemctl is-active --quiet "$unit"; then
        cuda_profile_die "V100-specific service is still active: $unit"
      fi
      if systemctl is-enabled --quiet "$unit"; then
        cuda_profile_die "V100-specific service is still enabled: $unit"
      fi
    fi
  done
}

cuda_profile_request_reboot() {
  cuda_profile_require_root
  cuda_profile_require_command systemctl
  cuda_profile_log "Rebooting to activate the staged NVIDIA driver profile."
  systemctl reboot
}
