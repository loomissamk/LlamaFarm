# CUDA GPU profile switchers

These two operator commands switch one mixed-capable Linux host between two
deliberately separate inference modes:

```bash
sudo ./scripts/host/cuda507.sh --apply
sudo ./scripts/host/cudav100.sh --apply
```

`cuda507.sh` uses NVIDIA R580 Open and deploys `rtx5070ti-16gb`. It disables
LlamaFarm V100/mixed-binding units and clears only the configured 5070 Ti PCI
function's driver override. This is the current mode when the V100 is removed.

`cudav100.sh` uses proprietary NVIDIA R580 and deploys `v100-32gb`, pinned to
the exact UUID in `/etc/llamafarm/v100-cuda-5070-nvk.env`. It fails before apt,
systemd, Docker, or PCI mutation unless both the configured V100 and 5070 Ti
PCI functions are physically present. The guarded host unit binds only the
5070 Ti display function to Nouveau; CUDA and the container see only the V100.

Both scripts preserve named Docker volumes, models, workspace data, and every
unrelated private setting. A successful switch atomically replaces only the
GPU-selection fields in `~/.config/llamafarm/node.env`, preserving its mode and
owner. Both modes persist one exact live GPU UUID: the 5070 mode enables its
DRI/display nodes, while V100 mode is compute-only and omits DRI. The prior
file remains beside it as `node.env.llamafarm-gpu-backup`. If deployment or
verification fails after the write, the script automatically restores that
backup. Secrets are never printed or written into the repository.

The two commands are also the rollback path for one another: run the other
profile's `--apply` (and its `--reboot` only if requested). Neither path purges
model data or calls Compose with `-v`.

## Driver activation

Start with `--apply`. If the selected kernel-module flavor is already live,
the script immediately rebuilds/recreates the bundle without `down -v`, then
runs `check-node.sh`. If a package transition needs a reboot, `--apply` exits
with status 10 and makes no success claim. Then use:

```bash
sudo ./scripts/host/cuda507.sh --reboot
# after the host returns
sudo ./scripts/host/cuda507.sh --apply
```

Substitute `cudav100.sh` for the future V100 mode. `--reboot` does not reboot
when the requested profile is already live. `--preflight` is non-mutating;
`--verify` checks both the host GPU state and the deployed LlamaFarm node.

The package defaults are `nvidia-driver-580-open` and `nvidia-driver-580`.
Advanced recovery can override them with `LLAMAFARM_NVIDIA_OPEN_PACKAGE`,
`LLAMAFARM_NVIDIA_PROPRIETARY_PACKAGE`, or change the branch with
`LLAMAFARM_NVIDIA_DRIVER_BRANCH`. A non-default 5070 Ti PCI function can be
selected with `LLAMAFARM_5070_PCI_BDF`; its expected device ID defaults to the
host's `2c05` and can be overridden with `LLAMAFARM_5070_DEVICE_ID`.

Run these commands from SSH or a text console when changing driver flavor.
The V100 switch deliberately restarts the guarded display-binding unit, so an
active graphical session on the 5070 Ti can be interrupted.
