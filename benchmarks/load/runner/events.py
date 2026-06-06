"""Structured operation-event normalization for load-harness reports."""

from __future__ import annotations

import time
from dataclasses import dataclass
from typing import Any

from benchmarks.load.runner.client import CodeRLMClientError, ReadinessTimeout


SUCCESS = "success"
ERROR = "error"
UNSUPPORTED = "unsupported"


@dataclass(frozen=True)
class OperationContext:
    scenario_id: str
    fixture_id: str
    agent_id: str
    project_id: str
    session_id: str
    project_root: str


def begin_operation() -> float:
    return time.monotonic()


def success_event(
    operation: str,
    context: OperationContext,
    started_at: float,
    response: dict[str, Any],
    *,
    sequence: int | None = None,
) -> dict[str, Any]:
    ended_at = time.monotonic()
    return _base_event(operation, context, started_at, ended_at, sequence=sequence) | {
        "status": SUCCESS,
        "ok": True,
        "expected_error": False,
        "error_classification": None,
        "error": None,
        "response_summary": response_summary(operation, response),
    }


def error_event(
    operation: str,
    context: OperationContext,
    started_at: float,
    exc: BaseException,
    *,
    sequence: int | None = None,
    expected_classifications: set[str] | None = None,
) -> dict[str, Any]:
    ended_at = time.monotonic()
    classification = classify_error(exc)
    expected = classification in (expected_classifications or set())
    return _base_event(operation, context, started_at, ended_at, sequence=sequence) | {
        "status": UNSUPPORTED if classification == "unsupported_operation_target" else ERROR,
        "ok": expected,
        "expected_error": expected,
        "error_classification": classification,
        "error": str(exc),
        "response_summary": None,
    }


def response_summary(operation: str, response: dict[str, Any]) -> dict[str, Any]:
    count = response.get("count")
    if isinstance(count, int):
        return {"count": count}
    if operation == "read_implementation":
        return {"source_bytes": len(response.get("source", ""))}
    total_matches = response.get("total_matches")
    if isinstance(total_matches, int):
        return {"total_matches": total_matches}
    lines = response.get("lines")
    if isinstance(lines, list):
        return {"line_count": len(lines)}
    if "tree" in response:
        return {"file_count": response.get("file_count")}
    return {"keys": sorted(response)[:8]}


def classify_error(exc: BaseException) -> str:
    if isinstance(exc, ReadinessTimeout):
        return "readiness_timeout"
    if isinstance(exc, UnsupportedOperationTarget):
        return "unsupported_operation_target"
    if isinstance(exc, CodeRLMClientError):
        if exc.status_code == 410:
            return "project_gone"
        if exc.status_code == 404:
            return "not_found"
        if exc.status_code == 400:
            return "bad_request"
        if exc.status_code is not None:
            return "http_error"
        return "transport_error"
    return "runner_error"


class UnsupportedOperationTarget(RuntimeError):
    """Raised when fixture metadata cannot support an operation."""


def _base_event(
    operation: str,
    context: OperationContext,
    started_at: float,
    ended_at: float,
    *,
    sequence: int | None = None,
) -> dict[str, Any]:
    event = {
        "operation": operation,
        "scenario_id": context.scenario_id,
        "fixture_id": context.fixture_id,
        "agent_id": context.agent_id,
        "project_id": context.project_id,
        "session_id": context.session_id,
        "project_root": context.project_root,
        "started_at_monotonic": round(started_at, 6),
        "ended_at_monotonic": round(ended_at, 6),
        "elapsed_seconds": round(ended_at - started_at, 6),
    }
    if sequence is not None:
        event["sequence"] = sequence
    return event
