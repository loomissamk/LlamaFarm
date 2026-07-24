"""Offline tests for the isolated recent-research pilot."""

from __future__ import annotations

import importlib.util
import json
import sys
import unittest
from pathlib import Path
from typing import Any


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "recent_research_pilot.py"
SPEC = importlib.util.spec_from_file_location("recent_research_pilot", SCRIPT)
assert SPEC and SPEC.loader
pilot = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = pilot
SPEC.loader.exec_module(pilot)


ATOM_SAMPLE = """\
<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"
      xmlns:arxiv="http://arxiv.org/schemas/atom">
  <entry>
    <id>https://arxiv.org/abs/2401.01234v2</id>
    <updated>2026-07-23T12:00:00Z</updated>
    <published>2026-07-22T10:00:00Z</published>
    <title>
      A   Useful
      Paper
    </title>
    <summary>First abstract.</summary>
    <author><name>Ada Example</name></author>
    <author><name>Lin Example</name></author>
    <category term="cs.AI"/>
    <category term="cs.LG"/>
    <link href="https://arxiv.org/abs/2401.01234v2" rel="alternate"/>
    <link href="https://arxiv.org/pdf/2401.01234v2" type="application/pdf"/>
    <arxiv:doi>10.1000/example</arxiv:doi>
  </entry>
  <entry>
    <id>http://arxiv.org/abs/hep-th/9901001v3</id>
    <updated>2026-07-21T12:00:00Z</updated>
    <published>2026-07-20T10:00:00Z</published>
    <title>Legacy identifier</title>
    <summary>Second abstract.</summary>
    <author><name>Grace Example</name></author>
    <category term="hep-th"/>
  </entry>
</feed>
"""


class FakeResult:
    def __init__(self, *, matched: int, modified: int, upserted_id: Any) -> None:
        self.matched_count = matched
        self.modified_count = modified
        self.upserted_id = upserted_id


class FakeCollection:
    def __init__(self) -> None:
        self.documents: dict[Any, dict[str, Any]] = {}
        self.indexes: list[tuple[Any, dict[str, Any]]] = []

    def create_index(self, keys: Any, **kwargs: Any) -> None:
        self.indexes.append((keys, kwargs))

    def update_one(
        self, selector: dict[str, Any], update: dict[str, Any], *, upsert: bool
    ) -> FakeResult:
        key = selector["_id"]
        existed = key in self.documents
        document = dict(self.documents.get(key, {"_id": key}))
        if not existed:
            document.update(update.get("$setOnInsert", {}))
        before = json.dumps(document, sort_keys=True)
        document.update(update.get("$set", {}))
        self.documents[key] = document
        after = json.dumps(document, sort_keys=True)
        return FakeResult(
            matched=int(existed),
            modified=int(existed and before != after),
            upserted_id=None if existed else key,
        )


class FakeDatabase:
    def __init__(self) -> None:
        self.collections: dict[str, FakeCollection] = {}

    def __getitem__(self, name: str) -> FakeCollection:
        return self.collections.setdefault(name, FakeCollection())


class CanonicalArxivIdTests(unittest.TestCase):
    def test_modern_legacy_urls_prefixes_and_versions(self) -> None:
        cases = {
            "arXiv:2401.01234v2": "2401.01234",
            "https://arxiv.org/abs/2401.01234v9": "2401.01234",
            "https://arxiv.org/pdf/2401.01234v1.pdf?download=1": "2401.01234",
            "hep-th/9901001v3": "hep-th/9901001",
            "oai:arXiv.org:math.GT/0309136v1": "math.gt/0309136",
            "not-an-arxiv-id": None,
            "": None,
        }
        for value, expected in cases.items():
            with self.subTest(value=value):
                self.assertEqual(pilot.canonical_arxiv_id(value), expected)

    def test_source_lookup_terms_match_bare_versioned_id(self) -> None:
        terms = pilot.arxiv_source_lookup_terms("2401.01234")
        patterns = [term for term in terms if hasattr(term, "fullmatch")]
        self.assertIn("2401.01234", terms)
        self.assertEqual(len(patterns), 1)
        self.assertTrue(patterns[0].fullmatch("2401.01234v12"))
        self.assertFalse(patterns[0].fullmatch("2401.012340v1"))


class AtomParsingTests(unittest.TestCase):
    def test_parse_atom_normalizes_and_preserves_provenance(self) -> None:
        papers = pilot.parse_arxiv_atom(ATOM_SAMPLE, "test topic")
        self.assertEqual([paper["arxiv_id"] for paper in papers], [
            "2401.01234",
            "hep-th/9901001",
        ])
        self.assertEqual(papers[0]["title"], "A Useful Paper")
        self.assertEqual(papers[0]["authors"], ["Ada Example", "Lin Example"])
        self.assertEqual(papers[0]["categories"], ["cs.AI", "cs.LG"])
        self.assertEqual(papers[0]["query_topics"], ["test topic"])
        self.assertEqual(papers[0]["doi"], "10.1000/example")
        self.assertEqual(len(papers[0]["content_hash"]), 64)

    def test_query_requests_submission_date_sort(self) -> None:
        url = pilot.arxiv_query_url("https://export.arxiv.org/api/query", "graph theory", 7)
        query = __import__("urllib.parse").parse.urlsplit(url).query
        params = __import__("urllib.parse").parse.parse_qs(query)
        self.assertEqual(params["sortBy"], ["submittedDate"])
        self.assertEqual(params["sortOrder"], ["descending"])
        self.assertEqual(params["max_results"], ["7"])


