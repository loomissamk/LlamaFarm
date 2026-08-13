#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
HELPER="$ROOT_DIR/scripts/host/v100-cuda-5070-nvk.sh"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/llamafarm-v100-host-test.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT

PCI_ROOT="$TMP_ROOT/pci"
MODULE_ROOT="$TMP_ROOT/modules-sysfs"
DRI_ROOT="$TMP_ROOT/dri"
NOUVEAU_MODULE="$TMP_ROOT/modules/nouveau.ko"
FAKE_BIN="$TMP_ROOT/bin"
CONFIG="$TMP_ROOT/host.env"
mkdir -p \
  "$PCI_ROOT/devices/0000:01:00.0" \
  "$PCI_ROOT/devices/0000:02:00.0" \
  "$PCI_ROOT/drivers/nvidia" \
  "$PCI_ROOT/drivers/nouveau" \
  "$MODULE_ROOT/nvidia" \
  "$DRI_ROOT/by-path" \
  "$(dirname "$NOUVEAU_MODULE")" \
  "$FAKE_BIN"

printf '0x10de\n' >"$PCI_ROOT/devices/0000:01:00.0/vendor"
printf '0x1db6\n' >"$PCI_ROOT/devices/0000:01:00.0/device"
printf '0x030200\n' >"$PCI_ROOT/devices/0000:01:00.0/class"
printf '\n' >"$PCI_ROOT/devices/0000:01:00.0/driver_override"
printf '0x10de\n' >"$PCI_ROOT/devices/0000:02:00.0/vendor"
printf '0x2f00\n' >"$PCI_ROOT/devices/0000:02:00.0/device"
printf '0x030000\n' >"$PCI_ROOT/devices/0000:02:00.0/class"
printf '\n' >"$PCI_ROOT/devices/0000:02:00.0/driver_override"
ln -s "$PCI_ROOT/drivers/nvidia" "$PCI_ROOT/devices/0000:01:00.0/driver"
ln -s "$PCI_ROOT/drivers/nvidia" "$PCI_ROOT/devices/0000:02:00.0/driver"
printf 'P\n' >"$MODULE_ROOT/nvidia/taint"
printf 'fake module fixture\n' >"$NOUVEAU_MODULE"

cat >"$FAKE_BIN/nvidia-smi" <<'EOF'
#!/usr/bin/env bash
printf '00000000:01:00.0, GPU-00000000-0000-0000-0000-000000000000\n'
EOF
chmod +x "$FAKE_BIN/nvidia-smi"
cat >"$FAKE_BIN/modinfo" <<EOF
#!/usr/bin/env bash
if [ "\${1:-}" = -F ] && [ "\${2:-}" = license ] && [ "\${3:-}" = nvidia ]; then
  printf 'NVIDIA\\n'
elif [ "\${1:-}" = -n ] && [ "\${2:-}" = nouveau ]; then
  printf '%s\\n' '$NOUVEAU_MODULE'
else
  exit 2
fi
EOF
cat >"$FAKE_BIN/udevadm" <<'EOF'
#!/usr/bin/env bash
[ "${1:-}" = settle ]
EOF
chmod +x "$FAKE_BIN/modinfo" "$FAKE_BIN/udevadm"

cat >"$CONFIG" <<'EOF'
V100_PCI_BDF=0000:01:00.0
V100_PCI_VENDOR_ID=10de
V100_PCI_DEVICE_ID=1db6
V100_GPU_UUID=GPU-00000000-0000-0000-0000-000000000000
DISPLAY_GPU_PCI_BDF=0000:02:00.0
DISPLAY_GPU_PCI_VENDOR_ID=10de
DISPLAY_GPU_PCI_DEVICE_ID=2f00
EOF

PATH="$FAKE_BIN:$PATH" LLAMAFARM_PCI_SYSFS_ROOT="$PCI_ROOT" \
  LLAMAFARM_MODULE_SYSFS_ROOT="$MODULE_ROOT" LLAMAFARM_DRI_ROOT="$DRI_ROOT" \
  "$HELPER" preflight "$CONFIG" >/dev/null

mv "$NOUVEAU_MODULE" "$NOUVEAU_MODULE.missing"
if PATH="$FAKE_BIN:$PATH" LLAMAFARM_PCI_SYSFS_ROOT="$PCI_ROOT" \
  LLAMAFARM_MODULE_SYSFS_ROOT="$MODULE_ROOT" LLAMAFARM_DRI_ROOT="$DRI_ROOT" \
  "$HELPER" preflight "$CONFIG" >/dev/null 2>&1; then
  echo "preflight accepted a missing resolved nouveau module file" >&2
  exit 1
fi
mv "$NOUVEAU_MODULE.missing" "$NOUVEAU_MODULE"

