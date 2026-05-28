from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from benchmarks.load.config import ConfigValidationError, ScenarioOverrides, load_scenario


ROOT = Path(__file__).resolve().parents[1]
STARTER = ROOT / "benchmarks/load/scenarios/starter.json"


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


class _temporary_directory:
    def __enter__(self) -> Path:
        self._manager = tempfile.TemporaryDirectory()
        return Path(self._manager.__enter__())

    def __exit__(self, exc_type, exc, traceback) -> None:
        self._manager.__exit__(exc_type, exc, traceback)
