import importlib.util
from pathlib import Path
import stat
import tempfile
import tomllib
import unittest


SCRIPT_PATH = Path(__file__).parents[1] / "merge_builtin_agents.py"
SPEC = importlib.util.spec_from_file_location("merge_builtin_agents", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
MIGRATION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MIGRATION)

TEMPLATE = """
default_provider = "ollama"

[[model_routes]]
hint = "devsecops"
provider = "ollama"
model = "runtime-model"

[agents.devsecops]
provider = ""
model = ""
agentic = true
system_prompt = \"\"\"
Verify real state.
\"\"\"
allowed_tools = ["shell", "docker", "db_schema"]
max_iterations = 12
"""


class MergeBuiltinAgentsTests(unittest.TestCase):
    def test_agent_extraction_stops_before_following_global_table(self):
        template = (
            TEMPLATE
            + """

[arxiv_rag]
enabled = true
collection = "arxiv_papers"
"""
        )
        original = """
[arxiv_rag]
enabled = true
collection = "existing-corpus"
"""

        merged, additions = MIGRATION.merge_builtin_agents(original, template)

        self.assertEqual(
            additions, ["model_routes:devsecops", "agents:devsecops"]
        )
        parsed = tomllib.loads(merged)
        self.assertEqual(parsed["arxiv_rag"]["collection"], "existing-corpus")
        self.assertIn("devsecops", parsed["agents"])

    def test_adds_missing_route_and_agent_without_rewriting_existing_text(self):
        original = (
            'api_key = "enc:v1:sensitive-value" # preserve this exact line\n'
            'default_model = "custom-model"\n'
        )

        merged, additions = MIGRATION.merge_builtin_agents(original, TEMPLATE)

        self.assertTrue(merged.startswith(original))
        self.assertEqual(
            additions, ["model_routes:devsecops", "agents:devsecops"]
        )
        parsed = tomllib.loads(merged)
        self.assertEqual(parsed["api_key"], "enc:v1:sensitive-value")
        self.assertEqual(parsed["agents"]["devsecops"]["max_iterations"], 12)
        self.assertEqual(parsed["model_routes"][0]["hint"], "devsecops")

    def test_second_merge_is_byte_for_byte_idempotent(self):
        first, _ = MIGRATION.merge_builtin_agents('default_model = "custom"\n', TEMPLATE)

        second, additions = MIGRATION.merge_builtin_agents(first, TEMPLATE)

        self.assertEqual(second, first)
        self.assertEqual(additions, [])

    def test_existing_custom_agent_is_authoritative(self):
        original = """
[[model_routes]]
hint = "devsecops"
provider = "custom"
model = "operator-choice"

[agents.devsecops]
provider = "custom"
model = "operator-choice"
agentic = false
allowed_tools = ["file_read"]
"""

        merged, additions = MIGRATION.merge_builtin_agents(original, TEMPLATE)

        self.assertEqual(merged, original)
        self.assertEqual(additions, [])
        parsed = tomllib.loads(merged)
        self.assertFalse(parsed["agents"]["devsecops"]["agentic"])
        self.assertEqual(parsed["model_routes"][0]["provider"], "custom")

    def test_adds_only_missing_agent_when_custom_route_exists(self):
        original = """
[[model_routes]]
hint = "devsecops"
provider = "custom"
model = "operator-choice"
"""

        merged, additions = MIGRATION.merge_builtin_agents(original, TEMPLATE)

        self.assertEqual(additions, ["agents:devsecops"])
        parsed = tomllib.loads(merged)
        self.assertEqual(parsed["model_routes"][0]["provider"], "custom")
        self.assertIn("devsecops", parsed["agents"])

    def test_migrate_file_preserves_permissions(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config_path = root / "config.toml"
            template_path = root / "template.toml"
            config_path.write_text('default_model = "custom"\n', encoding="utf-8")
            template_path.write_text(TEMPLATE, encoding="utf-8")
            config_path.chmod(0o600)

            additions = MIGRATION.migrate_file(config_path, template_path)

            self.assertEqual(
                additions, ["model_routes:devsecops", "agents:devsecops"]
            )
            self.assertEqual(stat.S_IMODE(config_path.stat().st_mode), 0o600)

    def test_invalid_config_is_rejected_without_writing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config_path = root / "config.toml"
            template_path = root / "template.toml"
            invalid = "[agents.invalid\nsecret = \"keep-me\"\n"
            config_path.write_text(invalid, encoding="utf-8")
            template_path.write_text(TEMPLATE, encoding="utf-8")

            with self.assertRaises(ValueError):
                MIGRATION.migrate_file(config_path, template_path)

            self.assertEqual(config_path.read_text(encoding="utf-8"), invalid)


if __name__ == "__main__":
    unittest.main()
