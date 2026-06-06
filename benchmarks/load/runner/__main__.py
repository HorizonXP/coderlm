"""Validate, run, and report load harness scenarios."""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import sys
import time
from pathlib import Path
from typing import Any

from benchmarks.load.config import ConfigValidationError, ScenarioOverrides, load_scenario
from benchmarks.load.runner.client import CodeRLMClient, CodeRLMClientError
from benchmarks.load.runner.events import UNSUPPORTED, classify_error
from benchmarks.load.runner.executor import create_agent_sessions, run_agent_operations


class HarnessError(RuntimeError):
    """Raised when the harness cannot complete the configured workflow."""


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python -m benchmarks.load.runner",
        description="Validate a load harness scenario and print normalized metadata.",
    )
    parser.add_argument(
        "scenario",
        type=Path,
        help="Path to a JSON scenario config, such as benchmarks/load/scenarios/starter.json.",
    )
    parser.add_argument(
        "--validate-only",
        action="store_true",
        help="Validate and normalize config without starting any load workload.",
    )
    parser.add_argument("--agents", type=int, dest="agent_count")
    parser.add_argument("--duration-seconds", type=int)
    parser.add_argument("--output-dir")
    parser.add_argument("--cpu-profile-label")
    parser.add_argument("--readiness-timeout-seconds", type=int)
    parser.add_argument("--reliability-threshold", type=float)
    parser.add_argument("--request-rate", type=float, dest="requests_per_second")
    parser.add_argument("--server-host")
    parser.add_argument("--server-port", type=int)
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    overrides = ScenarioOverrides(
        agent_count=args.agent_count,
        duration_seconds=args.duration_seconds,
        output_dir=args.output_dir,
        cpu_profile_label=args.cpu_profile_label,
        readiness_timeout_seconds=args.readiness_timeout_seconds,
        reliability_threshold=args.reliability_threshold,
        requests_per_second=args.requests_per_second,
        server_host=args.server_host,
        server_port=args.server_port,
    )

    try:
        normalized = load_scenario(args.scenario, overrides)
    except ConfigValidationError as exc:
        for error in exc.errors:
            print(f"error: {error}", file=sys.stderr)
        return 2

    if args.validate_only:
        print(json.dumps(normalized, indent=2, sort_keys=True))
        return 0

    try:
        report = run_scenario(normalized)
    except HarnessError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    except CodeRLMClientError as exc:
        print(f"error: {classify_error(exc)}: {exc}", file=sys.stderr)
        return 1

    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


def run_scenario(config: dict[str, Any]) -> dict[str, Any]:
    """Run the configured smoke workflow against a CodeRLM server."""

    base_url = _server_base_url(config)
    timeout = config["readiness_timeout_seconds"]
    client = CodeRLMClient(base_url)
    health = _poll_health(client, timeout)
    agent_sessions = create_agent_sessions(client, config)
    operations = _run_agent_batches(client, config, agent_sessions)

    total = len(operations)
    failed = [
        operation
        for operation in operations
        if not operation["ok"] and operation["status"] != UNSUPPORTED
    ]
    unsupported = [operation for operation in operations if operation["status"] == UNSUPPORTED]
    error_rate = len(failed) / total if total else 0.0
    allowed_error_rate = 1.0 - config["reliability_threshold"]

    report = {
        "ok": not failed and error_rate <= allowed_error_rate,
        "scenario": config,
        "server": {
            "base_url": base_url,
            "health": health,
            "readiness": [agent_session.readiness for agent_session in agent_sessions],
            "sessions": [agent_session.session.raw for agent_session in agent_sessions],
            "session": agent_sessions[0].session.raw if agent_sessions else None,
        },
        "operations": operations,
        "summary": {
            "total_requests": total,
            "request_errors": len(failed),
            "unsupported_requests": len(unsupported),
            "error_rate": error_rate,
            "allowed_error_rate": allowed_error_rate,
        },
        "runtime_environment": _runtime_environment(config),
    }
    _write_report(config["report_path"], report)

    if failed:
        names = ", ".join(operation["operation"] for operation in failed)
        raise HarnessError(f"core API workflow failed for operation(s): {names}")
    if error_rate > allowed_error_rate:
        raise HarnessError(
            "request error rate exceeded reliability threshold: "
            f"{error_rate:.4f} > {allowed_error_rate:.4f}"
        )
    return report


def _run_agent_batches(
    client: CodeRLMClient,
    config: dict[str, Any],
    agent_sessions: list,
) -> list[dict[str, Any]]:
    if len(agent_sessions) <= 1:
        return [
            event
            for agent_session in agent_sessions
            for event in run_agent_operations(client, config, agent_session)
        ]

    operations: list[dict[str, Any]] = []
    max_workers = min(len(agent_sessions), config["agent_count"])
    with concurrent.futures.ThreadPoolExecutor(max_workers=max_workers) as executor:
        futures = [
            executor.submit(run_agent_operations, client, config, agent_session)
            for agent_session in agent_sessions
        ]
        for future in concurrent.futures.as_completed(futures):
            operations.extend(future.result())
    return operations


def _server_base_url(config: dict[str, Any]) -> str:
    server_options = config["server_options"]
    return os.environ.get(
        "CODERLM_HARNESS_SERVER_BASE_URL",
        f"http://{server_options['host']}:{server_options['port']}",
    ).rstrip("/")


def _poll_health(client: CodeRLMClient, timeout_seconds: int) -> dict[str, Any]:
    deadline = time.monotonic() + timeout_seconds
    last_error = "health endpoint was not attempted"
    while time.monotonic() < deadline:
        try:
            payload = client.health()
            if payload.get("status") == "ok":
                return payload
            last_error = f"unexpected health payload: {payload!r}"
        except CodeRLMClientError as exc:
            last_error = str(exc)
        time.sleep(0.25)
    raise HarnessError(
        f"health endpoint unavailable after {timeout_seconds}s at "
        f"{client.base_url}/api/v1/health: {last_error}"
    )


def _runtime_environment(config: dict[str, Any]) -> dict[str, Any]:
    return {
        "cpu_profile": config["cpu_profile"],
        "raw_environment": {
            "CODERLM_CPU_PROFILE_LABEL": os.environ.get("CODERLM_CPU_PROFILE_LABEL"),
            "COMPOSE_PROFILES": os.environ.get("COMPOSE_PROFILES"),
            "HOSTNAME": os.environ.get("HOSTNAME"),
        },
    }


def _write_report(path: str, report: dict[str, Any]) -> None:
    report_path = Path(path)
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(report, indent=2, sort_keys=True), encoding="utf-8")


if __name__ == "__main__":
    raise SystemExit(main())
