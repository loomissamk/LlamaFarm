# Mixed NVIDIA host: V100 CUDA only, 5070 Ti display only

This profile is for one Linux host with a Tesla V100 dedicated to proprietary
NVIDIA CUDA compute and an RTX 5070 Ti dedicated to Nouveau/NVK display and
gaming. LlamaFarm receives exactly the configured V100 GPU UUID. It receives
no `/dev/dri`, Vulkan, or second NVIDIA compute device.

The binding helper validates PCI class, vendor ID, device ID, the proprietary
(not Open) NVIDIA module, and V100 UUID before and after a change. It resolves
the exact Nouveau module file and loads that path with `--ignore-install`, so
the host's `blacklist nouveau` and `alias nouveau off` rules cannot redirect
the operation. NVIDIA modules remain loaded. The helper writes sysfs only for
the configured 5070 Ti display function; audio/USB functions and every other
PCI device are left alone. A mismatch fails closed before unbinding anything.

## Configure and install

Find the two display-class PCI functions and the V100 UUID:

```bash
lspci -Dnn | grep -iE 'vga|3d|display'
nvidia-smi --query-gpu=pci.bus_id,name,uuid --format=csv
```

Copy the template outside the repository, replace every placeholder, and run
the non-mutating preflight:

```bash
cp deploy/host-profiles/v100-cuda-5070-nvk.env.example /tmp/v100-cuda-5070-nvk.env
sudo ./scripts/host/v100-cuda-5070-nvk.sh preflight /tmp/v100-cuda-5070-nvk.env
sudo ./scripts/host/install-v100-cuda-5070-nvk.sh /tmp/v100-cuda-5070-nvk.env
```

The installer copies the profile and helper, enables the systemd unit, and
does not change the live binding. On the next boot, the unit waits for udev and
Pop!_OS `gpu-manager`, then runs before the display manager, graphical target,
Docker, NVIDIA persistence, and CDI refresh. It does not unload NVIDIA modules.
To apply without rebooting, leave graphical work, stop GPU-using applications,
then explicitly run:

```bash
sudo systemctl start llamafarm-v100-cuda-5070-nvk.service
sudo /usr/local/libexec/llamafarm/v100-cuda-5070-nvk verify
```

Set the same V100 UUID in the private LlamaFarm node override:

```bash
LLAMAFARM_NVIDIA_VISIBLE_DEVICES=GPU-00000000-0000-0000-0000-000000000000
```

`up-node.sh v100-32gb` rejects a missing value, `all`, an index, a list, or a
UUID different from the root-owned host profile. Its generated Compose request
and `NVIDIA_VISIBLE_DEVICES` both select that UUID. The profile fixes
`LLAMAFARM_EXPOSE_DRI=false`, so compute-only mode omits the Nouveau render
nodes even if a private node override tries to enable them. Verify the running
container with:

```bash
./scripts/docker/check-node.sh v100-32gb
```

## Rollback

Disable boot-time reapplication before returning the 5070 Ti display function
to the proprietary driver:

```bash
sudo systemctl disable --now llamafarm-v100-cuda-5070-nvk.service
sudo /usr/local/libexec/llamafarm/v100-cuda-5070-nvk rollback
```

Rollback clears the 5070 Ti display function's override after rebinding it to
`nvidia`, and again verifies that the configured V100 is unchanged. It does
not remove packages, configuration, containers, models, or named volumes.
