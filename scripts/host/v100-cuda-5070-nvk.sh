#!/usr/bin/env bash
set -euo pipefail

DEFAULT_CONFIG=/etc/llamafarm/v100-cuda-5070-nvk.env
ACTION="${1:-verify}"
CONFIG_FILE="${2:-${LLAMAFARM_V100_HOST_CONFIG:-$DEFAULT_CONFIG}}"
PCI_ROOT="${LLAMAFARM_PCI_SYSFS_ROOT:-/sys/bus/pci}"
MODULE_ROOT="${LLAMAFARM_MODULE_SYSFS_ROOT:-/sys/module}"
DRI_ROOT="${LLAMAFARM_DRI_ROOT:-/dev/dri}"

usage() {
  cat <<'USAGE'
Usage: v100-cuda-5070-nvk.sh <preflight|apply|verify|rollback> [config-file]

Keeps one configured V100 on the proprietary NVIDIA driver and binds only the
configured RTX 5070 Ti display function to Nouveau. It never binds or exposes
the 5070 Ti as an inference device.
USAGE
}

die() {
  echo "v100-cuda-5070-nvk: $*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

normalize_id() {
  local value="${1,,}"
  printf '%s' "${value#0x}"
}

normalize_bdf() {
  local value="${1,,}"
  local domain="${value%%:*}"
  local rest="${value#*:}"
  if [ "${#domain}" -gt 4 ]; then
    domain="${domain: -4}"
  fi
  printf '%s:%s' "$domain" "$rest"
}

pci_device_path() {
  printf '%s/devices/%s' "$PCI_ROOT" "$1"
}

pci_driver() {
  local link
  link="$(pci_device_path "$1")/driver"
  if [ -L "$link" ]; then
    basename "$(readlink "$link")"
  fi
}

read_config() {
  [ -r "$CONFIG_FILE" ] || die "configuration is not readable: $CONFIG_FILE"

  # The installed file is root-owned, contains identifiers only, and is also
  # consumed by systemd. Treat it as trusted shell-style KEY=value input.
  # shellcheck disable=SC1090
  source "$CONFIG_FILE"

  local required
  for required in \
    V100_PCI_BDF V100_PCI_VENDOR_ID V100_PCI_DEVICE_ID V100_GPU_UUID \
    DISPLAY_GPU_PCI_BDF DISPLAY_GPU_PCI_VENDOR_ID DISPLAY_GPU_PCI_DEVICE_ID; do
    [ -n "${!required:-}" ] || die "missing configuration value: $required"
    [[ "${!required}" != REPLACE_WITH_* ]] || die "replace placeholder: $required"
  done

  [[ "$V100_PCI_BDF" =~ ^[[:xdigit:]]{4}:[[:xdigit:]]{2}:[[:xdigit:]]{2}\.[0-7]$ ]] \
    || die "invalid V100_PCI_BDF: $V100_PCI_BDF"
  [[ "$DISPLAY_GPU_PCI_BDF" =~ ^[[:xdigit:]]{4}:[[:xdigit:]]{2}:[[:xdigit:]]{2}\.[0-7]$ ]] \
    || die "invalid DISPLAY_GPU_PCI_BDF: $DISPLAY_GPU_PCI_BDF"
  [[ "$(normalize_id "$V100_PCI_VENDOR_ID")" =~ ^[[:xdigit:]]{4}$ ]] \
    || die "invalid V100_PCI_VENDOR_ID"
  [[ "$(normalize_id "$V100_PCI_DEVICE_ID")" =~ ^[[:xdigit:]]{4}$ ]] \
    || die "invalid V100_PCI_DEVICE_ID"
  [[ "$(normalize_id "$DISPLAY_GPU_PCI_VENDOR_ID")" =~ ^[[:xdigit:]]{4}$ ]] \
    || die "invalid DISPLAY_GPU_PCI_VENDOR_ID"
  [[ "$(normalize_id "$DISPLAY_GPU_PCI_DEVICE_ID")" =~ ^[[:xdigit:]]{4}$ ]] \
    || die "invalid DISPLAY_GPU_PCI_DEVICE_ID"
  [[ "$V100_GPU_UUID" =~ ^GPU-[[:xdigit:]]{8}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{12}$ ]] \
    || die "V100_GPU_UUID must be one full GPU UUID"

  V100_PCI_BDF="$(normalize_bdf "$V100_PCI_BDF")"
  DISPLAY_GPU_PCI_BDF="$(normalize_bdf "$DISPLAY_GPU_PCI_BDF")"
  [ "$V100_PCI_BDF" != "$DISPLAY_GPU_PCI_BDF" ] \
    || die "V100 and display GPU PCI functions must be different"
}

validate_pci_identity() {
  local label="$1"
  local bdf="$2"
  local expected_vendor="$3"
  local expected_device="$4"
  local path actual_vendor actual_device class

  path="$(pci_device_path "$bdf")"
  [ -d "$path" ] || die "$label PCI function not found: $bdf"
  actual_vendor="$(normalize_id "$(<"$path/vendor")")"
  actual_device="$(normalize_id "$(<"$path/device")")"
  class="$(<"$path/class")"

  [ "$actual_vendor" = "$(normalize_id "$expected_vendor")" ] \
    || die "$label vendor mismatch at $bdf"
  [ "$actual_device" = "$(normalize_id "$expected_device")" ] \
    || die "$label device mismatch at $bdf"
  [[ "${class,,}" == 0x03* ]] || die "$label is not a PCI display-class function: $bdf"
}

validate_v100() {
  local expected_bdf found=0 bus uuid nvidia_license nvidia_taint

  validate_pci_identity V100 "$V100_PCI_BDF" "$V100_PCI_VENDOR_ID" "$V100_PCI_DEVICE_ID"
  [ "$(pci_driver "$V100_PCI_BDF")" = nvidia ] \
    || die "V100 $V100_PCI_BDF is not bound to the proprietary nvidia driver"
  require_command modinfo
  nvidia_license="$(modinfo -F license nvidia)"
  [ "$nvidia_license" = NVIDIA ] \
    || die "loaded NVIDIA module is not the proprietary module (license: ${nvidia_license:-unknown})"
  [ -r "$MODULE_ROOT/nvidia/taint" ] \
    || die "cannot verify loaded NVIDIA module taint at $MODULE_ROOT/nvidia/taint"
  nvidia_taint="$(<"$MODULE_ROOT/nvidia/taint")"
  [[ "$nvidia_taint" == *P* ]] \
    || die "loaded NVIDIA module is not marked proprietary"
  require_command nvidia-smi
  expected_bdf="$(normalize_bdf "$V100_PCI_BDF")"

  while IFS=, read -r bus uuid; do
    bus="${bus//[[:space:]]/}"
    uuid="${uuid//[[:space:]]/}"
    if [ "$(normalize_bdf "$bus")" = "$expected_bdf" ]; then
      [ "$uuid" = "$V100_GPU_UUID" ] \
        || die "V100 UUID mismatch at $V100_PCI_BDF"
      found=$((found + 1))
    fi
  done < <(nvidia-smi --query-gpu=pci.bus_id,uuid --format=csv,noheader,nounits)

  [ "$found" -eq 1 ] || die "nvidia-smi did not report exactly one configured V100 at $V100_PCI_BDF"
}

validate_display_identity() {
  validate_pci_identity "5070 Ti display" "$DISPLAY_GPU_PCI_BDF" \
    "$DISPLAY_GPU_PCI_VENDOR_ID" "$DISPLAY_GPU_PCI_DEVICE_ID"
}

resolve_nouveau_module() {
  local module_path
  require_command modinfo
  module_path="$(modinfo -n nouveau)"
  [[ "$module_path" == /* ]] \
    || die "modinfo did not return an absolute nouveau module path: ${module_path:-empty}"
  [ -f "$module_path" ] \
    || die "resolved nouveau module file does not exist: $module_path"
  printf '%s' "$module_path"
}

validate_display_dri() {
  local card_path render_path
  require_command udevadm
  udevadm settle --timeout=30
  card_path="$DRI_ROOT/by-path/pci-$DISPLAY_GPU_PCI_BDF-card"
  render_path="$DRI_ROOT/by-path/pci-$DISPLAY_GPU_PCI_BDF-render"
  [ -e "$card_path" ] \
    || die "5070 Ti card node missing after udev settle: $card_path"
  [ -e "$render_path" ] \
    || die "5070 Ti render node missing after udev settle: $render_path"
}

require_root() {
  [ "$(id -u)" -eq 0 ] || die "$ACTION requires root"
}

set_driver_override() {
  local driver="$1"
  if [ -n "$driver" ]; then
    printf '%s' "$driver" >"$(pci_device_path "$DISPLAY_GPU_PCI_BDF")/driver_override"
  else
    printf '\n' >"$(pci_device_path "$DISPLAY_GPU_PCI_BDF")/driver_override"
  fi
}

unbind_display() {
  local driver="$1"
  [ -n "$driver" ] || return 0
  printf '%s' "$DISPLAY_GPU_PCI_BDF" \
    >"$PCI_ROOT/drivers/$driver/unbind"
}

probe_display() {
  printf '%s' "$DISPLAY_GPU_PCI_BDF" >"$PCI_ROOT/drivers_probe"
}

apply_binding() {
  local current nouveau_module
  require_root
  validate_v100
  validate_display_identity
  nouveau_module="$(resolve_nouveau_module)"
  current="$(pci_driver "$DISPLAY_GPU_PCI_BDF")"

  if [ "$current" != nouveau ]; then
    set_driver_override nouveau
    if ! unbind_display "$current" \
      || ! modprobe --ignore-install "$nouveau_module" \
      || ! probe_display \
      || [ "$(pci_driver "$DISPLAY_GPU_PCI_BDF")" != nouveau ]; then
      set_driver_override nvidia
      current="$(pci_driver "$DISPLAY_GPU_PCI_BDF")"
      if [ -n "$current" ] && [ "$current" != nvidia ]; then
        unbind_display "$current" || true
      fi
      modprobe nvidia
      if [ "$(pci_driver "$DISPLAY_GPU_PCI_BDF")" != nvidia ]; then
        probe_display || true
      fi
      set_driver_override ""
      validate_v100
      die "failed to bind $DISPLAY_GPU_PCI_BDF to nouveau; attempted an nvidia restore"
    fi
  else
    set_driver_override nouveau
  fi

  [ "$(pci_driver "$DISPLAY_GPU_PCI_BDF")" = nouveau ] \
    || die "5070 Ti display function did not bind to nouveau"
  validate_display_dri
  validate_v100
  echo "Verified: V100 $V100_GPU_UUID uses nvidia; $DISPLAY_GPU_PCI_BDF uses nouveau."
}

verify_binding() {
  validate_v100
  validate_display_identity
  resolve_nouveau_module >/dev/null
  [ "$(pci_driver "$DISPLAY_GPU_PCI_BDF")" = nouveau ] \
    || die "5070 Ti display function is not bound to nouveau"
  validate_display_dri
  echo "Verified: V100 $V100_GPU_UUID uses nvidia; $DISPLAY_GPU_PCI_BDF uses nouveau."
}

rollback_binding() {
  local current
  require_root
  validate_v100
  validate_display_identity
  current="$(pci_driver "$DISPLAY_GPU_PCI_BDF")"

  if [ "$current" != nvidia ]; then
    set_driver_override nvidia
    unbind_display "$current"
    modprobe nvidia
    probe_display
  fi

  [ "$(pci_driver "$DISPLAY_GPU_PCI_BDF")" = nvidia ] \
    || die "5070 Ti display function did not return to nvidia"
  set_driver_override ""
  validate_v100
  echo "Rolled back: $DISPLAY_GPU_PCI_BDF uses nvidia; V100 validation still passes."
}

case "$ACTION" in
  preflight)
    read_config
    validate_v100
    validate_display_identity
    resolve_nouveau_module >/dev/null
    echo "Preflight passed for configured V100 and 5070 Ti display functions."
    ;;
  apply)
    read_config
    apply_binding
    ;;
  verify)
    read_config
    verify_binding
    ;;
  rollback)
    read_config
    rollback_binding
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