class FingerprintTests(unittest.TestCase):
    def test_fingerprint_is_order_independent_and_sensitive_to_values(self) -> None:
        first = pilot.fingerprint({"b": [2, 3], "a": 1})
        reordered = pilot.fingerprint({"a": 1, "b": [2, 3]})
        changed = pilot.fingerprint({"a": 1, "b": [2, 4]})
        self.assertEqual(first, reordered)
        self.assertNotEqual(first, changed)
        self.assertEqual(len(first), 64)

    def test_web_record_identity_and_hashes_are_stable(self) -> None:
        records = [
            {
                "url": "HTTPS://Example.COM/story#section",
                "title": "  New   result ",
                "published": "2026-07-24T00:00:00Z",
                "content": "article text",
                "arxiv_ids": ["arXiv:2401.01234v4"],
            }
        ]
        first, first_links = pilot.normalize_web_records(records, "2026-07-24T01:00:00Z")
        second, second_links = pilot.normalize_web_records(records, "2026-07-24T02:00:00Z")
        self.assertEqual(first[0]["_id"], second[0]["_id"])
        self.assertEqual(first[0]["record_hash"], second[0]["record_hash"])
        self.assertEqual(first[0]["content_hash"], second[0]["content_hash"])
        self.assertEqual(first_links[0]["_id"], second_links[0]["_id"])
        self.assertEqual(first[0]["url"], "https://example.com/story")


class IsolationTests(unittest.TestCase):
    def test_dry_run_never_requires_target(self) -> None:
        pilot.validate_write_isolation(
            write=False,
            source_uri="mongodb://source/ArXivDB",
            source_database="ArXivDB",
            target_uri=None,
            target_database="ArXivDB",
        )

    def test_write_requires_separate_target_uri_and_database(self) -> None:
        with self.assertRaisesRegex(pilot.PilotError, "requires --target-mongo-uri"):
            pilot.validate_write_isolation(
                write=True,
                source_uri="mongodb://source/ArXivDB",
                source_database="ArXivDB",
                target_uri=None,
                target_database="recent",
            )
        with self.assertRaisesRegex(pilot.PilotError, "must differ"):
            pilot.validate_write_isolation(
                write=True,
                source_uri="mongodb://source/ArXivDB",
                source_database="ArXivDB",
                target_uri="mongodb://writer/other",
                target_database="arxivdb",
            )
        with self.assertRaisesRegex(pilot.PilotError, "names the source"):
            pilot.validate_write_isolation(
                write=True,
                source_uri="mongodb://source/ArXivDB",
                source_database="ArXivDB",
                target_uri="mongodb://writer/ArXivDB",
                target_database="recent",
            )
        with self.assertRaisesRegex(pilot.PilotError, "distinct target"):
            pilot.validate_write_isolation(
                write=True,
                source_uri="mongodb://same-host",
                source_database="ArXivDB",
                target_uri="mongodb://same-host",
                target_database="recent",
            )

        pilot.validate_write_isolation(
            write=True,
            source_uri="mongodb://reader/source",
            source_database="ArXivDB",
            target_uri="mongodb://writer/recent",
            target_database="recent",
        )


class IdempotentPersistenceTests(unittest.TestCase):
    def test_repeated_upserts_do_not_duplicate_records(self) -> None:
        database = FakeDatabase()
        paper = {"_id": "2401.01234", "arxiv_id": "2401.01234", "title": "Paper"}
        web = {"_id": "web-1", "url": "https://example.com/story"}
        link = {
            "_id": "link-1",
            "source": {"kind": "web_source", "id": "web-1"},
            "target": {"kind": "recent_paper", "id": "2401.01234"},
            "method": "declared_arxiv_id",
        }
        run = {"_id": "run-1", "run_id": "run-1", "status": "complete"}

        first = pilot.persist_dataset(
            database,
            papers=[paper],
            web_sources=[web],
            links=[link],
            run_document=run,
            observed_at="2026-07-24T01:00:00Z",
        )
        second = pilot.persist_dataset(
            database,
            papers=[paper],
            web_sources=[web],
            links=[link],
            run_document=run,
            observed_at="2026-07-24T02:00:00Z",
        )

        self.assertEqual(first["papers"]["upserted"], 1)
        self.assertEqual(second["papers"]["matched"], 1)
        self.assertEqual(first["crawl_runs"]["attempted"], 2)
        self.assertEqual(len(database["papers"].documents), 1)
        self.assertEqual(len(database["web_sources"].documents), 1)
        self.assertEqual(len(database["links"].documents), 1)
        self.assertEqual(len(database["crawl_runs"].documents), 1)
        self.assertEqual(
            database["papers"].documents["2401.01234"]["first_seen_at"],
            "2026-07-24T01:00:00Z",
        )
        self.assertEqual(
            database["papers"].documents["2401.01234"]["last_seen_at"],
            "2026-07-24T02:00:00Z",
        )
        self.assertEqual(database["crawl_runs"].documents["run-1"]["status"], "complete")
        self.assertTrue(database["papers"].indexes)


if __name__ == "__main__":
    unittest.main()
