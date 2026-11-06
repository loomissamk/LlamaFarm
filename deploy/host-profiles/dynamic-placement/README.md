# Dynamic dual-V100 Ollama placement daemon

Polls both GPUs every 5s via `nvidia-smi`. Steady state: UnQwen
(`qwen3.8-unqwen:27b-256k`) unplaced -> primary/GPU0; locked Qwen
(`qwen3.8:27b-256k`) placed -> worker `gpu1`. If a model's preferred GPU is
busy (>70% util) and the other is idle (<20%), temporarily repoints that
model's placement to the idle GPU via `PUT /api/ollama/gpu-placement` so new
requests don't queue behind in-flight generation. Reverts to steady state as
soon as load no longer calls for the override.

Requires the `gpu0` worker entry (non-managed, endpoint
`http://127.0.0.1:11434`) in `provider.ollama_workers` so placements can
target the primary endpoint explicitly, not just the gpu1 sibling worker.

Install: `sudo cp llamafarm-dynamic-placement.service /etc/systemd/system/ &&
sudo systemctl enable --now llamafarm-dynamic-placement.service` (run as
`bat`, so `dynamic_placement.py` should live at `/home/bat/dynamic_placement.py`).
Currently deployed as a plain `nohup` background process on pop-os-1 for live
testing; installing the unit file makes it survive reboots.
