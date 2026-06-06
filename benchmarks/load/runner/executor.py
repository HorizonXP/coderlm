"""Session lifecycle and core API operation executor for load scenarios."""

from __future__ import annotations

import os
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from benchmarks.load.fixtures_support import PreparedFixture, prepare_fixture_working_copy
from benchmarks.load.runner.client import CodeRLMClient, CodeRLMSession
from benchmarks.load.runner.events import (
    OperationContext,
    UnsupportedOperationTarget,
    begin_operation,
    error_event,
    success_event,
)


SYMBOL_DEPENDENT_OPERATIONS = {
    "search_symbols",
    "read_implementation",
    "list_callers",
    "list_tests",
}


@dataclass(frozen=True)
class AgentSession:
    agent_id: str
    project_id: str
    prepared_fixture: PreparedFixture
    session: CodeRLMSession
    readiness: dict[str, Any]


@dataclass(frozen=True)
class OperationPlanItem:
    operation: str
    expected_error_classifications: set[str]


@dataclass(frozen=True)
class PacingPlan:
    duration_seconds: float
    interval_seconds: float
    operation_count: int


def create_agent_sessions(
    client: CodeRLMClient,
    config: dict[str, Any],
) -> list[AgentSession]:
    """Create fixture sessions for the configured agent/project matrix."""

    output_dir = Path(config["output_dir"])
    run_root = output_dir / "fixture-worktrees" / f"run-{time.time_ns()}"
    project_count = config["project_count"]
    agent_count = config["agent_count"]
    sessions: list[AgentSession] = []
    prepared_by_project: dict[str, PreparedFixture] = {}

    for project_index in range(project_count):
        project_id = f"project-{project_index + 1}"
        prepared_by_project[project_id] = prepare_fixture_working_copy(
            config["fixture_id"],
            run_root,
            project_id,
        )

    for agent_index in range(agent_count):
        agent_id = f"agent-{agent_index + 1}"
        project_id = f"project-{(agent_index % project_count) + 1}"
        prepared = prepared_by_project[project_id]
        session = client.create_session(os.fspath(prepared.worktree_path))
        readiness = client.wait_for_ready(
            session,
            config["readiness_timeout_seconds"],
        )
        sessions.append(
            AgentSession(
                agent_id=agent_id,
                project_id=project_id,
                prepared_fixture=prepared,
                session=session,
                readiness=readiness,
            )
        )

    return sessions


def run_agent_operations(
    client: CodeRLMClient,
    config: dict[str, Any],
    agent_session: AgentSession,
    *,
    cancel_event: threading.Event | None = None,
    monotonic: Any = time.monotonic,
    sleep: Any = time.sleep,
) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    context = OperationContext(
        scenario_id=config["scenario_id"],
        fixture_id=config["fixture_id"],
        agent_id=agent_session.agent_id,
        project_id=agent_session.project_id,
        session_id=agent_session.session.session_id,
        project_root=agent_session.session.project_root,
    )
    operation_plan = _weighted_operation_plan(config["scenario_mix"])
    pacing = _pacing_plan(config)
    started_at = monotonic()
    cancel_event = cancel_event or threading.Event()

    for sequence in range(pacing.operation_count):
        if cancel_event.is_set():
            break
        item = operation_plan[sequence % len(operation_plan)]
        operation = item.operation
        target_start = started_at + (sequence * pacing.interval_seconds)
        _sleep_until(target_start, cancel_event, monotonic, sleep)
        if cancel_event.is_set():
            break

        started = begin_operation()
        try:
            response = run_operation(
                client,
                agent_session.session,
                agent_session.prepared_fixture.fixture.as_dict(),
                operation,
                readiness_timeout_seconds=config["readiness_timeout_seconds"],
            )
            events.append(
                success_event(operation, context, started, response, sequence=sequence)
            )
        except Exception as exc:
            events.append(
                error_event(
                    operation,
                    context,
                    started,
                    exc,
                    sequence=sequence,
                    expected_classifications=item.expected_error_classifications,
                )
            )
    return events


