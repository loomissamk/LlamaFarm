#!/usr/bin/env python3
"""Build an isolated, auditable "recent research" dataset.

The default mode is a read-only dry run:

* arXiv is queried through its official Atom API;
* an optional existing MongoDB corpus is queried only with ``find``;
* optional Ollama/Qdrant semantic links are queried over HTTP;
* no MongoDB writes occur unless ``--write`` is explicitly supplied.

Write mode is deliberately separate.  It requires a target URI and refuses to
write to the source database.  It only upserts into the target database's
``papers``, ``web_sources``, ``links``, and ``crawl_runs`` collections.

The implementation uses the Python standard library except for MongoDB access,
which is enabled when the optional ``pymongo`` package is installed.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence


DEFAULT_TOPICS = (
    "quantum error correction",
    "soil microbiome drought",
    "Byzantine trade networks",
)
DEFAULT_ARXIV_ENDPOINT = "https://export.arxiv.org/api/query"
ATOM_NS = "http://www.w3.org/2005/Atom"
ARXIV_NS = "http://arxiv.org/schemas/atom"
MODERN_ARXIV_ID = re.compile(r"^\d{4}\.\d{4,5}$")
LEGACY_ARXIV_ID = re.compile(r"^[a-z][a-z0-9.-]*/\d{7}$", re.IGNORECASE)
VERSION_SUFFIX = re.compile(r"v\d+$", re.IGNORECASE)
SPACE_RUN = re.compile(r"\s+")


class PilotError(RuntimeError):
    """An expected, user-actionable pilot failure."""


def utc_now() -> str:
    """Return an RFC 3339 UTC timestamp without fractional seconds."""

    timestamp = dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()
    return timestamp.replace("+00:00", "Z")


def canonical_json(value: Any) -> str:
    """Serialize a value deterministically for hashing and audit output."""

    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def fingerprint(value: Any) -> str:
    """Return a stable SHA-256 fingerprint for a JSON-compatible value."""

    return hashlib.sha256(canonical_json(value).encode("utf-8")).hexdigest()


def normalize_text(value: str | None) -> str:
    """Collapse XML/HTML-style whitespace without changing word order."""

    return SPACE_RUN.sub(" ", value or "").strip()


def canonical_arxiv_id(value: Any) -> str | None:
    """Canonicalize modern or legacy arXiv identifiers and strip versions.

    Accepted examples include ``arXiv:2401.01234v2``, arXiv abs/pdf URLs,
    ``oai:arXiv.org:hep-th/9901001v3``, and bare identifiers.  The canonical
    form is lower-case and never contains a version suffix.
    """

    if value is None:
        return None
    raw = urllib.parse.unquote(str(value)).strip()
    if not raw:
        return None

    parsed = urllib.parse.urlsplit(raw)
    if parsed.scheme in {"http", "https"} and parsed.netloc:
        path = parsed.path.strip("/")
        for prefix in ("abs/", "pdf/", "format/"):
            if path.lower().startswith(prefix):
                path = path[len(prefix) :]
                break
        raw = path

    raw = raw.split("?", 1)[0].split("#", 1)[0].strip().strip("/")
    if raw.lower().startswith("oai:arxiv.org:"):
        raw = raw[len("oai:arXiv.org:") :]
    if raw.lower().startswith("arxiv:"):
        raw = raw[len("arxiv:") :]
    if raw.lower().endswith(".pdf"):
        raw = raw[:-4]
    raw = VERSION_SUFFIX.sub("", raw).strip().strip("/").lower()

    if MODERN_ARXIV_ID.fullmatch(raw) or LEGACY_ARXIV_ID.fullmatch(raw):
        return raw
    return None


def parse_arxiv_atom(xml_payload: bytes | str, topic: str) -> list[dict[str, Any]]:
    """Parse an arXiv Atom response into normalized paper records."""

    root = ET.fromstring(xml_payload)
    ns = {"atom": ATOM_NS, "arxiv": ARXIV_NS}
    papers: list[dict[str, Any]] = []

    for entry in root.findall("atom:entry", ns):
        raw_id = entry.findtext("atom:id", default="", namespaces=ns)
        arxiv_id = canonical_arxiv_id(raw_id)
        if not arxiv_id:
            continue

        authors = [
            normalize_text(node.findtext("atom:name", default="", namespaces=ns))
            for node in entry.findall("atom:author", ns)
        ]
        authors = [author for author in authors if author]
        categories = sorted(
            {
                node.attrib.get("term", "").strip()
                for node in entry.findall("atom:category", ns)
                if node.attrib.get("term", "").strip()
            }
        )
        links: dict[str, str] = {}
        for node in entry.findall("atom:link", ns):
            href = node.attrib.get("href", "").strip()
            if not href:
                continue
            relation = node.attrib.get("rel", "")
            media_type = node.attrib.get("type", "")
            key = "pdf" if media_type == "application/pdf" else relation or media_type or "other"
            links.setdefault(key, href)

        paper = {
            "_id": arxiv_id,
            "arxiv_id": arxiv_id,
            "versioned_id": str(raw_id).rstrip("/").rsplit("/", 1)[-1],
            "title": normalize_text(entry.findtext("atom:title", default="", namespaces=ns)),
            "abstract": normalize_text(entry.findtext("atom:summary", default="", namespaces=ns)),
            "authors": authors,
            "categories": categories,
            "published": normalize_text(
                entry.findtext("atom:published", default="", namespaces=ns)
            ),
            "updated": normalize_text(entry.findtext("atom:updated", default="", namespaces=ns)),
            "doi": normalize_text(entry.findtext("arxiv:doi", default="", namespaces=ns)) or None,
            "journal_ref": normalize_text(
                entry.findtext("arxiv:journal_ref", default="", namespaces=ns)
            )
            or None,
            "links": links,
            "query_topics": [topic],
            "provenance": {
                "source": "arxiv_atom_api",
                "entry_id": raw_id,
            },
        }
        paper["content_hash"] = fingerprint(
            {
                "arxiv_id": paper["arxiv_id"],
                "title": paper["title"],
                "abstract": paper["abstract"],
                "authors": paper["authors"],
                "categories": paper["categories"],
                "published": paper["published"],
                "updated": paper["updated"],
            }
        )
        papers.append(paper)
    return papers


def arxiv_query_url(endpoint: str, topic: str, max_results: int) -> str:
    """Construct an official arXiv API query sorted by newest submission."""

    escaped_topic = topic.replace('"', '\\"')
    params = urllib.parse.urlencode(
        {
            "search_query": f'all:"{escaped_topic}"',
            "start": 0,
            "max_results": max_results,
            "sortBy": "submittedDate",
            "sortOrder": "descending",
        }
    )
    return f"{endpoint}?{params}"


def _http_request_json(
    url: str,
    payload: Mapping[str, Any] | None = None,
    *,
    timeout: float = 30.0,
    headers: Mapping[str, str] | None = None,
) -> Any:
    body = None if payload is None else canonical_json(payload).encode("utf-8")
    request_headers = {"Accept": "application/json"}
    if body is not None:
        request_headers["Content-Type"] = "application/json"
    request_headers.update(headers or {})
    request = urllib.request.Request(url, data=body, headers=request_headers)
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode("utf-8"))


class ArxivClient:
    """Rate-limited arXiv Atom client with a filesystem response cache."""

    def __init__(
        self,
        *,
        endpoint: str = DEFAULT_ARXIV_ENDPOINT,
        cache_dir: Path | None = None,
        cache_ttl_seconds: float = 21_600,
        rate_limit_seconds: float = 3.0,
        timeout_seconds: float = 30.0,
        retries: int = 2,
        user_agent: str = "LlamaFarmRecentResearch/1.0",
    ) -> None:
        self.endpoint = endpoint.rstrip("?")
        self.cache_dir = cache_dir
        self.cache_ttl_seconds = max(cache_ttl_seconds, 0)
        self.rate_limit_seconds = max(rate_limit_seconds, 0)
        self.timeout_seconds = timeout_seconds
        self.retries = max(retries, 0)
        self.user_agent = user_agent
        self._last_request_at = 0.0

    def fetch(self, topic: str, max_results: int) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        url = arxiv_query_url(self.endpoint, topic, max_results)
        cache_key = fingerprint({"url": url})
        cache_path = self.cache_dir / f"{cache_key}.xml" if self.cache_dir else None
        cache_hit = False
        payload: bytes | None = None

        if cache_path and cache_path.exists():
            age = max(0.0, time.time() - cache_path.stat().st_mtime)
            if age <= self.cache_ttl_seconds:
                payload = cache_path.read_bytes()
                cache_hit = True

        attempts = 0
        if payload is None:
            for attempt in range(self.retries + 1):
                attempts = attempt + 1
                wait_for = self.rate_limit_seconds - (time.monotonic() - self._last_request_at)
                if wait_for > 0:
                    time.sleep(wait_for)
                request = urllib.request.Request(
                    url,
                    headers={
                        "Accept": "application/atom+xml",
                        "User-Agent": self.user_agent,
                    },
                )
                try:
                    self._last_request_at = time.monotonic()
                    with urllib.request.urlopen(request, timeout=self.timeout_seconds) as response:
                        payload = response.read()
                    break
                except (urllib.error.URLError, TimeoutError) as exc:
                    if attempt >= self.retries:
                        raise PilotError(f"arXiv request failed for {topic!r}: {exc}") from exc
                    time.sleep(min(2**attempt, 8))

            if cache_path and payload is not None:
                cache_path.parent.mkdir(parents=True, exist_ok=True)
                temporary = cache_path.with_suffix(".tmp")
                temporary.write_bytes(payload)
                temporary.replace(cache_path)

        assert payload is not None
        try:
            papers = parse_arxiv_atom(payload, topic)
        except ET.ParseError as exc:
            raise PilotError(f"arXiv returned invalid Atom XML for {topic!r}: {exc}") from exc
        return papers, {
            "topic": topic,
            "query_url": url,
            "cache_hit": cache_hit,
            "attempts": attempts,
            "entries": len(papers),
            "response_hash": hashlib.sha256(payload).hexdigest(),
        }


def merge_papers(topic_results: Iterable[Sequence[dict[str, Any]]]) -> list[dict[str, Any]]:
    """Deduplicate papers by canonical ID while retaining query provenance."""

    merged: dict[str, dict[str, Any]] = {}
    for papers in topic_results:
        for paper in papers:
            arxiv_id = canonical_arxiv_id(paper.get("arxiv_id"))
            if not arxiv_id:
                continue
            if arxiv_id not in merged:
                merged[arxiv_id] = dict(paper)
                merged[arxiv_id]["_id"] = arxiv_id
                merged[arxiv_id]["arxiv_id"] = arxiv_id
                merged[arxiv_id]["query_topics"] = sorted(set(paper.get("query_topics", [])))
            else:
                topics = set(merged[arxiv_id].get("query_topics", []))
                topics.update(paper.get("query_topics", []))
                merged[arxiv_id]["query_topics"] = sorted(topics)
    return sorted(
        merged.values(),
        key=lambda paper: (paper.get("published", ""), paper["_id"]),
        reverse=True,
    )


def filter_since_days(
    papers: Sequence[dict[str, Any]], since_days: int | None, now: dt.datetime | None = None
) -> list[dict[str, Any]]:
    """Filter papers by published timestamp when a recency window is supplied."""

    if since_days is None:
        return list(papers)
    reference = now or dt.datetime.now(dt.timezone.utc)
    cutoff = reference - dt.timedelta(days=max(since_days, 0))
    selected = []
    for paper in papers:
        published = paper.get("published")
        try:
            parsed = dt.datetime.fromisoformat(str(published).replace("Z", "+00:00"))
        except (TypeError, ValueError):
            continue
        if parsed >= cutoff:
            selected.append(paper)
    return selected


def arxiv_id_variants(arxiv_id: str) -> list[str]:
    """Generate conservative exact values commonly stored by arXiv corpora."""

    canonical = canonical_arxiv_id(arxiv_id)
    if not canonical:
        return []
    return [
        canonical,
        f"arXiv:{canonical}",
        f"https://arxiv.org/abs/{canonical}",
        f"http://arxiv.org/abs/{canonical}",
    ]


def arxiv_source_lookup_terms(arxiv_id: str) -> list[Any]:
    """Return exact source values, including an indexed bare-ID version pattern."""

    canonical = canonical_arxiv_id(arxiv_id)
    if not canonical:
        return []
    # The live ArXivDB corpus stores modern identifiers such as
    # ``2402.16562v1``.  An anchored literal-prefix regex can use its ``id``
    # index while still comparing the version-stripped canonical value after
    # retrieval.
    versioned_bare_id = re.compile(rf"^{re.escape(canonical)}v\d+$", re.IGNORECASE)
    return [*arxiv_id_variants(canonical), versioned_bare_id]


def exact_source_matches(
    collection: Any,
    papers: Sequence[dict[str, Any]],
    *,
    source_database: str,
    source_collection: str,
    id_fields: Sequence[str] = ("id", "arxiv_id"),
) -> tuple[dict[str, list[dict[str, Any]]], list[dict[str, Any]]]:
    """Read exact canonical-ID matches from an existing corpus.

    Only ``find`` is invoked on the supplied source collection.
    """

    canonical_ids = [paper["arxiv_id"] for paper in papers]
    lookup_terms: list[Any] = []
    seen_strings: set[str] = set()
    for arxiv_id in canonical_ids:
        for term in arxiv_source_lookup_terms(arxiv_id):
            if isinstance(term, str):
                if term in seen_strings:
                    continue
                seen_strings.add(term)
            lookup_terms.append(term)
    if not lookup_terms:
        return {}, []

    query = {"$or": [{field: {"$in": lookup_terms}} for field in id_fields]}
    projection = {"_id": 1}
    projection.update({field: 1 for field in id_fields})
    matches: dict[str, list[dict[str, Any]]] = {arxiv_id: [] for arxiv_id in canonical_ids}
    links: list[dict[str, Any]] = []

    for document in collection.find(query, projection):
        matched_field = None
        canonical = None
        for field in id_fields:
            candidate = canonical_arxiv_id(document.get(field))
            if candidate in matches:
                matched_field = field
                canonical = candidate
                break
        if canonical is None:
            continue
        reference = {
            "database": source_database,
            "collection": source_collection,
            "document_id": str(document.get("_id")),
            "arxiv_id": canonical,
            "matched_field": matched_field,
            "matched_value": str(document.get(matched_field)),
        }
        matches[canonical].append(reference)
        link = {
            "source": {"kind": "recent_paper", "id": canonical},
            "target": {
                "kind": "mongo_document",
                "database": source_database,
                "collection": source_collection,
                "id": str(document.get("_id")),
                "arxiv_id": canonical,
            },
            "method": "exact_canonical_arxiv_id",
            "score": 1.0,
            "provenance": {
                "matched_field": matched_field,
                "matched_value": str(document.get(matched_field)),
                "canonicalizer": "strip_arxiv_version_v1",
                "source_access": "read_only_find",
            },
        }
        link["_id"] = fingerprint(
            {
                "source": link["source"],
                "target": link["target"],
                "method": link["method"],
            }
        )
        links.append(link)
    return matches, links


def load_web_records(path: Path | None) -> list[dict[str, Any]]:
    """Load optional web/news records from a JSON array or ``{"records": []}``."""

    if path is None:
        return []
    payload = json.loads(path.read_text(encoding="utf-8"))
    if isinstance(payload, dict):
        payload = payload.get("records")
    if not isinstance(payload, list):
        raise PilotError("web/news JSON must be an array or an object containing a records array")
    if not all(isinstance(record, dict) for record in payload):
        raise PilotError("every web/news record must be a JSON object")
    return payload


def normalize_url(value: str) -> str:
    """Normalize a web URL for stable identity without rewriting its path."""

    parsed = urllib.parse.urlsplit(value.strip())
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        raise PilotError(f"web/news record has an invalid URL: {value!r}")
    hostname = (parsed.hostname or "").lower()
    port = f":{parsed.port}" if parsed.port else ""
    netloc = f"{hostname}{port}"
    path = parsed.path or "/"
    return urllib.parse.urlunsplit((parsed.scheme.lower(), netloc, path, parsed.query, ""))


def normalize_web_records(
    records: Sequence[Mapping[str, Any]], fetched_at: str
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Normalize web provenance records and declared arXiv links."""

    normalized: list[dict[str, Any]] = []
    links: list[dict[str, Any]] = []
    for record in records:
        url = normalize_url(str(record.get("url", "")))
        source_id = fingerprint({"url": url})
        content = str(record.get("content") or record.get("summary") or record.get("text") or "")
        raw_arxiv_ids = record.get("arxiv_ids", record.get("arxiv_id", []))
        if isinstance(raw_arxiv_ids, str):
            raw_arxiv_ids = [raw_arxiv_ids]
        if not isinstance(raw_arxiv_ids, list):
            raise PilotError(f"arxiv_ids for {url} must be a string or array")
        raw_topics = record.get("topics") or []
        if isinstance(raw_topics, str):
            raw_topics = [raw_topics]
        if not isinstance(raw_topics, list):
            raise PilotError(f"topics for {url} must be a string or array")
        arxiv_ids = sorted(
            {
                candidate
                for value in raw_arxiv_ids
                if (candidate := canonical_arxiv_id(value)) is not None
            }
        )
        normalized_record = {
            "_id": source_id,
            "url": url,
            "title": normalize_text(str(record.get("title") or "")),
            "published": record.get("published"),
            "fetched": record.get("fetched") or fetched_at,
            "publisher": record.get("publisher"),
            "topics": sorted({str(topic) for topic in raw_topics if str(topic).strip()}),
            "arxiv_ids": arxiv_ids,
            "content_hash": hashlib.sha256(content.encode("utf-8")).hexdigest(),
            "record_hash": fingerprint(
                {
                    "url": url,
                    "title": normalize_text(str(record.get("title") or "")),
                    "published": record.get("published"),
                    "publisher": record.get("publisher"),
                    "arxiv_ids": arxiv_ids,
                    "content_hash": hashlib.sha256(content.encode("utf-8")).hexdigest(),
                }
            ),
            "provenance": {
                "input": "web_news_json",
                "supplied_fetched_timestamp": bool(record.get("fetched")),
            },
        }
        normalized.append(normalized_record)

        for arxiv_id in arxiv_ids:
            link = {
                "source": {"kind": "web_source", "id": source_id, "url": url},
                "target": {"kind": "recent_paper", "id": arxiv_id},
                "method": "declared_arxiv_id",
                "score": 1.0,
                "provenance": {
                    "record_hash": normalized_record["record_hash"],
                    "canonicalizer": "strip_arxiv_version_v1",
                },
            }
            link["_id"] = fingerprint(
                {
                    "source": link["source"],
                    "target": link["target"],
                    "method": link["method"],
                }
            )
            links.append(link)
    return normalized, links


