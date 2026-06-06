from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest import mock
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib import parse

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
from benchmarks.load.runner.__main__ import HarnessError, run_scenario
from benchmarks.load.runner.__main__ import _run_agent_batches
from benchmarks.load.runner.client import CodeRLMClient, CodeRLMSession, ReadinessTimeout
from benchmarks.load.runner.events import (
    OperationContext,
    UnsupportedOperationTarget,
    error_event,
    success_event,
)
from benchmarks.load.runner.executor import AgentSession, run_agent_operations, run_operation


ROOT = Path(__file__).resolve().parents[1]
STARTER = ROOT / "benchmarks/load/scenarios/starter.json"
FIXTURE_REPOS = ROOT / "benchmarks/load/scenarios/fixture-repos.json"
WATCHER_MUTATION = ROOT / "benchmarks/load/scenarios/watcher-mutation.json"
COMPOSE_2CPU = ROOT / "benchmarks/load/scenarios/compose-2cpu-smoke.json"
COMPOSE_4CPU = ROOT / "benchmarks/load/scenarios/compose-4cpu-smoke.json"
COMPOSE_HEALTH_TIMEOUT = ROOT / "benchmarks/load/scenarios/compose-health-timeout.json"


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
                "bind": "127.0.0.1",
                "port": 3000,
                "max_file_size": 1_000_000,
                "max_projects": 5,
                "watcher_enabled": True,
                "log_level": "info",
                "reuse_existing": False,
            },
        )
        self.assertEqual(normalized["cpu_profile_label"], "none")
        self.assertEqual(normalized["cpu_profile"]["label"], "none")
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
        self.assertTrue(normalized["server_options"]["watcher_enabled"])

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
            (
                {"server_options": {"port": 70000}},
                "server_options.port must be an integer between 1 and 65535",
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

    def test_compose_cpu_profiles_normalize_metadata(self) -> None:
        two_cpu = load_scenario(COMPOSE_2CPU)
        four_cpu = load_scenario(COMPOSE_4CPU)

        self.assertEqual(two_cpu["scenario_id"], "SCENARIO-05")
        self.assertEqual(two_cpu["cpu_profile"]["label"], "2cpu")
        self.assertEqual(two_cpu["cpu_profile"]["compose_profile"], "cpu-2")
        self.assertEqual(two_cpu["cpu_profile"]["cpu_limit"], 2.0)
        self.assertFalse(two_cpu["server_options"]["watcher_enabled"])

        self.assertEqual(four_cpu["scenario_id"], "SCENARIO-05")
        self.assertEqual(four_cpu["cpu_profile"]["label"], "4cpu")
        self.assertEqual(four_cpu["cpu_profile"]["compose_profile"], "cpu-4")
        self.assertEqual(four_cpu["cpu_profile"]["cpu_limit"], 4.0)

        comparable_two = dict(two_cpu)
        comparable_four = dict(four_cpu)
        for key in ("name", "server_options", "cpu_profile_label", "output_dir", "report_path", "cpu_profile"):
            comparable_two.pop(key)
            comparable_four.pop(key)
        self.assertEqual(comparable_two, comparable_four)

    def test_runner_writes_report_for_core_api_workflow(self) -> None:
        tmp_dir = self.enterContext(_temporary_directory())
        server = self.enterContext(_fake_coderlm_server())
        scenario_path = write_scenario(
            tmp_dir,
            {
                "scenario_id": "SCENARIO-05",
                "workload_id": "core_api_smoke",
                "scenario_mix": [
                    {"operation": "search_symbols", "weight": 1},
                    {"operation": "read_implementation", "weight": 1},
                    {"operation": "grep", "weight": 1},
                ],
                "server_options": {
                    "host": "127.0.0.1",
                    "port": server.port,
                },
                "cpu_profile_label": "2cpu",
                "output_dir": str(tmp_dir / "reports"),
                "readiness_timeout_seconds": 2,
                "duration_seconds": 1,
                "request_pacing": {
                    "mode": "fixed_rps",
                    "requests_per_second": 3.0,
                },
            },
        )
        config = load_scenario(scenario_path)

        report = run_scenario(config)

        self.assertTrue(report["ok"])
        self.assertEqual(report["summary"]["request_errors"], 0)
        self.assertEqual(report["scenario"]["cpu_profile"]["label"], "2cpu")
        written = json.loads((tmp_dir / "reports/report.json").read_text(encoding="utf-8"))
        self.assertEqual(written["server"]["session"]["session_id"], "session-1")
        self.assertEqual(len(written["server"]["sessions"]), 2)
        self.assertEqual(written["operations"][0]["scenario_id"], "SCENARIO-05")
        self.assertEqual(written["operations"][0]["fixture_id"], "starter-project")
        self.assertEqual(written["operations"][0]["status"], "success")

    def test_executor_paces_weighted_operations_without_mutating_mix(self) -> None:
        tmp_dir = self.enterContext(_temporary_directory())
        prepared = prepare_fixture_working_copy("starter-project", tmp_dir, "paced")
        session = CodeRLMSession(
            session_id="session-paced",
            project_root=str(prepared.worktree_path),
            raw={"session_id": "session-paced", "project": str(prepared.worktree_path)},
        )
        agent_session = AgentSession(
            agent_id="agent-1",
            project_id="project-1",
            prepared_fixture=prepared,
            session=session,
            readiness={"ready": True},
        )
        config = load_scenario(
            write_scenario(
                tmp_dir,
                {
                    "scenario_id": "SCENARIO-08",
                    "scenario_mix": [
                        {"operation": "structure", "weight": 2},
                        {"operation": "grep", "weight": 1},
                    ],
                    "duration_seconds": 3,
                    "request_pacing": {
                        "mode": "fixed_rps",
                        "requests_per_second": 2.0,
                        "think_time_seconds": 0.75,
                    },
                    "output_dir": str(tmp_dir / "reports"),
                },
            )
        )
        clock = _FakeClock()
        observed_starts: list[float] = []

        def fake_run_operation(*args: object, **kwargs: object) -> dict:
            observed_starts.append(round(clock.now, 1))
            return {"count": 1}

        with mock.patch(
            "benchmarks.load.runner.executor.run_operation",
            side_effect=fake_run_operation,
        ):
            events = run_agent_operations(
                CodeRLMClient("http://127.0.0.1:1"),
                config,
                agent_session,
                monotonic=clock.monotonic,
                sleep=clock.sleep,
            )

        self.assertEqual(
            [event["operation"] for event in events],
            ["structure", "structure", "grep", "structure"],
        )
        self.assertEqual([event["sequence"] for event in events], list(range(4)))
        self.assertEqual(observed_starts, [0.0, 0.8, 1.5, 2.2])

    def test_runner_fans_out_agents_across_multiple_projects(self) -> None:
        tmp_dir = self.enterContext(_temporary_directory())
        server = self.enterContext(_fake_coderlm_server())
        config = load_scenario(
            write_scenario(
                tmp_dir,
                {
                    "scenario_id": "SCENARIO-08",
                    "workload_id": "core_api_smoke",
                    "agent_count": 3,
                    "project_count": 2,
                    "duration_seconds": 1,
                    "request_pacing": {
                        "mode": "fixed_rps",
                        "requests_per_second": 2.0,
                    },
                    "scenario_mix": [{"operation": "structure", "weight": 1}],
                    "server_options": {"host": "127.0.0.1", "port": server.port},
                    "output_dir": str(tmp_dir / "reports"),
                    "readiness_timeout_seconds": 2,
                },
            )
        )

        report = run_scenario(config)

        self.assertTrue(report["ok"])
        self.assertEqual(report["summary"]["total_requests"], 6)
        self.assertEqual(
            {operation["agent_id"] for operation in report["operations"]},
            {"agent-1", "agent-2", "agent-3"},
        )
        self.assertEqual(
            {operation["project_id"] for operation in report["operations"]},
            {"project-1", "project-2"},
        )

    def test_worker_exception_is_reported_as_partial_event_and_cancels_batch(self) -> None:
        tmp_dir = self.enterContext(_temporary_directory())
        prepared = prepare_fixture_working_copy("starter-project", tmp_dir, "cancel")
        sessions = [
            AgentSession(
                agent_id=f"agent-{index}",
                project_id="project-1",
                prepared_fixture=prepared,
                session=CodeRLMSession(
                    session_id=f"session-{index}",
                    project_root=str(prepared.worktree_path),
                    raw={"session_id": f"session-{index}", "project": str(prepared.worktree_path)},
                ),
                readiness={"ready": True},
            )
            for index in (1, 2)
        ]
        config = load_scenario(
            write_scenario(
                tmp_dir,
                {
                    "scenario_id": "SCENARIO-08",
                    "agent_count": 2,
                    "duration_seconds": 1,
                    "scenario_mix": [{"operation": "structure", "weight": 1}],
                    "output_dir": str(tmp_dir / "reports"),
                },
            )
        )

        def fake_run_agent_operations(*args: object, **kwargs: object) -> list[dict]:
            cancel_event = kwargs["cancel_event"]
            agent_session = args[2]
            if agent_session.agent_id == "agent-1":
                raise RuntimeError("worker exploded")
            while not cancel_event.is_set():
                time.sleep(0.01)
            return []

        with mock.patch(
            "benchmarks.load.runner.__main__.run_agent_operations",
            side_effect=fake_run_agent_operations,
        ):
            events = _run_agent_batches(CodeRLMClient("http://127.0.0.1:1"), config, sessions)

        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]["operation"], "worker")
        self.assertEqual(events[0]["error_classification"], "runner_error")

    def test_expected_error_classification_does_not_fail_report(self) -> None:
        tmp_dir = self.enterContext(_temporary_directory())
        server = self.enterContext(_fake_coderlm_server(gone_paths={"/api/v1/structure"}))
        config = load_scenario(
            write_scenario(
                tmp_dir,
                {
                    "scenario_id": "SCENARIO-09",
                    "workload_id": "core_api_smoke",
                    "agent_count": 1,
                    "duration_seconds": 1,
                    "scenario_mix": [
                        {
                            "operation": "structure",
                            "weight": 1,
                            "expected_error_classifications": ["project_gone"],
                        }
                    ],
                    "server_options": {"host": "127.0.0.1", "port": server.port},
                    "output_dir": str(tmp_dir / "reports"),
                    "readiness_timeout_seconds": 2,
                },
            )
        )

        report = run_scenario(config)

        self.assertTrue(report["ok"])
        self.assertEqual(report["summary"]["expected_errors"], 1)
        self.assertTrue(report["operations"][0]["expected_error"])

    def test_client_constructs_session_header_and_query_request(self) -> None:
        server = self.enterContext(_fake_coderlm_server())
        client = CodeRLMClient(f"http://127.0.0.1:{server.port}")

        payload = client.get(
            "/api/v1/symbols/search",
            session_id="session-for-header",
            query={"q": "compute_total", "limit": "20"},
        )

        self.assertEqual(payload["count"], 1)
        self.assertEqual(_FakeCoderlmHandler.last_session_header, "session-for-header")
        self.assertEqual(_FakeCoderlmHandler.last_path, "/api/v1/symbols/search")
        self.assertEqual(
            _FakeCoderlmHandler.last_query,
            {"q": ["compute_total"], "limit": ["20"]},
        )

    def test_client_rejects_relative_paths_and_non_object_json(self) -> None:
        server = self.enterContext(_fake_coderlm_server(non_object_path="/api/v1/health"))
        client = CodeRLMClient(f"http://127.0.0.1:{server.port}")

        with self.assertRaisesRegex(Exception, "must start with '/'"):
            client.get("api/v1/health")
        with self.assertRaisesRegex(Exception, "non-object JSON"):
            client.health()

    def test_readiness_timeout_is_explicitly_classified(self) -> None:
        server = self.enterContext(_fake_coderlm_server(ready=False))
        client = CodeRLMClient(f"http://127.0.0.1:{server.port}")
        session = CodeRLMSession(
            session_id="session-never-ready",
            project_root="/fixture",
            raw={"session_id": "session-never-ready", "project": "/fixture"},
        )

        with self.assertRaises(ReadinessTimeout) as exc:
            client.wait_for_ready(session, 1, poll_interval_seconds=0.01)

        context = OperationContext(
            scenario_id="SCENARIO-07",
            fixture_id="starter-project",
            agent_id="agent-1",
            project_id="project-1",
            session_id=session.session_id,
            project_root=session.project_root,
        )
        event = error_event("readiness", context, 1.0, exc.exception)

        self.assertEqual(event["status"], "error")
        self.assertEqual(event["error_classification"], "readiness_timeout")

    def test_unsupported_operation_target_normalizes_to_event(self) -> None:
        context = OperationContext(
            scenario_id="SCENARIO-07",
            fixture_id="starter-project",
            agent_id="agent-1",
            project_id="project-1",
            session_id="session-1",
            project_root="/fixture",
        )
        exc = UnsupportedOperationTarget("missing target")

        event = error_event("list_tests", context, 1.0, exc)

        self.assertFalse(event["ok"])
        self.assertEqual(event["status"], "unsupported")
        self.assertEqual(
            event["error_classification"],
            "unsupported_operation_target",
        )

    def test_runner_records_unsupported_targets_without_failing_report(self) -> None:
        tmp_dir = self.enterContext(_temporary_directory())
        server = self.enterContext(_fake_coderlm_server())
        scenario_path = write_scenario(
            tmp_dir,
            {
                "scenario_id": "SCENARIO-07",
                "workload_id": "core_api_smoke",
                "scenario_mix": [{"operation": "list_tests", "weight": 1}],
                "server_options": {"host": "127.0.0.1", "port": server.port},
                "output_dir": str(tmp_dir / "reports"),
                "readiness_timeout_seconds": 2,
            },
        )
        config = load_scenario(scenario_path)

        unsupported_event = {
            "operation": "list_tests",
            "scenario_id": "SCENARIO-07",
            "fixture_id": "starter-project",
            "agent_id": "agent-1",
            "project_id": "project-1",
            "session_id": "session-1",
            "project_root": "/fixture",
            "started_at_monotonic": 1.0,
            "ended_at_monotonic": 1.1,
            "elapsed_seconds": 0.1,
            "status": "unsupported",
            "ok": False,
            "error_classification": "unsupported_operation_target",
            "error": "missing target",
            "response_summary": None,
        }
        with (
            mock.patch("benchmarks.load.runner.__main__.create_agent_sessions", return_value=[]),
            mock.patch(
                "benchmarks.load.runner.__main__._run_agent_batches",
                return_value=[unsupported_event],
            ),
        ):
            report = run_scenario(config)

        self.assertTrue(report["ok"])
        self.assertEqual(report["summary"]["request_errors"], 0)
        self.assertEqual(report["summary"]["unsupported_requests"], 1)
        self.assertEqual(report["operations"][0]["status"], "unsupported")

    def test_success_event_normalizes_identity_and_response_summary(self) -> None:
        context = OperationContext(
            scenario_id="SCENARIO-08",
            fixture_id="starter-project",
            agent_id="agent-2",
            project_id="project-1",
            session_id="session-2",
            project_root="/fixture",
        )

        event = success_event("grep", context, 1.0, {"total_matches": 3})

        self.assertTrue(event["ok"])
        self.assertEqual(event["status"], "success")
        self.assertEqual(event["scenario_id"], "SCENARIO-08")
        self.assertEqual(event["agent_id"], "agent-2")
        self.assertEqual(event["response_summary"], {"total_matches": 3})

    def test_success_event_ignores_malformed_count_summary(self) -> None:
        context = OperationContext(
            scenario_id="SCENARIO-08",
            fixture_id="starter-project",
            agent_id="agent-2",
            project_id="project-1",
            session_id="session-2",
            project_root="/fixture",
        )

        event = success_event("search_symbols", context, 1.0, {"count": "one", "symbols": []})

        self.assertEqual(event["response_summary"], {"keys": ["count", "symbols"]})

    def test_missing_fixture_target_is_classified_without_http_request(self) -> None:
        client = CodeRLMClient("http://127.0.0.1:1")
        fixture = {
            "known_targets": {
                "structure": {"path": "src/starter/math_ops.py"},
            }
        }

        with self.assertRaises(UnsupportedOperationTarget):
            run_operation(client, "session-1", fixture, "list_tests")

    def test_health_timeout_exits_nonzero_with_actionable_error(self) -> None:
        config = load_scenario(COMPOSE_HEALTH_TIMEOUT)

        with self.assertRaises(HarnessError) as exc:
            run_scenario(config)

        self.assertIn("health endpoint unavailable", str(exc.exception))

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


