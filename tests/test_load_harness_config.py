from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from benchmarks.load.config import ConfigValidationError, ScenarioOverrides, load_scenario
from benchmarks.load.fixtures_support import (
    FixtureError,
    append_to_known_file,
    fixture_digest,
    list_fixture_ids,
    prepare_fixture_working_copy,
    replace_in_known_file,
    resolve_fixture,
    touch_known_file,
)


ROOT = Path(__file__).resolve().parents[1]
STARTER = ROOT / "benchmarks/load/scenarios/starter.json"
FIXTURE_REPOS = ROOT / "benchmarks/load/scenarios/fixture-repos.json"
WATCHER_MUTATION = ROOT / "benchmarks/load/scenarios/watcher-mutation.json"


def write_scenario(tmp_path: Path, updates: dict) -> Path:
    data = json.loads(STARTER.read_text(encoding="utf-8"))
    data.update(updates)
    path = tmp_path / "scenario.json"
    path.write_text(json.dumps(data), encoding="utf-8")
    return path


class LoadHarnessConfigTests(unittest.TestCase):
    def test_starter_scenario_normalizes_all_required_workload_knobs(self) -> None:
        normalized = load_scenario(STARTER)

        self.assertEqual(normalized["scenario_id"], "SCENARIO-01")
        self.assertEqual(normalized["agent_count"], 2)
        self.assertEqual(normalized["project_count"], 1)
        self.assertEqual(normalized["duration_seconds"], 60)
        self.assertEqual(
            normalized["request_pacing"],
            {
                "mode": "fixed_rps",
                "requests_per_second": 1.5,
            },
        )
        self.assertEqual(normalized["fixture_id"], "starter-project")
        self.assertEqual(normalized["fixture"]["fixture_id"], "starter-project")
        self.assertIn("search_symbols", normalized["fixture"]["known_targets"])
        self.assertTrue(normalized["scenario_mix"])
        self.assertEqual(
            normalized["server_options"],
            {
                "host": "127.0.0.1",
                "port": 3000,
                "reuse_existing": False,
            },
        )
        self.assertEqual(normalized["cpu_profile_label"], "none")
        self.assertEqual(
            normalized["output_dir"],
            "benchmarks/load/reports/starter-smoke",
        )
        self.assertEqual(normalized["readiness_timeout_seconds"], 30)
        self.assertEqual(normalized["reliability_threshold"], 0.99)
        self.assertEqual(
            normalized["report_path"],
            "benchmarks/load/reports/starter-smoke/report.json",
        )

    def test_defaults_are_applied_for_optional_fields(self) -> None:
        with self.subTest("minimal config"):
            tmp_dir = self.enterContext(_temporary_directory())
            path = tmp_dir / "minimal.json"
            path.write_text(
                json.dumps(
                    {
                        "scenario_id": "SCENARIO-02",
                        "name": "Defaults",
                        "workload_id": "symbol_lookup_smoke",
                        "fixture_id": "starter-project",
                        "scenario_mix": [{"operation": "search_symbols", "weight": 1}],
                    }
                ),
                encoding="utf-8",
            )

            normalized = load_scenario(path)

        self.assertEqual(normalized["agent_count"], 1)
        self.assertEqual(normalized["duration_seconds"], 60)
        self.assertEqual(normalized["request_pacing"]["requests_per_second"], 1.0)
        self.assertEqual(normalized["server_options"]["port"], 3000)

    def test_invalid_values_return_actionable_errors(self) -> None:
        cases = [
            ({"fixture_id": "missing-fixture"}, "unknown fixture_id"),
            ({"scenario_id": "SCENARIO-99"}, "unknown scenario_id"),
            ({"duration_seconds": -1}, "duration_seconds must be a positive integer"),
            ({"agent_count": 0}, "agent_count must be a positive integer"),
            (
                {"scenario_mix": [{"operation": "unknown_operation", "weight": 1}]},
                "scenario_mix[0].operation is unknown",
            ),
            ({"workload_id": "unknown_workload"}, "unknown workload_id"),
            ({"cpu_profile_label": "kernel-trace"}, "invalid cpu_profile_label"),
            (
                {"reliability_threshold": 1.5},
                "reliability_threshold must be greater than 0",
            ),
        ]

        for updates, message in cases:
            with self.subTest(message=message):
                tmp_dir = self.enterContext(_temporary_directory())
                path = write_scenario(tmp_dir, updates)

                with self.assertRaises(ConfigValidationError) as exc:
                    load_scenario(path)

                self.assertIn(message, str(exc.exception))

    def test_missing_required_fields_are_rejected(self) -> None:
        tmp_dir = self.enterContext(_temporary_directory())
        path = tmp_dir / "missing.json"
        path.write_text("{}", encoding="utf-8")

        with self.assertRaises(ConfigValidationError) as exc:
            load_scenario(path)

        self.assertIn("missing required field: scenario_id", str(exc.exception))
        self.assertIn("missing required field: scenario_mix", str(exc.exception))

    def test_command_line_overrides_are_validated(self) -> None:
        result = subprocess.run(
            [
                sys.executable,
                "-m",
                "benchmarks.load.runner",
                "--validate-only",
                "--agents",
                "4",
                "--request-rate",
                "2.25",
                "--output-dir",
                "benchmarks/load/reports/override-run",
                str(STARTER),
            ],
            cwd=ROOT,
            check=True,
            text=True,
            capture_output=True,
        )

        normalized = json.loads(result.stdout)

        self.assertEqual(normalized["agent_count"], 4)
        self.assertEqual(normalized["request_pacing"]["requests_per_second"], 2.25)
        self.assertEqual(
            normalized["report_path"],
            "benchmarks/load/reports/override-run/report.json",
        )

    def test_command_line_overrides_cannot_create_invalid_metadata(self) -> None:
        result = subprocess.run(
            [
                sys.executable,
                "-m",
                "benchmarks.load.runner",
                "--validate-only",
                "--request-rate",
                "0",
                str(STARTER),
            ],
            cwd=ROOT,
            text=True,
            capture_output=True,
        )

        self.assertEqual(result.returncode, 2)
        self.assertIn(
            "request_pacing.requests_per_second must be a positive number",
            result.stderr,
        )

    def test_programmatic_overrides_are_validated(self) -> None:
        with self.assertRaises(ConfigValidationError) as exc:
            load_scenario(STARTER, ScenarioOverrides(cpu_profile_label="impossible"))

        self.assertIn("invalid cpu_profile_label", str(exc.exception))

    def test_fixture_scenario_metadata_documents_stable_fixture_ids(self) -> None:
        self.assertEqual(
            list_fixture_ids(),
            ("mixed-language-project", "starter-project"),
        )

        fixture_scenario = load_scenario(FIXTURE_REPOS)
        watcher_scenario = load_scenario(WATCHER_MUTATION)

        self.assertEqual(fixture_scenario["scenario_id"], "SCENARIO-03")
        self.assertEqual(fixture_scenario["fixture_id"], "mixed-language-project")
        self.assertEqual(fixture_scenario["fixture"]["size_class"], "larger")
        self.assertIn("rust", fixture_scenario["fixture"]["languages"])
        self.assertIn("typescript", fixture_scenario["fixture"]["languages"])
        self.assertIn("grep", fixture_scenario["fixture"]["known_targets"])

        self.assertEqual(watcher_scenario["scenario_id"], "SCENARIO-04")
        self.assertEqual(watcher_scenario["fixture_id"], "starter-project")
        self.assertEqual(watcher_scenario["agent_count"], 4)
        self.assertIn("watcher_mutation", watcher_scenario["fixture"]["known_targets"])

    def test_fixture_metadata_resolves_known_targets_to_existing_files(self) -> None:
        for fixture_id in list_fixture_ids():
            with self.subTest(fixture_id=fixture_id):
                metadata = resolve_fixture(fixture_id)

                self.assertTrue(metadata.source_path.is_dir())
                self.assertTrue(metadata.known_targets["structure"]["path"])
                self.assertTrue(metadata.known_targets["search_symbols"]["symbol"])
                self.assertTrue(metadata.known_targets["read_implementation"]["path"])
                self.assertTrue(metadata.known_targets["list_callers"]["caller"])
                self.assertTrue(metadata.known_targets["list_tests"]["path"])
                self.assertTrue(metadata.known_targets["grep"]["pattern"])
                self.assertTrue(metadata.known_targets["watcher_mutation"]["replace"])

    def test_fixture_preparation_is_deterministic_and_mutates_only_working_copy(self) -> None:
        tmp_dir = self.enterContext(_temporary_directory())
        metadata = resolve_fixture("starter-project")
        original_digest = fixture_digest(metadata.source_path)

        first = prepare_fixture_working_copy("starter-project", tmp_dir, "first")
        second = prepare_fixture_working_copy("starter-project", tmp_dir, "second")

        self.assertEqual(fixture_digest(first.worktree_path), original_digest)
        self.assertEqual(fixture_digest(second.worktree_path), original_digest)
        self.assertEqual(
            first.fixture.known_targets["watcher_mutation"],
            second.fixture.known_targets["watcher_mutation"],
        )

        touched = touch_known_file(first)
        appended = append_to_known_file(first, "\nmutation note\n")
        replaced = replace_in_known_file(
            first,
            "starter-original-sentinel",
            "starter-mutated-sentinel",
        )

        self.assertTrue(touched.is_relative_to(first.worktree_path))
        self.assertTrue(appended.is_relative_to(first.worktree_path))
        self.assertTrue(replaced.is_relative_to(first.worktree_path))
        self.assertNotEqual(fixture_digest(first.worktree_path), original_digest)
        self.assertEqual(fixture_digest(second.worktree_path), original_digest)
        self.assertEqual(fixture_digest(metadata.source_path), original_digest)

    def test_mutation_helper_rejects_missing_or_source_fixture_targets(self) -> None:
        tmp_dir = self.enterContext(_temporary_directory())
        metadata = resolve_fixture("starter-project")
        prepared = prepare_fixture_working_copy("starter-project", tmp_dir, "mutable")
        source_like = type(prepared)(
            fixture=metadata,
            worktree_path=metadata.source_path,
        )

        missing = prepared.path_for_target("touch")
        missing.unlink()

        with self.assertRaises(FixtureError) as missing_exc:
            touch_known_file(prepared)
        self.assertIn("cannot touch missing fixture file", str(missing_exc.exception))

        with self.assertRaises(FixtureError) as source_exc:
            append_to_known_file(source_like, "must not write")
        self.assertIn(
            "refusing to mutate checked-in fixture source",
            str(source_exc.exception),
        )
        self.assertNotIn(
            "must not write",
            (metadata.source_path / "README.md").read_text(),
        )


class _temporary_directory:
    def __enter__(self) -> Path:
        self._manager = tempfile.TemporaryDirectory()
        return Path(self._manager.__enter__())

    def __exit__(self, exc_type, exc, traceback) -> None:
        self._manager.__exit__(exc_type, exc, traceback)
