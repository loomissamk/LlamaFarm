#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
CUDA507="$ROOT_DIR/scripts/host/cuda507.sh"
CUDAV100="$ROOT_DIR/scripts/host/cudav100.sh"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/llamafarm-cuda-profiles.XXXXXX")"
trap 'rm -rf "$TEST_ROOT"' EXIT

FAKE_BIN="$TEST_ROOT/bin"
PCI_ROOT="$TEST_ROOT/pci"
MODULE_ROOT="$TEST_ROOT/modules"
DRI_ROOT="$TEST_ROOT/dri"
STATE_ROOT="$TEST_ROOT/state"
NODE_ENV="$TEST_ROOT/node.env"
HOST_CONFIG="$TEST_ROOT/v100-host.env"
NOUVEAU_MODULE="$TEST_ROOT/lib/modules/nouveau.ko"
INSTALLED_HELPER="$TEST_ROOT/usr/local/libexec/llamafarm/v100-cuda-5070-nvk"
UNIT_DIR="$TEST_ROOT/etc/systemd/system"
mkdir -p \
  "$FAKE_BIN" "$PCI_ROOT/devices" "$PCI_ROOT/drivers/nvidia" \
  "$PCI_ROOT/drivers/nouveau" "$MODULE_ROOT/nvidia" "$DRI_ROOT/by-path" \
  "$STATE_ROOT" "$(dirname "$NOUVEAU_MODULE")" "$UNIT_DIR"
printf 'fixture\n' >"$NOUVEAU_MODULE"

make_gpu() {
  local bdf="$1"
  local device="$2"
  local driver="$3"
  local path="$PCI_ROOT/devices/$bdf"
  mkdir -p "$path/drm/card0"
  printf '0x10de\n' >"$path/vendor"
  printf '0x%s\n' "$device" >"$path/device"
  printf '0x030000\n' >"$path/class"
  printf '\n' >"$path/driver_override"
  ln -sfn "$PCI_ROOT/drivers/$driver" "$path/driver"
}

make_gpu 0000:01:00.0 2c05 nvidia
make_gpu 0000:02:00.0 1db6 nvidia
printf '\n' >"$MODULE_ROOT/nvidia/taint"
printf 'Dual MIT/GPL\n' >"$STATE_ROOT/license"

cat >"$NODE_ENV" <<'EOF'
LLAMAFARM_MANUAL_PEERS=http://example.com:42617
LLAMAFARM_FEDERATION_TOKEN=test-only-federation-token
LLAMAFARM_NVIDIA_VISIBLE_DEVICES=GPU-11111111-1111-1111-1111-111111111111
LLAMAFARM_NVIDIA_REQUIRE_EXACT_UUID=true
EOF
chmod 0600 "$NODE_ENV"
NODE_ENV_IDENTITY="$(stat -c '%u:%g:%a' "$NODE_ENV")"

cat >"$HOST_CONFIG" <<'EOF'
V100_PCI_BDF=0000:02:00.0
V100_PCI_VENDOR_ID=10de
V100_PCI_DEVICE_ID=1db6
V100_GPU_UUID=GPU-00000000-0000-0000-0000-000000000000
DISPLAY_GPU_PCI_BDF=0000:01:00.0
DISPLAY_GPU_PCI_VENDOR_ID=10de
DISPLAY_GPU_PCI_DEVICE_ID=2c05
EOF

cat >"$FAKE_BIN/id" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = -u ]; then printf '0\n'; else /usr/bin/id "$@"; fi
EOF
cat >"$FAKE_BIN/dpkg-query" <<'EOF'
#!/usr/bin/env bash
package="${*: -1}"
if [ "$package" = "${TEST_INSTALLED_PACKAGE:-}" ]; then
  printf 'ii '
else
  exit 1
fi
EOF
cat >"$FAKE_BIN/apt-get" <<'EOF'
#!/usr/bin/env bash
printf 'apt-get %s\n' "$*" >>"$TEST_STATE/apt.log"
EOF
cat >"$FAKE_BIN/update-initramfs" <<'EOF'
#!/usr/bin/env bash
printf 'update-initramfs %s\n' "$*" >>"$TEST_STATE/apt.log"
EOF
cat >"$FAKE_BIN/modinfo" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = -F ] && [ "${2:-}" = license ] && [ "${3:-}" = nvidia ]; then
  cat "$TEST_STATE/license"
elif [ "${1:-}" = -n ] && [ "${2:-}" = nouveau ]; then
  printf '%s\n' "$TEST_NOUVEAU_MODULE"
else
  exit 2
