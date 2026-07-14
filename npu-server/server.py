#!/usr/bin/env python3
"""
Embedding server — NPU on Intel AI Boost, CPU fallback on any other box.
OpenAI-compatible /v1/embeddings + Ollama-compatible /api/embeddings.
Wire into OpenWebUI: Settings → Documents → Embedding Model API URL → http://localhost:11436
"""
import os, time, logging, contextlib
from pathlib import Path
from typing import Union
import numpy as np
import openvino as ov
from transformers import AutoTokenizer
from fastapi import FastAPI
from pydantic import BaseModel
import uvicorn

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
log = logging.getLogger("embed")

PORT         = int(os.environ.get("EMBED_PORT", 11436))
SEQ_LEN      = 128
MODEL_DIR    = Path(__file__).parent / "models"
STATIC_MODEL = MODEL_DIR / "minilm-static" / "model.xml"
DYNAMIC_DIR  = MODEL_DIR / "minilm-npu"

def detect_device():
    try:
        if "NPU" in ov.Core().available_devices:
            log.info("Intel NPU detected")
            return "NPU"
    except Exception:
        pass
    log.info("Using CPU fallback")
    return "CPU"

class EmbedEngine:
    def __init__(self):
        self.device = detect_device()
        core = ov.Core()
        if self.device == "NPU" and STATIC_MODEL.exists():
            model = core.read_model(str(STATIC_MODEL))
        else:
            model = core.read_model(str(DYNAMIC_DIR / "openvino_model.xml"))
            model.reshape({i.get_any_name(): [1, SEQ_LEN] for i in model.inputs})
        self.compiled = core.compile_model(model, self.device)
        self.req      = self.compiled.create_infer_request()
        self.tok      = AutoTokenizer.from_pretrained(str(DYNAMIC_DIR))
        log.info(f"Ready on {self.device}")

    def embed(self, texts):
        out = []
        for text in texts:
            enc = self.tok(text, max_length=SEQ_LEN, padding="max_length",
                           truncation=True, return_tensors="np")
            for inp in self.compiled.inputs:
                self.req.set_tensor(inp, ov.Tensor(enc[inp.get_any_name()].astype(np.int64)))
            self.req.infer()
            emb   = self.req.get_output_tensor(0).data[0]
            mask  = enc["attention_mask"][0].astype(float)
            pool  = (emb * mask[:, None]).sum(0) / mask.sum()
            n     = np.linalg.norm(pool)
            out.append((pool / n if n > 0 else pool).tolist())
        return out

engine = None

@contextlib.asynccontextmanager
async def lifespan(app):
    global engine
    engine = EmbedEngine()
    yield

app = FastAPI(lifespan=lifespan)

class OAI(BaseModel):
    model: str = "all-MiniLM-L6-v2"
    input: Union[str, list]

class Ollama(BaseModel):
    model: str = "all-MiniLM-L6-v2"
    prompt: str

@app.get("/health")
def health():
    return {"status": "ok", "device": engine.device}

@app.post("/v1/embeddings")
def oai(req: OAI):
    t = req.input if isinstance(req.input, list) else [req.input]
    t0 = time.time()
    v = engine.embed(t)
    return {"object": "list", "model": req.model,
            "data": [{"object": "embedding", "index": i, "embedding": x} for i, x in enumerate(v)],
            "usage": {"prompt_tokens": len(t), "total_tokens": len(t)},
            "_device": engine.device, "_ms": round((time.time()-t0)*1000, 1)}

@app.post("/api/embeddings")
def ollama(req: Ollama):
    return {"embedding": engine.embed([req.prompt])[0]}

@app.get("/api/tags")
def tags():
    return {"models": [{"name": "all-MiniLM-L6-v2"}]}

if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=PORT, log_level="info")