def embed_with_ollama(
    texts: Sequence[str], *, ollama_url: str, model: str, timeout: float
) -> list[list[float]]:
    """Embed texts through Ollama's current ``/api/embed`` endpoint."""

    if not texts:
        return []
    response = _http_request_json(
        f"{ollama_url.rstrip('/')}/api/embed",
        {"model": model, "input": list(texts)},
        timeout=timeout,
    )
    embeddings = response.get("embeddings") if isinstance(response, dict) else None
    if not isinstance(embeddings, list) or len(embeddings) != len(texts):
        raise PilotError("Ollama embed response did not contain one embedding per input")
    return embeddings


def qdrant_neighbors(
    vector: Sequence[float],
    *,
    qdrant_url: str,
    collection: str,
    limit: int,
    timeout: float,
) -> list[dict[str, Any]]:
    """Query Qdrant, supporting both the newer query and legacy search APIs."""

    base = qdrant_url.rstrip("/")
    encoded_collection = urllib.parse.quote(collection, safe="")
    query_url = f"{base}/collections/{encoded_collection}/points/query"
    try:
        response = _http_request_json(
            query_url,
            {"query": list(vector), "limit": limit, "with_payload": True},
            timeout=timeout,
        )
        result = response.get("result", {}) if isinstance(response, dict) else {}
        points = result.get("points", []) if isinstance(result, dict) else []
    except urllib.error.HTTPError as exc:
        if exc.code not in {404, 405}:
            raise
        response = _http_request_json(
            f"{base}/collections/{encoded_collection}/points/search",
            {"vector": list(vector), "limit": limit, "with_payload": True},
            timeout=timeout,
        )
        points = response.get("result", []) if isinstance(response, dict) else []

    neighbors = []
    for point in points if isinstance(points, list) else []:
        if not isinstance(point, dict):
            continue
        payload = point.get("payload") if isinstance(point.get("payload"), dict) else {}
        candidate = next(
            (
                canonical_arxiv_id(payload.get(key))
                for key in ("arxiv_id", "id", "identifier")
                if payload.get(key)
            ),
            None,
        )
        if not candidate:
            continue
        neighbors.append(
            {
                "arxiv_id": candidate,
                "point_id": str(point.get("id")),
                "score": float(point.get("score", 0.0)),
                "payload_title": payload.get("title"),
            }
        )
    return neighbors


