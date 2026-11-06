#!/usr/bin/env python3
"""Dynamic GPU-load-aware Ollama placement daemon for LlamaFarm's dual-V100 setup.

Steady state: UnQwen (qwen3.8-unqwen:27b-256k) unplaced -> primary/GPU0.
              locked Qwen (qwen3.8:27b-256k) placed -> worker "gpu1".

Overflow: if the preferred GPU for a model is busy (>70% util) and the other
GPU is idle (<20% util), temporarily repoint that model's placement to the
idle GPU so a new request doesn't queue behind in-flight generation. Reverts
to steady state as soon as load no longer calls for the override.
"""
import json
import subprocess
import time
import urllib.request

GATEWAY = "http://127.0.0.1:42617"
UNQWEN = "qwen3.8-unqwen:27b-256k"
LOCKED = "qwen3.8:27b-256k"
GPU0_UUID = "GPU-80940ec0-6838-c4da-f698-68a121ce5024"
GPU1_UUID = "GPU-4dda4033-eb45-c5a7-fe70-16979f13147c"
BUSY = 70
IDLE = 20
POLL = 5


def gpu_util():
    out = subprocess.run(
        ["nvidia-smi", "--query-gpu=uuid,utilization.gpu", "--format=csv,noheader,nounits"],
        capture_output=True, text=True, timeout=10, check=True,
    ).stdout
    util = {}
    for line in out.strip().splitlines():
        uuid, u = [x.strip() for x in line.split(",")]
        util[uuid] = int(u)
    return util.get(GPU0_UUID, 0), util.get(GPU1_UUID, 0)


def get_placement():
    with urllib.request.urlopen(f"{GATEWAY}/api/ollama/gpu-placement", timeout=10) as r:
        return json.load(r)


def put_placement(workers, placements):
    body = json.dumps({"workers": workers, "placements": placements}).encode()
    req = urllib.request.Request(
        f"{GATEWAY}/api/ollama/gpu-placement", data=body,
        headers={"Content-Type": "application/json"}, method="PUT",
    )
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.load(r)


def main():
    print(f"[{time.strftime('%H:%M:%S')}] dynamic placement daemon starting", flush=True)
    last_state = None
    while True:
        try:
            gpu0_util, gpu1_util = gpu_util()
            current = get_placement()
            workers = current["workers"]

            placements = [{"model": LOCKED, "worker_id": "gpu1"}]
            state = "steady"

            gpu0_busy = gpu0_util > BUSY
            gpu1_busy = gpu1_util > BUSY
            gpu0_idle = gpu0_util < IDLE
            gpu1_idle = gpu1_util < IDLE

            if gpu0_busy and gpu1_idle:
                placements = [
                    {"model": LOCKED, "worker_id": "gpu1"},
                    {"model": UNQWEN, "worker_id": "gpu1"},
                ]
                state = "overflow:unqwen->gpu1"
            elif gpu1_busy and gpu0_idle:
                placements = [{"model": LOCKED, "worker_id": "gpu0"}]
                state = "overflow:locked->gpu0"

            if state != last_state:
                print(
                    f"[{time.strftime('%H:%M:%S')}] gpu0={gpu0_util}% gpu1={gpu1_util}% "
                    f"-> {state} placements={placements}",
                    flush=True,
                )
                put_placement(workers, placements)
                last_state = state
        except Exception as e:
            print(f"[{time.strftime('%H:%M:%S')}] ERROR: {e}", flush=True)
        time.sleep(POLL)


if __name__ == "__main__":
    main()
