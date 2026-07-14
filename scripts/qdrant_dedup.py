#!/usr/bin/env python3
"""
Deduplicate arxiv_papers Qdrant collection by arxiv_id.

Scrolls all points, finds any arxiv_id that maps to more than one point ID,
keeps the first seen (lowest point ID) and deletes the rest.

Run against the LOCAL Qdrant after snapshot restore — do NOT run against 154.

Usage:
  python3 scripts/qdrant_dedup.py [--qdrant-url http://localhost:6333] [--dry-run]
"""
import argparse, sys
from qdrant_client import QdrantClient
from qdrant_client.models import PointIdsList

COLLECTION = "arxiv_papers"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--qdrant-url", default="http://localhost:6333")
    parser.add_argument("--dry-run", action="store_true", help="report without deleting")
    args = parser.parse_args()

    client = QdrantClient(url=args.qdrant_url)

    print(f"Connecting to {args.qdrant_url} ...")
    info = client.get_collection(COLLECTION)
    total = info.points_count
    print(f"Collection '{COLLECTION}': {total:,} points")

    seen: dict[str, int] = {}   # arxiv_id → first point id
    dupes: list[int] = []

    offset = None
    scanned = 0
    batch = 512

    while True:
        results, next_offset = client.scroll(
            collection_name=COLLECTION,
            limit=batch,
            offset=offset,
            with_payload=["arxiv_id"],
            with_vectors=False,
        )
        if not results:
            break

        for point in results:
            aid = (point.payload or {}).get("arxiv_id", "")
            if not aid:
                continue
            aid = str(aid)
            if aid in seen:
                dupes.append(point.id)
            else:
                seen[aid] = point.id

        scanned += len(results)
        if scanned % 50000 < batch:
            print(f"  scanned {scanned:,}/{total:,}  dupes so far: {len(dupes):,}", flush=True)

        if next_offset is None:
            break
        offset = next_offset

    print(f"\nScan complete: {scanned:,} points, {len(dupes):,} duplicates found")

    if not dupes:
        print("Nothing to delete.")
        return

    if args.dry_run:
        print("Dry run — skipping delete.")
        return

    print(f"Deleting {len(dupes):,} duplicate points ...")
    chunk = 1000
    deleted = 0
    for i in range(0, len(dupes), chunk):
        batch_ids = dupes[i:i+chunk]
        client.delete(
            collection_name=COLLECTION,
            points_selector=PointIdsList(points=batch_ids),
        )
        deleted += len(batch_ids)
        print(f"  deleted {deleted:,}/{len(dupes):,}", flush=True)

    print("Done.")


if __name__ == "__main__":
    main()