def build_semantic_links(
    papers: Sequence[dict[str, Any]],
    *,
    ollama_url: str,
    embedding_model: str,
    qdrant_url: str,
    qdrant_collection: str,
    neighbor_limit: int,
    paper_limit: int,
    timeout: float,
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    """Link recent candidates to semantic neighbors with explicit provenance."""

    selected = list(papers[: max(paper_limit, 0)])
    texts = [f"{paper.get('title', '')}\n\n{paper.get('abstract', '')}" for paper in selected]
    embeddings = embed_with_ollama(
        texts, ollama_url=ollama_url, model=embedding_model, timeout=timeout
    )
    links: list[dict[str, Any]] = []
    query_metrics = []
    for paper, vector in zip(selected, embeddings):
        neighbors = qdrant_neighbors(
            vector,
            qdrant_url=qdrant_url,
            collection=qdrant_collection,
            limit=neighbor_limit + 1,
            timeout=timeout,
        )
        emitted = 0
        for neighbor in neighbors:
            if neighbor["arxiv_id"] == paper["arxiv_id"]:
                continue
            link = {
                "source": {"kind": "recent_paper", "id": paper["arxiv_id"]},
                "target": {
                    "kind": "qdrant_arxiv_paper",
                    "id": neighbor["arxiv_id"],
                    "point_id": neighbor["point_id"],
                },
                "method": "ollama_embedding_qdrant_cosine",
                "score": neighbor["score"],
                "provenance": {
                    "embedding_model": embedding_model,
                    "qdrant_collection": qdrant_collection,
                    "ollama_endpoint": public_endpoint(ollama_url),
                    "qdrant_endpoint": public_endpoint(qdrant_url),
                    "query_content_hash": paper.get("content_hash"),
                },
            }
            link["_id"] = fingerprint(
                {
                    "source": link["source"],
                    "target": link["target"],
                    "method": link["method"],
                    "embedding_model": embedding_model,
                    "qdrant_collection": qdrant_collection,
                }
            )
            links.append(link)
            emitted += 1
            if emitted >= neighbor_limit:
                break
        query_metrics.append(
            {
                "arxiv_id": paper["arxiv_id"],
                "returned": len(neighbors),
                "linked": emitted,
            }
        )
    return links, {"queries": len(selected), "links": len(links), "per_paper": query_metrics}


def public_endpoint(url: str) -> str:
    """Remove credentials, paths, query parameters, and fragments from an endpoint."""

    parsed = urllib.parse.urlsplit(url)
    if not parsed.scheme or not parsed.hostname:
        return "configured"
    port = f":{parsed.port}" if parsed.port else ""
    return f"{parsed.scheme}://{parsed.hostname}{port}"


def _uri_database_name(uri: str | None) -> str | None:
    if not uri:
        return None
    parsed = urllib.parse.urlsplit(uri)
    path = parsed.path.strip("/")
    return urllib.parse.unquote(path) if path else None


def validate_write_isolation(
    *,
    write: bool,
    source_uri: str | None,
    source_database: str,
    target_uri: str | None,
    target_database: str,
) -> None:
    """Fail fast if write mode could target the source corpus."""

    if not write:
        return
    if not target_uri:
        raise PilotError(
            "--write requires --target-mongo-uri or RECENT_TARGET_MONGO_URI; "
            "the source URI is never reused"
        )
    if source_database.casefold() == target_database.casefold():
        raise PilotError("target database must differ from the source database")
    target_uri_database = _uri_database_name(target_uri)
    if target_uri_database and target_uri_database.casefold() == source_database.casefold():
        raise PilotError("target Mongo URI names the source database")
    if source_uri and target_uri.strip() == source_uri.strip():
        raise PilotError("write mode requires a distinct target Mongo URI")


def _import_pymongo() -> Any:
    try:
        import pymongo  # type: ignore[import-not-found]
    except ImportError as exc:
        raise PilotError(
            "MongoDB access requires the optional pymongo package: python -m pip install pymongo"
        ) from exc
    return pymongo


def connect_mongo(uri: str, *, timeout_ms: int) -> Any:
    """Create and validate a Mongo client without exposing its URI."""

    pymongo = _import_pymongo()
    client = pymongo.MongoClient(
        uri,
        serverSelectionTimeoutMS=timeout_ms,
        connectTimeoutMS=timeout_ms,
        socketTimeoutMS=max(timeout_ms, 30_000),
        appname="llamafarm-recent-research-pilot",
    )
    client.admin.command("ping")
    return client


def _upsert(collection: Any, document: Mapping[str, Any], observed_at: str) -> dict[str, int]:
    stable_id = document.get("_id")
    if stable_id is None:
        raise PilotError("refusing to upsert a record without a stable _id")
    body = dict(document)
    # MongoDB derives _id from the equality filter during an upsert.  Including
    # it in $set would attempt to modify an immutable field on repeat runs.
    body.pop("_id", None)
    body["last_seen_at"] = observed_at
    result = collection.update_one(
        {"_id": stable_id},
        {
            "$set": body,
            "$setOnInsert": {"first_seen_at": observed_at},
        },
        upsert=True,
    )
    return {
        "attempted": 1,
        "matched": int(getattr(result, "matched_count", 0)),
        "modified": int(getattr(result, "modified_count", 0)),
        "upserted": int(getattr(result, "upserted_id", None) is not None),
    }


def _merge_counts(target: dict[str, int], result: Mapping[str, int]) -> None:
    for key, value in result.items():
        target[key] = target.get(key, 0) + int(value)


def ensure_target_indexes(database: Any) -> None:
    """Create the non-default indexes used by the isolated target database."""

    database["papers"].create_index([("arxiv_id", 1)], unique=True, name="arxiv_id_unique")
    database["papers"].create_index([("published", -1)], name="published_desc")
    database["papers"].create_index([("query_topics", 1)], name="query_topics")
    database["web_sources"].create_index([("url", 1)], unique=True, name="url_unique")
    database["web_sources"].create_index([("published", -1)], name="published_desc")
    database["links"].create_index(
        [("source.kind", 1), ("source.id", 1), ("method", 1)], name="source_method"
    )
    database["links"].create_index(
        [("target.kind", 1), ("target.id", 1), ("method", 1)], name="target_method"
    )
    database["crawl_runs"].create_index([("started_at", -1)], name="started_at_desc")
    database["crawl_runs"].create_index([("status", 1)], name="status")


def persist_dataset(
    database: Any,
    *,
    papers: Sequence[Mapping[str, Any]],
    web_sources: Sequence[Mapping[str, Any]],
    links: Sequence[Mapping[str, Any]],
    run_document: Mapping[str, Any],
    observed_at: str,
) -> dict[str, dict[str, int]]:
    """Idempotently upsert one dataset; safe to rerun with the same run ID."""

    ensure_target_indexes(database)
    summaries: dict[str, dict[str, int]] = {
        "crawl_runs": {"attempted": 0, "matched": 0, "modified": 0, "upserted": 0}
    }
    running_run = dict(run_document)
    running_run["status"] = "running"
    running_run.pop("finished_at", None)
    _merge_counts(
        summaries["crawl_runs"],
        _upsert(database["crawl_runs"], running_run, observed_at),
    )

    for collection_name, documents in (
        ("papers", papers),
        ("web_sources", web_sources),
        ("links", links),
    ):
        counts: dict[str, int] = {"attempted": 0, "matched": 0, "modified": 0, "upserted": 0}
        for document in documents:
            _merge_counts(counts, _upsert(database[collection_name], document, observed_at))
        summaries[collection_name] = counts

    run = dict(run_document)
    run.setdefault("status", "complete")
    _merge_counts(
        summaries["crawl_runs"], _upsert(database["crawl_runs"], run, observed_at)
    )
    return summaries


def _unique_links(links: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    by_id = {link["_id"]: link for link in links}
    return [by_id[key] for key in sorted(by_id)]


def _write_report(report: Mapping[str, Any], destination: str) -> None:
    output = json.dumps(report, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    if destination == "-":
        sys.stdout.write(output)
        return
    path = Path(destination).expanduser()
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(output, encoding="utf-8")
    temporary.replace(path)
    print(f"report: {path}", file=sys.stderr)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Create an auditable recent-research report. Defaults to a dry run; "
            "MongoDB writes require --write and a separate target URI."
        )
    )
    parser.add_argument(
        "--topic",
        action="append",
        dest="topics",
        help="arXiv topic phrase; repeat for multiple topics (default: three unrelated samples)",
    )
    parser.add_argument("--max-results-per-topic", type=int, default=5)
    parser.add_argument("--since-days", type=int)
    parser.add_argument("--arxiv-endpoint", default=DEFAULT_ARXIV_ENDPOINT)
    parser.add_argument(
        "--cache-dir",
        default=os.environ.get(
            "RECENT_ARXIV_CACHE_DIR", "~/.cache/llamafarm/recent-research/arxiv"
        ),
    )
    parser.add_argument("--no-cache", action="store_true")
    parser.add_argument("--cache-ttl-seconds", type=float, default=21_600)
    parser.add_argument("--rate-limit-seconds", type=float, default=3.0)
    parser.add_argument("--http-timeout-seconds", type=float, default=30.0)
    parser.add_argument("--http-retries", type=int, default=2)
    parser.add_argument("--user-agent", default="LlamaFarmRecentResearch/1.0")
    parser.add_argument("--web-records", type=Path, help="optional JSON web/news records")

    parser.add_argument(
        "--source-mongo-uri",
        default=os.environ.get("ARXIV_SOURCE_MONGO_URI"),
        help="read-only source URI (or ARXIV_SOURCE_MONGO_URI)",
    )
    parser.add_argument("--source-db", default="ArXivDB")
    parser.add_argument("--source-collection", default="Papers")
    parser.add_argument(
        "--source-id-fields",
        default="id",
        help=(
            "comma-separated indexed exact-ID fields in the source corpus "
            "(default: id; avoid unindexed fields on large collections)"
        ),
    )
    parser.add_argument("--mongo-timeout-ms", type=int, default=10_000)

    parser.add_argument("--semantic", action="store_true", help="query Ollama and Qdrant")
    parser.add_argument(
        "--ollama-url", default=os.environ.get("OLLAMA_URL", "http://127.0.0.1:11434")
    )
    parser.add_argument(
        "--embedding-model",
        default=os.environ.get("RECENT_EMBEDDING_MODEL", "nomic-embed-text"),
    )
    parser.add_argument(
        "--qdrant-url", default=os.environ.get("QDRANT_URL", "http://127.0.0.1:6333")
    )
    parser.add_argument("--qdrant-collection", default="arxiv_papers")
    parser.add_argument("--semantic-neighbors", type=int, default=3)
    parser.add_argument("--semantic-paper-limit", type=int, default=6)
    parser.add_argument(
        "--strict-semantic",
        action="store_true",
        help="fail instead of recording an Ollama/Qdrant error",
    )

    parser.add_argument("--write", action="store_true", help="enable isolated target DB upserts")
    parser.add_argument(
        "--target-mongo-uri",
        default=os.environ.get("RECENT_TARGET_MONGO_URI"),
        help="separate target URI required by --write (or RECENT_TARGET_MONGO_URI)",
    )
    parser.add_argument("--target-db", default="recent")
    parser.add_argument(
        "--run-id",
        help="stable run ID for explicit resume/idempotency (default: generated per execution)",
    )
    parser.add_argument("--report", default="-", help="JSON report path, or - for stdout")
    return parser


def execute(args: argparse.Namespace) -> tuple[dict[str, Any], int]:
    started_at = utc_now()
    topics = list(args.topics or DEFAULT_TOPICS)
    if not topics:
        raise PilotError("at least one topic is required")
    if args.max_results_per_topic < 1 or args.max_results_per_topic > 100:
        raise PilotError("--max-results-per-topic must be between 1 and 100")
    if args.semantic_neighbors < 1:
        raise PilotError("--semantic-neighbors must be positive")

    validate_write_isolation(
        write=args.write,
        source_uri=args.source_mongo_uri,
        source_database=args.source_db,
        target_uri=args.target_mongo_uri,
        target_database=args.target_db,
    )

    run_seed = {
        "started_at": started_at,
        "topics": topics,
        "max_results_per_topic": args.max_results_per_topic,
        "since_days": args.since_days,
    }
    compact_started_at = started_at.replace(":", "").replace("-", "")
    run_id = args.run_id or f"{compact_started_at}-{fingerprint(run_seed)[:12]}"
    cache_dir = None if args.no_cache else Path(args.cache_dir).expanduser()
    client = ArxivClient(
        endpoint=args.arxiv_endpoint,
        cache_dir=cache_dir,
        cache_ttl_seconds=args.cache_ttl_seconds,
        rate_limit_seconds=args.rate_limit_seconds,
        timeout_seconds=args.http_timeout_seconds,
        retries=args.http_retries,
        user_agent=args.user_agent,
    )

    fetches: list[dict[str, Any]] = []
    fetch_errors: list[dict[str, str]] = []
    topic_papers: list[list[dict[str, Any]]] = []
    for topic in topics:
        try:
            papers, fetch = client.fetch(topic, args.max_results_per_topic)
            topic_papers.append(papers)
            fetches.append(fetch)
        except Exception as exc:  # each topic remains independently auditable
            fetch_errors.append({"topic": topic, "error": str(exc)})

    papers = filter_since_days(merge_papers(topic_papers), args.since_days)
    observed_at = utc_now()
    source_matches: dict[str, list[dict[str, Any]]] = {}
    exact_links: list[dict[str, Any]] = []
    source_status: dict[str, Any] = {
        "configured": bool(args.source_mongo_uri),
        "database": args.source_db,
        "collection": args.source_collection,
        "access": "read_only_find",
        "status": "skipped",
    }
    source_error = None
    if args.source_mongo_uri:
        source_client = None
        try:
            source_client = connect_mongo(args.source_mongo_uri, timeout_ms=args.mongo_timeout_ms)
            source_matches, exact_links = exact_source_matches(
                source_client[args.source_db][args.source_collection],
                papers,
                source_database=args.source_db,
                source_collection=args.source_collection,
                id_fields=[
                    field.strip()
                    for field in args.source_id_fields.split(",")
                    if field.strip()
                ],
            )
            source_status["status"] = "ok"
            source_status["matched_papers"] = sum(
                bool(values) for values in source_matches.values()
            )
            for paper in papers:
                paper["existing_corpus_matches"] = source_matches.get(paper["arxiv_id"], [])
        except Exception as exc:
            source_error = str(exc)
            source_status["status"] = "error"
            source_status["error"] = source_error
        finally:
            if source_client is not None:
                source_client.close()

    raw_web_records = load_web_records(args.web_records)
    web_sources, web_links = normalize_web_records(raw_web_records, observed_at)

    semantic_links: list[dict[str, Any]] = []
    semantic_status: dict[str, Any] = {
        "enabled": args.semantic,
        "status": "skipped",
    }
    if args.semantic:
        try:
            semantic_links, semantic_metrics = build_semantic_links(
                papers,
                ollama_url=args.ollama_url,
                embedding_model=args.embedding_model,
                qdrant_url=args.qdrant_url,
                qdrant_collection=args.qdrant_collection,
                neighbor_limit=args.semantic_neighbors,
                paper_limit=args.semantic_paper_limit,
                timeout=args.http_timeout_seconds,
            )
            semantic_status = {
                "enabled": True,
                "status": "ok",
                "embedding_model": args.embedding_model,
                "qdrant_collection": args.qdrant_collection,
                **semantic_metrics,
            }
        except Exception as exc:
            semantic_status = {"enabled": True, "status": "error", "error": str(exc)}
            if args.strict_semantic:
                raise PilotError(f"semantic linking failed: {exc}") from exc

    links = _unique_links([*exact_links, *web_links, *semantic_links])
    finished_at = utc_now()
    metrics = {
        "topics_requested": len(topics),
        "topics_fetched": len(fetches),
        "topics_failed": len(fetch_errors),
        "entries_fetched": sum(fetch["entries"] for fetch in fetches),
        "unique_recent_papers": len(papers),
        "duplicate_entries": max(
            0, sum(fetch["entries"] for fetch in fetches) - len(merge_papers(topic_papers))
        ),
        "source_exact_matched_papers": sum(bool(values) for values in source_matches.values()),
        "source_exact_links": len(exact_links),
        "web_sources": len(web_sources),
        "web_declared_links": len(web_links),
        "semantic_links": len(semantic_links),
        "total_links": len(links),
    }
    run_document = {
        "_id": run_id,
        "run_id": run_id,
        "status": "complete",
        "mode": "write" if args.write else "dry_run",
        "started_at": started_at,
        "finished_at": finished_at,
        "topics": topics,
        "metrics": metrics,
        "source_status": source_status,
        "semantic_status": semantic_status,
    }

    write_summary = None
    write_error = None
    if args.write:
        target_client = None
        try:
            target_client = connect_mongo(args.target_mongo_uri, timeout_ms=args.mongo_timeout_ms)
            write_summary = persist_dataset(
                target_client[args.target_db],
                papers=papers,
                web_sources=web_sources,
                links=links,
                run_document=run_document,
                observed_at=observed_at,
            )
        except Exception as exc:
            write_error = str(exc)
        finally:
            if target_client is not None:
                target_client.close()

    report = {
        "schema_version": 1,
        "run_id": run_id,
        "mode": "write" if args.write else "dry_run",
        "started_at": started_at,
        "finished_at": finished_at,
        "isolation": {
            "source_database": args.source_db,
            "source_collection": args.source_collection,
            "source_uri_configured": bool(args.source_mongo_uri),
            "source_operations": ["ping", "find"] if args.source_mongo_uri else [],
            "target_database": args.target_db,
            "target_uri_configured": bool(args.target_mongo_uri),
            "writes_enabled": bool(args.write),
            "allowed_write_collections": [
                "papers",
                "web_sources",
                "links",
                "crawl_runs",
            ],
        },
        "configuration": {
            "topics": topics,
            "max_results_per_topic": args.max_results_per_topic,
            "since_days": args.since_days,
            "arxiv_endpoint": public_endpoint(args.arxiv_endpoint),
            "cache_enabled": not args.no_cache,
            "cache_ttl_seconds": args.cache_ttl_seconds,
            "rate_limit_seconds": args.rate_limit_seconds,
            "semantic_enabled": args.semantic,
            "embedding_model": args.embedding_model if args.semantic else None,
            "qdrant_collection": args.qdrant_collection if args.semantic else None,
        },
        "fetches": fetches,
        "fetch_errors": fetch_errors,
        "source_status": source_status,
        "semantic_status": semantic_status,
        "metrics": metrics,
        "papers": papers,
        "web_sources": web_sources,
        "links": links,
        "write_summary": write_summary,
        "write_error": write_error,
    }
    report["report_hash"] = fingerprint(
        {key: value for key, value in report.items() if key != "report_hash"}
    )

    exit_code = 0
    if not papers:
        exit_code = 2
    elif source_error or write_error:
        exit_code = 1
    return report, exit_code


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        report, exit_code = execute(args)
    except (PilotError, json.JSONDecodeError, OSError) as exc:
        parser.error(str(exc))
    _write_report(report, args.report)
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
