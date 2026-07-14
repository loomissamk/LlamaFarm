#!/usr/bin/env python3
"""
Parallel ArXiv → Qdrant ingest for LlamaFarm semantic search.
Uses multiprocessing to split MongoDB cursor across N workers,
each embedding via nomic-embed-text on the GPU through Ollama.

Usage:
  python3 scripts/arxiv_to_qdrant.py [--workers 8] [--batch 128] [--reset]

Checkpointed per-worker — safe to kill and resume.
"""
import sys, time, json, argparse, os, math, multiprocessing, hashlib, struct
from pathlib import Path

try:
    from pymongo import MongoClient
    from bson import ObjectId
    from qdrant_client import QdrantClient
    from qdrant_client.models import VectorParams, Distance, PointStruct
    import requests
except ImportError:
    print("pip install pymongo qdrant-client requests")
    sys.exit(1)

MONGO_URI   = "mongodb://192.168.1.154:27017"
MONGO_DB    = "ArXivDB"
MONGO_COL   = "Papers"
QDRANT_URL  = "http://qdrant:6333"
COLLECTION  = "arxiv_papers"
OLLAMA_URL  = "http://localhost:11434"
EMBED_MODEL = "nomic-embed-text"
EMBED_DIM   = 768
CHECKPOINT_DIR = Path("~/.llamafarm/arxiv_ingest").expanduser()


def embed_batch(texts: list[str], ollama_url: str) -> list[list[float]]:
    resp = requests.post(
        f"{ollama_url}/api/embed",
        json={"model": EMBED_MODEL, "input": texts},
        timeout=120,
    )
    resp.raise_for_status()
    return resp.json()["embeddings"]


def stable_id(aid: str) -> int:
    return struct.unpack('>Q', hashlib.sha256(aid.encode()).digest()[:8])[0]


def doc_to_point(vec, d):
    aid = d.get("id") or d.get("arxiv_id") or str(d["_id"])
    return PointStruct(
        id=stable_id(aid),
        vector=vec,
        payload={
            "arxiv_id":   aid,
            "title":      (d.get("title") or "")[:500],
            "abstract":   (d.get("abstract") or "")[:800],
            "categories": d.get("categories", "").split()
                          if isinstance(d.get("categories"), str)
                          else d.get("categories", []),
            "authors":    (d.get("authors") or "")[:200],
        },
    )


def worker(worker_id: int, skip: int, limit: int, batch_size: int):
    ckpt_file = CHECKPOINT_DIR / f"worker_{worker_id}.json"
    CHECKPOINT_DIR.mkdir(parents=True, exist_ok=True)

    offset = 0
    if ckpt_file.exists():
        offset = json.loads(ckpt_file.read_text()).get("offset", 0)
        print(f"[W{worker_id}] resuming from offset {offset}", flush=True)

    mongo = MongoClient(MONGO_URI, serverSelectionTimeoutMS=10000)
    col   = mongo[MONGO_DB][MONGO_COL]
    qdrant = QdrantClient(url=QDRANT_URL)

    cursor = col.find(
        {}, {"id": 1, "title": 1, "abstract": 1, "categories": 1, "authors": 1}
    ).skip(skip + offset).limit(max(0, limit - offset)).batch_size(batch_size)

    batch_texts, batch_docs, processed = [], [], 0
    t0 = time.time()

    for doc in cursor:
        title    = doc.get("title", "")
        abstract = doc.get("abstract", "")
        if not title and not abstract:
            continue
        batch_texts.append(f"{title}\n\n{abstract}"[:2000])
        batch_docs.append(doc)

        if len(batch_texts) >= batch_size:
            try:
                vecs   = embed_batch(batch_texts, OLLAMA_URL)
                points = [doc_to_point(v, d) for v, d in zip(vecs, batch_docs)]
                qdrant.upsert(collection_name=COLLECTION, points=points)
                processed += len(points)
                ckpt_file.write_text(json.dumps({"offset": offset + processed}))

                elapsed = time.time() - t0
                rate    = processed / elapsed if elapsed > 0 else 0
                print(
                    f"[W{worker_id}] {processed:,}/{limit:,}  {rate:.1f} docs/s",
                    flush=True,
                )
            except Exception as e:
                print(f"[W{worker_id}] batch error: {e}", flush=True)
            batch_texts, batch_docs = [], []

    # flush remainder
    if batch_texts:
        try:
            vecs   = embed_batch(batch_texts, OLLAMA_URL)
            points = [doc_to_point(v, d) for v, d in zip(vecs, batch_docs)]
            qdrant.upsert(collection_name=COLLECTION, points=points)
            processed += len(points)
            ckpt_file.write_text(json.dumps({"offset": offset + processed}))
        except Exception as e:
            print(f"[W{worker_id}] final flush error: {e}", flush=True)

    elapsed = time.time() - t0
    print(f"[W{worker_id}] done: {processed:,} docs in {elapsed:.0f}s", flush=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--workers",  type=int, default=4,   help="parallel workers (default 4)")
    parser.add_argument("--batch",    type=int, default=128,  help="embed batch size (default 128)")
    parser.add_argument("--reset",    action="store_true",    help="clear checkpoints and restart")
    parser.add_argument("--limit",    type=int, default=0,    help="cap total docs (0 = all)")
    args = parser.parse_args()

    if args.reset and CHECKPOINT_DIR.exists():
        import shutil
        shutil.rmtree(CHECKPOINT_DIR)
        print("Checkpoints cleared.")

    # Get total doc count fast
    mongo = MongoClient(MONGO_URI, serverSelectionTimeoutMS=10000)
    total = mongo[MONGO_DB][MONGO_COL].estimated_document_count()
    print(f"Total papers: {total:,}", flush=True)
    mongo.close()

    n = args.workers
    cap = args.limit if args.limit > 0 else total
    chunk = math.ceil(cap / n)

    # Ensure Qdrant collection exists
    qdrant = QdrantClient(url=QDRANT_URL)
    if COLLECTION not in {c.name for c in qdrant.get_collections().collections}:
        qdrant.create_collection(
            COLLECTION,
            vectors_config=VectorParams(size=EMBED_DIM, distance=Distance.COSINE),
        )
        print(f"Created Qdrant collection '{COLLECTION}'", flush=True)
    else:
        cnt = qdrant.get_collection(COLLECTION).points_count
        print(f"Qdrant collection exists: {cnt:,} vectors", flush=True)

    print(f"Starting {n} workers, batch={args.batch}, chunk={chunk:,} each", flush=True)

    procs = []
    for i in range(n):
        skip  = i * chunk
        limit = min(chunk, cap - skip)
        if limit <= 0:
            break
        p = multiprocessing.Process(target=worker, args=(i, skip, limit, args.batch), daemon=True)
        p.start()
        procs.append(p)

    for p in procs:
        p.join()

    print("All workers done.", flush=True)


if __name__ == "__main__":
    main()