class _FakeClock:
    def __init__(self) -> None:
        self.now = 0.0

    def monotonic(self) -> float:
        return self.now

    def sleep(self, seconds: float) -> None:
        self.now += seconds


class _fake_coderlm_server:
    def __init__(
        self,
        *,
        ready: bool = True,
        non_object_path: str | None = None,
        gone_paths: set[str] | None = None,
    ) -> None:
        self.ready = ready
        self.non_object_path = non_object_path
        self.gone_paths = gone_paths or set()

    def __enter__(self) -> "_fake_coderlm_server":
        _FakeCoderlmHandler.ready = self.ready
        _FakeCoderlmHandler.non_object_path = self.non_object_path
        _FakeCoderlmHandler.gone_paths = self.gone_paths
        _FakeCoderlmHandler.last_path = ""
        _FakeCoderlmHandler.last_query = {}
        _FakeCoderlmHandler.last_session_header = None
        _FakeCoderlmHandler.project_paths = []
        _FakeCoderlmHandler.session_count = 0
        self._server = ThreadingHTTPServer(("127.0.0.1", 0), _FakeCoderlmHandler)
        self.port = self._server.server_address[1]
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()
        return self

    def __exit__(self, exc_type, exc, traceback) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=2)


class _FakeCoderlmHandler(BaseHTTPRequestHandler):
    project_paths: list[str] = []
    session_count = 0
    ready = True
    non_object_path: str | None = None
    gone_paths: set[str] = set()
    last_path = ""
    last_query: dict[str, list[str]] = {}
    last_session_header: str | None = None

    def log_message(self, format: str, *args: object) -> None:
        return

    def do_GET(self) -> None:
        parsed = parse.urlparse(self.path)
        query = parse.parse_qs(parsed.query)
        type(self).last_path = parsed.path
        type(self).last_query = query
        type(self).last_session_header = self.headers.get("X-Session-Id")
        if parsed.path in self.gone_paths:
            self._send({"error": "project gone"}, status=410)
        elif parsed.path == self.non_object_path:
            self._send(["not", "an", "object"])
        elif parsed.path == "/api/v1/health":
            self._send({"status": "ok", "projects": 1, "active_sessions": 0})
        elif parsed.path == "/api/v1/roots":
            self._send(
                {
                    "roots": [
                        {
                            "path": project_path,
                            "ready": self.ready,
                            "readiness": "ready" if self.ready else "indexing",
                        }
                        for project_path in self.project_paths
                    ],
                    "count": len(self.project_paths),
                }
            )
        elif parsed.path == "/api/v1/structure":
            self._send({"tree": "src/\n", "file_count": 1})
        elif parsed.path == "/api/v1/symbols/search":
            self._send({"symbols": [{"name": query["q"][0]}], "count": 1})
        elif parsed.path == "/api/v1/symbols/implementation":
            self._send({"source": "def compute_total():\n    return 1\n"})
        elif parsed.path == "/api/v1/grep":
            self._send({"matches": [{"line": 1}], "total_matches": 1})
        elif parsed.path == "/api/v1/peek":
            self._send({"lines": ["fixture line"]})
        else:
            self.send_error(404)

    def do_POST(self) -> None:
        if self.path != "/api/v1/sessions":
            self.send_error(404)
            return
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length).decode("utf-8"))
        if body["cwd"] not in type(self).project_paths:
            type(self).project_paths.append(body["cwd"])
        type(self).session_count += 1
        session_id = f"session-{type(self).session_count}"
        self._send(
            {
                "session_id": session_id,
                "created_at": "2026-06-02T00:00:00Z",
                "project": body["cwd"],
                "structure": {},
            }
        )

    def _send(self, payload: object, *, status: int = 200) -> None:
        content = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(content)))
        self.end_headers()
        self.wfile.write(content)