if PATH="$FAKE_BIN:$PATH" LLAMAFARM_PCI_SYSFS_ROOT="$PCI_ROOT" \
  LLAMAFARM_MODULE_SYSFS_ROOT="$MODULE_ROOT" LLAMAFARM_DRI_ROOT="$DRI_ROOT" \
  "$HELPER" verify "$CONFIG" >/dev/null 2>&1; then
  echo "verify accepted a display GPU that was not bound to nouveau" >&2
  exit 1
fi

ln -sfn "$PCI_ROOT/drivers/nouveau" "$PCI_ROOT/devices/0000:02:00.0/driver"
printf 'card fixture\n' >"$DRI_ROOT/card0"
printf 'render fixture\n' >"$DRI_ROOT/renderD128"
ln -s ../card0 "$DRI_ROOT/by-path/pci-0000:02:00.0-card"
ln -s ../renderD128 "$DRI_ROOT/by-path/pci-0000:02:00.0-render"
PATH="$FAKE_BIN:$PATH" LLAMAFARM_PCI_SYSFS_ROOT="$PCI_ROOT" \
  LLAMAFARM_MODULE_SYSFS_ROOT="$MODULE_ROOT" LLAMAFARM_DRI_ROOT="$DRI_ROOT" \
  "$HELPER" verify "$CONFIG" >/dev/null

rm "$DRI_ROOT/by-path/pci-0000:02:00.0-render"
if PATH="$FAKE_BIN:$PATH" LLAMAFARM_PCI_SYSFS_ROOT="$PCI_ROOT" \
  LLAMAFARM_MODULE_SYSFS_ROOT="$MODULE_ROOT" LLAMAFARM_DRI_ROOT="$DRI_ROOT" \
  "$HELPER" verify "$CONFIG" >/dev/null 2>&1; then
  echo "verify accepted a missing 5070 Ti render node" >&2
  exit 1
fi
ln -s ../renderD128 "$DRI_ROOT/by-path/pci-0000:02:00.0-render"

printf '\n' >"$MODULE_ROOT/nvidia/taint"
if PATH="$FAKE_BIN:$PATH" LLAMAFARM_PCI_SYSFS_ROOT="$PCI_ROOT" \
  LLAMAFARM_MODULE_SYSFS_ROOT="$MODULE_ROOT" LLAMAFARM_DRI_ROOT="$DRI_ROOT" \
  "$HELPER" preflight "$CONFIG" >/dev/null 2>&1; then
  echo "preflight accepted an NVIDIA module not marked proprietary" >&2
  exit 1
fi
printf 'P\n' >"$MODULE_ROOT/nvidia/taint"

printf '0xffff\n' >"$PCI_ROOT/devices/0000:02:00.0/vendor"
if PATH="$FAKE_BIN:$PATH" LLAMAFARM_PCI_SYSFS_ROOT="$PCI_ROOT" \
  LLAMAFARM_MODULE_SYSFS_ROOT="$MODULE_ROOT" LLAMAFARM_DRI_ROOT="$DRI_ROOT" \
  "$HELPER" preflight "$CONFIG" >/dev/null 2>&1; then
  echo "preflight accepted a mismatched display GPU vendor" >&2
  exit 1
fi
[ "$(basename "$(readlink "$PCI_ROOT/devices/0000:02:00.0/driver")")" = nouveau ] || {
  echo "failed preflight changed the display GPU binding" >&2
  exit 1
}

NODE_ENV="$TMP_ROOT/node.env"
cat >"$NODE_ENV" <<'EOF'
LLAMAFARM_MANUAL_PEERS=http://example.com:42617
LLAMAFARM_FEDERATION_TOKEN=test-only-federation-token
EOF

if env -u LLAMAFARM_NVIDIA_VISIBLE_DEVICES \
  LLAMAFARM_NODE_ENV="$NODE_ENV" LLAMAFARM_V100_HOST_CONFIG="$CONFIG" \
  "$ROOT_DIR/scripts/docker/up-node.sh" v100-32gb config >/dev/null 2>&1; then
  echo "v100 node profile accepted missing NVIDIA_VISIBLE_DEVICES" >&2
  exit 1
fi
if env LLAMAFARM_NVIDIA_VISIBLE_DEVICES=all \
  LLAMAFARM_NODE_ENV="$NODE_ENV" LLAMAFARM_V100_HOST_CONFIG="$CONFIG" \
  "$ROOT_DIR/scripts/docker/up-node.sh" v100-32gb config >/dev/null 2>&1; then
  echo "v100 node profile accepted broad NVIDIA visibility" >&2
  exit 1
fi
if env LLAMAFARM_NVIDIA_VISIBLE_DEVICES=GPU-11111111-1111-1111-1111-111111111111 \
  LLAMAFARM_NODE_ENV="$NODE_ENV" LLAMAFARM_V100_HOST_CONFIG="$CONFIG" \
  "$ROOT_DIR/scripts/docker/up-node.sh" v100-32gb config >/dev/null 2>&1; then
  echo "v100 node profile accepted the wrong GPU UUID" >&2
  exit 1
fi

echo "v100-cuda-5070-nvk host-profile tests passed"