fi
EOF
cat >"$FAKE_BIN/nvidia-smi" <<'EOF'
#!/usr/bin/env bash
case "${TEST_GPU_MODE:-5070}" in
  5070)
    printf '00000000:01:00.0, NVIDIA GeForce RTX 5070 Ti, GPU-50705070-5070-5070-5070-507050705070\n'
    ;;
  v100)
    if [[ "$*" == *"pci.bus_id,uuid"* ]]; then
      printf '00000000:02:00.0, GPU-00000000-0000-0000-0000-000000000000\n'
    else
      printf '00000000:02:00.0, Tesla V100-PCIE-32GB, GPU-00000000-0000-0000-0000-000000000000\n'
    fi
    ;;
  *) exit 1 ;;
esac
EOF
cat >"$FAKE_BIN/udevadm" <<'EOF'
#!/usr/bin/env bash
[ "${1:-}" = settle ]
EOF
cat >"$FAKE_BIN/systemctl" <<'EOF'
#!/usr/bin/env bash
printf 'systemctl %s\n' "$*" >>"$TEST_STATE/systemctl.log"
case "${1:-}" in
  list-unit-files)
    printf 'llamafarm-v100-cuda-5070-nvk.service enabled\n'
    printf 'llamafarm-v100-profile.service enabled\n'
    ;;
  is-active|is-enabled)
    exit 1
    ;;
  *) ;;
esac
EOF
cat >"$FAKE_BIN/up-node" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >"$TEST_STATE/up.log"
cp "$LLAMAFARM_NODE_ENV" "$TEST_STATE/deployed-node.env"
[ "${TEST_UP_FAIL:-0}" != 1 ]
EOF
cat >"$FAKE_BIN/check-node" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >"$TEST_STATE/check.log"
cp "$LLAMAFARM_NODE_ENV" "$TEST_STATE/checked-node.env"
EOF
cat >"$FAKE_BIN/docker" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = exec ]; then
  case "${TEST_GPU_MODE:-5070}" in
    5070) printf 'GPU-50705070-5070-5070-5070-507050705070\n' ;;
    v100) printf 'GPU-00000000-0000-0000-0000-000000000000\n' ;;
  esac
elif [ "${1:-}" = inspect ]; then
  case "${TEST_GPU_MODE:-5070}" in
    5070) printf 'NVIDIA_VISIBLE_DEVICES=GPU-50705070-5070-5070-5070-507050705070\n' ;;
    v100) printf 'NVIDIA_VISIBLE_DEVICES=GPU-00000000-0000-0000-0000-000000000000\n' ;;
  esac
else
  exit 2
