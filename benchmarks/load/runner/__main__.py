"""Validate, run, and report load harness scenarios."""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path
from typing import Any
from urllib import error, parse, request

from benchmarks.load.config import ConfigValidationError, ScenarioOverrides, load_scenario


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

    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


def run_scenario(config: dict[str, Any]) -> dict[str, Any]:
    """Run the configured smoke workflow against a CodeRLM server."""

    base_url = _server_base_url(config)
    timeout = config["readiness_timeout_seconds"]
    health = _poll_health(base_url, timeout)
    session = _create_session(base_url, config["fixture"]["source_path"])
    session_id = session["session_id"]
    readiness = _poll_readiness(base_url, session_id, session["project"], timeout)
    operations = _run_operations(base_url, session_id, config)

    total = len(operations)
    failed = [operation for operation in operations if not operation["ok"]]
    error_rate = len(failed) / total if total else 0.0
    allowed_error_rate = 1.0 - config["reliability_threshold"]

    report = {
        "ok": not failed and error_rate <= allowed_error_rate,
        "scenario": config,
        "server": {
            "base_url": base_url,
            "health": health,
            "readiness": readiness,
            "session": session,
        },
        "operations": operations,
        "summary": {
            "total_requests": total,
            "request_errors": len(failed),
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


def _server_base_url(config: dict[str, Any]) -> str:
    server_options = config["server_options"]
    return os.environ.get(
        "CODERLM_HARNESS_SERVER_BASE_URL",
        f"http://{server_options['host']}:{server_options['port']}",
    ).rstrip("/")


def _poll_health(base_url: str, timeout_seconds: int) -> dict[str, Any]:
    deadline = time.monotonic() + timeout_seconds
    last_error = "health endpoint was not attempted"
    while time.monotonic() < deadline:
        try:
            payload = _json_request("GET", f"{base_url}/api/v1/health")
            if payload.get("status") == "ok":
                return payload
            last_error = f"unexpected health payload: {payload!r}"
        except HarnessError as exc:
            last_error = str(exc)
        time.sleep(0.25)
    raise HarnessError(
        f"health endpoint unavailable after {timeout_seconds}s at "
        f"{base_url}/api/v1/health: {last_error}"
    )


def _create_session(base_url: str, fixture_path: str) -> dict[str, Any]:
    payload = _json_request(
        "POST",
        f"{base_url}/api/v1/sessions",
        body={"cwd": fixture_path},
    )
    if not payload.get("session_id"):
        raise HarnessError(f"session creation returned no session_id: {payload!r}")
    return payload


def _poll_readiness(
    base_url: str,
    session_id: str,
    project_path: str,
    timeout_seconds: int,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout_seconds
    last_payload: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        payload = _json_request("GET", f"{base_url}/api/v1/roots")
        last_payload = payload
        roots = payload.get("roots", [])
        for root in roots:
            if root.get("path") == project_path and root.get("ready") is True:
                return root
        time.sleep(0.25)
    raise HarnessError(
        "project did not become ready after "
        f"{timeout_seconds}s for session {session_id}; last roots payload: {last_payload!r}"
    )


def _run_operations(
    base_url: str,
    session_id: str,
    config: dict[str, Any],
) -> list[dict[str, Any]]:
    operations: list[dict[str, Any]] = []
    for item in config["scenario_mix"]:
        operation = item["operation"]
        started = time.monotonic()
        try:
            response = _run_operation(base_url, session_id, config["fixture"], operation)
            operations.append(
                {
                    "operation": operation,
                    "ok": True,
                    "elapsed_seconds": round(time.monotonic() - started, 6),
                    "response_summary": _response_summary(operation, response),
                }
            )
        except HarnessError as exc:
            operations.append(
                {
                    "operation": operation,
                    "ok": False,
                    "elapsed_seconds": round(time.monotonic() - started, 6),
                    "error": str(exc),
                }
            )
    return operations


def _run_operation(
    base_url: str,
    session_id: str,
    fixture: dict[str, Any],
    operation: str,
) -> dict[str, Any]:
    targets = fixture["known_targets"]
    headers = {"X-Session-Id": session_id}

    if operation == "search_symbols":
        target = targets["search_symbols"]
        return _json_request(
            "GET",
            _url(base_url, "/api/v1/symbols/search", q=target["query"], limit="20"),
            headers=headers,
        )
    if operation == "read_implementation":
        target = targets["read_implementation"]
        return _json_request(
            "GET",
            _url(
                base_url,
                "/api/v1/symbols/implementation",
                symbol=target["symbol"],
                file=target["path"],
            ),
            headers=headers,
        )
    if operation == "list_callers":
        target = targets["list_callers"]
        return _json_request(
            "GET",
            _url(
                base_url,
                "/api/v1/symbols/callers",
                symbol=target["symbol"],
                file=target["path"],
                limit="20",
            ),
            headers=headers,
        )
    if operation == "list_tests":
        target = targets["list_tests"]
        return _json_request(
            "GET",
            _url(
                base_url,
                "/api/v1/symbols/tests",
                symbol=target["symbol"],
                file=target["path"],
                limit="20",
            ),
            headers=headers,
        )
    if operation == "grep":
        target = targets["grep"]
        return _json_request(
            "GET",
            _url(
                base_url,
                "/api/v1/grep",
                pattern=target["pattern"],
                file=target["path"],
                file_match="exact",
                max_matches="20",
            ),
            headers=headers,
        )
    if operation == "peek_file":
        target = targets["structure"]
        return _json_request(
            "GET",
            _url(base_url, "/api/v1/peek", file=target["path"], start="0", end="40"),
            headers=headers,
        )
    if operation in {"touch_file", "append_file", "replace_file"}:
        target = targets["structure"]
        return _json_request(
            "GET",
            _url(base_url, "/api/v1/peek", file=target["path"], start="0", end="20"),
            headers=headers,
        )
    raise HarnessError(f"unsupported runtime operation: {operation}")


def _response_summary(operation: str, response: dict[str, Any]) -> dict[str, Any]:
    if "count" in response:
        return {"count": response["count"]}
    if operation == "read_implementation":
        return {"source_bytes": len(response.get("source", ""))}
    if "total_matches" in response:
        return {"total_matches": response["total_matches"]}
    if "lines" in response:
        return {"line_count": len(response["lines"])}
    return {"keys": sorted(response)[:8]}


def _json_request(
    method: str,
    url: str,
    body: dict[str, Any] | None = None,
    headers: dict[str, str] | None = None,
) -> dict[str, Any]:
    data = None
    request_headers = {"Accept": "application/json"}
    if headers:
        request_headers.update(headers)
    if body is not None:
        data = json.dumps(body).encode("utf-8")
        request_headers["Content-Type"] = "application/json"

    req = request.Request(url, data=data, headers=request_headers, method=method)
    try:
        with request.urlopen(req, timeout=5) as response:
            return json.loads(response.read().decode("utf-8"))
    except error.HTTPError as exc:
        detail = exc.read().decode("utf-8", errors="replace")
        raise HarnessError(f"{method} {url} returned HTTP {exc.code}: {detail}") from exc
    except error.URLError as exc:
        raise HarnessError(f"{method} {url} failed: {exc.reason}") from exc
    except TimeoutError as exc:
        raise HarnessError(f"{method} {url} timed out") from exc
    except json.JSONDecodeError as exc:
        raise HarnessError(f"{method} {url} returned invalid JSON: {exc}") from exc


def _url(base_url: str, path: str, **query: str) -> str:
    return f"{base_url}{path}?{parse.urlencode(query)}"


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