def run_operation(
    client: CodeRLMClient,
    session: CodeRLMSession | str,
    fixture: dict[str, Any],
    operation: str,
    *,
    readiness_timeout_seconds: int | None = None,
) -> dict[str, Any]:
    targets = fixture["known_targets"]
    session_id = session.session_id if isinstance(session, CodeRLMSession) else session
    if operation in SYMBOL_DEPENDENT_OPERATIONS and isinstance(session, CodeRLMSession):
        client.wait_for_ready(session, readiness_timeout_seconds or 30)

    if operation == "structure":
        target = _target(targets, "structure", operation)
        return client.get(
            "/api/v1/structure",
            session_id=session_id,
            query={"depth": "2", "detail": "1", "file": target["path"]},
        )
    if operation == "search_symbols":
        target = _target(targets, "search_symbols", operation)
        return client.get(
            "/api/v1/symbols/search",
            session_id=session_id,
            query={"q": target["query"], "limit": "20"},
        )
    if operation == "read_implementation":
        target = _target(targets, "read_implementation", operation)
        return client.get(
            "/api/v1/symbols/implementation",
            session_id=session_id,
            query={"symbol": target["symbol"], "file": target["path"]},
        )
    if operation == "list_callers":
        target = _target(targets, "list_callers", operation)
        return client.get(
            "/api/v1/symbols/callers",
            session_id=session_id,
            query={"symbol": target["symbol"], "file": target["path"], "limit": "20"},
        )
    if operation == "list_tests":
        target = _target(targets, "list_tests", operation)
        return client.get(
            "/api/v1/symbols/tests",
            session_id=session_id,
            query={"symbol": target["symbol"], "file": target["path"], "limit": "20"},
        )
    if operation == "grep":
        target = _target(targets, "grep", operation)
        return client.get(
            "/api/v1/grep",
            session_id=session_id,
            query={
                "pattern": target["pattern"],
                "file": target["path"],
                "file_match": "exact",
                "max_matches": "20",
            },
        )
    if operation == "peek_file":
        target = _target(targets, "structure", operation)
        return client.get(
            "/api/v1/peek",
            session_id=session_id,
            query={"file": target["path"], "start": "1", "end": "40"},
        )
    if operation in {"touch_file", "append_file", "replace_file"}:
        target = _target(targets, "structure", operation)
        return client.get(
            "/api/v1/peek",
            session_id=session_id,
            query={"file": target["path"], "start": "1", "end": "20"},
        )
    raise UnsupportedOperationTarget(f"unsupported runtime operation: {operation}")


def _target(targets: dict[str, Any], target_name: str, operation: str) -> dict[str, Any]:
    target = targets.get(target_name)
    if not isinstance(target, dict):
        raise UnsupportedOperationTarget(
            f"fixture target '{target_name}' is unavailable for operation {operation}"
        )
    return target


def _weighted_operation_plan(scenario_mix: list[dict[str, Any]]) -> list[OperationPlanItem]:
    plan: list[OperationPlanItem] = []
    for item in scenario_mix:
        expected = set(item.get("expected_error_classifications", []))
        plan.extend(
            OperationPlanItem(
                operation=item["operation"],
                expected_error_classifications=expected,
            )
            for _ in range(item["weight"])
        )
    if not plan:
        raise UnsupportedOperationTarget("scenario mix produced no executable operations")
    return plan


def _pacing_plan(config: dict[str, Any]) -> PacingPlan:
    duration_seconds = float(config["duration_seconds"])
    requests_per_second = float(config["request_pacing"]["requests_per_second"])
    think_time_seconds = float(config["request_pacing"].get("think_time_seconds", 0.0))
    interval_seconds = max(1.0 / requests_per_second, think_time_seconds)
    operation_count = max(1, int(duration_seconds / interval_seconds))
    return PacingPlan(
        duration_seconds=duration_seconds,
        interval_seconds=interval_seconds,
        operation_count=operation_count,
    )


def _sleep_until(
    target_start: float,
    cancel_event: threading.Event,
    monotonic: Any,
    sleep: Any,
) -> None:
    while True:
        remaining = target_start - monotonic()
        if remaining <= 0 or cancel_event.is_set():
            return
        sleep(min(remaining, 0.1))