fi
EOF
chmod +x "$FAKE_BIN"/*

export PATH="$FAKE_BIN:/usr/bin:/bin"
export TEST_STATE="$STATE_ROOT"
export TEST_NOUVEAU_MODULE="$NOUVEAU_MODULE"
export TMPDIR="$TEST_ROOT/tmp"
export HOME="$TEST_ROOT/home"
export LLAMAFARM_NODE_ENV="$NODE_ENV"
export LLAMAFARM_PCI_SYSFS_ROOT="$PCI_ROOT"
export LLAMAFARM_MODULE_SYSFS_ROOT="$MODULE_ROOT"
export LLAMAFARM_DRI_ROOT="$DRI_ROOT"
export LLAMAFARM_UP_NODE_SCRIPT="$FAKE_BIN/up-node"
export LLAMAFARM_CHECK_NODE_SCRIPT="$FAKE_BIN/check-node"
export LLAMAFARM_V100_HOST_CONFIG="$HOST_CONFIG"
export LLAMAFARM_INSTALLED_V100_HELPER="$INSTALLED_HELPER"
export LLAMAFARM_SYSTEMD_UNIT_DIR="$UNIT_DIR"
export LLAMAFARM_5070_PCI_BDF=0000:01:00.0
mkdir -p "$TMPDIR" "$HOME"

# Ready 5070 mode deploys the correct profile and durably replaces stale pinning.
export TEST_INSTALLED_PACKAGE=nvidia-driver-580-open
export TEST_GPU_MODE=5070
: >"$STATE_ROOT/systemctl.log"
"$CUDA507" --apply >/dev/null
grep -q '^rtx5070ti-16gb up -d --build --force-recreate$' "$STATE_ROOT/up.log"
grep -q '^rtx5070ti-16gb$' "$STATE_ROOT/check.log"
grep -q '^LLAMAFARM_NVIDIA_VISIBLE_DEVICES=GPU-50705070-5070-5070-5070-507050705070$' "$NODE_ENV"
grep -q '^LLAMAFARM_NVIDIA_REQUIRE_EXACT_UUID=true$' "$NODE_ENV"
grep -q '^LLAMAFARM_NVIDIA_COMPUTE_ONLY=false$' "$NODE_ENV"
grep -q '^LLAMAFARM_EXPOSE_DRI=true$' "$NODE_ENV"
cmp -s "$NODE_ENV" "$STATE_ROOT/deployed-node.env"
[ -r "$NODE_ENV.llamafarm-gpu-backup" ]
[ "$(stat -c '%u:%g:%a' "$NODE_ENV")" = "$NODE_ENV_IDENTITY" ] || {
  echo "5070 mode changed private node configuration ownership or mode" >&2
  exit 1
}
grep -q 'disable --now llamafarm-v100-cuda-5070-nvk.service' "$STATE_ROOT/systemctl.log"

# A failed redeploy atomically restores the entire prior private file.
cp "$NODE_ENV.llamafarm-gpu-backup" "$NODE_ENV"
cp "$NODE_ENV" "$STATE_ROOT/node-before-failed-deploy.env"
export TEST_UP_FAIL=1
set +e
"$CUDA507" --apply >/dev/null 2>&1
status=$?
set -e
unset TEST_UP_FAIL
[ "$status" -ne 0 ] || {
  echo "5070 deploy failure returned success" >&2
  exit 1
}
cmp -s "$NODE_ENV" "$STATE_ROOT/node-before-failed-deploy.env" || {
  echo "5070 deploy failure did not restore the private node configuration" >&2
  exit 1
}

# A loaded proprietary module stages 5070 mode but never deploys prematurely.
printf 'NVIDIA\n' >"$STATE_ROOT/license"
printf 'P\n' >"$MODULE_ROOT/nvidia/taint"
rm -f "$STATE_ROOT/up.log"
set +e
"$CUDA507" --apply >/dev/null 2>&1
status=$?
set -e
[ "$status" -eq 10 ] || {
  echo "5070 apply did not report the reboot-required exit code" >&2
  exit 1
}
[ ! -e "$STATE_ROOT/up.log" ] || {
  echo "5070 apply deployed before NVIDIA Open was live" >&2
  exit 1
}

# A physically absent V100 fails before apt, systemd, or Docker mutation.
mv "$PCI_ROOT/devices/0000:02:00.0" "$PCI_ROOT/v100-removed"
: >"$STATE_ROOT/node-before-absent-v100.env"
cp "$NODE_ENV" "$STATE_ROOT/node-before-absent-v100.env"
: >"$STATE_ROOT/apt.log"
: >"$STATE_ROOT/systemctl.log"
rm -f "$STATE_ROOT/up.log"
export TEST_INSTALLED_PACKAGE=nvidia-driver-580
set +e
"$CUDAV100" --apply >/dev/null 2>&1
status=$?
set -e
[ "$status" -ne 0 ] || {
  echo "V100 apply accepted a physically absent V100" >&2
  exit 1
}
[ ! -s "$STATE_ROOT/apt.log" ] && [ ! -s "$STATE_ROOT/systemctl.log" ] && [ ! -e "$STATE_ROOT/up.log" ] || {
  echo "V100 absence caused a host or container mutation" >&2
  exit 1
}
cmp -s "$NODE_ENV" "$STATE_ROOT/node-before-absent-v100.env" || {
  echo "V100 absence changed private node configuration" >&2
  exit 1
}
mv "$PCI_ROOT/v100-removed" "$PCI_ROOT/devices/0000:02:00.0"

# Ready V100 mode binds the display GPU away from CUDA and pins the node UUID.
printf 'NVIDIA\n' >"$STATE_ROOT/license"
printf 'P\n' >"$MODULE_ROOT/nvidia/taint"
ln -sfn "$PCI_ROOT/drivers/nvidia" "$PCI_ROOT/devices/0000:02:00.0/driver"
ln -sfn "$PCI_ROOT/drivers/nouveau" "$PCI_ROOT/devices/0000:01:00.0/driver"
printf 'card fixture\n' >"$DRI_ROOT/card0"
printf 'render fixture\n' >"$DRI_ROOT/renderD128"
ln -sfn ../card0 "$DRI_ROOT/by-path/pci-0000:01:00.0-card"
ln -sfn ../renderD128 "$DRI_ROOT/by-path/pci-0000:01:00.0-render"
export TEST_GPU_MODE=v100
: >"$STATE_ROOT/systemctl.log"
"$CUDAV100" --apply >/dev/null
grep -q '^v100-32gb up -d --build --force-recreate$' "$STATE_ROOT/up.log"
grep -q '^v100-32gb$' "$STATE_ROOT/check.log"
[ "$(grep -c '^LLAMAFARM_NVIDIA_VISIBLE_DEVICES=' "$STATE_ROOT/deployed-node.env")" -eq 1 ]
grep -q '^LLAMAFARM_NVIDIA_VISIBLE_DEVICES=GPU-00000000-0000-0000-0000-000000000000$' \
  "$STATE_ROOT/deployed-node.env"
grep -q 'restart llamafarm-v100-cuda-5070-nvk.service' "$STATE_ROOT/systemctl.log"

echo "CUDA profile switcher tests passed"
