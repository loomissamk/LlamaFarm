# Two V100s on one LlamaFarm node

Use one LlamaFarm gateway and one Ollama process per independent model workload.
The bundle already contains the primary Ollama process. A managed GPU runtime
in Integrations starts a second Ollama process in a sibling container; it does
not start another LlamaFarm gateway. All runtimes share the model store.

## Identify the cards

Run `nvidia-smi --query-gpu=index,uuid,name,memory.total --format=csv,noheader` on
the host after installing the second card. Use UUIDs, since GPU indices can
change across reboots. Confirm both cards are visible to the LlamaFarm bundle
with `GET /api/ollama/gpu-placement` before changing routes.

## Two Qwen instances

1. Set `LLAMAFARM_OLLAMA_GPU_DEVICES` in the bundle's `.env` to the UUID for
   Kilo's primary Ollama GPU. Recreate the bundle at a maintenance point so
   the bundled Ollama process inherits the new binding. The gateway still sees
   both cards for placement.
2. In **Integrations → Runtime preference → Advanced runtime pools**, add a
   managed runtime on the second GPU UUID, save it, and select **Start / reconcile**.
3. Route the LlamaFarm dashboard's Qwen model to that worker. The Kilo provider
   that calls the primary Ollama port stays on the first GPU. Both processes
   may load the same model tag independently, so Kilo and LlamaFarm can run a
   Qwen instance at the same time.
4. Confirm the two processes and GPU memory with the placement API, each
   runtime's `/api/ps`, and host `nvidia-smi` under simultaneous requests.

## One larger model across both GPUs

Unload the two single-GPU Qwen instances, then create one managed runtime with
both UUIDs selected and **Spread across selected GPUs** enabled. Route the
larger model to that runtime and verify both cards hold model memory. Ollama
may choose one GPU for a model that fits there; selecting two cards does not
guarantee a split or double inference speed.

## PyTorch or CPU mode

An idle GPU can be used by a PyTorch job with `CUDA_VISIBLE_DEVICES=<GPU-UUID>`.
Unload or stop the Ollama runtime on that card first. Two full 27B instances
and a GPU training job need three independent memory pools; with two 32 GB
cards, switch one card between inference and training as needed.

A managed **CPU only** runtime can serve a model without allocating a GPU.
Route a model there in the same placement panel. This is useful while both
cards are occupied by training or a larger model. CPU inference will be slower.

The placement panel controls Ollama runtimes. It does not restart an existing
external PyTorch container or move an active CUDA job between devices.
